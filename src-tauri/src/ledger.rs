//! The conversation ledger: every Claude transcript, read once and kept in
//! `ledger.sqlite` as a hierarchy of tables, each level nested in the one above.
//!
//!   sessions   one transcript file; its scan position and rolled-up cost
//!   exchanges  a user prompt plus everything until the next prompt
//!   turns      one assistant message (its lines in the file share a message id)
//!
//! Turns keep the byte range of their lines, so reading a turn's contents later
//! means seeking into the file, not parsing it. Transcripts only ever grow, so a
//! scan resumes at each file's saved offset and adds only what is new.
//!
//! Resumed and compacted sessions copy old turns into a new file. Every copy keeps
//! its own row (so each session can list its whole file), but exactly one copy per
//! message id is `counted`, the one in the session with the lowest path. Costs and
//! totals sum counted turns only.
//!
//! Subagent transcripts are sessions too (their turns are real spend); linking a
//! subagent to the turn that spawned it is not stored yet.

use crate::config;
use crate::scan;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, UNIX_EPOCH};

/// Bump when the tables, the parsing or the price table in `scan::model_info`
/// change: a database from another version is dropped and rebuilt on next launch.
const VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    mtime INTEGER NOT NULL, len INTEGER NOT NULL,
    -- Bytes already read; always ends on a line boundary.
    offset INTEGER NOT NULL,
    -- Number of /compact boundaries read so far.
    segment INTEGER NOT NULL DEFAULT 0,
    reset5 INTEGER, reset7 INTEGER,
    first_ms INTEGER, last_ms INTEGER,
    cost REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE exchanges (
    id INTEGER PRIMARY KEY,
    session INTEGER NOT NULL,
    -- 1-based within its segment; numbering restarts after each /compact.
    no INTEGER NOT NULL, segment INTEGER NOT NULL,
    -- Byte offset of the prompt line.
    off INTEGER NOT NULL, ts INTEGER NOT NULL,
    cost REAL NOT NULL DEFAULT 0, n_turns INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_exchanges_session ON exchanges(session);
CREATE TABLE turns (
    id INTEGER PRIMARY KEY,
    session INTEGER NOT NULL,
    exchange INTEGER,
    msg_id TEXT,
    ts INTEGER NOT NULL, cost REAL NOT NULL, model TEXT,
    -- Byte range of the message's lines, end exclusive.
    off_start INTEGER NOT NULL, off_end INTEGER NOT NULL,
    counted INTEGER NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX idx_turns_session_msg ON turns(session, msg_id);
CREATE INDEX idx_turns_msg ON turns(msg_id);
CREATE INDEX idx_turns_exchange ON turns(exchange);
CREATE INDEX idx_turns_ts ON turns(ts);
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
        conn.execute_batch("DROP TABLE IF EXISTS turns; DROP TABLE IF EXISTS exchanges; DROP TABLE IF EXISTS sessions;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(&format!("PRAGMA user_version = {VERSION};"))?;
    }
    // Scratch lists for one sync: messages whose copies need re-ranking, and the
    // sessions whose totals need re-summing.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS touched (msg_id TEXT PRIMARY KEY);
         CREATE TEMP TABLE IF NOT EXISTS changed (id INTEGER PRIMARY KEY);",
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

/// What one transcript line contributes.
enum Ev {
    Compact,
    Prompt { off: u64, ts: i64 },
    Turn { id: Option<String>, ts: i64, cost: f64, model: Option<String>, start: u64, end: u64 },
}

/// Newest rate-limit reset seen per window, epoch ms.
#[derive(Default)]
struct Resets {
    five: Option<i64>,
    seven: Option<i64>,
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

/// Read the complete lines in `buf`, which starts at byte `base` of the file.
fn parse(buf: &[u8], base: u64) -> (Vec<Ev>, Resets) {
    let mut evs = Vec::new();
    let mut resets = Resets::default();
    let mut pos = base;
    for raw in buf.split(|b| *b == b'\n') {
        let start = pos;
        pos += raw.len() as u64 + 1;
        if raw.is_empty() {
            continue;
        }
        let line = String::from_utf8_lossy(raw);
        if line.contains("\"rate_limit_info\"") {
            rate_limit_resets(&line, &mut resets);
        }
        if line.contains("\"subtype\":\"compact_boundary\"") {
            evs.push(Ev::Compact);
            continue;
        }
        if line.contains("\"usage\"") && line.contains("\"assistant\"") {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some((id, _ctx, cost, model)) = scan::assistant_usage(&v) {
                    // A turn with no timestamp cannot be placed on a day.
                    if let Some(ts) = v.get("timestamp").and_then(Value::as_str).and_then(scan::iso_to_millis) {
                        evs.push(Ev::Turn { id, ts, cost, model, start, end: pos });
                    }
                    continue;
                }
            }
        }
        if scan::is_user_prompt(&line) {
            let ts = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|v| v.get("timestamp").and_then(Value::as_str).and_then(scan::iso_to_millis))
                .unwrap_or(0);
            evs.push(Ev::Prompt { off: start, ts });
        }
    }
    (evs, resets)
}

/// Write one file's new events under its session row. Returns the segment count
/// after them.
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
    // A message streamed over several lines upserts: later lines widen the range.
    let mut ins_turn = tx.prepare_cached(
        "INSERT INTO turns (session, exchange, msg_id, ts, cost, model, off_start, off_end)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(session, msg_id) DO UPDATE SET off_end = MAX(off_end, excluded.off_end)",
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
                ins_exchange.execute(params![sid, no, segment, *off as i64, ts])?;
                cur = Some(tx.last_insert_rowid());
            }
            Ev::Turn { id, ts, cost, model, start, end } => {
                ins_turn.execute(params![sid, cur, id, ts, cost, model, *start as i64, *end as i64])?;
                if let Some(id) = id {
                    touch.execute([id])?;
                }
            }
        }
    }
    Ok(segment)
}

/// Remove a session and everything under it. Its messages are queued for
/// re-ranking, since a copy elsewhere may now be the counted one.
fn delete_session(tx: &Connection, sid: i64) -> rusqlite::Result<()> {
    tx.execute("INSERT OR IGNORE INTO touched SELECT msg_id FROM turns WHERE session = ?1 AND msg_id IS NOT NULL", [sid])?;
    tx.execute("DELETE FROM turns WHERE session = ?1", [sid])?;
    tx.execute("DELETE FROM exchanges WHERE session = ?1", [sid])?;
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

/// Bring the ledger up to date with every transcript under `root`, reading only
/// what was appended since last time. Returns how many files changed.
fn sync_root(conn: &Connection, root: &Path) -> rusqlite::Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let pattern = root.join("**").join("*.jsonl").to_string_lossy().replace('\\', "/");
    let mut live: HashSet<String> = HashSet::new();
    let mut changed_files = 0;
    if let Ok(paths) = glob::glob(&pattern) {
        for p in paths.flatten() {
            let Ok(md) = std::fs::metadata(&p) else { continue };
            let (mtime, len) = (mtime_ms(&md), md.len());
            let path = p.to_string_lossy().to_string();
            live.insert(path.clone());
            let row: Option<(i64, i64, i64, i64, Option<i64>, Option<i64>)> = tx
                .query_row(
                    "SELECT id, mtime, len, offset, reset5, reset7 FROM sessions WHERE path = ?1",
                    [&path],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                )
                .optional()?;
            let (sid, from, r5, r7) = match row {
                Some((_, m, l, _, _, _)) if m == mtime && l as u64 == len => continue,
                Some((id, _, _, off, r5, r7)) if len >= off as u64 => (id, off as u64, r5, r7),
                other => {
                    // New, or shrunk (rewritten): start over.
                    if let Some((id, ..)) = other {
                        delete_session(&tx, id)?;
                    }
                    tx.execute(
                        "INSERT INTO sessions (path, mtime, len, offset) VALUES (?1, ?2, ?3, 0)",
                        params![path, mtime, len as i64],
                    )?;
                    (tx.last_insert_rowid(), 0, None, None)
                }
            };
            let Ok(mut f) = std::fs::File::open(&p) else { continue };
            if f.seek(SeekFrom::Start(from)).is_err() {
                continue;
            }
            let mut buf = Vec::new();
            if f.read_to_end(&mut buf).is_err() {
                continue;
            }
            // Stop at the last full line: a transcript still being written ends mid-line.
            let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            let (evs, resets) = parse(&buf[..end], from);
            let segment = store(&tx, sid, &evs)?;
            tx.execute(
                "UPDATE sessions SET mtime = ?2, len = ?3, offset = ?4, segment = ?5, reset5 = ?6, reset7 = ?7 WHERE id = ?1",
                params![sid, mtime, len as i64, (from + end as u64) as i64, segment, r5.max(resets.five), r7.max(resets.seven)],
            )?;
            tx.execute("INSERT OR IGNORE INTO changed (id) VALUES (?1)", [sid])?;
            changed_files += 1;
        }
    }

    // Transcripts that no longer exist.
    let gone: Vec<i64> = {
        let mut st = tx.prepare("SELECT id, path FROM sessions")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.flatten().filter(|(_, p)| !live.contains(p)).map(|(id, _)| id).collect()
    };
    for id in gone {
        delete_session(&tx, id)?;
    }

    settle(&tx)?;
    tx.commit()?;
    Ok(changed_files)
}

/// Re-rank copies of the messages that moved, then re-sum every session and
/// exchange that a changed file or a re-ranked copy touches.
fn settle(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute_batch(
        "UPDATE turns SET counted = CASE WHEN session = (
             SELECT t2.session FROM turns t2 JOIN sessions s2 ON s2.id = t2.session
             WHERE t2.msg_id = turns.msg_id ORDER BY s2.path LIMIT 1
         ) THEN 1 ELSE 0 END
         WHERE msg_id IN (SELECT msg_id FROM touched);

         INSERT OR IGNORE INTO changed (id)
             SELECT DISTINCT session FROM turns WHERE msg_id IN (SELECT msg_id FROM touched);

         UPDATE sessions SET
             cost = COALESCE((SELECT SUM(cost) FROM turns WHERE session = sessions.id AND counted = 1), 0),
             n_turns = (SELECT COUNT(*) FROM turns WHERE session = sessions.id AND counted = 1),
             first_ms = (SELECT MIN(ts) FROM turns WHERE session = sessions.id),
             last_ms = (SELECT MAX(ts) FROM turns WHERE session = sessions.id)
         WHERE id IN (SELECT id FROM changed);

         UPDATE exchanges SET
             cost = COALESCE((SELECT SUM(cost) FROM turns WHERE exchange = exchanges.id AND counted = 1), 0),
             n_turns = (SELECT COUNT(*) FROM turns WHERE exchange = exchanges.id AND counted = 1)
         WHERE session IN (SELECT id FROM changed);

         DELETE FROM touched;
         DELETE FROM changed;",
    )
}

/// The counted turns since `since` (epoch ms) as `(time, cost)`, oldest first, and
/// the newest 5-hour and weekly reset instants any transcript has recorded, after
/// bringing the ledger up to date.
pub fn recent(since: i64) -> Option<(Vec<(i64, f64)>, Option<i64>, Option<i64>)> {
    with_db(|conn| {
        let started = Instant::now();
        let files = sync_root(conn, &config::claude_dir().join("projects"))?;
        config::debug_log(
            &config::load_config(),
            &format!("ledger sync: {files} files changed in {:?}", started.elapsed()),
        );
        if files > 100 {
            // A first scan leaves a log as big as the database; fold it back in.
            let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        }
        let mut st = conn.prepare("SELECT ts, cost FROM turns WHERE counted = 1 AND ts >= ?1 ORDER BY ts")?;
        let events = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let (r5, r7) = conn.query_row("SELECT MAX(reset5), MAX(reset7) FROM sessions", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok((events, r5, r7))
    })
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

    fn q<T: rusqlite::types::FromSql>(conn: &Connection, sql: &str) -> T {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn growing_file_adds_only_new_turns_and_joins_split_messages() {
        let root = scratch("grow");
        let a = root.join("a.jsonl");
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:02.000Z"));
        let conn = db();
        assert_eq!(sync_root(&conn, &root).unwrap(), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 1);
        let end1: i64 = q(&conn, "SELECT off_end FROM turns");
        assert_eq!(end1, fs::metadata(&a).unwrap().len() as i64);

        // Unchanged file: nothing to do.
        assert_eq!(sync_root(&conn, &root).unwrap(), 0);

        // The same message gains another line later; a new prompt and turn follow.
        append(&a, &turn("m1", "2026-10-01T00:00:03.000Z"));
        append(&a, &prompt("2026-10-01T00:01:00.000Z"));
        append(&a, &turn("m2", "2026-10-01T00:01:01.000Z"));
        assert_eq!(sync_root(&conn, &root).unwrap(), 1);
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
    fn only_one_copy_of_a_message_counts_and_survives_the_original_going_away() {
        let root = scratch("dup");
        let (a, b) = (root.join("a.jsonl"), root.join("b.jsonl"));
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        // A resumed session copies the old turn, then adds its own.
        append(&b, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&b, &prompt("2026-10-02T00:00:00.000Z"));
        append(&b, &turn("m2", "2026-10-02T00:00:01.000Z"));
        let conn = db();
        sync_root(&conn, &root).unwrap();
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 3);
        assert_eq!(q::<i64>(&conn, "SELECT SUM(counted) FROM turns"), 2);
        // The lowest path owns the shared message; the copy still lists in b.
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions WHERE path LIKE '%a.jsonl'"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions WHERE path LIKE '%b.jsonl'"), 1);

        fs::remove_file(&a).unwrap();
        sync_root(&conn, &root).unwrap();
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM sessions"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT SUM(counted) FROM turns"), 2);
        assert_eq!(q::<i64>(&conn, "SELECT n_turns FROM sessions"), 2);
    }

    #[test]
    fn compaction_restarts_exchange_numbering_and_a_rewritten_file_is_re_read() {
        let root = scratch("compact");
        let a = root.join("a.jsonl");
        append(&a, &prompt("2026-10-01T00:00:00.000Z"));
        append(&a, &turn("m1", "2026-10-01T00:00:01.000Z"));
        append(&a, "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"timestamp\":\"2026-10-01T00:00:02.000Z\"}\n");
        append(&a, &prompt("2026-10-01T00:01:00.000Z"));
        append(&a, &turn("m2", "2026-10-01T00:01:01.000Z"));
        let conn = db();
        sync_root(&conn, &root).unwrap();
        assert_eq!(q::<i64>(&conn, "SELECT segment FROM sessions"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT no FROM exchanges WHERE segment = 1"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM exchanges"), 2);

        // Rewritten shorter: the old rows are replaced, not doubled.
        fs::write(&a, turn("m9", "2026-10-03T00:00:00.000Z")).unwrap();
        sync_root(&conn, &root).unwrap();
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM turns"), 1);
        assert_eq!(q::<i64>(&conn, "SELECT COUNT(*) FROM exchanges"), 0);
        assert_eq!(q::<i64>(&conn, "SELECT segment FROM sessions"), 0);
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
