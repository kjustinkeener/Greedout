//! Window geometry persistence for the `main` window, hand-rolled.
//!
//! Replaces `tauri-plugin-window-state`, which lets you override the state file's
//! name but not its directory: it always creates `%APPDATA%\<bundle-id>` through the
//! Windows known-folder API. Greedout keeps every other sidecar in its own data dir
//! (`%LOCALAPPDATA%\Greedout`), so the plugin's one stray folder was the only thing the
//! app left outside that dir. Writing `window-state.json` next to `config.json` removes it.
//!
//! Scope is deliberately tiny: physical size, position and a maximized flag, main window
//! only. A missing or corrupt file restores nothing, so the app falls back to the
//! config default size, OS-centered. Every write is best-effort; bad state never panics.
//! Follows the shared window-state pattern used across these apps.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

/// Smallest geometry worth trusting, in physical pixels.
///
/// A Win+D restore of a borderless window collapses it to roughly 215x26 (the height of
/// a title bar that is not even drawn) and reports that size while `is_minimized()` has
/// already gone false. Anything at or under that is the collapse, not a window a user
/// made. The floor is Greedout's own `minWidth`/`minHeight` rather than the pattern's
/// 300x300, because 220x90 is a legitimate gauge size here and 300 would reject it.
pub const MIN_W: u32 = 220;
pub const MIN_H: u32 = 90;

/// True when a size is too small to have come from the user.
pub fn too_small(w: u32, h: u32) -> bool {
    w < MIN_W || h < MIN_H
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
struct WinState {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    #[serde(default)]
    maximized: bool,
}

fn state_file() -> PathBuf {
    crate::config::app_dir().join("window-state.json")
}

fn load() -> Option<WinState> {
    let text = std::fs::read_to_string(state_file()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Does the saved rectangle still overlap a monitor that is plugged in right now?
///
/// Size is always restored, but position only when this holds. A window last closed on a
/// second display that has since been unplugged would otherwise come back at coordinates
/// no monitor covers: alive, focusable, and invisible. When the monitors cannot be
/// enumerated at all, assume yes rather than fight the user.
fn on_screen(window: &WebviewWindow, s: &WinState) -> bool {
    let monitors = match window.available_monitors() {
        Ok(m) if !m.is_empty() => m,
        _ => return true,
    };
    let (l, t) = (s.x, s.y);
    let (r, b) = (s.x + s.width as i32, s.y + s.height as i32);
    monitors.iter().any(|m| {
        let (mp, ms) = (m.position(), m.size());
        let (ml, mt) = (mp.x, mp.y);
        let (mr, mb) = (mp.x + ms.width as i32, mp.y + ms.height as i32);
        l < mr && r > ml && t < mb && b > mt
    })
}

/// Adopt geometry left behind by `tauri-plugin-window-state` and clear its folder.
///
/// The plugin wrote `%APPDATA%\<bundle-id>\.window-state.json`, keyed by window label.
/// Anyone upgrading has a window there they arranged deliberately, so read it once
/// rather than resetting them to the default, then take the folder away. Removing the
/// directory is best-effort and only succeeds when nothing else lives in it.
fn migrate_from_plugin(window: &WebviewWindow) {
    let Some(base) = dirs::data_dir() else { return };
    let dir = base.join(&window.app_handle().config().identifier);
    let old = dir.join(".window-state.json");
    let Ok(text) = std::fs::read_to_string(&old) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let m = &v["main"];
    let num = |k: &str| m[k].as_f64();
    if let (Some(w), Some(h), Some(x), Some(y)) =
        (num("width"), num("height"), num("x"), num("y"))
    {
        let s = WinState {
            x: x as i32,
            y: y as i32,
            width: w as u32,
            height: h as u32,
            maximized: m["maximized"].as_bool().unwrap_or(false),
        };
        write(&s);
    }
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::remove_dir(&dir);
}

/// Apply the saved geometry. Call from `setup`, before the window is shown.
pub fn restore(window: &WebviewWindow) {
    if !state_file().exists() {
        migrate_from_plugin(window);
    }
    let Some(s) = load() else { return };
    if too_small(s.width, s.height) {
        return;
    }
    let _ = window.set_size(PhysicalSize::new(s.width, s.height));
    if on_screen(window, &s) {
        let _ = window.set_position(PhysicalPosition::new(s.x, s.y));
    }
    if s.maximized {
        let _ = window.maximize();
    }
}

/// Persist the current geometry. Called eagerly from the move and resize events: the
/// tray Quit and an external restart both kill the process without a chance to save.
///
/// While maximized, keep the previously saved normal bounds and only flip the flag, so a
/// later unmaximize lands on a reasonable size instead of the maximized rectangle.
pub fn save(window: &WebviewWindow) {
    let maximized = window.is_maximized().unwrap_or(false);
    let mut s = load().unwrap_or(WinState { x: 0, y: 0, width: 340, height: 260, maximized: false });
    if maximized {
        s.maximized = true;
    } else if let (Ok(sz), Ok(pos)) = (window.inner_size(), window.outer_position()) {
        if too_small(sz.width, sz.height) {
            return;
        }
        s.width = sz.width;
        s.height = sz.height;
        s.x = pos.x;
        s.y = pos.y;
        s.maximized = false;
    }
    write(&s);
}

fn write(s: &WinState) {
    let path = state_file();
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&path, text);
    }
}
