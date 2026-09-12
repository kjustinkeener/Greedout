//! User-defined color themes, persisted as a sidecar in the app data dir.
//!
//! The analog of `winstate`: a best-effort JSON sidecar (`themes.json` next to
//! `config.json` in `%LOCALAPPDATA%\Greedout`), loaded and saved through the
//! same tolerant pattern. A missing or corrupt file yields an empty list and
//! never panics, so a hand-edited palette that is malformed cannot take the app
//! down with it.
//!
//! The struct serializes to exactly the JSON shape of the frontend `ThemeData`
//! / `ThemeColors` interfaces (`src/lib/themeCss.ts`): camelCase keys, a full
//! palette optional, individual optionals absent when unset. A built-in theme is
//! never written here; the frontend catalog owns those. This module only holds
//! the themes a user saved.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::Emitter;

/// The flat color palette of a user theme. Mirrors `ThemeColors` in
/// `src/lib/themeCss.ts`: `bg` is an RGB triple (so the opacity slider touches
/// only alpha) and every other token is a hex/rgba string. `#[serde(default)]`
/// lets a hand-edited file omit tokens and still load.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UserThemeColors {
    pub bg: [u8; 3],
    pub fg: String,
    pub muted: String,
    pub track: String,
    pub green: String,
    pub yellow: String,
    pub red: String,
    pub live: String,
    pub edge: String,
    pub edge_soft: String,
    pub hover: String,
    pub panel: String,
}

/// One saved user theme. Mirrors `ThemeData` in `src/lib/themeCss.ts`.
/// `#[serde(default)]` plus `Option` on every non-identity field means a
/// partially hand-written entry still deserializes rather than erroring.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct UserTheme {
    pub id: String,
    pub label: String,
    pub group: String,
    pub scheme: String,
    pub note: Option<String>,
    pub colors: Option<UserThemeColors>,
    pub gradient: Option<[String; 3]>,
    pub spend_fg: Option<String>,
    pub spend_shadow: Option<String>,
    pub auto_light_from: Option<String>,
}

fn user_themes_path() -> PathBuf {
    crate::config::app_dir().join("themes.json")
}

/// Best-effort read of the user themes. Missing file or corrupt JSON both yield
/// an empty list; never panics. Mirrors winstate's tolerance exactly.
pub fn load_user_themes() -> Vec<UserTheme> {
    let Ok(text) = std::fs::read_to_string(user_themes_path()) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<UserTheme>>(&text) {
        Ok(v) => v,
        Err(e) => {
            crate::config::debug_log_raw(&format!("themes.json parse failed: {e}"));
            Vec::new()
        }
    }
}

/// Persist the user themes as pretty JSON, creating the app dir if needed.
pub fn save_user_themes(themes: &[UserTheme]) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::config::app_dir())?;
    let text = serde_json::to_string_pretty(themes).unwrap_or_else(|_| "[]".into());
    std::fs::write(user_themes_path(), text)
}

/// The user's saved themes, for the frontend to inject alongside the built-ins.
#[tauri::command]
pub fn get_user_themes() -> Vec<UserTheme> {
    load_user_themes()
}

/// Persist the edited set of user themes, then notify every secondary window so
/// an open Theme browser / Settings / Explorer repaints without a restart. The
/// window list mirrors the one `set_config` keeps in `lib.rs`.
#[tauri::command]
pub fn set_user_themes(app: tauri::AppHandle, themes: Vec<UserTheme>) -> Result<(), String> {
    save_user_themes(&themes).map_err(|e| e.to_string())?;
    for label in ["settings", "about", "baseline", "themes", "fonts"] {
        let _ = app.emit_to(label, "user-themes", &themes);
    }
    Ok(())
}
