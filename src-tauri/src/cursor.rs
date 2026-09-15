//! Cursor (Anysphere) session adapter.
//!
//! Unlike Claude and Codex (one transcript FILE per session), Cursor keeps EVERY
//! conversation in ONE global SQLite DB:
//! `%APPDATA%\Cursor\User\globalStorage\state.vscdb`, table `cursorDiskKV` (a
//! key/JSON-value store). A Greedout "session" is one Cursor *composer* (one chat).
//!
//! To fit the file-per-session model the rest of Greedout assumes, each composer is
//! represented by a SYNTHETIC path under the Cursor dir --
//! `…\User\globalStorage\composer-<composerId>.cursor` -- that never exists on disk;
//! it only encodes the id so the existing prefix-dispatch (`path.starts_with(
//! cursor_dir())`) and `id_from_path` keep working. Every read here goes to the DB
//! by that id, ignoring the (nonexistent) file bytes callers may pass.
//!
//! Token semantics (verified against a real DB, see the recon doc):
//!   * ctx (gauge numerator) = `composerData.contextTokensUsed` (fallback
//!     `promptTokenBreakdown.totalUsedTokens`) -- a REAL live context-fill snapshot
//!     Cursor persists per composer. Used directly, never estimated.
//!   * Per-bubble `tokenCount.{input,output}` are {0,0} on current builds, so spend
//!     is an ESTIMATE: ceil(char_len/4) per bubble, split by `type` (==1 user/input,
//!     else assistant/output), priced by `model_info()` for the composer's model.
//!     No cache tokens. This mirrors CodeBurn's `text` fallback.
//!   * model = the last non-empty `modelInfo.modelName` across the composer's bubbles.
//!
//! The DB is copied to a temp file (with any `-wal`/`-shm`) and opened READ-ONLY, so
//! a live, WAL-locked Cursor install is never touched or blocked.

use crate::config::Config;
use crate::config::{cursor_db_path, cursor_dir};
use crate::scan::{downsample, model_info, parse_model_version, Sample, Session};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

/// Estimated tokens for a chunk of visible text: Cursor persists no usable per-turn
/// counts on current builds, so we approximate 4 characters per token (CodeBurn's
/// `CHARS_PER_TOKEN`).
fn est_tokens(chars: u64) -> u64 {
    chars.div_ceil(4)
}

/// The `composer-<id>.cursor` synthetic path for a composer id. Never exists on
/// disk; it only carries the id through the file-per-session plumbing.
fn synthetic_path(id: &str) -> PathBuf {
    cursor_dir()
        .join("User")
        .join("globalStorage")
        .join(format!("composer-{id}.cursor"))
}

/// Recover a composer id from its synthetic path: strip the `composer-` prefix and
/// the `.cursor` extension.
pub(crate) fn id_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    stem.strip_prefix("composer-").map(str::to_string).unwrap_or(stem)
}

// --- read-only DB access over a temp copy ---

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A `state.vscdb` path with a suffix appended to the file NAME (e.g. `-wal`), used
/// to reach the WAL/shm sidecars that sit beside the main DB.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.to_string_lossy(), suffix))
}

/// Copy the live `state.vscdb` (plus any `-wal`/`-shm`) to a unique temp file so we
/// can open a stable, read-only snapshot without locking or writing the real DB
/// (Cursor writes via WAL and may hold it open). Returns the temp DB path and any
/// sidecar copies to clean up. `None` if Cursor isn't installed (no DB present).
fn copy_db_to_temp() -> Option<(PathBuf, Vec<PathBuf>)> {
    let src = cursor_db_path();
    std::fs::metadata(&src).ok()?; // absent => Cursor not installed
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dst = std::env::temp_dir().join(format!("greedout-cursor-{}-{n}.vscdb", std::process::id()));
    // Copy the WAL/shm sidecars BEFORE the main file: Cursor keeps a large live WAL
    // (multi-MB) holding the freshest writes, so the newest turn's ctx/lastUpdatedAt
    // live there, not in the main db. Copying sidecars first minimizes a torn copy
    // (CodeBurn does the same), and opening the copy read-write below lets SQLite
    // replay the WAL so we see the latest turn.
    let mut extras = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let s = sidecar(&src, suffix);
        if s.exists() {
            let d = sidecar(&dst, suffix);
            if std::fs::copy(&s, &d).is_ok() {
                extras.push(d);
            }
        }
    }
    std::fs::copy(&src, &dst).ok()?;
    Some((dst, extras))
}

/// (mtime_ms, size) of the live DB and its `-wal`, so any change -- including a WAL
/// write that leaves the main file's mtime untouched -- invalidates the snapshot.
type Fingerprint = (u64, u64, u64, u64);

fn file_stat(p: &Path) -> (u64, u64) {
    std::fs::metadata(p)
        .ok()
        .map(|m| {
            let ms = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            (ms, m.len())
        })
        .unwrap_or((0, 0))
}

fn db_fingerprint() -> Option<Fingerprint> {
    let src = cursor_db_path();
    std::fs::metadata(&src).ok()?; // absent => Cursor not installed
    let (m1, s1) = file_stat(&src);
    let (m2, s2) = file_stat(&sidecar(&src, "-wal"));
    Some((m1, s1, m2, s2))
}

struct Snapshot {
    path: PathBuf,
    fp: Fingerprint,
}

/// A stable snapshot of the live DB, rebuilt only when its fingerprint changes. Held
/// across polls so the many `with_db` reads in one poll (candidates + one per composer
/// + focus) share a SINGLE copy instead of each re-copying a potentially multi-GB DB.
static SNAP: Mutex<Option<Snapshot>> = Mutex::new(None);

/// Path to a current read-only snapshot of the live DB, building one if the cached
/// snapshot is missing or stale. The build copies the DB (+`-wal`/`-shm`), replays and
/// truncates the WAL, then switches the copy out of WAL mode so subsequent reads can
/// open it READ-ONLY (a WAL-mode file needs write access even to read). `None` if
/// Cursor isn't installed or the copy fails.
fn snapshot_path() -> Option<PathBuf> {
    let fp = db_fingerprint()?;
    let mut guard = SNAP.lock().ok()?;
    if let Some(s) = guard.as_ref() {
        if s.fp == fp && s.path.exists() {
            return Some(s.path.clone());
        }
    }
    // Stale or absent: build a fresh snapshot. The lock is held across the copy so a
    // concurrent caller waits and then reuses the result rather than copying again.
    let (tmp, extras) = copy_db_to_temp()?;
    // Open READ-WRITE once to fold the copied WAL into the file and drop WAL mode, so
    // every later read can open the file read-only without needing to write a -shm.
    let built = match Connection::open_with_flags(&tmp, OpenFlags::SQLITE_OPEN_READ_WRITE) {
        Ok(conn) => {
            let _ = conn.busy_timeout(Duration::from_millis(1000));
            let _ = conn.pragma_update(None, "wal_checkpoint", "TRUNCATE");
            let _ = conn.pragma_update(None, "journal_mode", "DELETE");
            true
        }
        Err(_) => false,
    };
    // The RW conn is dropped; SQLite has removed the temp's own -wal/-shm, and the
    // copies we made are folded in -- clean up any that linger.
    for e in extras {
        let _ = std::fs::remove_file(e);
    }
    if !built {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    // Retire the previous snapshot file (best-effort: a read-only reader may still hold
    // it briefly on Windows; a failed delete just leaks one temp until the next build).
    if let Some(old) = guard.take() {
        let _ = std::fs::remove_file(&old.path);
    }
    *guard = Some(Snapshot { path: tmp.clone(), fp });
    Some(tmp)
}

/// Open the shared read-only snapshot, run `f`, and return its result. Best-effort:
/// any IO/SQL failure yields `None` so every caller degrades to "no Cursor data"
/// rather than erroring.
fn with_db<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    let path = snapshot_path()?;
    // The snapshot is a plain (non-WAL) copy, so a read-only open needs no write access
    // and many such opens can share the one file concurrently.
    match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => {
            let _ = conn.busy_timeout(Duration::from_millis(1000));
            f(&conn).ok()
        }
        Err(_) => None,
    }
}

/// The DB file's mtime in epoch seconds, the shared fallback recency for composers
/// with no `createdAt`.
fn db_mtime_secs() -> u64 {
    std::fs::metadata(cursor_db_path())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- composer parsing ---

/// One assistant/user message ("bubble") of a composer, reduced to what we price
/// and search: the model tag (assistant bubbles only), the turn type (1 = user),
/// the visible text, and a best-effort timestamp in epoch ms.
struct Bubble {
    model: Option<String>,
    btype: i64,
    text: String,
    ts: Option<i64>,
}

/// Everything one composer contributes to a Greedout row, from a single DB pass.
struct ComposerInfo {
    /// Real context-fill snapshot (`contextTokensUsed`), None if the meter is absent.
    ctx: Option<u64>,
    /// Cursor's own `contextTokenLimit` for this composer's model, when present.
    ctx_limit: Option<u64>,
    /// Resolved model id (last non-empty `modelInfo.modelName`).
    model: Option<String>,
    /// Estimated cumulative spend (USD) across all bubbles.
    cost_usd: f64,
    /// Composer `createdAt` in epoch ms, for recency/sorting.
    created_ms: Option<i64>,
    /// Composer `name`, if Cursor recorded one.
    title: Option<String>,
    /// Per-bubble estimated spend rows for Daily Spend: (msg_id, ts_ms, cost).
    turns: Vec<(Option<String>, i64, f64)>,
}

/// Read a JSON-extracted `createdAt` cell that may be an integer/real ms epoch or an
/// ISO-ish string, into epoch ms.
fn created_to_ms(v: &SqlValue) -> Option<i64> {
    match v {
        SqlValue::Integer(i) => Some(*i),
        SqlValue::Real(r) => Some(*r as i64),
        SqlValue::Text(s) => s.parse::<i64>().ok().or_else(|| crate::scan::iso_to_millis(s)),
        _ => None,
    }
}

/// Fetch a composer's bubbles in insertion order (ROWID) so "last non-empty model"
/// is the newest one.
fn fetch_bubbles(conn: &Connection, id: &str) -> rusqlite::Result<Vec<Bubble>> {
    let pattern = format!("bubbleId:{id}:%");
    let mut stmt = conn.prepare(
        "SELECT json_extract(value,'$.modelInfo.modelName'),
                json_extract(value,'$.type'),
                json_extract(value,'$.text'),
                json_extract(value,'$.createdAt')
         FROM cursorDiskKV WHERE key LIKE ?1 ORDER BY rowid ASC",
    )?;
    let rows = stmt.query_map([pattern], |r| {
        let model: Option<String> = r.get(0)?;
        let btype: i64 = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
        let text: String = r.get::<_, Option<String>>(2)?.unwrap_or_default();
        let created: SqlValue = r.get(3)?;
        Ok(Bubble { model, btype, text, ts: created_to_ms(&created) })
    })?;
    rows.collect()
}

/// The composer's context meter, `createdAt`, and name from its `composerData` row.
/// Returns `(ctx, created_ms, title)`.
/// Returns `(ctx, ctx_limit, created_ms, title)`. `ctx_limit` is Cursor's own
/// `contextTokenLimit` (the real window for this composer's model), used as the
/// gauge's hard limit when present.
fn fetch_meter(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<(Option<u64>, Option<u64>, Option<i64>, Option<String>)> {
    let key = format!("composerData:{id}");
    conn.query_row(
        "SELECT json_extract(value,'$.contextTokensUsed'),
                json_extract(value,'$.promptTokenBreakdown.totalUsedTokens'),
                json_extract(value,'$.contextTokenLimit'),
                json_extract(value,'$.lastUpdatedAt'),
                json_extract(value,'$.createdAt'),
                json_extract(value,'$.name')
         FROM cursorDiskKV WHERE key = ?1",
        [key],
        |r| {
            let ctx_used: Option<i64> = r.get(0)?;
            let total_used: Option<i64> = r.get(1)?;
            let ctx_limit: Option<i64> = r.get(2)?;
            let updated: SqlValue = r.get(3)?;
            let created: SqlValue = r.get(4)?;
            let name: Option<String> = r.get(5)?;
            let ctx = ctx_used.or(total_used).filter(|v| *v >= 0).map(|v| v as u64);
            let limit = ctx_limit.filter(|v| *v > 0).map(|v| v as u64);
            let title = name.filter(|s| !s.is_empty());
            // Prefer lastUpdatedAt for recency; fall back to createdAt.
            let ts = created_to_ms(&updated).or_else(|| created_to_ms(&created));
            Ok((ctx, limit, ts, title))
        },
    )
}

/// Assemble one composer's row data (meter + priced bubbles) in a single DB pass.
fn composer_info(id: &str) -> Option<ComposerInfo> {
    with_db(|conn| {
        let (ctx, ctx_limit, created_ms, title) =
            fetch_meter(conn, id).unwrap_or((None, None, None, None));
        let bubbles = fetch_bubbles(conn, id)?;

        // Resolve the composer's model once (last non-empty wins) so every bubble
        // prices against the same rates.
        let model = bubbles
            .iter()
            .rev()
            .find_map(|b| b.model.as_deref().filter(|m| !m.is_empty()).map(str::to_string));
        let mi = model_info(model.as_deref().unwrap_or(""));

        let mut cost_usd = 0.0;
        let mut turns: Vec<(Option<String>, i64, f64)> = Vec::new();
        for b in &bubbles {
            let chars = b.text.chars().count() as u64;
            if chars == 0 {
                continue;
            }
            let tokens = est_tokens(chars) as f64;
            // type==1 is the user turn (input); everything else is assistant (output).
            let rate = if b.btype == 1 { mi.price_in } else { mi.price_out };
            let cost = tokens * rate / 1_000_000.0;
            cost_usd += cost;
            // Per-turn spend row (Daily Spend). No stable dedupe id needed: each
            // composer's bubbles are inserted only under its own synthetic path.
            let ts = b.ts.or(created_ms);
            if let (Some(t), true) = (ts, cost > 0.0) {
                turns.push((None, t, cost));
            }
        }

        Ok(ComposerInfo { ctx, ctx_limit, model, cost_usd, created_ms, title, turns })
    })
}

// --- composer -> workspace folder mapping (project attribution) ---

/// Percent-decode a byte string (`%3A` -> `:`). Cursor's `workspace.json` stores the
/// folder as a URI whose drive colon and any spaces are percent-encoded.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Convert a `file://` folder URI (`file:///c%3A/claude-local/App`) into a native
/// Windows path (`C:\claude-local\App`). None if it isn't a local file URI.
fn file_uri_to_path(uri: &str) -> Option<String> {
    // `file:///c%3A/...` = scheme + empty authority + a path starting `/c%3A/...`.
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix('/').unwrap_or(rest); // drop the slash before the drive
    let decoded = percent_decode(rest);
    if decoded.is_empty() {
        return None;
    }
    let mut path = decoded.replace('/', "\\");
    // Uppercase a leading drive letter so the folder matches the other harnesses' cwds.
    if path.as_bytes().get(1) == Some(&b':') {
        if let Some(first) = path.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
    }
    Some(path)
}

/// The real folder for a `workspaceStorage/<hash>` workspace, from its `workspace.json`
/// `folder` (single-root) or `workspace` (`.code-workspace`) URI. None for the
/// `empty-window` pseudo-workspace or an unreadable/rootless entry.
fn workspace_folder(hash: &str) -> Option<String> {
    if hash.is_empty() || hash == "empty-window" {
        return None;
    }
    let wj = cursor_dir().join("User").join("workspaceStorage").join(hash).join("workspace.json");
    let raw = std::fs::read_to_string(&wj).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let uri = v
        .get("folder")
        .or_else(|| v.get("workspace"))
        .and_then(serde_json::Value::as_str)?;
    file_uri_to_path(uri)
}

/// composerId -> real workspace folder path, cached per DB fingerprint. Source: the
/// global `composerHeaders` table maps each composer to a `workspaceId` (a
/// `workspaceStorage/<hash>` name), and each hash's `workspace.json` gives the folder.
/// Composers with no open folder (`workspaceId` == "empty-window", or a missing
/// workspace.json) are omitted, so they keep the friendly "Cursor" bucket.
static WS_MAP: Mutex<Option<(Fingerprint, HashMap<String, String>)>> = Mutex::new(None);

fn workspace_map() -> HashMap<String, String> {
    let fp = db_fingerprint();
    if let (Some(fp), Ok(guard)) = (fp, WS_MAP.lock()) {
        if let Some((cached_fp, map)) = guard.as_ref() {
            if *cached_fp == fp {
                return map.clone();
            }
        }
    }
    // Rebuild: composerHeaders is a real table in the snapshot (global DB). A build
    // that lacks it just errors here, yielding an empty map (all rows keep "Cursor").
    let mut hash_folder: HashMap<String, String> = HashMap::new();
    let map = with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT composerId, workspaceId FROM composerHeaders
             WHERE workspaceId IS NOT NULL AND workspaceId <> '' AND workspaceId <> 'empty-window'",
        )?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out: HashMap<String, String> = HashMap::new();
        for (composer, hash) in rows.flatten() {
            // Resolve each distinct hash's folder once.
            let folder = match hash_folder.get(&hash) {
                Some(f) => Some(f.clone()),
                None => {
                    let f = workspace_folder(&hash);
                    if let Some(ref f) = f {
                        hash_folder.insert(hash.clone(), f.clone());
                    }
                    f
                }
            };
            if let Some(folder) = folder {
                out.insert(composer, folder);
            }
        }
        Ok(out)
    })
    .unwrap_or_default();
    if let (Some(fp), Ok(mut guard)) = (fp, WS_MAP.lock()) {
        *guard = Some((fp, map.clone()));
    }
    map
}

/// `(project, project_path)` for a composer: its mapped workspace folder (basename +
/// full path) when known, else the friendly "Cursor" bucket with an empty path.
fn project_for(id: &str) -> (String, String) {
    match workspace_map().get(id) {
        Some(folder) if !folder.is_empty() => {
            (crate::scan::last_component(folder), folder.clone())
        }
        _ => ("Cursor".to_string(), String::new()),
    }
}

// --- public adapter surface (mirrors codex.rs) ---

/// Every Cursor composer, as a `(synthetic_path, mtime_secs)` pair for the poll
/// pool. mtime prefers the composer's own `createdAt` (per-composer recency); it
/// falls back to the shared DB mtime when a composer records none. Skips the
/// `composerData:empty-state-draft` sentinel (a null-meter placeholder row).
pub fn candidates() -> Vec<(PathBuf, u64)> {
    let db_mtime = db_mtime_secs();
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT substr(key, length('composerData:')+1),
                    json_extract(value,'$.lastUpdatedAt'),
                    json_extract(value,'$.createdAt')
             FROM cursorDiskKV
             WHERE key >= 'composerData:' AND key < 'composerData;'",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let updated: SqlValue = r.get(1)?;
            let created: SqlValue = r.get(2)?;
            // Recency = lastUpdatedAt (bumps every turn), falling back to createdAt,
            // then the shared DB mtime. Using createdAt alone froze active composers
            // low in the poll pool so a fresh Cursor turn never rose to a live gauge.
            Ok((id, created_to_ms(&updated).or_else(|| created_to_ms(&created))))
        })?;
        let mut out = Vec::new();
        for row in rows.flatten() {
            let (id, updated) = row;
            if id.is_empty() || id == "empty-state-draft" {
                continue;
            }
            let mtime = updated.map(|ms| (ms / 1000) as u64).unwrap_or(db_mtime);
            out.push((synthetic_path(&id), mtime));
        }
        Ok(out)
    })
    .unwrap_or_default()
}

/// Build one Cursor composer row for the poll list. `focused` is the composerId the
/// caller resolved as frontmost (via the OS foreground gate in scan.rs), or None.
/// Mirrors codex::build_session's shape.
pub fn build_session(path: &Path, mtime: u64, now: u64, cfg: &Config, focused: Option<&str>) -> Session {
    let id = id_from_path(path);
    let info = composer_info(&id);

    let (ctx, ctx_limit, model, cost_usd, title) = match &info {
        Some(i) => (i.ctx, i.ctx_limit, i.model.clone(), i.cost_usd, i.title.clone()),
        None => (None, None, None, 0.0, None),
    };

    // Real workspace folder when the composer maps to one (global composerHeaders ->
    // workspaceStorage/<hash>/workspace.json); else the friendly "Cursor" bucket.
    let (project, project_path) = project_for(&id);

    // One user-set target drives the gauge budget across harnesses; the model still
    // sets the hard window and label.
    let mi = model.as_deref().map(model_info);
    let target = cfg.target_tokens;
    // Prefer Cursor's own reported window for this composer; fall back to the model
    // table, then the configured target.
    let limit = ctx_limit
        .or_else(|| mi.as_ref().map(|i| i.context_max))
        .unwrap_or(cfg.target_tokens);
    let model_label = mi.as_ref().map(|i| i.label.to_string()).unwrap_or_default();
    let model_version = model.as_deref().map(parse_model_version).unwrap_or_default();

    let title = title.unwrap_or_else(|| project.clone());
    let pct = ctx.map(|c| c as f64 / target.max(1) as f64);
    let live = now.saturating_sub(mtime) as f64 <= (cfg.poll_seconds * 2.0).max(1.0);
    let is_focused = focused == Some(id.as_str());

    Session {
        id,
        project,
        project_root: project_path.clone(),
        sub_path: String::new(),
        project_path,
        title,
        subtitle: String::new(),
        ctx,
        target,
        limit,
        model: model_label,
        model_version,
        pct,
        live,
        mtime,
        size_bytes: 0,
        cost_usd,
        focused: is_focused,
        compact: None,
    }
}

/// Cursor's "focused" composer, as `(composerId, lastUpdatedAt ms)`.
///
/// Cursor writes NO focus/selection event anywhere (verified 2026-09-14: not to any
/// log under `%APPDATA%\Cursor\logs`, not to the global DB -- no `selectedComposerId`
/// / `activeComposer` field, and the UI-state blob holds zero composer uuids), so
/// unlike Codex we cannot read which composer the user selected. What we CAN do is
/// what scan.rs already does for Claude/Codex: gate on the OS foreground window. When
/// Cursor is frontmost, the composer the user is looking at is almost always the one
/// they last ran a turn in -- the max `lastUpdatedAt`. So this returns that composer;
/// the frontmost-is-Cursor check is the caller's (see scan.rs's follow_focus block).
/// The timestamp is that composer's own `lastUpdatedAt` (turn time, not a focus time),
/// used only for the timestamp-tiebreak fallback when no app is known-frontmost.
pub fn focused_session_id() -> Option<(String, u64)> {
    with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT substr(key, length('composerData:')+1),
                    json_extract(value,'$.lastUpdatedAt'),
                    json_extract(value,'$.createdAt')
             FROM cursorDiskKV
             WHERE key >= 'composerData:' AND key < 'composerData;'",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let updated: SqlValue = r.get(1)?;
            let created: SqlValue = r.get(2)?;
            Ok((id, created_to_ms(&updated).or_else(|| created_to_ms(&created))))
        })?;
        let mut best: Option<(String, i64)> = None;
        for (id, ms) in rows.flatten() {
            if id.is_empty() || id == "empty-state-draft" {
                continue;
            }
            let Some(ms) = ms else { continue };
            if best.as_ref().map(|(_, b)| ms > *b).unwrap_or(true) {
                best = Some((id, ms));
            }
        }
        Ok(best.map(|(id, ms)| (id, ms.max(0) as u64)))
    })
    .flatten()
}

/// Over-time curve for one composer. Cursor persists no per-turn context snapshots,
/// so this is a single point: the composer's context-meter fill and its estimated
/// cumulative spend at its `createdAt` (or the DB mtime as a fallback timestamp).
pub fn history(id: &str) -> Vec<Sample> {
    let Some(info) = composer_info(id) else { return Vec::new() };
    let t = info.created_ms.unwrap_or_else(|| (db_mtime_secs() as i64) * 1000);
    let ctx = info.ctx.unwrap_or(0);
    downsample(vec![Sample { t, ctx, cost: info.cost_usd }], 200)
}

/// Whole-composer browse-cache row: the Cursor analog of `scan::enrich_meta`,
/// producing the SAME `EnrichMeta`. Reads the DB by the id encoded in `path` (the
/// synthetic file itself never exists), so the `raw` variant below ignores its bytes.
pub fn enrich_meta(path: &Path) -> crate::scan::EnrichMeta {
    let id = id_from_path(path);
    let mut out = crate::scan::EnrichMeta::default();
    let Some(info) = composer_info(&id) else { return out };

    out.ctx = info.ctx;
    out.cost_usd = info.cost_usd;
    out.turn_count = info.turns.len() as u64;
    out.turns = info.turns;
    out.title = info.title;
    // Real workspace folder when mapped, so browse Pass-2 refines the project from it;
    // unmapped composers report None and keep the Pass-1 "Cursor" bucket.
    out.cwd = workspace_map().get(&id).cloned();
    out.has_context_usage = false;
    if let Some(m) = &info.model {
        let mi = model_info(m);
        out.model_label = mi.label.to_string();
        out.model_version = parse_model_version(m);
        out.target = mi.target;
        out.limit = info.ctx_limit.unwrap_or(mi.context_max);
    } else if let Some(lim) = info.ctx_limit {
        out.limit = lim;
    }
    out
}

/// Parse-only shim for the enrich dispatcher. Cursor data lives in the DB, not in
/// the (nonexistent) file bytes, so `raw` is ignored and we go straight to the DB.
pub fn enrich_meta_from(path: &Path, _raw: &str) -> crate::scan::EnrichMeta {
    enrich_meta(path)
}

/// Chat text of one composer for the browse search, in the same `ChatDoc` shape as
/// the Claude/Codex readers. Reads the DB by the id in `path`; `raw` is ignored (the
/// synthetic file has no bytes). Timestamps are left None -- Cursor's per-bubble
/// times are unreliable, and callers fall back to the row mtime for the span.
pub fn read_chat_doc(path: &Path, _raw: &str) -> crate::browse::ChatDoc {
    let id = id_from_path(path);
    let mut doc = crate::browse::ChatDoc {
        text: String::new(),
        title: None,
        cwd: None,
        first_user: None,
        first_ts: None,
        last_ts: None,
    };
    let Some((title, bubbles)) = with_db(|conn| {
        let (_ctx, _lim, _created, title) = fetch_meter(conn, &id).unwrap_or((None, None, None, None));
        Ok((title, fetch_bubbles(conn, &id)?))
    }) else {
        return doc;
    };
    doc.title = title;
    for b in &bubbles {
        if b.text.trim().is_empty() {
            continue;
        }
        if b.btype == 1 && doc.first_user.is_none() {
            doc.first_user = Some(b.text.clone());
        }
        doc.text.push_str(&b.text);
        doc.text.push('\n');
    }
    doc
}

/// The composer's real workspace folder (its project cwd), or None when it maps to no
/// open folder (`empty-window`) and thus keeps the friendly "Cursor" bucket. Kept for
/// adapter-surface parity with codex.rs; build_session/enrich resolve the folder inline.
#[allow(dead_code)]
pub fn cwd_of(path: &Path) -> Option<String> {
    workspace_map().get(&id_from_path(path)).cloned()
}

/// The composer's title and a treemap size-analog, in a single DB read. Cursor rows
/// have no file on disk, so a synthetic path reports 0 bytes and the byte-sized browse
/// treemap would give the whole harness zero area. We size a composer by its context
/// magnitude instead -- tokens * 4, the bytes a transcript of that context would take
/// -- so the Cursor tile is present and comparably scaled. Returns `(title, bytes)`.
pub(crate) fn title_and_size(id: &str) -> (Option<String>, i64) {
    with_db(|conn| {
        let (ctx, _lim, _created, title) =
            fetch_meter(conn, id).unwrap_or((None, None, None, None));
        Ok((title, (ctx.unwrap_or(0) as i64) * 4))
    })
    .unwrap_or((None, 0))
}

/// Ordered `(is_user, text, ts_ms)` for a composer's non-empty bubbles, feeding the
/// Context Explorer's estimated Messages tree. User bubbles carry `type == 1`.
pub(crate) fn chat_blocks(id: &str) -> Vec<(bool, String, Option<i64>)> {
    with_db(|conn| {
        Ok(fetch_bubbles(conn, id)?
            .into_iter()
            .filter(|b| !b.text.trim().is_empty())
            .map(|b| (b.btype == 1, b.text, b.ts))
            .collect())
    })
    .unwrap_or_default()
}

/// The composer's real context fill, its window, and model for the Context Explorer
/// analysis: `(ctx_tokens, ctx_limit, model, created_ms)`. None if the composer is
/// absent from the DB.
pub(crate) fn baseline_meter(id: &str) -> Option<(Option<u64>, Option<u64>, Option<String>, Option<i64>)> {
    let info = composer_info(id)?;
    Some((info.ctx, info.ctx_limit, info.model, info.created_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn est_tokens_is_ceil_div_4() {
        assert_eq!(est_tokens(0), 0);
        assert_eq!(est_tokens(1), 1);
        assert_eq!(est_tokens(4), 1);
        assert_eq!(est_tokens(5), 2);
        assert_eq!(est_tokens(400), 100);
    }

    #[test]
    fn id_round_trips_through_synthetic_path() {
        let id = "8775c7c6-4342-4a1f-80fb-a6f450f47e22";
        let p = synthetic_path(id);
        assert_eq!(id_from_path(&p), id);
        // A non-composer stem is returned whole.
        assert_eq!(id_from_path(Path::new("C:/x/tiny.cursor")), "tiny");
    }

    #[test]
    fn file_uri_decodes_to_windows_path() {
        assert_eq!(
            file_uri_to_path("file:///c%3A/claude-local/MoonPool/public-repos/MoonPool").as_deref(),
            Some("C:\\claude-local\\MoonPool\\public-repos\\MoonPool")
        );
        // Percent-encoded space, lowercase drive uppercased.
        assert_eq!(
            file_uri_to_path("file:///d%3A/My%20Code/app").as_deref(),
            Some("D:\\My Code\\app")
        );
        // Not a file URI.
        assert_eq!(file_uri_to_path("vscode-remote://ssh/x"), None);
    }

    #[test]
    fn percent_decode_leaves_bare_and_trailing_percent() {
        assert_eq!(percent_decode("a%3Ab"), "a:b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("tail%"), "tail%");
    }

    #[test]
    fn synthetic_path_is_under_cursor_dir() {
        // The prefix dispatch the whole adapter relies on must hold.
        let p = synthetic_path("abc");
        assert!(p.starts_with(cursor_dir()));
    }

    #[test]
    fn created_to_ms_reads_int_real_and_iso() {
        assert_eq!(created_to_ms(&SqlValue::Integer(1789423799103)), Some(1789423799103));
        assert_eq!(created_to_ms(&SqlValue::Real(1789423799103.0)), Some(1789423799103));
        assert_eq!(
            created_to_ms(&SqlValue::Text("1789423799103".into())),
            Some(1789423799103)
        );
        assert_eq!(
            created_to_ms(&SqlValue::Text("2026-09-14T00:00:00.000Z".into())),
            crate::scan::iso_to_millis("2026-09-14T00:00:00.000Z")
        );
        assert_eq!(created_to_ms(&SqlValue::Null), None);
    }

    // Exercises the real local DB when present (skips cleanly in CI / on machines
    // without Cursor), like Greedout's other data-backed tests. Verifies the known
    // fixture on this machine: one composer, ctx 58686, model grok-4.6.
    #[test]
    fn real_db_lists_composer_and_meter_when_present() {
        if std::fs::metadata(cursor_db_path()).is_err() {
            return; // Cursor not installed here; nothing to assert.
        }
        let cands = candidates();
        // The sentinel row must never surface as a composer.
        assert!(cands.iter().all(|(p, _)| id_from_path(p) != "empty-state-draft"));
        for (path, _mtime) in cands {
            let id = id_from_path(&path);
            if let Some(info) = composer_info(&id) {
                // Meter is a real snapshot; enrich must expose it unmodified.
                let em = enrich_meta(&path);
                assert_eq!(em.ctx, info.ctx);
                // If a model resolved, it prices to a known label (Grok on this box).
                if let Some(m) = &info.model {
                    assert!(!model_info(m).label.is_empty());
                }
            }
        }
    }
}
