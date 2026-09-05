<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import type { Config } from "../types";
  import { applyTheme, THEMES, GROUP_ORDER, type Theme } from "./theme";
  import Brand from "./Brand.svelte";

  let cfg = $state<Config | null>(null);
  let current = $state<string>("dark");

  (async () => {
    cfg = await invoke<Config>("get_config");
    current = cfg.theme;
    applyTheme(current);
  })();

  // This window emits "theme", but Settings can change the theme too, so follow
  // the event as well or the highlighted swatch here goes stale.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => {
      current = e.payload;
      applyTheme(e.payload);
    }).then((f) => (un = f));
    return () => un?.();
  });

  type Palette = {
    bg: string;
    fg: string;
    muted: string;
    track: string;
    edge: string;
    g0: string;
    g1: string;
    g2: string;
  };

  // The theme palettes live as `:root[data-theme=...]` blocks in app.css, so the
  // only way to read a non-active theme's colors is to briefly stamp it on the
  // root, read the computed vars, then restore. Do it once when the window opens.
  function capture(): Record<string, Palette> {
    const root = document.documentElement;
    const prev = root.getAttribute("data-theme");
    const v = (n: string) => getComputedStyle(root).getPropertyValue(n).trim();
    const out: Record<string, Palette> = {};
    for (const t of THEMES) {
      root.setAttribute("data-theme", t.id);
      out[t.id] = {
        bg: `rgb(${v("--bg-rgb")})`,
        fg: v("--fg"),
        muted: v("--muted"),
        track: v("--track"),
        edge: v("--edge"),
        g0: v("--g0"),
        g1: v("--g1"),
        g2: v("--g2"),
      };
    }
    if (prev) root.setAttribute("data-theme", prev);
    else root.removeAttribute("data-theme");
    return out;
  }

  const pal = capture();

  // Gauge gradient: g0 at 0, g1 at 52%, g2 at 100%, mirroring gaugeColor()'s stops.
  function gauge(p: Palette): string {
    return `linear-gradient(90deg, ${p.g0} 0%, ${p.g1} 52%, ${p.g2} 100%)`;
  }

  // Apply live to this window + the main window, and persist to config so the
  // choice sticks. Picking keeps the window open so themes can be compared.
  async function pick(id: string) {
    if (!cfg) return;
    current = id;
    cfg.theme = id;
    applyTheme(id as Theme);
    void emit("theme", id); // main window applies live; Settings updates its label
    await invoke("set_config", { config: { ...cfg, theme: id } });
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") getCurrentWindow().close();
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="win">
  <div class="bar" data-tauri-drag-region>
    <Brand size={16} font={15} />
    <span class="title">Themes</span>
    <span class="spacer"></span>
    <button class="x" onclick={() => getCurrentWindow().close()} aria-label="Close">✕</button>
  </div>

  <div class="scroll">
    {#each GROUP_ORDER as g}
      <div class="grouplabel">{g}</div>
      <div class="grid">
        {#each THEMES.filter((t) => t.group === g) as t}
          {@const p = pal[t.id]}
          <button
            class="card"
            class:active={t.id === current}
            style="background:{p.bg}; border-color:{p.edge};"
            onclick={() => pick(t.id)}
            title={t.label}
          >
            <div class="name" style="color:{p.fg};">{t.label}</div>
            <div class="sub" style="color:{p.muted};">context / spend</div>
            <div class="gaugebar" style="background:{gauge(p)};"></div>
            <div class="track" style="background:{p.track};">
              <div class="fill" style="background:{gauge(p)}; background-size:143% 100%;"></div>
            </div>
            <div class="dots">
              <span style="background:{p.g0};"></span>
              <span style="background:{p.g1};"></span>
              <span style="background:{p.g2};"></span>
              <span class="num" style="color:{p.g2};">$0.00</span>
            </div>
            {#if t.id === current}<div class="check" style="color:{p.g0};"><Icon name="check" size={13} /></div>{/if}
          </button>
        {/each}
      </div>
    {/each}
  </div>
</div>

<style>
  :global(html),
  :global(body) {
    background: var(--bg);
  }
  .win {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--edge-soft);
  }
  .title {
    font-weight: var(--w-bold);
    font-size: calc(14px * var(--size-ui));
    color: var(--muted);
  }
  .spacer {
    flex: 1;
  }
  .x {
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    font-size: calc(13px * var(--size-ui));
    padding: 2px 6px;
    border-radius: 4px;
  }
  .x:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .scroll {
    flex: 1;
    overflow-y: auto;
    padding: 8px 12px 14px;
  }
  .grouplabel {
    font-size: calc(10px * var(--size-ui));
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    font-weight: var(--w-semibold);
    margin: 12px 2px 6px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(150px, 1fr));
    gap: 8px;
  }
  .card {
    position: relative;
    text-align: left;
    border: 1px solid;
    border-radius: 8px;
    padding: 8px 9px 9px;
    cursor: pointer;
    display: flex;
    flex-direction: column;
    gap: 5px;
    transition:
      transform 0.08s ease,
      box-shadow 0.08s ease;
  }
  .card:hover {
    transform: translateY(-1px);
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
  }
  .card.active {
    outline: 2px solid var(--fg);
    outline-offset: 1px;
  }
  .name {
    font-weight: var(--w-bold);
    font-size: calc(12px * var(--size-ui));
  }
  .sub {
    font-size: calc(10px * var(--size-ui));
  }
  .gaugebar {
    height: 8px;
    border-radius: 4px;
  }
  .track {
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    width: 70%;
    border-radius: 3px;
  }
  .dots {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .dots span {
    width: 9px;
    height: 9px;
    border-radius: 50%;
  }
  .dots .num {
    width: auto;
    height: auto;
    border-radius: 0;
    margin-left: auto;
    font-size: calc(11px * var(--size-ui));
    font-weight: var(--w-bold);
  }
  .check {
    position: absolute;
    top: 6px;
    right: 8px;
    display: flex;
  }
</style>
