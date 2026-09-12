//! Opt-in cross-session index: browse context across every project, not just the
//! `n` most-recent sessions the poll loop tracks.
//!
//! The whole feature is gated behind `config.browse_enabled`. Turning it on is
//! what creates `cache.sqlite` in the app data dir -- so the user knowingly opts
//! into a potentially large first scan instead of getting one silently. No DB
//! exists until then, keeping the default footprint tiny.
//!
//! Two scan tiers keep the common case fast:
//!   - **Pass 1 (index):** stat every `projects/*/*.jsonl`, record path/mtime/size.
//!     Near-instant (no file contents read); powers the harness/project levels,
//!     where tiles are sized by on-disk footprint.
//!   - **Pass 2 (enrich):** read a transcript's contents to fill ctx tokens, cost,
//!     model, turn count, and whether it ran `/context`. Done lazily when the user
//!     drills into a project, or eagerly for every session in the optional "deep"
//!     scan. A row is re-enriched only when its mtime/size changed since last time.
//!
//! The top level is the *harness* (Claude Code today; the DB carries a `harness`
//! column so other providers -- Ollama, Cline, Codex -- slot in later without a
//! schema change).

use crate::codex;
use crate::config::{cache_db_path, claude_dir, codex_dir};
use crate::grouping;
use crate::scan;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::Emitter;

const HARNESS: &str = "claude-code";

/// Enrich one transcript with the adapter that matches its harness. A path under
/// the Codex data dir is a rollout; everything else is a Claude transcript. Reading
/// the path is enough at every call site (the `sessions.harness` column agrees), so
/// no extra query is needed to dispatch.
fn enrich_for(path: &Path) -> scan::EnrichMeta {
    if path.starts_with(codex_dir()) {
        codex::enrich_meta(path)
    } else {
        scan::enrich_meta(path)
    }
}

/// Parse-only enrich dispatcher: caller supplies the bytes, so this is pure CPU.
fn enrich_from(path: &Path, raw: &str) -> scan::EnrichMeta {
    if path.starts_with(codex_dir()) {
        codex::enrich_meta_from(path, raw)
    } else {
        scan::enrich_meta_from(path, raw)
    }
}

/// Parse-only chat-doc dispatcher (span + FTS text) from bytes the caller holds.
fn chat_doc_from(path: &Path, raw: &str) -> (String, Option<i64>, Option<i64>) {
    let doc = if path.starts_with(codex_dir()) {
        codex::read_chat_doc(path, raw)
    } else {
        read_chat_doc(path, raw)
    };
    let last = doc.last_ts.as_deref().and_then(scan::iso_to_millis);
    let first = doc.first_ts.as_deref().and_then(scan::iso_to_millis).or(last);
    (doc.text, first, last)
}

/// Trace to greedout.log (gated by the Debug logging setting), prefixed so browse
/// lines are easy to grep out of the poll-loop noise.
///
/// A macro, not a function, so that with logging off nothing is paid: the gate is
/// an atomic load, and the message is never formatted. This used to load and
/// parse config.json from disk on every call, before testing the flag, which made
/// the "leave the instrumentation in, it's free" bargain false in the one code
/// path that traces inside loops.
macro_rules! blog {
    ($($arg:tt)*) => {
        if $crate::config::debug_enabled() {
            $crate::config::debug_log_raw(&format!("browse: {}", format_args!($($arg)*)));
        }
    };
}

/// Set while a scan thread is running, so a second `browse_scan` is a no-op rather
/// than racing the first over the same DB.
fn running() -> &'static AtomicBool {
    static R: OnceLock<AtomicBool> = OnceLock::new();
    R.get_or_init(|| AtomicBool::new(false))
}

/// Set by `browse_cancel`; the scan worker checks it each iteration and stops,
/// leaving a usable partial index behind.
fn cancel_flag() -> &'static AtomicBool {
    static C: OnceLock<AtomicBool> = OnceLock::new();
    C.get_or_init(|| AtomicBool::new(false))
}

// --- Frontend-facing payloads (camelCase to match the rest of the app). ---

/// One harness (provider) aggregate. Only Claude Code is populated now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessAgg {
    pub harness: String,
    pub label: String,
    pub project_count: u64,
    pub session_count: u64,
    pub total_bytes: u64,
    /// Summed ctx tokens over ENRICHED sessions only (null rows are ignored).
    pub total_tokens: u64,
    pub total_cost: f64,
}

/// One project (directory) aggregate within a harness.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectAgg {
    pub harness: String,
    pub project: String,
    pub project_path: String,
    pub session_count: u64,
    pub total_bytes: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
    /// How many of this project's sessions have Pass-2 data yet.
    pub enriched_count: u64,
}

/// One session row, as browsed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    pub id: String,
    pub harness: String,
    pub project: String,
    pub project_path: String,
    pub title: String,
    pub mtime: i64,
    pub size_bytes: u64,
    pub ctx: Option<u64>,
    pub cost_usd: f64,
    pub model: String,
    pub turn_count: u64,
    pub has_context_usage: bool,
    /// True once Pass-2 has read this session's contents (else only size is known).
    pub enriched: bool,
}

/// State for the browse UI's gate/progress.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseStatus {
    pub enabled: bool,
    pub db_exists: bool,
    pub indexed: u64,
    pub enriched: u64,
    pub last_full_scan: Option<i64>,
    pub scanning: bool,
}

/// Progress event payload, emitted on "browse-progress" during a scan.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    phase: &'static str, // "index" | "enrich" | "done" | "canceled"
    done: u64,
    total: u64,
    current: String,
}

// --- DB plumbing ---

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn file_stat(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// Cleared whenever the cache file is (re)created, so the schema batch runs again
/// against a fresh database rather than being skipped on a stale flag.
fn schema_ready() -> &'static AtomicBool {
    static READY: OnceLock<AtomicBool> = OnceLock::new();
    READY.get_or_init(|| AtomicBool::new(false))
}

fn open_db() -> rusqlite::Result<Connection> {
    let conn = Connection::open(cache_db_path())?;
    // Tolerate the scan thread holding a write lock while a query lands.
    conn.busy_timeout(Duration::from_secs(5))?;
    // synchronous=NORMAL drops the per-commit fsync to one at checkpoint -- a big
    // win for the many-row enrich loop. Per connection, so it is set every time.
    let _ = conn.pragma_update(None, "synchronous", "NORMAL");
    // journal_mode=WAL and the CREATE TABLE batch are properties of the file, not
    // of the connection, so they only have to run once per process. Every browse
    // query opens its own connection, and re-parsing the whole schema batch on
    // each one was pure overhead.
    if !schema_ready().load(Ordering::Relaxed) {
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        init_db(&conn)?;
        schema_ready().store(true, Ordering::Relaxed);
    }
    Ok(conn)
}

fn init_db(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            path TEXT PRIMARY KEY,
            harness TEXT NOT NULL,
            session_id TEXT,
            project TEXT,
            project_path TEXT,
            mtime INTEGER,
            size_bytes INTEGER,
            scanned_mtime INTEGER,
            scanned_size INTEGER,
            ctx_tokens INTEGER,
            cost_usd REAL,
            model TEXT,
            turn_count INTEGER,
            title TEXT,
            has_context_usage INTEGER,
            first_ms INTEGER,
            last_ms INTEGER,
            updated_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_hp ON sessions(harness, project);
         CREATE INDEX IF NOT EXISTS idx_sid ON sessions(session_id);
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);
         -- Full-text chat index (chat-only text, not tool i/o), one row per session
         -- keyed to its sessions.rowid. tokenize='trigram' gives case-insensitive
         -- substring matching (partial words, e.g. \"grep\" inside \"ripgrep\") at
         -- query time. Content-owning (stores the text) so scoring + snippets need
         -- no file reads and deletes are by rowid alone. Populated during enrich.
         CREATE VIRTUAL TABLE IF NOT EXISTS chat_fts USING fts5(text, tokenize='trigram');
         -- Per-turn billed rows for Daily Spend, written during the enrich pass so
         -- the tree is parsed once for both windows. Rows are raw (undeduped): the
         -- Daily Spend read dedups by msg_id globally. One row set per session_path,
         -- replaced wholesale when its transcript is re-enriched.
         CREATE TABLE IF NOT EXISTS turns (
            session_path TEXT NOT NULL,
            msg_id TEXT,
            ts INTEGER NOT NULL,
            cost REAL NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_turns_path ON turns(session_path);
         CREATE INDEX IF NOT EXISTS idx_turns_ts ON turns(ts);
         -- Composite for the Daily Spend read: it scans turns in (session_path, ts)
         -- order, so this index satisfies its ORDER BY directly and removes the
         -- whole-table sort that (session_path) alone forced.
         CREATE INDEX IF NOT EXISTS idx_turns_path_ts ON turns(session_path, ts);",
    )?;
    // Add the search-span columns to DBs created before they existed (fresh DBs get
    // them from the CREATE above). A duplicate-column error is expected + ignored.
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN first_ms INTEGER", []);
    let _ = conn.execute("ALTER TABLE sessions ADD COLUMN last_ms INTEGER", []);
    // One-time migrations that force a single full re-enrich by clearing scanned_mtime
    // (a normal staleness scan would skip already-enriched rows). Each is gated by its
    // own meta flag so it runs exactly once:
    //   turns_ready - populate the per-turn `turns` table (added after first ship).
    //   fts_ready   - populate `chat_fts` + the first_ms/last_ms columns (added now);
    //                 without this, rows enriched before FTS existed would be neither
    //                 in the index nor re-scanned, so unsearchable.
    for flag in ["turns_ready", "fts_ready"] {
        let done: bool = conn
            .query_row("SELECT value FROM meta WHERE key=?1", [flag], |r| r.get::<_, String>(0))
            .map(|s| s == "1")
            .unwrap_or(false);
        if !done {
            conn.execute("UPDATE sessions SET scanned_mtime=NULL", [])?;
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, '1')
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [flag],
            )?;
        }
    }
    Ok(())
}

/// Create the DB if it doesn't exist yet (called when the user opts in).
pub fn enable() -> Result<(), String> {
    if let Some(p) = cache_db_path().parent() {
        let _ = std::fs::create_dir_all(p);
    }
    // The file can be gone (never created, or deleted from under us), in which
    // case the schema has to be laid down again on the next open.
    if !cache_db_path().exists() {
        schema_ready().store(false, Ordering::Relaxed);
    }
    open_db().map(|_| ()).map_err(|e| e.to_string())
}

pub fn status(enabled: bool) -> BrowseStatus {
    let db_exists = cache_db_path().exists();
    let (indexed, enriched, last_full_scan) = if db_exists {
        open_db()
            .and_then(|conn| {
                let indexed: u64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
                let enriched: u64 = conn.query_row(
                    "SELECT COUNT(*) FROM sessions WHERE scanned_mtime IS NOT NULL",
                    [],
                    |r| r.get(0),
                )?;
                let last: Option<i64> = conn
                    .query_row(
                        "SELECT value FROM meta WHERE key='last_full_scan'",
                        [],
                        |r| r.get::<_, String>(0),
                    )
                    .ok()
                    .and_then(|s| s.parse().ok());
                Ok((indexed, enriched, last))
            })
            .unwrap_or((0, 0, None))
    } else {
        (0, 0, None)
    };
    BrowseStatus {
        enabled,
        db_exists,
        indexed,
        enriched,
        last_full_scan,
        scanning: running().load(Ordering::Relaxed),
    }
}

/// First occurrence of each `msg_id` wins; rows whose msg_id is `None` are all
/// kept. The input MUST already be ordered so the intended winner comes first
/// (callers order by `session_path, ts` in SQL). Pure and DB-free so a unit test
/// can drive it with plain tuples -- the dedupe rule was previously only exercisable
/// against the live cache.
fn dedup_first_by_msg_id<T>(
    rows: impl IntoIterator<Item = T>,
    msg_id: impl Fn(&T) -> Option<&str>,
) -> Vec<T> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<T> = Vec::new();
    for row in rows {
        if let Some(mid) = msg_id(&row) {
            if !seen.insert(mid.to_string()) {
                continue;
            }
        }
        out.push(row);
    }
    out
}

/// Per-day, per-project spend totals for the Daily Spend overview (months + days
/// levels). Dedupes resumed/compacted duplicates by msg_id inside SQL (first per id
/// by session_path, ts) and buckets by the user's LOCAL calendar day, matching the
/// frontend's `new Date` bucketing. Tiny result (a few hundred rows) versus the
/// ~100k per-turn events the old path shipped. Returns empty until the first enrich
/// has run (the caller then triggers a scan).
pub fn spend_summary() -> Vec<scan::SpendSummary> {
    if !cache_db_path().exists() {
        return Vec::new();
    }
    let Ok(conn) = open_db() else { return Vec::new() };
    spend_summary_conn(&conn).unwrap_or_default()
}

fn spend_summary_conn(conn: &Connection) -> rusqlite::Result<Vec<scan::SpendSummary>> {
    // Window dedupe (first per msg_id) + local-day bucket. The `msg_id IS NULL OR
    // rn = 1` clause deliberately keeps ALL null-msg_id rows even though they share
    // one window partition -- a null id can't be deduped, so every such turn counts.
    let mut stmt = conn.prepare(
        "WITH r AS (
           SELECT t.msg_id, t.ts, t.cost, s.project,
                  ROW_NUMBER() OVER (PARTITION BY t.msg_id ORDER BY t.session_path, t.ts) rn
           FROM turns t JOIN sessions s ON s.path = t.session_path
           WHERE t.cost > 0
         )
         SELECT strftime('%Y-%m-%d', ts/1000, 'unixepoch', 'localtime') AS day,
                COALESCE(project,'') AS project, sum(cost) AS cost
         FROM r WHERE msg_id IS NULL OR rn = 1
         GROUP BY day, project",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(scan::SpendSummary {
            day: r.get(0)?,
            project: r.get(1)?,
            cost: r.get(2)?,
        })
    })?;
    rows.collect()
}

/// The billed, deduped turns for a single local day, for the swim lanes. The
/// frontend passes the local-day `[lo, hi)` millisecond range it already computes,
/// so no timezone code is needed here -- the query is an indexed ts-range scan
/// (idx_turns_ts). Duplicate copies of a msg_id keep their ORIGINAL ts through
/// resume/compaction, so they all fall in the same local day: per-day dedupe here
/// equals the global first-wins result.
pub fn spend_day(lo_ms: i64, hi_ms: i64) -> Vec<scan::SpendEvent> {
    if !cache_db_path().exists() {
        return Vec::new();
    }
    let Ok(conn) = open_db() else { return Vec::new() };
    spend_day_conn(&conn, lo_ms, hi_ms).unwrap_or_default()
}

type SpendRow = (
    Option<String>, // msg_id
    i64,            // ts
    f64,            // cost
    Option<String>, // session_id
    Option<String>, // project
    Option<String>, // title
    Option<i64>,    // mtime
    String,         // harness
    Option<String>, // project_path
);

fn spend_day_conn(conn: &Connection, lo_ms: i64, hi_ms: i64) -> rusqlite::Result<Vec<scan::SpendEvent>> {
    let mut stmt = conn.prepare(
        "SELECT t.msg_id, t.ts, t.cost, s.session_id, s.project, s.title, s.mtime, s.harness, s.project_path
         FROM turns t JOIN sessions s ON s.path = t.session_path
         WHERE t.cost > 0 AND t.ts >= ?1 AND t.ts < ?2
         ORDER BY t.session_path, t.ts",
    )?;
    let rows = stmt.query_map([lo_ms, hi_ms], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, f64>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, String>(7)?,
            r.get::<_, Option<String>>(8)?,
        ))
    })?;
    let collected: Vec<SpendRow> = rows.collect::<rusqlite::Result<_>>()?;
    // First per msg_id by the query's (session_path, ts) order, then build events.
    let deduped = dedup_first_by_msg_id(collected, |row| row.0.as_deref());
    let mut out: Vec<scan::SpendEvent> = deduped
        .into_iter()
        .filter(|r| r.2 > 0.0) // cost > 0 already SQL-filtered; belt-and-braces
        .map(|(_msg_id, ts, cost, session, project, title, mtime, harness, project_path)| {
            scan::SpendEvent {
                t: ts,
                cost,
                session: session.unwrap_or_default(),
                project: project.unwrap_or_default(),
                project_path: project_path.unwrap_or_default(),
                harness,
                title: title.unwrap_or_default(),
                mtime: mtime.unwrap_or(0),
            }
        })
        .collect();
    out.sort_by_key(|e| e.t);
    Ok(out)
}

/// Kick off a scan on a background thread. Pass 1 always runs; if `deep`, every
/// stale row is then enriched (the optional "deluxe" full scan). Emits
/// "browse-progress" throughout. A no-op if a scan is already running.
pub fn run_scan(app: tauri::AppHandle, deep: bool) {
    if running().swap(true, Ordering::SeqCst) {
        blog!("scan skipped: already running");
        return; // already scanning
    }
    cancel_flag().store(false, Ordering::SeqCst);
    blog!("scan start (deep={deep})");
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let result = scan_worker(&app, deep);
        running().store(false, Ordering::SeqCst);
        let phase = if cancel_flag().load(Ordering::Relaxed) {
            "canceled"
        } else {
            "done"
        };
        let total = result.unwrap_or(0);
        blog!("scan {} rows in {}ms (deep={deep})", total, t0.elapsed().as_millis());
        let _ = app.emit(
            "browse-progress",
            ScanProgress { phase, done: total, total, current: String::new() },
        );
    });
}

pub fn cancel() {
    cancel_flag().store(true, Ordering::SeqCst);
}

/// Human-readable "project - title" for a progress line, so the scan log shows what
/// is being processed instead of an opaque session guid. Falls back to the id when
/// there is no title, and drops the project half when there is no cwd.
fn scan_label(id: &str, m: &scan::EnrichMeta) -> String {
    let proj = m.cwd.as_deref().map(scan::last_component).unwrap_or_default();
    let name = m.title.as_deref().filter(|s| !s.is_empty()).unwrap_or(id);
    if proj.is_empty() {
        name.to_string()
    } else {
        format!("{proj} \u{00b7} {name}")
    }
}

fn emit_progress(app: &tauri::AppHandle, phase: &'static str, done: u64, total: u64, current: &str) {
    let _ = app.emit(
        "browse-progress",
        ScanProgress { phase, done, total, current: current.to_string() },
    );
}

/// The actual scan. Returns the number of rows indexed. Bails early (leaving a
/// usable partial index) whenever the cancel flag is set.
fn scan_worker(app: &tauri::AppHandle, deep: bool) -> rusqlite::Result<u64> {
    let mut conn = open_db()?;

    // Enumerate transcripts. Forward slashes REQUIRED for the glob crate on
    // Windows (same trap as scan.rs::scan()).
    let pattern = claude_dir()
        .join("projects")
        .join("*")
        .join("*.jsonl")
        .to_string_lossy()
        .replace('\\', "/");
    let paths: Vec<std::path::PathBuf> = glob::glob(&pattern)
        .map(|g| g.flatten().collect())
        .unwrap_or_default();
    let total = paths.len() as u64;
    blog!("scan_worker indexing {total} transcripts (deep={deep})");
    let p1 = std::time::Instant::now();

    // --- Pass 1: stat + upsert. Project columns are set on INSERT only, so a
    // later enrich's real cwd isn't clobbered by the cheap decoded guess. ---
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    {
        let tx = conn.transaction()?;
        let mut done = 0u64;
        for path in &paths {
            if cancel_flag().load(Ordering::Relaxed) {
                break;
            }
            let path_str = path.to_string_lossy().to_string();
            seen.insert(path_str.clone());
            let id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let (mtime, size) = file_stat(path).unwrap_or((0, 0));
            let dir = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let decoded = scan::decode_project_dir(&dir);
            let project = scan::last_component(&decoded);
            tx.execute(
                "INSERT INTO sessions (path, harness, session_id, project, project_path, mtime, size_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(path) DO UPDATE SET mtime=excluded.mtime, size_bytes=excluded.size_bytes",
                rusqlite::params![path_str, HARNESS, id, project, decoded, mtime, size],
            )?;
            done += 1;
            if done % 25 == 0 || done == total {
                emit_progress(app, "index", done, total, &project);
            }
        }
        // Codex rollouts alongside the Claude transcripts. Codex encodes no project
        // in the filename, so the project comes from the session's cwd (a small tail
        // read); the enrich pass later confirms it from a whole-file scan. Rows are
        // written the same way, only harness + discovery differ. Titles are cheap
        // (a lookup in session_index.jsonl), so we set them here rather than waiting
        // for enrich.
        for (path, mtime_secs) in codex::candidates() {
            if cancel_flag().load(Ordering::Relaxed) {
                break;
            }
            let path_str = path.to_string_lossy().to_string();
            seen.insert(path_str.clone());
            let id = codex::id_from_path(&path);
            let (mtime, size) =
                file_stat(&path).unwrap_or(((mtime_secs as i64) * 1000, 0));
            let cwd = codex::cwd_of(&path);
            let (project, project_path) = match &cwd {
                Some(c) if !c.is_empty() => (scan::last_component(c), c.clone()),
                _ => (id.clone(), String::new()),
            };
            let title = codex::session_title(&id);
            tx.execute(
                "INSERT INTO sessions (path, harness, session_id, project, project_path, mtime, size_bytes, title)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(path) DO UPDATE SET mtime=excluded.mtime, size_bytes=excluded.size_bytes",
                rusqlite::params![path_str, "codex", id, project, project_path, mtime, size, title],
            )?;
        }
        // Drop rows whose transcript vanished (and their turns + FTS entry). The FTS
        // row is keyed by sessions.rowid, so grab the rowid before deleting the row.
        {
            let mut stmt = tx.prepare("SELECT rowid, path FROM sessions")?;
            let existing: Vec<(i64, String)> = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .flatten()
                .collect();
            let mut del = tx.prepare("DELETE FROM sessions WHERE path=?1")?;
            let mut del_turns = tx.prepare("DELETE FROM turns WHERE session_path=?1")?;
            let mut del_fts = tx.prepare("DELETE FROM chat_fts WHERE rowid=?1")?;
            for (rowid, p) in existing {
                if !seen.contains(&p) {
                    del.execute([&p])?;
                    del_turns.execute([&p])?;
                    del_fts.execute([rowid])?;
                }
            }
        }
        tx.commit()?;
    }
    emit_progress(app, "index", total, total, "");
    blog!("index (pass 1) {} transcripts in {}ms", total, p1.elapsed().as_millis());

    // --- Pass 2 (deep only): enrich every stale row. ---
    if deep && !cancel_flag().load(Ordering::Relaxed) {
        let stale: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT path, session_id FROM sessions
                 WHERE scanned_mtime IS NULL OR scanned_mtime != mtime OR scanned_size != size_bytes",
            )?;
            let rows: Vec<(String, String)> = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .flatten()
                .collect();
            rows
        };
        enrich_parallel(app, &mut conn, stale, 5)?;
    }

    if !cancel_flag().load(Ordering::Relaxed) {
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('last_full_scan', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [now_millis().to_string()],
        )?;
    }
    Ok(total)
}

/// One transcript's parsed Pass-2 payload, produced by the parallel read stage
/// and consumed by the serial (transactional) write stage.
struct EnrichRow {
    path: String,
    meta: scan::EnrichMeta,
    mtime: i64,
    size: u64,
    /// Chat-only text for the FTS index, plus the session's activity span, read in
    /// the same parallel stage (the file is already hot in the OS cache from the
    /// enrich read, so this second read is effectively free).
    chat_text: String,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
}

/// Accumulated per-thread timings for the enrich pass, summed across all worker
/// threads. Lets a debug log attribute the pass to disk (`read_us`) vs CPU
/// (`parse_us`). Microseconds; only ever written when Debug logging is on.
#[derive(Default)]
struct EnrichTimers {
    read_us: u128,
    parse_us: u128,
    stat_us: u128,
    bytes: u64,
    files: u64,
}

impl EnrichTimers {
    fn add(&mut self, o: &EnrichTimers) {
        self.read_us += o.read_us;
        self.parse_us += o.parse_us;
        self.stat_us += o.stat_us;
        self.bytes += o.bytes;
        self.files += o.files;
    }
}

/// Read a transcript's chat-only text (for `chat_fts`) and its activity span,
/// dispatching to the Codex or Claude reader by path. Separate from `enrich_for`
/// so the enrich metadata pipeline stays untouched; the OS page cache makes the
/// re-read cheap since enrich just read the same bytes.
fn chat_doc_for(path: &Path) -> (String, Option<i64>, Option<i64>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return (String::new(), None, None);
    };
    let doc = if path.starts_with(codex_dir()) {
        codex::read_chat_doc(path, &raw)
    } else {
        read_chat_doc(path, &raw)
    };
    let last = doc.last_ts.as_deref().and_then(scan::iso_to_millis);
    let first = doc.first_ts.as_deref().and_then(scan::iso_to_millis).or(last);
    (doc.text, first, last)
}

/// Read + parse every stale transcript IN PARALLEL (std threads, no dep), then
/// write all rows on the single owning connection inside batched transactions.
/// Reads are independent and CPU+IO bound; writes must stay serial (one conn).
/// `throttle` emits a progress event only every Nth row (the frontend re-reads
/// the whole project per emit). Returns the number of rows processed.
fn enrich_parallel(
    app: &tauri::AppHandle,
    conn: &mut Connection,
    stale: Vec<(String, String)>,
    throttle: u64,
) -> rusqlite::Result<u64> {
    let etotal = stale.len() as u64;
    if etotal == 0 {
        return Ok(0);
    }
    let nthreads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
    let chunk = stale.len().div_ceil(nthreads);
    let done = std::sync::atomic::AtomicU64::new(0);
    let wall = std::time::Instant::now();
    // Parallel read/parse. `thread::scope` lets the threads borrow `done`/`app`
    // without 'static bounds; each returns its enriched rows plus timers so the
    // scan can attribute the pass to disk (read) vs CPU (parse). Reading once and
    // feeding the same bytes to both parsers also drops the old second read.
    let mut results: Vec<EnrichRow> = Vec::with_capacity(stale.len());
    let mut tm = EnrichTimers::default();
    std::thread::scope(|s| {
        let handles: Vec<_> = stale
            .chunks(chunk.max(1))
            .map(|part| {
                let done = &done;
                s.spawn(move || {
                    let mut out = Vec::with_capacity(part.len());
                    let mut t = EnrichTimers::default();
                    for (path, id) in part {
                        if cancel_flag().load(Ordering::Relaxed) {
                            break;
                        }
                        let p = Path::new(path);
                        let ts = std::time::Instant::now();
                        let (mtime, size) = file_stat(p).unwrap_or((0, 0));
                        t.stat_us += ts.elapsed().as_micros();
                        let tr = std::time::Instant::now();
                        let raw = std::fs::read_to_string(p).unwrap_or_default();
                        t.read_us += tr.elapsed().as_micros();
                        t.bytes += raw.len() as u64;
                        let tp = std::time::Instant::now();
                        let meta = enrich_from(p, &raw);
                        let (chat_text, first_ms, last_ms) = chat_doc_from(p, &raw);
                        t.parse_us += tp.elapsed().as_micros();
                        t.files += 1;
                        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if throttle <= 1 || d % throttle == 0 || d == etotal {
                            emit_progress(app, "enrich", d, etotal, &scan_label(id, &meta));
                        }
                        out.push(EnrichRow {
                            path: path.clone(),
                            meta,
                            mtime,
                            size,
                            chat_text,
                            first_ms,
                            last_ms,
                        });
                    }
                    (out, t)
                })
            })
            .collect();
        for h in handles {
            let (rows, t) = h.join().unwrap_or_default();
            results.extend(rows);
            tm.add(&t);
        }
    });
    // Per-thread totals SUM across threads, so read+parse can exceed wall time --
    // that gap is exactly the parallel overlap. read >> parse => disk bound;
    // parse >> read => CPU bound. `wall` is the real elapsed for the whole pass.
    blog!(
        "enrich {} files: read {}ms parse {}ms stat {}ms bytes {} over {} threads, wall {}ms",
        tm.files,
        tm.read_us / 1000,
        tm.parse_us / 1000,
        tm.stat_us / 1000,
        tm.bytes,
        nthreads,
        wall.elapsed().as_millis(),
    );
    // Serial batched write: one fsync per ~200 rows instead of per row. This stage
    // (turns inserts + the chat_fts trigram index build) is a large fraction of the
    // pass on a big history, so it emits its own "write" progress -- otherwise the
    // bar sits pinned at 100% here while the DB is written (was a long dead pause).
    let wtotal = results.len() as u64;
    let wt0 = std::time::Instant::now();
    let mut n = 0u64;
    let mut tx = conn.transaction()?;
    for r in &results {
        write_enrich(&tx, &r.path, &r.meta, r.mtime, r.size, &r.chat_text, r.first_ms, r.last_ms)?;
        n += 1;
        if n % 200 == 0 {
            tx.commit()?;
            tx = conn.transaction()?;
        }
        if n % 50 == 0 || n == wtotal {
            let id = Path::new(&r.path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            emit_progress(app, "write", n, wtotal, &scan_label(&id, &r.meta));
        }
    }
    tx.commit()?;
    blog!("enrich write {} rows in {}ms", wtotal, wt0.elapsed().as_millis());
    Ok(n)
}

/// Write one transcript's Pass-2 columns from its already-parsed metadata, plus its
/// chat-only text into the FTS index and its activity span.
fn write_enrich(
    conn: &Connection,
    path_str: &str,
    m: &scan::EnrichMeta,
    mtime: i64,
    size: u64,
    chat_text: &str,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
) -> rusqlite::Result<()> {
    // Refine project from the real cwd when the transcript recorded one.
    let (project, project_path) = match &m.cwd {
        Some(cwd) => (Some(scan::last_component(cwd)), Some(cwd.clone())),
        None => (None, None),
    };
    conn.execute(
        "UPDATE sessions SET
            scanned_mtime=?1, scanned_size=?2,
            ctx_tokens=?3, cost_usd=?4, model=?5, turn_count=?6,
            title=?7, has_context_usage=?8, updated_at=?9,
            project=COALESCE(?10, project), project_path=COALESCE(?11, project_path),
            first_ms=?13, last_ms=?14
         WHERE path=?12",
        rusqlite::params![
            mtime,
            size,
            m.ctx.map(|c| c as i64),
            m.cost_usd,
            m.model_label,
            m.turn_count as i64,
            m.title,
            m.has_context_usage as i64,
            now_millis(),
            project,
            project_path,
            path_str,
            first_ms,
            last_ms,
        ],
    )?;
    // Refresh this session's FTS entry, keyed to its sessions.rowid (delete-then-insert;
    // a content-owning FTS5 table deletes by rowid alone, no old text needed).
    if let Ok(rowid) =
        conn.query_row("SELECT rowid FROM sessions WHERE path=?1", [path_str], |r| r.get::<_, i64>(0))
    {
        conn.execute("DELETE FROM chat_fts WHERE rowid=?1", [rowid])?;
        conn.execute(
            "INSERT INTO chat_fts (rowid, text) VALUES (?1, ?2)",
            rusqlite::params![rowid, chat_text],
        )?;
    }
    // Replace this transcript's per-turn rows wholesale (raw, undeduped).
    conn.execute("DELETE FROM turns WHERE session_path=?1", [path_str])?;
    if !m.turns.is_empty() {
        let mut ins =
            conn.prepare("INSERT INTO turns (session_path, msg_id, ts, cost) VALUES (?1, ?2, ?3, ?4)")?;
        for (id, ts, cost) in &m.turns {
            ins.execute(rusqlite::params![path_str, id, ts, cost])?;
        }
    }
    Ok(())
}

// --- Queries ---

pub fn harnesses() -> Vec<HarnessAgg> {
    let Ok(conn) = open_db() else { return Vec::new() };
    let mut stmt = match conn.prepare(
        "SELECT harness,
                COUNT(DISTINCT project),
                COUNT(*),
                COALESCE(SUM(size_bytes),0),
                COALESCE(SUM(ctx_tokens),0),
                COALESCE(SUM(cost_usd),0)
         FROM sessions GROUP BY harness ORDER BY 4 DESC",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    stmt.query_map([], |r| {
        let harness: String = r.get(0)?;
        Ok(HarnessAgg {
            label: harness_label(&harness),
            harness,
            project_count: r.get(1)?,
            session_count: r.get(2)?,
            total_bytes: r.get(3)?,
            total_tokens: r.get::<_, i64>(4)? as u64,
            total_cost: r.get(5)?,
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Every distinct working directory a harness has sessions in, mapped to the
/// project it belongs to. Sessions started in a subdirectory (`Greedout\src-tauri`)
/// fold into their project instead of standing as a project of their own; see
/// grouping.rs for the rules.
fn groups(conn: &Connection, harness: &str) -> HashMap<String, grouping::Group> {
    let dirs: Vec<PathBuf> = conn
        .prepare("SELECT DISTINCT COALESCE(project_path, project) FROM sessions WHERE harness=?1")
        .and_then(|mut s| {
            s.query_map([harness], |r| r.get::<_, String>(0)).map(|rows| {
                rows.flatten()
                    .filter(|d| !d.is_empty())
                    .map(PathBuf::from)
                    .collect()
            })
        })
        .unwrap_or_default();
    grouping::group_dirs(&dirs)
        .into_iter()
        .map(|(dir, g)| (dir.to_string_lossy().to_string(), g))
        .collect()
}

/// The directories that make up one project: its own, plus every subdirectory
/// session folded into it. Used in place of a `project=?` filter, which matches
/// only the sessions started in the project directory itself.
fn group_dirs_for(conn: &Connection, harness: &str, project: &str) -> Vec<String> {
    groups(conn, harness)
        .into_iter()
        .filter(|(_, g)| g.label == project)
        .map(|(dir, _)| dir)
        .collect()
}

/// `?2, ?3, ...` for an IN clause of `n` bound values (`?1` is the harness).
fn placeholders(n: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(",")
}

/// Bind params for the harness plus a list of directories.
fn dir_params(harness: &str, dirs: &[String]) -> Vec<Box<dyn rusqlite::ToSql>> {
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(harness.to_string())];
    params.extend(
        dirs.iter()
            .map(|d| Box::new(d.clone()) as Box<dyn rusqlite::ToSql>),
    );
    params
}

pub fn projects(harness: &str) -> Vec<ProjectAgg> {
    let Ok(conn) = open_db() else { return Vec::new() };
    let groups = groups(&conn, harness);
    // One row per working directory, folded into projects in Rust: which
    // directories belong together is a path question SQL cannot answer.
    let mut stmt = match conn.prepare(
        "SELECT COALESCE(project_path, project),
                project,
                COUNT(*),
                COALESCE(SUM(size_bytes),0),
                COALESCE(SUM(ctx_tokens),0),
                COALESCE(SUM(cost_usd),0),
                SUM(CASE WHEN scanned_mtime IS NOT NULL THEN 1 ELSE 0 END)
         FROM sessions WHERE harness=?1 GROUP BY 1",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows: Vec<(String, String, u64, u64, i64, f64, u64)> = stmt
        .query_map([harness], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();

    let mut aggs: Vec<ProjectAgg> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (dir, name, count, bytes, tokens, cost, enriched) in rows {
        let g = groups.get(&dir);
        let root = g
            .map(|g| g.root.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.clone());
        let label = g.map(|g| g.label.clone()).unwrap_or(name);
        let i = *index.entry(root.clone()).or_insert_with(|| {
            aggs.push(ProjectAgg {
                harness: harness.to_string(),
                project: label,
                project_path: root,
                session_count: 0,
                total_bytes: 0,
                total_tokens: 0,
                total_cost: 0.0,
                enriched_count: 0,
            });
            aggs.len() - 1
        });
        let a = &mut aggs[i];
        a.session_count += count;
        a.total_bytes += bytes;
        a.total_tokens += tokens.max(0) as u64;
        a.total_cost += cost;
        a.enriched_count += enriched;
    }
    aggs.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes));
    aggs
}

/// Sessions in a project, READ-ONLY: returns whatever is already in the DB (size
/// always, tokens/cost only on already-enriched rows). Enrichment is a separate,
/// cancelable background pass (`run_enrich_project`) so drilling in is instant and
/// the window can be closed while it finishes.
pub fn sessions(harness: &str, project: &str) -> Vec<SessionMeta> {
    let Ok(conn) = open_db() else { return Vec::new() };
    let rows = read_sessions(&conn, harness, project);
    blog!("sessions read {harness}/{project} -> {} rows", rows.len());
    rows
}

/// How many of a project's rows still need enriching (stale or never scanned).
fn project_stale(conn: &Connection, harness: &str, project: &str) -> Vec<(String, String)> {
    let dirs = group_dirs_for(conn, harness, project);
    if dirs.is_empty() {
        return Vec::new();
    }
    let sql = format!(
        "SELECT path, session_id FROM sessions
         WHERE harness=?1 AND COALESCE(project_path, project) IN ({})
           AND (scanned_mtime IS NULL OR scanned_mtime != mtime OR scanned_size != size_bytes)",
        placeholders(dirs.len())
    );
    let params = dir_params(harness, &dirs);
    conn.prepare(&sql)
        .and_then(|mut s| {
            s.query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
}

/// Enrich one project's stale rows on a background thread, emitting "browse-progress"
/// (phase "enrich", then "done"/"canceled"). No-op if another scan/enrich is already
/// running (its rows will fill in as that pass proceeds). The frontend re-reads the
/// tiles via `sessions()` as progress arrives.
pub fn run_enrich_project(app: tauri::AppHandle, harness: String, project: String) {
    if running().swap(true, Ordering::SeqCst) {
        blog!("enrich {harness}/{project} skipped: a scan is already running");
        return;
    }
    cancel_flag().store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let done = enrich_project_worker(&app, &harness, &project).unwrap_or_else(|e| {
            blog!("enrich {harness}/{project} error: {e}");
            0
        });
        running().store(false, Ordering::SeqCst);
        let canceled = cancel_flag().load(Ordering::Relaxed);
        blog!(
            "enrich {harness}/{project} finished: {done} enriched{}",
            if canceled { " (canceled)" } else { "" }
        );
        let _ = app.emit(
            "browse-progress",
            ScanProgress {
                phase: if canceled { "canceled" } else { "done" },
                done,
                total: done,
                current: String::new(),
            },
        );
    });
}

fn enrich_project_worker(
    app: &tauri::AppHandle,
    harness: &str,
    project: &str,
) -> rusqlite::Result<u64> {
    let mut conn = open_db()?;
    let stale = project_stale(&conn, harness, project);
    let total = stale.len() as u64;
    blog!("enrich {harness}/{project} start: {total} stale rows");
    emit_progress(app, "enrich", 0, total, project);
    // Parallel read + batched transactional write. Throttle emits to every 5th
    // row (the frontend re-reads the whole project per emit).
    enrich_parallel(app, &mut conn, stale, 5)
}

fn read_sessions(conn: &Connection, harness: &str, project: &str) -> Vec<SessionMeta> {
    let dirs = group_dirs_for(conn, harness, project);
    if dirs.is_empty() {
        return Vec::new();
    }
    let sql = format!(
        "SELECT session_id, harness, project, project_path, mtime, size_bytes,
                ctx_tokens, cost_usd, model, turn_count, has_context_usage,
                title, scanned_mtime
         FROM sessions WHERE harness=?1 AND COALESCE(project_path, project) IN ({})
         ORDER BY mtime DESC",
        placeholders(dirs.len())
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let params = dir_params(harness, &dirs);
    stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
        let title: Option<String> = r.get(11)?;
        let project: String = r.get(2)?;
        let scanned: Option<i64> = r.get(12)?;
        Ok(SessionMeta {
            id: r.get(0)?,
            harness: r.get(1)?,
            title: title.filter(|t| !t.is_empty()).unwrap_or_else(|| project.clone()),
            project,
            project_path: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            mtime: r.get(4)?,
            size_bytes: r.get(5)?,
            ctx: r.get::<_, Option<i64>>(6)?.map(|c| c as u64),
            cost_usd: r.get::<_, Option<f64>>(7)?.unwrap_or(0.0),
            model: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
            turn_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0) as u64,
            has_context_usage: r.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
            enriched: scanned.is_some(),
        })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Enrich one session (by id) and return its row, e.g. right before opening its
/// deepest breakdown.
pub fn session(id: &str) -> Option<SessionMeta> {
    let conn = open_db().ok()?;
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT path, project, harness FROM sessions WHERE session_id=?1 LIMIT 1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        )
        .ok();
    let (path, project, harness) = row?;
    let p = Path::new(&path);
    let (mtime, size) = file_stat(p).unwrap_or((0, 0));
    let (chat_text, first_ms, last_ms) = chat_doc_for(p);
    let _ = write_enrich(&conn, &path, &enrich_for(p), mtime, size, &chat_text, first_ms, last_ms);
    read_sessions(&conn, &harness, &project)
        .into_iter()
        .find(|s| s.id == id)
}

fn harness_label(h: &str) -> String {
    match h {
        "claude-code" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        other => other.to_string(),
    }
}

// --- Full-text session search (chat text only, not tools) ---

/// Set while a search thread runs, so a new search cancels+replaces cleanly.
fn search_running() -> &'static AtomicBool {
    static R: OnceLock<AtomicBool> = OnceLock::new();
    R.get_or_init(|| AtomicBool::new(false))
}
fn search_cancel() -> &'static AtomicBool {
    static C: OnceLock<AtomicBool> = OnceLock::new();
    C.get_or_init(|| AtomicBool::new(false))
}
/// Bumped on every new search. A thread captures its value at start and stops
/// emitting once it no longer matches, so a cancelled run that hasn't yet
/// noticed the flag can't bleed hits into the run that replaced it.
fn search_gen() -> &'static AtomicU64 {
    static G: OnceLock<AtomicU64> = OnceLock::new();
    G.get_or_init(|| AtomicU64::new(0))
}

/// One search hit, streamed to the UI as it's found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub id: String,
    pub title: String,
    pub project: String,
    pub project_path: String,
    pub size_bytes: u64,
    pub mtime: i64,
    /// Session activity span from transcript timestamps (millis). first = first
    /// turn, last = last turn; both fall back to file mtime if the transcript
    /// carries no timestamps.
    pub first_ms: i64,
    pub last_ms: i64,
    pub score: u32,
    pub snippet: String,
    /// "claude-code" | "codex" -- which harness this session belongs to, so the
    /// results list can badge cross-harness hits.
    pub harness: String,
}

/// Progress for a running search.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchProgress {
    phase: &'static str, // "search" | "done" | "canceled"
    done: u64,
    total: u64,
    hits: u64,
}

/// A session's chat text (user + assistant text/thinking, NOT tool i/o) plus a
/// title and cwd, from a single file read. Used only for search. Filled by
/// `read_chat_doc` for Claude transcripts and `codex::read_chat_doc` for Codex
/// rollouts (different on-disk shapes, same struct).
pub(crate) struct ChatDoc {
    pub text: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub first_user: Option<String>,
    pub first_ts: Option<String>,
    pub last_ts: Option<String>,
}

fn read_chat_doc(_path: &Path, raw: &str) -> ChatDoc {
    use serde_json::Value;
    let mut doc = ChatDoc {
        text: String::new(),
        title: None,
        cwd: None,
        first_user: None,
        first_ts: None,
        last_ts: None,
    };
    for line in raw.lines() {
        if line.contains("custom-title") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(t) = v.get("customTitle").and_then(Value::as_str) {
                    doc.title = Some(t.to_string());
                }
            }
            continue;
        }
        if doc.cwd.is_none() && line.contains("\"cwd\"") {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                    doc.cwd = Some(c.to_string());
                }
            }
        }
        if !line.contains("\"message\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        // Track the session's activity span from record timestamps (first turn to
        // last), the ground truth for "first/last modified" independent of file mtime.
        if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
            if doc.first_ts.is_none() {
                doc.first_ts = Some(ts.to_string());
            }
            doc.last_ts = Some(ts.to_string());
        }
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let content = v.get("message").and_then(|m| m.get("content"));
        match kind {
            "assistant" => {
                if let Some(arr) = content.and_then(Value::as_array) {
                    for b in arr {
                        match b.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = b.get("text").and_then(Value::as_str) {
                                    doc.text.push_str(t);
                                    doc.text.push('\n');
                                }
                            }
                            Some("thinking") => {
                                if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                                    doc.text.push_str(t);
                                    doc.text.push('\n');
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            "user" => {
                if let Some(s) = content.and_then(Value::as_str) {
                    if doc.first_user.is_none() && !s.trim().is_empty() {
                        doc.first_user = Some(s.to_string());
                    }
                    doc.text.push_str(s);
                    doc.text.push('\n');
                } else if let Some(arr) = content.and_then(Value::as_array) {
                    // Only real user text blocks; skip tool_result content.
                    for b in arr {
                        if b.get("type").and_then(Value::as_str) == Some("text") {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                if doc.first_user.is_none() && !t.trim().is_empty() {
                                    doc.first_user = Some(t.to_string());
                                }
                                doc.text.push_str(t);
                                doc.text.push('\n');
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    doc
}

/// Score a haystack against the query. Returns None on no match. A whole-query
/// substring scores highest; otherwise a cheap fuzzy AND of the query's terms
/// (every word present somewhere) still matches, scored lower.
fn score_text(hay_lower: &str, full: &str, terms: &[String]) -> Option<u32> {
    if !full.is_empty() {
        let sub = hay_lower.matches(full).count();
        if sub > 0 {
            // Phrase tier: always above fuzzy. Whole-word occurrences (the query
            // bounded by non-alphanumerics) are boosted hard so an exact match
            // outranks a substring buried inside a longer word; raw occurrence
            // count then breaks ties so more mentions rank higher.
            let wb = word_bounded_count(hay_lower, full);
            let score = 1_000_000 + (wb as u32) * 10_000 + (sub.min(9_999) as u32);
            return Some(score);
        }
    }
    if terms.len() > 1 && terms.iter().all(|t| hay_lower.contains(t)) {
        let hits: usize = terms.iter().map(|t| hay_lower.matches(t.as_str()).count()).sum();
        return Some(hits.min(999_999) as u32);
    }
    None
}

/// Count occurrences of `needle` in `hay` that sit on word boundaries (the chars
/// immediately before and after are not alphanumeric). Both args are lowercase.
fn word_bounded_count(hay: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let bytes = hay.as_bytes();
    let mut count = 0;
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let at = from + rel;
        let before_ok = at == 0
            || !bytes
                .get(at - 1)
                .map(|c| c.is_ascii_alphanumeric())
                .unwrap_or(false);
        let after = at + needle.len();
        let after_ok = after >= bytes.len()
            || !bytes.get(after).map(|c| c.is_ascii_alphanumeric()).unwrap_or(false);
        if before_ok && after_ok {
            count += 1;
        }
        from = at + needle.len();
    }
    count
}

/// A one-line snippet around the first match, with the matched span roughly
/// centered. `orig` is the original-case text, `lower` its lowercased twin.
fn make_snippet(orig: &str, lower: &str, full: &str, terms: &[String]) -> String {
    let needle = if !full.is_empty() && lower.contains(full) {
        full
    } else {
        terms.iter().find(|t| lower.contains(t.as_str())).map(|s| s.as_str()).unwrap_or("")
    };
    let at = if needle.is_empty() { 0 } else { lower.find(needle).unwrap_or(0) };
    // Byte window around the match, snapped to char boundaries.
    let want = 140usize;
    let mut start = at.saturating_sub(want / 3);
    while start > 0 && !orig.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (at + needle.len() + want).min(orig.len());
    while end < orig.len() && !orig.is_char_boundary(end) {
        end += 1;
    }
    let mut snip: String = orig[start..end].split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 160;
    if snip.chars().count() > MAX {
        snip = snip.chars().take(MAX).collect::<String>() + "…";
    }
    if start > 0 {
        snip.insert(0, '…');
    }
    snip
}

/// Cancel any in-flight search.
pub fn cancel_search() {
    search_cancel().store(true, Ordering::SeqCst);
}

/// Run a search over every session's chat text on a background thread, streaming
/// "search-hit" events as matches are found and "search-progress" throughout.
/// Cancels any prior search first.
pub fn run_search(app: tauri::AppHandle, query: String) {
    // Ask a running search to stop, then wait briefly for it to clear.
    if search_running().load(Ordering::SeqCst) {
        search_cancel().store(true, Ordering::SeqCst);
        for _ in 0..50 {
            if !search_running().load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let full = query.trim().to_lowercase();
    if full.is_empty() {
        return;
    }
    let terms: Vec<String> =
        full.split_whitespace().filter(|s| s.len() >= 2).map(|s| s.to_string()).collect();

    // Claim this run's generation. Any thread still winding down from a prior
    // search holds an older gen and will stop emitting below.
    let my_gen = search_gen().fetch_add(1, Ordering::SeqCst) + 1;
    search_running().store(true, Ordering::SeqCst);
    search_cancel().store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let is_current = || search_gen().load(Ordering::SeqCst) == my_gen;
        let cdir = codex_dir();
        // Hits are labeled with the project a session belongs to, not the folder
        // it ran in, so a subdirectory session reads the same here as in the tree.
        let hit_groups: HashMap<String, grouping::Group> =
            grouping::group_dirs(&scan::known_project_dirs())
                .into_iter()
                .map(|(dir, g)| (dir.to_string_lossy().to_string(), g))
                .collect();

        // Fast path: any session already indexed in chat_fts (enriched + up to date)
        // is served instantly from the index. The set of "fresh" paths is exactly the
        // rows whose stored data still matches the file, and (post the fts_ready
        // migration) exactly the rows that have an FTS entry, so we can partition:
        // fresh -> FTS, everything else -> the disk scan below. If the query has no
        // >=3-char term the trigram index can't help, so FTS is skipped and every
        // file is disk-scanned (fresh not excluded) to keep coverage.
        let fts_expr = fts_match_expr(&terms);
        let conn = open_db().ok();
        let mut fresh: std::collections::HashSet<String> = std::collections::HashSet::new();
        let hits = AtomicU64::new(0);
        if let (Some(conn), Some(expr)) = (&conn, &fts_expr) {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT path FROM sessions
                 WHERE scanned_mtime = mtime AND scanned_size = size_bytes",
            ) {
                if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                    fresh = rows.flatten().collect();
                }
            }
            if !fresh.is_empty() {
                let n = run_fts_hits(&app, conn, expr, &full, &terms, &hit_groups, my_gen);
                hits.fetch_add(n, Ordering::Relaxed);
            }
        }
        drop(conn);

        // Slow path: disk-scan every transcript NOT served by the FTS fast path (all
        // of them when the index is empty or unusable -> the original full scan).
        let pattern = claude_dir()
            .join("projects")
            .join("*")
            .join("*.jsonl")
            .to_string_lossy()
            .replace('\\', "/");
        let mut all_paths: Vec<std::path::PathBuf> =
            glob::glob(&pattern).map(|g| g.flatten().collect()).unwrap_or_default();
        // Codex rollouts too: same search over their plaintext chat (see
        // codex::read_chat_doc). Dispatched per-path below by the codex_dir prefix.
        all_paths.extend(codex::candidates().into_iter().map(|(p, _)| p));
        let paths: Vec<std::path::PathBuf> = all_paths
            .into_iter()
            .filter(|p| !fresh.contains(&p.to_string_lossy().to_string()))
            .collect();
        let total = paths.len() as u64;
        let done = AtomicU64::new(0);
        emit_search(&app, "search", 0, total, hits.load(Ordering::Relaxed));
        // Read + score every transcript IN PARALLEL (std threads, no dep), streaming
        // each hit as it's found. Reads are the bottleneck (I/O + the JSON parse), so
        // chunk the paths across the cores; hits and progress emit straight from the
        // worker threads (AppHandle::emit is Send+Sync; the UI sorts hits by score).
        let nthreads =
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8);
        let chunk = paths.len().div_ceil(nthreads).max(1);
        std::thread::scope(|s| {
            for part in paths.chunks(chunk) {
                let (app, full, terms, cdir, hit_groups, done, hits) =
                    (&app, &full, &terms, &cdir, &hit_groups, &done, &hits);
                s.spawn(move || {
                    for path in part {
                        // A superseded or canceled run stops emitting and quits early.
                        if search_cancel().load(Ordering::Relaxed)
                            || search_gen().load(Ordering::SeqCst) != my_gen
                        {
                            break;
                        }
                        if let Some(hit) = search_one(path, cdir, full, terms, hit_groups) {
                            hits.fetch_add(1, Ordering::Relaxed);
                            if search_gen().load(Ordering::SeqCst) == my_gen {
                                let _ = app.emit("search-hit", hit);
                            }
                        }
                        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if (d % 16 == 0 || d == total)
                            && search_gen().load(Ordering::SeqCst) == my_gen
                        {
                            emit_search(app, "search", d, total, hits.load(Ordering::Relaxed));
                        }
                    }
                });
            }
        });
        // Only report terminal state if we're still the current run: a superseded
        // thread must not flip the new run's progress to done/canceled.
        if is_current() {
            let d = done.load(Ordering::Relaxed);
            let canceled = search_cancel().load(Ordering::Relaxed);
            emit_search(&app, if canceled { "canceled" } else { "done" }, d, total, hits.load(Ordering::Relaxed));
        }
        search_running().store(false, Ordering::SeqCst);
    });
}

/// Build an FTS5 MATCH expression from the query terms: each term of >=3 chars
/// (the trigram floor) quoted (so operators/quotes in the term are literal) and
/// AND-ed. Returns None if no term is long enough to index by trigram, in which
/// case the caller must fall back to a full disk scan for coverage.
fn fts_match_expr(terms: &[String]) -> Option<String> {
    let parts: Vec<String> = terms
        .iter()
        .filter(|t| t.chars().count() >= 3)
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

/// Stream hits for the already-indexed ("fresh") sessions straight from `chat_fts`:
/// the trigram MATCH narrows to candidates instantly, then the SAME scorer/snippet
/// as the disk path runs over the stored text (no file reads). Only fresh rows are
/// returned (stale/never-enriched sessions are covered by the disk scan), so hits
/// never double up. Returns the number emitted.
#[allow(clippy::too_many_arguments)]
fn run_fts_hits(
    app: &tauri::AppHandle,
    conn: &Connection,
    expr: &str,
    full: &str,
    terms: &[String],
    hit_groups: &HashMap<String, grouping::Group>,
    my_gen: u64,
) -> u64 {
    let mut stmt = match conn.prepare(
        "SELECT s.session_id, s.project_path, s.harness, s.title, s.mtime, s.size_bytes,
                s.first_ms, s.last_ms, f.text
         FROM chat_fts f JOIN sessions s ON s.rowid = f.rowid
         WHERE f.text MATCH ?1
           AND s.scanned_mtime = s.mtime AND s.scanned_size = s.size_bytes",
    ) {
        Ok(s) => s,
        Err(e) => {
            blog!("fts prepare failed: {e}");
            return 0;
        }
    };
    let rows = stmt.query_map([expr], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, i64>(4)?,
            r.get::<_, i64>(5)? as u64,
            r.get::<_, Option<i64>>(6)?,
            r.get::<_, Option<i64>>(7)?,
            r.get::<_, String>(8)?,
        ))
    });
    let Ok(rows) = rows else { return 0 };
    let mut n = 0u64;
    for row in rows.flatten() {
        if search_cancel().load(Ordering::Relaxed) || search_gen().load(Ordering::SeqCst) != my_gen {
            break;
        }
        let (id, project_path, harness, title, mtime, size_bytes, first_ms, last_ms, text) = row;
        let lower = text.to_lowercase();
        let Some(score) = score_text(&lower, full, terms) else { continue };
        let project = hit_groups
            .get(&project_path)
            .map(|g| g.label.clone())
            .unwrap_or_else(|| scan::last_component(&project_path));
        let title = if title.is_empty() { project.clone() } else { title };
        let snippet = make_snippet(&text, &lower, full, terms);
        let hit = SearchHit {
            id,
            title,
            project,
            project_path,
            size_bytes,
            mtime,
            first_ms: first_ms.unwrap_or(mtime),
            last_ms: last_ms.unwrap_or(mtime),
            score,
            snippet,
            harness,
        };
        if search_gen().load(Ordering::SeqCst) == my_gen {
            let _ = app.emit("search-hit", hit);
            n += 1;
        }
    }
    n
}

/// Read one transcript, cheaply pre-filter it, and (on a match) build its hit. The
/// pre-filter is the speed win: if not one query term appears anywhere in the raw
/// file bytes, the chat text (a subset) can't match, so we skip the per-line JSON
/// parse entirely. Only surviving files get parsed to chat-only text and scored.
fn search_one(
    path: &Path,
    cdir: &Path,
    full: &str,
    terms: &[String],
    hit_groups: &HashMap<String, grouping::Group>,
) -> Option<SearchHit> {
    let raw = std::fs::read_to_string(path).ok()?;
    let raw_lower = raw.to_lowercase();
    let present = if !full.is_empty() && raw_lower.contains(full) {
        true
    } else {
        terms.iter().any(|t| raw_lower.contains(t))
    };
    if !present {
        return None;
    }
    let is_codex = path.starts_with(cdir);
    let doc = if is_codex { codex::read_chat_doc(path, &raw) } else { read_chat_doc(path, &raw) };
    let lower = doc.text.to_lowercase();
    let score = score_text(&lower, full, terms)?;
    let id = if is_codex {
        codex::id_from_path(path)
    } else {
        path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
    };
    let (mtime, size) = file_stat(path).unwrap_or((0, 0));
    let last_ms = doc.last_ts.as_deref().and_then(scan::iso_to_millis).unwrap_or(mtime);
    let first_ms = doc.first_ts.as_deref().and_then(scan::iso_to_millis).unwrap_or(last_ms);
    let project_path = doc.cwd.clone().unwrap_or_else(|| {
        let dir = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        scan::decode_project_dir(&dir)
    });
    // Codex rollouts sit under a dated folder, not an encoded project dir, so group by
    // the session's own cwd (its real project) instead.
    let decoded = if is_codex {
        project_path.clone()
    } else {
        path.parent()
            .and_then(|p| p.file_name())
            .map(|s| scan::decode_project_dir(&s.to_string_lossy()))
            .unwrap_or_default()
    };
    let project = hit_groups
        .get(&decoded)
        .map(|g| g.label.clone())
        .unwrap_or_else(|| scan::last_component(&project_path));
    let title = doc
        .title
        .clone()
        .or_else(|| doc.first_user.as_deref().map(short_title))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| project.clone());
    let snippet = make_snippet(&doc.text, &lower, full, terms);
    Some(SearchHit {
        id,
        title,
        project,
        project_path,
        size_bytes: size,
        mtime,
        first_ms,
        last_ms,
        score,
        snippet,
        harness: if is_codex { "codex".into() } else { HARNESS.into() },
    })
}

fn emit_search(app: &tauri::AppHandle, phase: &'static str, done: u64, total: u64, hits: u64) {
    let _ = app.emit("search-progress", SearchProgress { phase, done, total, hits });
}

/// First line / clause of a user message, trimmed to a short title.
fn short_title(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 60;
    if flat.chars().count() > MAX {
        flat.chars().take(MAX).collect::<String>() + "…"
    } else {
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(ws: &[&str]) -> Vec<String> {
        ws.iter().map(|s| s.to_string()).collect()
    }

    // --- fts_match_expr ---

    #[test]
    fn fts_match_expr_drops_short_terms_and_ands_the_rest() {
        // "ab" is below the 3-char trigram floor and is dropped; "grep" survives.
        assert_eq!(fts_match_expr(&terms(&["ab", "grep"])), Some("\"grep\"".to_string()));
        assert_eq!(
            fts_match_expr(&terms(&["foo", "bar"])),
            Some("\"foo\" AND \"bar\"".to_string())
        );
    }

    #[test]
    fn fts_match_expr_none_when_no_term_qualifies() {
        assert_eq!(fts_match_expr(&terms(&["ab", "cd"])), None);
        assert_eq!(fts_match_expr(&terms(&[])), None);
    }

    #[test]
    fn fts_match_expr_doubles_embedded_quotes() {
        // An embedded double-quote is escaped by doubling so it stays literal in MATCH.
        assert_eq!(
            fts_match_expr(&terms(&["he\"llo"])),
            Some("\"he\"\"llo\"".to_string())
        );
    }

    // --- score_text ---

    #[test]
    fn score_text_phrase_outranks_fuzzy() {
        let phrase = score_text("ripgrep is a grep tool", "grep", &terms(&["grep"]))
            .expect("phrase match");
        let fuzzy = score_text("foo then bar", "", &terms(&["foo", "bar"])).expect("fuzzy match");
        assert!(phrase >= 1_000_000, "phrase tier is >= 1,000,000");
        assert!(fuzzy < 1_000_000, "fuzzy tier is below the phrase tier");
        assert!(phrase > fuzzy);
    }

    #[test]
    fn score_text_word_bounded_outranks_buried_substring() {
        // Standalone "grep" (word-bounded) must outrank "grep" buried inside "ripgrepx".
        let bounded = score_text("grep", "grep", &terms(&["grep"])).unwrap();
        let buried = score_text("ripgrepx", "grep", &terms(&["grep"])).unwrap();
        assert!(bounded > buried, "bounded {bounded} should beat buried {buried}");
    }

    #[test]
    fn score_text_none_when_absent() {
        assert!(score_text("nothing relevant here", "xyz", &terms(&["xyz"])).is_none());
        // Single fuzzy term that isn't present, no phrase: None.
        assert!(score_text("alpha beta", "", &terms(&["gamma"])).is_none());
    }

    #[test]
    fn word_bounded_count_ignores_substring_hits() {
        // "grep" standalone twice; the one inside "ripgrep" does not count.
        assert_eq!(word_bounded_count("grep ripgrep grep", "grep"), 2);
        assert_eq!(word_bounded_count("", "grep"), 0);
        assert_eq!(word_bounded_count("anything", ""), 0);
    }

    // --- make_snippet ---

    #[test]
    fn make_snippet_includes_the_match() {
        let orig = "hello world grep here";
        let lower = orig.to_lowercase();
        let snip = make_snippet(orig, &lower, "grep", &terms(&["grep"]));
        assert!(snip.contains("grep"), "snippet must contain the matched term: {snip}");
    }

    #[test]
    fn make_snippet_marks_leading_ellipsis_when_match_is_deep() {
        let prefix = "a".repeat(120);
        let orig = format!("{prefix} grep tail");
        let lower = orig.to_lowercase();
        let snip = make_snippet(&orig, &lower, "grep", &terms(&["grep"]));
        assert!(snip.starts_with('…'), "a deep match gets a leading ellipsis: {snip}");
        assert!(snip.contains("grep"));
    }

    // --- small pure helpers ---

    #[test]
    fn placeholders_are_one_indexed_from_two() {
        assert_eq!(placeholders(1), "?2");
        assert_eq!(placeholders(3), "?2,?3,?4");
    }

    #[test]
    fn harness_label_maps_known_harnesses() {
        assert_eq!(harness_label("claude-code"), "Claude Code");
        assert_eq!(harness_label("codex"), "Codex");
        assert_eq!(harness_label("weird"), "weird");
    }

    #[test]
    fn short_title_flattens_and_truncates() {
        assert_eq!(short_title("  hello   world  "), "hello world");
        let long = "word ".repeat(40);
        let t = short_title(&long);
        assert!(t.ends_with('…'));
        assert_eq!(t.chars().count(), 61); // 60 chars + ellipsis
    }

    // --- read_chat_doc (Claude) ---

    #[test]
    fn read_chat_doc_collects_chat_text_but_skips_tool_results() {
        let user = r#"{"type":"user","timestamp":"2026-09-10T01:00:00.000Z","message":{"content":"find the bug"}}"#;
        let asst = r#"{"type":"assistant","timestamp":"2026-09-10T01:00:05.000Z","message":{"content":[{"type":"text","text":"here is the fix"},{"type":"thinking","thinking":"pondering deeply"}]}}"#;
        let tool = r#"{"type":"user","timestamp":"2026-09-10T01:00:06.000Z","message":{"content":[{"type":"tool_result","content":"SECRETTOOLOUTPUT"}]}}"#;
        let raw = format!("{user}\n{asst}\n{tool}\n");
        let doc = read_chat_doc(std::path::Path::new("x.jsonl"), &raw);
        assert!(doc.text.contains("find the bug"));
        assert!(doc.text.contains("here is the fix"));
        assert!(doc.text.contains("pondering deeply"));
        assert!(!doc.text.contains("SECRETTOOLOUTPUT"), "tool_result content must be excluded");
        assert_eq!(doc.first_user.as_deref(), Some("find the bug"));
        assert_eq!(doc.first_ts.as_deref(), Some("2026-09-10T01:00:00.000Z"));
        assert_eq!(doc.last_ts.as_deref(), Some("2026-09-10T01:00:06.000Z"));
    }

    // --- dedup_first_by_msg_id (pure) ---

    #[test]
    fn dedup_first_by_msg_id_keeps_first_per_id() {
        // (msg_id, tag) rows in winner-first order: first "a" wins, later "a" drops.
        let rows = vec![
            (Some("a".to_string()), 1),
            (Some("b".to_string()), 2),
            (Some("a".to_string()), 3),
            (Some("b".to_string()), 4),
        ];
        let out = dedup_first_by_msg_id(rows, |r| r.0.as_deref());
        let tags: Vec<i32> = out.iter().map(|r| r.1).collect();
        assert_eq!(tags, vec![1, 2]); // first a, first b
    }

    #[test]
    fn dedup_first_by_msg_id_keeps_all_none_ids() {
        // Every None-msg_id row survives even though they collide as a group.
        let rows = vec![
            (None::<String>, 1),
            (Some("a".to_string()), 2),
            (None::<String>, 3),
            (Some("a".to_string()), 4),
            (None::<String>, 5),
        ];
        let out = dedup_first_by_msg_id(rows, |r| r.0.as_deref());
        let tags: Vec<i32> = out.iter().map(|r| r.1).collect();
        assert_eq!(tags, vec![1, 2, 3, 5]); // all Nones + first "a", input order preserved
    }

    // --- spend_summary_conn / spend_day_conn (temp sqlite, no new deps) ---

    fn temp_db() -> (Connection, std::path::PathBuf) {
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "greedout-spend-test-{}-{:?}.sqlite",
            std::process::id(),
            std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        );
        p.push(uniq);
        let conn = Connection::open(&p).unwrap();
        init_db(&conn).unwrap();
        (conn, p)
    }

    fn insert_session(conn: &Connection, path: &str, sid: &str, project: &str) {
        conn.execute(
            "INSERT INTO sessions (path, harness, session_id, project, project_path, mtime, title)
             VALUES (?1, 'claude-code', ?2, ?3, ?4, 0, '')",
            rusqlite::params![path, sid, project, format!("/p/{project}")],
        )
        .unwrap();
    }

    fn insert_turn(conn: &Connection, path: &str, msg_id: Option<&str>, ts: i64, cost: f64) {
        conn.execute(
            "INSERT INTO turns (session_path, msg_id, ts, cost) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![path, msg_id, ts, cost],
        )
        .unwrap();
    }

    #[test]
    fn spend_summary_and_day_dedup_and_bucket() {
        let (conn, path) = temp_db();
        // The summary buckets by SQLite 'localtime' (depends on the OS zone), so we
        // assert the day is a well-formed YYYY-MM-DD rather than a hard-coded value.
        // All turns are within 30ms of `base`, so they cannot straddle midnight.
        let base: i64 = 1_760_000_000_000; // arbitrary ms in range
        insert_session(&conn, "sA", "sessA", "proj");
        insert_session(&conn, "sB", "sessB", "proj");
        // Same msg_id "m1" appears in two session_paths (a resume copy). sA sorts
        // before sB, so sA's copy is the winner; sB's is dropped.
        insert_turn(&conn, "sA", Some("m1"), base, 1.0);
        insert_turn(&conn, "sB", Some("m1"), base + 5, 1.0); // duplicate, dropped
        // A distinct billed turn and a null-msg_id turn (both kept).
        insert_turn(&conn, "sA", Some("m2"), base + 10, 2.0);
        insert_turn(&conn, "sB", None, base + 20, 0.5);
        // A zero-cost row is filtered out entirely.
        insert_turn(&conn, "sA", Some("m3"), base + 30, 0.0);

        // Summary: one project, one day, cost = 1.0 (m1 once) + 2.0 (m2) + 0.5 (null) = 3.5
        let summary = spend_summary_conn(&conn).unwrap();
        assert_eq!(summary.len(), 1, "one (day, project) bucket");
        assert_eq!(summary[0].project, "proj");
        assert_eq!(summary[0].day.len(), 10, "day is YYYY-MM-DD");
        assert_eq!(summary[0].day.matches('-').count(), 2, "day is YYYY-MM-DD");
        assert!((summary[0].cost - 3.5).abs() < 1e-9, "cost was {}", summary[0].cost);

        // Day range covering the whole thing: three deduped events (m1, m2, null),
        // sorted by ts ascending.
        let events = spend_day_conn(&conn, base - 100, base + 1000).unwrap();
        let costs: Vec<f64> = events.iter().map(|e| e.cost).collect();
        assert_eq!(costs, vec![1.0, 2.0, 0.5], "deduped + ts-sorted");
        assert!(events.iter().all(|e| e.t >= base && e.t <= base + 20));

        // A narrow range excludes turns outside it.
        let none = spend_day_conn(&conn, base + 100, base + 1000).unwrap();
        assert!(none.is_empty(), "range past all turns yields nothing");

        drop(conn);
        let _ = std::fs::remove_file(path);
    }
}
