//! Plan-limit dials: how much of the 5-hour and weekly allowance is used, against
//! how far through each window we are.
//!
//! Transcripts never record the real percentage, so usage is estimated: spend
//! inside the window divided by a dollar budget. The budget is learned from the
//! user's own `/usage` reports (copied, then read by clicking the dials): each
//! report pairs a real percentage with the local spend at that moment, and the
//! budget is fitted across the saved pairs. Until one exists the dials show
//! elapsed time only.
//!
//! Per-turn costs of every recent transcript are kept in `limits.sqlite`, so a
//! launch re-reads only files that changed since the last run instead of the whole
//! week of transcripts.

use crate::config;
use crate::scan;
use serde::{Deserialize, Serialize};
use rusqlite::{params, Connection};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const H5_MS: i64 = 5 * 3600 * 1000;
const D7_MS: i64 = 7 * 24 * 3600 * 1000;
/// Saved calibration pairs per window, and how long one stays useful.
const KEEP_PAIRS: usize = 10;
const PAIR_TTL_MS: i64 = 30 * 24 * 3600 * 1000;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    /// Estimated share used: window spend / budget, 0..1+. None until calibrated.
    pub pct: Option<f64>,
    /// Spend inside the window, USD.
    pub cost: f64,
    /// The budget the estimate divides by, USD; 0 until calibrated.
    pub budget: f64,
    /// Fraction of the window elapsed, 0..1; None when the window start is unknown.
    pub elapsed: Option<f64>,
    /// Window end, epoch ms.
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub five_hour: LimitWindow,
    pub weekly: LimitWindow,
}

/// One `/usage` reading: the real share used, and local spend over the same window
/// up to the moment the report was made.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pair {
    cost: f64,
    pct: f64,
    at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Calib {
    five: Vec<Pair>,
    week: Vec<Pair>,
    /// Latest reset instants read off a report, epoch ms. The weekly one is a fixed
    /// anchor rolled forward in whole weeks; the 5-hour one pins the live window.
    five_reset: i64,
    week_reset: i64,
}

fn calib_path() -> PathBuf {
    config::app_dir().join("limit_calibration.json")
}

fn load_calib() -> Calib {
    std::fs::read_to_string(calib_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save_calib(c: &Calib) -> Result<(), String> {
    let _ = std::fs::create_dir_all(config::app_dir());
    let text = serde_json::to_string_pretty(c).map_err(|e| e.to_string())?;
    std::fs::write(calib_path(), text).map_err(|e| format!("could not save calibration: {e}"))
}

/// Least-squares budget through the origin (cost = budget * pct), so readings at a
/// high percentage, where rounding matters least, count most. 0 when there is none.
fn fit_budget(pairs: &[Pair]) -> f64 {
    let (num, den) = pairs.iter().fold((0.0, 0.0), |(n, d), p| (n + p.cost * p.pct, d + p.pct * p.pct));
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

fn db_path() -> PathBuf {
    config::app_dir().join("limits.sqlite")
}

/// The per-turn store. Its own file, apart from the opt-in browse cache: that one
/// is deleted by "Clear cache" and its existence means "the index is built".
fn open_db() -> Option<Connection> {
    let _ = std::fs::create_dir_all(config::app_dir());
    let conn = Connection::open(db_path()).ok()?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").ok()?;
    conn.execute_batch(SCHEMA).ok()?;
    Some(conn)
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS files (
        path TEXT PRIMARY KEY, mtime INTEGER NOT NULL, len INTEGER NOT NULL,
        offset INTEGER NOT NULL, reset5 INTEGER, reset7 INTEGER
     );
     CREATE TABLE IF NOT EXISTS turns (
        path TEXT NOT NULL, msg_id TEXT, ts INTEGER NOT NULL, cost REAL NOT NULL
     );
     CREATE INDEX IF NOT EXISTS idx_turns_path ON turns(path);";

/// Everything stored from earlier runs, so the first scan only has to look at
/// files whose size or mtime moved.
fn load_cache(conn: &Connection) -> HashMap<PathBuf, Cached> {
    let mut out: HashMap<PathBuf, Cached> = HashMap::new();
    let files = conn.prepare("SELECT path, mtime, len, offset, reset5, reset7 FROM files").and_then(|mut st| {
        st.query_map([], |r| {
            let data = FileData { turns: Vec::new(), reset_5h: r.get(4)?, reset_7d: r.get(5)? };
            Ok((PathBuf::from(r.get::<_, String>(0)?), Cached { mtime: r.get(1)?, len: r.get::<_, i64>(2)? as u64, offset: r.get::<_, i64>(3)? as u64, data }))
        })
        .map(|rows| rows.flatten().collect::<Vec<_>>())
    });
    out.extend(files.unwrap_or_default());
    // rowid order keeps each file's turns in the order they were appended, which
    // `save_file` relies on to know how many are already stored.
    let turns = conn.prepare("SELECT path, msg_id, ts, cost FROM turns ORDER BY rowid").and_then(|mut st| {
        st.query_map([], |r| Ok((PathBuf::from(r.get::<_, String>(0)?), (r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)?, r.get::<_, f64>(3)?))))
            .map(|rows| rows.flatten().collect::<Vec<_>>())
    });
    for (p, t) in turns.unwrap_or_default() {
        if let Some(c) = out.get_mut(&p) {
            c.data.turns.push(t);
        }
    }
    out
}

/// Store a file's new state. `kept` is how many of its turns were already stored
/// (None = the file was re-read from the top, so its stored turns are replaced).
fn save_file(tx: &Connection, p: &Path, c: &Cached, kept: Option<usize>) {
    let path = p.to_string_lossy();
    let from = kept.unwrap_or_else(|| {
        let _ = tx.execute("DELETE FROM turns WHERE path = ?1", params![path]);
        0
    });
    for (id, t, cost) in &c.data.turns[from..] {
        let _ = tx.execute("INSERT INTO turns (path, msg_id, ts, cost) VALUES (?1, ?2, ?3, ?4)", params![path, id, t, cost]);
    }
    let _ = tx.execute(
        "INSERT OR REPLACE INTO files (path, mtime, len, offset, reset5, reset7) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![path, c.mtime, c.len as i64, c.offset as i64, c.data.reset_5h, c.data.reset_7d],
    );
}

#[derive(Clone, Default)]
struct FileData {
    turns: Vec<(Option<String>, i64, f64)>,
    reset_5h: Option<i64>,
    reset_7d: Option<i64>,
}

struct Cached {
    mtime: i64,
    len: u64,
    /// Bytes already parsed (always ends on a line boundary), so a transcript that
    /// is still being appended to is read from here, not from the top, each cycle.
    offset: u64,
    data: FileData,
}

static FILES: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);
static LATEST: Mutex<Option<Limits>> = Mutex::new(None);

pub fn latest() -> Limits {
    LATEST.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Pull every `"rate_limit_info":{...}` object out of a line and keep the newest
/// reset per window type. Those records appear only when a limit is hit or
/// approached, but when present they pin the real window end.
fn rate_limit_resets(line: &str, out: &mut FileData) {
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
            "five_hour" => &mut out.reset_5h,
            "seven_day" => &mut out.reset_7d,
            _ => continue,
        };
        let ms = at * 1000;
        if slot.map_or(true, |p| ms > p) {
            *slot = Some(ms);
        }
    }
}

fn parse_into(d: &mut FileData, text: &str) {
    for line in text.lines() {
        if line.contains("\"rate_limit_info\"") {
            rate_limit_resets(line, d);
        }
        if line.contains("\"usage\"") && line.contains("\"assistant\"") {
            if let Some((id, t, _ctx, cost)) = scan::history_row(line) {
                d.turns.push((id, t, cost));
            }
        }
    }
}

/// Bring one file's cache entry up to date, parsing only the complete lines added
/// since last time (the whole file if it is new or shrank).
fn refresh(p: &Path, old: Option<Cached>, mtime: i64, len: u64) -> Option<Cached> {
    let (mut data, from) = match old {
        Some(c) if len >= c.offset => (c.data, c.offset),
        _ => (FileData::default(), 0),
    };
    let mut f = std::fs::File::open(p).ok()?;
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let end = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    parse_into(&mut data, &String::from_utf8_lossy(&buf[..end]));
    Some(Cached { mtime, len, offset: from + end as u64, data })
}

/// Every transcript touched in the last 8 days (the weekly window plus slack),
/// parsed once and re-parsed only when its size or mtime changes.
fn gather(now: i64) -> (Vec<(i64, f64)>, Option<i64>, Option<i64>) {
    let root = config::claude_dir().join("projects");
    let pattern = root.join("**").join("*.jsonl").to_string_lossy().replace('\\', "/");
    let cutoff = now - D7_MS - 24 * 3600 * 1000;
    let db = open_db();
    let mut guard = FILES.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(|| db.as_ref().map(load_cache).unwrap_or_default());
    let tx = db.as_ref().and_then(|c| c.unchecked_transaction().ok());
    let mut live: HashSet<PathBuf> = HashSet::new();
    if let Ok(paths) = glob::glob(&pattern) {
        for p in paths.flatten() {
            let Ok(md) = std::fs::metadata(&p) else { continue };
            let mtime = md
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            if mtime < cutoff {
                continue;
            }
            live.insert(p.clone());
            let fresh = cache.get(&p).is_some_and(|c| c.mtime == mtime && c.len == md.len());
            if !fresh {
                let old = cache.remove(&p);
                let kept = old.as_ref().filter(|c| md.len() >= c.offset).map(|c| c.data.turns.len());
                if let Some(c) = refresh(&p, old, mtime, md.len()) {
                    if let Some(tx) = &tx {
                        save_file(tx, &p, &c, kept);
                    }
                    cache.insert(p, c);
                }
            }
        }
    }
    let gone: Vec<PathBuf> = cache.keys().filter(|p| !live.contains(*p)).cloned().collect();
    for p in &gone {
        cache.remove(p);
        if let Some(tx) = &tx {
            let path = p.to_string_lossy();
            let _ = tx.execute("DELETE FROM turns WHERE path = ?1", params![path]);
            let _ = tx.execute("DELETE FROM files WHERE path = ?1", params![path]);
        }
    }
    if let Some(tx) = tx {
        let _ = tx.commit();
    }

    // Resumed and compacted transcripts re-append whole turns, so dedupe by message
    // id across every file, the same rule the Daily Spend totals use.
    let mut seen: HashSet<&str> = HashSet::new();
    let mut events: Vec<(i64, f64)> = Vec::new();
    let (mut r5, mut r7): (Option<i64>, Option<i64>) = (None, None);
    for c in cache.values() {
        for (id, t, cost) in &c.data.turns {
            if *t < cutoff {
                continue;
            }
            if let Some(id) = id {
                if !seen.insert(id.as_str()) {
                    continue;
                }
            }
            events.push((*t, *cost));
        }
        r5 = r5.max(c.data.reset_5h);
        r7 = r7.max(c.data.reset_7d);
    }
    events.sort_by_key(|e| e.0);
    (events, r5, r7)
}

fn window(cost: f64, budget: f64, end: Option<i64>, len: i64, now: i64) -> LimitWindow {
    LimitWindow {
        pct: (budget > 0.0).then(|| cost / budget),
        cost,
        budget,
        elapsed: end.map(|e| ((now - (e - len)) as f64 / len as f64).clamp(0.0, 1.0)),
        resets_at: end,
    }
}

type Gathered = (Vec<(i64, f64)>, Option<i64>, Option<i64>);

/// Estimate both windows from local transcripts and the saved calibration.
fn estimate(now: i64) -> Limits {
    estimate_from(&gather(now), &load_calib(), now)
}

fn estimate_from(gathered: &Gathered, calib: &Calib, now: i64) -> Limits {
    let (events, r5, r7) = (&gathered.0, gathered.1, gathered.2);

    // 5-hour window: it opens with the first message after the previous one closed
    // and lasts five hours. A reset in the future, from a `/usage` report or a
    // recorded limit event, pins the end exactly.
    let mut start: Option<i64> = None;
    for (t, _) in events {
        if start.map_or(true, |s| *t >= s + H5_MS) {
            start = Some(*t);
        }
    }
    let mut end5 = start.map(|s| s + H5_MS).filter(|e| *e > now);
    if let Some(r) = [r5, Some(calib.five_reset)].into_iter().flatten().filter(|r| *r > now).max() {
        end5 = Some(r);
    }
    let cost5: f64 = end5
        .map(|e| events.iter().filter(|(t, _)| *t >= e - H5_MS && *t < e).map(|(_, c)| c).sum())
        .unwrap_or(0.0);

    // Weekly window: the reset is a fixed weekly anchor, rolled forward in whole
    // weeks. Without one the spend is a plain trailing 7 days and progress is unknown.
    let anchor7 = if calib.week_reset > 0 { Some(calib.week_reset) } else { r7 };
    let end7 = anchor7.map(|r| {
        let mut e = r;
        while e <= now {
            e += D7_MS;
        }
        while e - D7_MS > now {
            e -= D7_MS;
        }
        e
    });
    let lo = end7.map(|e| e - D7_MS).unwrap_or(now - D7_MS);
    let cost7: f64 = events.iter().filter(|(t, _)| *t >= lo).map(|(_, c)| c).sum();

    Limits {
        five_hour: window(cost5, fit_budget(&calib.five), end5, H5_MS, now),
        weekly: window(cost7, fit_budget(&calib.week), end7, D7_MS, now),
    }
}

/// What a `/usage` report says about one limit: share used (0..1) and window end.
struct Reading {
    pct: f64,
    reset: i64,
}

struct Report {
    at: Option<i64>,
    five: Option<Reading>,
    week: Option<Reading>,
}

/// Read the "Plan limits" lines of a copied `/usage` report:
/// `- session-0: 25% (resets 2026-11-01T06:19:59.632633+00:00)` for the 5-hour
/// limit and `- weekly_all-1: ...` for the weekly one. Other limit lines (per-model
/// weekly caps) are ignored.
fn parse_report(text: &str) -> Option<Report> {
    let at = text
        .lines()
        .find(|l| l.contains("usage report"))
        .and_then(|l| l.split_once('(').map(|(_, r)| r.trim_end_matches(')').trim()))
        .and_then(scan::iso_to_millis);
    let (mut five, mut week) = (None, None);
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("- ") else { continue };
        let Some((name, rest)) = rest.split_once(':') else { continue };
        let slot = if name.starts_with("session") {
            &mut five
        } else if name.starts_with("weekly_all") {
            &mut week
        } else {
            continue;
        };
        let Some((pct, rest)) = rest.trim().split_once('%') else { continue };
        let Ok(pct) = pct.trim().parse::<f64>() else { continue };
        let Some(reset) = rest
            .split_once("resets")
            .map(|(_, r)| r.trim().trim_end_matches(')').trim())
            .and_then(scan::iso_to_millis)
        else {
            continue;
        };
        *slot = Some(Reading { pct: pct / 100.0, reset });
    }
    (five.is_some() || week.is_some()).then_some(Report { at, five, week })
}

/// Fold a pasted `/usage` report into the saved calibration.
fn calibrate(text: &str, gathered: &Gathered, now: i64) -> Result<Calib, String> {
    let report = parse_report(text).ok_or("not a /usage report")?;
    let at = report.at.unwrap_or(now);
    if now - at > D7_MS {
        return Err("that /usage report is too old".into());
    }
    let events = &gathered.0;
    let spent = |end: i64, len: i64| -> f64 {
        events.iter().filter(|(t, _)| *t >= end - len && *t < end && *t <= at).map(|(_, c)| c).sum()
    };
    let mut calib = load_calib();
    if let Some(r) = &report.five {
        calib.five_reset = r.reset;
        if r.pct > 0.0 {
            calib.five.push(Pair { cost: spent(r.reset, H5_MS), pct: r.pct, at });
        }
    }
    if let Some(r) = &report.week {
        calib.week_reset = r.reset;
        if r.pct > 0.0 {
            calib.week.push(Pair { cost: spent(r.reset, D7_MS), pct: r.pct, at });
        }
    }
    for pairs in [&mut calib.five, &mut calib.week] {
        pairs.retain(|p| now - p.at < PAIR_TTL_MS);
        // Pasting the same report again replaces its reading rather than adding a copy.
        pairs.reverse();
        let mut seen = HashSet::new();
        pairs.retain(|p| seen.insert(p.at));
        pairs.reverse();
        let extra = pairs.len().saturating_sub(KEEP_PAIRS);
        pairs.drain(..extra);
    }
    save_calib(&calib)?;
    Ok(calib)
}

fn publish(app: &tauri::AppHandle, out: &Limits) {
    use tauri::Emitter;
    if let Ok(mut g) = LATEST.lock() {
        *g = Some(out.clone());
    }
    let _ = app.emit("limits", out);
}

/// Read the clipboard once, on the user's click, and calibrate from it.
pub fn calibrate_from_clipboard(app: &tauri::AppHandle) -> Result<Limits, String> {
    let text = arboard::Clipboard::new()
        .and_then(|mut c| c.get_text())
        .map_err(|_| "could not read the clipboard".to_string())?;
    let now = now_ms();
    let started = std::time::Instant::now();
    let gathered = gather(now);
    let scanned = started.elapsed();
    let calib = calibrate(&text, &gathered, now)?;
    let out = estimate_from(&gathered, &calib, now);
    config::debug_log(&config::load_config(), &format!("limits calibrate: scan {scanned:?}, total {:?}", started.elapsed()));
    publish(app, &out);
    Ok(out)
}

/// Background loop: refresh the estimate (cheap, cached per file), publishing each
/// result as the "limits" event.
pub fn spawn(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        let out = estimate(now_ms());
        config::debug_log(
            &config::load_config(),
            &format!(
                "limits 5h pct={:?} cost={:.2} budget={:.2} end={:?} el={:?} | 7d pct={:?} cost={:.2} budget={:.2} end={:?} el={:?}",
                out.five_hour.pct,
                out.five_hour.cost,
                out.five_hour.budget,
                out.five_hour.resets_at,
                out.five_hour.elapsed,
                out.weekly.pct,
                out.weekly.cost,
                out.weekly.budget,
                out.weekly.resets_at,
                out.weekly.elapsed
            ),
        );
        publish(&app, &out);
        std::thread::sleep(Duration::from_secs(20));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "Claude Code usage report (2026-11-01T04:37:03.116Z)\nClient: x\n\nPlan limits:\n- session-0: 25% (resets 2026-11-01T06:19:59.632633+00:00)\n- weekly_all-1: 39% (resets 2026-11-05T17:59:59.632650+00:00)\n- weekly_fable-2: 5% (resets 2026-11-05T17:59:59+00:00)\n";

    #[test]
    fn rate_limit_info_keeps_newest_reset_per_window() {
        let mut d = FileData::default();
        rate_limit_resets(
            r#"{"x":{"rate_limit_info":{"status":"rejected","resetsAt":1000,"rateLimitType":"five_hour"}}}"#,
            &mut d,
        );
        rate_limit_resets(
            r#"{"rate_limit_info":{"resetsAt":2000,"rateLimitType":"five_hour"},"b":{"rate_limit_info":{"resetsAt":5,"rateLimitType":"seven_day"}}}"#,
            &mut d,
        );
        assert_eq!(d.reset_5h, Some(2_000_000));
        assert_eq!(d.reset_7d, Some(5_000));
    }

    #[test]
    fn stored_turns_round_trip_and_append() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let p = Path::new("a.jsonl");
        let turn = |id: &str, t, c| (Some(id.to_string()), t, c);
        let mut c = Cached {
            mtime: 1,
            len: 10,
            offset: 10,
            data: FileData { turns: vec![turn("m1", 100, 0.5)], reset_5h: Some(7), reset_7d: None },
        };
        save_file(&conn, p, &c, None);
        // The file grew: only the new turn is appended.
        c.data.turns.push(turn("m2", 200, 0.25));
        c.len = 20;
        c.offset = 20;
        save_file(&conn, p, &c, Some(1));
        let loaded = load_cache(&conn);
        let got = &loaded[p];
        assert_eq!((got.len, got.offset, got.data.reset_5h), (20, 20, Some(7)));
        assert_eq!(got.data.turns, c.data.turns);
        // Re-read from the top replaces the stored turns instead of doubling them.
        save_file(&conn, p, &c, None);
        assert_eq!(load_cache(&conn)[p].data.turns.len(), 2);
    }

    #[test]
    fn report_reads_both_limits_and_ignores_the_rest() {
        let r = parse_report(REPORT).unwrap();
        assert_eq!(r.at, scan::iso_to_millis("2026-11-01T04:37:03.116Z"));
        let (f, w) = (r.five.unwrap(), r.week.unwrap());
        assert!((f.pct - 0.25).abs() < 1e-9 && (w.pct - 0.39).abs() < 1e-9);
        assert_eq!(f.reset, scan::iso_to_millis("2026-11-01T06:19:59.632Z").unwrap());
        assert!(parse_report("hello").is_none());
    }

    #[test]
    fn budget_fit_weights_high_percentages() {
        assert_eq!(fit_budget(&[]), 0.0);
        let p = |cost, pct| Pair { cost, pct, at: 0 };
        assert!((fit_budget(&[p(12.5, 0.25)]) - 50.0).abs() < 1e-9);
        // A noisy low reading barely moves a clean high one.
        let b = fit_budget(&[p(50.0, 1.0), p(15.0, 0.25)]);
        assert!((b - 50.0).abs() < 2.0, "{b}");
    }
}
