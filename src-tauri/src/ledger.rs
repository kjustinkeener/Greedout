//! The conversation ledger: every Claude Code, Codex and Cursor conversation, read
//! once and kept in `ledger.sqlite` as a hierarchy of tables, each level nested in
//! the one above and each storing its own totals.
//!
//!   harnesses    Claude Code, Codex, Cursor
//!   projects     the folded groups the user sees (see `grouping`), across harnesses
//!   directories  one working directory of one harness; sessions point at it
//!   sessions     one transcript (or one Cursor chat); a subagent run is a session
//!                hung under the turn that spawned it (`parent_turn`)
//!   exchanges    a user prompt plus everything until the next prompt
//!   turns        one assistant message; the smallest thing that carries a cost
//!
//! Totals are rolled up when data changes, never recomputed when read:
//! `cost` is what a row spent itself, `total` adds the subagent runs under it (so
//! the tree can show them). Directories, projects and harnesses sum `cost`, which
//! counts every dollar once whether or not a subagent could be linked to its turn.
//! `daily` keeps spend per directory per local day for the Daily Spend graph, and
//! `windows` keeps the running spend of the open 5-hour and weekly limit windows.
//!
//! Claude and Codex transcripts only ever grow, so a scan resumes at each file's
//! saved offset and adds only what is new; a turn keeps the byte range of its lines
//! so its contents can be read later by seeking. Cursor keeps every chat in one
//! shared database, so a changed chat is re-read whole (`stamp` marks the version
//! seen). Resumed and compacted Claude sessions copy old turns into a new file:
//! every copy keeps its own row, but exactly one copy per message id is `counted`
//! (the one in the session with the lowest path) and only counted turns are summed.
//! Rows stay after a transcript is deleted, so history outlives Claude Code's own
//! clean-up.

use crate::config;
use crate::grouping;
use crate::scan;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use memchr::memmem;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Instant, UNIX_EPOCH};

pub(crate) const H5_MS: i64 = 5 * 3600 * 1000;
pub(crate) const D7_MS: i64 = 7 * 24 * 3600 * 1000;

const CLAUDE: i64 = 1;
const CODEX: i64 = 2;
const CURSOR: i64 = 3;

/// Bump when the tables, the parsing or the price table in `scan::model_info`
/// change: a database from another version is dropped and rebuilt on next launch.
const VERSION: i64 = 3;

const TABLES: [&str; 10] = [
    "meta", "windows", "daily", "spawn_links", "turns", "exchanges", "sessions", "directories", "projects",
    "harnesses",
];

const SCHEMA: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE harnesses (
    id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE,
    cost REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0
);
INSERT INTO harnesses (id, name) VALUES (1, 'claude-code'), (2, 'codex'), (3, 'cursor');
CREATE TABLE projects (
    id INTEGER PRIMARY KEY, root TEXT NOT NULL UNIQUE, label TEXT NOT NULL,
    cost REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE directories (
    id INTEGER PRIMARY KEY, harness INTEGER NOT NULL, path TEXT NOT NULL, project INTEGER,
    cost REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0,
    UNIQUE (harness, path)
);
CREATE INDEX idx_directories_project ON directories(project);
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY, harness INTEGER NOT NULL, directory INTEGER NOT NULL,
    -- The turn that launched this subagent run; NULL for ordinary sessions.
    parent_turn INTEGER,
    -- Identity within the harness: a path under the transcript root, or a chat id.
    key TEXT NOT NULL,
    -- What the user knows it by: the transcript's file name, or the chat id.
    sid TEXT NOT NULL,
    agent_id TEXT, title TEXT,
    -- Newest model seen; priced forward by Codex.
    model TEXT,
    -- File sources: where it is and how far it has been read (always a line end).
    path TEXT, mtime INTEGER, len INTEGER, offset INTEGER,
    -- Shared-database sources: the version of the chat last read.
    stamp INTEGER,
    -- Number of /compact boundaries read so far.
    segment INTEGER NOT NULL DEFAULT 0,
    first_ms INTEGER, last_ms INTEGER,
    cost REAL NOT NULL DEFAULT 0, total REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0,
    UNIQUE (harness, key)
);
CREATE INDEX idx_sessions_directory ON sessions(directory);
CREATE INDEX idx_sessions_parent ON sessions(parent_turn);
CREATE INDEX idx_sessions_agent ON sessions(agent_id);
-- Which tool call launched which subagent run, read off the parent's tool results.
CREATE TABLE spawn_links (
    agent_id TEXT PRIMARY KEY, session INTEGER NOT NULL, tool_use_id TEXT NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE exchanges (
    id INTEGER PRIMARY KEY, session INTEGER NOT NULL,
    -- 1-based within its segment, restarting after each /compact; 0 for turns that
    -- have no prompt of their own (the stretch right after a /compact).
    no INTEGER NOT NULL, segment INTEGER NOT NULL,
    -- Byte offset of the prompt line, when the source is a file.
    off INTEGER, ts INTEGER NOT NULL,
    cost REAL NOT NULL DEFAULT 0, total REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_exchanges_session ON exchanges(session);
CREATE TABLE turns (
    -- Never reused, so a higher id always means a newer row, even after deletes.
    id INTEGER PRIMARY KEY AUTOINCREMENT, session INTEGER NOT NULL, exchange INTEGER NOT NULL,
    msg_id TEXT,
    ts INTEGER NOT NULL, cost REAL NOT NULL, total REAL NOT NULL, model TEXT,
    -- File sources: byte range of the message's lines, end exclusive.
    off_start INTEGER, off_end INTEGER,
    -- Shared-database sources: the chat message this turn is.
    bubble TEXT,
    -- Tool calls in this turn that launched subagents.
    spawns TEXT,
    counted INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX idx_turns_session_msg ON turns(session, msg_id);
CREATE INDEX idx_turns_msg ON turns(msg_id);
CREATE INDEX idx_turns_exchange ON turns(exchange);
CREATE INDEX idx_turns_ts ON turns(ts);
CREATE TABLE daily (
    directory INTEGER NOT NULL, day TEXT NOT NULL,
    cost REAL NOT NULL, n_turns INTEGER NOT NULL,
    PRIMARY KEY (directory, day)
) WITHOUT ROWID;
-- The open 5-hour ('five') and weekly ('seven') limit windows: spend since the
-- window opened, with how far into the turns it has been folded.
CREATE TABLE windows (
    kind TEXT PRIMARY KEY, mode TEXT NOT NULL,
    start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL, cost REAL NOT NULL,
    upto INTEGER NOT NULL DEFAULT 0, last_id INTEGER NOT NULL DEFAULT 0,
    stale INTEGER NOT NULL DEFAULT 0
);
";

static DB: Mutex<Option<Connection>> = Mutex::new(None);

fn db_path() -> PathBuf {
    config::app_dir().join("ledger.sqlite")
}

fn open() -> rusqlite::Result<Connection> {
    let _ = std::fs::create_dir_all(config::app_dir());
    let conn = Connection::open(db_path())?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    init(&conn)?;
    Ok(conn)
}

fn init(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version != VERSION {
        for t in TABLES {
            conn.execute_batch(&format!("DROP TABLE IF EXISTS {t};"))?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {VERSION};"))?;
    }
    // Scratch lists for one sync: messages whose copies need re-ranking, the sessions
    // and directories whose totals need re-summing, and the days to re-total.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS touched (msg_id TEXT PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS changed (id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS ex (id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS level (id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS next (id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS dirs (id INTEGER PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS dirty (dir INTEGER, day TEXT, PRIMARY KEY (dir, day));
         CREATE TEMP TABLE IF NOT EXISTS regroup (x INTEGER);",
    )
}

fn with_db<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Option<T> {
    let mut guard = DB.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        match open() {
            Ok(c) => *guard = Some(c),
            Err(e) => {
                config::debug_log(&config::load_config(), &format!("ledger: could not open: {e}"));
                return None;
            }
        }
    }
    match f(guard.as_ref()?) {
        Ok(v) => Some(v),
        Err(e) => {
            config::debug_log(&config::load_config(), &format!("ledger: {e}"));
            None
        }
    }
}

/// What one line of a conversation contributes.
pub(crate) enum Ev {
    Compact,
    Prompt { off: Option<u64>, ts: i64 },
    Turn {
        id: Option<String>,
        ts: i64,
        cost: f64,
        model: Option<String>,
        start: Option<u64>,
        end: Option<u64>,
        spawns: Option<String>,
        bubble: Option<String>,
    },
}

/// Newest rate-limit reset seen per window, epoch ms.
#[derive(Default)]
struct Resets {
    five: Option<i64>,
    seven: Option<i64>,
}

/// Everything one read of a conversation yields.
#[derive(Default)]
pub(crate) struct Parsed {
    pub evs: Vec<Ev>,
    resets: Resets,
    pub cwd: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    /// `(subagent id, tool call id)` pairs read off tool results.
    agents: Vec<(String, String)>,
}

/// Pull every `"rate_limit_info":{...}` object out of a line and keep the newest
/// reset per window type. Those records appear only when a limit is hit or
/// approached, but when present they pin the real window end.
fn rate_limit_resets(line: &str, out: &mut Resets) {
    let key = "\"rate_limit_info\":";
    let mut rest = line;
    while let Some(i) = rest.find(key) {
        rest = &rest[i + key.len()..];
        let Some(Ok(v)) = serde_json::Deserializer::from_str(rest).into_iter::<Value>().next() else {
            continue;
        };
        let (Some(ty), Some(at)) = (
            v.get("rateLimitType").and_then(Value::as_str),
            v.get("resetsAt").and_then(Value::as_i64),
        ) else {
            continue;
        };
        let slot = match ty {
            "five_hour" => &mut out.five,
            "seven_day" => &mut out.seven,
            _ => continue,
        };
        let ms = at * 1000;
        if slot.map_or(true, |p| ms > p) {
            *slot = Some(ms);
        }
    }
}

/// A subagent's own transcript opens with the prompt it was given, flagged as a
/// side chain, which `scan::is_user_prompt` rejects for the main transcript.
fn is_sidechain_prompt(line: &[u8]) -> bool {
    let n = &*NEEDLES;
    has(&n.user, line) && !has(&n.tool_result, line) && !has(&n.meta, line)
}

/// Same test as `scan::is_user_prompt`, on bytes.
fn is_user_prompt(line: &[u8]) -> bool {
    let n = &*NEEDLES;
    has(&n.user, line)
        && !has(&n.tool_result, line)
        && !has(&n.compact_summary, line)
        && !has(&n.meta, line)
        && !has(&n.sidechain, line)
        && !has(&n.command_name, line)
        && !has(&n.command_message, line)
        && !has(&n.local_stdout, line)
        && !has(&n.local_stderr, line)
}

/// The phrases that tell a transcript line's kind. Searching lines with `memchr` is
/// several times faster than `str::contains`, which dev builds compile unoptimized.
struct Needles {
    rate_limit: memmem::Finder<'static>,
    cwd: memmem::Finder<'static>,
    compact_boundary: memmem::Finder<'static>,
    usage: memmem::Finder<'static>,
    assistant: memmem::Finder<'static>,
    tool_use_result: memmem::Finder<'static>,
    agent_id: memmem::Finder<'static>,
    user: memmem::Finder<'static>,
    tool_result: memmem::Finder<'static>,
    compact_summary: memmem::Finder<'static>,
    meta: memmem::Finder<'static>,
    sidechain: memmem::Finder<'static>,
    command_name: memmem::Finder<'static>,
    command_message: memmem::Finder<'static>,
    local_stdout: memmem::Finder<'static>,
    local_stderr: memmem::Finder<'static>,
    msg_id: memmem::Finder<'static>,
    assistant_head: memmem::Finder<'static>,
    task_call: memmem::Finder<'static>,
    agent_call: memmem::Finder<'static>,
}

/// The fields of an assistant line that we keep, so the rest (message text, tool
/// input, which is most of the bytes) is skipped rather than built into a tree.
#[derive(serde::Deserialize)]
struct AssistantLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    message: Option<AssistantMessage>,
    usage: Option<TokenCounts>,
}

#[derive(serde::Deserialize)]
struct AssistantMessage {
    id: Option<String>,
    model: Option<String>,
    usage: Option<TokenCounts>,
}

/// A count that is missing or not a whole number reads as 0.
#[derive(serde::Deserialize, Default)]
struct TokenCounts {
    #[serde(default, deserialize_with = "count")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    output_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    cache_creation_input_tokens: u64,
    #[serde(default, deserialize_with = "count")]
    cache_read_input_tokens: u64,
}

fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    use serde::Deserialize;
    Ok(Value::deserialize(d)?.as_u64().unwrap_or(0))
}

static NEEDLES: LazyLock<Needles> = LazyLock::new(|| {
    let f = |s: &'static str| memmem::Finder::new(s.as_bytes());
    Needles {
        rate_limit: f("\"rate_limit_info\""),
        cwd: f("\"cwd\":\""),
        compact_boundary: f("\"subtype\":\"compact_boundary\""),
        usage: f("\"usage\""),
        assistant: f("\"assistant\""),
        tool_use_result: f("\"toolUseResult\""),
        agent_id: f("\"agentId\""),
        user: f("\"type\":\"user\""),
        tool_result: f("\"tool_result\""),
        compact_summary: f("\"isCompactSummary\""),
        meta: f("\"isMeta\":true"),
        sidechain: f("\"isSidechain\":true"),
        command_name: f("<command-name>"),
        command_message: f("<command-message>"),
        local_stdout: f("<local-command-stdout>"),
        local_stderr: f("<local-command-stderr>"),
        msg_id: f("\"id\":\"msg_"),
        assistant_head: f("\"message\":{\"model\""),
        task_call: f("\"name\":\"Task\""),
        agent_call: f("\"name\":\"Agent\""),
    }
});

fn has(needle: &memmem::Finder<'_>, hay: &[u8]) -> bool {
    needle.find(hay).is_some()
}

/// A message streamed over several lines repeats its usage on each, and only the
/// first is costed. When `raw` continues the turn just before it, widen that turn
/// (and note any subagent calls) without parsing the line.
fn extends_turn(raw: &[u8], end: u64, evs: &mut [Ev]) -> bool {
    let n = &*NEEDLES;
    let Some(i) = n.msg_id.find(raw) else { return false };
    let from = i + 6;
    let Some(len) = memchr::memchr(b'"', &raw[from..]) else { return false };
    let id = &raw[from..from + len];
    let Some(Ev::Turn { id: Some(last), end: last_end, spawns, .. }) = evs.last_mut() else { return false };
    if last.as_bytes() != id {
        return false;
    }
    *last_end = Some(end);
    if has(&n.task_call, raw) || has(&n.agent_call, raw) {
        if let Some(more) = serde_json::from_slice::<Value>(raw).ok().and_then(|v| spawn_ids(&v)) {
            *spawns = Some(match spawns.take() {
                Some(s) => format!("{s} {more}"),
                None => more,
            });
        }
    }
    true
}

/// The ids of the tool calls in an assistant message that launch subagents.
fn spawn_ids(v: &Value) -> Option<String> {
    let ids: Vec<&str> = v
        .get("message")?
        .get("content")?
        .as_array()?
        .iter()
        .filter(|b| {
            b.get("type").and_then(Value::as_str) == Some("tool_use")
                && matches!(b.get("name").and_then(Value::as_str), Some("Task" | "Agent"))
        })
        .filter_map(|b| b.get("id").and_then(Value::as_str))
        .collect();
    (!ids.is_empty()).then(|| ids.join(" "))
}

/// `(subagent id, tool call id)` from a tool result that reports a launched subagent.
fn agent_result(v: &Value) -> Option<(String, String)> {
    let agent = v.get("toolUseResult")?.get("agentId")?.as_str()?;
    let call = v.get("message")?.get("content")?.as_array()?.iter().find_map(|b| {
        (b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .then(|| b.get("tool_use_id").and_then(Value::as_str))
            .flatten()
    })?;
    Some((agent.to_string(), call.to_string()))
}

/// Read the complete lines in `buf`, which starts at byte `base` of a Claude
/// transcript.
fn parse_claude(buf: &[u8], base: u64, sidechain: bool) -> Parsed {
    let mut out = Parsed::default();
    let mut pos = base;
    let mut lines = 0u64;
    // Time since the last lap goes to one of `PARSE_STEPS`.
    let mut steps = [0u64; PARSE_STEPS.len()];
    let mut clock = Instant::now();
    macro_rules! lap {
        ($i:expr) => {{
            let now = Instant::now();
            steps[$i] += (now - clock).as_nanos() as u64;
            clock = now;
        }};
    }
    // `memchr` finds line ends far faster than a per-byte closure, which dev builds
    // compile unoptimized.
    let mut rest = buf;
    while !rest.is_empty() {
        let cut = memchr::memchr(b'\n', rest).unwrap_or(rest.len());
        let raw = &rest[..cut];
        rest = rest.get(cut + 1..).unwrap_or(&[]);
        let start = pos;
        pos += raw.len() as u64 + 1;
        if raw.is_empty() {
            continue;
        }
        lines += 1;
        let line = || String::from_utf8_lossy(raw);
        let n = &*NEEDLES;
        // User lines state their type up front and assistant lines open their message
        // with the model; either way the searches for other kinds are skipped.
        let head = &raw[..raw.len().min(256)];
        let user = has(&n.user, head);
        let assistant = !user && has(&n.assistant_head, head);
        lap!(0);
        if !user && has(&n.rate_limit, raw) {
            rate_limit_resets(&line(), &mut out.resets);
        }
        if out.cwd.is_none() && has(&n.cwd, raw) {
            out.cwd = serde_json::from_str::<Value>(&line())
                .ok()
                .and_then(|v| v.get("cwd").and_then(Value::as_str).map(str::to_string));
        }
        if raw.starts_with(b"{\"type\":\"custom-title\"") {
            let title = serde_json::from_str::<Value>(&line())
                .ok()
                .and_then(|v| v.get("customTitle").and_then(Value::as_str).map(str::to_string));
            if title.is_some() {
                out.title = title;
            }
        }
        lap!(1);
        if !user && !assistant && has(&n.compact_boundary, raw) {
            out.evs.push(Ev::Compact);
            lap!(2);
            continue;
        }
        let usage_line = !user && (assistant || (has(&n.usage, raw) && has(&n.assistant, raw)));
        lap!(2);
        if usage_line {
            if extends_turn(raw, pos, &mut out.evs) {
                lap!(3);
                continue;
            }
            lap!(3);
            let typed = serde_json::from_slice::<AssistantLine>(raw);
            lap!(4);
            if let Ok(a) = typed {
                let msg = a.message;
                let usage = msg.as_ref().and_then(|m| m.usage.as_ref()).or(a.usage.as_ref());
                if let (Some("assistant"), Some(u)) = (a.kind.as_deref(), usage) {
                    // A turn with no timestamp cannot be placed on a day.
                    if let Some(ts) = a.timestamp.as_deref().and_then(scan::iso_to_millis) {
                        let model = msg.as_ref().and_then(|m| m.model.clone());
                        let cost = scan::token_cost(
                            model.as_deref().unwrap_or(""),
                            u.input_tokens,
                            u.output_tokens,
                            u.cache_creation_input_tokens,
                            u.cache_read_input_tokens,
                        );
                        lap!(5);
                        // Only messages that launch a subagent need the whole record.
                        let spawns = (has(&n.task_call, raw) || has(&n.agent_call, raw))
                            .then(|| serde_json::from_slice::<Value>(raw).ok())
                            .flatten()
                            .and_then(|v| spawn_ids(&v));
                        lap!(6);
                        out.evs.push(Ev::Turn {
                            id: msg.and_then(|m| m.id),
                            ts,
                            cost,
                            model,
                            start: Some(start),
                            end: Some(pos),
                            spawns,
                            bubble: None,
                        });
                    }
                    lap!(5);
                    continue;
                }
            }
        }
        if has(&n.tool_use_result, raw) && has(&n.agent_id, raw) {
            if let Some(link) = serde_json::from_str::<Value>(&line()).ok().as_ref().and_then(agent_result) {
                out.agents.push(link);
            }
            lap!(7);
            continue;
        }
        let prompt = if sidechain { user && is_sidechain_prompt(raw) } else { user && is_user_prompt(raw) };
        if prompt {
            let ts = serde_json::from_str::<Value>(&line())
                .ok()
                .and_then(|v| v.get("timestamp").and_then(Value::as_str).and_then(scan::iso_to_millis))
                .unwrap_or(0);
            out.evs.push(Ev::Prompt { off: Some(start), ts });
        }
        lap!(8);
    }
    for (total, step) in PARSE_STEPS_NS.iter().zip(steps) {
        total.fetch_add(step, Ordering::Relaxed);
    }
    PARSE_LINES.fetch_add(lines, Ordering::Relaxed);
    out
}

/// Write one conversation's new events under its session row. Returns the segment
/// count after them.
fn store(tx: &Connection, sid: i64, evs: &[Ev]) -> rusqlite::Result<i64> {
    let mut segment: i64 = tx.query_row("SELECT segment FROM sessions WHERE id = ?1", [sid], |r| r.get(0))?;
    // Continue the last exchange, unless a /compact came after it.
    let last: Option<(i64, i64, i64)> = tx
        .query_row(
            "SELECT id, no, segment FROM exchanges WHERE session = ?1 ORDER BY id DESC LIMIT 1",
            [sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (mut cur, mut no) = match last {
        Some((id, no, seg)) if seg == segment => (Some(id), no),
        _ => (None, 0),
    };
    let mut ins_exchange =
        tx.prepare_cached("INSERT INTO exchanges (session, no, segment, off, ts) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    // A message streamed over several lines upserts: later lines widen the range and
    // add the subagent calls they carry.
    let mut ins_turn = tx.prepare_cached(
        "INSERT INTO turns (session, exchange, msg_id, ts, cost, total, model, off_start, off_end, spawns, bubble)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(session, msg_id) DO UPDATE SET
             off_end = MAX(off_end, excluded.off_end),
             spawns = CASE WHEN excluded.spawns IS NULL THEN spawns
                           WHEN spawns IS NULL THEN excluded.spawns
                           ELSE spawns || ' ' || excluded.spawns END",
    )?;
    let mut touch = tx.prepare_cached("INSERT OR IGNORE INTO touched (msg_id) VALUES (?1)")?;
    for ev in evs {
        match ev {
            Ev::Compact => {
                segment += 1;
                cur = None;
                no = 0;
            }
            Ev::Prompt { off, ts } => {
                no += 1;
                ins_exchange.execute(params![sid, no, segment, off.map(|o| o as i64), ts])?;
                cur = Some(tx.last_insert_rowid());
            }
            Ev::Turn { id, ts, cost, model, start, end, spawns, bubble } => {
                // A turn with no prompt of its own still belongs to an exchange.
                let ex = match cur {
                    Some(ex) => ex,
                    None => {
                        ins_exchange.execute(params![sid, 0, segment, Option::<i64>::None, ts])?;
                        let ex = tx.last_insert_rowid();
                        cur = Some(ex);
                        ex
                    }
                };
                ins_turn.execute(params![
                    sid,
                    ex,
                    id,
                    ts,
                    cost,
                    model,
                    start.map(|o| o as i64),
                    end.map(|o| o as i64),
                    spawns,
                    bubble
                ])?;
                if let Some(id) = id {
                    touch.execute([id])?;
                }
            }
        }
    }
    Ok(segment)
}

/// Stretch a session's first/last turn times to cover the new turns, so they never
/// need a scan over its turns.
fn widen_span(tx: &Connection, sid: i64, evs: &[Ev]) -> rusqlite::Result<()> {
    let times = evs.iter().filter_map(|e| if let Ev::Turn { ts, .. } = e { Some(*ts) } else { None });
    let (lo, hi) = times.fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
    if lo > hi {
        return Ok(());
    }
    tx.execute(
        "UPDATE sessions SET first_ms = MIN(COALESCE(first_ms, ?2), ?2), last_ms = MAX(COALESCE(last_ms, ?3), ?3) WHERE id = ?1",
        params![sid, lo, hi],
    )?;
    Ok(())
}

/// Remove the exchanges and turns of a session, keeping its row. The days they
/// were spent on and the messages whose copies need re-ranking are queued.
fn clear_children(tx: &Connection, sid: i64) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO dirty
         SELECT s.directory, strftime('%Y-%m-%d', t.ts / 1000, 'unixepoch', 'localtime')
         FROM turns t JOIN sessions s ON s.id = t.session WHERE t.session = ?1",
        [sid],
    )?;
    tx.execute("INSERT OR IGNORE INTO touched SELECT msg_id FROM turns WHERE session = ?1 AND msg_id IS NOT NULL", [sid])?;
    tx.execute("UPDATE sessions SET parent_turn = NULL WHERE parent_turn IN (SELECT id FROM turns WHERE session = ?1)", [sid])?;
    tx.execute("UPDATE spawn_links SET resolved = 0 WHERE session = ?1", [sid])?;
    tx.execute("DELETE FROM turns WHERE session = ?1", [sid])?;
    tx.execute("DELETE FROM exchanges WHERE session = ?1", [sid])?;
    tx.execute("UPDATE sessions SET segment = 0, first_ms = NULL, last_ms = NULL WHERE id = ?1", [sid])?;
    tx.execute("UPDATE windows SET stale = 1", [])?;
    Ok(())
}

/// Remove a session and everything under it.
fn delete_session(tx: &Connection, sid: i64) -> rusqlite::Result<()> {
    tx.execute("INSERT OR IGNORE INTO dirs SELECT directory FROM sessions WHERE id = ?1", [sid])?;
    clear_children(tx, sid)?;
    tx.execute("DELETE FROM spawn_links WHERE session = ?1", [sid])?;
    tx.execute("DELETE FROM sessions WHERE id = ?1", [sid])?;
    Ok(())
}

fn mtime_ms(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The directory row for `(harness, path)`, created on first sight. A new
/// directory can change how directories fold into projects, so it asks for a
/// regroup.
fn ensure_directory(tx: &Connection, harness: i64, path: &str) -> rusqlite::Result<i64> {
    let added = tx.execute("INSERT OR IGNORE INTO directories (harness, path) VALUES (?1, ?2)", params![harness, path])?;
    if added > 0 {
        tx.execute("INSERT INTO regroup VALUES (1)", [])?;
    }
    tx.query_row("SELECT id FROM directories WHERE harness = ?1 AND path = ?2", params![harness, path], |r| r.get(0))
}

/// Parses the bytes of a conversation file read from `base`; the third argument is
/// the model carried over from earlier reads.
type ParseFn<'a> = &'a dyn Fn(&[u8], u64, Option<&str>) -> Parsed;

/// Where a file-backed session stands in the database.
struct Row {
    id: i64,
    path: Option<String>,
    mtime: Option<i64>,
    len: Option<i64>,
    offset: i64,
    model: Option<String>,
}

/// A conversation file with something new in it: where the next read starts and the
/// state the database held for it.
struct Work {
    existing: Option<i64>,
    from: u64,
    model: Option<String>,
    mtime: i64,
    len: i64,
}

/// Decide whether a conversation file needs reading. Returns `None` when the
/// database is already current.
fn plan_file(
    tx: &Connection,
    harness: i64,
    path: &Path,
    md: &std::fs::Metadata,
    key: &str,
) -> rusqlite::Result<Option<Work>> {
    let (mtime, len) = (mtime_ms(md), md.len() as i64);
    let row: Option<Row> = tx
        .prepare_cached("SELECT id, path, mtime, len, COALESCE(offset, 0), model FROM sessions WHERE harness = ?1 AND key = ?2")?
        .query_row(params![harness, key], |r| {
            Ok(Row {
                id: r.get(0)?,
                path: r.get(1)?,
                mtime: r.get(2)?,
                len: r.get(3)?,
                offset: r.get(4)?,
                model: r.get(5)?,
            })
        })
        .optional()?;
    let work = match row {
        Some(r) if r.mtime == Some(mtime) && r.len == Some(len) => {
            // Archiving moves a Codex rollout without touching its bytes.
            let path_s = path.to_string_lossy();
            if r.path.as_deref() != Some(&*path_s) {
                tx.execute("UPDATE sessions SET path = ?2 WHERE id = ?1", params![r.id, &*path_s])?;
            }
            None
        }
        Some(r) if len >= r.offset => Some(Work { existing: Some(r.id), from: r.offset as u64, model: r.model, mtime, len }),
        other => {
            // Rewritten shorter: start over.
            if let Some(r) = other {
                delete_session(tx, r.id)?;
            }
            Some(Work { existing: None, from: 0, model: None, mtime, len })
        }
    };
    Ok(work)
}

/// Read what was appended to a conversation file and parse it. Touches no database,
/// so several files can be done at once. Returns the parse and the bytes consumed.
fn read_parse(path: &Path, work: &Work, parse: ParseFn, buf: &mut Vec<u8>) -> Option<(Parsed, usize)> {
    let t0 = Instant::now();
    let mut f = std::fs::File::open(path).ok()?;
    f.seek(SeekFrom::Start(work.from)).ok()?;
    let t_open = Instant::now();
    // Each worker reuses one buffer: fresh memory for every large file costs a page
    // fault per 4 KB.
    buf.clear();
    buf.reserve((work.len as u64).saturating_sub(work.from) as usize + 1);
    f.read_to_end(buf).ok()?;
    READ_OPEN_NS.fetch_add((t_open - t0).as_nanos() as u64, Ordering::Relaxed);
    READ_BYTES_NS.fetch_add(t_open.elapsed().as_nanos() as u64, Ordering::Relaxed);
    // Stop at the last full line: a transcript still being written ends mid-line.
    let end = memchr::memrchr(b'\n', &buf).map_or(0, |i| i + 1);
    let t1 = Instant::now();
    let parsed = parse(&buf[..end], work.from, work.model.as_deref());
    FILE_READ_NS.fetch_add((t1 - t0).as_nanos() as u64, Ordering::Relaxed);
    FILE_PARSE_NS.fetch_add(t1.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Some((parsed, end))
}

/// Write a parsed conversation file into the database.
fn finish_file(
    tx: &Connection,
    harness: i64,
    path: &Path,
    work: &Work,
    key: &str,
    parsed: &Parsed,
    end: usize,
    sid: &str,
    agent: Option<&str>,
    fallback_dir: &dyn Fn() -> String,
) -> rusqlite::Result<()> {
    let t = Instant::now();
    let path_s = path.to_string_lossy().to_string();
    let (from, mtime, len) = (work.from, work.mtime, work.len);
    let session = match work.existing {
        Some(id) => id,
        None => {
            let dir = ensure_directory(tx, harness, &parsed.cwd.clone().unwrap_or_else(fallback_dir))?;
            tx.execute(
                "INSERT INTO sessions (harness, directory, key, sid, agent_id, path, mtime, len, offset)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0)",
                params![harness, dir, key, sid, agent, path_s, mtime, len],
            )?;
            tx.last_insert_rowid()
        }
    };
    let segment = store(tx, session, &parsed.evs)?;
    tx.execute(
        "UPDATE sessions SET path = ?2, mtime = ?3, len = ?4, offset = ?5, segment = ?6,
             model = COALESCE(?7, model), title = COALESCE(?8, title) WHERE id = ?1",
        params![session, path_s, mtime, len, (from + end as u64) as i64, segment, parsed.model, parsed.title],
    )?;
    widen_span(tx, session, &parsed.evs)?;
    for (name, at) in [("reset5", parsed.resets.five), ("reset7", parsed.resets.seven)] {
        if let Some(at) = at {
            tx.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = MAX(CAST(value AS INTEGER), CAST(excluded.value AS INTEGER))",
                params![name, at],
            )?;
        }
    }
    for (agent_id, call) in &parsed.agents {
        tx.execute(
            "INSERT OR IGNORE INTO spawn_links (agent_id, session, tool_use_id) VALUES (?1, ?2, ?3)",
            params![agent_id, session, call],
        )?;
    }
    tx.execute("INSERT OR IGNORE INTO changed (id) VALUES (?1)", [session])?;
    FILE_STORE_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    Ok(())
}

/// Bring one conversation file up to date, reading only what was appended since
/// last time. Returns whether anything was read.
fn sync_file(
    tx: &Connection,
    harness: i64,
    path: &Path,
    md: &std::fs::Metadata,
    key: &str,
    sid: &str,
    agent: Option<&str>,
    fallback_dir: &dyn Fn() -> String,
    parse: ParseFn,
) -> rusqlite::Result<bool> {
    let Some(work) = plan_file(tx, harness, path, md, key)? else { return Ok(false) };
    let Some((parsed, end)) = read_parse(path, &work, parse, &mut Vec::new()) else { return Ok(false) };
    finish_file(tx, harness, path, &work, key, &parsed, end, sid, agent, fallback_dir)?;
    Ok(true)
}

/// Every `.jsonl` file under `root`. Threads share one stack of folders still to
/// read: listing a folder is a system call that mostly waits, so many can be in
/// flight at once, and folders found by one thread are picked up by any.
fn collect_parallel(root: &Path) -> Vec<(PathBuf, std::fs::Metadata)> {
    let stack = Mutex::new(vec![root.to_path_buf()]);
    // Folders on the stack plus those being read; zero means the walk is over.
    let pending = AtomicUsize::new(1);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get() * 2).min(32);
    let mut found = Vec::new();
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let (mut files, mut dirs) = (Vec::new(), Vec::new());
                    loop {
                        let next = stack.lock().ok().and_then(|mut s| s.pop());
                        let Some(dir) = next else {
                            if pending.load(Ordering::Acquire) == 0 {
                                break;
                            }
                            std::thread::yield_now();
                            continue;
                        };
                        dirs.clear();
                        collect_level(&dir, &mut files, &mut dirs);
                        pending.fetch_add(dirs.len(), Ordering::AcqRel);
                        if let Ok(mut s) = stack.lock() {
                            s.append(&mut dirs);
                        }
                        pending.fetch_sub(1, Ordering::AcqRel);
                    }
                    files
                })
            })
            .collect();
        for w in workers {
            found.extend(w.join().unwrap_or_default());
        }
    });
    found
}

/// One folder's transcripts into `files`, its subfolders into `dirs`. Saved tool output,
/// about half of all folders, never holds a transcript.
fn collect_level(dir: &Path, files: &mut Vec<(PathBuf, std::fs::Metadata)>, dirs: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() && e.file_name() != "tool-results" => dirs.push(path),
            Ok(t) if t.is_file() && path.extension().is_some_and(|x| x == "jsonl") => {
                if let Ok(md) = e.metadata() {
                    files.push((path, md));
                }
            }
            _ => {}
        }
    }
}

/// Every Claude Code transcript under `root`, subagent runs included.
fn sync_claude(tx: &Connection, root: &Path) -> rusqlite::Result<usize> {
    let mut files = 0;
    let t = Instant::now();
    let mut paths = collect_parallel(root);
    paths.sort_by(|a, b| a.0.cmp(&b.0));
    WALK_NS.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    // Debug builds only: GREEDOUT_MAX_FILES=200 reads just that many Claude files, so a
    // first scan finishes in seconds. The rest are picked up once the variable is gone.
    let cap = if cfg!(debug_assertions) {
        std::env::var("GREEDOUT_MAX_FILES").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX)
    } else {
        usize::MAX
    };
    // Plan on this thread (it reads the database), parse a batch of files on several
    // threads (it does not), store on this thread, in path order.
    struct Job {
        path: PathBuf,
        key: String,
        sid: String,
        agent: Option<String>,
        sidechain: bool,
        folder: String,
        work: Work,
    }
    let t_plan = Instant::now();
    let mut jobs = Vec::new();
    for (p, md) in paths.into_iter().take(cap) {
        let rel = p.strip_prefix(root).unwrap_or(&p);
        let key = rel.to_string_lossy().replace('\\', "/");
        let Some(work) = plan_file(tx, CLAUDE, &p, &md, &key)? else { continue };
        let sid = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let agent = sid.strip_prefix("agent-").map(str::to_string);
        let sidechain = rel.components().any(|c| c.as_os_str() == "subagents");
        // Without a recorded working directory, the transcript folder's name says
        // where the session ran.
        let folder = rel.components().next().map(|c| c.as_os_str().to_string_lossy().to_string()).unwrap_or_default();
        jobs.push(Job { path: p, key, sid, agent, sidechain, folder, work });
    }
    PLAN_NS.fetch_add(t_plan.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    for batch in jobs.chunks(64) {
        let next = AtomicUsize::new(0);
        let t_batch = Instant::now();
        let mut done: Vec<Option<(Parsed, usize)>> = std::thread::scope(|s| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let mut local = Vec::new();
                        let mut buf = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(job) = batch.get(i) else { break };
                            let parse = |buf: &[u8], base: u64, _m: Option<&str>| parse_claude(buf, base, job.sidechain);
                            local.push((i, read_parse(&job.path, &job.work, &parse, &mut buf)));
                        }
                        local
                    })
                })
                .collect();
            let mut slots: Vec<Option<(Parsed, usize)>> = (0..batch.len()).map(|_| None).collect();
            for w in workers {
                for (i, r) in w.join().unwrap_or_default() {
                    slots[i] = r;
                }
            }
            slots
        });
        BATCH_NS.fetch_add(t_batch.elapsed().as_nanos() as u64, Ordering::Relaxed);
        for (job, slot) in batch.iter().zip(done.iter_mut()) {
            let Some((parsed, end)) = slot.take() else { continue };
            let fallback = || scan::decode_project_dir(&job.folder);
            finish_file(tx, CLAUDE, &job.path, &job.work, &job.key, &parsed, end, &job.sid, job.agent.as_deref(), &fallback)?;
            files += 1;
        }
    }
    Ok(files)
}

/// The given Codex rollout transcripts.
fn sync_codex(tx: &Connection, files: &[PathBuf]) -> rusqlite::Result<usize> {
    let mut n = 0;
    for p in files {
        let id = crate::codex::id_from_path(p);
        let Ok(md) = std::fs::metadata(p) else { continue };
        let parse = |buf: &[u8], base: u64, model: Option<&str>| crate::codex::ledger_parse(buf, base, model, &id);
        if sync_file(tx, CODEX, p, &md, &id, &id, None, &|| "Codex".to_string(), &parse)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Every Cursor chat whose version changed since it was last read. All chats live
/// in one shared database, so its modified time gates the whole pass.
fn sync_cursor(tx: &Connection) -> rusqlite::Result<usize> {
    let Some(db_ms) = crate::cursor::db_file_mtime_ms() else { return Ok(0) };
    let seen: Option<i64> = tx
        .query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'cursor_db'", [], |r| r.get(0))
        .optional()?;
    if seen == Some(db_ms) {
        return Ok(0);
    }
    let mut n = 0;
    for (path, mtime_secs) in crate::cursor::candidates() {
        let id = crate::cursor::id_from_path(&path);
        let stamp = mtime_secs as i64;
        let row: Option<(i64, Option<i64>)> = tx
            .query_row("SELECT id, stamp FROM sessions WHERE harness = ?1 AND key = ?2", params![CURSOR, id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if matches!(row, Some((_, Some(s))) if s == stamp) {
            continue;
        }
        let Some(chat) = crate::cursor::ledger_composer(&id) else { continue };
        let session = match row {
            Some((sid, _)) => {
                clear_children(tx, sid)?;
                sid
            }
            None => {
                let dir = ensure_directory(tx, CURSOR, &chat.cwd.clone().unwrap_or_else(|| "Cursor".to_string()))?;
                tx.execute(
                    "INSERT INTO sessions (harness, directory, key, sid) VALUES (?1, ?2, ?3, ?3)",
                    params![CURSOR, dir, id],
                )?;
                tx.last_insert_rowid()
            }
        };
        let evs: Vec<Ev> = chat
            .bubbles
            .iter()
            .flat_map(|b| {
                let prompt = b.user.then_some(Ev::Prompt { off: None, ts: b.ts });
                let turn = (b.cost > 0.0).then(|| Ev::Turn {
                    id: None,
                    ts: b.ts,
                    cost: b.cost,
                    model: chat.model.clone(),
                    start: None,
                    end: None,
                    spawns: None,
                    bubble: Some(b.key.clone()),
                });
                prompt.into_iter().chain(turn)
            })
            .collect();
        store(tx, session, &evs)?;
        tx.execute(
            "UPDATE sessions SET stamp = ?2, mtime = ?3, title = ?4, model = ?5 WHERE id = ?1",
            params![session, stamp, stamp * 1000, chat.title, chat.model],
        )?;
        widen_span(tx, session, &evs)?;
        tx.execute("INSERT OR IGNORE INTO changed (id) VALUES (?1)", [session])?;
        n += 1;
    }
    tx.execute(
        "INSERT INTO meta (key, value) VALUES ('cursor_db', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [db_ms],
    )?;
    Ok(n)
}

// Time inside `sync_file`, split by what it was doing; `sync` logs and resets them.
static WALK_NS: AtomicU64 = AtomicU64::new(0);
static PLAN_NS: AtomicU64 = AtomicU64::new(0);
static BATCH_NS: AtomicU64 = AtomicU64::new(0);
static FILE_READ_NS: AtomicU64 = AtomicU64::new(0);
static FILE_PARSE_NS: AtomicU64 = AtomicU64::new(0);
static FILE_STORE_NS: AtomicU64 = AtomicU64::new(0);

// Inside `parse_claude`, thread time by step: cutting out a line and judging its kind
// from its head; the rate-limit, working-directory and title checks; the kind
// searches that pick out usage lines; a streamed line that continues the previous
// turn; the typed JSON parse of a usage line; its cost; the whole-record parse of a
// message that launches subagents; the tool result that names a subagent; and user
// prompts.
const PARSE_STEPS: [&str; 9] = [
    "cut and head",
    "rate, cwd, title",
    "kind searches",
    "continued turn",
    "usage json",
    "cost",
    "spawn json",
    "subagent link",
    "prompt",
];
static PARSE_STEPS_NS: [AtomicU64; PARSE_STEPS.len()] = [const { AtomicU64::new(0) }; PARSE_STEPS.len()];
static PARSE_LINES: AtomicU64 = AtomicU64::new(0);
// Inside `read_parse`: opening the file, then reading it.
static READ_OPEN_NS: AtomicU64 = AtomicU64::new(0);
static READ_BYTES_NS: AtomicU64 = AtomicU64::new(0);

fn take_ms(c: &AtomicU64) -> f64 {
    c.swap(0, Ordering::Relaxed) as f64 / 1e6
}

/// Time spent in each step of a sync, in order; `sync` logs it.
struct Phases {
    last: Instant,
    steps: Vec<(&'static str, std::time::Duration)>,
}

impl Phases {
    fn new() -> Self {
        Phases { last: Instant::now(), steps: Vec::new() }
    }

    /// Charge the time since the previous mark to `name`.
    fn mark(&mut self, name: &'static str) {
        let now = Instant::now();
        self.steps.push((name, now - self.last));
        self.last = now;
    }

    fn line(&self) -> String {
        self.steps.iter().map(|(n, d)| format!("{n} {:.1}ms", d.as_secs_f64() * 1000.0)).collect::<Vec<_>>().join(", ")
    }
}

/// Bring the ledger up to date with every conversation. Returns how many changed.
fn sync_sources(
    conn: &Connection,
    claude_root: &Path,
    codex: &[PathBuf],
    cursor: bool,
    ph: &mut Phases,
) -> rusqlite::Result<usize> {
    let watermark: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM turns", [], |r| r.get(0))?;
    let tx = conn.unchecked_transaction()?;
    ph.mark("begin");
    let mut files = sync_claude(&tx, claude_root)?;
    ph.mark("claude");
    files += sync_codex(&tx, codex)?;
    ph.mark("codex");
    if cursor {
        files += sync_cursor(&tx)?;
    }
    ph.mark("cursor");
    settle(&tx, watermark, ph)?;
    tx.commit()?;
    ph.mark("commit");
    Ok(files)
}

/// Re-rank copies of the messages that moved, then roll the changes up the tree.
fn settle(tx: &Connection, watermark: i64, ph: &mut Phases) -> rusqlite::Result<()> {
    // Exactly one copy of each message counts: the one in the lowest-path session.
    tx.execute(
        "UPDATE turns SET counted = 1 - counted
         WHERE msg_id IN (SELECT msg_id FROM touched)
           AND counted != (CASE WHEN session = (
                 SELECT t2.session FROM turns t2 JOIN sessions s2 ON s2.id = t2.session
                 WHERE t2.msg_id = turns.msg_id ORDER BY s2.path LIMIT 1
               ) THEN 1 ELSE 0 END)",
        [],
    )?;
    if tx.changes() > 0 {
        tx.execute("UPDATE windows SET stale = 1", [])?;
    }
    tx.execute_batch(
        "INSERT OR IGNORE INTO changed
             SELECT DISTINCT session FROM turns WHERE msg_id IN (SELECT msg_id FROM touched);",
    )?;
    // Days whose spend moved: where new turns landed, and where a copy changed hands.
    tx.execute(
        "INSERT OR IGNORE INTO dirty
         SELECT s.directory, strftime('%Y-%m-%d', t.ts / 1000, 'unixepoch', 'localtime')
         FROM turns t JOIN sessions s ON s.id = t.session
         WHERE t.id > ?1 OR t.msg_id IN (SELECT msg_id FROM touched)",
        [watermark],
    )?;
    ph.mark("rerank+dirty");

    // Exchanges that gained turns or lost a counted copy need re-summing; every other
    // exchange keeps its stored totals.
    tx.execute("INSERT OR IGNORE INTO ex SELECT exchange FROM turns WHERE id > ?1", [watermark])?;
    tx.execute("INSERT OR IGNORE INTO ex SELECT exchange FROM turns WHERE msg_id IN (SELECT msg_id FROM touched)", [])?;
    ph.mark("ex");

    link_subagents(tx)?;
    ph.mark("link");
    rollup_sessions(tx)?;
    ph.mark("rollup");

    // Directories, then the projects and harnesses above them.
    tx.execute("INSERT OR IGNORE INTO dirs SELECT directory FROM sessions WHERE id IN (SELECT id FROM changed)", [])?;
    tx.execute_batch(
        "UPDATE directories SET
             cost = COALESCE((SELECT SUM(cost) FROM sessions WHERE directory = directories.id), 0),
             n_turns = COALESCE((SELECT SUM(n_turns) FROM sessions WHERE directory = directories.id), 0)
         WHERE id IN (SELECT id FROM dirs);",
    )?;
    ph.mark("directories");
    let regrouped: i64 = tx.query_row("SELECT COUNT(*) FROM regroup", [], |r| r.get(0))?;
    if regrouped > 0 {
        regroup(tx)?;
    }
    ph.mark("regroup");
    tx.execute_batch(
        "UPDATE projects SET
             cost = COALESCE((SELECT SUM(cost) FROM directories WHERE project = projects.id), 0),
             n_turns = COALESCE((SELECT SUM(n_turns) FROM directories WHERE project = projects.id), 0)
         WHERE id IN (SELECT project FROM directories WHERE id IN (SELECT id FROM dirs));
         UPDATE harnesses SET
             cost = COALESCE((SELECT SUM(cost) FROM directories WHERE harness = harnesses.id), 0),
             n_turns = COALESCE((SELECT SUM(n_turns) FROM directories WHERE harness = harnesses.id), 0)
         WHERE id IN (SELECT harness FROM directories WHERE id IN (SELECT id FROM dirs));",
    )?;

    ph.mark("projects+harnesses");
    retotal_days(tx)?;
    ph.mark("daily");
    tx.execute_batch(
        "DELETE FROM touched; DELETE FROM changed; DELETE FROM level; DELETE FROM next; DELETE FROM ex;
         DELETE FROM dirs; DELETE FROM dirty; DELETE FROM regroup;",
    )
}

/// Attach each subagent run to the tool call that launched it.
fn link_subagents(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute_batch(
        "UPDATE sessions SET parent_turn = (
             SELECT t.id FROM spawn_links l JOIN turns t
                 ON t.session = l.session AND instr(t.spawns, l.tool_use_id) > 0
             WHERE l.agent_id = sessions.agent_id ORDER BY t.counted DESC LIMIT 1)
         WHERE parent_turn IS NULL
           AND agent_id IN (SELECT agent_id FROM spawn_links WHERE resolved = 0);
         INSERT OR IGNORE INTO changed
             SELECT id FROM sessions WHERE parent_turn IS NOT NULL
                 AND agent_id IN (SELECT agent_id FROM spawn_links WHERE resolved = 0);
         UPDATE spawn_links SET resolved = 1
             WHERE resolved = 0 AND agent_id IN (SELECT agent_id FROM sessions WHERE parent_turn IS NOT NULL);",
    )
}

/// Re-sum the changed exchanges from their turns and the changed sessions from
/// their exchanges. A session that launched a changed subagent run has a launching
/// turn whose total moved, so that turn, its exchange and its session come next,
/// and so on up the nesting.
fn rollup_sessions(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute("INSERT OR IGNORE INTO level SELECT id FROM changed", [])?;
    for _ in 0..8 {
        tx.execute_batch(
            "UPDATE exchanges SET
                 cost = COALESCE((SELECT SUM(cost) FROM turns WHERE exchange = exchanges.id AND counted = 1), 0),
                 total = COALESCE((SELECT SUM(total) FROM turns WHERE exchange = exchanges.id AND counted = 1), 0),
                 n_turns = (SELECT COUNT(*) FROM turns WHERE exchange = exchanges.id AND counted = 1)
             WHERE id IN (SELECT id FROM ex);
             DELETE FROM ex;

             UPDATE sessions SET
                 cost = COALESCE((SELECT SUM(cost) FROM exchanges WHERE session = sessions.id), 0),
                 total = COALESCE((SELECT SUM(total) FROM exchanges WHERE session = sessions.id), 0),
                 n_turns = COALESCE((SELECT SUM(n_turns) FROM exchanges WHERE session = sessions.id), 0)
             WHERE id IN (SELECT id FROM level);

             UPDATE turns SET total = cost + COALESCE((SELECT SUM(s.total) FROM sessions s WHERE s.parent_turn = turns.id), 0)
             WHERE id IN (SELECT parent_turn FROM sessions WHERE id IN (SELECT id FROM level) AND parent_turn IS NOT NULL);

             DELETE FROM next;
             INSERT OR IGNORE INTO next
                 SELECT t.session FROM sessions s JOIN turns t ON t.id = s.parent_turn
                 WHERE s.id IN (SELECT id FROM level);
             INSERT OR IGNORE INTO ex
                 SELECT t.exchange FROM sessions s JOIN turns t ON t.id = s.parent_turn
                 WHERE s.id IN (SELECT id FROM level);",
        )?;
        let parents: i64 = tx.query_row("SELECT COUNT(*) FROM next", [], |r| r.get(0))?;
        if parents == 0 {
            break;
        }
        tx.execute_batch(
            "DELETE FROM level;
             INSERT INTO level SELECT id FROM next;
             INSERT OR IGNORE INTO changed SELECT id FROM next;",
        )?;
    }
    tx.execute("DELETE FROM ex", [])?;
    Ok(())
}

/// Assign every directory to its folded project (see `grouping`) and re-sum all
/// project totals. Needed only when a directory appears for the first time.
fn regroup(tx: &Connection) -> rusqlite::Result<()> {
    let dirs: Vec<(i64, String)> = {
        let mut st = tx.prepare("SELECT id, path FROM directories")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let paths: Vec<PathBuf> = dirs.iter().filter(|(_, p)| !p.is_empty()).map(|(_, p)| PathBuf::from(p)).collect();
    let groups = grouping::group_dirs(&paths);
    for (id, path) in &dirs {
        let Some(g) = groups.get(&PathBuf::from(path)) else { continue };
        let root = g.root.to_string_lossy().to_string();
        tx.execute(
            "INSERT INTO projects (root, label) VALUES (?1, ?2) ON CONFLICT(root) DO UPDATE SET label = excluded.label",
            params![root, g.label],
        )?;
        tx.execute(
            "UPDATE directories SET project = (SELECT id FROM projects WHERE root = ?2) WHERE id = ?1",
            params![id, root],
        )?;
    }
    tx.execute_batch(
        "UPDATE projects SET
             cost = COALESCE((SELECT SUM(cost) FROM directories WHERE project = projects.id), 0),
             n_turns = COALESCE((SELECT SUM(n_turns) FROM directories WHERE project = projects.id), 0);
         DELETE FROM projects WHERE id NOT IN (SELECT project FROM directories WHERE project IS NOT NULL);",
    )
}

/// Re-total the days that changed in `daily`. Epoch bounds of a local day come from
/// SQLite's own `utc` conversion, so they match the day labels exactly.
fn retotal_days(tx: &Connection) -> rusqlite::Result<()> {
    let dirty: i64 = tx.query_row("SELECT COUNT(*) FROM dirty", [], |r| r.get(0))?;
    if dirty == 0 {
        return Ok(());
    }
    if dirty > 300 {
        // A first scan touches nearly every day: one pass over the turns beats
        // looking each day up.
        return tx.execute_batch(
            "DELETE FROM daily;
             INSERT INTO daily
                 SELECT s.directory, strftime('%Y-%m-%d', t.ts / 1000, 'unixepoch', 'localtime'), SUM(t.cost), COUNT(*)
                 FROM turns t JOIN sessions s ON s.id = t.session
                 WHERE t.counted = 1 AND t.cost > 0
                 GROUP BY 1, 2;",
        );
    }
    tx.execute_batch(
        "DELETE FROM daily WHERE (directory, day) IN (SELECT dir, day FROM dirty);
         INSERT INTO daily
             SELECT d.dir, d.day, SUM(t.cost), COUNT(*)
             FROM dirty d
             JOIN sessions s ON s.directory = d.dir
             JOIN turns t ON t.session = s.id AND t.counted = 1 AND t.cost > 0
                 AND t.ts >= CAST(strftime('%s', d.day, 'utc') AS INTEGER) * 1000
                 AND t.ts <  CAST(strftime('%s', d.day, '+1 day', 'utc') AS INTEGER) * 1000
             GROUP BY d.dir, d.day;",
    )
}

/// Bring the ledger up to date with every conversation on disk.
fn sync(conn: &Connection) -> rusqlite::Result<()> {
    let started = Instant::now();
    let mut ph = Phases::new();
    let codex: Vec<PathBuf> = crate::codex::candidates().into_iter().map(|(p, _)| p).collect();
    ph.mark("list codex");
    let files = sync_sources(conn, &config::claude_dir().join("projects"), &codex, true, &mut ph)?;
    config::debug_log(
        &config::load_config(),
        &format!(
            "ledger sync: {files} files changed in {:?} [{}] (claude files, wall: walk {:.0}ms, plan {:.0}ms, read and parse {:.0}ms, store {:.0}ms; thread time: read {:.0}ms, parse {:.0}ms) \
             (open {:.0}ms, read bytes {:.0}ms) (parse of {} lines: {})",
            started.elapsed(),
            ph.line(),
            take_ms(&WALK_NS),
            take_ms(&PLAN_NS),
            take_ms(&BATCH_NS),
            take_ms(&FILE_STORE_NS),
            take_ms(&FILE_READ_NS),
            take_ms(&FILE_PARSE_NS),
            take_ms(&READ_OPEN_NS),
            take_ms(&READ_BYTES_NS),
            PARSE_LINES.swap(0, Ordering::Relaxed),
            PARSE_STEPS
                .iter()
                .zip(&PARSE_STEPS_NS)
                .map(|(name, ns)| format!("{name} {:.0}ms", take_ms(ns)))
                .collect::<Vec<_>>()
                .join(", "),
        ),
    );
    if files > 100 {
        // A first scan leaves a log as big as the database; fold it back in.
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    }
    Ok(())
}

/// One limit window's spend so far and when it ends.
#[derive(Debug, Clone, Copy, Default)]
pub struct Win {
    pub cost: f64,
    pub end: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Windows {
    pub five: Win,
    pub seven: Win,
}

struct WinRow {
    mode: String,
    start: i64,
    end: i64,
    cost: f64,
    upto: i64,
    last_id: i64,
    stale: bool,
}

fn win_row(conn: &Connection, kind: &str) -> rusqlite::Result<Option<WinRow>> {
    conn.query_row(
        "SELECT mode, start_ms, end_ms, cost, upto, last_id, stale FROM windows WHERE kind = ?1",
        [kind],
        |r| {
            Ok(WinRow {
                mode: r.get(0)?,
                start: r.get(1)?,
                end: r.get(2)?,
                cost: r.get(3)?,
                upto: r.get(4)?,
                last_id: r.get(5)?,
                stale: r.get::<_, i64>(6)? != 0,
            })
        },
    )
    .optional()
}

fn put_win(conn: &Connection, kind: &str, w: &WinRow) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO windows (kind, mode, start_ms, end_ms, cost, upto, last_id, stale)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
        params![kind, w.mode, w.start, w.end, w.cost, w.upto, w.last_id],
    )?;
    Ok(())
}

fn sum_between(conn: &Connection, lo: i64, hi: i64) -> rusqlite::Result<f64> {
    conn.query_row(
        "SELECT COALESCE(SUM(cost), 0) FROM turns WHERE counted = 1 AND ts >= ?1 AND ts < ?2",
        [lo, hi],
        |r| r.get(0),
    )
}

/// Spend in the window ending at `end`, `len` long. Kept as a running total: new
/// turns are added to the stored row, and the total is rebuilt from the turns only
/// when the window moved or earlier turns changed.
fn pinned_window(conn: &Connection, kind: &str, end: i64, len: i64, open_ended: bool) -> rusqlite::Result<f64> {
    let (start, max_id): (i64, i64) =
        (end - len, conn.query_row("SELECT COALESCE(MAX(id), 0) FROM turns", [], |r| r.get(0))?);
    let hi = if open_ended { i64::MAX } else { end };
    let prev = win_row(conn, kind)?.filter(|r| r.mode == "pin" && r.start == start && !r.stale);
    let cost = match prev {
        Some(r) => {
            r.cost
                + conn.query_row(
                    "SELECT COALESCE(SUM(cost), 0) FROM turns WHERE counted = 1 AND id > ?1 AND ts >= ?2 AND ts < ?3",
                    [r.last_id, start, hi],
                    |x| x.get::<_, f64>(0),
                )?
        }
        None => sum_between(conn, start, hi)?,
    };
    put_win(conn, kind, &WinRow { mode: "pin".into(), start, end, cost, upto: 0, last_id: max_id, stale: false })?;
    Ok(cost)
}

/// The 5-hour window with no known reset: it opens with the first turn after the
/// previous window closed and lasts five hours. Folded forward one turn at a time.
fn derived_five(conn: &Connection, now: i64) -> rusqlite::Result<Win> {
    let max_id: i64 = conn.query_row("SELECT COALESCE(MAX(id), 0) FROM turns", [], |r| r.get(0))?;
    let prev = win_row(conn, "five")?.filter(|r| r.mode == "fold" && !r.stale);
    let fresh = |conn: &Connection| -> rusqlite::Result<Vec<(i64, f64)>> {
        let mut st = conn.prepare("SELECT ts, cost FROM turns WHERE counted = 1 AND ts >= ?1 ORDER BY ts")?;
        let rows = st.query_map([now - D7_MS - 24 * 3600 * 1000], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    };
    let mut state: Option<(i64, f64, i64)> = None; // (start, cost, upto)
    let mut events: Option<Vec<(i64, f64)>> = None;
    if let Some(r) = &prev {
        let mut st = conn.prepare("SELECT ts, cost FROM turns WHERE counted = 1 AND id > ?1 ORDER BY ts")?;
        let rows: Vec<(i64, f64)> =
            st.query_map([r.last_id], |x| Ok((x.get(0)?, x.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        // A turn older than what was already folded cannot be added in order.
        if rows.iter().all(|(t, _)| *t > r.upto) {
            state = Some((r.start, r.cost, r.upto));
            events = Some(rows);
        }
    }
    let events = match events {
        Some(e) => e,
        None => fresh(conn)?,
    };
    let (mut start, mut cost, mut upto) = state.map_or((None, 0.0, 0), |(s, c, u)| (Some(s), c, u));
    for (t, c) in events {
        if start.map_or(true, |s| t >= s + H5_MS) {
            start = Some(t);
            cost = 0.0;
        }
        cost += c;
        upto = t;
    }
    let Some(start) = start else { return Ok(Win::default()) };
    put_win(
        conn,
        "five",
        &WinRow { mode: "fold".into(), start, end: start + H5_MS, cost, upto, last_id: max_id, stale: false },
    )?;
    Ok(if start + H5_MS > now { Win { cost, end: Some(start + H5_MS) } } else { Win::default() })
}

/// The newest 5-hour and weekly resets any transcript has recorded.
fn recorded_resets(conn: &Connection) -> rusqlite::Result<(Option<i64>, Option<i64>)> {
    let get = |k: &str| -> rusqlite::Result<Option<i64>> {
        conn.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?1", [k], |r| r.get(0)).optional()
    };
    Ok((get("reset5")?, get("reset7")?))
}

/// Sync, then report the spend of the open 5-hour and weekly windows. `reset5` and
/// `anchor7` are the instants a `/usage` report pinned (zero when none); a reset
/// recorded in a transcript fills in when the report has none.
pub fn windows(now: i64, reset5: i64, anchor7: i64) -> Option<Windows> {
    with_db(|conn| {
        sync(conn)?;
        let (r5, r7) = recorded_resets(conn)?;
        let pin5 = [r5, Some(reset5)].into_iter().flatten().filter(|r| *r > now).max();
        let five = match pin5 {
            Some(end) => Win { cost: pinned_window(conn, "five", end, H5_MS, false)?, end: Some(end) },
            None => derived_five(conn, now)?,
        };
        // The weekly reset is a fixed anchor, rolled forward in whole weeks.
        let anchor = if anchor7 > 0 { Some(anchor7) } else { r7 };
        let end7 = anchor.map(|r| {
            let mut e = r;
            while e <= now {
                e += D7_MS;
            }
            while e - D7_MS > now {
                e -= D7_MS;
            }
            e
        });
        let seven = match end7 {
            Some(end) => Win { cost: pinned_window(conn, "seven", end, D7_MS, true)?, end: Some(end) },
            None => Win { cost: sum_between(conn, now - D7_MS, i64::MAX)?, end: None },
        };
        Ok(Windows { five, seven })
    })
}

/// Spend of the counted turns in `[lo, hi)` epoch ms, for calibration against a
/// `/usage` report's window.
pub fn spent(lo: i64, hi: i64) -> f64 {
    with_db(|conn| sum_between(conn, lo, hi)).unwrap_or(0.0)
}

/// Per-day, per-project spend for the Daily Spend overview, after syncing.
pub fn spend_summary() -> Vec<scan::SpendSummary> {
    with_db(|conn| {
        sync(conn)?;
        let mut st = conn.prepare(
            "SELECT d.day, p.label, SUM(d.cost)
             FROM daily d JOIN directories r ON r.id = d.directory JOIN projects p ON p.id = r.project
             GROUP BY d.day, p.label",
        )?;
        let rows = st.query_map([], |r| Ok(scan::SpendSummary { day: r.get(0)?, project: r.get(1)?, cost: r.get(2)? }))?;
        rows.collect()
    })
    .unwrap_or_default()
}

/// The counted, billed turns in `[lo, hi)` epoch ms, for one day's swim lanes.
pub fn spend_day(lo: i64, hi: i64) -> Vec<scan::SpendEvent> {
    with_db(|conn| {
        let mut st = conn.prepare(
            "SELECT t.ts, t.cost, s.sid, p.label, p.root, h.name, COALESCE(s.title, ''), COALESCE(s.mtime, 0)
             FROM turns t
             JOIN sessions s ON s.id = t.session
             JOIN directories r ON r.id = s.directory
             JOIN projects p ON p.id = r.project
             JOIN harnesses h ON h.id = s.harness
             WHERE t.counted = 1 AND t.cost > 0 AND t.ts >= ?1 AND t.ts < ?2
             ORDER BY t.ts",
        )?;
        let rows = st.query_map([lo, hi], |r| {
            Ok(scan::SpendEvent {
                t: r.get(0)?,
                cost: r.get(1)?,
                session: r.get(2)?,
                project: r.get(3)?,
                project_path: r.get(4)?,
                harness: r.get(5)?,
                title: r.get(6)?,
                mtime: r.get(7)?,
            })
        })?;
        rows.collect()
    })
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("greedout-ledger-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn turn(id: &str, ts: &str) -> String {
        format!(
            "{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{\"id\":\"{id}\",\"model\":\"claude-opus-4-1\",\"usage\":{{\"input_tokens\":1000,\"output_tokens\":1000,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":0}}}}}}\n"
        )
    }

    fn prompt(ts: &str) -> String {
        format!("{{\"type\":\"user\",\"timestamp\":\"{ts}\",\"message\":{{\"role\":\"user\",\"content\":\"hi\"}}}}\n")
    }

    fn append(p: &Path, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::OpenOptions::new().create(true).append(true).open(p).unwrap().write_all(text.as_bytes()).unwrap();
    }

    fn one_turn_cost() -> f64 {
        let v: Value = serde_json::from_str(turn("x", "2026-10-01T00:00:00.000Z").trim()).unwrap();
        scan::assistant_usage(&v).unwrap().2
    }

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        conn
    }

    fn run(conn: &Connection, root: &Path) -> usize {
        sync_sources(conn, root, &[], false, &mut Phases::new()).unwrap()
    }

    fn q<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn growing_file_adds_only_new_turns_and_joins_split_messages() {
        let root = scratch("grow");
        let a = root.join("proj").join("a.jsonl");
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:02.000Z"));
        let conn = db();
        assert_eq!(run(&conn, &root), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 1);
        let end1: i64 = q(&conn, "SELECT off_end FROM turns");
        assert_eq!(end1, fs::metadata(&a).unwrap().len() as i64);

        // Unchanged file: nothing to do.
        assert_eq!(run(&conn, &root), 0);

        // The same message gains another line later; a new prompt and turn follow.
        append(&a, &turn("m1", "2026-10-01T00:00:03.000Z"));
        append(&a, &prompt("2026-10-01T00:01:00.000Z"));
        append(&a, &turn("m2", "2026-10-01T00:01:01.000Z"));
        assert_eq!(run(&conn, &root), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 2);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM exchanges"), 2);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM exchanges WHERE no = 2"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions"), 2);
        assert!((q::<f64>(&conn, "SELECT cost FROM sessions") - 2.0 * one_turn_cost()).abs() < 1e-12);
        assert!(one_turn_cost() > 0.0);
        let end2: i64 = q(&conn, "SELECT off_end FROM turns WHERE msg_id = 'm1'");
        assert!(end2 > end1);
    }

    #[test]
    fn totals_roll_up_every_level_without_rereading_turns() {
        let root = scratch("levels");
        append(&root.join("C--work-app").join("a.jsonl"), &(prompt("2026-10-01T00:00:00.000Z") + &turn("m1", "2026-10-01T00:00:01.000Z")));
        append(&root.join("C--work-other").join("b.jsonl"), &(prompt("2026-10-01T00:00:00.000Z") + &turn("m2", "2026-10-01T00:00:01.000Z")));
        let conn = db();
        run(&conn, &root);
        let c = one_turn_cost();
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM directories"), 2);
        assert!((q::<f64>(&conn, "SELECT cost FROM directories ORDER BY id LIMIT 1") - c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT SUM(cost) FROM projects") - 2.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT cost FROM harnesses WHERE name = 'claude-code'") - 2.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT SUM(cost) FROM daily") - 2.0 * c).abs() < 1e-12);

        // A change lands in every level above it.
        append(&root.join("C--work-app").join("a.jsonl"), &turn("m3", "2026-10-02T00:00:01.000Z"));
        run(&conn, &root);
        assert!((q::<f64>(&conn, "SELECT cost FROM harnesses WHERE name = 'claude-code'") - 3.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT SUM(cost) FROM projects") - 3.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT SUM(cost) FROM daily") - 3.0 * c).abs() < 1e-12);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM daily"), 3);
    }

    #[test]
    fn only_one_copy_of_a_message_counts_and_history_outlives_the_file() {
        let root = scratch("dup");
        let (a, b) = (root.join("p").join("a.jsonl"), root.join("p").join("b.jsonl"));
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        // A resumed session copies the old turn, then adds its own.
        append(&b, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&b, &prompt("2026-10-02T00:00:00.000Z"));
        append(&b, &turn("m2", "2026-10-02T00:00:01.000Z"));
        let conn = db();
        run(&conn, &root);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 3);
        assert_eq!(q::<i64>(&conn, "SELECT SUM(counted) FROM turns"), 2);
        // The lowest path owns the shared message; the copy still lists in b.
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions WHERE path LIKE '%a.jsonl'"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions WHERE path LIKE '%b.jsonl'"), 1);
        assert!((q::<f64>(&conn, "SELECT cost FROM harnesses") - 2.0 * one_turn_cost()).abs() < 1e-12);

        // Deleting the transcript keeps its rows, and the totals with them.
        fs::remove_file(&a).unwrap();
        run(&conn, &root);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM sessions"), 2);
        assert_eq!(q::<i64>(&conn, "SELECT SUM(counted) FROM turns"), 2);
        assert!((q::<f64>(&conn, "SELECT cost FROM harnesses") - 2.0 * one_turn_cost()).abs() < 1e-12);
    }

    #[test]
    fn compaction_restarts_exchange_numbering_and_a_rewritten_file_is_re_read() {
        let root = scratch("compact");
        let a = root.join("p").join("a.jsonl");
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&a, "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"timestamp\":\"2026-10-01T00:00:02.000Z\"}\n");
        append(&a, &prompt("2026-10-01T00:01:00.000Z"));
        append(&a, &turn("m2", "2026-10-01T00:01:01.000Z"));
        let conn = db();
        run(&conn, &root);
        assert_eq!(q::<i64>(&conn, "SELECT segment FROM sessions"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT no FROM exchanges WHERE segment = 1"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM exchanges"), 2);

        // Rewritten shorter: the old rows are replaced, not doubled. The lone turn has
        // no prompt, so it sits in an exchange of its own.
        fs::write(&a, turn("m9", "2026-10-03T00:00:00.000Z")).unwrap();
        run(&conn, &root);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM exchanges"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT no FROM exchanges"), 0);
        assert_eq!(q::<i64>(&conn, "SELECT segment FROM sessions"), 0);
        assert!((q::<f64>(&conn, "SELECT SUM(cost) FROM daily") - one_turn_cost()).abs() < 1e-12);
    }

    #[test]
    fn a_subagent_run_hangs_under_the_turn_that_launched_it() {
        let root = scratch("agent");
        let parent = root.join("p").join("s1.jsonl");
        append(&parent, &prompt("2026-10-01T00:00:00.000Z"));
        append(
            &parent,
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-01T00:00:01.000Z\",\"message\":{\"id\":\"m1\",\"model\":\"claude-opus-4-1\",\"content\":[{\"type\":\"tool_use\",\"id\":\"toolu_A\",\"name\":\"Agent\",\"input\":{}}],\"usage\":{\"input_tokens\":1000,\"output_tokens\":1000}}}\n",
        );
        append(
            &parent,
            "{\"type\":\"user\",\"timestamp\":\"2026-10-01T00:00:02.000Z\",\"toolUseResult\":{\"status\":\"async_launched\",\"agentId\":\"abc\"},\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"toolu_A\",\"content\":\"ok\"}]}}\n",
        );
        let sub = root.join("p").join("s1").join("subagents").join("agent-abc.jsonl");
        append(
            &sub,
            "{\"isSidechain\":true,\"agentId\":\"abc\",\"type\":\"user\",\"timestamp\":\"2026-10-01T00:00:03.000Z\",\"message\":{\"role\":\"user\",\"content\":\"do it\"}}\n",
        );
        append(&sub, &turn("s1", "2026-10-01T00:00:04.000Z"));
        let conn = db();
        run(&conn, &root);
        let c = one_turn_cost();
        // The run is linked, owns a normal exchange, and the launching turn shows its total.
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM sessions WHERE parent_turn IS NOT NULL"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM exchanges WHERE session = (SELECT id FROM sessions WHERE agent_id = 'abc')"), 1);
        assert!((q::<f64>(&conn, "SELECT total FROM turns WHERE msg_id = 'm1'") - 2.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT cost FROM turns WHERE msg_id = 'm1'") - c).abs() < 1e-12);
        let parent_total: f64 = q(&conn, "SELECT total FROM sessions WHERE agent_id IS NULL");
        assert!((parent_total - 2.0 * c).abs() < 1e-12);
        // The directory counts each dollar once.
        assert!((q::<f64>(&conn, "SELECT cost FROM directories") - 2.0 * c).abs() < 1e-12);

        // More work in the subagent flows up through the launching turn.
        append(&sub, &turn("s2", "2026-10-01T00:00:05.000Z"));
        run(&conn, &root);
        assert!((q::<f64>(&conn, "SELECT total FROM turns WHERE msg_id = 'm1'") - 3.0 * c).abs() < 1e-12);
        assert!((q::<f64>(&conn, "SELECT cost FROM directories") - 3.0 * c).abs() < 1e-12);
    }

    #[test]
    fn limit_windows_keep_a_running_total() {
        let root = scratch("windows");
        let a = root.join("p").join("a.jsonl");
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:00.000Z"));
        let conn = db();
        run(&conn, &root);
        let c = one_turn_cost();
        let t0 = scan::iso_to_millis("2026-10-01T00:00:00.000Z").unwrap();
        let now = t0 + 3_600_000;

        let w = derived_five(&conn, now).unwrap();
        assert!((w.cost - c).abs() < 1e-12);
        assert_eq!(w.end, Some(t0 + H5_MS));

        // A new turn inside the window is added to the stored row.
        append(&a, &turn("m2", "2026-10-01T00:30:00.000Z"));
        run(&conn, &root);
        let w = derived_five(&conn, now).unwrap();
        assert!((w.cost - 2.0 * c).abs() < 1e-12);

        // After the window closed, the next turn opens a new one.
        append(&a, &turn("m3", "2026-10-01T06:00:00.000Z"));
        run(&conn, &root);
        let later = t0 + 6 * 3_600_000 + 1000;
        let w = derived_five(&conn, later).unwrap();
        assert!((w.cost - c).abs() < 1e-12);

        // A pinned end sums the turns inside it, then keeps adding.
        let end = t0 + 2 * 3_600_000;
        assert!((pinned_window(&conn, "five", end, H5_MS, false).unwrap() - 2.0 * c).abs() < 1e-12);
        append(&a, &turn("m4", "2026-10-01T01:00:00.000Z"));
        run(&conn, &root);
        assert!((pinned_window(&conn, "five", end, H5_MS, false).unwrap() - 3.0 * c).abs() < 1e-12);
    }

    #[test]
    fn rate_limit_info_keeps_newest_reset_per_window() {
        let mut r = Resets::default();
        rate_limit_resets(
            r#"{"x":{"rate_limit_info":{"status":"rejected","resetsAt":1000,"rateLimitType":"five_hour"}}}"#,
            &mut r,
        );
        rate_limit_resets(
            r#"{"rate_limit_info":{"resetsAt":2000,"rateLimitType":"five_hour"},"b":{"rate_limit_info":{"resetsAt":5,"rateLimitType":"seven_day"}}}"#,
            &mut r,
        );
        assert_eq!(r.five, Some(2_000_000));
        assert_eq!(r.seven, Some(5_000));
    }
}
