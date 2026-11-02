//! The Context Explorer's data: browse every conversation by harness, project and
//! session, and search their chat text. Everything is read from the cache
//! (`cache.rs`); this module owns no database of its own.
//!
//! Search text lives in the cache's `chat_fts` table as chunks keyed to byte ranges
//! of each transcript. A background thread, kicked after every cache sync, reads the
//! bytes a transcript gained since its last chunk and indexes just those, so the
//! index trails the transcripts by seconds and opening the Explorer never waits.
//! Sessions the index has not caught up with yet are searched straight from disk.

use crate::codex;
use crate::config::{codex_dir, cursor_dir};
use crate::cursor;
use crate::grouping;
use crate::cache;
use crate::scan;
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, UNIX_EPOCH};
use tauri::Emitter;

const HARNESS: &str = "claude-code";

/// Trace to greedout.log, prefixed so browse lines are easy to grep out of the
/// poll-loop noise. A macro so that with logging off the message is never formatted.
macro_rules! blog {
    ($($arg:tt)*) => {
        if $crate::config::debug_enabled() {
            $crate::config::debug_log_raw(&format!("browse: {}", format_args!($($arg)*)));
        }
    };
}

/// Set while a cache sync is being followed, so a second `browse_scan` is a no-op.
fn running() -> &'static AtomicBool {
    static R: OnceLock<AtomicBool> = OnceLock::new();
    R.get_or_init(|| AtomicBool::new(false))
}

/// Set by `browse_cancel`: the window stops following the sync (which carries on).
fn cancel_flag() -> &'static AtomicBool {
    static C: OnceLock<AtomicBool> = OnceLock::new();
    C.get_or_init(|| AtomicBool::new(false))
}

// --- Frontend-facing payloads (camelCase to match the rest of the app). ---

/// One harness (provider) aggregate.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessAgg {
    pub harness: String,
    pub label: String,
    pub project_count: u64,
    pub session_count: u64,
    pub total_bytes: u64,
    pub total_tokens: u64,
    pub total_cost: f64,
}

/// One project (directory group) aggregate within a harness.
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
}

/// How much the Explorer has to show, and whether a sync is being followed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowseStatus {
    pub indexed: u64,
    pub scanning: bool,
}

/// Progress event payload, emitted on "browse-progress" during a sync.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanProgress {
    phase: &'static str, // "sync" | "done" | "canceled"
    done: u64,
    total: u64,
}

// --- Cache queries ---

/// A session counts as this big in the size-sized treemap: its transcript, or for a
/// Cursor chat (which has no file) four bytes per context token.
const SIZE: &str = "COALESCE(s.len, s.ctx * 4, 0)";

/// Sessions of the main conversations only: subagent runs show under their parent.
const TOP: &str = "s.agent_id IS NULL AND s.parent_turn IS NULL";

pub fn status() -> BrowseStatus {
    let indexed = cache::with_db(|c| {
        c.query_row(&format!("SELECT COUNT(*) FROM sessions s WHERE {TOP}"), [], |r| r.get::<_, u64>(0))
    })
    .unwrap_or(0);
    BrowseStatus { indexed, scanning: running().load(Ordering::Relaxed) }
}

/// How much of the transcript text the chat index has covered.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexProgress {
    pub done: u64,
    pub total: u64,
}

/// Sessions whose text is fully indexed, out of all file-backed main sessions.
pub fn index_progress() -> IndexProgress {
    let (done, total) = cache::with_db(|c| {
        c.query_row(
            "SELECT COALESCE(SUM(fts_off >= COALESCE(offset, 0)), 0), COUNT(*) FROM sessions
             WHERE path IS NOT NULL AND agent_id IS NULL",
            [],
            |r| Ok((r.get::<_, u64>(0)?, r.get::<_, u64>(1)?)),
        )
    })
    .unwrap_or((0, 0));
    IndexProgress { done, total }
}

/// Follow a cache sync on a background thread, emitting "browse-progress" with how
/// many conversation files it has stored, then "done". A no-op while one is followed.
pub fn run_scan(app: tauri::AppHandle) {
    if running().swap(true, Ordering::SeqCst) {
        return;
    }
    cancel_flag().store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let t0 = std::time::Instant::now();
        let sync = std::thread::spawn(cache::refresh);
        while !sync.is_finished() && !cancel_flag().load(Ordering::Relaxed) {
            let (done, total) = cache::sync_progress();
            let _ = app.emit("browse-progress", ScanProgress { phase: "sync", done, total });
            std::thread::sleep(Duration::from_millis(100));
        }
        let canceled = cancel_flag().load(Ordering::Relaxed);
        running().store(false, Ordering::SeqCst);
        blog!("sync followed for {}ms{}", t0.elapsed().as_millis(), if canceled { " (canceled)" } else { "" });
        let _ = app.emit(
            "browse-progress",
            ScanProgress { phase: if canceled { "canceled" } else { "done" }, done: 0, total: 0 },
        );
    });
}

pub fn cancel() {
    cancel_flag().store(true, Ordering::SeqCst);
}

pub fn harnesses() -> Vec<HarnessAgg> {
    cache::with_db(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT h.name, COUNT(DISTINCT d.project), COUNT(*), COALESCE(SUM({SIZE}), 0),
                    COALESCE(SUM(s.ctx), 0), COALESCE(SUM(s.cost), 0)
             FROM sessions s JOIN directories d ON d.id = s.directory JOIN harnesses h ON h.id = s.harness
             WHERE {TOP} GROUP BY h.id ORDER BY 4 DESC"
        ))?;
        let rows = stmt.query_map([], |r| {
            let harness: String = r.get(0)?;
            Ok(HarnessAgg {
                label: harness_label(&harness),
                harness,
                project_count: r.get(1)?,
                session_count: r.get(2)?,
                total_bytes: r.get(3)?,
                total_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                total_cost: r.get(5)?,
            })
        })?;
        rows.collect()
    })
    .unwrap_or_default()
}

pub fn projects(harness: &str) -> Vec<ProjectAgg> {
    cache::with_db(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT p.label, p.root, COUNT(*), COALESCE(SUM({SIZE}), 0),
                    COALESCE(SUM(s.ctx), 0), COALESCE(SUM(s.cost), 0)
             FROM sessions s JOIN directories d ON d.id = s.directory
                  JOIN projects p ON p.id = d.project JOIN harnesses h ON h.id = s.harness
             WHERE h.name = ?1 AND {TOP} GROUP BY p.id ORDER BY 4 DESC"
        ))?;
        let rows = stmt.query_map([harness], |r| {
            Ok(ProjectAgg {
                harness: harness.to_string(),
                project: r.get(0)?,
                project_path: r.get(1)?,
                session_count: r.get(2)?,
                total_bytes: r.get(3)?,
                total_tokens: r.get::<_, i64>(4)?.max(0) as u64,
                total_cost: r.get(5)?,
            })
        })?;
        rows.collect()
    })
    .unwrap_or_default()
}

/// The columns of a session row, joined to its directory and project.
const SESSION_SELECT: &str = "SELECT s.sid, h.name, p.label, d.path, s.title, COALESCE(s.mtime, 0), s.len, s.ctx,
        s.cost, s.model, s.n_turns, s.ctx_cmd
     FROM sessions s JOIN directories d ON d.id = s.directory
          JOIN projects p ON p.id = d.project JOIN harnesses h ON h.id = s.harness";

fn session_row(r: &rusqlite::Row) -> rusqlite::Result<SessionMeta> {
    let project: String = r.get(2)?;
    let title: Option<String> = r.get(4)?;
    let len: Option<i64> = r.get(6)?;
    let ctx: Option<i64> = r.get(7)?;
    let model: Option<String> = r.get(9)?;
    Ok(SessionMeta {
        id: r.get(0)?,
        harness: r.get(1)?,
        title: title.filter(|t| !t.is_empty()).unwrap_or_else(|| project.clone()),
        project,
        project_path: r.get(3)?,
        mtime: r.get(5)?,
        size_bytes: len.or(ctx.map(|c| c * 4)).unwrap_or(0).max(0) as u64,
        ctx: ctx.map(|c| c as u64),
        cost_usd: r.get(8)?,
        model: model.map(|m| scan::model_info(&m).label.to_string()).unwrap_or_default(),
        turn_count: r.get::<_, i64>(10)?.max(0) as u64,
        has_context_usage: r.get::<_, i64>(11)? != 0,
    })
}

/// Sessions in a project, newest first. A transcript that was deleted from disk stays
/// in the cache as history but cannot be opened, so it is left out here.
pub fn sessions(harness: &str, project: &str) -> Vec<SessionMeta> {
    let rows = cache::with_db(|conn| {
        let mut stmt = conn.prepare(&format!(
            "{SESSION_SELECT} WHERE h.name = ?1 AND p.label = ?2 AND {TOP} ORDER BY s.mtime DESC"
        ))?;
        let rows = stmt.query_map([harness, project], session_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .unwrap_or_default();
    blog!("sessions read {harness}/{project} -> {} rows", rows.len());
    rows
}

/// One session by its id.
pub fn session(id: &str) -> Option<SessionMeta> {
    cache::with_db(|conn| {
        conn.prepare(&format!("{SESSION_SELECT} WHERE s.sid = ?1 AND s.agent_id IS NULL LIMIT 1"))?
            .query_map([id], session_row)?
            .next()
            .transpose()
    })
    .flatten()
}

fn harness_label(h: &str) -> String {
    match h {
        "claude-code" => "Claude Code".to_string(),
        "codex" => "Codex".to_string(),
        "cursor" => "Cursor".to_string(),
        other => other.to_string(),
    }
}

fn file_stat(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    Some((mtime, meta.len()))
}

// --- Chat index (fills `chat_fts` in the cache) ---

/// Longest stretch of a transcript read into one chunk.
const CHUNK: u64 = 4 << 20;

static INDEXING: AtomicBool = AtomicBool::new(false);

/// Start the indexer if it is not running. Called after every cache sync.
pub fn kick_index() {
    if INDEXING.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        while index_round() > 0 {}
        INDEXING.store(false, Ordering::SeqCst);
    });
}

/// A transcript the index has not caught up with.
struct Pending {
    id: i64,
    harness: i64,
    path: String,
    offset: u64,
    indexed: u64,
}

/// Index the newest sessions with unindexed bytes. Returns how many it worked on.
fn index_round() -> usize {
    let todo: Vec<Pending> = cache::with_db(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, harness, path, offset, fts_off FROM sessions
             WHERE path IS NOT NULL AND agent_id IS NULL AND fts_off < COALESCE(offset, 0)
             ORDER BY mtime DESC LIMIT 32",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Pending { id: r.get(0)?, harness: r.get(1)?, path: r.get(2)?, offset: r.get(3)?, indexed: r.get(4)? })
        })?;
        rows.collect()
    })
    .unwrap_or_default();
    let t0 = std::time::Instant::now();
    for p in &todo {
        index_session(p);
    }
    if !todo.is_empty() {
        blog!("indexed chat text of {} sessions in {}ms", todo.len(), t0.elapsed().as_millis());
    }
    todo.len()
}

/// Mark a session fully indexed without text, so a file that cannot be read is not
/// retried forever.
fn give_up(id: i64, offset: u64) {
    cache::with_db(|c| c.execute("UPDATE sessions SET fts_off = ?2 WHERE id = ?1", params![id, offset as i64]));
}

/// Fill `buf` with `want` bytes of `file` starting at `pos`.
fn read_at(file: &mut std::fs::File, pos: u64, want: u64, buf: &mut Vec<u8>) -> bool {
    buf.resize(want as usize, 0);
    file.seek(SeekFrom::Start(pos)).is_ok() && file.read_exact(buf).is_ok()
}

/// Read a session's transcript from where the index stopped, chunk by chunk, and add
/// each chunk's chat text. File reads and parsing happen outside the cache lock.
fn index_session(p: &Pending) {
    let Ok(mut file) = std::fs::File::open(&p.path) else { return give_up(p.id, p.offset) };
    let path = Path::new(&p.path);
    let mut pos = p.indexed;
    let mut buf = Vec::new();
    while pos < p.offset {
        // A line longer than a chunk is read whole, up to what the cache holds.
        let mut want = (p.offset - pos).min(CHUNK);
        if !read_at(&mut file, pos, want, &mut buf) {
            return give_up(p.id, p.offset);
        }
        if pos + want < p.offset && memchr::memrchr(b'\n', &buf).is_none() {
            want = p.offset - pos;
            if !read_at(&mut file, pos, want, &mut buf) {
                return give_up(p.id, p.offset);
            }
        }
        let end = if pos + want >= p.offset { buf.len() } else { memchr::memrchr(b'\n', &buf).map_or(buf.len(), |i| i + 1) };
        let raw = String::from_utf8_lossy(&buf[..end]);
        let doc = if p.harness == 2 { codex::read_chat_doc(path, &raw) } else { read_chat_doc(path, &raw) };
        let intro = doc.first_user.as_deref().map(short_title);
        let next = pos + end as u64;
        let stored = cache::with_db(|conn| {
            let tx = conn.unchecked_transaction()?;
            // The session may have been rewritten since it was picked: then its chunks
            // were dropped and this one no longer belongs.
            let now: Option<i64> = tx
                .query_row("SELECT fts_off FROM sessions WHERE id = ?1", [p.id], |r| r.get(0))
                .ok();
            if now != Some(pos as i64) {
                return Ok(false);
            }
            if !doc.text.is_empty() {
                tx.execute("INSERT INTO chat_chunks (session) VALUES (?1)", [p.id])?;
                let chunk = tx.last_insert_rowid();
                tx.execute("INSERT INTO chat_fts (rowid, text) VALUES (?1, ?2)", params![chunk, doc.text])?;
            }
            tx.execute(
                "UPDATE sessions SET fts_off = ?2, intro = COALESCE(intro, ?3) WHERE id = ?1",
                params![p.id, next as i64, intro],
            )?;
            tx.commit()?;
            Ok(true)
        });
        if stored != Some(true) {
            return;
        }
        pos = next;
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
    /// "claude-code" | "codex" | "cursor" -- which harness this session belongs to, so
    /// the results list can badge cross-harness hits.
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

        // Fast path: every transcript the chat index has caught up with is served from
        // `chat_fts`. The rest are read from disk below. If the query has no >=3-char
        // term the trigram index can't help, so every file is read from disk.
        let hits = AtomicU64::new(0);
        let mut indexed: HashSet<String> = HashSet::new();
        if fts_match_expr(&terms).is_some() {
            indexed = cache::with_db(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT path FROM sessions
                     WHERE path IS NOT NULL AND agent_id IS NULL AND fts_off >= COALESCE(offset, 0)",
                )?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                rows.collect()
            })
            .unwrap_or_default();
            if !indexed.is_empty() {
                let n = run_fts_hits(&app, &full, &terms, my_gen);
                hits.fetch_add(n, Ordering::Relaxed);
            }
        }

        // Slow path: read every transcript the index does not cover. Cursor chats have
        // no file; they are always read from their database.
        let mut all_paths: Vec<PathBuf> = cache::with_db(|conn| {
            let mut stmt =
                conn.prepare("SELECT path FROM sessions WHERE path IS NOT NULL AND agent_id IS NULL")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.map(|p| p.map(PathBuf::from)).collect()
        })
        .unwrap_or_default();
        all_paths.extend(cursor::candidates().into_iter().map(|(p, _)| p));
        let paths: Vec<PathBuf> =
            all_paths.into_iter().filter(|p| !indexed.contains(p.to_string_lossy().as_ref())).collect();
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

/// Stream hits for the fully indexed sessions straight from `chat_fts`. A session is
/// a candidate when each indexable term matches in some chunk of it; its chunks are
/// then joined and run through the same scorer and snippet as the disk path, so the
/// two never rank differently. The cache is locked only while reading, never while
/// scoring, so the limit dials keep polling. Returns the number emitted.
fn run_fts_hits(app: &tauri::AppHandle, full: &str, terms: &[String], my_gen: u64) -> u64 {
    let mut candidates: Option<HashSet<i64>> = None;
    for term in terms.iter().filter(|t| t.chars().count() >= 3) {
        let Some(expr) = fts_match_expr(std::slice::from_ref(term)) else { continue };
        let found: Option<HashSet<i64>> = cache::with_db(|conn| {
            conn.prepare(
                "SELECT DISTINCT c.session FROM chat_fts f
                 JOIN chat_chunks c ON c.id = f.rowid JOIN sessions s ON s.id = c.session
                 WHERE f.text MATCH ?1 AND s.agent_id IS NULL AND s.fts_off >= COALESCE(s.offset, 0)",
            )?
            .query_map([&expr], |r| r.get::<_, i64>(0))?
            .collect()
        });
        let Some(found) = found else { return 0 };
        candidates = Some(match candidates {
            Some(c) => c.intersection(&found).copied().collect(),
            None => found,
        });
    }
    let mut sessions: Vec<i64> = candidates.unwrap_or_default().into_iter().collect();
    sessions.sort_unstable();
    let mut n = 0u64;
    for id in sessions {
        if search_cancel().load(Ordering::Relaxed) || search_gen().load(Ordering::SeqCst) != my_gen {
            break;
        }
        let row = cache::with_db(|conn| {
            let body = conn
                .prepare("SELECT f.text FROM chat_chunks c JOIN chat_fts f ON f.rowid = c.id WHERE c.session = ?1 ORDER BY c.id")?
                .query_map([id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .join("
");
            let meta = conn.query_row(
                "SELECT s.sid, p.label, d.path, h.name, s.title, s.intro, COALESCE(s.mtime, 0), COALESCE(s.len, 0),
                        s.first_ms, s.last_ms
                 FROM sessions s JOIN directories d ON d.id = s.directory
                      JOIN projects p ON p.id = d.project JOIN harnesses h ON h.id = s.harness
                 WHERE s.id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                        r.get::<_, Option<i64>>(9)?,
                    ))
                },
            )?;
            Ok((body, meta))
        });
        let Some((body, (sid, project, project_path, harness, title, intro, mtime, len, first_ms, last_ms))) = row else {
            continue;
        };
        let lower = body.to_lowercase();
        let Some(score) = score_text(&lower, full, terms) else { continue };
        let hit = SearchHit {
            id: sid,
            title: title
                .filter(|t| !t.is_empty())
                .or(intro.filter(|t| !t.is_empty()))
                .unwrap_or_else(|| project.clone()),
            project,
            project_path,
            size_bytes: len.max(0) as u64,
            mtime,
            first_ms: first_ms.unwrap_or(mtime),
            last_ms: last_ms.unwrap_or(mtime),
            score,
            snippet: make_snippet(&body, &lower, full, terms),
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
    // Cursor: no file on disk. Read the composer's chat from the DB by the id in the
    // synthetic path and score its text directly (no raw-bytes pre-filter).
    if path.starts_with(&cursor_dir()) {
        let doc = cursor::read_chat_doc(path, "");
        let lower = doc.text.to_lowercase();
        let score = score_text(&lower, full, terms)?;
        let id = cursor::id_from_path(path);
        let (mtime, _size) = file_stat(path).unwrap_or((0, 0));
        let title = doc
            .title
            .clone()
            .or_else(|| doc.first_user.as_deref().map(short_title))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Cursor".to_string());
        let snippet = make_snippet(&doc.text, &lower, full, terms);
        // Real workspace folder when the composer maps to one; else the "Cursor" bucket.
        let (project, project_path) = match cursor::cwd_of(path) {
            Some(cwd) if !cwd.is_empty() => (scan::last_component(&cwd), cwd),
            _ => ("Cursor".to_string(), String::new()),
        };
        return Some(SearchHit {
            id,
            title,
            project,
            project_path,
            size_bytes: 0,
            mtime,
            first_ms: mtime,
            last_ms: mtime,
            score,
            snippet,
            harness: "cursor".into(),
        });
    }
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
}
