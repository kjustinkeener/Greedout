//! Agent control surface: a per-user named pipe that lets a local MCP client read Greedout's
//! live state and drive it. Windows only; `mcp.rs` is the stdio shim that speaks to it.
//!
//! One code path with the UI: every verb calls the same function the matching
//! `#[tauri::command]` does, never a second implementation that could drift from a click.
//!
//! Off unless the user ticks "Enable agent control" in Settings (`Config::agent_control`).
//! The pipe is created with a DACL granting only the current user, so no other account on the
//! machine can open it, and `ENABLED` is checked per request so switching it off cuts off
//! clients that are already connected, not just new ones.
//!
//! Protocol: newline-delimited JSON. A request is `{"cmd": "...", ...params}`; a reply is
//! `{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`. `watch` is the one stream: it
//! keeps the connection and pushes `{"event":"snapshot","result":[sessions]}` frames.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Fixed pipe name. A pipe is a kernel namespace object, not a file under AppData, so the
/// desktop app's AppData virtualization cannot shadow it.
#[cfg(not(debug_assertions))]
pub const PIPE_NAME: &str = r"\\.\pipe\greedout";
/// Debug builds get their own pipe so dev never talks to, or collides with, the installed app.
#[cfg(debug_assertions)]
pub const PIPE_NAME: &str = r"\\.\pipe\greedout-dev";

/// Every window Greedout can own. `screenshot` and `window` accept only these labels, checked
/// before any window lookup, so neither can be pointed at another application's window.
pub const ALL_WINDOWS: [&str; 9] =
    ["main", "settings", "about", "baseline", "dailyspend", "themes", "fonts", "compact", "tips"];

/// Settings an agent may not change: turning the surface on is the user's decision, saved font
/// sets are edited in their own window, and `locale_resolved` is a cache the webview owns.
const PROTECTED_KEYS: [&str; 3] = ["agent_control", "font_sets", "locale_resolved"];

const WATCH_DEFAULT_MS: u64 = 1000;
const WATCH_MIN_MS: u64 = 250;
const WATCH_MAX_MS: u64 = 60_000;
const LOG_TAIL_MAX: usize = 500;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// The accept-loop task, so switching the setting off can abort it and free the pipe name.
struct ControlState(Mutex<Option<tauri::async_runtime::JoinHandle<()>>>);

/// Start or stop the pipe to match the setting. Idempotent; called at startup and from
/// `set_config`, so the checkbox takes effect live with no restart.
pub fn sync(app: &AppHandle, enabled: bool) {
    ENABLED.store(enabled, Ordering::SeqCst);
    if app.try_state::<ControlState>().is_none() {
        app.manage(ControlState(Mutex::new(None)));
    }
    let state = app.state::<ControlState>();
    let mut slot = state.0.lock().unwrap();
    match (enabled, slot.is_some()) {
        (true, false) => {
            let app = app.clone();
            *slot = Some(tauri::async_runtime::spawn(async move {
                if let Err(e) = serve(app).await {
                    crate::config::debug_log_raw(&format!("control: pipe server stopped: {e}"));
                }
            }));
        }
        (false, true) => {
            if let Some(h) = slot.take() {
                h.abort();
            }
        }
        _ => {}
    }
}

/// A `SECURITY_ATTRIBUTES` whose DACL grants full access to the current user's SID and no one
/// else. Built once and kept for the life of the process: the pipe is re-created per client,
/// and each creation needs the same descriptor.
fn pipe_security() -> Result<*mut c_void, String> {
    static SA: OnceLock<usize> = OnceLock::new();
    if let Some(p) = SA.get() {
        return Ok(*p as *mut c_void);
    }
    let sid = current_user_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})").encode_utf16().chain(Some(0)).collect();
    let mut psd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut psd,
            None,
        )
        .map_err(|e| format!("security descriptor: {e}"))?;
    }
    let sa = Box::new(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    });
    let p = Box::into_raw(sa) as usize;
    let _ = SA.set(p);
    Ok(*SA.get().unwrap() as *mut c_void)
}

/// The current user's SID as a string, from the process token.
fn current_user_sid() -> Result<String, String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("OpenProcessToken: {e}"))?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            len,
            &mut len,
        );
        let _ = CloseHandle(token);
        got.map_err(|e| format!("GetTokenInformation: {e}"))?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut wide = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut wide)
            .map_err(|e| format!("ConvertSidToStringSid: {e}"))?;
        let s = wide.to_string().map_err(|e| e.to_string());
        let _ = LocalFree(Some(HLOCAL(wide.0 as *mut c_void)));
        s
    }
}

fn create_pipe(first: bool) -> Result<NamedPipeServer, String> {
    let sa = pipe_security()?;
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first);
    unsafe { opts.create_with_security_attributes_raw(PIPE_NAME, sa) }.map_err(|e| e.to_string())
}

async fn serve(app: AppHandle) -> Result<(), String> {
    // `first_pipe_instance` stops a second Greedout (or anything else) from squatting the name.
    let mut server = create_pipe(true)?;
    loop {
        server.connect().await.map_err(|e| e.to_string())?;
        let connected = server;
        // Stand the next instance up before serving this one, so a connect never finds no pipe.
        server = create_pipe(false)?;
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            // A client hanging up surfaces as an IO error; that is normal, not a fault.
            let _ = handle(connected, app).await;
        });
    }
}

/// One request frame. Verb parameters ride in `extra`, so adding a verb never means adding a
/// typed field; each arm pulls what it needs with `arg!` / `opt`.
#[derive(Deserialize)]
struct Request {
    cmd: String,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl Request {
    fn arg<T: DeserializeOwned>(&self, key: &str) -> Result<T, String> {
        match self.extra.get(key) {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("bad '{key}': {e}")),
            None => Err(format!("missing required parameter '{key}'")),
        }
    }
    fn opt<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.extra.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

macro_rules! arg {
    ($req:expr, $key:expr) => {
        match $req.arg($key) {
            Ok(v) => v,
            Err(e) => return err(e),
        }
    };
}

fn ok(v: impl serde::Serialize) -> Value {
    json!({ "ok": true, "result": v })
}

fn err(e: impl std::fmt::Display) -> Value {
    json!({ "ok": false, "error": e.to_string() })
}

fn reply<T: serde::Serialize, E: std::fmt::Display>(r: Result<T, E>) -> Value {
    match r {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

/// Run blocking work (transcript scans, SQLite) off the async threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, v: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await
}

async fn handle(pipe: NamedPipeServer, app: AppHandle) -> std::io::Result<()> {
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let mut lines = BufReader::new(read_half).lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !ENABLED.load(Ordering::SeqCst) {
            write_frame(&mut write_half, &err("agent control is switched off in Greedout's Settings"))
                .await?;
            continue;
        }
        let req: Request = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                write_frame(&mut write_half, &err(format!("bad request: {e}"))).await?;
                continue;
            }
        };
        if req.cmd == "watch" {
            stream_sessions(&app, &mut write_half, &req).await?;
            return Ok(());
        }
        let out = dispatch(&app, req).await;
        write_frame(&mut write_half, &out).await?;
    }
    Ok(())
}

/// `watch`: push the live session list on an interval until the client goes away. It rides the
/// poll loop's own `sessions` event instead of scanning again, so a watcher costs the app
/// nothing; a slow reader only stalls its own task, since the latest value overwrites the last.
async fn stream_sessions<W: AsyncWrite + Unpin>(
    app: &AppHandle,
    w: &mut W,
    req: &Request,
) -> std::io::Result<()> {
    let ms = req.opt::<u64>("interval_ms").unwrap_or(WATCH_DEFAULT_MS).clamp(WATCH_MIN_MS, WATCH_MAX_MS);
    let (tx, rx) = tokio::sync::watch::channel(String::new());
    let id = app.listen("sessions", move |e| {
        let _ = tx.send(e.payload().to_string());
    });
    write_frame(w, &json!({ "event": "watching", "interval_ms": ms })).await?;
    let mut tick = tokio::time::interval(Duration::from_millis(ms));
    let res = loop {
        tick.tick().await;
        if !ENABLED.load(Ordering::SeqCst) {
            break Ok(());
        }
        let latest = rx.borrow().clone();
        if latest.is_empty() {
            continue;
        }
        let result: Value = serde_json::from_str(&latest).unwrap_or(Value::Null);
        if let Err(e) = write_frame(w, &json!({ "event": "snapshot", "result": result })).await {
            break Err(e);
        }
    };
    app.unlisten(id);
    res
}

async fn dispatch(app: &AppHandle, req: Request) -> Value {
    match req.cmd.as_str() {
        "ping" => ok(json!({
            "app": "greedout",
            "version": env!("CARGO_PKG_VERSION"),
            "build_epoch": crate::build_epoch(),
            "pid": std::process::id(),
            "debug_build": cfg!(debug_assertions),
        })),

        // ---- Live state ----
        "sessions" => reply(blocking(crate::get_sessions).await),
        "limits" => ok(crate::get_limits()),
        "history" => {
            let id: String = arg!(req, "id");
            let max_points = req.opt::<usize>("max_points").unwrap_or(200).max(2);
            match crate::get_history(id).await {
                Ok(samples) => ok(thin(samples, max_points)),
                Err(e) => err(e),
            }
        }
        "compact_summary" => {
            let id: String = arg!(req, "id");
            reply(blocking(move || crate::get_compact_summary(id)).await)
        }

        // ---- Spend ----
        "spend_summary" => {
            // Optional `since` ("YYYY-MM-DD") keeps years of rows out of one reply.
            let since = req.opt::<String>("since");
            match crate::get_spend_summary().await {
                Ok(rows) => ok(rows
                    .into_iter()
                    .filter(|r| since.as_deref().map_or(true, |s| r.day.as_str() >= s))
                    .collect::<Vec<_>>()),
                Err(e) => err(e),
            }
        }
        "spend_day" => {
            let (lo, hi): (i64, i64) = (arg!(req, "lo"), arg!(req, "hi"));
            reply(blocking(move || crate::get_spend_day(lo, hi)).await)
        }

        // ---- Context Explorer ----
        "baseline" => {
            let id: String = arg!(req, "id");
            reply(blocking(move || crate::analyze_baseline(id)).await)
        }
        "chat_breakdown" => {
            let id: String = arg!(req, "id");
            reply(blocking(move || crate::chat_breakdown(id)).await)
        }
        "chat_block" => {
            let (id, gid): (String, u64) = (arg!(req, "id"), arg!(req, "gid"));
            reply(blocking(move || crate::chat_block(id, gid)).await)
        }
        "search_first_turn" => {
            let (id, query): (String, String) = (arg!(req, "id"), arg!(req, "query"));
            reply(blocking(move || crate::search_first_turn(id, query)).await)
        }
        // The agent's way into past conversation, including text a /compact removed from the
        // live context. Optional `session` limits it to one session.
        "search" => {
            let query: String = arg!(req, "query");
            let session = req.opt::<String>("session");
            let limit = req.opt::<usize>("limit").unwrap_or(10).clamp(1, 50);
            let window = req.opt::<usize>("window").unwrap_or(300).clamp(50, 2000);
            reply(blocking(move || crate::browse::search_passages(&query, session.as_deref(), limit, window)).await.and_then(|r| r))
        }
        "browse_status" => reply(blocking(crate::browse_status).await),
        "browse_harnesses" => reply(blocking(crate::browse_harnesses).await),
        "browse_projects" => {
            let harness: String = arg!(req, "harness");
            reply(blocking(move || crate::browse_projects(harness)).await)
        }
        "browse_sessions" => {
            let (harness, project): (String, String) = (arg!(req, "harness"), arg!(req, "project"));
            reply(blocking(move || crate::browse_sessions(harness, project)).await)
        }
        "browse_session" => {
            let id: String = arg!(req, "id");
            reply(blocking(move || crate::browse_session(id)).await)
        }
        // Brings the history cache up to date; returns at once and runs in the background.
        "browse_scan" => {
            crate::browse_scan(app.clone());
            ok("started")
        }
        "browse_cancel" => {
            crate::browse_cancel();
            ok("cancelled")
        }

        // ---- Settings ----
        "config" => ok(crate::config::load_config()),
        "set_config" => reply(set_config_patch(app, arg!(req, "patch"))),
        "themes" => ok(crate::themes::get_user_themes()),
        "set_label" => {
            let id: String = arg!(req, "id");
            reply(crate::set_label(id, req.opt::<String>("label")))
        }

        // ---- Windows ----
        "windows" => ok(window_states(app)),
        "window" => {
            let (label, action): (String, String) = (arg!(req, "label"), arg!(req, "action"));
            reply(window_action(app, &label, &action))
        }
        "open_window" => {
            let which: String = arg!(req, "window");
            if !["settings", "about", "dailyspend", "tips", "explorer", "compact"].contains(&which.as_str()) {
                return err(format!("unknown window '{which}'"));
            }
            let mut payload = json!({ "window": which });
            for k in ["id", "view"] {
                if let Some(v) = req.extra.get(k) {
                    payload[k] = v.clone();
                }
            }
            reply(app.emit("agent:open", payload))
        }
        "screenshot" => reply(screenshot(app, req.opt::<String>("label").as_deref())),

        // ---- Diagnostics ----
        "log_tail" => ok(log_tail(req.opt::<usize>("lines").unwrap_or(100).min(LOG_TAIL_MAX))),
        // Read only. Applying an update restarts the app, so that stays a user action.
        "update_check" => reply(blocking(crate::update::update_check).await.and_then(|r| r)),

        other => err(format!("unknown verb '{other}'")),
    }
}

/// Evenly thin a long series to at most `max` points, always keeping the last, so a session
/// with thousands of turns still fits one reply.
fn thin<T: serde::Serialize>(items: Vec<T>, max: usize) -> Vec<T> {
    if items.len() <= max {
        return items;
    }
    let n = items.len();
    let step = (n - 1) as f64 / (max - 1) as f64;
    let keep: std::collections::BTreeSet<usize> =
        (0..max).map(|i| ((i as f64 * step).round() as usize).min(n - 1)).collect();
    items.into_iter().enumerate().filter(|(i, _)| keep.contains(i)).map(|(_, v)| v).collect()
}

/// Merge `patch` into the saved settings and apply it as the Settings window would, including
/// the live-apply events the main window listens for.
fn set_config_patch(app: &AppHandle, patch: Map<String, Value>) -> Result<Value, String> {
    let mut cur = serde_json::to_value(crate::config::load_config()).map_err(|e| e.to_string())?;
    let obj = cur.as_object_mut().ok_or("config is not an object")?;
    for (k, v) in patch {
        if PROTECTED_KEYS.contains(&k.as_str()) {
            return Err(format!("'{k}' cannot be changed through the agent control surface"));
        }
        if !obj.contains_key(&k) {
            return Err(format!("unknown setting '{k}'"));
        }
        obj.insert(k, v);
    }
    let next: crate::config::Config = serde_json::from_value(cur).map_err(|e| format!("bad value: {e}"))?;
    // The Settings window blocks this state in the UI: no tray icon and no taskbar button
    // leaves a borderless always-on-top window with no way back once hidden.
    if !next.show_in_tray && !next.show_in_taskbar {
        return Err("show_in_tray and show_in_taskbar cannot both be off".to_string());
    }
    crate::set_config(app.clone(), next.clone())?;
    let _ = app.emit("dim-hours", next.dim_hours);
    let _ = app.emit("show-statusbar", next.show_statusbar);
    let _ = app.emit("show-prompt", next.show_prompt);
    let _ = app.emit("clickable-titles", next.clickable_titles);
    let _ = app.emit("opacity-preview", next.opacity);
    let _ = app.emit("theme", next.theme.clone());
    serde_json::to_value(next).map_err(|e| e.to_string())
}

fn own_window(app: &AppHandle, label: &str) -> Result<tauri::WebviewWindow, String> {
    if !ALL_WINDOWS.contains(&label) {
        return Err(format!("unknown window '{label}'"));
    }
    app.get_webview_window(label).ok_or_else(|| format!("window '{label}' is not open"))
}

fn window_states(app: &AppHandle) -> Vec<Value> {
    ALL_WINDOWS
        .iter()
        .filter_map(|label| {
            let w = app.get_webview_window(label)?;
            let pos = w.outer_position().ok();
            let size = w.inner_size().ok();
            Some(json!({
                "label": label,
                "visible": w.is_visible().unwrap_or(false),
                "focused": w.is_focused().unwrap_or(false),
                "minimized": w.is_minimized().unwrap_or(false),
                "x": pos.map(|p| p.x), "y": pos.map(|p| p.y),
                "width": size.map(|s| s.width), "height": size.map(|s| s.height),
            }))
        })
        .collect()
}

fn window_action(app: &AppHandle, label: &str, action: &str) -> Result<String, String> {
    if label == "main" && action == "show" {
        crate::reveal(app);
        return Ok("shown".into());
    }
    let w = own_window(app, label)?;
    match action {
        "show" => w.show(),
        "hide" => w.hide(),
        "focus" => w.set_focus(),
        "minimize" => w.minimize(),
        "unminimize" => w.unminimize(),
        // Closing the main window quits the app, which is the launcher's job, not the pipe's.
        "close" if label != "main" => w.close(),
        "close" => return Err("the main window cannot be closed through the control surface".into()),
        other => return Err(format!("unknown action '{other}'")),
    }
    .map_err(|e| e.to_string())?;
    Ok(action.to_string())
}

/// Render one of Greedout's own windows to a PNG and return its path. A file, not inline
/// image data: a full-window capture is far past an MCP reply's size limit. The folder is
/// under the home directory, not AppData, which the desktop app's tools see a shadow of.
fn screenshot(app: &AppHandle, label: Option<&str>) -> Result<String, String> {
    let label = label.unwrap_or("main");
    let window = own_window(app, label)?;
    let hwnd = window.hwnd().map_err(|e| e.to_string())?;
    let png = crate::screenshot::capture_hwnd(hwnd)?;
    let dir = dirs::home_dir().ok_or("no home directory")?.join(crate::config::DIR_NAME).join("screenshots");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{label}.png"));
    std::fs::write(&path, png).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

fn log_tail(lines: usize) -> Vec<String> {
    let text = std::fs::read(crate::config::app_dir().join("greedout.log"))
        .map(|b| String::from_utf8_lossy(&b[b.len().saturating_sub(256 * 1024)..]).into_owned())
        .unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].iter().map(|s| s.to_string()).collect()
}
