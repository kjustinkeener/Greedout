//! Opt-in cross-session index: browse context across every project, not just the
//! `n` most-recent sessions the poll loop tracks.
//!
//! The whole feature is gated behind `config.browse_enabled`. Turning it on is
//! what creates `~/.claude/greedout/cache.sqlite` -- so the user knowingly opts
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

use crate::config::{cache_db_path, claude_dir};
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
            updated_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_hp ON sessions(harness, project);
         CREATE INDEX IF NOT EXISTS idx_sid ON sessions(session_id);
         CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);",
    )
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
        let result = scan_worker(&app, deep);
        running().store(false, Ordering::SeqCst);
        let phase = if cancel_flag().load(Ordering::Relaxed) {
            "canceled"
        } else {
            "done"
        };
        let total = result.unwrap_or(0);
        let _ = app.emit(
            "browse-progress",
            ScanProgress { phase, done: total, total, current: String::new() },
        );
    });
}

pub fn cancel() {
    cancel_flag().store(true, Ordering::SeqCst);
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
        // Drop rows whose transcript vanished.
        {
            let mut stmt = tx.prepare("SELECT path FROM sessions")?;
            let existing: Vec<String> =
                stmt.query_map([], |r| r.get::<_, String>(0))?.flatten().collect();
            let mut del = tx.prepare("DELETE FROM sessions WHERE path=?1")?;
            for p in existing {
                if !seen.contains(&p) {
                    del.execute([&p])?;
                }
            }
        }
        tx.commit()?;
    }
    emit_progress(app, "index", total, total, "");

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
    // Parallel read/parse. `thread::scope` lets the threads borrow `done`/`app`
    // without 'static bounds; each returns its enriched rows.
    let results: Vec<EnrichRow> = std::thread::scope(|s| {
        let handles: Vec<_> = stale
            .chunks(chunk.max(1))
            .map(|part| {
                let done = &done;
                s.spawn(move || {
                    let mut out = Vec::with_capacity(part.len());
                    for (path, id) in part {
                        if cancel_flag().load(Ordering::Relaxed) {
                            break;
                        }
                        let p = Path::new(path);
                        let (mtime, size) = file_stat(p).unwrap_or((0, 0));
                        let meta = scan::enrich_meta(p);
                        out.push(EnrichRow { path: path.clone(), meta, mtime, size });
                        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                        if throttle <= 1 || d % throttle == 0 || d == etotal {
                            emit_progress(app, "enrich", d, etotal, id);
                        }
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    });
    // Serial batched write: one fsync per ~200 rows instead of per row.
    let mut n = 0u64;
    let mut tx = conn.transaction()?;
    for r in &results {
        write_enrich(&tx, &r.path, &r.meta, r.mtime, r.size)?;
        n += 1;
        if n % 200 == 0 {
            tx.commit()?;
            tx = conn.transaction()?;
        }
    }
    tx.commit()?;
    Ok(n)
}

/// Write one transcript's Pass-2 columns from its already-parsed metadata.
fn write_enrich(
    conn: &Connection,
    path_str: &str,
    m: &scan::EnrichMeta,
    mtime: i64,
    size: u64,
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
            project=COALESCE(?10, project), project_path=COALESCE(?11, project_path)
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
        ],
    )?;
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
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT path, project FROM sessions WHERE session_id=?1 LIMIT 1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .ok();
    let (path, project) = row?;
    let p = Path::new(&path);
    let (mtime, size) = file_stat(p).unwrap_or((0, 0));
    let _ = write_enrich(&conn, &path, &scan::enrich_meta(p), mtime, size);
    read_sessions(&conn, HARNESS, &project)
        .into_iter()
        .find(|s| s.id == id)
}

fn harness_label(h: &str) -> String {
    match h {
        "claude-code" => "Claude Code".to_string(),
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
/// title and cwd, from a single file read. Used only for search.
struct ChatDoc {
    text: String,
    title: Option<String>,
    cwd: Option<String>,
    first_user: Option<String>,
    first_ts: Option<String>,
    last_ts: Option<String>,
}

fn read_chat_doc(path: &Path) -> ChatDoc {
    use serde_json::Value;
    let mut doc = ChatDoc {
        text: String::new(),
        title: None,
        cwd: None,
        first_user: None,
        first_ts: None,
        last_ts: None,
    };
    let Ok(raw) = std::fs::read_to_string(path) else { return doc };
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
        let pattern = claude_dir()
            .join("projects")
            .join("*")
            .join("*.jsonl")
            .to_string_lossy()
            .replace('\\', "/");
        let paths: Vec<std::path::PathBuf> =
            glob::glob(&pattern).map(|g| g.flatten().collect()).unwrap_or_default();
        let total = paths.len() as u64;
        // Hits are labeled with the project a session belongs to, not the folder
        // it ran in, so a subdirectory session reads the same here as in the tree.
        let hit_groups: HashMap<String, grouping::Group> =
            grouping::group_dirs(&scan::known_project_dirs())
                .into_iter()
                .map(|(dir, g)| (dir.to_string_lossy().to_string(), g))
                .collect();
        let mut done = 0u64;
        let mut hits = 0u64;
        emit_search(&app, "search", 0, total, 0);
        for path in &paths {
            if search_cancel().load(Ordering::Relaxed) || !is_current() {
                break;
            }
            done += 1;
            let doc = read_chat_doc(path);
            let lower = doc.text.to_lowercase();
            if let Some(score) = score_text(&lower, &full, &terms) {
                hits += 1;
                let id = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let (mtime, size) = file_stat(path).unwrap_or((0, 0));
                let mtime_ms = mtime; // file_stat already returns millis
                let last_ms = doc
                    .last_ts
                    .as_deref()
                    .and_then(scan::iso_to_millis)
                    .unwrap_or(mtime_ms);
                let first_ms = doc
                    .first_ts
                    .as_deref()
                    .and_then(scan::iso_to_millis)
                    .unwrap_or(last_ms);
                let project_path = doc.cwd.clone().unwrap_or_else(|| {
                    let dir = path
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    scan::decode_project_dir(&dir)
                });
                let decoded = path
                    .parent()
                    .and_then(|p| p.file_name())
                    .map(|s| scan::decode_project_dir(&s.to_string_lossy()))
                    .unwrap_or_default();
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
                let snippet = make_snippet(&doc.text, &lower, &full, &terms);
                if is_current() {
                    let _ = app.emit(
                        "search-hit",
                        SearchHit { id, title, project, project_path, size_bytes: size, mtime, first_ms, last_ms, score, snippet },
                    );
                }
            }
            if (done % 10 == 0 || done == total) && is_current() {
                emit_search(&app, "search", done, total, hits);
            }
        }
        // Only report terminal state if we're still the current run: a superseded
        // thread must not flip the new run's progress to done/canceled.
        if is_current() {
            let canceled = search_cancel().load(Ordering::Relaxed);
            emit_search(&app, if canceled { "canceled" } else { "done" }, done, total, hits);
        }
        search_running().store(false, Ordering::SeqCst);
    });
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
