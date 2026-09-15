//! Baseline-context analysis, grounded in the real `/context` snapshot.
//!
//! When the user runs `/context` in a session, the CLI writes a structured
//! `contextUsage` object into the transcript (a `type:"system",
//! subtype:"local_command"` record). It holds the exact token breakdown the
//! `/context` screen shows: per-category totals, a per-tool MCP list, per-file
//! memory sizes, and per-skill sizes. We parse the newest such record and turn
//! it into the treemap -- no estimates, no reconciliation.
//!
//! If a session has never run `/context`, there is nothing to read: the schemas
//! only exist inside a live session. In that case we return `needs_context` and
//! the UI asks the user to type `/context` and re-check.

use crate::scan;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read as _;

/// One node of the baseline treemap. If `children` is non-empty they sum to
/// `tokens`. `kind` groups tiles for coloring/labelling.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub name: String,
    pub tokens: u64,
    pub kind: String,
    pub detail: String,
    /// Stable id of a Chat message block, for the click-to-open full-content
    /// panel (`chat_block`). 0 for every non-chat-leaf node.
    #[serde(default)]
    pub gid: u64,
    /// ISO wall-clock of a Chat block/turn (its transcript line's `timestamp`),
    /// for the "when" shown on sequential rects. Empty for non-chat nodes.
    #[serde(default)]
    pub ts: String,
    pub children: Vec<Node>,
}

impl Node {
    fn leaf(name: &str, tokens: u64, kind: &str, detail: &str) -> Node {
        Node { name: name.into(), tokens, kind: kind.into(), detail: detail.into(), gid: 0, ts: String::new(), children: vec![] }
    }
}

/// The full analysis for one session.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub ok: bool,
    pub message: String,
    /// True when the session has no `/context` snapshot yet -- the UI then shows
    /// the "type /context" instructions instead of a treemap.
    pub needs_context: bool,
    /// Total context tokens at the snapshot (the `/context` "Tokens" figure).
    pub total: u64,
    /// The hard context window (raw_max_tokens).
    pub max: u64,
    /// Wall-clock of the snapshot, ISO string, for "as of ..." display.
    pub captured_at: String,
    /// Estimated USD per context token (session model's cache-read price), so the
    /// Context Explorer can show a rough dollar figure on each rect.
    #[serde(default)]
    pub cost_rate: f64,
    /// Per-token USD input rate (fed-in blocks: user prompts, tool results).
    #[serde(default)]
    pub cost_in: f64,
    /// Per-token USD output rate (generated blocks: agent text, thinking, tool calls).
    #[serde(default)]
    pub cost_out: f64,
    pub nodes: Vec<Node>,
}

impl Report {
    fn err(message: &str) -> Report {
        Report {
            ok: false,
            message: message.into(),
            needs_context: false,
            total: 0,
            max: 0,
            captured_at: String::new(),
            cost_rate: 0.0,
            cost_in: 0.0,
            cost_out: 0.0,
            nodes: vec![],
        }
    }
    fn needs() -> Report {
        Report {
            ok: true,
            message: String::new(),
            needs_context: true,
            total: 0,
            max: 0,
            captured_at: String::new(),
            cost_rate: 0.0,
            cost_in: 0.0,
            cost_out: 0.0,
            nodes: vec![],
        }
    }
}

// --- Shape of the transcript's `contextUsage` object (only the fields we use). ---

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "contextUsage")]
    context_usage: Option<ContextUsage>,
    timestamp: Option<String>,
}

#[derive(Deserialize)]
struct ContextUsage {
    total_tokens: u64,
    raw_max_tokens: u64,
    #[serde(default)]
    categories: Vec<Category>,
    #[serde(default)]
    mcp_tools: Vec<McpTool>,
    #[serde(default)]
    memory_files: Vec<MemoryFile>,
    #[serde(default)]
    skills: Vec<SkillItem>,
}

#[derive(Deserialize)]
struct Category {
    name: String,
    tokens: u64,
}

#[derive(Deserialize)]
struct McpTool {
    name: String,
    server_name: String,
    tokens: u64,
}

#[derive(Deserialize)]
struct MemoryFile {
    path: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    tokens: u64,
}

#[derive(Deserialize)]
struct SkillItem {
    name: String,
    #[serde(default)]
    source: Option<String>,
    tokens: u64,
}

/// Analyze one session's context, from its newest `/context` snapshot.
pub fn analyze(id: &str) -> Report {
    // Codex sessions have no `/context` snapshot and no `contextUsage` object, so
    // there is nothing to read verbatim: their tree is ESTIMATED from on-disk
    // content and reconciled to the real token count (see `analyze_codex`).
    if is_codex(id) {
        return analyze_codex(id);
    }
    if is_cursor(id) {
        return analyze_cursor(id);
    }
    let Some(path) = scan::transcript_path(id) else {
        return Report::err("No transcript found for this session yet.");
    };
    let (cost_in, cost_out, cost_rate) = scan::context_price_rates(id);
    let Some((cu, ts)) = latest_context_usage(&path) else {
        return Report { cost_rate, cost_in, cost_out, ..Report::needs() };
    };

    let mut nodes: Vec<Node> = Vec::new();

    // Category tokens by name, for the buckets that have no per-item list.
    let cat = |name: &str| cu.categories.iter().find(|c| c.name == name).map(|c| c.tokens);

    // --- System prompt (leaf) ---
    if let Some(t) = cat("System prompt") {
        if t > 0 {
            nodes.push(Node::leaf("System prompt", t, "core", "Claude Code's base instructions"));
        }
    }

    // --- Built-in tools: loaded + deferred (no per-tool list is provided) ---
    let sys_loaded = cat("System tools").unwrap_or(0);
    let sys_deferred = cat("System tools (deferred)").unwrap_or(0);
    if sys_loaded + sys_deferred > 0 {
        let mut kids = vec![];
        if sys_loaded > 0 {
            kids.push(Node::leaf("Loaded", sys_loaded, "core", "active built-in tool schemas"));
        }
        if sys_deferred > 0 {
            kids.push(Node::leaf(
                "Deferred",
                sys_deferred,
                "deferred",
                "loaded on demand, not counted until used",
            ));
        }
        nodes.push(Node {
            name: "Built-in tools".into(),
            tokens: sys_loaded + sys_deferred,
            kind: "core".into(),
            detail: "native tool schemas (Read, Edit, Bash, …)".into(),
            gid: 0,
            ts: String::new(),
            children: kids,
        });
    }

    // --- MCP tools: real per-server / per-tool breakdown from the tool list ---
    if !cu.mcp_tools.is_empty() {
        // Group by server, summing, and keep each server's tools as children.
        let mut servers: Vec<(String, Vec<Node>, u64)> = Vec::new();
        for t in &cu.mcp_tools {
            let short = short_tool_name(&t.name, &t.server_name);
            let leaf = Node::leaf(&short, t.tokens, "mcp", &t.name);
            match servers.iter_mut().find(|(s, _, _)| *s == t.server_name) {
                Some((_, kids, sum)) => {
                    kids.push(leaf);
                    *sum += t.tokens;
                }
                None => servers.push((t.server_name.clone(), vec![leaf], t.tokens)),
            }
        }
        let mut server_nodes: Vec<Node> = servers
            .into_iter()
            .map(|(name, mut kids, sum)| {
                kids.sort_by(|a, b| b.tokens.cmp(&a.tokens));
                Node {
                    name: pretty_server(&name),
                    tokens: sum,
                    kind: "mcp".into(),
                    detail: format!("{} tools", kids.len()),
                    gid: 0,
                    ts: String::new(),
                    children: kids,
                }
            })
            .collect();
        server_nodes.sort_by(|a, b| b.tokens.cmp(&a.tokens));
        let mcp_total: u64 = server_nodes.iter().map(|n| n.tokens).sum();
        nodes.push(Node {
            name: "MCP tools".into(),
            tokens: mcp_total,
            kind: "mcp".into(),
            detail: "connected server tool schemas (most load lazily)".into(),
            gid: 0,
            ts: String::new(),
            children: server_nodes,
        });
    }

    // --- Memory files: per-file ---
    if !cu.memory_files.is_empty() {
        let mut kids: Vec<Node> = cu
            .memory_files
            .iter()
            .map(|m| {
                let label = m.kind.clone().unwrap_or_else(|| file_name(&m.path));
                Node::leaf(&label, m.tokens, "measured", &m.path)
            })
            .collect();
        kids.sort_by(|a, b| b.tokens.cmp(&a.tokens));
        let sum: u64 = kids.iter().map(|n| n.tokens).sum();
        nodes.push(Node {
            name: "Memory & instructions".into(),
            tokens: sum,
            kind: "measured".into(),
            detail: "CLAUDE.md and memory files".into(),
            gid: 0,
            ts: String::new(),
            children: kids,
        });
    }

    // --- Skills ---
    if !cu.skills.is_empty() {
        let mut kids: Vec<Node> = cu
            .skills
            .iter()
            .map(|s| {
                let d = s.source.clone().unwrap_or_default();
                Node::leaf(&s.name, s.tokens, "measured", &d)
            })
            .collect();
        kids.sort_by(|a, b| b.tokens.cmp(&a.tokens));
        let sum: u64 = kids.iter().map(|n| n.tokens).sum();
        nodes.push(Node {
            name: "Skills".into(),
            tokens: sum,
            kind: "measured".into(),
            detail: "available skill descriptions".into(),
            gid: 0,
            ts: String::new(),
            children: kids,
        });
    }

    // --- Messages: the conversation so far (not baseline, but part of the total) ---
    if let Some(t) = cat("Messages") {
        if t > 0 {
            nodes.push(Node::leaf(
                "Messages",
                t,
                "messages",
                "the conversation so far (grows over time)",
            ));
        }
    }

    // Keep injected-context sequence order (System prompt → Built-in tools → MCP
    // tools → Memory & instructions → Skills → Messages) for the Context Explorer
    // column layout; do NOT sort top-level nodes by tokens.
    Report {
        ok: true,
        message: String::new(),
        needs_context: false,
        total: cu.total_tokens,
        max: cu.raw_max_tokens,
        captured_at: ts,
        cost_rate,
        cost_in,
        cost_out,
        nodes,
    }
}

/// One message block within a turn. `full` is the complete content, kept so the
/// click-to-open panel can show it verbatim; the tree only stores a preview.
struct Blk {
    turn: u32,
    /// Short tile label, e.g. "User", "Agent", "Thinking", "Tool: Edit", "Tool Result".
    label: String,
    /// The block's complete content.
    full: String,
    /// ISO timestamp of the transcript line this block came from (may be empty).
    ts: String,
}

/// Parse a session's conversation (from the last /compact boundary) into an
/// ordered list of message blocks. The index of a block in this Vec is its stable
/// `gid`, used by `chat_block` to fetch full content on click. `chat_breakdown`
/// groups the SAME list into the Turn tree, so gids line up exactly.
fn parse_blocks(id: &str) -> Vec<Blk> {
    let Some(path) = scan::transcript_path(id) else { return vec![] };
    let mut s = String::new();
    if std::fs::File::open(&path).and_then(|mut f| f.read_to_string(&mut s)).is_err() {
        return vec![];
    }
    let lines: Vec<&str> = s.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.contains("compact_boundary"))
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut blocks: Vec<Blk> = Vec::new();
    let mut turn: u32 = 0;
    let mut push = |turn: u32, label: String, full: String, ts: &str| {
        if !full.is_empty() {
            blocks.push(Blk { turn, label, full, ts: ts.to_string() });
        }
    };

    for line in &lines[start..] {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let line_ts = v.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let content = v.get("message").and_then(|m| m.get("content"));
        match kind {
            "assistant" => {
                if let Some(arr) = content.and_then(Value::as_array) {
                    for b in arr {
                        let bt = b.get("type").and_then(Value::as_str).unwrap_or("");
                        match bt {
                            "text" => {
                                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                push(turn, "Agent".into(), t.to_string(), &line_ts);
                            }
                            "thinking" => {
                                let t = b.get("thinking").and_then(Value::as_str).unwrap_or("");
                                push(turn, "Thinking".into(), t.to_string(), &line_ts);
                            }
                            "tool_use" => {
                                let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                                let input = b
                                    .get("input")
                                    .map(|i| serde_json::to_string_pretty(i).unwrap_or_else(|_| i.to_string()))
                                    .unwrap_or_default();
                                push(turn, format!("Tool: {name}"), input, &line_ts);
                            }
                            _ => {}
                        }
                    }
                }
            }
            "user" => {
                // A turn begins at a real user prompt (text), not a tool_result-only
                // message (those belong to the assistant's ongoing turn).
                let str_content = content.and_then(Value::as_str);
                let arr = content.and_then(Value::as_array);
                let has_text = str_content.map(|s| !s.trim().is_empty()).unwrap_or(false)
                    || arr
                        .map(|a| a.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("text")))
                        .unwrap_or(false);
                if has_text {
                    turn += 1;
                }
                if let Some(txt) = str_content {
                    push(turn, "User".into(), txt.to_string(), &line_ts);
                } else if let Some(arr) = arr {
                    for b in arr {
                        let bt = b.get("type").and_then(Value::as_str).unwrap_or("");
                        match bt {
                            "text" => {
                                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                push(turn, "User".into(), t.to_string(), &line_ts);
                            }
                            "tool_result" => {
                                let c = b.get("content").map(json_text).unwrap_or_default();
                                push(turn, "Tool Result".into(), c, &line_ts);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    blocks
}

/// Estimated breakdown of the Chat (Messages) bucket, as a nested tree:
/// Turn N -> individual message blocks (User / Agent / Thinking / Tool: X /
/// Tool Result), each carrying a one-line preview and a stable `gid` so a click can
/// open its full content. The `/context` snapshot only gives Chat as one number,
/// so token sizes are ESTIMATED from text length (chars/4) and the frontend
/// rescales them to the live Chat total.
pub fn chat_breakdown(id: &str) -> Vec<Node> {
    let blocks = blocks_for(id);
    const CHARS_PER_TOKEN: f64 = 4.0;
    let est = |chars: usize| (chars as f64 / CHARS_PER_TOKEN).round() as u64;

    // Group into turn -> [(gid, block)], preserving parse order.
    let mut turns: BTreeMap<u32, Vec<(u64, &Blk)>> = BTreeMap::new();
    for (gid, b) in blocks.iter().enumerate() {
        turns.entry(b.turn).or_default().push((gid as u64, b));
    }

    let turn_nodes: Vec<Node> = turns
        .into_iter()
        .map(|(turn, items)| {
            // The turn's tile label snippet = its first "You" block.
            let snippet = items
                .iter()
                .find(|(_, b)| b.label == "User")
                .map(|(_, b)| preview(&b.full))
                .unwrap_or_default();
            // Turn's timestamp = its first block's (turn-start wall-clock).
            let turn_ts = items.first().map(|(_, b)| b.ts.clone()).unwrap_or_default();
            let mut leaves: Vec<Node> = items
                .into_iter()
                .map(|(gid, b)| Node {
                    name: b.label.clone(),
                    tokens: est(b.full.len()),
                    kind: "messages".into(),
                    detail: preview(&b.full),
                    gid,
                    ts: b.ts.clone(),
                    children: vec![],
                })
                .collect();
            // Sequence order: leaves stay in parse order (ascending gid). The
            // Context Explorer's column layout renders them in reading order, so
            // no token-descending sort here (the layout is deliberately in sequence order).
            leaves.sort_by(|a, b| a.gid.cmp(&b.gid));
            let sum = leaves.iter().map(|n| n.tokens).sum();
            let name = if turn == 0 { "Pre-turn".to_string() } else { format!("Turn {turn}") };
            Node {
                name,
                tokens: sum,
                kind: "messages".into(),
                detail: snippet,
                gid: 0,
                ts: turn_ts,
                children: leaves,
            }
        })
        .collect();
    // Turns already iterate the BTreeMap in ascending turn order (= chronological
    // sequence). Keep it; the column layout renders turns in reading order.
    turn_nodes
}

/// The turn number of the FIRST message block containing `query` (case-insensitive),
/// using the same block/turn parse as `chat_breakdown` so the number lines up with
/// the "Turn N" tiles. `None` if nothing matches. Lets the Context Explorer
/// highlight the turn a search hit came from.
pub fn search_first_turn(id: &str, query: &str) -> Option<u32> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return None;
    }
    blocks_for(id)
        .into_iter()
        .find(|b| b.full.to_lowercase().contains(&q))
        .map(|b| b.turn)
}

/// Full content of one Chat message block, by its stable `gid` (see
/// `chat_breakdown`). Returns `(label, content)`; empty label if out of range.
pub fn chat_block(id: &str, gid: u64) -> (String, String) {
    blocks_for(id)
        .into_iter()
        .nth(gid as usize)
        .map(|b| (b.label, b.full))
        .unwrap_or_default()
}

// --- Codex (OpenAI) branch --------------------------------------------------
//
// Codex rollouts carry no `/context` snapshot, so the Context Explorer can't read
// an exact breakdown the way it does for Claude. Instead we ESTIMATE each bucket
// from on-disk content (chars/4, the same crude tokens-per-char the Chat estimate
// uses) and RECONCILE: the newest `token_usage_record` gives the real context
// occupancy, so we scale every estimate by one factor that makes the tree's grand
// total land exactly on that real figure. Fixed buckets (System prompt, World
// state) plus the reconciled Messages remainder sum to the real `input_tokens`,
// mirroring how the Claude path lets real category totals stand while the Chat
// tree is rescaled to the live Messages figure.

/// True when `id` names a Codex rollout rather than a Claude transcript. Claude
/// resolves through `scan::transcript_path`; anything that doesn't, but does match
/// a Codex rollout stem, takes the estimated path.
fn is_codex(id: &str) -> bool {
    scan::transcript_path(id).is_none()
        && crate::codex::candidates().iter().any(|(p, _)| crate::codex::id_from_path(p) == id)
}

/// The rollout path for a Codex session id (the newest, per `codex::candidates`).
fn codex_path(id: &str) -> Option<std::path::PathBuf> {
    crate::codex::candidates()
        .into_iter()
        .find(|(p, _)| crate::codex::id_from_path(p) == id)
        .map(|(p, _)| p)
}

/// The message blocks for a session, from whichever harness owns the id. Keeps
/// `chat_breakdown` / `search_first_turn` / `chat_block` harness-agnostic: they all
/// operate on the same `Blk` list, so gids (the Vec index) line up regardless of
/// source.
fn blocks_for(id: &str) -> Vec<Blk> {
    if is_codex(id) {
        parse_blocks_codex(id)
    } else if is_cursor(id) {
        parse_blocks_cursor(id)
    } else {
        parse_blocks(id)
    }
}

/// A Cursor composer id: not a Claude transcript, but a known Cursor composer.
fn is_cursor(id: &str) -> bool {
    scan::transcript_path(id).is_none()
        && crate::cursor::candidates().iter().any(|(p, _)| crate::cursor::id_from_path(p) == id)
}

/// A Cursor composer's messages as ordered blocks. Cursor exposes only user/assistant
/// bubbles (no thinking/tool split), so every non-empty bubble is one User or Agent
/// block; a new turn begins at each user bubble. Timestamps are left empty (Cursor's
/// per-bubble times are unreliable; the tile falls back to the row's own time).
fn parse_blocks_cursor(id: &str) -> Vec<Blk> {
    let mut blocks = Vec::new();
    let mut turn: u32 = 0;
    for (is_user, text, _ts) in crate::cursor::chat_blocks(id) {
        if is_user {
            turn += 1;
        }
        let label = if is_user { "User" } else { "Agent" };
        blocks.push(Blk { turn, label: label.into(), full: text, ts: String::new() });
    }
    blocks
}

/// Context Explorer analysis for a Cursor composer. Cursor gives a real context-fill
/// total (`contextTokensUsed`) but no per-category breakdown and no system-prompt or
/// world-state visibility, so the tree is a single estimated Messages node (chars/4
/// per bubble) reconciled to that real total, mirroring `analyze_codex`'s reconcile.
fn analyze_cursor(id: &str) -> Report {
    let Some((ctx, limit, model, created)) = crate::cursor::baseline_meter(id) else {
        return Report::err("No Cursor conversation found for this session yet.");
    };
    let mi = scan::model_info(model.as_deref().unwrap_or(""));
    let cost_in = mi.price_in / 1_000_000.0;
    let cost_out = mi.price_out / 1_000_000.0;
    let cost_rate = mi.price_cache_read / 1_000_000.0;

    let blocks = parse_blocks_cursor(id);
    let chat_est: u64 = blocks.iter().map(|b| est_tokens(b.full.len())).sum();

    // Reconcile the estimated chat to Cursor's real meter when present; otherwise the
    // raw estimate stands (factor 1) so the tree still shows something.
    let factor = match ctx {
        Some(t) if chat_est > 0 => t as f64 / chat_est as f64,
        _ => 1.0,
    };
    let scale = |t: u64| (t as f64 * factor).round() as u64;
    let total = ctx.unwrap_or(chat_est);
    let max = limit.unwrap_or(mi.context_max);

    let mut nodes: Vec<Node> = Vec::new();
    if chat_est > 0 {
        nodes.push(Node::leaf(
            "Messages",
            scale(chat_est),
            "messages",
            "the conversation so far (grows over time)",
        ));
    }

    Report {
        ok: true,
        message: String::new(),
        needs_context: false,
        total,
        max,
        // Cursor's per-composer time is unreliable; leave "as of" blank rather than
        // stamp a misleading instant.
        captured_at: {
            let _ = created;
            String::new()
        },
        cost_rate,
        cost_in,
        cost_out,
        nodes,
    }
}

/// Cheap `chars/4` token estimate, shared by every Codex bucket.
fn est_tokens(chars: usize) -> u64 {
    (chars as f64 / 4.0).round() as u64
}

/// The top-level `type` of a Codex rollout line, parsed only after a cheap
/// `contains` pre-check has already matched (mirrors codex.rs's `is_record_type`).
fn codex_line_type(line: &str) -> Option<String> {
    serde_json::from_str::<Value>(line)
        .ok()?
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Join a Codex `content` array's text parts. UserMessage uses `{type:"text"}` and
/// AgentMessage uses `{type:"Text"}`, so we just collect every `text` field rather
/// than switching on the case-varying `type`.
fn codex_content_text(content: &Value) -> String {
    content
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Join a Codex string array (e.g. Reasoning `summary_text`), accepting either bare
/// strings or `{text}` objects.
fn codex_string_array(v: Option<&Value>) -> String {
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

/// Turn one Codex `item_completed` item into a `(label, full text)` block, using the
/// desktop app's PLAINTEXT processed view. Returns `None` for item types with no
/// display text (the encrypted `response_item/reasoning` is deliberately never used
/// here -- only these `event_msg` plaintext items). `is_user` tells the caller to
/// start a new turn before pushing.
fn codex_item_block(item: &Value) -> Option<(String, String, bool)> {
    let it = item.get("type").and_then(Value::as_str)?;
    match it {
        "UserMessage" => {
            let full = codex_content_text(item.get("content").unwrap_or(&Value::Null));
            Some(("User".into(), full, true))
        }
        "AgentMessage" => {
            let full = codex_content_text(item.get("content").unwrap_or(&Value::Null));
            Some(("Agent".into(), full, false))
        }
        "Reasoning" => {
            let mut full = codex_string_array(item.get("summary_text"));
            let raw = codex_string_array(item.get("raw_content"));
            if !raw.is_empty() {
                if !full.is_empty() {
                    full.push('\n');
                }
                full.push_str(&raw);
            }
            Some(("Thinking".into(), full, false))
        }
        "CommandExecution" => {
            // Prefer the parsed command (human form, e.g. "Get-ChildItem ...") over the
            // raw argv whose head is the runtime's pwsh.exe path.
            let cmd = item
                .get("parsed_cmd")
                .and_then(Value::as_array)
                .and_then(|a| a.first())
                .and_then(|c| c.get("cmd").and_then(Value::as_str))
                .map(str::to_string)
                .unwrap_or_else(|| {
                    item.get("command")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")
                        })
                        .unwrap_or_default()
                });
            let head = cmd.split_whitespace().next().unwrap_or("cmd");
            let exit = item.get("exit_code").and_then(Value::as_i64);
            let stdout = item.get("stdout").and_then(Value::as_str).unwrap_or("");
            let mut full = format!("$ {cmd}");
            if let Some(code) = exit {
                full.push_str(&format!("\n(exit {code})"));
            }
            if !stdout.is_empty() {
                full.push('\n');
                full.push_str(stdout);
            }
            Some((format!("Tool: {head}"), full, false))
        }
        "FileChange" => {
            // `changes` maps each touched path to `{type, content}`.
            let mut full = String::new();
            if let Some(obj) = item.get("changes").and_then(Value::as_object) {
                for (path, change) in obj {
                    let kind = change.get("type").and_then(Value::as_str).unwrap_or("edit");
                    full.push_str(&format!("{kind} {path}\n"));
                    if let Some(c) = change.get("content").and_then(Value::as_str) {
                        full.push_str(c);
                        full.push('\n');
                    }
                }
            }
            Some(("Tool: edit".into(), full, false))
        }
        "SubAgentActivity" => {
            let kind = item.get("kind").and_then(Value::as_str).unwrap_or("");
            let agent = item.get("agent_path").and_then(Value::as_str).unwrap_or("");
            Some(("Subagent".into(), format!("{kind} {agent}").trim().to_string(), false))
        }
        _ => None,
    }
}

/// Parse a Codex rollout's conversation into the same ordered `Blk` list as the
/// Claude `parse_blocks`, so gids line up for `chat_block` / `search_first_turn`.
/// Starts after the LAST top-level `compacted` record (compaction resets the
/// window, the analog of Claude's `compact_boundary` split), and emits one block per
/// `event_msg`/`item_completed` plaintext item, incrementing the turn on each
/// UserMessage.
fn parse_blocks_codex(id: &str) -> Vec<Blk> {
    let Some(path) = codex_path(id) else { return vec![] };
    let mut s = String::new();
    if std::fs::File::open(&path).and_then(|mut f| f.read_to_string(&mut s)).is_err() {
        return vec![];
    }
    let lines: Vec<&str> = s.lines().collect();
    let start = lines
        .iter()
        .rposition(|l| l.contains("\"compacted\"") && codex_line_type(l).as_deref() == Some("compacted"))
        .map(|i| i + 1)
        .unwrap_or(0);

    let mut blocks: Vec<Blk> = Vec::new();
    let mut turn: u32 = 0;
    for line in &lines[start..] {
        if !line.contains("\"item_completed\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let payload = v.get("payload");
        if payload.and_then(|p| p.get("type")).and_then(Value::as_str) != Some("item_completed") {
            continue;
        }
        let line_ts = v.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
        let Some(item) = payload.and_then(|p| p.get("item")) else { continue };
        let Some((label, full, is_user)) = codex_item_block(item) else { continue };
        if is_user {
            turn += 1;
        }
        if !full.is_empty() {
            blocks.push(Blk { turn, label, full, ts: line_ts });
        }
    }
    blocks
}

/// The estimation inputs pulled from one whole-file read of a Codex rollout.
#[derive(Default)]
struct CodexMeta {
    /// `session_meta.payload.base_instructions.text` -- the system prompt body.
    system_prompt: String,
    /// The project dir (`session_meta.payload.cwd`), for the system-prompt detail.
    cwd: String,
    /// Newest `world_state` `full` state map, component -> its JSON body length.
    world: Vec<(String, usize)>,
    /// Newest `token_usage_record.payload.usage.input_tokens` -- real occupancy.
    input_tokens: Option<u64>,
    /// That record's ISO `timestamp`, for "as of ...".
    captured_at: String,
    /// The enforced per-session window (`model_context_window`).
    window: Option<u64>,
    /// Newest `payload.model`, for pricing.
    model: Option<String>,
}

/// One whole-file pass over a Codex rollout collecting everything `analyze_codex`
/// needs. Newest-wins for the single-valued fields (we overwrite as we go so the
/// last occurrence -- nearest EOF -- stands).
fn codex_meta(path: &std::path::Path) -> CodexMeta {
    let mut s = String::new();
    if std::fs::File::open(path).and_then(|mut f| f.read_to_string(&mut s)).is_err() {
        return CodexMeta::default();
    }
    let mut m = CodexMeta::default();
    for line in s.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = v.get("payload");
        match ty {
            "session_meta" => {
                if let Some(p) = payload {
                    if let Some(t) = p.get("base_instructions").and_then(|b| b.get("text")).and_then(Value::as_str) {
                        m.system_prompt = t.to_string();
                    }
                    if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                        m.cwd = c.to_string();
                    }
                }
            }
            "world_state" => {
                // Only a `full` snapshot lists every component; skip partial deltas.
                if let Some(p) = payload {
                    if p.get("full").and_then(Value::as_bool) == Some(true) {
                        if let Some(state) = p.get("state").and_then(Value::as_object) {
                            m.world = state
                                .iter()
                                .map(|(k, val)| (k.clone(), val.to_string().len()))
                                .collect();
                        }
                    }
                }
            }
            "token_usage_record" => {
                if let Some(usage) = payload.and_then(|p| p.get("usage")) {
                    if let Some(ctx) = usage.get("input_tokens").and_then(Value::as_u64) {
                        m.input_tokens = Some(ctx);
                        m.captured_at =
                            v.get("timestamp").and_then(Value::as_str).unwrap_or("").to_string();
                    }
                }
            }
            _ => {}
        }
        // `model_context_window` rides on `task_started` (payload.model_context_window)
        // and `token_count` (payload.info.model_context_window); both report the same
        // enforced window, so accept whichever appears.
        if let Some(p) = payload {
            if let Some(w) = p
                .get("model_context_window")
                .or_else(|| p.get("info").and_then(|i| i.get("model_context_window")))
                .and_then(Value::as_u64)
                .filter(|w| *w > 0)
            {
                m.window = Some(w);
            }
            if let Some(model) = p.get("model").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                m.model = Some(model.to_string());
            }
        }
    }
    m
}

/// Estimated + reconciled context tree for a Codex session. Builds the top-level
/// nodes in injected-context sequence order -- System prompt, World state, Messages
/// -- from `chars/4` estimates, then scales them all by one factor so the grand
/// total equals the real `input_tokens`. `max` is the session's enforced window,
/// `captured_at` the newest usage record's timestamp; cost rates come from the
/// session's own model (`scan::context_price_rates` is Claude-only, so we price
/// Codex directly off `scan::model_info`).
fn analyze_codex(id: &str) -> Report {
    let Some(path) = codex_path(id) else {
        return Report::err("No Codex rollout found for this session yet.");
    };
    let m = codex_meta(&path);

    // Per-role USD rates from the session's model (mirrors context_price_rates, which
    // only resolves Claude ids). Fed-in blocks price at input, generated at output,
    // carried baseline context at cache-read.
    let mi = scan::model_info(m.model.as_deref().unwrap_or(""));
    let cost_in = mi.price_in / 1_000_000.0;
    let cost_out = mi.price_out / 1_000_000.0;
    let cost_rate = mi.price_cache_read / 1_000_000.0;

    // --- Raw estimates (chars/4) for each bucket, before reconciliation. ---
    let sys_est = est_tokens(m.system_prompt.chars().count());

    // World-state components worth their own tile: drop the tiny bool/flag entries so
    // the node isn't cluttered with 1-token leaves (host_skills body, environments and
    // the like carry the real weight).
    let mut world_leaves: Vec<(String, u64)> = m
        .world
        .iter()
        .map(|(k, len)| (k.clone(), est_tokens(*len)))
        .filter(|(_, t)| *t >= 10)
        .collect();
    world_leaves.sort_by(|a, b| b.1.cmp(&a.1));
    let world_est: u64 = world_leaves.iter().map(|(_, t)| *t).sum();

    let chat_blocks = parse_blocks_codex(id);
    let chat_est: u64 = chat_blocks.iter().map(|b| est_tokens(b.full.len())).sum();

    let raw_total = sys_est + world_est + chat_est;

    // Reconcile: scale every estimate so the tree's grand total lands on the real
    // occupancy. With no usage record yet, fall back to the raw estimates (factor 1)
    // so the tree still shows something.
    let factor = match m.input_tokens {
        Some(t) if raw_total > 0 => t as f64 / raw_total as f64,
        _ => 1.0,
    };
    let scale = |t: u64| (t as f64 * factor).round() as u64;
    let total = m.input_tokens.unwrap_or(raw_total);
    let max = m.window.unwrap_or(mi.context_max);

    let mut nodes: Vec<Node> = Vec::new();

    // --- System prompt (leaf) ---
    if sys_est > 0 {
        let detail = if m.cwd.is_empty() {
            "Codex base instructions".to_string()
        } else {
            format!("Codex base instructions ({})", m.cwd)
        };
        nodes.push(Node::leaf("System prompt", scale(sys_est), "core", &detail));
    }

    // --- World state (system context: skills, environments, permissions, …) ---
    if !world_leaves.is_empty() {
        let kids: Vec<Node> = world_leaves
            .iter()
            .map(|(k, t)| Node::leaf(&pretty_component(k), scale(*t), "measured", k))
            .collect();
        let sum: u64 = kids.iter().map(|n| n.tokens).sum();
        nodes.push(Node {
            name: "World state".into(),
            tokens: sum,
            kind: "measured".into(),
            detail: "injected skills, environment and permissions".into(),
            gid: 0,
            ts: String::new(),
            children: kids,
        });
    }

    // --- Messages: the reconciled remainder; the frontend attaches the estimated
    // Turn>block children from `chat_breakdown` and rescales them to this figure. ---
    if chat_est > 0 {
        nodes.push(Node::leaf(
            "Messages",
            scale(chat_est),
            "messages",
            "the conversation so far (grows over time)",
        ));
    }

    Report {
        ok: true,
        message: String::new(),
        needs_context: false,
        total,
        max,
        captured_at: m.captured_at,
        cost_rate,
        cost_in,
        cost_out,
        nodes,
    }
}

/// Prettify a `world_state` component key (`host_skills` -> "host skills").
fn pretty_component(key: &str) -> String {
    key.replace('_', " ")
}

/// Collapse a content block to a one-line preview: trim, flatten whitespace, cap.
fn preview(s: &str) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 300;
    if flat.chars().count() > MAX {
        let cut: String = flat.chars().take(MAX).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// Text of a tool_result `content` field, which may be a string or an array of
/// `{type:"text", text}` blocks. Falls back to the raw JSON.
fn json_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(arr) = v.as_array() {
        let mut out = String::new();
        for b in arr {
            if let Some(t) = b.get("text").and_then(Value::as_str) {
                out.push_str(t);
                out.push(' ');
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    v.to_string()
}

/// Scan the whole transcript for the LAST line carrying a `contextUsage`, and
/// return it with the line's timestamp. Cheap string prefilter before parsing.
fn latest_context_usage(path: &std::path::Path) -> Option<(ContextUsage, String)> {
    let mut s = String::new();
    std::fs::File::open(path).ok()?.read_to_string(&mut s).ok()?;
    let mut found: Option<(ContextUsage, String)> = None;
    for line in s.lines() {
        if !line.contains("\"contextUsage\"") {
            continue;
        }
        if let Ok(l) = serde_json::from_str::<Line>(line) {
            if let Some(cu) = l.context_usage {
                found = Some((cu, l.timestamp.unwrap_or_default()));
            }
        }
    }
    found
}

/// Strip the `mcp__<server>__` prefix from a tool name for a compact label.
fn short_tool_name(full: &str, server: &str) -> String {
    let prefix = format!("mcp__{server}__");
    full.strip_prefix(&prefix).unwrap_or(full).to_string()
}

/// Turn a raw server id into something readable (opaque hash ids pass through).
fn pretty_server(name: &str) -> String {
    name.replace('_', " ")
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- est_tokens: chars/4, rounded ---

    #[test]
    fn est_tokens_rounds_chars_over_four() {
        assert_eq!(est_tokens(0), 0);
        assert_eq!(est_tokens(4), 1);
        assert_eq!(est_tokens(400), 100);
        // 2/4 = 0.5 rounds away from zero to 1.
        assert_eq!(est_tokens(2), 1);
    }

    // --- codex_line_type: top-level `type` ---

    #[test]
    fn codex_line_type_reads_type_or_none() {
        assert_eq!(
            codex_line_type(r#"{"type":"token_usage_record"}"#).as_deref(),
            Some("token_usage_record")
        );
        // Missing `type` field.
        assert_eq!(codex_line_type(r#"{"payload":{}}"#), None);
        // Not JSON at all.
        assert_eq!(codex_line_type("not json"), None);
    }

    // --- codex_content_text: join `text` parts of an array ---

    #[test]
    fn codex_content_text_joins_text_parts() {
        let v = json!([{"type":"text","text":"hello"},{"type":"Text","text":"world"}]);
        assert_eq!(codex_content_text(&v), "hello\nworld");
        // A part with no `text` field is skipped.
        let mixed = json!([{"text":"keep"},{"foo":"drop"}]);
        assert_eq!(codex_content_text(&mixed), "keep");
    }

    #[test]
    fn codex_content_text_non_array_is_empty() {
        assert_eq!(codex_content_text(&json!({"text":"x"})), "");
        assert_eq!(codex_content_text(&json!(null)), "");
    }

    // --- codex_string_array: bare strings or {text} objects ---

    #[test]
    fn codex_string_array_accepts_strings_and_objects() {
        let strings = json!(["a", "b"]);
        assert_eq!(codex_string_array(Some(&strings)), "a\nb");
        let objs = json!([{"text":"one"}, {"text":"two"}]);
        assert_eq!(codex_string_array(Some(&objs)), "one\ntwo");
        let mixed = json!(["bare", {"text":"obj"}]);
        assert_eq!(codex_string_array(Some(&mixed)), "bare\nobj");
    }

    #[test]
    fn codex_string_array_none_or_non_array_is_empty() {
        assert_eq!(codex_string_array(None), "");
        assert_eq!(codex_string_array(Some(&json!("nope"))), "");
    }

    // --- pretty_component: underscores to spaces ---

    #[test]
    fn pretty_component_replaces_underscores() {
        assert_eq!(pretty_component("host_skills"), "host skills");
        assert_eq!(pretty_component("plain"), "plain");
    }

    // --- preview: flatten whitespace + cap at 300 chars ---

    #[test]
    fn preview_flattens_whitespace() {
        assert_eq!(preview("  hello\n\tworld  "), "hello world");
        assert_eq!(preview(""), "");
    }

    #[test]
    fn preview_caps_oversized_with_ellipsis() {
        let long = "x".repeat(400);
        let p = preview(&long);
        assert_eq!(p.chars().count(), 301, "300 chars + one ellipsis");
        assert!(p.ends_with('\u{2026}'));
    }

    // --- json_text: string, array of {text}, or raw JSON fallback ---

    #[test]
    fn json_text_string_passes_through() {
        assert_eq!(json_text(&json!("plain text")), "plain text");
    }

    #[test]
    fn json_text_array_joins_with_trailing_space() {
        // Each text block is pushed followed by a space.
        assert_eq!(json_text(&json!([{"text":"a"},{"text":"b"}])), "a b ");
    }

    #[test]
    fn json_text_falls_back_to_raw_json() {
        // A number has no string/array shape, so the raw JSON rendering is returned.
        assert_eq!(json_text(&json!(42)), "42");
        // An array with no `text` fields also falls back to raw JSON.
        assert_eq!(json_text(&json!([{"foo":"bar"}])), "[{\"foo\":\"bar\"}]");
    }

    // --- short_tool_name: strip the mcp__<server>__ prefix ---

    #[test]
    fn short_tool_name_strips_prefix() {
        assert_eq!(short_tool_name("mcp__github__create_issue", "github"), "create_issue");
        // No matching prefix leaves the name untouched.
        assert_eq!(short_tool_name("Read", "github"), "Read");
    }

    // --- pretty_server: underscores to spaces ---

    #[test]
    fn pretty_server_replaces_underscores() {
        assert_eq!(pretty_server("my_server"), "my server");
        assert_eq!(pretty_server("opaquehash"), "opaquehash");
    }

    // --- file_name: last path component across both separators ---

    #[test]
    fn file_name_takes_last_component() {
        assert_eq!(file_name("C:\\a\\b\\note.txt"), "note.txt");
        assert_eq!(file_name("/x/y/z.rs"), "z.rs");
        assert_eq!(file_name("bare"), "bare");
    }
}
