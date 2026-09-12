// English is both the source catalog and the schema every other language is typed
// against. Flat dotted keys: the dots are a naming convention, not a structure.
//
// WHAT DOES NOT GET TRANSLATED, decided once here rather than re-litigated per
// string (see the shared localization pattern for the full reasoning):
//
//   - The brand. "greedout" is a name.
//   - The author's name, and "Rust", "Tauri", "Svelte", "Claude", "Claude Code".
//     Proper nouns.
//   - Theme names (Nord, Dracula, Sakura) and font family names. Names again, and
//     they are ids in the config file besides.
//   - Model names, session ids, project paths and file names: they exist on disk
//     or on the wire with those spellings.
//   - "MB", "GB", "k", "$". Unit symbols, not words. "sec" and "hr" ARE words and
//     are translated; the compact time-ago suffixes are too, because a language
//     that abbreviates minutes differently should be able to say so.
//   - Numbers, percentages and money figures. Formatting is Intl's job.
//
// Surfaces not yet extracted: the Context Explorer, the theme browser and the font
// picker. The Explorer copy is still moving, and extracting a string that is about
// to be rewritten wastes the translation. Its strings are still English literals.

export type Plural = { zero?: string; one?: string; two?: string; few?: string; many?: string; other: string };
export type Msg = string | Plural;

export const en = {
  // Titlebar menu and window controls
  "menu.menu": "Menu",
  "menu.closeMenu": "Close menu",
  "menu.explorer": "Context Explorer…",
  "menu.dailySpend": "Daily Spend…",
  "menu.settings": "Settings…",
  "menu.about": "About…",
  "win.minimize": "Minimize",
  "win.close": "Close",

  // The gauge list
  "main.noSessions": "No active sessions",
  "main.renamePrompt": "Rename “{title}”",
  "main.exploreOrRename": "Click to explore · double-click to rename",
  "main.rename": "Double-click to rename",
  "main.exploreProject": "Explore this project",
  // Value labels prefixed to the session-title and project tooltips.
  "main.sessionTip": "Session: {title}",
  "main.projectTip": "Project: {path}",
  // Focus panel: the two context limits, and the labels the history graph hands
  // back when a line is hovered.
  "main.limits": "target {target} · max {max}",
  "main.spend": "spend",
  "main.compacted": "Compacted",
  "main.compactRead": "read summary",
  "main.compactTip": "Context was compacted. Click to read the summary.",
  "main.compactEncrypted": "summary encrypted",
  "main.compactEncryptedTip": "Context was compacted. Codex encrypts the compaction summary on disk, so it cannot be shown.",
  "main.context": "context",
  "main.startupFloor": "startup floor",

  // Compact time since the last message. Kept short on purpose: these sit in a
  // 340px-wide window beside the project name.
  "ago.seconds": "{n}s",
  "ago.minutes": "{n}m",
  "ago.hours": "{n}h",
  "ago.days": "{n}d",
  "ago.weeks": "{n}w",

  // Readout tooltips. One place, so a number means the same thing wherever it
  // appears: a row, the focus panel, the Explorer.
  "tip.money":
    "Nominal: estimated from this session's tokens at published per-token prices, priced per turn by the model that ran it. On a flat-rate plan you are not charged this; it is what the same work would cost through the API.",
  "tip.spend": "{amount} so far. {money}",
  "tip.ago": "Time since the last message in this session",
  "tip.ctx":
    "Context carried by the most recent message, against the target the gauge is scaled to",
  "tip.pct": "Context in the most recent message, as a share of the target",
  "tip.model": "Model that ran the most recent turn",
  "tip.size": "Size of this session's transcript file on disk",
  "tip.limits": "Target is where the gauge redlines; max is the model's hard context window",
  "tip.gauge": "context fill gauge",
  "tip.history": "context and spend over time",

  // Status bar
  "status.cpu": "CPU",
  "status.mem": "MEM",
  "status.cpuTip": "per-core CPU utilization",
  "status.memTip": "memory {used} / {total} GB",

  // Settings window
  "settings.title": "Settings",
  "settings.followFocus": "Follow the focused session",
  "settings.gaugePerHarness": "One large gauge per harness",
  "settings.alwaysOnTop": "Always on top",
  "settings.showInTray": "Show in tray",
  "settings.showInTaskbar": "Show in taskbar",
  "settings.minimizeToTray": "Minimize to tray",
  "settings.closeToTray": "Close to tray",
  "settings.showStatusbar": "Show CPU / memory status bar",
  "settings.showPrompt": "Show prompt text",
  "settings.clickableTitles": "Clickable links in main window",
  "settings.checkUpdates": "Check for updates at launch",
  "settings.debugLogging": "Debug logging to file",
  "settings.lockoutTip":
    "Can't turn this off: it is the only way left to reach the window. Enable the other one first.",
  "settings.language": "Language",
  "settings.languageAuto": "Auto (system)",
  "settings.theme": "Theme",
  "settings.fonts": "Fonts",
  "settings.boldness": "Boldness",
  "settings.opacity": "Opacity",
  "settings.maxSessions": "Max sessions shown",
  "settings.refreshEvery": "Refresh every",
  "settings.dimAfter": "Fully dimmed after",
  "settings.budget": "Gauge budget (k tokens)",
  "settings.budgetHint": "The context each gauge treats as full, for every session and tool.",
  // Hover tips (native title=) for every control. Missing translations fall back to
  // English, so these ship immediately in all languages; the locale gate treats a
  // missing key as a soft warning, not a failure.
  "settings.followFocusTip":
    "Keep the session you're currently working in pinned to the top of the list.",
  "settings.gaugePerHarnessTip":
    "Show one big gauge for the focused session's harness instead of a row per session. Needs Follow the focused session.",
  "settings.alwaysOnTopTip": "Keep the Greedout window above other windows so it stays visible.",
  "settings.showInTrayTip": "Show a Greedout icon in the system tray; right-click it for quick actions.",
  "settings.showInTaskbarTip": "Show Greedout as a button on the taskbar.",
  "settings.minimizeToTrayTip": "Minimizing hides the window to the tray instead of the taskbar.",
  "settings.closeToTrayTip": "Closing the window hides it to the tray instead of quitting Greedout.",
  "settings.showStatusbarTip": "Show a bottom bar with live CPU and memory usage.",
  "settings.showPromptTip": "Show each session's latest prompt text beneath its title.",
  "settings.clickableTitlesTip":
    "Make session and project titles in the main window clickable links that open the Context Explorer.",
  "settings.checkUpdatesTip":
    "Check for a new version each time Greedout launches. About can always check on demand.",
  "settings.debugLoggingTip":
    "Write diagnostic logs to a file for troubleshooting. Leave off for normal use.",
  "settings.languageTip": "Interface language. Auto follows your system setting.",
  "settings.themeTip": "Open the theme browser to change the color scheme.",
  "settings.fontsTip": "Open the font picker to change the interface, number, and monospace fonts.",
  "settings.boldnessTip": "Overall font weight across the interface, from lightest to boldest.",
  "settings.opacityTip": "Window transparency. Lower is more see-through.",
  "settings.refreshEveryTip": "How often Greedout re-reads sessions to update the gauges.",
  "settings.dimAfterTip": "How long an idle session waits before it is fully dimmed in the list.",
  "settings.resetAll": "Reset all",
  "settings.resetAllTip": "Reset every setting to its default",
  "settings.clearCache": "Clear cache",
  "settings.clearCacheTip": "Delete the cross-session index and spend cache (rebuilds on next scan)",
  "settings.clearCacheDone": "Cache cleared",

  // The five rungs of the boldness slider, lightest to boldest.
  "bold.lightest": "Lightest",
  "bold.lighter": "Lighter",
  "bold.normal": "Normal",
  "bold.bolder": "Bolder",
  "bold.boldest": "Boldest",

  "unit.sec": "sec",
  "unit.hr": "hr",

  // About window
  "about.version": "Version {version}",
  "about.built": "built {date}",
  "about.tagline": "AI spend readout, reporting, and session history explorer",
  "about.body":
    "Greedout reads AI coding session transcripts directly and shows context fill, spend, and history as an always-on-top dashboard. Claude Code is the harness it supports today: the status line its Windows desktop app can't render on its own.",
  "about.madeBy": "Made by",
  "about.builtWith": "Built with",
  "about.license": "License",
  "about.notices": "Third-party notices",

  // The first-run install card. Only ever seen by someone who downloaded the raw
  // exe, so it says what the app is about to do to their machine before it does it.
  "install.heading": "Install",
  "install.update": "Update",
  "install.location": "Location",
  "install.desktopShortcut": "Also add a desktop shortcut",
  "install.working": "Installing…",
  "install.failed": "Install failed: {error}",
  "install.retry": "Try again",
  "install.starting": "Installed. Starting greedout…",
  "install.pokeTip": "Hover to rev · click to reskin",

  // The update banner and the manual check in About. The banner is the only
  // thing in the app that appears without the user asking for it, so it says
  // what it wants in one line and can be dismissed.
  "update.available": "Version {version} is available. You have {current}.",
  "update.install": "Download and install",
  "update.downloading": "Downloading {version}…",
  "update.installed": "Installed. Restarting…",
  "update.failed": "Update failed: {error}",
  "update.dismiss": "Not now",
  "update.check": "Check for updates",
  "update.checking": "Checking…",
  "update.upToDate": "You are on the latest version.",

  "common.close": "Close",
  "common.loading": "Loading…",

  "compact.title": "Compaction Summary",
  "compact.loading": "Loading summary…",
  "compact.none": "No compaction summary available for this session.",
  "compact.tokens": "tokens",
} as const;

// `Record<keyof typeof en, Msg>` rather than `typeof en`: `as const` makes the
// values string literals, and without the widening a translation would have to
// equal the English string to type-check.
export type Dict = Record<keyof typeof en, Msg>;

// Partial is deliberate. A translation that lags English falls back key by key at
// runtime, so "ship the feature now, land the translation next week" is a
// supported state rather than a broken build. The CI gate reports a missing key
// as a warning and a renamed one as a hard failure.
export type PartialDict = Partial<Dict>;
