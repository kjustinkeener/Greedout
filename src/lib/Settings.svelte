<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import type { Config } from "../types";
  import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { onDestroy } from "svelte";
  import { applyTheme, THEMES, type Theme } from "./theme";
  import {
    applyFonts,
    clampSize,
    DEFAULT_FONTS,
    fontById,
    type FontPrefs,
  } from "./fonts";
  import Brand from "./Brand.svelte";
  import { LOCALES, localeChoice, setLocale, t } from "./i18n.svelte";
  import type { Dict } from "./locales/en";

  // The five rungs of the boldness slider, lightest to boldest. Keys, not
  // translated strings: resolving them here would freeze the language at load.
  const BOLD_KEYS: (keyof Dict)[] = [
    "bold.lightest",
    "bold.lighter",
    "bold.normal",
    "bold.bolder",
    "bold.boldest",
  ];

  // Persist the language, then tell every other window to switch with it.
  async function pickLocale(v: string) {
    await setLocale(v);
    void emit("settings:locale", v);
  }

  // Open one of the picker windows (theme browser, font picker) in its own
  // window, reusing it if already open. Each picker persists the choice itself
  // and emits an event the listeners below fold back into cfg.
  async function openPicker(label: string, title: string, w: number, h: number) {
    const existing = await WebviewWindow.getByLabel(label);
    if (existing) {
      try {
        await existing.unminimize();
        await existing.show();
        await existing.setFocus();
        return;
      } catch {
        try {
          await existing.close();
        } catch {
          // already gone
        }
      }
    }
    let pos: { x: number; y: number } | undefined;
    try {
      const me = getCurrentWindow();
      const sf = await me.scaleFactor();
      const p = (await me.outerPosition()).toLogical(sf);
      pos = { x: Math.round(p.x + 20), y: Math.round(p.y + 20) };
    } catch {
      // Position unavailable; let the OS place it.
    }
    const win = new WebviewWindow(label, {
      url: `${label}.html`,
      title,
      width: w,
      height: h,
      minWidth: 320,
      minHeight: 300,
      resizable: true,
      alwaysOnTop: cfg?.always_on_top ?? true,
      focus: true,
      ...(pos ? { x: pos.x, y: pos.y } : {}),
    });
    void win.once("tauri://error", async () => {
      const stale = await WebviewWindow.getByLabel(label);
      if (stale) {
        try {
          await stale.close();
        } catch {
          // already gone
        }
      }
    });
  }

  const openThemes = () => openPicker("themes", `Greedout ${t("settings.theme")}`, 600, 620);
  const openFonts = () => openPicker("fonts", `Greedout ${t("settings.fonts")}`, 820, 680);

  // The themes window persists the choice itself and emits "theme"; mirror it
  // into this panel so the button label and preview stay in sync.
  const unlistenTheme = listen<Theme>("theme", (e) => {
    if (cfg) cfg.theme = e.payload;
    applyTheme(e.payload);
  });
  onDestroy(() => void unlistenTheme.then((u) => u()));

  // Same for fonts: the picker owns the choice, this panel mirrors the label.
  const unlistenFonts = listen<FontPrefs>("fonts", (e) => {
    if (cfg) cfg = { ...cfg, ...e.payload };
    applyFonts(e.payload);
  });
  onDestroy(() => void unlistenFonts.then((u) => u()));

  let { onClose }: { onClose: () => void } = $props();

  let cfg = $state<Config | null>(null);

  // Default values (mirror `Config::default()` in src-tauri/src/config.rs). Used by
  // right-click-to-reset (per field) and the Reset all button. `ui_scale` is the
  // window zoom, not a settings field, so it's preserved rather than reset.
  // `browse_enabled` is excluded too: it's a data-index toggle, not a cosmetic
  // setting, so "Reset all" must not silently drop the built cross-session index.
  // `font_sets` likewise: those are the user's own saved sets, not a setting to
  // restore, and resetting the look should not throw them away.
  const DEFAULTS: Omit<Config, "ui_scale" | "browse_enabled" | "font_sets"> = {
    N: 5,
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
    dim_hours: 24,
    theme: "dark",
    show_statusbar: true,
    show_prompt: true,
    clickable_titles: true,
    check_updates: true,
    ...DEFAULT_FONTS,
  };

  // Right-click a control to reset just that field to its default (saves live).
  function resetField(key: keyof typeof DEFAULTS, ev?: Event) {
    ev?.preventDefault();
    if (!cfg) return;
    (cfg as unknown as Record<string, unknown>)[key] = DEFAULTS[key];
    if (key === "opacity") previewOpacity(cfg.opacity);
    if (key === "theme") previewTheme(cfg.theme);
    if (String(key).startsWith("font_")) previewFonts();
    commit(true);
  }

  // Reset every field to default (preview opacity/theme live; keeps current zoom).
  function resetAll() {
    if (!cfg) return;
    cfg = { ...cfg, ...DEFAULTS };
    previewOpacity(cfg.opacity);
    previewTheme(cfg.theme);
    previewFonts();
    commit(true);
  }

  // Apply the font preferences here and in every other window.
  function previewFonts() {
    if (!cfg) return;
    const p: FontPrefs = {
      font_ui: cfg.font_ui,
      font_num: cfg.font_num,
      font_mono: cfg.font_mono,
      font_weight: cfg.font_weight,
      size_ui: cfg.size_ui,
      size_num: cfg.size_num,
      size_mono: cfg.size_mono,
    };
    applyFonts(p);
    void emit("fonts", p);
  }

  // Ask the main window to preview an opacity live (this settings panel runs in
  // its own window, so it can't tint the gauge window directly).
  function previewOpacity(v: number) {
    void emit("opacity-preview", v);
  }

  // Preview a theme: apply to this settings window and ask the main window to
  // apply it too.
  function previewTheme(t: Theme) {
    applyTheme(t);
    void emit("theme", t);
  }

  // Load current settings when the dialog opens.
  (async () => {
    cfg = await invoke<Config>("get_config");
    applyTheme(cfg.theme);
    applyFonts(cfg);
  })();

  // Auto-save: every change persists immediately (no Save button). Text/slider
  // edits debounce so we don't spam set_config mid-drag; toggles save at once.
  // Turning off both the tray icon and the taskbar button leaves a decorationless
  // always-on-top window with no way to get it back if it is ever hidden. Whichever
  // one is the last visible entry point locks itself on.
  // A function, not a const: a captured `t()` would freeze the language at load.
  const lockoutTip = () => t("settings.lockoutTip");

  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  function commit(immediate = false) {
    if (!cfg) return;
    clearTimeout(saveTimer);
    const run = () => void persist();
    if (immediate) run();
    else saveTimer = setTimeout(run, 250);
  }

  async function persist() {
    if (!cfg) return;
    // Clamp to sane minimums so a stray 0 can't stall the poll loop.
    const clean: Config = {
      N: Math.max(1, Math.round(cfg.N)),
      poll_seconds: Math.max(0.1, cfg.poll_seconds),
      target_tokens: Math.max(1000, Math.round(cfg.target_tokens)),
      follow_focus: cfg.follow_focus,
      show_in_tray: cfg.show_in_tray,
      show_in_taskbar: cfg.show_in_taskbar,
      minimize_to_tray: cfg.minimize_to_tray,
      close_to_tray: cfg.close_to_tray,
      debug_logging: cfg.debug_logging,
      always_on_top: cfg.always_on_top,
      opacity: cfg.opacity,
      ui_scale: cfg.ui_scale,
      dim_hours: Math.max(1, cfg.dim_hours),
      theme: cfg.theme,
      // Preserved, not edited here: turning browsing on/off happens in the
      // Context Breakdown window. Omitting it would reset it to false on save.
      browse_enabled: cfg.browse_enabled,
      show_statusbar: cfg.show_statusbar,
      show_prompt: cfg.show_prompt,
      clickable_titles: cfg.clickable_titles,
      check_updates: cfg.check_updates,
      font_ui: cfg.font_ui,
      font_num: cfg.font_num,
      font_mono: cfg.font_mono,
      font_weight: Math.max(-2, Math.min(2, Math.round(cfg.font_weight))),
      size_ui: clampSize(cfg.size_ui),
      size_num: clampSize(cfg.size_num),
      size_mono: clampSize(cfg.size_mono),
      // Saved font sets are edited in the Fonts window; omitting them here
      // would empty them on any settings save.
      font_sets: cfg.font_sets ?? [],
    };
    await invoke("set_config", { config: clean });
    previewOpacity(clean.opacity); // keep the applied value on the main window
    void emit("dim-hours", clean.dim_hours); // live-apply on the main window
    void emit("show-statusbar", clean.show_statusbar); // live-toggle the status bar
    void emit("show-prompt", clean.show_prompt); // live-toggle prompt text in the main window
    void emit("clickable-titles", clean.clickable_titles); // live-toggle clickable titles
  }
</script>

<!-- Escape closes the window, the same as the Close button. This is a
     decorationless window, so without it the only way out is the button. -->
<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") onClose();
  }}
/>

<!-- Auto-save: input/change events from any control bubble here. `input` (typing,
     slider drag) debounces; `change` (toggle, select, blur) saves immediately. -->
<div class="panel" oninput={() => commit()} onchange={() => commit(true)}>
  <div class="head"><Brand size={16} font={15} /><span class="htxt">{t("settings.title")}</span></div>
  {#if !cfg}
    <div class="loading">{t("common.loading")}</div>
  {:else}
    <div class="cols">
    <div class="col">
    <label class="check" oncontextmenu={(e) => resetField("follow_focus", e)}>
      <input type="checkbox" bind:checked={cfg.follow_focus} />
      {t("settings.followFocus")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("always_on_top", e)}>
      <input type="checkbox" bind:checked={cfg.always_on_top} />
      {t("settings.alwaysOnTop")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("show_in_tray", e)}>
      <input
        type="checkbox"
        bind:checked={cfg.show_in_tray}
        disabled={cfg.show_in_tray && !cfg.show_in_taskbar}
        title={cfg.show_in_tray && !cfg.show_in_taskbar ? lockoutTip() : undefined}
      />
      {t("settings.showInTray")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("show_in_taskbar", e)}>
      <input
        type="checkbox"
        bind:checked={cfg.show_in_taskbar}
        disabled={cfg.show_in_taskbar && !cfg.show_in_tray}
        title={cfg.show_in_taskbar && !cfg.show_in_tray ? lockoutTip() : undefined}
      />
      {t("settings.showInTaskbar")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("minimize_to_tray", e)}>
      <input type="checkbox" bind:checked={cfg.minimize_to_tray} />
      {t("settings.minimizeToTray")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("close_to_tray", e)}>
      <input type="checkbox" bind:checked={cfg.close_to_tray} />
      {t("settings.closeToTray")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("show_statusbar", e)}>
      <input type="checkbox" bind:checked={cfg.show_statusbar} />
      {t("settings.showStatusbar")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("show_prompt", e)}>
      <input type="checkbox" bind:checked={cfg.show_prompt} />
      {t("settings.showPrompt")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("clickable_titles", e)}>
      <input type="checkbox" bind:checked={cfg.clickable_titles} />
      {t("settings.clickableTitles")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("check_updates", e)}>
      <input type="checkbox" bind:checked={cfg.check_updates} />
      {t("settings.checkUpdates")}
    </label>
    <label class="check" oncontextmenu={(e) => resetField("debug_logging", e)}>
      <input type="checkbox" bind:checked={cfg.debug_logging} />
      {t("settings.debugLogging")}
    </label>
    </div>

    <div class="col">
    <label class="field">
      <span>{t("settings.language")}</span>
      <select
        class="sel"
        value={localeChoice()}
        onchange={(e) => void pickLocale(e.currentTarget.value)}
      >
        <option value="auto">{t("settings.languageAuto")}</option>
        {#each LOCALES as loc}
          <option value={loc.id}>{loc.label}</option>
        {/each}
      </select>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("theme", e)}>
      <span>{t("settings.theme")}</span>
      <button class="sel themebtn" onclick={openThemes}>
        {THEMES.find((th) => th.id === cfg!.theme)?.label ?? cfg.theme}
        <span class="chev"><Icon name="chevron-right" size={11} /></span>
      </button>
    </label>

    <label class="field">
      <span>{t("settings.fonts")}</span>
      <button class="sel themebtn" onclick={openFonts}>
        {fontById(cfg.font_ui, "ui").label} / {fontById(cfg.font_num, "num").label}
        <span class="chev"><Icon name="chevron-right" size={11} /></span>
      </button>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("font_weight", e)}>
      <span>{t("settings.boldness")}</span>
      <input
        type="range"
        min="-2"
        max="2"
        step="1"
        value={cfg.font_weight}
        oninput={(e) => {
          cfg!.font_weight = Number(e.currentTarget.value);
          previewFonts();
        }}
      />
      <span class="pctval">{t(BOLD_KEYS[cfg.font_weight + 2])}</span>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("opacity", e)}>
      <span>{t("settings.opacity")}</span>
      <input
        type="range"
        min="0.3"
        max="1"
        step="0.02"
        value={cfg.opacity}
        oninput={(e) => {
          cfg!.opacity = Number(e.currentTarget.value);
          previewOpacity(cfg!.opacity);
        }}
      />
      <span class="pctval">{Math.round(cfg.opacity * 100)}%</span>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("N", e)}>
      <span>{t("settings.maxSessions")}</span>
      <span class="numwrap">
        <input type="number" min="1" bind:value={cfg.N} />
        <span class="unit"></span>
      </span>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("poll_seconds", e)}>
      <span>{t("settings.refreshEvery")}</span>
      <span class="numwrap">
        <input type="number" min="0.1" step="0.1" bind:value={cfg.poll_seconds} />
        <span class="unit">{t("unit.sec")}</span>
      </span>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("dim_hours", e)}>
      <span>{t("settings.dimAfter")}</span>
      <span class="numwrap">
        <input type="number" min="1" step="1" bind:value={cfg.dim_hours} />
        <span class="unit">{t("unit.hr")}</span>
      </span>
    </label>

    <label class="field" oncontextmenu={(e) => resetField("target_tokens", e)}>
      <span>{t("settings.budget")}</span>
      <span class="numwrap">
        <input
          type="number"
          min="1"
          value={Math.round(cfg.target_tokens / 1000)}
          oninput={(e) => (cfg!.target_tokens = Number(e.currentTarget.value) * 1000)}
        />
        <span class="unit"></span>
      </span>
    </label>
    <div class="hint">{t("settings.budgetHint")}</div>
    </div>
    </div>

    <div class="actions">
      <span class="spacer"></span>
      <button class="btn reset" onclick={resetAll} title={t("settings.resetAllTip")}>
        {t("settings.resetAll")}
      </button>
      <button class="btn primary" onclick={onClose}>{t("common.close")}</button>
    </div>
  {/if}
</div>

<style>
  :global(html),
  :global(body) {
    background: var(--bg);
  }
  .panel {
    min-height: 100vh;
    max-height: 100vh;
    overflow-y: auto;
    box-sizing: border-box;
    padding: 12px;
    color: var(--fg);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: var(--w-bold);
    color: var(--fg);
    margin-bottom: 8px;
  }
  .htxt {
    color: var(--muted);
    font-weight: var(--w-semibold);
  }
  .loading {
    color: var(--muted);
    padding: 8px 0;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--fg);
    margin-bottom: 8px;
    cursor: pointer;
  }
  /* Two columns: toggles on the left, value fields on the right, split by a
     vertical rule (the old horizontal divider is now that rule). Collapses to a
     single column if the window is dragged narrow. */
  .cols {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 16px;
    margin-bottom: 8px;
  }
  .col {
    min-width: 0;
  }
  .cols .col + .col {
    border-left: 1px solid var(--edge);
    padding-left: 16px;
  }
  @media (max-width: 419px) {
    .cols {
      grid-template-columns: 1fr;
    }
    .cols .col + .col {
      border-left: none;
      padding-left: 0;
      border-top: 1px solid var(--edge);
      padding-top: 8px;
    }
  }
  .sel {
    width: auto;
    max-width: 150px;
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 2px 4px;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    cursor: pointer;
  }
  .themebtn {
    display: inline-flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    text-align: left;
  }
  .themebtn:hover {
    background: var(--hover);
  }
  .themebtn .chev {
    color: var(--muted);
    display: flex;
    align-items: center;
  }
  .field {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 6px;
    color: var(--fg);
  }
  .field input[type="range"] {
    width: 96px;
    padding: 0;
    background: none;
    border: none;
    accent-color: var(--green);
  }
  .pctval {
    width: 34px;
    text-align: right;
    color: var(--muted);
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  /* Value + unit column: fixed widths so every number input's right edge and the
     unit column line up across rows (empty units still reserve their width). */
  .numwrap {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 5px;
  }
  .unit {
    width: 26px;
    text-align: left;
    color: var(--muted);
    font-size: calc(11px * var(--size-ui));
  }
  .field input {
    width: 70px;
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 2px 4px;
    text-align: right;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
  }
  .hint {
    color: var(--muted);
    opacity: 0.7;
    font-size: calc(10px * var(--size-ui));
    margin: -2px 0 8px;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: 4px;
  }
  .spacer {
    flex: 1 1 auto;
  }
  .btn.reset {
    color: var(--muted);
  }
  .btn {
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 3px 10px;
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
  }
  .btn.primary {
    background: var(--green);
    border-color: var(--green);
    color: #06210c;
    font-weight: var(--w-semibold);
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
