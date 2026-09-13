<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import type { Config } from "../types";
  import {
    applyTheme,
    previewTheme,
    applyUserThemes,
    THEMES,
    GROUP_ORDER,
    type Theme,
  } from "./theme";
  import type { ThemeData, ThemeColors } from "./themeCss";
  import Brand from "./Brand.svelte";

  let cfg = $state<Config | null>(null);
  let current = $state<string>("dark");
  // The user's own themes (full palettes), loaded from the config-folder sidecar.
  let userThemes = $state<ThemeData[]>([]);

  (async () => {
    cfg = await invoke<Config>("get_config");
    current = cfg.theme;
    applyTheme(current);
    try {
      userThemes = await invoke<ThemeData[]>("get_user_themes");
    } catch {
      // non-Tauri / failed call: leave the list empty, built-ins still show
    }
  })();

  // This window emits "theme", but Settings can change the theme too, so follow
  // the event as well or the highlighted swatch here goes stale.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => {
      current = e.payload;
      if (!editing) applyTheme(e.payload);
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

  // The built-in palettes live as `:root[data-theme=...]` blocks in the generated
  // CSS, so the only way to read a non-active theme's colors is to briefly stamp
  // it on the root, read the computed vars, then restore. Once when the window opens.
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

  // A user theme carries its full palette inline, so its card preview reads
  // straight off the object rather than the DOM.
  function userPalette(t: ThemeData): Palette {
    const c = t.colors!;
    return {
      bg: `rgb(${c.bg.join(", ")})`,
      fg: c.fg,
      muted: c.muted,
      track: c.track,
      edge: c.edge,
      g0: t.gradient?.[0] ?? "#000",
      g1: t.gradient?.[1] ?? "#000",
      g2: t.gradient?.[2] ?? "#000",
    };
  }

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

  // ---- Editor ----------------------------------------------------------------

  // A draft always has a full palette and gradient (the editor edits them all),
  // unlike the built-in registry which is id/label/group only.
  type Draft = ThemeData & { colors: ThemeColors; gradient: [string, string, string] };
  // The string-valued color tokens (everything but the bg RGB triple).
  type SKey = Exclude<keyof ThemeColors, "bg">;

  let editing = $state<Draft | null>(null);
  let editingIsNew = $state(false);
  const editPal = $derived(editing ? userPalette(editing) : null);

  const HEX_TOKENS: [SKey, string][] = [
    ["fg", "Text"],
    ["muted", "Muted"],
    ["track", "Track"],
    ["green", "Gauge low"],
    ["yellow", "Gauge mid"],
    ["red", "Gauge high"],
    ["live", "Live"],
  ];
  const RGBA_TOKENS: [SKey, string][] = [
    ["edge", "Edge"],
    ["edgeSoft", "Edge soft"],
    ["hover", "Hover"],
    ["panel", "Panel"],
  ];
  const GRAD_LABELS = ["Gradient 0", "Gradient 52%", "Gradient 100%"];
  const SCHEMES = ["dark", "light", "light dark"];

  function rgbTripleToHex(bg: number[]): string {
    const h = (n: number) =>
      Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
    return `#${h(bg[0] ?? 0)}${h(bg[1] ?? 0)}${h(bg[2] ?? 0)}`;
  }
  function hexToTriple(hex: string): number[] {
    const m = hex.trim().replace("#", "");
    const n = parseInt(m, 16);
    if (m.length !== 6 || Number.isNaN(n)) return [0, 0, 0];
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  }

  // Read every palette token of a built-in (or any stamped id) off the DOM, to
  // seed a duplicate. bg comes back as a comma triple; color-scheme tells us the
  // starting scheme so a duplicated light theme stays light.
  function captureFull(id: string): Draft {
    const root = document.documentElement;
    const prev = root.getAttribute("data-theme");
    root.setAttribute("data-theme", id);
    const cs = getComputedStyle(root);
    const v = (n: string) => cs.getPropertyValue(n).trim();
    const bg = v("--bg-rgb")
      .split(",")
      .map((s) => parseInt(s.trim(), 10) || 0);
    const cscheme = v("color-scheme");
    const scheme = cscheme.includes("light") && cscheme.includes("dark")
      ? "light dark"
      : cscheme.includes("light")
        ? "light"
        : "dark";
    const spendFg = v("--spend-fg");
    const draft: Draft = {
      id,
      label: "",
      group: "",
      scheme,
      note: null,
      colors: {
        bg: [bg[0] ?? 0, bg[1] ?? 0, bg[2] ?? 0],
        fg: v("--fg"),
        muted: v("--muted"),
        track: v("--track"),
        green: v("--green"),
        yellow: v("--yellow"),
        red: v("--red"),
        live: v("--live"),
        edge: v("--edge"),
        edgeSoft: v("--edge-soft"),
        hover: v("--hover"),
        panel: v("--panel"),
      },
      gradient: [v("--g0"), v("--g1"), v("--g2")],
      spendFg: spendFg || null,
    };
    if (prev) root.setAttribute("data-theme", prev);
    else root.removeAttribute("data-theme");
    return draft;
  }

  // A slug id unique against both the built-in registry and existing user themes.
  function uniqueSlug(label: string): string {
    const base =
      label
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, "-")
        .replace(/^-+|-+$/g, "") || "theme";
    const taken = new Set([...THEMES.map((t) => t.id), ...userThemes.map((t) => t.id)]);
    let id = base;
    let i = 2;
    while (taken.has(id)) id = `${base}-${i++}`;
    return id;
  }

  function duplicate(id: string, label: string, group: string) {
    const d = captureFull(id);
    d.label = `${label} copy`;
    d.group = group;
    d.id = uniqueSlug(d.label);
    editingIsNew = true;
    editing = d;
  }

  function editUser(t: ThemeData) {
    editingIsNew = false;
    editing = $state.snapshot(t) as Draft;
  }

  // Live preview: whenever the draft changes, inject it alongside the saved set
  // (registering its id so resolveTheme accepts it) and paint this window with it.
  // previewTheme does not persist, so a cold start still comes up on the real pick.
  $effect(() => {
    if (!editing) return;
    const snap = $state.snapshot(editing) as ThemeData;
    const rest = userThemes.filter((t) => t.id !== snap.id);
    applyUserThemes([...rest, snap]);
    previewTheme(snap.id);
  });

  function setScheme(s: string) {
    if (editing) editing.scheme = s;
  }
  function toggleSpendFg(on: boolean) {
    if (!editing) return;
    editing.spendFg = on ? (editing.spendFg ?? editing.colors.fg) : null;
  }

  async function save() {
    if (!editing || !editing.label.trim()) return;
    const draft = $state.snapshot(editing) as ThemeData;
    const next = [...userThemes.filter((t) => t.id !== draft.id), draft];
    userThemes = next;
    applyUserThemes(next);
    try {
      await invoke("set_user_themes", { themes: next });
    } catch {
      // ignore; the palette is still injected for this session
    }
    editing = null;
    await pick(draft.id); // saved theme becomes the active one
  }

  function cancel() {
    editing = null;
    applyUserThemes(userThemes); // drop the unsaved draft's palette + id
    previewTheme(current as Theme);
  }

  async function del(id: string) {
    const next = userThemes.filter((t) => t.id !== id);
    userThemes = next;
    applyUserThemes(next);
    try {
      await invoke("set_user_themes", { themes: next });
    } catch {
      // ignore
    }
    if (current === id) await pick("auto"); // was selected -> fall back
  }

  // Built-in groups first, then any extra groups the user's themes introduce.
  const groups = $derived([
    ...GROUP_ORDER,
    ...new Set(
      userThemes
        .map((t) => t.group)
        .filter((g) => g && !GROUP_ORDER.includes(g as (typeof GROUP_ORDER)[number])),
    ),
  ]);

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (editing) cancel();
    else getCurrentWindow().close();
  }
</script>

<svelte:window onkeydown={onKey} />

{#snippet cardBody(label: string, p: Palette, active: boolean)}
  <div class="name" style="color:{p.fg};">{label}</div>
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
  {#if active}<div class="check" style="color:{p.g0};"><Icon name="check" size={13} /></div>{/if}
{/snippet}

<div class="win">
  <div class="bar" data-tauri-drag-region>
    <Brand size={16} font={15} />
    <span class="title">Themes</span>
    <span class="spacer"></span>
    <button class="x" onclick={() => getCurrentWindow().close()} aria-label="Close">✕</button>
  </div>

  <div class="scroll">
    {#each groups as g}
      {@const builtins = THEMES.filter((t) => t.group === g)}
      {@const users = userThemes.filter((t) => t.group === g)}
      {#if builtins.length || users.length}
        <div class="grouplabel">{g}</div>
        <div class="grid">
          {#each builtins as t}
            {@const p = pal[t.id]}
            <div class="cardwrap">
              <button
                class="card"
                class:active={t.id === current}
                style="background:{p.bg}; border-color:{p.edge};"
                onclick={() => pick(t.id)}
                title={t.label}
              >
                {@render cardBody(t.label, p, t.id === current)}
              </button>
              <div class="tools">
                <button class="mini" title="Duplicate & edit" aria-label="Duplicate and edit"
                  onclick={() => duplicate(t.id, t.label, t.group)}>
                  <Icon name="copy" size={12} />
                </button>
              </div>
            </div>
          {/each}
          {#each users as t}
            {@const p = userPalette(t)}
            <div class="cardwrap">
              <button
                class="card"
                class:active={t.id === current}
                style="background:{p.bg}; border-color:{p.edge};"
                onclick={() => pick(t.id)}
                title={t.label}
              >
                {@render cardBody(t.label, p, t.id === current)}
              </button>
              <div class="tools">
                <button class="mini" title="Edit" aria-label="Edit" onclick={() => editUser(t)}>
                  <Icon name="edit" size={12} />
                </button>
                <button class="mini" title="Delete" aria-label="Delete" onclick={() => del(t.id)}>
                  <Icon name="trash" size={12} />
                </button>
              </div>
            </div>
          {/each}
        </div>
      {/if}
    {/each}
  </div>
</div>

{#if editing}
  <div class="editor">
    <div class="ebar" data-tauri-drag-region>
      <span class="title">{editingIsNew ? "New theme" : "Edit theme"}</span>
      <span class="spacer"></span>
      <button class="ghost" onclick={cancel}>Cancel</button>
      <button class="primary" disabled={!editing.label.trim()} onclick={save}>Save</button>
    </div>

    <div class="escroll">
      {#if editPal}
        <div class="previewcard" style="background:{editPal.bg}; border-color:{editPal.edge};">
          {@render cardBody(editing.label || "Untitled", editPal, true)}
        </div>
      {/if}

      <label class="field">
        <span>Name</span>
        <input type="text" bind:value={editing.label} placeholder="My theme" />
      </label>
      <label class="field">
        <span>Group</span>
        <input type="text" bind:value={editing.group} list="groups" placeholder="Custom" />
        <datalist id="groups">
          {#each GROUP_ORDER as g}<option value={g}></option>{/each}
        </datalist>
      </label>

      <div class="field">
        <span>Scheme</span>
        <div class="seg">
          {#each SCHEMES as s}
            <button class:on={editing.scheme === s} onclick={() => setScheme(s)}>{s}</button>
          {/each}
        </div>
      </div>

      <div class="grouplabel">Background</div>
      <label class="row">
        <span>Background</span>
        <input
          type="color"
          value={rgbTripleToHex(editing.colors.bg)}
          oninput={(e) => (editing!.colors.bg = hexToTriple(e.currentTarget.value))}
        />
        <span class="hex ro">{rgbTripleToHex(editing.colors.bg)}</span>
      </label>

      <div class="grouplabel">Colors</div>
      {#each HEX_TOKENS as [key, lbl]}
        <label class="row">
          <span>{lbl}</span>
          <input type="color" bind:value={editing.colors[key]} />
          <input type="text" class="hex" bind:value={editing.colors[key]} />
        </label>
      {/each}

      <div class="grouplabel">Gauge gradient</div>
      {#each [0, 1, 2] as i}
        <label class="row">
          <span>{GRAD_LABELS[i]}</span>
          <input type="color" bind:value={editing.gradient[i]} />
          <input type="text" class="hex" bind:value={editing.gradient[i]} />
        </label>
      {/each}

      <div class="grouplabel">Surfaces (rgba allowed)</div>
      {#each RGBA_TOKENS as [key, lbl]}
        <label class="row">
          <span>{lbl}</span>
          <span class="swatch" style="background:{editing.colors[key]};"></span>
          <input type="text" class="hex wide" bind:value={editing.colors[key]} />
        </label>
      {/each}

      <div class="grouplabel">Spend readout</div>
      <label class="row check">
        <input
          type="checkbox"
          checked={editing.spendFg != null}
          onchange={(e) => toggleSpendFg(e.currentTarget.checked)}
        />
        <span>Custom spend text color (recommended on light themes)</span>
      </label>
      {#if editing.spendFg != null}
        <label class="row">
          <span>Spend text</span>
          <input type="color" bind:value={editing.spendFg} />
          <input type="text" class="hex" bind:value={editing.spendFg} />
        </label>
      {/if}
    </div>
  </div>
{/if}

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
  .bar,
  .ebar {
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
  .cardwrap {
    position: relative;
  }
  .card {
    position: relative;
    width: 100%;
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
  .tools {
    position: absolute;
    top: 6px;
    right: 8px;
    display: flex;
    gap: 4px;
    opacity: 0;
    transition: opacity 0.08s ease;
  }
  .cardwrap:hover .tools {
    opacity: 1;
  }
  .mini {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 20px;
    height: 20px;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
    padding: 0;
  }
  .mini:hover {
    background: var(--hover);
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

  /* ---- Editor overlay ---- */
  .editor {
    position: fixed;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: var(--bg);
    color: var(--fg);
  }
  .escroll {
    flex: 1;
    overflow-y: auto;
    padding: 10px 14px 18px;
  }
  .ghost,
  .primary {
    border-radius: 6px;
    padding: 4px 12px;
    cursor: pointer;
    font-size: calc(12px * var(--size-ui));
    font-weight: var(--w-semibold);
  }
  .ghost {
    background: none;
    border: 1px solid var(--edge);
    color: var(--muted);
  }
  .ghost:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .primary {
    background: var(--fg);
    border: 1px solid var(--fg);
    color: var(--bg);
  }
  .primary:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .previewcard {
    border: 1px solid;
    border-radius: 8px;
    padding: 8px 9px 9px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    max-width: 220px;
    margin-bottom: 10px;
    position: relative;
  }
  .field {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 6px 0;
    font-size: calc(12px * var(--size-ui));
  }
  .field > span {
    width: 64px;
    color: var(--muted);
  }
  .field input[type="text"] {
    flex: 1;
    background: var(--panel);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--fg);
    padding: 4px 7px;
    font: inherit;
  }
  .seg {
    display: flex;
    gap: 4px;
  }
  .seg button {
    background: var(--panel);
    border: 1px solid var(--edge);
    color: var(--muted);
    border-radius: 5px;
    padding: 3px 9px;
    cursor: pointer;
    font-size: calc(11px * var(--size-ui));
  }
  .seg button.on {
    background: var(--fg);
    border-color: var(--fg);
    color: var(--bg);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 4px 0;
    font-size: calc(12px * var(--size-ui));
  }
  .row > span:first-child {
    width: 90px;
    color: var(--muted);
  }
  .row.check {
    align-items: flex-start;
  }
  .row.check > span {
    width: auto;
    color: var(--fg);
  }
  .row input[type="color"] {
    width: 30px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--edge);
    border-radius: 5px;
    background: none;
    cursor: pointer;
  }
  .hex {
    background: var(--panel);
    border: 1px solid var(--edge);
    border-radius: 5px;
    color: var(--fg);
    padding: 3px 6px;
    font: inherit;
    width: 110px;
  }
  .hex.wide {
    width: 190px;
  }
  .hex.ro {
    color: var(--muted);
    border-style: dashed;
  }
  .swatch {
    width: 30px;
    height: 24px;
    border: 1px solid var(--edge);
    border-radius: 5px;
  }
</style>
