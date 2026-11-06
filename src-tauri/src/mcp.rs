//! MCP shim. `greedout.exe mcp` speaks the Model Context Protocol over stdio, so an agent finds
//! `mcp__greedout__*` tools with no instructions. It is the same exe as the app: no second
//! binary to build, install or keep in sync. Each tool call opens the running app's control
//! pipe (`control.rs`), forwards one request line and relays the reply.
//!
//! Dispatched from `run()` before the Tauri builder, keyed on `argv[1] == "mcp"`, so it never
//! opens a window or touches the app's data dir. Register it in the MCP client config as
//! `{"command": "<path to greedout.exe>", "args": ["mcp"]}`. Point it at a copy OUTSIDE
//! `%LOCALAPPDATA%`: the Claude desktop app's container serves a frozen shadow of that folder.
//!
//! The tool catalog below is data, and `tools/list` and `tools/call` both read it, so a verb
//! is added in one place here and one arm in `control.rs`.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Map, Value};

use crate::control::PIPE_NAME;

const PROTOCOL_VERSION: &str = "2025-06-18";
const WATCH_DEFAULT_SAMPLES: u64 = 5;
const WATCH_MAX_SAMPLES: u64 = 60;

/// (name, JSON type, description, required)
type Param = (&'static str, &'static str, &'static str, bool);

struct Verb {
    name: &'static str,
    about: &'static str,
    params: &'static [Param],
}

const ID: Param = ("id", "string", "Session id (the `id` field from `sessions`).", true);

const VERBS: &[Verb] = &[
    Verb { name: "ping", about: "Check that Greedout is running and agent control is on. Returns app version, build time and pid.", params: &[] },
    // Live state
    Verb { name: "sessions", about: "The live session list the gauge window shows: per session the title, project, model, context tokens and percent of budget, estimated spend in USD, whether it is live or focused, and any just-compacted banner.", params: &[] },
    Verb { name: "limits", about: "The 5-hour and weekly plan-limit dials: percent used and when each resets.", params: &[] },
    Verb { name: "history", about: "One session's context-size and cumulative-spend curve over time, as {t (epoch ms), ctx, cost} points. Long curves are thinned evenly (last point kept).", params: &[ID, ("max_points", "integer", "Most points to return (default 200).", false)] },
    Verb { name: "compact_summary", about: "The latest /compact summary text and boundary token counts for a session, or null if it has none.", params: &[ID] },
    // Spend
    Verb { name: "spend_summary", about: "Daily Spend rollup: one {day, project, cost} row per local day and project, across all history.", params: &[("since", "string", "Only days on or after this date, YYYY-MM-DD. Use it: full history can be large.", false)] },
    Verb { name: "spend_day", about: "The individual billed turns in one local day, as the Daily Spend swim lanes show them.", params: &[("lo", "integer", "Start of the day, epoch ms (inclusive).", true), ("hi", "integer", "End of the day, epoch ms (exclusive).", true)] },
    // Context Explorer
    Verb { name: "search", about: "Full-text search of past conversation: user and assistant text and thinking across every session in the history cache, found by case-insensitive substring (any word of 3+ characters; all words must appear together). Includes text a /compact removed from a session's live context, so use it to recover what was said earlier in the current session or any past one. Returns passages with the match marked [[like this]]. Pass `session` to dig through one session (oldest first). Tool input/output is not indexed. If sessions_not_fully_indexed is above 0, run browse_scan first for the newest text.", params: &[("query", "string", "Words to find, e.g. \"migration rollback\".", true), ("session", "string", "Limit to one session id (from `sessions` or a previous search).", false), ("limit", "integer", "Most passages (default 10, max 50).", false), ("window", "integer", "Characters of context each side of the match (default 300, max 2000).", false)] },
    Verb { name: "baseline", about: "Estimated breakdown of a session's base context (system prompt, tools, MCP, memory), reconciled against its real first-turn total.", params: &[ID] },
    Verb { name: "chat_breakdown", about: "Estimated Role/Turn/content breakdown of a session's Chat bucket, as the Context Explorer's treemap shows it.", params: &[ID] },
    Verb { name: "chat_block", about: "Full text of one Chat block. Returns [label, content]. Get `gid` from `chat_breakdown`.", params: &[ID, ("gid", "integer", "Block id from `chat_breakdown`.", true)] },
    Verb { name: "search_first_turn", about: "The turn number of the first message in a session matching a text query, or null.", params: &[ID, ("query", "string", "Text to find.", true)] },
    Verb { name: "browse_status", about: "How many conversations the history cache holds and whether a sync is running.", params: &[] },
    Verb { name: "browse_harnesses", about: "The harnesses with history (claude, codex, cursor) and their session counts and spend.", params: &[] },
    Verb { name: "browse_projects", about: "Projects under one harness, with session counts and spend.", params: &[("harness", "string", "claude, codex or cursor.", true)] },
    Verb { name: "browse_sessions", about: "Every session in one project, from the history cache (not just live ones).", params: &[("harness", "string", "claude, codex or cursor.", true), ("project", "string", "Project name from `browse_projects`.", true)] },
    Verb { name: "browse_session", about: "History-cache metadata for one session id, or null.", params: &[ID] },
    Verb { name: "browse_scan", about: "Bring the history cache up to date with the transcripts on disk. Returns at once; poll `browse_status`.", params: &[] },
    Verb { name: "browse_cancel", about: "Stop following a running history sync (it carries on in the background).", params: &[] },
    // Settings
    Verb { name: "config", about: "All current settings.", params: &[] },
    Verb { name: "set_config", about: "Change settings, applied live like the Settings window: pass only the keys to change, using the names from `config`. Cannot change agent_control or font_sets. Returns the full new settings.", params: &[("patch", "object", "Setting names to new values, e.g. {\"always_on_top\": false, \"opacity\": 0.8}.", true)] },
    Verb { name: "themes", about: "The user's saved custom themes.", params: &[] },
    Verb { name: "set_label", about: "Set or clear a session's custom title.", params: &[ID, ("label", "string", "New title. Omit to clear.", false)] },
    // Windows
    Verb { name: "windows", about: "Which Greedout windows are open, with visibility, focus, minimized state, position and size.", params: &[] },
    Verb { name: "window", about: "Show, hide, focus, minimize, unminimize or close one of Greedout's own windows. The main window cannot be closed, only hidden.", params: &[("label", "string", "main, settings, about, baseline (Context Explorer), dailyspend, themes, fonts or compact.", true), ("action", "string", "show, hide, focus, minimize, unminimize or close.", true)] },
    Verb { name: "open_window", about: "Open a window as a click would. Returns at once; check `windows` or take a `screenshot` to confirm it appeared.", params: &[("window", "string", "settings, about, dailyspend, explorer or compact.", true), ("id", "string", "Session id, for explorer and compact.", false), ("view", "string", "For explorer: session (default) or project.", false)] },
    Verb { name: "screenshot", about: "Render one of Greedout's own windows to a PNG and return the file path; read the path to see it. Use it to check a UI change actually rendered. Cannot capture any other app.", params: &[("label", "string", "Window label (default main).", false)] },
    // Diagnostics
    Verb { name: "log_tail", about: "The last lines of greedout.log (written when debug logging is on, always in dev builds).", params: &[("lines", "integer", "How many lines (default 100, max 500).", false)] },
    Verb { name: "update_check", about: "Ask GitHub whether a newer release exists. Read only; it never installs.", params: &[] },
];

pub fn serve() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_msg(&mut out, &rpc_error(Value::Null, -32700, &format!("parse error: {e}")));
                continue;
            }
        };
        // A message without an id is a notification and gets no reply.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let framed = match handle(method, msg.get("params")) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, m)) => rpc_error(id, code, &m),
        };
        write_msg(&mut out, &framed);
    }
}

fn handle(method: &str, params: Option<&Value>) -> Result<Value, (i64, String)> {
    match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "greedout", "version": env!("CARGO_PKG_VERSION") },
            "instructions": "Drive the running Greedout gauge: read live Claude Code / Codex session \
                context and spend, plan limits, Daily Spend and the Context Explorer's data, change \
                settings, and open or screenshot its windows. Greedout must be running with \"Enable \
                agent control\" ticked in its Settings; otherwise every tool says so."
        })),
        "notifications/initialized" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_list() })),
        "tools/call" => call_tool(params),
        other => Err((-32601, format!("method not found: {other}"))),
    }
}

fn tool_list() -> Value {
    let mut tools: Vec<Value> = VERBS.iter().map(tool_def).collect();
    tools.push(json!({
        "name": "watch",
        "description": "Collect a short time series of the live session list (the same data as \
            `sessions`), one snapshot per interval, and return them as an array. Use it to see a \
            session's context or spend move while it works.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "interval_ms": { "type": "integer", "minimum": 250, "maximum": 60000, "description": "Milliseconds between snapshots (default 1000)." },
                "samples": { "type": "integer", "minimum": 1, "maximum": WATCH_MAX_SAMPLES, "description": "How many snapshots (default 5)." }
            },
            "additionalProperties": false
        }
    }));
    Value::Array(tools)
}

fn tool_def(v: &Verb) -> Value {
    let mut props = Map::new();
    let mut required = Vec::new();
    for (name, ty, desc, req) in v.params {
        props.insert((*name).into(), json!({ "type": ty, "description": desc }));
        if *req {
            required.push(json!(name));
        }
    }
    let mut schema = json!({ "type": "object", "properties": props, "additionalProperties": false });
    if !required.is_empty() {
        schema["required"] = Value::Array(required);
    }
    json!({ "name": v.name, "description": v.about, "inputSchema": schema })
}

fn call_tool(params: Option<&Value>) -> Result<Value, (i64, String)> {
    let params = params.ok_or((-32602, "missing params".to_string()))?;
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").and_then(Value::as_object).cloned().unwrap_or_default();

    if name == "watch" {
        let samples = args
            .get("samples")
            .and_then(Value::as_u64)
            .unwrap_or(WATCH_DEFAULT_SAMPLES)
            .clamp(1, WATCH_MAX_SAMPLES);
        return Ok(watch_result(args.get("interval_ms").and_then(Value::as_u64), samples));
    }
    let Some(verb) = VERBS.iter().find(|v| v.name == name) else {
        return Err((-32602, format!("unknown tool '{name}'")));
    };
    // The pipe rejects what it does not know, but checking here gives the agent the missing
    // parameter's name before a round trip.
    for (p, _, _, required) in verb.params {
        if *required && !args.contains_key(*p) {
            return Ok(tool_error(&format!("missing required parameter '{p}'")));
        }
    }
    let mut req = args;
    req.insert("cmd".into(), json!(name));
    Ok(match pipe_call(&Value::Object(req)) {
        Ok(reply) if reply.get("ok").and_then(Value::as_bool) == Some(true) => {
            let result = reply.get("result").cloned().unwrap_or(Value::Null);
            // A plain string (the screenshot path, "started") goes back as is, not JSON-quoted.
            match result {
                Value::String(s) => text_result(&s, false),
                other => text_result(&serde_json::to_string_pretty(&other).unwrap_or_default(), false),
            }
        }
        Ok(reply) => tool_error(reply.get("error").and_then(Value::as_str).unwrap_or("unknown error")),
        Err(e) => tool_error(&e),
    })
}

fn open_pipe() -> Result<File, String> {
    OpenOptions::new().read(true).write(true).open(PIPE_NAME).map_err(|e| {
        format!("cannot reach Greedout ({e}). Start Greedout and tick \"Enable agent control\" in its Settings.")
    })
}

/// One request line out, one reply line back.
fn pipe_call(req: &Value) -> Result<Value, String> {
    let mut pipe = open_pipe()?;
    send_line(&mut pipe, req)?;
    let mut reader = BufReader::new(pipe);
    let mut buf = String::new();
    reader.read_line(&mut buf).map_err(|e| e.to_string())?;
    if buf.trim().is_empty() {
        return Err("empty reply from Greedout".into());
    }
    serde_json::from_str(buf.trim()).map_err(|e| format!("bad reply: {e}"))
}

fn send_line(pipe: &mut File, req: &Value) -> Result<(), String> {
    let mut line = serde_json::to_vec(req).map_err(|e| e.to_string())?;
    line.push(b'\n');
    pipe.write_all(&line).and_then(|_| pipe.flush()).map_err(|e| e.to_string())
}

/// MCP tool calls are request/response, so the pipe's stream is collected into a bounded array:
/// open once, read the `watching` ack, take `samples` snapshots, then drop the connection.
fn watch_result(interval_ms: Option<u64>, samples: u64) -> Value {
    let mut req = json!({ "cmd": "watch" });
    if let Some(ms) = interval_ms {
        req["interval_ms"] = json!(ms);
    }
    let mut pipe = match open_pipe() {
        Ok(p) => p,
        Err(e) => return tool_error(&e),
    };
    if let Err(e) = send_line(&mut pipe, &req) {
        return tool_error(&e);
    }
    let mut reader = BufReader::new(pipe);
    let mut got: Vec<Value> = Vec::new();
    let mut raw = String::new();
    while (got.len() as u64) < samples {
        raw.clear();
        match reader.read_line(&mut raw) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return tool_error(&e.to_string()),
        }
        let Ok(frame) = serde_json::from_str::<Value>(raw.trim()) else { continue };
        if frame.get("ok").and_then(Value::as_bool) == Some(false) {
            return tool_error(frame.get("error").and_then(Value::as_str).unwrap_or("unknown error"));
        }
        if frame.get("event").and_then(Value::as_str) == Some("snapshot") {
            got.push(frame.get("result").cloned().unwrap_or(frame));
        }
    }
    text_result(
        &serde_json::to_string_pretty(&json!({ "samples": got.len(), "snapshots": got })).unwrap_or_default(),
        false,
    )
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn tool_error(msg: &str) -> Value {
    text_result(msg, true)
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_msg(out: &mut impl Write, v: &Value) {
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}
