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
pub(crate) struct ModelInfo {
    pub label: &'static str,
    pub price_in: f64,
    pub price_out: f64,
    pub price_cache_write: f64,
    pub price_cache_read: f64,
    pub context_max: u64,
    pub target: u64,
}

/// Look up a model by (case-insensitive substring of) its id. Central table --
/// add rows here as we bring in other clouds (GPT, Gemini, etc.). Unknown models
/// fall back to the Opus row (a safe high estimate) with a "?" label.
pub(crate) fn model_info(model: &str) -> ModelInfo {
    let m = model.to_ascii_lowercase();
    // --- OpenAI GPT / Codex ---
    // Codex (desktop + CLI) reports ids like "gpt-5", "gpt-5-codex", the smaller
    // "gpt-5-mini"/"gpt-5-nano", or internal codenames ("gpt-5.6-terra"). All share
    // the 400k window (sweet spot 272k = the input half); only the price tier
    // differs. Codex's `input_tokens` already includes cached, so the cache-read
    // row is priced against `cached_input_tokens`. Rates per OpenAI's API pricing
    // (verified 2026-09-09). NOTE: versioned codex variants (gpt-5.3-codex at
    // 1.75/14, and the unpriced gpt-5.x codenames the desktop app ships) cost MORE
    // than base gpt-5; without a published number they fall through to the gpt-5
    // row below, so their spend is an under-estimate.
    let gpt = m.contains("gpt")
        || (m.starts_with('o') && m[1..].chars().next().is_some_and(|c| c.is_ascii_digit()));
    if gpt {
        // GPT-6 (gpt-6-astra, released 2026-09-03; VERIFIED against OpenAI docs
        // 2026-09-10). 1.05M window; 272k is astra's real tier boundary -- above it
        // the WHOLE request bills 2x input / 1.5x output, so 272k is a genuine
        // "getting expensive" target, not a guess. We price per turn at the standard
        // (<=272k) rate; a rare >272k turn is under-priced, acceptable for a gauge.
        if m.contains("gpt-6") {
            return ModelInfo { label: "GPT", price_in: 10.0, price_out: 50.0,
                price_cache_write: 12.5, price_cache_read: 1.0,
                context_max: 1_050_000, target: 272_000 };
        }
        if m.contains("nano") {
            return ModelInfo { label: "GPT", price_in: 0.05, price_out: 0.40,
                price_cache_write: 0.05, price_cache_read: 0.005,
                context_max: 400_000, target: 272_000 };
        }
        if m.contains("mini") {
            return ModelInfo { label: "GPT", price_in: 0.25, price_out: 2.0,
                price_cache_write: 0.25, price_cache_read: 0.025,
                context_max: 400_000, target: 272_000 };
        }
        // gpt-5.6 (terra/sol/luna) and gpt-5.5 are separately-priced published
        // tiers, NOT one price -- and the version number doesn't track cost (5.5 is
        // dearer than 5.6-sol). VERIFIED against OpenAI API pricing 2026-09-11.
        // cache-write mirrors input (Codex logs cache_write=0 anyway); context_max
        // falls back to 400k, but Codex sessions read their real
        // model_context_window off disk (see codex.rs), so this only backstops
        // browse. Match the codename with its leading hyphen so "sol"/"luna" can't
        // collide with a substring of some other id.
        if m.contains("-terra") {
            return ModelInfo { label: "GPT", price_in: 2.0, price_out: 12.0,
                price_cache_write: 2.0, price_cache_read: 0.20,
                context_max: 400_000, target: 272_000 };
        }
        if m.contains("-sol") {
            return ModelInfo { label: "GPT", price_in: 4.0, price_out: 20.0,
                price_cache_write: 4.0, price_cache_read: 0.40,
                context_max: 400_000, target: 272_000 };
        }
        if m.contains("-luna") {
            return ModelInfo { label: "GPT", price_in: 0.20, price_out: 1.20,
                price_cache_write: 0.20, price_cache_read: 0.02,
                context_max: 400_000, target: 272_000 };
        }
        if m.contains("gpt-5.5") {
            return ModelInfo { label: "GPT", price_in: 5.0, price_out: 30.0,
                price_cache_write: 5.0, price_cache_read: 0.50,
                context_max: 400_000, target: 272_000 };
        }
        // PLACEHOLDER: any OTHER versioned codex codename (decimal minor after the
        // 5) with no published price we've captured. Proxy to the confirmed
        // gpt-5.3-codex tier (1.75 / 14) rather than under-billing at the base rate.
        if m.contains("gpt-5.") {
            return ModelInfo { label: "GPT", price_in: 1.75, price_out: 14.0,
                price_cache_write: 1.75, price_cache_read: 0.175,
                context_max: 400_000, target: 272_000 };
        }
        return ModelInfo { label: "GPT", price_in: 1.25, price_out: 10.0,
            price_cache_write: 1.25, price_cache_read: 0.125,
            context_max: 400_000, target: 272_000 };
    }
    // --- Anthropic Claude ---
    // Sweet spot 200k, hard window 1M (per our own findings).
    // Fable 5.1 ($10 in / $50 out): cache-read is a preferential 0.025x (not the
    // usual 0.1x), a rate the pricing table footnotes as exclusive to Fable 5.1 /
    // Mythos 5.1 (verified against Anthropic pricing docs 2026-09-10). Must precede
    // no branch in particular (a fable id matches none of opus/sonnet/haiku), but
    // listed first so the label/price are obvious.
    if m.contains("fable") {
        return ModelInfo { label: "Fable", price_in: 10.0, price_out: 50.0,
            price_cache_write: 12.50, price_cache_read: 0.25,
            context_max: 1_000_000, target: 200_000 };
    }
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
    // One focused session TOTAL across every harness, not one per harness: look up
    // each harness's focus with its own freshness timestamp (Claude's
    // `lastFocusedAt`, Codex's desktop-log mtime, both ms), then keep only the more
    // recent -- that's the session the user is actually looking at right now. The
    // loser is dropped so with n=1 the single gauge tracks true focus across apps.
    let (focused, codex_focused) = if cfg.follow_focus && cfg.gauge_per_harness {
        // One gauge PER HARNESS: pin each harness's own focused session to the top
        // independently, so both a Claude and a Codex panel can show at once. No
        // cross-harness tiebreak -- both winners are kept.
        (
            focused_session_id().map(|(id, _)| id),
            crate::codex::focused_session_id().map(|(id, _)| id),
        )
    } else if cfg.follow_focus {
        let claude = focused_session_id();
        let codex = crate::codex::focused_session_id();
        // A bare alt-tab between the two apps writes nothing to either's logs
        // (Claude rewrites `lastFocusedAt` only on a within-Claude thread switch;
        // Codex logs a route line only when a DIFFERENT thread is selected), so the
        // on-disk selection timestamps can't tell which app you just tabbed to. The
        // OS foreground window can: if the frontmost process is one of the two apps,
        // that app's last-selected thread is what you're looking at. Only when the
        // foreground is neither (e.g. you clicked Greedout itself) do we fall back
        // to whichever selection timestamp is newer.
        // Which of the two apps was frontmost is remembered across polls (LAST_FG)
        // so that tabbing AWAY to some third window (a browser, an editor) holds the
        // last app you were actually in rather than flipping by stale timestamps.
        use std::sync::atomic::{AtomicU8, Ordering};
        static LAST_FG: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 claude, 2 codex
        let fg = foreground_exe();
        let app = match fg.as_deref() {
            Some("chatgpt.exe") => 2,
            Some("claude.exe") => 1,
            _ => LAST_FG.load(Ordering::Relaxed), // third window: keep last app
        };
        match app {
            2 if codex.is_some() => {
                LAST_FG.store(2, Ordering::Relaxed);
                (None, codex.map(|(id, _)| id))
            }
            1 if claude.is_some() => {
                LAST_FG.store(1, Ordering::Relaxed);
                (claude.map(|(id, _)| id), None)
            }
            // No remembered app yet (or its session vanished): newest selection wins.
            _ => match (&claude, &codex) {
                (Some((_, ct)), Some((_, xt))) if xt > ct => (None, codex.map(|(id, _)| id)),
                (Some(_), Some(_)) => (claude.map(|(id, _)| id), None),
                _ => (claude.map(|(id, _)| id), codex.map(|(id, _)| id)),
            },
        }
    } else {
        (None, None)
    };

    // (path, mtime) for every transcript. We keep the list ALWAYS full: rather
    // than drop a session the instant it goes idle, we show the `n` most-recent
    // transcripts and let idle ones stay until a newer session bumps them off the
    // bottom. Age is conveyed by the row's mtime-based dimming (frontend), so there
    // is no idle cutoff here at all.
    // Each candidate is (path, mtime, is_codex). Claude and Codex sessions compete
    // in one pool so the `n` most-recent across BOTH harnesses are what show.
    let mut candidates: Vec<(PathBuf, u64, bool)> = Vec::new();
    if let Ok(paths) = glob::glob(&pattern) {
        for entry in paths.flatten() {
            let Ok(meta) = std::fs::metadata(&entry) else { continue };
            let Ok(mtime) = meta.modified() else { continue };
            let mtime = mtime
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            candidates.push((entry, mtime, false));
        }
    }
    for (path, mtime) in crate::codex::candidates() {
        candidates.push((path, mtime, true));
    }

    // Sort key: the focused session (Claude via its focus sidecar, or Codex via its
    // desktop-app DB) is pinned to the very top; everything else ranks by mtime.
    let sort_key = |p: &Path, mtime: u64, is_codex: bool| -> u64 {
        let focused_here = if is_codex {
            codex_focused
                .as_deref()
                .map(|f| crate::codex::id_from_path(p) == f)
                .unwrap_or(false)
        } else {
            focused
                .as_deref()
                .map(|f| p.file_stem().map(|s| s == f).unwrap_or(false))
                .unwrap_or(false)
        };
        if focused_here {
            u64::MAX
        } else {
            mtime
        }
    };
    candidates.sort_by(|a, b| sort_key(&b.0, b.1, b.2).cmp(&sort_key(&a.0, a.1, a.2)));
    // The window's own height decides how many rows actually render (App.svelte's
    // `fit()` grows to the pool then trims to fit), so there is no user-facing count.
    // This is only a safety ceiling so a machine with hundreds of transcripts doesn't
    // build every one each poll; no realistic window shows this many rows.
    const MAX_POOL: usize = 50;
    candidates.truncate(MAX_POOL);

    let mut sessions: Vec<Session> = candidates
        .into_iter()
        .map(|(path, mtime, is_codex)| {
            if is_codex {
                crate::codex::build_session(&path, mtime, now, cfg, codex_focused.as_deref())
            } else {
                build_session(&path, mtime, now, cfg, labels, focused.as_deref())
            }
        })
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

    // One user-set target drives the gauge budget for every session/harness
    // (comparable across Claude and Codex); the model still sets the hard window
    // and label. Unknown model falls back to that same target for the window.
    let mi = t.model.as_deref().map(model_info);
    let target = cfg.target_tokens;
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
        // Grow until we have BOTH the current context AND the newest last-prompt,
        // or we've read from byte 0 (nothing more to grow into). ctx sits at the
        // very end, but a single turn whose output exceeds the window pushes that
        // turn's last-prompt (written when the prompt landed) back past a tail
        // sized for ctx alone -- so without also waiting for the subtitle we'd
        // show a stale prompt. Every session with a prompt has a last-prompt, so
        // this terminates near EOF rather than forcing a full read.
        if (out.ctx.is_some() && out.subtitle.is_some()) || start == 0 {
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

/// Lowercased file name of the process that owns the OS foreground window
/// (e.g. `"chatgpt.exe"`, `"claude.exe"`, `"greedout.exe"`), or None if it can't be
/// determined. Used to tell which app the user just tabbed to when no thread was
/// selected. Win32 declared inline to avoid pulling in the whole `windows` crate.
#[cfg(windows)]
fn foreground_exe() -> Option<String> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    }
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return None;
        }
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h == 0 {
            return None;
        }
        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(h);
        if ok == 0 {
            return None;
        }
        let path = std::ffi::OsString::from_wide(&buf[..size as usize]);
        Some(
            std::path::Path::new(&path)
                .file_name()?
                .to_string_lossy()
                .to_lowercase(),
        )
    }
}

#[cfg(not(windows))]
fn foreground_exe() -> Option<String> {
    None
}

/// Returns `(session id, lastFocusedAt in ms)` so the caller can compare Claude
/// focus freshness against Codex's and pick a single globally-focused session.
fn focused_session_id() -> Option<(String, u64)> {
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
    best.map(|(t, id)| (id, t))
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
        // Not a Claude transcript: it may be a Codex session (different id space
        // and on-disk format), so let the Codex adapter reconstruct the curve.
        return crate::codex::history(id);
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
    /// Session title (custom title or user label); empty if not yet known.
    pub title: String,
    /// Session's overall last-activity time (transcript mtime), epoch millis.
    pub mtime: i64,
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
pub(crate) fn downsample(v: Vec<Sample>, max: usize) -> Vec<Sample> {
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
pub(crate) fn parse_model_version(id: &str) -> String {
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
