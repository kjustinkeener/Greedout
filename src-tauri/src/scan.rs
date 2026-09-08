//! Session discovery and cheap transcript tailing.
//!
//! We never read whole transcripts (they reach 200KB-1MB+). We stat every
//! candidate `.jsonl` (cheap), keep only those touched within the inactivity
//! window, then tail the last 64KB of each and scan backward for the newest
//! assistant `usage`, `custom-title`, `last-prompt`, and `cwd` records.

use crate::config::{claude_dir, Config};
use crate::grouping;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const TAIL_BYTES: u64 = 64 * 1024;
const SUBTITLE_MAX: usize = 80;

/// Pricing and context limits for one model. Prices are USD per million tokens
/// (as published), converted to per-token at use. `context_max` is the hard
/// enforced window; `target` is the recommended "sweet spot" the gauge fills to.
struct ModelInfo {
    label: &'static str,
    price_in: f64,
    price_out: f64,
    price_cache_write: f64,
    price_cache_read: f64,
    context_max: u64,
    target: u64,
}

/// Look up a model by (case-insensitive substring of) its id. Central table --
/// add rows here as we bring in other clouds (GPT, Gemini, etc.). Unknown models
/// fall back to the Opus row (a safe high estimate) with a "?" label.
fn model_info(model: &str) -> ModelInfo {
    let m = model.to_ascii_lowercase();
    // --- Anthropic Claude ---
    // Sweet spot 200k, hard window 1M (per our own findings).
    if m.contains("opus") {
        return ModelInfo { label: "Opus", price_in: 15.0, price_out: 75.0,
            price_cache_write: 18.75, price_cache_read: 1.50,
            context_max: 1_000_000, target: 200_000 };
    }
    if m.contains("sonnet") {
        return ModelInfo { label: "Sonnet", price_in: 3.0, price_out: 15.0,
            price_cache_write: 3.75, price_cache_read: 0.30,
            context_max: 1_000_000, target: 200_000 };
    }
    if m.contains("haiku") {
        return ModelInfo { label: "Haiku", price_in: 1.0, price_out: 5.0,
            price_cache_write: 1.25, price_cache_read: 0.10,
            context_max: 200_000, target: 200_000 };
    }
    // --- future: other cloud models go here ---
    ModelInfo { label: "?", price_in: 15.0, price_out: 75.0,
        price_cache_write: 18.75, price_cache_read: 1.50,
        context_max: 1_000_000, target: 200_000 }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub project: String,
    pub project_path: String,
    /// Directory of the project this session is grouped under. Same as
    /// `project_path` unless the session ran in a subdirectory (see grouping.rs).
    pub project_root: String,
    /// Path from the project down to the session's own directory (`src-tauri`),
    /// empty when the session ran in the project directory itself.
    pub sub_path: String,
    pub title: String,
    pub subtitle: String,
    pub ctx: Option<u64>,
    /// Gauge denominator: the model's recommended sweet-spot budget.
    pub target: u64,
    /// The model's hard enforced context window.
    pub limit: u64,
    /// Short model name (Opus / Sonnet / Haiku / …).
    pub model: String,
    /// Version pulled from the raw model id (e.g. "4.8", "5"); empty if unknown.
    pub model_version: String,
    pub pct: Option<f64>,
    pub live: bool,
    pub mtime: u64,
    /// Transcript file size in bytes.
    pub size_bytes: u64,
    /// Cumulative estimated spend (USD) over the whole session so far.
    pub cost_usd: f64,
    /// Currently open in the Claude app: pinned to top and shown bold. Transient
    /// (re-read each poll); not persisted, so it reverts on switching away.
    pub focused: bool,
}

/// One point on the focus panel's over-time graph: context size and cumulative
/// spend at a given moment (epoch milliseconds, matching the frontend clock).
#[derive(Debug, Clone, Serialize)]
pub struct Sample {
    pub t: i64,
    pub ctx: u64,
    pub cost: f64,
}

/// What a single backward tail-scan yields.
#[derive(Default)]
struct Tailed {
    ctx: Option<u64>,
    model: Option<String>,
    title: Option<String>,
    subtitle: Option<String>,
    cwd: Option<String>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Build the current snapshot: active sessions, newest first, capped at `n`.
pub fn scan(cfg: &Config, labels: &HashMap<String, String>) -> Vec<Session> {
    let now = now_secs();
    // Forward slashes are REQUIRED: the glob crate treats `\` as an escape, so a
    // backslash `projects\*\*.jsonl` silently matches nothing on Windows (only the
    // focus-forced session would ever show). Same fix as session_history().
    let pattern = claude_dir()
        .join("projects")
        .join("*")
        .join("*.jsonl")
        .to_string_lossy()
        .replace('\\', "/");

    // The session currently open in the Claude app, if we can tell. It's forced
    // to the top so switching to a session shows its gauge immediately, without
    // waiting for Claude to write to the transcript.
    let focused = if cfg.follow_focus {
        focused_session_id()
    } else {
        None
    };

    // (path, mtime) for every transcript. We keep the list ALWAYS full: rather
    // than drop a session the instant it goes idle, we show the `n` most-recent
    // transcripts and let idle ones stay until a newer session bumps them off the
    // bottom. Age is conveyed by the row's mtime-based dimming (frontend), so there
    // is no idle cutoff here at all.
    let mut candidates: Vec<(PathBuf, u64)> = Vec::new();
    if let Ok(paths) = glob::glob(&pattern) {
        for entry in paths.flatten() {
            let Ok(meta) = std::fs::metadata(&entry) else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let mtime = mtime
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            candidates.push((entry, mtime));
        }
    }

    // Sort key: focused session pinned first, then most-recent mtime.
    let sort_key = |p: &Path, mtime: u64| -> u64 {
        let focused_here = focused
            .as_deref()
            .map(|f| p.file_stem().map(|s| s == f).unwrap_or(false))
            .unwrap_or(false);
        if focused_here {
            u64::MAX
        } else {
            mtime
        }
    };
    candidates.sort_by(|a, b| sort_key(&b.0, b.1).cmp(&sort_key(&a.0, a.1)));
    candidates.truncate(cfg.n);

    let mut sessions: Vec<Session> = candidates
        .into_iter()
        .map(|(path, mtime)| build_session(&path, mtime, now, cfg, labels, focused.as_deref()))
        .collect();
    apply_grouping(&mut sessions);
    sessions
}

/// Every project folder Claude has written, decoded to a working directory. Only
/// the `n` newest sessions get built, but grouping needs to see all of them: which
/// parents hold several projects is what says where the projects are.
pub(crate) fn known_project_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(claude_dir().join("projects")) else {
        return out;
    };
    for e in entries.flatten() {
        if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = e.file_name().to_string_lossy().to_string();
        out.push(PathBuf::from(decode_project_dir(&name)));
    }
    out
}

/// Fold sessions that ran in a subdirectory back into their project: the label
/// becomes the project's, with the subdirectory kept alongside it so the row still
/// says where the session actually started. `project_path` is left alone.
fn apply_grouping(sessions: &mut [Session]) {
    let mut dirs = known_project_dirs();
    // The transcript's own `cwd` is exact where the decoded folder name is only a
    // guess, so add the scanned sessions' paths as better evidence.
    let mut seen: std::collections::HashSet<String> =
        dirs.iter().map(|d| d.to_string_lossy().to_lowercase()).collect();
    for s in sessions.iter() {
        if seen.insert(s.project_path.to_lowercase()) {
            dirs.push(PathBuf::from(&s.project_path));
        }
    }
    let groups = grouping::group_dirs(&dirs);
    for s in sessions.iter_mut() {
        match groups.get(Path::new(&s.project_path)) {
            Some(g) => {
                s.project = g.label.clone();
                s.project_root = g.root.to_string_lossy().to_string();
                s.sub_path = g.sub.clone().unwrap_or_default();
            }
            None => s.project_root = s.project_path.clone(),
        }
    }
}

fn build_session(
    path: &Path,
    mtime: u64,
    now: u64,
    cfg: &Config,
    labels: &HashMap<String, String>,
    focused: Option<&str>,
) -> Session {
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let is_focused = focused == Some(id.as_str());

    let t = tail_scan(path).unwrap_or_default();
    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let cost_usd = cumulative_cost(&id, path);

    // Project name: prefer the exact cwd from the transcript; fall back to
    // best-effort decoding of the encoded parent dir name.
    let (project, project_path) = match &t.cwd {
        Some(cwd) => (last_component(cwd), cwd.clone()),
        None => {
            let dir = path
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let decoded = decode_project_dir(&dir);
            (last_component(&decoded), decoded)
        }
    };

    // Title precedence: custom label > native customTitle > project name.
    // A native title found in the tail (a recent rename, or the original on a
    // short session) is authoritative and refreshes the cache; otherwise fall
    // back to the head-read title (the original, which sits above the tail
    // window on long sessions) before giving up to the project name.
    let native_title = match t.title.clone() {
        Some(tt) => {
            if let Ok(mut c) = title_cache().lock() {
                c.insert(id.clone(), tt.clone());
            }
            Some(tt)
        }
        None => head_title(&id, path),
    };
    let title = labels
        .get(&id)
        .cloned()
        .or(native_title)
        .unwrap_or_else(|| project.clone());

    // Model drives the gauge budget, the hard window, and the label. Unknown /
    // not-yet-seen model falls back to the configured global target.
    let mi = t.model.as_deref().map(model_info);
    let target = mi.as_ref().map(|i| i.target).unwrap_or(cfg.target_tokens);
    let limit = mi.as_ref().map(|i| i.context_max).unwrap_or(cfg.target_tokens);
    let model = mi.as_ref().map(|i| i.label.to_string()).unwrap_or_default();
    let model_version = t.model.as_deref().map(parse_model_version).unwrap_or_default();

    // Current size when known; otherwise fall back to the session's startup
    // context (first turn) so the gauge shows a sensible floor right after a
    // /compact instead of blanking to "--" until the next turn lands.
    let ctx = t.ctx.or_else(|| initial_ctx(&id, path));
    let pct = ctx.map(|c| c as f64 / target.max(1) as f64);
    let live = now.saturating_sub(mtime) as f64 <= (cfg.poll_seconds * 2.0).max(1.0);

    Session {
        id,
        project,
        // Filled in by apply_grouping() once every project dir is known.
        project_root: project_path.clone(),
        sub_path: String::new(),
        project_path,
        title,
        subtitle: t.subtitle.unwrap_or_default(),
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

/// Running spend per session, so we only parse transcript bytes appended since
/// the last poll instead of re-reading the whole file each time.
struct CostState {
    /// Byte offset up to which we've already tallied (always on a line boundary).
    offset: u64,
    /// Accumulated USD so far.
    cost: f64,
    /// Assistant message ids already counted. A transcript can contain the same
    /// message many times over (resume/rewind re-appends history; a heavily
    /// compacted session had each turn written ~7-8x), so without deduping we'd
    /// bill every duplicate and overstate spend several fold.
    seen: HashSet<String>,
}

fn cost_cache() -> &'static Mutex<HashMap<String, CostState>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CostState>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cumulative estimated spend for a session. On first sight we tally the whole
/// transcript once; after that only the newly-appended tail is parsed. Offset is
/// kept on a newline boundary so a half-written final line is never double-counted.
fn cumulative_cost(id: &str, path: &Path) -> f64 {
    let mut cache = match cost_cache().lock() {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let state = cache
        .entry(id.to_string())
        .or_insert(CostState { offset: 0, cost: 0.0, seen: HashSet::new() });

    let Ok(mut file) = std::fs::File::open(path) else { return state.cost };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    // File shrank or rotated (e.g. cleared/rewritten): start the tally over.
    if len < state.offset {
        state.offset = 0;
        state.cost = 0.0;
        state.seen.clear();
    }
    if len == state.offset {
        return state.cost;
    }

    if file.seek(SeekFrom::Start(state.offset)).is_err() {
        return state.cost;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return state.cost;
    }
    // Only consume through the last complete line; leave any partial tail for next time.
    let last_nl = match buf.iter().rposition(|&b| b == b'\n') {
        Some(i) => i,
        None => return state.cost,
    };
    let consumed = &buf[..=last_nl];
    let text = String::from_utf8_lossy(consumed);
    for line in text.lines() {
        if line.contains("\"usage\"") && line.contains("\"assistant\"") {
            match line_id_cost(line) {
                // Count each assistant message once; skip duplicate re-appends.
                Some((Some(mid), c)) => {
                    if state.seen.insert(mid) {
                        state.cost += c;
                    }
                }
                // No id available: fall back to counting it (better than dropping).
                Some((None, c)) => state.cost += c,
                None => {}
            }
        }
    }
    state.offset += consumed.len() as u64;
    state.cost
}

/// USD cost of one assistant turn plus its message id (when present) so callers
/// can dedupe -- a transcript often repeats the same message many times. Priced
/// by the turn's own model so a mid-session model switch is billed exactly.
fn line_id_cost(line: &str) -> Option<(Option<String>, f64)> {
    let v = serde_json::from_str::<Value>(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let msg = v.get("message");
    let u = msg.and_then(|m| m.get("usage")).or_else(|| v.get("usage"))?;
    let model = msg.and_then(|m| m.get("model")).and_then(Value::as_str).unwrap_or("");
    let p = model_info(model);
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as f64;
    let cost = (get("input_tokens") * p.price_in
        + get("output_tokens") * p.price_out
        + get("cache_creation_input_tokens") * p.price_cache_write
        + get("cache_read_input_tokens") * p.price_cache_read)
        / 1_000_000.0;
    let id = msg.and_then(|m| m.get("id")).and_then(Value::as_str).map(str::to_string);
    Some((id, cost))
}

/// Extract everything the enrich pass needs from ONE already-parsed assistant
/// usage record: `(message id, ctx tokens, USD cost, model id)`. Folds what used
/// to be three separate `serde_json::from_str` calls (ctx, model, id+cost) into a
/// single parse. Returns None for non-assistant records or missing usage.
fn assistant_usage(v: &Value) -> Option<(Option<String>, u64, f64, Option<String>)> {
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let msg = v.get("message");
    let u = msg.and_then(|m| m.get("usage")).or_else(|| v.get("usage"))?;
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let ctx = get("input_tokens") + get("cache_creation_input_tokens") + get("cache_read_input_tokens");
    let model = msg.and_then(|m| m.get("model")).and_then(Value::as_str);
    let p = model_info(model.unwrap_or(""));
    let cost = (get("input_tokens") as f64 * p.price_in
        + get("output_tokens") as f64 * p.price_out
        + get("cache_creation_input_tokens") as f64 * p.price_cache_write
        + get("cache_read_input_tokens") as f64 * p.price_cache_read)
        / 1_000_000.0;
    let id = msg.and_then(|m| m.get("id")).and_then(Value::as_str).map(str::to_string);
    Some((id, ctx, cost, model.map(str::to_string)))
}

/// `compactMetadata.postTokens` from an already-parsed `compact_boundary` record.
fn post_tokens_from(v: &Value) -> Option<u64> {
    if v.get("subtype")?.as_str()? != "compact_boundary" {
        return None;
    }
    v.get("compactMetadata").and_then(|m| m.get("postTokens")).and_then(Value::as_u64)
}

/// Parse one assistant line once into `(timestamp_millis, ctx, cost)` for the
/// history backfill -- avoids re-parsing the same JSON separately for ctx,
/// timestamp, and cost. Returns None for non-assistant lines or missing fields.
fn history_row(line: &str) -> Option<(Option<String>, i64, u64, f64)> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let msg = v.get("message");
    let u = msg.and_then(|m| m.get("usage")).or_else(|| v.get("usage"))?;
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let ctx = get("input_tokens") + get("cache_creation_input_tokens") + get("cache_read_input_tokens");

    let ts = v.get("timestamp").and_then(Value::as_str)?;
    let t = iso_to_millis(ts)?;

    let model = msg.and_then(|m| m.get("model")).and_then(Value::as_str).unwrap_or("");
    let p = model_info(model);
    let cost = (get("input_tokens") as f64 * p.price_in
        + get("output_tokens") as f64 * p.price_out
        + get("cache_creation_input_tokens") as f64 * p.price_cache_write
        + get("cache_read_input_tokens") as f64 * p.price_cache_read)
        / 1_000_000.0;

    let id = msg.and_then(|m| m.get("id")).and_then(Value::as_str).map(str::to_string);
    Some((id, t, ctx, cost))
}

/// The session's startup context: the first assistant `usage` record in the
/// transcript. Cached per session (it never changes) and read from the file head
/// only when needed, i.e. when the current size is unknown.
fn initial_ctx_cache() -> &'static Mutex<HashMap<String, u64>> {
    static CACHE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn initial_ctx(id: &str, path: &Path) -> Option<u64> {
    if let Ok(cache) = initial_ctx_cache().lock() {
        if let Some(v) = cache.get(id) {
            return Some(*v);
        }
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; TAIL_BYTES as usize];
    let read = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..read]);
    let found = text.lines().find_map(|line| {
        (line.contains("\"assistant\"") && line.contains("\"usage\"")).then(|| parse_ctx(line)).flatten()
    })?;
    if let Ok(mut cache) = initial_ctx_cache().lock() {
        cache.insert(id.to_string(), found);
    }
    Some(found)
}

/// The session's title, cached per id. Claude writes the native `custom-title`
/// ONCE near the top of the transcript, so `tail_scan` (which stops growing as
/// soon as it has ctx, always within the last 64KB) never reaches it on a long
/// session and the row would fall back to the project name. So: read the title
/// from the file HEAD when we don't already have it cached. A later rename is
/// appended near EOF, caught by `tail_scan`, and written back into this cache by
/// the caller, so the cache always holds the freshest title we have ever seen.
fn title_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn head_title(id: &str, path: &Path) -> Option<String> {
    if let Ok(cache) = title_cache().lock() {
        if let Some(v) = cache.get(id) {
            return Some(v.clone());
        }
    }
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    // The title is near the top, but big early records (a long first prompt, an
    // attachment) can push it past a small head window, so grow until we find it
    // or have read the whole file. Only reached on a cache miss, then cached.
    let mut win = TAIL_BYTES;
    loop {
        file.seek(SeekFrom::Start(0)).ok()?;
        let mut buf = vec![0u8; win.min(len) as usize];
        let read = file.read(&mut buf).ok()?;
        let text = String::from_utf8_lossy(&buf[..read]);
        let found = text.lines().find_map(|line| {
            line.contains("custom-title").then(|| parse_field(line, "customTitle")).flatten()
        });
        if let Some(v) = found {
            if let Ok(mut cache) = title_cache().lock() {
                cache.insert(id.to_string(), v.clone());
            }
            return Some(v);
        }
        if win >= len {
            return None;
        }
        win = win.saturating_mul(8);
    }
}

/// Read a trailing window and scan lines backward for the newest of each target
/// record. Starts at `TAIL_BYTES` but grows the window (and rescans) whenever the
/// current context wasn't found within it: a burst of large non-usage records
/// (attachments, sidecar lines) can push the newest assistant `usage` further
/// back than a small tail, and without growing we'd silently report a stale,
/// far-too-small context (a 160k/80% session reading as 55k/28%). Growth stops
/// once ctx is found or the whole file has been read.
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
        // Found the real context, or we've already read from byte 0 (nothing more
        // to grow into): accept this result.
        if out.ctx.is_some() || start == 0 {
            return Some(out);
        }
        win = win.saturating_mul(8);
    }
}

/// Scan one trailing buffer backward for the newest of each target record.
/// `partial_start` is true when the buffer began mid-file, so its first line is
/// likely a truncated record and must be dropped.
fn scan_tail_buf(buf: &[u8], partial_start: bool) -> Tailed {
    let text = String::from_utf8_lossy(buf);

    let mut lines: Vec<&str> = text.lines().collect();
    // If we started mid-file, the first line is likely a partial record.
    if partial_start && !lines.is_empty() {
        lines.remove(0);
    }

    let mut out = Tailed::default();
    for line in lines.iter().rev() {
        // A /compact leaves the old (large) pre-compact usage records in the
        // transcript. Scanning backward, the first thing we hit is the truth:
        // a post-compact assistant usage (real current size) OR, if none has
        // landed yet, the compact_boundary itself -- whose postTokens is the
        // exact reset size. Either way we stop counting ctx there, so we never
        // report the stale pre-compact number right after a compact.
        if out.ctx.is_none()
            && line.contains("\"assistant\"")
            && line.contains("\"usage\"")
        {
            if let Some(c) = parse_ctx(line) {
                out.ctx = Some(c);
                out.model = parse_model(line);
            }
        }
        if out.ctx.is_none() && line.contains("compact_boundary") {
            // Seen the boundary before any newer usage: context was reset.
            // Use its postTokens; fall back to 0 (empty) rather than let a
            // pre-compact usage further back masquerade as the current size.
            out.ctx = Some(parse_post_tokens(line).unwrap_or(0));
        }
        if out.title.is_none() && line.contains("custom-title") {
            if let Some(v) = parse_field(line, "customTitle") {
                out.title = Some(v);
            }
        }
        if out.subtitle.is_none() && line.contains("last-prompt") {
            if let Some(v) = parse_field(line, "lastPrompt") {
                out.subtitle = Some(truncate(&v, SUBTITLE_MAX));
            }
        }
        if out.cwd.is_none() && line.contains("\"cwd\"") {
            if let Some(v) = parse_field(line, "cwd") {
                out.cwd = Some(v);
            }
        }
        // Stop once we have everything this tail can give us.
        if out.ctx.is_some()
            && out.title.is_some()
            && out.subtitle.is_some()
            && out.cwd.is_some()
        {
            break;
        }
    }
    out
}

/// The transcript session id currently focused in the Claude desktop app.
///
/// The app tracks focus by its OWN session id (`local_<uuid>`), which is NOT the
/// transcript filename. The two are linked by per-session sidecars the app writes
/// under `%APPDATA%\Claude\claude-code-sessions\<proj>\<host>\local_*.json`, each
/// carrying `cliSessionId` (the transcript stem) and `lastFocusedAt`. The focused
/// session is simply the sidecar with the newest `lastFocusedAt`; we return its
/// `cliSessionId`. Reading the app's `main.log` instead never worked -- its ids
/// are `local_<uuid>`, so they matched no transcript file. Best-effort: a missing
/// dir just yields `None` (no pin) rather than an error.
///
/// The app rewrites `lastFocusedAt` the instant you switch sessions, so this pins
/// a freshly-opened session on the next poll -- before it has written any turn to
/// its transcript (the old symptom: a session stayed invisible until you sent a
/// prompt and its mtime moved).
/// The folder holding the Claude desktop app's per-session sidecar files.
///
/// The desktop app is packaged (MSIX), so it writes under its package's private
/// store, NOT the plain `%APPDATA%\Claude` (which a non-packaged reader like us
/// sees as empty). We therefore prefer the package overlay
/// `%LOCALAPPDATA%\Packages\Claude_*\LocalCache\Roaming\Claude\claude-code-sessions`,
/// and fall back to plain `%APPDATA%\Claude\claude-code-sessions` for a future
/// non-packaged Claude install.
fn claude_sessions_dir() -> Option<PathBuf> {
    if let Some(local) = dirs::data_local_dir() {
        let pkgs = local.join("Packages");
        if let Ok(entries) = std::fs::read_dir(&pkgs) {
            for e in entries.flatten() {
                let name = e.file_name();
                if name.to_string_lossy().starts_with("Claude_") {
                    let cand = e
                        .path()
                        .join("LocalCache")
                        .join("Roaming")
                        .join("Claude")
                        .join("claude-code-sessions");
                    if cand.is_dir() {
                        return Some(cand);
                    }
                }
            }
        }
    }
    let plain = dirs::config_dir()?.join("Claude").join("claude-code-sessions");
    plain.is_dir().then_some(plain)
}

fn focused_session_id() -> Option<String> {
    let base = claude_sessions_dir()?;
    // The glob crate matches unreliably across multiple `\`-separated wildcard
    // segments on Windows; forward slashes match fine and Windows accepts them.
    let pattern = base
        .join("*")
        .join("*")
        .join("local_*.json")
        .to_string_lossy()
        .replace('\\', "/");

    let mut best: Option<(u64, String)> = None;
    for path in glob::glob(&pattern).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        let focused_at = v.get("lastFocusedAt").and_then(Value::as_u64).unwrap_or(0);
        let Some(cli) = v.get("cliSessionId").and_then(Value::as_str) else { continue };
        if best.as_ref().map(|(t, _)| focused_at > *t).unwrap_or(true) {
            best = Some((focused_at, cli.to_string()));
        }
    }
    best.map(|(_, id)| id)
}

/// Full over-time curve for one session, reconstructed from its transcript:
/// one point per assistant turn (its context size and the running spend up to
/// that turn), timestamped from the record's own `timestamp`. This is a one-time
/// full read (vs the cheap tail used every poll); the result is downsampled so
/// the payload and the SVG stay small no matter how long the session ran.
pub fn session_history(id: &str) -> Vec<Sample> {
    let pattern = claude_dir()
        .join("projects")
        .join("*")
        .join(format!("{id}.jsonl"))
        .to_string_lossy()
        .replace('\\', "/");
    let Some(path) = glob::glob(&pattern).ok().and_then(|mut g| g.find_map(Result::ok)) else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };

    // Collect (time, ctx, this line's cost) first. Transcript lines aren't
    // strictly time-ordered (subagent sidechains interleave), so we sort by
    // time before running the cumulative sum -- otherwise the spend curve would
    // zig-zag once the points are drawn left-to-right by time.
    let mut rows: Vec<(i64, u64, f64)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for line in text.lines() {
        if !(line.contains("\"usage\"") && line.contains("\"assistant\"")) {
            continue;
        }
        // One JSON parse per line yields time, ctx, and cost together. Parsing
        // three times over (ctx + timestamp + cost) made backfilling a long
        // multi-MB transcript needlessly slow ("collecting history..." lingered).
        // Dedupe by message id: a resumed/heavily-compacted transcript repeats
        // the same turn many times, which would otherwise inflate the spend curve.
        if let Some((id, t, ctx, cost)) = history_row(line) {
            if let Some(mid) = id {
                if !seen.insert(mid) {
                    continue;
                }
            }
            rows.push((t, ctx, cost));
        }
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

/// One billed assistant turn, for the Daily Spend window: a timestamp, its
/// estimated cost, and which session/project it belongs to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpendEvent {
    /// Turn timestamp, epoch millis (matches the frontend clock).
    pub t: i64,
    /// This turn's estimated USD cost.
    pub cost: f64,
    /// Session id (transcript file stem) it was first seen in.
    pub session: String,
    /// Readable project name (leaf of the decoded transcript directory).
    pub project: String,
}

/// Full per-session metadata for the browse cache (see browse.rs). Unlike the
/// poll-time `Session`, this is computed from a single WHOLE-file read, so it can
/// report the deduped cumulative cost, the turn count, and whether the session
/// ever ran `/context` -- none of which a cheap tail can know. Reserved for the
/// opt-in cross-session index; not on the hot poll path.
#[derive(Debug, Clone, Default)]
pub struct EnrichMeta {
    pub ctx: Option<u64>,
    pub model_label: String,
    pub model_version: String,
    pub target: u64,
    pub limit: u64,
    pub cost_usd: f64,
    pub turn_count: u64,
    pub title: Option<String>,
    pub cwd: Option<String>,
    /// True if any `/context` (`contextUsage`) record exists anywhere in the file,
    /// so the UI knows the deepest breakdown tile is available vs a placeholder.
    pub has_context_usage: bool,
    /// Every billed assistant turn in the file: (message id, timestamp ms, cost).
    /// Kept UNdeduped (unlike cost_usd/turn_count above): the Daily Spend view
    /// dedups by message id globally across files at read time, so per-file rows
    /// must stay raw. Collected here so the whole tree is parsed once, not twice.
    pub turns: Vec<(Option<String>, i64, f64)>,
}

/// Read a whole transcript once and derive its browse-cache row. ctx is
/// compact-aware (a later `compact_boundary` or post-compact usage wins, same
/// rule as the backward tail scan). Cost and turn count are deduped by assistant
/// message id, since resumed/compacted transcripts re-append whole turns.
pub fn enrich_meta(path: &Path) -> EnrichMeta {
    let mut out = EnrichMeta::default();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    let mut seen: HashSet<String> = HashSet::new();
    let mut last_model: Option<String> = None;
    for line in text.lines() {
        // The `/context` flag only needs the substring -- never parse those (they
        // are the largest lines in the file).
        if line.contains("\"contextUsage\"") {
            out.has_context_usage = true;
        }
        // Cheap substring prefilter: parse each interesting line EXACTLY ONCE,
        // then pull every field from that single Value (was 2-3 parses per line).
        let is_usage = line.contains("\"usage\"") && line.contains("\"assistant\"");
        let is_compact = line.contains("compact_boundary");
        let is_title = line.contains("custom-title");
        let is_cwd = line.contains("\"cwd\"");
        if !(is_usage || is_compact || is_title || is_cwd) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if is_usage {
            if let Some((id, ctx, cost, model)) = assistant_usage(&v) {
                out.ctx = Some(ctx);
                if let Some(m) = model {
                    last_model = Some(m);
                }
                // Per-turn row for Daily Spend: needs the timestamp (assistant_usage
                // doesn't return it). Skip rows with no timestamp -- they can't be
                // placed on a day. Rows are raw here; dedup happens at read time.
                if let Some(t) = v.get("timestamp").and_then(Value::as_str).and_then(iso_to_millis) {
                    out.turns.push((id.clone(), t, cost));
                }
                let fresh = match id {
                    Some(mid) => seen.insert(mid),
                    None => true, // no id: count it (better than dropping)
                };
                if fresh {
                    out.cost_usd += cost;
                    out.turn_count += 1;
                }
            }
        }
        if is_compact {
            if let Some(pt) = post_tokens_from(&v) {
                out.ctx = Some(pt);
            }
        }
        if is_title {
            if let Some(t) = v.get("customTitle").and_then(Value::as_str) {
                out.title = Some(t.to_string());
            }
        }
        if is_cwd {
            if let Some(c) = v.get("cwd").and_then(Value::as_str) {
                out.cwd = Some(c.to_string());
            }
        }
    }
    if let Some(m) = last_model {
        let mi = model_info(&m);
        out.model_label = mi.label.to_string();
        out.target = mi.target;
        out.limit = mi.context_max;
        out.model_version = parse_model_version(&m);
    }
    out
}

/// Resolve a session id to its transcript file path (searches every project).
pub fn transcript_path(id: &str) -> Option<PathBuf> {
    let pattern = claude_dir()
        .join("projects")
        .join("*")
        .join(format!("{id}.jsonl"))
        .to_string_lossy()
        .replace('\\', "/");
    glob::glob(&pattern).ok().and_then(|mut g| g.find_map(Result::ok))
}

/// Evenly thin a series down to at most `max` points, always keeping the last.
fn downsample(v: Vec<Sample>, max: usize) -> Vec<Sample> {
    let n = v.len();
    if n <= max || max == 0 {
        return v;
    }
    let last = n - 1;
    (0..max)
        .map(|i| if i + 1 == max { last } else { i * last / (max - 1) })
        .map(|idx| v[idx].clone())
        .collect()
}

/// Parse an ISO-8601 UTC instant like `2026-08-30T16:59:32.812Z` to epoch
/// milliseconds. Avoids a date-library dependency; assumes the trailing `Z`.
pub(crate) fn iso_to_millis(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse().ok() };
    let year = num(0, 4)?;
    let month = num(5, 7)?;
    let day = num(8, 10)?;
    let hour = num(11, 13)?;
    let min = num(14, 16)?;
    let sec = num(17, 19)?;
    let millis: i64 = if b.get(19) == Some(&b'.') {
        s.get(20..23).and_then(|f| f.parse().ok()).unwrap_or(0)
    } else {
        0
    };
    // Days from civil date (Howard Hinnant's algorithm), epoch 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12; // Mar=0 .. Feb=11
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + hour * 3600 + min * 60 + sec) * 1000) + millis)
}

/// ctx = input + cache_creation + cache_read from an assistant record's usage.
fn parse_ctx(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let u = v.get("message").and_then(|m| m.get("usage")).or_else(|| v.get("usage"))?;
    let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(get("input_tokens") + get("cache_creation_input_tokens") + get("cache_read_input_tokens"))
}

/// Extract a human version from a raw model id, e.g. "claude-opus-4-8-2025..."
/// -> "4.8", "claude-sonnet-5" -> "5". Numeric segments after the family name are
/// joined with '.'; long segments (8+ digits, i.e. a date stamp) are ignored.
fn parse_model_version(id: &str) -> String {
    let parts: Vec<&str> = id
        .split(['-', '_'])
        .filter(|s| s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() && s.len() < 4)
        .collect();
    parts.join(".")
}

/// Per-token USD rates (input, output, cache-read) for a session's model, so the
/// Context Explorer can price a block by role: generated blocks (assistant text,
/// thinking, tool calls) at the output rate, fed-in blocks (user prompts, tool
/// results) at the input rate, and carried baseline context at the cache-read rate.
/// This mirrors how the main speedo prices a turn from its usage counts.
pub fn context_price_rates(id: &str) -> (f64, f64, f64) {
    let model = transcript_path(id).and_then(|p| {
        let s = std::fs::read_to_string(&p).ok()?;
        s.lines().rev().find_map(parse_model)
    });
    let mi = model_info(model.as_deref().unwrap_or(""));
    (
        mi.price_in / 1_000_000.0,
        mi.price_out / 1_000_000.0,
        mi.price_cache_read / 1_000_000.0,
    )
}

/// The `message.model` id from an assistant record, if present.
fn parse_model(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get("message")
        .and_then(|m| m.get("model"))
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

/// Post-compact context size recorded on a `compact_boundary` system record
/// (`compactMetadata.postTokens`). This is the exact window size immediately
/// after a /compact, before any new turn has written a fresh usage.
fn parse_post_tokens(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("subtype")?.as_str()? != "compact_boundary" {
        return None;
    }
    v.get("compactMetadata")
        .and_then(|m| m.get("postTokens"))
        .and_then(Value::as_u64)
}

/// Pull a top-level string field out of a one-line JSON record.
fn parse_field(line: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    v.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

pub(crate) fn last_component(p: &str) -> String {
    p.replace('/', "\\")
        .rsplit('\\')
        .find(|s| !s.is_empty())
        .unwrap_or(p)
        .to_string()
}

/// Best-effort decode of `C--code-Greedout`. Lossy: a literal `-` in a
/// path segment is indistinguishable from a separator, so this is a fallback
/// only (we normally have the exact `cwd`).
pub(crate) fn decode_project_dir(dir: &str) -> String {
    // Leading drive letter: `C--` -> `C:\`.
    if dir.len() >= 3 && dir.as_bytes()[0].is_ascii_alphabetic() && &dir[1..3] == "--" {
        let drive = &dir[0..1];
        let rest = dir[3..].replace('-', "\\");
        format!("{drive}:\\{rest}")
    } else {
        dir.replace('-', "\\")
    }
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('\u{2026}');
        out
    }
}
