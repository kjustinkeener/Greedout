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
    downsample, iso_to_millis, last_component, model_info, Sample, Session,
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

/// Human model name for a Codex id, matching the labels Codex's own model picker
/// shows: the version keeps its full decimal and the internal codename is surfaced
/// rather than dropped. Returns `(label, version)`, rendered by the UI as
/// "label version" -- e.g. "gpt-6-astra" -> ("GPT-6","Astra") => "GPT-6 Astra",
/// "gpt-5.6-sol" -> ("GPT-5.6","Sol"), "gpt-5.5" -> ("GPT-5.5",""). (The generic
/// `parse_model_version` drops both the dotted minor and the codename, collapsing
/// terra/sol/luna to a bare "GPT".)
pub(crate) fn model_name(id: &str) -> (String, String) {
    let lower = id.to_ascii_lowercase();
    let rest = lower.strip_prefix("gpt-").unwrap_or(&lower);
    // "5.6-sol" -> ver "5.6", codename "sol"; "5.5" -> ver "5.5", codename "".
    let mut parts = rest.splitn(2, '-');
    let ver = parts.next().unwrap_or("");
    let codename = parts.next().unwrap_or("");
    let label = if ver.is_empty() { "GPT".to_string() } else { format!("GPT-{ver}") };
    let version = {
        let mut ch = codename.chars();
        match ch.next() {
            Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
            None => String::new(),
        }
    };
    (label, version)
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
    let (model, model_version) = t.model.as_deref().map(model_name).unwrap_or_default();
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
    // Set once we reach a `compacted` record newer than any usage record. The real
    // post-compact size isn't written until the next turn, and the record beyond it
    // is the stale pre-compact value (226k → 27k in practice). So leave ctx UNKNOWN
    // (None): the gauge renders a neutral "--" placeholder, neither a phantom-full
    // pre-compact bar nor a misleading 0%, and self-heals on the next turn. A
    // post-compact usage record nearer EOF is found first, so this only fires in the
    // brief just-compacted gap. Analog of scan.rs's Claude `compact_boundary` fix
    // (Claude has `postTokens`; Codex's `compacted` record carries no post count).
    let mut compacted = false;
    for line in lines.iter().rev() {
        if !compacted
            && out.ctx.is_none()
            && line.contains("\"compacted\"")
            && is_record_type(line, "compacted")
        {
            compacted = true;
        }
        // ctx and cumulative spend are DECOUPLED: a compaction leaves ctx unknown,
        // but `thread_token_usage` is money already spent and must survive it, so
        // keep reading the newest usage record for `thread` regardless. Otherwise
        // the just-compacted gap shows a phantom $0 spend on the live gauge (the
        // enrich path already tracks thread independently).
        if ((out.ctx.is_none() && !compacted) || out.thread.is_none())
            && line.contains("\"token_usage_record\"")
        {
            if let Some((ctx, thread)) = usage_from(line) {
                if out.ctx.is_none() && !compacted {
                    out.ctx = Some(ctx);
                }
                if out.thread.is_none() {
                    out.thread = Some(thread);
                }
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
        if (out.ctx.is_some() || compacted)
            && out.thread.is_some()
            && out.model.is_some()
            && out.cwd.is_some()
            && out.window.is_some()
        {
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
pub(crate) fn session_title(id: &str) -> Option<String> {
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

/// The session's working directory, read as cheaply as possible for the browse
/// Pass-1 project grouping: a growing backward tail that stops as soon as a `cwd`
/// is found (or the whole file has been read). Codex encodes no cwd in the rollout
/// filename, so unlike Claude's Pass-1 (which decodes the project dir name) this
/// costs a small tail read; the enrich pass later confirms it from a whole-file scan.
pub fn cwd_of(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut win = TAIL_BYTES;
    loop {
        let start = len.saturating_sub(win);
        file.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut buf).ok()?;
        if let Some(cwd) = scan_tail_buf(&buf, start > 0).cwd {
            return Some(cwd);
        }
        if start == 0 {
            return None;
        }
        win = win.saturating_mul(8);
    }
}

/// Whole-file browse-cache row for a Codex rollout: the Codex analog of
/// `scan::enrich_meta`, producing the SAME `EnrichMeta` struct in one pass.
///
/// Codex records are unique and append-only (no resume re-append the way Claude
/// transcripts get), so the per-turn Daily Spend rows carry `None` for msg_id and
/// need no dedupe key -- each `token_usage_record` is one billed turn. Context is
/// compaction-aware (a top-level `compacted` record newer than the last usage
/// resets it to 0, same rule as `scan_tail_buf`), and cumulative spend is priced
/// off the newest `thread_token_usage` to match the live gauge (see build_session).
pub fn enrich_meta(path: &Path) -> crate::scan::EnrichMeta {
    let mut out = crate::scan::EnrichMeta::default();
    let Ok(text) = std::fs::read_to_string(path) else { return out };

    // Resolve the session's model once (newest wins) so every per-turn row prices
    // consistently, mirroring history().
    let model = text.lines().rev().find_map(|l| {
        l.contains("\"model\"").then(|| payload_str(l, "model")).flatten()
    });

    // Walk forward so "last write wins" == newest for every single-valued field.
    let mut ctx: Option<u64> = None;
    let mut newest_thread: Option<Counts> = None;
    let mut window: Option<u64> = None;
    let mut cwd: Option<String> = None;
    let mut turn_count: u64 = 0;

    for line in text.lines() {
        let is_usage = line.contains("\"token_usage_record\"");
        let is_compacted = line.contains("\"compacted\"");
        let is_window = line.contains("model_context_window");
        let is_cwd = line.contains("\"cwd\"");
        if !(is_usage || is_compacted || is_window || is_cwd) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let rtype = v.get("type").and_then(Value::as_str);

        if is_usage && rtype == Some("token_usage_record") {
            if let Some(p) = v.get("payload") {
                if let Some(usage) = p.get("usage") {
                    turn_count += 1;
                    ctx = Some(usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0));
                    // Per-turn spend from THIS response's own usage, not the thread
                    // cumulative. msg_id None: Codex rows are unique, no dedupe key.
                    let cost = price(&counts_of(usage), model.as_deref());
                    if let Some(t) =
                        v.get("timestamp").and_then(Value::as_str).and_then(iso_to_millis)
                    {
                        out.turns.push((None, t, cost));
                    }
                    // Cumulative spend tracks the newest thread_token_usage (falls
                    // back to this response's usage when absent, as usage_from does).
                    newest_thread = Some(counts_of(p.get("thread_token_usage").unwrap_or(usage)));
                }
            }
        } else if is_compacted && rtype == Some("compacted") {
            // A compaction leaves ctx UNKNOWN until the next real usage record (see
            // scan_tail_buf). Forward walk = last write wins, so a following turn
            // overwrites this None with the real post-compact size; if the session
            // ends on the compaction, it stays None -> a neutral "--" placeholder,
            // not a misleading 0.
            ctx = None;
        }
        if is_window {
            if let Some(w) = window_from(line) {
                window = Some(w);
            }
        }
        if is_cwd {
            if let Some(c) = payload_str(line, "cwd") {
                cwd = Some(c);
            }
        }
    }

    out.ctx = ctx;
    out.cost_usd = newest_thread.map(|c| price(&c, model.as_deref())).unwrap_or(0.0);
    out.turn_count = turn_count;
    out.cwd = cwd;
    out.title = session_title(&id_from_path(path));
    // No Codex analog of Claude's `/context` breakdown.
    out.has_context_usage = false;
    if let Some(m) = &model {
        let mi = model_info(m);
        let (label, version) = model_name(m);
        out.model_label = label;
        out.model_version = version;
        out.target = mi.target;
        out.limit = window.unwrap_or(mi.context_max);
    } else {
        out.limit = window.unwrap_or(0);
    }
    out
}

/// Chat text of a Codex rollout for the browse search, in the same `ChatDoc` shape
/// as the Claude `read_chat_doc`. Pulls the PLAINTEXT conversation the desktop app
/// shows -- user + agent messages and reasoning SUMMARIES (the encrypted raw CoT is
/// never persisted in the clear, so it can't be searched) -- and skips tool i/o, to
/// match the chat-only scope of the Claude side. Whole file (no compaction cut):
/// search covers all history, not just the live window.
pub fn read_chat_doc(path: &Path, raw: &str) -> crate::browse::ChatDoc {
    let mut doc = crate::browse::ChatDoc {
        text: String::new(),
        title: session_title(&id_from_path(path)),
        cwd: None,
        first_user: None,
        first_ts: None,
        last_ts: None,
    };
    for line in raw.lines() {
        if doc.cwd.is_none() && line.contains("\"cwd\"") {
            if let Some(c) = payload_str(line, "cwd") {
                doc.cwd = Some(c);
            }
        }
        if !line.contains("\"item_completed\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let payload = v.get("payload");
        if payload.and_then(|p| p.get("type")).and_then(Value::as_str) != Some("item_completed") {
            continue;
        }
        let Some(item) = payload.and_then(|p| p.get("item")) else { continue };
        let text = match item.get("type").and_then(Value::as_str) {
            Some("UserMessage") => {
                let t = content_text(item.get("content"));
                if doc.first_user.is_none() && !t.trim().is_empty() {
                    doc.first_user = Some(t.clone());
                }
                t
            }
            Some("AgentMessage") => content_text(item.get("content")),
            Some("Reasoning") => string_array(item.get("summary_text")),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
            if doc.first_ts.is_none() {
                doc.first_ts = Some(ts.to_string());
            }
            doc.last_ts = Some(ts.to_string());
        }
        doc.text.push_str(&text);
        doc.text.push('\n');
    }
    doc
}

/// Concatenate the `text` of a Codex message `content` array. Same shape used by the
/// Context Explorer's Codex parse.
fn content_text(content: Option<&Value>) -> String {
    content
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Join a Codex string array (e.g. Reasoning `summary_text`), accepting bare strings
/// or `{text}` objects.
fn string_array(v: Option<&Value>) -> String {
    v.and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    e.as_str()
                        .map(str::to_string)
                        .or_else(|| e.get("text").and_then(Value::as_str).map(str::to_string))
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
