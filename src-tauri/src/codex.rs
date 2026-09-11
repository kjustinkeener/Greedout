//! Codex (OpenAI) session adapter.
//!
//! The Codex desktop app and CLI both write one append-only rollout transcript
//! per session as JSONL under `~/.codex`. Live sessions live in
//! `sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`; on close they move to a flat
//! `archived_sessions/`. Every record is `{"timestamp","type","payload":{…}}`,
//! so unlike Claude's transcripts the interesting fields sit one level down under
//! `payload`. We mirror scan.rs's cheap tail approach: stat every rollout, then
//! tail the newest and scan backward for the latest `token_usage_record` (context
//! + cumulative spend), model, and cwd. Titles come from `session_index.jsonl`.
//!
//! Token semantics differ from Claude and are handled here, not in scan.rs:
//!   * A `token_usage_record` carries three usage blocks -- `usage` (this
//!     response), `turn_token_usage` (turn so far) and `thread_token_usage`
//!     (whole session so far). Context = the newest response's `input_tokens`
//!     (which already INCLUDES `cached_input_tokens`); cumulative spend is priced
//!     straight off the newest `thread_token_usage`, no per-turn dedupe needed.

use crate::config::{codex_dir, Config};
use crate::scan::{
    downsample, iso_to_millis, last_component, model_info, parse_model_version, Sample, Session,
};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const TAIL_BYTES: u64 = 64 * 1024;

/// Every Codex rollout transcript on disk, as `(path, mtime_secs)`. Scans both the
/// dated `sessions/` tree (live) and the flat `archived_sessions/` (closed). When
/// a session id appears in both, the newer mtime wins so a just-archived session
/// doesn't show twice.
pub fn candidates() -> Vec<(PathBuf, u64)> {
    let base = codex_dir();
    // Forward slashes only: the glob crate treats `\` as an escape on Windows, so a
    // backslashed multi-wildcard pattern silently matches nothing (same trap as
    // scan.rs). Windows accepts forward slashes fine.
    let patterns = [
        base.join("sessions")
            .join("*").join("*").join("*")
            .join("rollout-*.jsonl"),
        base.join("archived_sessions").join("rollout-*.jsonl"),
    ];

    // Keep the newest mtime per session id (a session can be in both trees).
    let mut best: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for pat in patterns {
        let pat = pat.to_string_lossy().replace('\\', "/");
        let Ok(paths) = glob::glob(&pat) else { continue };
        for entry in paths.flatten() {
            let Ok(meta) = std::fs::metadata(&entry) else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let mtime = mtime.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let id = id_from_path(&entry);
            match best.get(&id) {
                Some((_, prev)) if *prev >= mtime => {}
                _ => {
                    best.insert(id, (entry, mtime));
                }
            }
        }
    }
    best.into_values().collect()
}

/// The session uuid encoded in a `rollout-<ts>-<uuid>.jsonl` filename. The uuid is
/// canonical (36 chars) and always the tail of the stem, so we slice it off the end
/// rather than trying to split the timestamp apart.
pub(crate) fn id_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if stem.len() >= 36 {
        stem[stem.len() - 36..].to_string()
    } else {
        stem
    }
}

/// One Codex usage block's raw counts (`input_tokens` already includes the cached
/// part). Priced later, once the session's model is known.
#[derive(Default, Clone, Copy)]
struct Counts {
    input: u64,
    cached: u64,
    cache_write: u64,
    output: u64,
}

/// What one backward tail-scan of a rollout yields.
#[derive(Default)]
struct Tailed {
    /// Newest response's `input_tokens` (context window occupancy).
    ctx: Option<u64>,
    /// Whole-session `thread_token_usage` counts, priced once the model is known.
    thread: Option<Counts>,
    model: Option<String>,
    cwd: Option<String>,
    /// The REAL per-session context window the desktop app enforces
    /// (`model_context_window`, e.g. 258400 = 272000 tier × 95% effective), read
    /// from the rollout itself rather than the model's API-ceiling `context_max`.
    window: Option<u64>,
}

/// Build one Codex session row for the poll list. Mirrors scan::build_session but
/// for Codex's on-disk shape; grouping/labels are applied by the caller.
pub fn build_session(path: &Path, mtime: u64, now: u64, cfg: &Config, focused: Option<&str>) -> Session {
    let id = id_from_path(path);
    let is_focused = focused == Some(id.as_str());
    let t = tail_scan(path).unwrap_or_default();
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let cwd = t.cwd.unwrap_or_default();
    let (project, project_path) = if cwd.is_empty() {
        (id.clone(), String::new())
    } else {
        (last_component(&cwd), cwd.clone())
    };

    let mi = t.model.as_deref().map(model_info);
    // Prefer the session's own enforced window over the model's API ceiling: the
    // desktop app caps context well below `context_max` (astra bills a 1.05M API
    // window but the app enforces 258400). If we found it, it's also the hard
    // `limit`, and the gauge's `target` sweet spot must sit at/under it.
    // One user-set target drives the budget across harnesses, clamped to the real
    // window so it can never exceed a session's enforced 258400 and never fill.
    let target0 = cfg.target_tokens;
    let max = mi.as_ref().map(|i| i.context_max).unwrap_or(cfg.target_tokens);
    let limit = t.window.unwrap_or(max);
    let target = t.window.map(|w| target0.min(w)).unwrap_or(target0);
    let model = mi.as_ref().map(|i| i.label.to_string()).unwrap_or_default();
    let model_version = t.model.as_deref().map(parse_model_version).unwrap_or_default();
    let cost_usd = t.thread.map(|c| price(&c, t.model.as_deref())).unwrap_or(0.0);

    let title = session_title(&id).unwrap_or_else(|| project.clone());
    let ctx = t.ctx;
    let pct = ctx.map(|c| c as f64 / target.max(1) as f64);
    let live = now.saturating_sub(mtime) as f64 <= (cfg.poll_seconds * 2.0).max(1.0);

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
        model,
        model_version,
        pct,
        live,
        mtime,
        size_bytes,
        cost_usd,
        focused: is_focused,
    }
}

/// Read a trailing window and scan backward for the newest `token_usage_record`
/// (context + cumulative spend), model, and cwd. Grows the window ×8 until it has
/// all three or has read from byte 0 -- the newest usage sits at EOF but the model
/// and cwd can lag a large tool-output burst, so a fixed 64KB tail isn't enough on
/// a busy turn.
fn tail_scan(path: &Path) -> Option<Tailed> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();

    let mut win = TAIL_BYTES;
    loop {
        let start = len.saturating_sub(win);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut buf).ok()?;
        let out = scan_tail_buf(&buf, start > 0);
        if (out.ctx.is_some() && out.model.is_some() && out.cwd.is_some()) || start == 0 {
            return Some(out);
        }
        win = win.saturating_mul(8);
    }
}

fn scan_tail_buf(buf: &[u8], partial_start: bool) -> Tailed {
    let text = String::from_utf8_lossy(buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if partial_start && !lines.is_empty() {
        lines.remove(0);
    }

    let mut out = Tailed::default();
    for line in lines.iter().rev() {
        // Compaction resets the window: scanning backward, if we reach a `compacted`
        // record before any `token_usage_record`, compaction is the newest context
        // event and the pre-compact record beyond it is stale (226k → 27k in
        // practice). Report 0 (empty) until the next turn writes a real usage record,
        // rather than showing a phantom-full gauge -- the analog of scan.rs's Claude
        // `compact_boundary` fix. A post-compact usage record nearer EOF is found
        // first, so this guard only fires in the brief just-compacted gap.
        if out.ctx.is_none()
            && line.contains("\"compacted\"")
            && is_record_type(line, "compacted")
        {
            out.ctx = Some(0);
        }
        if out.ctx.is_none() && line.contains("\"token_usage_record\"") {
            if let Some((ctx, thread)) = usage_from(line) {
                out.ctx = Some(ctx);
                out.thread = Some(thread);
            }
        }
        if out.model.is_none() && line.contains("\"model\"") {
            out.model = payload_str(line, "model");
        }
        if out.cwd.is_none() && line.contains("\"cwd\"") {
            out.cwd = payload_str(line, "cwd");
        }
        if out.window.is_none() && line.contains("model_context_window") {
            out.window = window_from(line);
        }
        if out.ctx.is_some() && out.model.is_some() && out.cwd.is_some() && out.window.is_some() {
            break;
        }
    }
    out
}

/// Cheap confirmation that a rollout line's top-level `type` equals `want` (the
/// `contains` pre-check keeps this parse off the hot path). Guards against matching
/// the word inside message text rather than a real record of that type.
fn is_record_type(line: &str, want: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("type").and_then(Value::as_str).map(|t| t == want))
        .unwrap_or(false)
}

/// The enforced per-session context window from a line that carries
/// `model_context_window` -- either an `event_msg/task_started` payload
/// (`payload.model_context_window`) or a `token_count` (`payload.info.
/// model_context_window`). Both report the same value; we accept whichever is present.
fn window_from(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    let p = v.get("payload")?;
    p.get("model_context_window")
        .or_else(|| p.get("info").and_then(|i| i.get("model_context_window")))
        .and_then(Value::as_u64)
        .filter(|w| *w > 0)
}

/// Context tokens and the whole-session `thread_token_usage` counts from one
/// `token_usage_record` line. Context is this response's `usage.input_tokens`; the
/// thread counts are priced later, once the session's model is resolved.
fn usage_from(line: &str) -> Option<(u64, Counts)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("token_usage_record") {
        return None;
    }
    let p = v.get("payload")?;
    let usage = p.get("usage")?;
    let ctx = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let thread = counts_of(p.get("thread_token_usage").unwrap_or(usage));
    Some((ctx, thread))
}

/// Read a Codex usage block's raw token counts.
fn counts_of(block: &Value) -> Counts {
    let get = |k: &str| block.get(k).and_then(Value::as_u64).unwrap_or(0);
    Counts {
        input: get("input_tokens"),
        cached: get("cached_input_tokens"),
        cache_write: get("cache_write_input_tokens"),
        output: get("output_tokens"),
    }
}

/// Price one Codex usage block against the session's model. Codex's `input_tokens`
/// already includes the cached part, so the non-cached remainder is billed at the
/// input rate and the cached part at the (cheaper) cache-read rate.
fn price(c: &Counts, model: Option<&str>) -> f64 {
    let mi = model_info(model.unwrap_or("gpt"));
    let non_cached = c.input.saturating_sub(c.cached) as f64;
    (non_cached * mi.price_in
        + c.cached as f64 * mi.price_cache_read
        + c.cache_write as f64 * mi.price_cache_write
        + c.output as f64 * mi.price_out)
        / 1_000_000.0
}

/// Pull a string field named `key` out of a rollout record's `payload` (or the top
/// level, as a fallback). Codex nests everything under `payload`, so `cwd`/`model`
/// live there, but we check both to stay robust to record shape drift.
fn payload_str(line: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get("payload")
        .and_then(|p| p.get(key))
        .or_else(|| v.get(key))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The Codex thread the user currently has open, including an IDLE one they just
/// clicked into without sending a turn. Codex writes no focus sidecar, and its
/// `state_5.sqlite` `recency_at_ms` (tried first, reverted) advances only on
/// activity -- so selecting a thread and sitting there was invisible. The one
/// place idle selection IS observable is the desktop app's own debug log: opening
/// or switching to a thread (no turn required) emits a line carrying
/// `ownerRoutePath=/local/<thread-uuid>`. We tail the newest such log and take the
/// last route id. Best-effort: any missing path / read error yields None = no pin,
/// same staleness tolerance as Claude's focus sidecar (a stale value just pins the
/// last thread the user looked at).
///
/// The log lives under the MSIX-virtualized LocalAppData, per-launch and
/// date-partitioned, so we glob for it rather than reading a fixed path:
/// `%LOCALAPPDATA%\Packages\OpenAI.Codex_*\LocalCache\Local\Codex\Logs\Y\M\D\*t0*.log`
/// (only the `t0` renderer log carries route events; t1/t2 are worker/utility).
/// Returns `(session id, focus timestamp in ms)`. The timestamp is the log file's
/// mtime, letting the caller compare Codex focus freshness against Claude's
/// `lastFocusedAt` to pick a single globally-focused session across harnesses.
pub fn focused_session_id() -> Option<(String, u64)> {
    let local = std::env::var_os("LOCALAPPDATA")?;
    // Forward slashes only (glob treats `\` as an escape on Windows -- same trap as
    // candidates()). `*t0*` picks the main renderer log across any launch/pid.
    let pat = PathBuf::from(local)
        .join("Packages").join("OpenAI.Codex_*")
        .join("LocalCache").join("Local").join("Codex").join("Logs")
        .join("*").join("*").join("*").join("*t0*.log")
        .to_string_lossy()
        .replace('\\', "/");
    let newest = glob::glob(&pat)
        .ok()?
        .flatten()
        .filter_map(|p| {
            let ms = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((p, ms))
        })
        .max_by_key(|(_, ms)| *ms)
        .map(|(p, _)| p)?;
    last_route(&newest)
}

/// The last `ownerRoutePath=/local/<uuid>` in a Codex desktop log = the most
/// recently focused thread, as `(id, selection time in ms)`. The time is parsed
/// from the SELECTION LINE's own leading ISO-Z timestamp, NOT the file mtime: the
/// log gets background writes with no thread change, so mtime is not a focus event
/// and would make Codex spuriously outrank Claude's `lastFocusedAt`. Reads only the
/// file's tail (route events cluster near EOF) and validates the 36-char uuid.
fn last_route(log: &Path) -> Option<(String, u64)> {
    const MARKER: &str = "ownerRoutePath=/local/";
    let mut f = std::fs::File::open(log).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(256 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let at = text.rfind(MARKER)?;
    let id: String = text[at + MARKER.len()..].chars().take(36).collect();
    if !is_uuid(&id) {
        return None;
    }
    // The line starts with `2026-09-10T20:35:22.530Z ` -- take the first token.
    let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let stamp = text[line_start..].split_whitespace().next()?;
    let ms = iso_to_millis(stamp)?.max(0) as u64;
    Some((id, ms))
}

/// Canonical 8-4-4-4-12 hex uuid check (dashes at 8/13/18/23, hex elsewhere).
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The session's title from `~/.codex/session_index.jsonl` (`id` -> `thread_name`).
/// The index is small and rewritten by the app; the last matching row wins.
fn session_title(id: &str) -> Option<String> {
    let text = std::fs::read_to_string(codex_dir().join("session_index.jsonl")).ok()?;
    let mut found = None;
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("id").and_then(Value::as_str) == Some(id) {
            if let Some(name) = v.get("thread_name").and_then(Value::as_str) {
                if !name.is_empty() {
                    found = Some(name.to_string());
                }
            }
        }
    }
    found
}

/// Full over-time curve for one Codex session: one point per `token_usage_record`,
/// its context (`usage.input_tokens`) and the running spend, timestamped from the
/// record's own ISO `timestamp`. A one-time whole-file read, downsampled like the
/// Claude path.
pub fn history(id: &str) -> Vec<Sample> {
    let Some((path, _)) = candidates().into_iter().find(|(p, _)| id_from_path(p) == id) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };

    // Resolve the session's model once so every per-turn row prices consistently.
    let model = text.lines().rev().find_map(|l| {
        l.contains("\"model\"").then(|| payload_str(l, "model")).flatten()
    });

    let mut rows: Vec<(i64, u64, f64)> = Vec::new();
    for line in text.lines() {
        if !line.contains("\"token_usage_record\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("token_usage_record") {
            continue;
        }
        let Some(p) = v.get("payload") else { continue };
        let Some(usage) = p.get("usage") else { continue };
        let ctx = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        // Per-turn spend from this response's own usage (running-summed below).
        let cost = price(&counts_of(usage), model.as_deref());
        let Some(t) = v.get("timestamp").and_then(Value::as_str).and_then(iso_to_millis) else {
            continue;
        };
        rows.push((t, ctx, cost));
    }
    rows.sort_by_key(|r| r.0);

    let mut out: Vec<Sample> = Vec::with_capacity(rows.len());
    let mut cost = 0.0;
    for (t, ctx, c) in rows {
        cost += c;
        out.push(Sample { t, ctx, cost });
    }
    downsample(out, 200)
}
