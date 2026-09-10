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
fn id_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if stem.len() >= 36 {
        stem[stem.len() - 36..].to_string()
    } else {
        stem
    }
}

/// What one backward tail-scan of a rollout yields.
#[derive(Default)]
struct Tailed {
    /// Newest response's `input_tokens` (context window occupancy).
    ctx: Option<u64>,
    /// Cumulative session spend, priced off the newest `thread_token_usage`.
    cost: Option<f64>,
    model: Option<String>,
    cwd: Option<String>,
}

/// Build one Codex session row for the poll list. Mirrors scan::build_session but
/// for Codex's on-disk shape; grouping/labels are applied by the caller.
pub fn build_session(path: &Path, mtime: u64, now: u64, cfg: &Config) -> Session {
    let id = id_from_path(path);
    let t = tail_scan(path).unwrap_or_default();
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let cwd = t.cwd.unwrap_or_default();
    let (project, project_path) = if cwd.is_empty() {
        (id.clone(), String::new())
    } else {
        (last_component(&cwd), cwd.clone())
    };

    let mi = t.model.as_deref().map(model_info);
    let target = mi.as_ref().map(|i| i.target).unwrap_or(cfg.target_tokens);
    let limit = mi.as_ref().map(|i| i.context_max).unwrap_or(cfg.target_tokens);
    let model = mi.as_ref().map(|i| i.label.to_string()).unwrap_or_default();
    let model_version = t.model.as_deref().map(parse_model_version).unwrap_or_default();

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
        cost_usd: t.cost.unwrap_or(0.0),
        focused: false,
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
        if out.ctx.is_none() && line.contains("\"token_usage_record\"") {
            if let Some((ctx, cost)) = usage_from(line) {
                out.ctx = Some(ctx);
                out.cost = Some(cost);
            }
        }
        if out.model.is_none() && line.contains("\"model\"") {
            out.model = payload_str(line, "model");
        }
        if out.cwd.is_none() && line.contains("\"cwd\"") {
            out.cwd = payload_str(line, "cwd");
        }
        if out.ctx.is_some() && out.model.is_some() && out.cwd.is_some() {
            break;
        }
    }
    out
}

/// Context tokens and cumulative spend from one `token_usage_record` line. Context
/// is this response's `usage.input_tokens`; spend prices the whole-session
/// `thread_token_usage` (Codex's `input_tokens` already includes the cached part,
/// so the non-cached remainder is billed at the input rate and the cached part at
/// the cache-read rate).
fn usage_from(line: &str) -> Option<(u64, f64)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("token_usage_record") {
        return None;
    }
    let p = v.get("payload")?;
    let get = |block: &Value, k: &str| block.get(k).and_then(Value::as_u64).unwrap_or(0);

    let usage = p.get("usage")?;
    let ctx = get(usage, "input_tokens");

    let thread = p.get("thread_token_usage").unwrap_or(usage);
    let cost = price(thread);
    Some((ctx, cost))
}

/// Price one Codex usage block. Model is looked up lazily -- the block itself
/// doesn't name it -- so callers price with the session's model already resolved;
/// here we can't see it, so we use the GPT row via a synthetic id.
fn price(block: &Value) -> f64 {
    let get = |k: &str| block.get(k).and_then(Value::as_u64).unwrap_or(0) as f64;
    let mi = model_info("gpt");
    let cached = get("cached_input_tokens");
    let input = get("input_tokens");
    let non_cached = (input - cached).max(0.0);
    (non_cached * mi.price_in
        + cached * mi.price_cache_read
        + get("cache_write_input_tokens") * mi.price_cache_write
        + get("output_tokens") * mi.price_out)
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
        let cost = price(usage);
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
