//! Config and custom-label sidecars, under `%LOCALAPPDATA%\Greedout`. The app
//! reads Claude Code's home (`~/.claude`) but writes only its own data dir.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Mirror of `Config::debug_logging`, kept here so a trace call can test the gate
/// without touching the disk. Refreshed by `load_config` and `save_config`, so a
/// toggle in Settings takes effect immediately, no restart.
static DEBUG_ON: AtomicBool = AtomicBool::new(false);

/// Cheap gate for trace call sites. The point of a gated logger is that leaving
/// instrumentation in costs nothing when it is off, which is only true if the
/// check itself is nearly free and happens *before* the message is built.
pub fn debug_enabled() -> bool {
    DEBUG_ON.load(Ordering::Relaxed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Max sessions to show.
    #[serde(rename = "N")]
    pub n: usize,
    /// Poll interval, in seconds (fractional allowed, e.g. 0.25).
    pub poll_seconds: f64,
    /// Denominator for the gauge (context budget).
    pub target_tokens: u64,
    /// Sort the session focused in the Claude app to the top (reads the app's
    /// `main.log`). Best-effort; ignored if the log/marker isn't found.
    pub follow_focus: bool,
    /// Show a system-tray icon.
    pub show_in_tray: bool,
    /// Show the window in the taskbar.
    pub show_in_taskbar: bool,
    /// Minimizing the window hides it to the tray instead.
    pub minimize_to_tray: bool,
    /// Closing the window hides it to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Append diagnostics to `greedout.log` in the app data dir.
    pub debug_logging: bool,
    /// Keep the window above other windows.
    pub always_on_top: bool,
    /// Window background opacity, 0.3..1.0.
    pub opacity: f64,
    /// UI zoom factor (Ctrl+wheel), 0.5..3.0. Reapplied on launch.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f64,
    /// Age (hours) at which a row reaches full dim; full brightness <=1h, linear
    /// down to the floor at this age. Frontend-applied.
    #[serde(default = "default_dim_hours")]
    pub dim_hours: f64,
    /// Language: a BCP-47 tag from the picker, or "auto" to follow the OS.
    #[serde(default = "default_locale")]
    pub locale: String,
    /// What "auto" last resolved to, cached by the frontend. The tray menu is
    /// built in `setup()`, before a webview exists to ask, so it reads this.
    #[serde(default = "default_locale_resolved")]
    pub locale_resolved: String,
    /// Color palette: "dark", "light", or "auto" (follow OS). Frontend-applied.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Cross-session browsing (the zoom-out treemap over every project). Off by
    /// default: turning it on is what creates the `cache.sqlite` index, so the
    /// user opts in to a potentially large first scan rather than getting one
    /// silently. See browse.rs.
    #[serde(default)]
    pub browse_enabled: bool,
    /// Show the bottom status bar (per-core CPU + memory usage). On by default.
    #[serde(default = "default_true")]
    pub show_statusbar: bool,
    /// Show each session's latest prompt text under its title in the main window.
    /// On by default.
    #[serde(default = "default_true")]
    pub show_prompt: bool,
    /// Make the session and project titles in the main window clickable (open the
    /// Context Explorer). With it off, titles are plain text (double-click still
    /// renames a session). On by default.
    #[serde(default = "default_true")]
    pub clickable_titles: bool,
    /// Ask GitHub once, at launch, whether a newer version exists. On by
    /// default, and the only outbound request the app ever makes: with it off,
    /// nothing here touches the network unless the user asks in About.
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// Interface font slots and the boldness preference. Frontend-applied; the
    /// values are ids from the catalog in `src/lib/fonts.ts`, not family names,
    /// so a face can be swapped or renamed without invalidating saved configs.
    #[serde(default = "default_font_ui")]
    pub font_ui: String,
    #[serde(default = "default_font_num")]
    pub font_num: String,
    #[serde(default = "default_font_mono")]
    pub font_mono: String,
    /// Boldness, -2 (lighter) ..= 2 (bolder); one step shifts every weight 100.
    #[serde(default)]
    pub font_weight: i64,
    /// Per-slot size trim, 0.5..=1.5. A face swapped into a slot can render
    /// visibly smaller or larger than the one it replaced; this is the nudge.
    #[serde(default = "default_size")]
    pub size_ui: f64,
    #[serde(default = "default_size_num")]
    pub size_num: f64,
    #[serde(default = "default_size")]
    pub size_mono: f64,
    /// Font sets the user saved. The built-in sets live in the frontend catalog
    /// and are never written here: a shipped set has to keep meaning the same
    /// thing across versions, so it stays read-only and you clone it instead.
    #[serde(default)]
    pub font_sets: Vec<UserFontSet>,
}

/// A saved font set: the same seven values the font preferences carry, plus a
/// name. Flat rather than nested so the frontend can spread it straight onto
/// the preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserFontSet {
    pub id: String,
    pub label: String,
    pub font_ui: String,
    pub font_num: String,
    pub font_mono: String,
    #[serde(default)]
    pub font_weight: i64,
    #[serde(default = "default_size")]
    pub size_ui: f64,
    #[serde(default = "default_size")]
    pub size_num: f64,
    #[serde(default = "default_size")]
    pub size_mono: f64,
}

fn default_ui_scale() -> f64 {
    1.0
}

fn default_dim_hours() -> f64 {
    24.0
}

fn default_locale() -> String {
    "auto".to_string()
}

fn default_locale_resolved() -> String {
    "en".to_string()
}

fn default_theme() -> String {
    "dark".to_string()
}

fn default_true() -> bool {
    true
}

// A fresh install wears the Oscilloscope set, matching DEFAULT_FONTS in
// src/lib/fonts.ts. These are catalog ids, not family names.
fn default_font_ui() -> String {
    "jura".to_string()
}

fn default_font_num() -> String {
    "jura-light".to_string()
}

fn default_font_mono() -> String {
    "source-code-pro".to_string()
}

fn default_size() -> f64 {
    1.0
}

fn default_size_num() -> f64 {
    1.05
}

impl Default for Config {
    fn default() -> Self {
        Config {
            n: 1,
            poll_seconds: 0.5,
            target_tokens: 200_000,
            follow_focus: true,
            show_in_tray: true,
            show_in_taskbar: true,
            minimize_to_tray: true,
            close_to_tray: true,
            debug_logging: false,
            always_on_top: true,
            opacity: 0.92,
            ui_scale: 1.0,
            dim_hours: 24.0,
            locale: default_locale(),
            locale_resolved: default_locale_resolved(),
            theme: "dark".to_string(),
            browse_enabled: false,
            show_statusbar: true,
            show_prompt: true,
            clickable_titles: true,
            check_updates: true,
            font_ui: default_font_ui(),
            font_num: default_font_num(),
            font_mono: default_font_mono(),
            font_weight: 0,
            size_ui: 1.0,
            size_num: default_size_num(),
            size_mono: 1.0,
            font_sets: Vec::new(),
        }
    }
}

/// `~/.claude`: the Claude Code home. Honors CLAUDE_CONFIG_DIR if set.
pub fn claude_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir().unwrap_or_default().join(".claude")
}

/// `~/.codex`: the Codex (OpenAI) home, where the CLI and the desktop app both
/// write per-session rollout transcripts. Honors CODEX_HOME if set.
pub fn codex_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        return PathBuf::from(dir);
    }
    dirs::home_dir().unwrap_or_default().join(".codex")
}

/// `%LOCALAPPDATA%\Greedout`: Greedout's own data dir (config, labels, log, and
/// the opt-in browse cache). The app reads `~/.claude` but writes only here, so
/// nothing of ours lands inside Claude Code's config home. Public so browse.rs
/// can place `cache.sqlite` alongside. Falls back to the old location only if the
/// platform has no local-data dir (not expected on Windows).
pub fn app_dir() -> PathBuf {
    dirs::data_local_dir()
        .map(|d| d.join("Greedout"))
        .unwrap_or_else(|| claude_dir().join("greedout"))
}

/// One-time move of the sidecar files out of `~/.claude/greedout` (where versions
/// up to 0.2.2 kept them) into `app_dir()`. Best-effort and idempotent: it only
/// moves a file when the destination does not already exist, and on any failure
/// the app still runs from the new dir. Removes the old dir once it is empty so
/// nothing of ours lingers in Claude Code's config home.
pub fn migrate_sidecars() {
    let old = claude_dir().join("greedout");
    let new = app_dir();
    if old == new || !old.exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&new);
    for name in [
        "config.json",
        "labels.json",
        "window-state.json",
        "cache.sqlite",
        "cache.sqlite-wal",
        "cache.sqlite-shm",
    ] {
        let src = old.join(name);
        let dst = new.join(name);
        if src.exists() && !dst.exists() {
            // A rename is instant within a volume; across volumes it fails, so
            // fall back to copy-then-delete.
            if std::fs::rename(&src, &dst).is_err() && std::fs::copy(&src, &dst).is_ok() {
                let _ = std::fs::remove_file(&src);
            }
        }
    }
    // The log is disposable (see truncate_log): drop the old one rather than move
    // it, both so a stale multi-run log does not follow the user across the move
    // and so the old dir is empty enough to remove below.
    let _ = std::fs::remove_file(old.join("greedout.log"));
    let _ = std::fs::remove_dir(&old);
}

/// Start each run with an empty log. The log is a rolling diagnostic, not a record
/// worth keeping between runs, and it is append-only, so leaving debug logging on
/// otherwise grows it without bound. Best-effort; a missing file is fine.
pub fn truncate_log() {
    let _ = std::fs::remove_file(app_dir().join("greedout.log"));
}

/// Path to the opt-in cross-session metadata cache. Only created once browsing is
/// enabled (see browse.rs); its mere presence means the user opted in.
pub fn cache_db_path() -> PathBuf {
    app_dir().join("cache.sqlite")
}

/// Append a diagnostic line to `greedout.log` when debug logging is on. Shared by
/// lib.rs and scan.rs so any module can trace without re-implementing the gate.
pub fn debug_log(cfg: &Config, msg: &str) {
    if !cfg.debug_logging {
        return;
    }
    debug_log_raw(msg);
}

/// Write unconditionally. Only call this behind `debug_enabled()`.
pub fn debug_log_raw(msg: &str) {
    let path = app_dir().join("greedout.log");
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let _ = writeln!(f, "{ts} {msg}");
    }
}

fn config_path() -> PathBuf {
    app_dir().join("config.json")
}

fn labels_path() -> PathBuf {
    app_dir().join("labels.json")
}

/// Load config, writing defaults if the file is missing or unreadable.
pub fn load_config() -> Config {
    let path = config_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<Config>(&text) {
            DEBUG_ON.store(cfg.debug_logging, Ordering::Relaxed);
            return cfg;
        }
    }
    let cfg = Config::default();
    DEBUG_ON.store(cfg.debug_logging, Ordering::Relaxed);
    let _ = std::fs::create_dir_all(app_dir());
    if let Ok(text) = serde_json::to_string_pretty(&cfg) {
        let _ = std::fs::write(&path, text);
    }
    cfg
}

/// Persist the config sidecar.
pub fn save_config(cfg: &Config) -> std::io::Result<()> {
    DEBUG_ON.store(cfg.debug_logging, Ordering::Relaxed);
    std::fs::create_dir_all(app_dir())?;
    let text = serde_json::to_string_pretty(cfg).unwrap_or_default();
    std::fs::write(config_path(), text)
}

/// sessionId -> custom label. Missing file is an empty map.
pub fn load_labels() -> HashMap<String, String> {
    std::fs::read_to_string(labels_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_label(id: &str, label: Option<String>) -> std::io::Result<()> {
    let mut labels = load_labels();
    match label {
        Some(l) if !l.is_empty() => {
            labels.insert(id.to_string(), l);
        }
        _ => {
            labels.remove(id);
        }
    }
    std::fs::create_dir_all(app_dir())?;
    let text = serde_json::to_string_pretty(&labels).unwrap_or_else(|_| "{}".into());
    std::fs::write(labels_path(), text)
}
