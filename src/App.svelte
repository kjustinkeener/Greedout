<script lang="ts">
  import { onMount, tick } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { invoke } from "@tauri-apps/api/core";
  import type { Session, Sample } from "./types";
  import Row from "./lib/Row.svelte";
  import FocusPanel from "./lib/FocusPanel.svelte";
  import StatusBar from "./lib/StatusBar.svelte";
  import Brand from "./lib/Brand.svelte";
  import Icon from "./lib/Icon.svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
  import { LogicalSize } from "@tauri-apps/api/dpi";
  import { applyOpacity, applyTheme, type Theme } from "./lib/theme";
  import { t, watchLocale } from "./lib/i18n.svelte";
  import type { Config } from "./types";

  let menuOpen = $state(false);

  const appWindow = getCurrentWindow();

  // Open the Settings panel in its own window (it can be taller than the gauge
  // window). Reuse the window if it's already open.
  async function openSettings() {
    menuOpen = false;
    const existing = await WebviewWindow.getByLabel("settings");
    if (existing) {
      // getByLabel can hand back a live window OR a stale handle for one that's
      // mid-teardown. show()+setFocus() reveals a real one; if it's a ghost the
      // calls reject, so force it closed and fall through to recreate.
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
    await createSettingsWindow(true);
  }

  // Create the settings window. A close() still in flight can make this fail
  // asynchronously with "a webview with label `settings` already exists". The
  // error arrives via the tauri://error event (never thrown), so the window
  // silently never appears. Catch it, tear down the straggler, and retry once.
  async function createSettingsWindow(retry: boolean) {
    // Place it over the main window (a touch inset), not at the OS default spot.
    let pos: { x: number; y: number } | undefined;
    try {
      const sf = await appWindow.scaleFactor();
      const p = (await appWindow.outerPosition()).toLogical(sf);
      pos = { x: Math.round(p.x + 16), y: Math.round(p.y + 16) };
    } catch {
      // Position unavailable; let the OS place it.
    }
    // Honor the app's "Always on top" setting for this window too.
    const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
    const w = new WebviewWindow("settings", {
      url: "settings.html",
      title: "Greedout Settings",
      width: 300,
      height: 520,
      minWidth: 240,
      minHeight: 200,
      resizable: true,
      alwaysOnTop: aot,
      focus: true,
      ...(pos ? { x: pos.x, y: pos.y } : {}),
    });
    if (retry) {
      void w.once("tauri://error", async () => {
        const stale = await WebviewWindow.getByLabel("settings");
        if (stale) {
          try {
            await stale.close();
          } catch {
            // already gone
          }
        }
        // Let the teardown settle, then create without a further retry.
        setTimeout(() => void createSettingsWindow(false), 150);
      });
    }
  }

  // Open the About window (small, its own window like Settings). Reuse if open.
  async function openAbout() {
    menuOpen = false;
    const existing = await WebviewWindow.getByLabel("about");
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
      const sf = await appWindow.scaleFactor();
      const p = (await appWindow.outerPosition()).toLogical(sf);
      pos = { x: Math.round(p.x + 16), y: Math.round(p.y + 16) };
    } catch {
      // Position unavailable; let the OS place it.
    }
    const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
    const w = new WebviewWindow("about", {
      url: "about.html",
      title: "About Greedout",
      width: 420,
      height: 400,
      resizable: false,
      alwaysOnTop: aot,
      focus: true,
      ...(pos ? { x: pos.x, y: pos.y } : {}),
    });
    void w.once("tauri://error", async () => {
      const stale = await WebviewWindow.getByLabel("about");
      if (stale) {
        try {
          await stale.close();
        } catch {
          // already gone
        }
      }
    });
  }

  // Open the baseline-context analysis in its own window (2x the gauge window,
  // separate from it) for the session open in the Claude app, or the most-recent
  // one if none is focused. Reuse the window if it's already open.
  async function openBaseline() {
    menuOpen = false;
    if (!explorerEnabled) return;
    const s = sessions.find((x) => x.focused) ?? sessions[0];
    if (!s) return;
    const existing = await WebviewWindow.getByLabel("baseline");
    if (existing) {
      // Live window → reveal it, and always snap it back to the default size
      // (this window intentionally never persists its size). A ghost
      // mid-teardown rejects these, so force it closed and recreate.
      try {
        await existing.unminimize();
        await existing.setSize(new LogicalSize(720, 560));
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
    await createBaselineWindow(s, true);
  }

  // Create the baseline window. A close() still in flight makes the constructor
  // fail asynchronously via tauri://error ("a webview with label `baseline`
  // already exists"), never thrown, so the window silently never appears.
  // Tear down the straggler and retry once.
  async function createBaselineWindow(s: Session, retry: boolean) {
    let pos: { x: number; y: number } | undefined;
    try {
      const sf = await appWindow.scaleFactor();
      const p = (await appWindow.outerPosition()).toLogical(sf);
      pos = { x: Math.round(p.x + 24), y: Math.round(p.y + 24) };
    } catch {
      // Position unavailable; let the OS place it.
    }
    const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
    const q = new URLSearchParams({ id: s.id, title: `${s.title} · ${s.project}` });
    const w = new WebviewWindow("baseline", {
      url: `baseline.html?${q.toString()}`,
      title: "Context Explorer",
      width: 720,
      height: 560,
      resizable: true,
      alwaysOnTop: aot,
      focus: true,
      ...(pos ? { x: pos.x, y: pos.y } : {}),
    });
    if (retry) {
      void w.once("tauri://error", async () => {
        const stale = await WebviewWindow.getByLabel("baseline");
        if (stale) {
          try {
            await stale.close();
          } catch {
            // already gone
          }
        }
        setTimeout(() => void createBaselineWindow(s, false), 150);
      });
    }
  }

  // Open the Explorer targeted at a specific session or project (clicking a
  // title/project in the list). Reuses the window (retargeting via an event) or
  // creates it with the target baked into the URL.
  async function openExplorer(opts: {
    id?: string;
    title: string;
    project?: string;
    view: "session" | "project";
  }) {
    if (!explorerEnabled) return;
    const existing = await WebviewWindow.getByLabel("baseline");
    if (existing) {
      try {
        await existing.unminimize();
        await existing.show();
        await existing.setFocus();
        await existing.emit("baseline-goto", opts);
        return;
      } catch {
        try {
          await existing.close();
        } catch {
          // already gone
        }
      }
    }
    await createExplorerWindow(opts, true);
  }

  async function createExplorerWindow(
    opts: { id?: string; title: string; project?: string; view: "session" | "project" },
    retry: boolean,
  ) {
    let pos: { x: number; y: number } | undefined;
    try {
      const sf = await appWindow.scaleFactor();
      const p = (await appWindow.outerPosition()).toLogical(sf);
      pos = { x: Math.round(p.x + 24), y: Math.round(p.y + 24) };
    } catch {
      // Position unavailable; let the OS place it.
    }
    const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
    const q = new URLSearchParams({
      id: opts.id ?? "",
      title: opts.title,
      view: opts.view,
      ...(opts.project ? { project: opts.project } : {}),
    });
    const w = new WebviewWindow("baseline", {
      url: `baseline.html?${q.toString()}`,
      title: "Context Explorer",
      width: 720,
      height: 560,
      resizable: true,
      alwaysOnTop: aot,
      focus: true,
      ...(pos ? { x: pos.x, y: pos.y } : {}),
    });
    if (retry) {
      void w.once("tauri://error", async () => {
        const stale = await WebviewWindow.getByLabel("baseline");
        if (stale) {
          try {
            await stale.close();
          } catch {
            // already gone
          }
        }
        setTimeout(() => void createExplorerWindow(opts, false), 150);
      });
    }
  }

  const openSession = (s: Session) =>
    openExplorer({ id: s.id, title: `${s.title} · ${s.project}`, project: s.project, view: "session" });
  const openProject = (s: Session) =>
    openExplorer({ id: s.id, title: s.project, project: s.project, view: "project" });

  const MAX_SAMPLES = 300;

  let sessions = $state<Session[]>([]);
  let error = $state<string | null>(null);
  // Auto-fit: render only as many rows as fit the window so the list fills the
  // space and never shows a scrollbar. `mainEl` is the scroll container (kept at
  // overflow:hidden); `visible` is trimmed to whole rows against its height.
  let mainEl = $state<HTMLElement | undefined>();
  let containerH = $state(0);
  let visible = $state(99);
  let dimHours = $state(24);
  let showStatusbar = $state(false);
  // Context Explorer offered at all (menu entry + clickable titles); on by default.
  let explorerEnabled = $state(true);

  // The update offer. Null until a check comes back with something newer, so
  // the banner does not exist on the overwhelmingly common launch where the app
  // is already current.
  type UpdateInfo = { version: string; notes: string; url: string; signature: string };
  let update = $state<UpdateInfo | null>(null);
  let updateCurrent = $state("");
  let updateStatus = $state("");
  let updating = $state(false);
  let updateDone = $state(false);

  async function installUpdate() {
    if (!update || updating || updateDone) return; // the button stays mounted
    updating = true;
    updateStatus = t("update.downloading", { version: update.version });
    try {
      // On success this never returns: the replacement is already starting and
      // this process exits underneath us.
      await invoke("update_apply", { info: update });
      updateStatus = t("update.installed");
      updateDone = true;
    } catch (e) {
      updateStatus = t("update.failed", { error: String(e) });
      updating = false;
    }
  }

  // Grow to show everything, then shave one row at a time until nothing overflows.
  // Cheap (list is small) and free of oscillation. Runs whenever the snapshot or
  // the container height changes.
  let fitting = false;
  async function fit() {
    if (!mainEl || fitting) return;
    fitting = true;
    try {
      visible = sessions.length;
      await tick();
      let guard = 0;
      while (mainEl && mainEl.scrollHeight > mainEl.clientHeight + 1 && visible > 1 && guard++ < 100) {
        visible--;
        await tick();
      }
      // Let one overflowing row peek in (clipped by overflow:hidden) so a partially
      // cut-off session still hints there are more, rather than vanishing entirely.
      if (visible < sessions.length) {
        visible++;
        await tick();
      }
    } finally {
      fitting = false;
    }
  }

  $effect(() => {
    // Re-fit on new snapshots (row count/heights change) and on resize.
    void sessions;
    void containerH;
    fit();
  });

  $effect(() => {
    if (!mainEl) return;
    const ro = new ResizeObserver(() => {
      if (mainEl) containerH = mainEl.clientHeight;
    });
    ro.observe(mainEl);
    return () => ro.disconnect();
  });
  // Per-session time-series. Seeded once from the transcript (history from before
  // the app opened), then extended live as snapshots stream in.
  let history = $state<Map<string, Sample[]>>(new Map());
  const seeded = new Set<string>();

  // Backfill one session's earlier curve from its transcript, once.
  async function seedHistory(id: string) {
    if (seeded.has(id)) return;
    seeded.add(id);
    try {
      const base = await invoke<Sample[]>("get_history", { id });
      if (!base.length) return;
      const cur = history.get(id) ?? [];
      const firstLive = cur.length ? cur[0].t : Infinity;
      const merged = [...base.filter((s) => s.t < firstLive), ...cur];
      if (merged.length > MAX_SAMPLES) merged.splice(0, merged.length - MAX_SAMPLES);
      const next = new Map(history);
      next.set(id, merged);
      history = next;
    } catch {
      // Transcript unreadable or gone; live samples still accumulate.
    }
  }

  function record(snapshot: Session[]) {
    sessions = snapshot;
    const t = Date.now();
    const next = new Map<string, Sample[]>();
    for (const s of snapshot) {
      const prev = history.get(s.id) ?? [];
      const last = prev[prev.length - 1];
      // Only append when context or spend actually moved; otherwise idle polls
      // would keep stretching the graph's time span sideways.
      const changed = !last || last.ctx !== s.ctx || last.cost !== s.costUsd;
      const series = changed ? [...prev, { t, ctx: s.ctx, cost: s.costUsd }] : prev;
      if (series.length > MAX_SAMPLES) series.splice(0, series.length - MAX_SAMPLES);
      next.set(s.id, series); // drop series for sessions no longer present
      if (!seeded.has(s.id)) seedHistory(s.id);
    }
    history = next;
  }

  // Ctrl + mousewheel zooms the whole UI (CSS zoom) and grows/shrinks the window
  // by the same factor, so the layout stays pixel-tight at any scale.
  let uiScale = 1;
  let saveScaleTimer: ReturnType<typeof setTimeout> | undefined;
  async function zoomBy(factor: number) {
    const next = Math.min(3, Math.max(0.5, uiScale * factor));
    const ratio = next / uiScale;
    if (ratio === 1) return;
    uiScale = next;
    document.documentElement.style.zoom = String(uiScale);
    try {
      const sf = await appWindow.scaleFactor();
      const cur = (await appWindow.innerSize()).toLogical(sf);
      await appWindow.setSize(new LogicalSize(cur.width * ratio, cur.height * ratio));
    } catch {
      // Window resize unavailable; the UI still scales.
    }
    // Debounce-persist the zoom so it's restored next launch (window size is
    // persisted separately, in winstate.rs).
    clearTimeout(saveScaleTimer);
    saveScaleTimer = setTimeout(() => invoke("set_ui_scale", { scale: uiScale }), 400);
  }
  function onWheel(e: WheelEvent) {
    if (!e.ctrlKey) return;
    e.preventDefault();
    zoomBy(e.deltaY < 0 ? 1.1 : 1 / 1.1);
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let unlistenOpacity: (() => void) | undefined;
    let unlistenDim: (() => void) | undefined;
    let unlistenTheme: (() => void) | undefined;
    let unlistenStatusbar: (() => void) | undefined;
    let unlistenExplorer: (() => void) | undefined;
    let unlistenLocale: (() => void) | undefined;
    window.addEventListener("wheel", onWheel, { passive: false });
    (async () => {
      try {
        // Apply the saved window opacity and zoom before first paint. Window
        // size/position are restored in setup by winstate.rs.
        const cfg0 = await invoke<Config>("get_config");
        applyOpacity(cfg0.opacity);
        applyTheme(cfg0.theme ?? "dark");
        uiScale = cfg0.ui_scale ?? 1;
        dimHours = cfg0.dim_hours ?? 24;
        showStatusbar = cfg0.show_statusbar ?? false;
        explorerEnabled = cfg0.explorer_enabled ?? true;
        // Fire and forget, and silent on failure: an app that cannot reach
        // GitHub is still a working app, and saying so on every launch behind a
        // captive portal would be noise.
        if (cfg0.check_updates ?? true) {
          void invoke<{ current: string; available: UpdateInfo | null }>("update_check")
            .then((r) => {
              updateCurrent = r.current;
              if (r.available) update = r.available;
            })
            .catch(() => {});
        }
        document.documentElement.style.zoom = String(uiScale);
        // Prime with an immediate snapshot, then stream updates.
        record(await invoke<Session[]>("get_sessions"));
        unlisten = await listen<Session[]>("sessions", (e) => record(e.payload));
        // Settings (separate window) pushes a live dim-hours preview on change.
        unlistenDim = await listen<number>("dim-hours", (e) => (dimHours = e.payload));
        // Settings (separate window) toggles the status bar live.
        unlistenStatusbar = await listen<boolean>("show-statusbar", (e) => (showStatusbar = e.payload));
        // Settings toggles the Explorer live; switching it off also closes the
        // window if it's open, so no orphaned Explorer stays behind.
        unlistenExplorer = await listen<boolean>("explorer-enabled", async (e) => {
          explorerEnabled = e.payload;
          if (!explorerEnabled) {
            const w = await WebviewWindow.getByLabel("baseline");
            try {
              await w?.close();
            } catch {
              // already gone
            }
          }
        });
        // The Settings window (separate) previews/saves opacity via this event.
        unlistenOpacity = await listen<number>("opacity-preview", (e) => applyOpacity(e.payload));
        // Settings (separate window) pushes a live theme preview on change.
        unlistenTheme = await listen<Theme>("theme", (e) => applyTheme(e.payload));
        unlistenLocale = await watchLocale();
      } catch (e) {
        error = String(e);
      }
    })();
    return () => {
      unlisten?.();
      unlistenOpacity?.();
      unlistenDim?.();
      unlistenTheme?.();
      unlistenStatusbar?.();
      unlistenExplorer?.();
      unlistenLocale?.();
      window.removeEventListener("wheel", onWheel);
    };
  });

  async function rename(s: Session) {
    const next = prompt(t("main.renamePrompt", { title: s.title }), s.title);
    if (next == null) return;
    await invoke("set_label", { id: s.id, label: next.trim() || null });
    // Optimistic; the next poll will confirm.
    sessions = sessions.map((x) => (x.id === s.id ? { ...x, title: next.trim() || x.title } : x));
  }
</script>

<header class="titlebar" data-tauri-drag-region>
  <Brand size={16} font={15} />
  <div class="menu">
    <button class="dots" title={t("menu.menu")} aria-label={t("menu.menu")} onclick={() => (menuOpen = !menuOpen)}>
      <Icon name="more" size={16} width={3} />
    </button>
    {#if menuOpen}
      <button class="scrim" aria-label={t("menu.closeMenu")} onclick={() => (menuOpen = false)}></button>
      <div class="dropdown">
        {#if explorerEnabled}
          <button class="item" onclick={openBaseline}>
          <span class="mi"><Icon name="layout" size={14} /></span>
            {t("menu.explorer")}
          </button>
        {/if}
        <button class="item" onclick={openSettings}>
          <span class="mi"><Icon name="sliders" size={14} /></span>
          {t("menu.settings")}
        </button>
        <button class="item" onclick={openAbout}>
          <span class="mi"><Icon name="info" size={14} /></span>
          {t("menu.about")}
        </button>
      </div>
    {/if}
  </div>
  <div class="controls">
    <button class="ctl" title={t("win.minimize")} aria-label={t("win.minimize")} onclick={() => appWindow.minimize()}>
      <Icon name="minus" size={11} width={1.5} />
    </button>
    <button class="ctl close" title={t("win.close")} aria-label={t("win.close")} onclick={() => appWindow.close()}>
      <Icon name="x" size={11} width={1.5} />
    </button>
  </div>
</header>

<main bind:this={mainEl}>
  {#if update}
    <div class="update">
      <span class="uicon"><Icon name={updateDone ? "check" : "arrow-up"} size={12} /></span>
      <span class="utext">
        {updateStatus || t("update.available", { version: update.version, current: updateCurrent })}
      </span>
      {#if !updating && !updateDone}
        <button class="ubtn" onclick={installUpdate}>{t("update.install")}</button>
        <button
          class="udismiss"
          onclick={() => (update = null)}
          title={t("update.dismiss")}
          aria-label={t("update.dismiss")}
        >
          <Icon name="x" size={11} />
        </button>
      {/if}
    </div>
  {/if}
  {#if error}
    <div class="msg err">{error}</div>
  {:else if sessions.length === 0}
    <div class="msg">{t("main.noSessions")}</div>
  {:else}
    {#each sessions.slice(0, visible) as s, i (s.id)}
      {#if s.focused}
        <FocusPanel
          session={s}
          samples={history.get(s.id) ?? []}
          onRename={() => rename(s)}
          onOpenSession={() => openSession(s)}
          onOpenProject={() => openProject(s)}
          explorer={explorerEnabled}
        />
      {:else}
        <Row
          session={s}
          onRename={() => rename(s)}
          onOpenSession={() => openSession(s)}
          onOpenProject={() => openProject(s)}
          explorer={explorerEnabled}
          top={i === 0}
          {dimHours}
        />
      {/if}
    {/each}
  {/if}
</main>

{#if showStatusbar}
  <StatusBar />
{/if}

<style>
  /* The one thing in this window that appears without being asked for, so it
     sits above the list rather than replacing anything, and it can be waved
     away. Colored with the gauge's own cyan so it reads as part of the app. */
  .update {
    display: flex;
    align-items: center;
    gap: 7px;
    margin: 0 0 6px;
    padding: 6px 8px;
    border: 1px solid color-mix(in srgb, var(--g0) 55%, transparent);
    border-radius: 7px;
    background: color-mix(in srgb, var(--g0) 10%, transparent);
    font-size: 11.5px;
  }
  .uicon {
    display: grid;
    place-items: center;
    flex: 0 0 auto;
    width: 17px;
    height: 17px;
    border-radius: 50%;
    background: var(--g0);
    color: rgb(var(--bg-rgb));
  }
  .utext {
    flex: 1 1 auto;
    min-width: 0;
    line-height: 1.35;
  }
  .ubtn {
    flex: 0 0 auto;
    padding: 3px 8px;
    font: inherit;
    font-size: 11px;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-semibold);
    color: rgb(var(--bg-rgb));
    background: var(--g0);
    border: none;
    border-radius: 5px;
    cursor: pointer;
  }
  .ubtn:hover {
    filter: brightness(1.08);
  }
  .udismiss {
    flex: 0 0 auto;
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    color: var(--muted);
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
  }
  .udismiss:hover {
    color: var(--fg);
    background: var(--panel);
  }
  header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 4px 0 10px;
    height: 28px;
    background: var(--panel);
    border-bottom: 1px solid var(--edge);
    cursor: grab;
    flex: 0 0 auto;
    user-select: none;
    -webkit-user-select: none;
  }
  .menu {
    margin-left: auto;
    position: relative;
  }
  .dots {
    background: none;
    border: none;
    color: var(--fg);
    cursor: pointer;
    display: grid;
    place-items: center;
    padding: 0 4px;
    border-radius: 4px;
  }
  /* Stay legible when the opacity slider makes the window see-through and the desktop
     shows behind the titlebar. A stroke is not text, so text-shadow does nothing to it
     and the shadow has to be a filter. */
  .dots :global(svg),
  .ctl :global(svg) {
    filter: drop-shadow(0 1px 2px rgba(0, 0, 0, 0.7));
  }
  .dots:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 9;
    background: none;
    border: none;
    cursor: default;
  }
  .dropdown {
    position: absolute;
    right: 0;
    top: 22px;
    z-index: 10;
    /* max-content so a label never wraps to min-content inside this ~30px
       button wrapper, but as WIDTH not min-width: a min-width beats max-width,
       and a long translation would then run off a 340px window with no clamp. */
    width: max-content;
    max-width: 90vw;
    /* Solid, not var(--bg): this sits inside #app, which is already painted with
       var(--bg), so the alpha would compose with itself and the menu would end up
       at an opacity nobody chose. A menu wants to be readable, so say opaque. */
    background: rgb(var(--bg-rgb));
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.5);
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    color: var(--fg);
    padding: 5px 8px;
    border-radius: 4px;
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    white-space: nowrap;
  }
  /* An svg leaves the line box, so the wrapper has to center it; a glyph sat on the
     text baseline on its own. The size lives on the component, not here. */
  .mi {
    display: flex;
    align-items: center;
    flex: 0 0 auto;
    color: var(--muted);
  }
  .item:hover .mi {
    color: var(--fg);
  }
  .item:hover {
    background: var(--hover);
  }
  .controls {
    display: flex;
    align-items: stretch;
    height: 100%;
  }
  .ctl {
    width: 30px;
    display: grid;
    place-items: center;
    border: none;
    background: transparent;
    color: var(--fg);
    cursor: pointer;
    transition: background 0.12s;
  }
  .ctl:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .ctl.close:hover {
    color: #fff;
    background: var(--red, #e81123);
  }
  main {
    flex: 1 1 auto;
    /* Never scroll: the list is auto-fitted to whole rows (see fit() in script). */
    overflow: hidden;
    padding: 2px 0;
    /* Clip the last row into the app's rounded bottom corners. */
    border-bottom-left-radius: 8px;
    border-bottom-right-radius: 8px;
  }
  .msg {
    padding: 12px;
    color: var(--muted);
    text-align: center;
  }
  .err {
    color: var(--red);
    white-space: pre-wrap;
    text-align: left;
    font-family: var(--font-mono);
    font-size: calc(1em * var(--size-mono) / var(--size-ui));
    font-size-adjust: var(--font-mono-adj);
  }
</style>
