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
    let blocks = parse_blocks(id);
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
    parse_blocks(id)
        .into_iter()
        .find(|b| b.full.to_lowercase().contains(&q))
        .map(|b| b.turn)
}

/// Full content of one Chat message block, by its stable `gid` (see
/// `chat_breakdown`). Returns `(label, content)`; empty label if out of range.
pub fn chat_block(id: &str, gid: u64) -> (String, String) {
    parse_blocks(id)
        .into_iter()
        .nth(gid as usize)
        .map(|b| (b.label, b.full))
        .unwrap_or_default()
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
