mod baseline;
mod browse;
mod codex;
mod config;
mod grouping;
mod i18n;
mod install;
mod update;
mod scan;
mod stats;
mod sysfonts;
mod winstate;

use scan::Session;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager};

/// Holds the tray icon so settings changes can show/hide it live.
struct TrayState(Mutex<TrayIcon>);

/// The last main-window size big enough to have come from the user.
///
/// Win+D collapses a borderless window to a sliver and Windows reports that size back
/// with the minimized flag already cleared. Both the live repair and the tray Show path
/// need something believable to put back, and a fresh launch that has never been resized
/// needs one too, so this is seeded in setup.
struct LastGood(Mutex<Option<(u32, u32)>>);

fn last_good(app: &tauri::AppHandle) -> Option<(u32, u32)> {
    app.try_state::<LastGood>().and_then(|s| *s.0.lock().unwrap())
}

/// Remember the main window's geometry after a move or resize, or repair it.
///
/// A Win+D restore arrives here as a resize to the collapsed placeholder, reported while
/// `is_minimized()` has already gone false. Saving that would persist the sliver and
/// bring it back next launch, so a sub-floor size re-asserts the last good size on the
/// live window instead of being written down.
fn track_geometry(app: &tauri::AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    let Ok(sz) = w.inner_size() else { return };
    if winstate::too_small(sz.width, sz.height) {
        if let Some((lw, lh)) = last_good(app) {
            let _ = w.set_size(tauri::PhysicalSize::new(lw, lh));
        }
        return;
    }
    if let Some(s) = app.try_state::<LastGood>() {
        *s.0.lock().unwrap() = Some((sz.width, sz.height));
    }
    winstate::save(&w);
}

/// Append a diagnostic line when debug logging is on. Delegates to config so the
/// log lands in the app data dir (`%LOCALAPPDATA%\Greedout`) like every other
/// sidecar; writing it here to `~/.claude/greedout` was recreating the very dir
/// the 0.2.3 move exists to vacate.
fn log_line(cfg: &config::Config, msg: &str) {
    config::debug_log(cfg, msg);
}

/// Bring the window back from the tray.
fn reveal(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        // If the window was hidden while collapsed to the Win+D sliver, Show would
        // otherwise bring the sliver back, at the off-screen spot it collapsed to.
        if let Ok(sz) = w.inner_size() {
            if winstate::too_small(sz.width, sz.height) {
                if let Some((lw, lh)) = last_good(app) {
                    let _ = w.set_size(tauri::PhysicalSize::new(lw, lh));
                }
                let _ = w.center();
            }
        }
        let _ = w.set_focus();
    }
}

/// Return the current snapshot immediately (used to prime the UI on mount).
/// Build timestamp (seconds since the epoch), stamped by build.rs. Shown in About
/// beside the version.
#[tauri::command]
fn build_epoch() -> u64 {
    env!("GREEDOUT_BUILD_EPOCH").parse().unwrap_or(0)
}

#[tauri::command]
fn get_sessions() -> Vec<Session> {
    let cfg = config::load_config();
    let labels = config::load_labels();
    scan::scan(&cfg, &labels)
}

/// The full context/spend curve for one session, rebuilt from its transcript,
/// so the graph can show history from before Greedout was opened.
#[tauri::command]
fn get_history(id: String) -> Vec<scan::Sample> {
    scan::session_history(&id)
}

/// Per-day, per-project spend totals for the Daily Spend overview (months + days).
/// A tiny aggregate read straight from the persistent per-turn cache (shared with
/// the Context Explorer); returns empty until the first enrich scan has populated it.
#[tauri::command]
fn get_spend_summary() -> Vec<scan::SpendSummary> {
    let t0 = std::time::Instant::now();
    let rows = browse::spend_summary();
    config::debug_log(
        &config::load_config(),
        &format!("spend_summary: {} rows in {}ms", rows.len(), t0.elapsed().as_millis()),
    );
    rows
}

/// The deduped billed turns for one local day (the `[lo, hi)` epoch-ms range the
/// window computes), for the swim lanes. Loaded lazily when a day is opened so the
/// window never ships the whole ~100k-turn history at once.
#[tauri::command]
fn get_spend_day(lo: i64, hi: i64) -> Vec<scan::SpendEvent> {
    let t0 = std::time::Instant::now();
    let rows = browse::spend_day(lo, hi);
    config::debug_log(
        &config::load_config(),
        &format!("spend_day: {} rows in {}ms", rows.len(), t0.elapsed().as_millis()),
    );
    rows
}

/// Ensure the shared cache exists and (re)scan so Daily Spend has per-turn data.
/// Always deep -- the turns table is only written by the enrich pass. Reuses the
/// browse scan machinery (progress via "browse-progress", cancel via browse_cancel)
/// and is a no-op if a scan is already running. Independent of the browse opt-in:
/// opening Daily Spend builds the cache without flipping the user's setting.
#[tauri::command]
fn spend_scan(app: tauri::AppHandle) -> Result<(), String> {
    browse::enable()?;
    browse::run_scan(app, true);
    Ok(())
}

/// Estimate the base-context breakdown (system prompt, tools, MCP, memory) for
/// one session, reconciled against its real first-turn total.
#[tauri::command]
fn analyze_baseline(id: String) -> baseline::Report {
    let cfg = config::load_config();
    let r = baseline::analyze(&id);
    config::debug_log(
        &cfg,
        &format!(
            "baseline: analyze {} -> ok={} needsContext={} total={} nodes={}",
            id,
            r.ok,
            r.needs_context,
            r.total,
            r.nodes.len()
        ),
    );
    r
}

/// Estimated Role/Turn/content breakdown of the Chat bucket for one session.
#[tauri::command]
fn chat_breakdown(id: String) -> Vec<baseline::Node> {
    baseline::chat_breakdown(&id)
}

/// Full content of one Chat message block, for the click-to-open panel.
/// Returns `[label, content]`.
#[tauri::command]
fn chat_block(id: String, gid: u64) -> (String, String) {
    baseline::chat_block(&id, gid)
}

/// Turn number of the first message block matching `query`, so the Context
/// Explorer can highlight the turn a search hit came from. `null` if no match.
#[tauri::command]
fn search_first_turn(id: String, query: String) -> Option<u32> {
    baseline::search_first_turn(&id, &query)
}

/// Frontend trace sink: routes UI-side baseline logs into greedout.log, gated by
/// the same `debug_logging` setting as the backend traces.
#[tauri::command]
fn baseline_log(msg: String) {
    // Cheap gate first: load_config() reads and parses config.json, so calling it
    // before testing the flag paid disk cost on every UI trace even with logging off.
    if !config::debug_enabled() {
        return;
    }
    config::debug_log_raw(&format!("baseline[ui]: {msg}"));
}

/// Persist the UI zoom factor (set via Ctrl+wheel) so it survives restarts.
/// Faces installed on this machine, offered alongside the bundled ones.
#[tauri::command]
fn system_fonts() -> Vec<String> {
    sysfonts::installed()
}

#[tauri::command]
fn set_ui_scale(scale: f64) -> Result<(), String> {
    let mut cfg = config::load_config();
    cfg.ui_scale = scale;
    config::save_config(&cfg).map_err(|e| e.to_string())
}

/// Set or clear a session's custom label.
#[tauri::command]
fn set_label(id: String, label: Option<String>) -> Result<(), String> {
    config::save_label(&id, label).map_err(|e| e.to_string())
}

/// Saved geometry for a secondary window, so the frontend can restore its size and
/// position (and clamp it on-screen) right after creating it. `None` if never saved.
/// Main restores in `setup`; the JS-created windows never pass through it, so they
/// read their entry back here.
#[tauri::command]
fn load_window_state(label: String) -> Option<winstate::WinState> {
    winstate::get(&label)
}

// --- Cross-session browse (opt-in; see browse.rs) ---

/// Whether browsing is enabled, whether the cache DB exists, and how far the
/// index/enrich has gotten. Drives the browse UI's gate and progress display.
#[tauri::command]
fn browse_status() -> browse::BrowseStatus {
    browse::status(config::load_config().browse_enabled)
}

/// Opt in: flip the setting, create the cache DB, and start the first scan.
/// `deep` also enriches every session now (the "deluxe" full scan) vs a fast
/// index-only pass that enriches lazily on drill-in.
#[tauri::command]
fn browse_enable(app: tauri::AppHandle, deep: bool) -> Result<(), String> {
    let mut cfg = config::load_config();
    cfg.browse_enabled = true;
    config::save_config(&cfg).map_err(|e| e.to_string())?;
    browse::enable()?;
    browse::run_scan(app, deep);
    Ok(())
}

/// Rescan (index always; enrich everything when `deep`). Emits "browse-progress".
#[tauri::command]
fn browse_scan(app: tauri::AppHandle, deep: bool) {
    browse::run_scan(app, deep);
}

/// Delete the cross-session cache (`cache.sqlite` + its WAL/SHM sidecars) so the
/// index and per-turn spend history rebuild from scratch on the next scan. The
/// live gauge list is unaffected (it reads transcripts directly, not the cache).
/// Best-effort per file; only a still-present main DB after the attempt is an error.
#[tauri::command]
fn clear_cache() -> Result<(), String> {
    let db = config::cache_db_path();
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            db.clone()
        } else {
            db.with_extension(format!("sqlite{suffix}"))
        };
        let _ = std::fs::remove_file(&p);
    }
    if db.exists() {
        return Err("Could not clear the cache (it may be in use).".into());
    }
    // The file is gone; reset the schema flag so the next open recreates the tables
    // instead of opening an empty, table-less DB that fails every scan and query.
    browse::on_cache_cleared();
    Ok(())
}

/// Cancel an in-flight scan; the partial index stays usable.
#[tauri::command]
fn browse_cancel() {
    browse::cancel();
}

#[tauri::command]
fn browse_harnesses() -> Vec<browse::HarnessAgg> {
    browse::harnesses()
}

#[tauri::command]
fn browse_projects(harness: String) -> Vec<browse::ProjectAgg> {
    browse::projects(&harness)
}

#[tauri::command]
fn browse_sessions(harness: String, project: String) -> Vec<browse::SessionMeta> {
    browse::sessions(&harness, &project)
}

/// Enrich a project's stale sessions in the background (tokens/cost/model). Returns
/// immediately; progress arrives on "browse-progress". Read tiles via browse_sessions.
#[tauri::command]
fn browse_enrich_project(app: tauri::AppHandle, harness: String, project: String) {
    browse::run_enrich_project(app, harness, project);
}

#[tauri::command]
fn browse_session(id: String) -> Option<browse::SessionMeta> {
    browse::session(&id)
}

/// Search every session's chat text (not tools). Streams "search-hit" and
/// "search-progress" events; cancels any prior search.
#[tauri::command]
fn browse_search(app: tauri::AppHandle, query: String) {
    browse::run_search(app, query);
}

/// Cancel an in-flight search.
#[tauri::command]
fn browse_search_cancel() {
    browse::cancel_search();
}

/// The current settings, for the Settings panel to edit.
#[tauri::command]
fn get_config() -> config::Config {
    config::load_config()
}

/// Persist edited settings. The poll loop reloads config each tick, so changes
/// take effect on the next poll without a restart.
#[tauri::command]
fn set_config(app: tauri::AppHandle, config: config::Config) -> Result<(), String> {
    config::save_config(&config).map_err(|e| e.to_string())?;
    // Apply the settings that affect the live window/tray right away.
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_skip_taskbar(!config.show_in_taskbar);
        let _ = w.set_always_on_top(config.always_on_top);
    }
    // Keep every secondary window in the same on-top band as main, else turning
    // "always on top" on would sink them behind the gauge window (which you are
    // usually editing from right then), making them hard to move or close.
    // One list, so a new window cannot be forgotten here.
    for label in ["settings", "about", "baseline", "themes", "fonts"] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.set_always_on_top(config.always_on_top);
        }
    }
    if let Some(tray) = app.try_state::<TrayState>() {
        if let Ok(t) = tray.0.lock() {
            let _ = t.set_visible(config.show_in_tray);
        }
    }
    log_line(&config, "settings saved");
    Ok(())
}

/// Build the tray's Show/Quit menu in a given language. Split out of `setup` so
/// a language change can rebuild it without restarting the app.
fn tray_menu<R: tauri::Runtime, M: tauri::Manager<R>>(
    app: &M,
    tag: &str,
) -> tauri::Result<Menu<R>> {
    let (show_label, quit_label) = i18n::tray_labels(tag);
    let show = MenuItem::with_id(app, "show", show_label, true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", quit_label, true, None::<&str>)?;
    Menu::with_items(app, &[&show, &quit])
}

/// Persist the language choice and relabel the tray.
///
/// Two values, not one: `locale` stays whatever the picker says, including
/// "auto", so the picker still reads Auto (system) next launch. `resolved` is
/// what "auto" actually resolved to in the webview, cached because `setup()`
/// runs before any webview exists to be asked.
/// Open a link in the user's browser.
///
/// `rundll32 url.dll,FileProtocolHandler` rather than the opener plugin: it is
/// one line against a DLL that ships with Windows, where the plugin is a crate,
/// an npm package and a capability entry to open one hard-coded address.
///
/// https and mailto only, and refuses anything with whitespace in it. The callers
/// pass literals, but this is the frontend handing the shell a string, so the
/// check belongs here rather than at the call site.
#[tauri::command]
fn open_url(url: String) {
    let scheme_ok = url.starts_with("https://") || url.starts_with("mailto:");
    if !scheme_ok || url.split_whitespace().count() != 1 {
        return;
    }
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", &url])
        .spawn();
}

#[tauri::command]
fn set_locale(app: tauri::AppHandle, locale: String, resolved: String) -> Result<(), String> {
    let mut cfg = config::load_config();
    cfg.locale = locale;
    cfg.locale_resolved = resolved;
    config::save_config(&cfg).map_err(|e| e.to_string())?;
    // No-op before the tray exists, and harmless when the tray is hidden: the
    // menu is relabeled either way, so unhiding it later shows the new language.
    if let Some(tray) = app.try_state::<TrayState>() {
        if let Ok(t) = tray.0.lock() {
            if let Ok(menu) = tray_menu(&app, &cfg.locale_resolved) {
                let _ = t.set_menu(Some(menu));
            }
        }
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Three things happen before the Tauri builder exists, because none of them
    // wants a window. Add/Remove Programs invokes us as `greedout.exe
    // --uninstall`, and that path never returns.
    if std::env::args().any(|a| a == "--uninstall") {
        install::run_uninstall();
    }
    // Then sweep up the copy a self-update left beside us last time, before
    // anything else can hold a handle on this directory.
    update::cleanup_old();
    // Move our sidecars out of `~/.claude/greedout` (pre-0.2.3 location) into our
    // own LOCALAPPDATA dir, so we never write inside Claude Code's config home.
    config::migrate_sidecars();
    // Fresh log each run so it can't grow without bound when debug logging is left on.
    config::truncate_log();

    tauri::Builder::default()
        // MUST be the first plugin registered. A second launch would otherwise get its
        // own tray icon and its own poll loop reading the same transcripts; instead the
        // new process hands off and the running window comes to the front.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            reveal(app);
        }))
        .invoke_handler(tauri::generate_handler![
            open_url,
            update::update_check,
            update::update_apply,
            get_sessions,
            get_history,
            get_spend_summary,
            get_spend_day,
            spend_scan,
            analyze_baseline,
            chat_breakdown,
            chat_block,
            search_first_turn,
            baseline_log,
            set_ui_scale,
            system_fonts,
            set_label,
            load_window_state,
            get_config,
            set_config,
            browse_status,
            browse_enable,
            browse_scan,
            clear_cache,
            browse_cancel,
            browse_harnesses,
            browse_projects,
            browse_sessions,
            browse_enrich_project,
            browse_session,
            browse_search,
            browse_search_cancel,
            build_epoch,
            set_locale,
            install::setup_state,
            install::perform_install,
            install::launch_installed_and_exit
        ])
        .setup(|app| {
            // Installer mode: this exe is sitting in a downloads folder, not in the
            // install directory. Show a card and nothing else.
            //
            // The early return is the whole point. Everything below it (the tray, the
            // poll loop, the saved window geometry) belongs to the installed app: a
            // card with a scanner running behind it and a tray icon beside it is not
            // an installer, and restoring a 340x260 gauge geometry onto the card
            // would fight the size the card asks for.
            if install::needs_setup() {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_decorations(false);
                    let _ = w.set_always_on_top(false);
                    let _ = w.set_resizable(false);
                    let _ = w.set_size(tauri::LogicalSize::new(452.0, 432.0));
                    let _ = w.center();
                    let _ = w.show();
                    let _ = w.set_focus();
                }
                return Ok(());
            }

            let cfg = config::load_config();
            log_line(&cfg, "greedout started");
            app.manage(LastGood(Mutex::new(None)));

            // Reflect the taskbar setting (overrides the static config default).
            if let Some(w) = app.get_webview_window("main") {
                // Belt and braces: `decorations: false` in tauri.conf.json has been
                // observed not to stick on Windows, and a title bar appearing on a
                // window whose CSS assumes there is none is a visible break.
                let _ = w.set_decorations(false);
                let _ = w.set_skip_taskbar(!cfg.show_in_taskbar);
                let _ = w.set_always_on_top(cfg.always_on_top);
                winstate::restore(&w);
                // Seed the last-good size from whatever the window ended up at, so a
                // Win+D before the user has ever resized still has a size to recover to.
                if let Ok(sz) = w.inner_size() {
                    if !winstate::too_small(sz.width, sz.height) {
                        *app.state::<LastGood>().0.lock().unwrap() = Some((sz.width, sz.height));
                    }
                }
            }

            // System-tray icon: left-click reveals the window; menu has Show/Quit.
            let menu = tray_menu(app, &cfg.locale_resolved)?;
            // The icon is a build invariant (it comes from the bundle config), so
            // this should never be None. Fall back to an icon-less tray anyway
            // rather than panicking: an unwrap here is the only avoidable panic in
            // the startup path, and losing the tray glyph beats losing the app.
            let mut builder = TrayIconBuilder::new();
            if let Some(icon) = app.default_window_icon() {
                builder = builder.icon(icon.clone());
            }
            let tray = builder
                .tooltip("Greedout")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => reveal(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        reveal(tray.app_handle());
                    }
                })
                .build(app)?;
            let _ = tray.set_visible(cfg.show_in_tray);
            app.manage(TrayState(Mutex::new(tray)));

            let handle = app.handle().clone();
            // Background poll loop: rescan every poll_seconds and push a snapshot.
            std::thread::spawn(move || {
                // System-stats sampler for the optional status bar. Kept across ticks
                // so per-core CPU deltas are measured against the previous refresh.
                let mut sampler = stats::StatsSampler::new();
                loop {
                // Profiling: time each phase of a poll cycle so we can see whether
                // latency lives in config/label reads, the transcript scan, or the
                // IPC emit. Only written when debug_logging is on (log_line gate).
                let t_cfg = Instant::now();
                let cfg = config::load_config();
                let labels = config::load_labels();
                let cfg_ms = t_cfg.elapsed().as_secs_f64() * 1000.0;

                let t_scan = Instant::now();
                let sessions = scan::scan(&cfg, &labels);
                let scan_ms = t_scan.elapsed().as_secs_f64() * 1000.0;
                let n = sessions.len();

                let t_emit = Instant::now();
                let _ = handle.emit("sessions", &sessions);
                let emit_ms = t_emit.elapsed().as_secs_f64() * 1000.0;

                // Only sample + emit system stats when the status bar is on, so the
                // sysinfo refresh costs nothing for users who keep it hidden.
                if cfg.show_statusbar {
                    let _ = handle.emit("sysstats", sampler.sample());
                }

                log_line(
                    &cfg,
                    &format!(
                        "poll cfg={cfg_ms:.1}ms scan={scan_ms:.1}ms emit={emit_ms:.1}ms \
                         sessions={n} interval={:.0}ms",
                        cfg.poll_seconds * 1000.0
                    ),
                );
                std::thread::sleep(Duration::from_secs_f64(cfg.poll_seconds.max(0.1)));
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            // In installer mode there is no tray, so the close- and minimize-to-tray
            // handlers below would hide the card with nothing left to restore it.
            if install::needs_setup() {
                return;
            }
            match event {
                // Close-to-tray: swallow the close and hide instead of quitting.
                // Main window ONLY. Without that guard the settings/about/explorer/picker
                // windows are never really closed, just hidden: their webviews and every
                // `listen` handler in them keep running, and "disable the Explorer closes
                // it" silently stops working.
                tauri::WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                    // Closing the main window tears down the whole UI: close every child
                    // window (settings/about/explorer/spend/pickers) so none is left
                    // orphaned on screen when main goes away or hides to tray.
                    let app = window.app_handle();
                    for (label, w) in app.webview_windows() {
                        if label != "main" {
                            let _ = w.close();
                        }
                    }
                    if config::load_config().close_to_tray {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
                // The shared transcript scan (Context Explorer + Daily Spend) exists
                // only to feed those two windows. When the last of them closes, cancel
                // any in-flight scan so it stops working in the background. The rows it
                // already wrote persist, so reopening a window resumes the scan from
                // where it left off rather than restarting. Guarded on "another
                // reporting window still open" so closing one while the other is up
                // leaves the scan running.
                tauri::WindowEvent::Destroyed
                    if matches!(window.label(), "baseline" | "dailyspend") =>
                {
                    let app = window.app_handle();
                    let others_open = app.webview_windows().keys().any(|l| {
                        l != window.label() && matches!(l.as_str(), "baseline" | "dailyspend")
                    });
                    if !others_open {
                        browse::cancel();
                    }
                }
                // Minimize-to-tray: a minimize arrives as a resize; hide when minimized.
                // Also persist size/position now, since an external restart can kill the
                // process before the plugin's save-on-exit can run.
                tauri::WindowEvent::Resized(_) if window.label() == "main" => {
                    if window.is_minimized().unwrap_or(false) {
                        if config::load_config().minimize_to_tray {
                            // A restore animation fires transient minimized frames. Hiding on
                            // the first one makes the window animate back to full size and
                            // then disappear, so wait and only hide if it really stayed down.
                            let w = window.clone();
                            std::thread::spawn(move || {
                                std::thread::sleep(Duration::from_millis(200));
                                if w.is_minimized().unwrap_or(false) {
                                    // Hide first, then clear the minimized flag. The other
                                    // order lets Windows animate the window back to full size
                                    // before it vanishes.
                                    let _ = w.hide();
                                    let _ = w.unminimize();
                                }
                            });
                        }
                    } else {
                        track_geometry(window.app_handle());
                    }
                }
                tauri::WindowEvent::Moved(_) if window.label() == "main" => {
                    track_geometry(window.app_handle());
                }
                // Every other persisted window: remember size and position so it
                // reopens where the user left it. Main has its own Win+D repair
                // above; these windows have decorations and a min size, so a plain
                // save is enough. Skip while minimized so a taskbar-minimize does not
                // overwrite the real geometry with the minimized rectangle.
                // About is non-resizable and always opens at its default size next
                // to main, so it is deliberately not in this list: persisting it only
                // pinned a stale size that masked a later default change.
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Moved(_)
                    if matches!(
                        window.label(),
                        "settings" | "baseline" | "dailyspend" | "themes" | "fonts"
                    ) =>
                {
                    if !window.is_minimized().unwrap_or(false) {
                        if let Some(w) = window.app_handle().get_webview_window(window.label()) {
                            winstate::save(&w);
                        }
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Greedout");
}
