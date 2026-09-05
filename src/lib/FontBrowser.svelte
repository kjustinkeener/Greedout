<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import Icon from "./Icon.svelte";
  import { emit, listen } from "@tauri-apps/api/event";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import type { Config } from "../types";
  import { applyTheme, gaugeColor, themeTick, type Theme } from "./theme";
  import {
    applyFonts,
    BOLDNESS_LABELS,
    DEFAULT_FONTS,
    fontById,
    fontsFor,
    FONT_SETS,
    GROUP_ORDER,
    freeLabel,
    ladder,
    matchSet,
    toStored,
    userSets,
    SIZE_KEY,
    SIZE_MAX,
    SIZE_MIN,
    sizeLabel,
    SLOTS,
    systemFont,
    type FontDef,
    type FontPrefs,
    type Slot,
    type StoredSet,
  } from "./fonts";
  import Brand from "./Brand.svelte";

  /** Where the sample row sits on the gauge: drives its fill width, its
      readouts, and its spend color. */
  const PV_PCT = 0.82;
  $effect(() => {
    void $themeTick;
    document.documentElement.style.setProperty("--hot", gaugeColor(PV_PCT));
  });

  let cfg = $state<Config | null>(null);
  let slot = $state<Slot>("ui");
  let prefs = $state<FontPrefs>({ ...DEFAULT_FONTS });

  // Shipped sets first, then saved ones. A shipped set is read-only: it has to
  // keep meaning the same thing from one version to the next, so editing it is
  // cloning it, exactly as the theme list works.
  let stored = $state<StoredSet[]>([]);
  let sets = $derived([...FONT_SETS, ...userSets(stored)]);
  // The set you last applied, remembered even after you change a face, so a
  // saved set can be edited: pick it, adjust, then commit the change back into
  // it. Without this the only way to change a set would be to delete it and
  // save a new one, losing its name.
  let picked = $state<string | null>(null);

  // A clone holds the same values as the set it came from, so more than one set
  // can match the screen at once. The one you actually clicked wins; only when
  // that one no longer matches does it fall back to whichever set does.
  let activeSet = $derived.by(() => {
    const mine = sets.filter((x) => x.id === picked);
    return matchSet(prefs, mine) ?? matchSet(prefs, sets);
  });

  let dirty = $derived(
    picked != null && activeSet !== picked && sets.some((x) => x.id === picked && !x.factory),
  );


  (async () => {
    cfg = await invoke<Config>("get_config");
    applyTheme(cfg.theme);
    prefs = {
      font_ui: cfg.font_ui,
      font_num: cfg.font_num,
      font_mono: cfg.font_mono,
      font_weight: cfg.font_weight,
      size_ui: cfg.size_ui,
      size_num: cfg.size_num,
      size_mono: cfg.size_mono,
    };
    stored = cfg.font_sets ?? [];
    applyFonts(prefs);
  })();

  // Follow live theme changes: the preview swatches here are read out of the
  // active palette, so a theme switch elsewhere would otherwise leave this
  // window showing the old colors.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => applyTheme(e.payload)).then((f) => (un = f));
    return () => un?.();
  });

  const KEY = { ui: "font_ui", num: "font_num", mono: "font_mono" } as const;

  // Everything installed on this machine, listed under the bundled catalog.
  // Unlike the bundled faces these do not travel with the app: a config that
  // names one falls back to the system face on a machine without it.
  let installed = $state<string[]>([]);
  let filter = $state("");
  void invoke<string[]>("system_fonts")
    .then((f) => (installed = f))
    .catch(() => {});

  let query = $derived(filter.trim().toLowerCase());
  let shown = $derived.by(() => {
    return query ? installed.filter((f) => f.toLowerCase().includes(query)) : installed;
  });

  /** Which face is currently in a slot. */
  function chosen(s: Slot): string {
    return prefs[KEY[s]];
  }

  // Persist + broadcast. Like the theme browser, picking does NOT close the
  // window: comparing faces is the whole point of being here.
  async function save() {
    applyFonts(prefs);
    void emit("fonts", { ...prefs });
    if (!cfg) return;
    cfg = { ...cfg, ...prefs };
    await invoke("set_config", { config: cfg });
  }

  function pick(s: Slot, id: string) {
    prefs = { ...prefs, [KEY[s]]: id };
    void save();
  }

  // Size is per slot: it is the trim for the face in that slot, so it follows
  // the tab rather than sitting on its own like boldness.
  let size = $derived(prefs[SIZE_KEY[slot]]);

  // Where each slider sits in its own range, 0..1, so the track can fill to the
  // thumb and the thumb can take the scale color at that point.
  let boldF = $derived((prefs.font_weight + 2) / 4);
  let sizeF = $derived((size - SIZE_MIN) / (SIZE_MAX - SIZE_MIN));
  let boldColor = $derived.by(() => {
    void $themeTick;
    return gaugeColor(boldF);
  });
  let sizeColor = $derived.by(() => {
    void $themeTick;
    return gaugeColor(sizeF);
  });

  function setSize(v: number) {
    prefs = { ...prefs, [SIZE_KEY[slot]]: v };
    void save();
  }

  function setWeight(v: number) {
    prefs = { ...prefs, font_weight: v };
    void save();
  }

  async function saveSets() {
    if (!cfg) return;
    cfg = { ...cfg, font_sets: stored };
    await invoke("set_config", { config: cfg });
  }

  function applySet(id: string) {
    const found = sets.find((x) => x.id === id);
    if (!found) return;
    picked = id;
    prefs = { ...found.prefs };
    void save();
  }

  /** Write what is on screen back into a set of your own. */
  function updateSet(id: string) {
    stored = stored.map((x) => (x.id === id ? { ...x, ...prefs } : x));
    void saveSets();
  }

  /** Clone whatever is on screen into a set of your own. */
  function cloneSet() {
    const from = sets.find((x) => x.id === activeSet);
    const base = from ? `${from.label} copy` : "My set";
    const label = freeLabel(
      base,
      sets.map((x) => x.label),
    );
    const id = "u" + Date.now().toString(36);
    stored = [...stored, toStored(id, label, prefs)];
    picked = id;
    void saveSets();
    renaming = id;
  }

  function removeSet(id: string) {
    if (picked === id) picked = null;
    stored = stored.filter((x) => x.id !== id);
    void saveSets();
  }

  let renaming = $state<string | null>(null);

  function rename(id: string, label: string) {
    const t = label.trim();
    renaming = null;
    if (!t) return;
    stored = stored.map((x) => (x.id === id ? { ...x, label: t } : x));
    void saveSets();
  }

  function focusNow(el: HTMLInputElement) {
    el.focus();
    el.select();
  }

  function resetAll() {
    prefs = { ...DEFAULT_FONTS };
    void save();
  }

  // The preview cards render at the live boldness so the slider moves every
  // sample at once, not just the chosen one.
  let rungs = $derived(ladder(prefs.font_weight));

  // The slots the card is NOT editing render at their current setting, so the
  // sample is the real row rather than one face pretending to be all three.
  const uiFace = { "font-family": "var(--font-ui)", "font-size-adjust": "var(--font-ui-adj)" };
  const numFace = { "font-family": "var(--font-num)", "font-size-adjust": "var(--font-num-adj)" };
  const css = (o: Record<string, string>) =>
    Object.entries(o)
      .map(([k, v]) => `${k}:${v}`)
      .join(";");

  // The search reads the name, the group and the note, so "condensed",
  // "cockpit" or "retro" find a face whose name gives no clue what it is.
  let list = $derived.by<{ group: string; fonts: FontDef[] }[]>(() => {
    const hit = (f: FontDef) =>
      !query ||
      [f.label, f.group, f.note ?? ""].some((t) => t.toLowerCase().includes(query));
    const all = fontsFor(slot).filter(hit);
    return GROUP_ORDER.map((g) => ({
      group: g,
      fonts: all.filter((f) => f.group === g),
    })).filter((g) => g.fonts.length > 0);
  });

  let nothing = $derived(query !== "" && list.length === 0 && shown.length === 0);

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") void getCurrentWindow().close();
  }
</script>

{#snippet sample(stack: string, adj: number | undefined)}
  {@const face = { "font-family": stack, "font-size-adjust": adj != null ? String(adj) : "none" }}
  {#if slot === "mono"}
    <!-- The mono slot is only ever the Explorer's reading pane, so the sample
         is that pane: an author line over a wrapped body, at the size and line
         height the pane actually uses. Judging a reading face needs several
         wrapped lines, not one string. -->
    <div class="pv-auth" style={css(face)} style:font-weight={rungs[4]}>assistant</div>
    <div class="pv-body" style={css(face)} style:font-weight={rungs[1]}>
      Read 12,480 tokens from the transcript tail, ending at line 3,097. Nothing
      newer on disk.
    </div>
  {:else}
    <!-- Everywhere else the sample is a miniature of a main-window session row
         at its real sizes, with only the slot being edited set in the candidate
         face: the interface tab varies the labels and leaves the numbers on
         their current face, the numbers tab does the reverse. You are looking
         at the row you will actually get. -->
    {@const ui = slot === "ui" ? face : uiFace}
    {@const num = slot === "num" ? face : numFace}
    <div class="pvl1">
      <span class="pv-t" style={css(ui)} style:font-weight={rungs[2]}>Greedout</span>
      <span class="pv-meta" style={css(num)} style:font-weight={rungs[1]}>4m ago</span>
    </div>
    <div class="pvl2">
      <span class="pv-spend" style={css(num)} style:font-weight={rungs[3]}>$4.21</span>
      <span class="pv-track"><span class="pv-fill"></span></span>
      <span class="pv-pct" style={css(num)} style:font-weight={rungs[1]}>82%</span>
      <span class="pv-ctx" style={css(num)} style:font-weight={rungs[1]}>164k/200k</span>
    </div>
  {/if}
{/snippet}

<svelte:window onkeydown={onKey} />

<div class="bar" data-tauri-drag-region>
  <Brand size={16} font={15} />
  <span class="title">Fonts</span>
  <span class="spacer"></span>
  <button class="x" onclick={() => getCurrentWindow().close()} title="Close">✕</button>
</div>

<div class="body">
  <aside class="nav">
    <div class="slots">
      {#each SLOTS as s (s.id)}
        {@const cur = fontById(chosen(s.id), s.id)}
        <button class="slot" class:active={slot === s.id} onclick={() => (slot = s.id)}>
          <span class="slabel">{s.label}</span>
          <span
            class="sfont"
            style:font-family={cur.stack}
            style:font-size-adjust={cur.adj != null ? String(cur.adj) : "none"}>{cur.label}</span
          >
          <span class="shint">{s.hint}</span>
        </button>
      {/each}
    </div>

    <div class="bold">
      <div class="brow">
        <span class="blabel">Boldness</span>
        <span class="bval">{BOLDNESS_LABELS[prefs.font_weight + 2]}</span>
      </div>
      <input
        type="range"
        min="-2"
        max="2"
        step="1"
        value={prefs.font_weight}
        style:--f={boldF}
        style:--tc={boldColor}
        title="Right-click to reset"
        oninput={(e) => setWeight(Number(e.currentTarget.value))}
        oncontextmenu={(e) => {
          e.preventDefault();
          setWeight(0);
        }}
      />
    </div>

    <div class="bold">
      <div class="brow">
        <span class="blabel">Size</span>
        <span class="bval">{sizeLabel(size)}</span>
      </div>
      <input
        type="range"
        min={SIZE_MIN}
        max={SIZE_MAX}
        step="0.05"
        value={size}
        style:--f={sizeF}
        style:--tc={sizeColor}
        title="Right-click to reset"
        oninput={(e) => setSize(Number(e.currentTarget.value))}
        oncontextmenu={(e) => {
          e.preventDefault();
          setSize(1);
        }}
      />
    </div>

    <button
      class="rst all"
      onclick={resetAll}
      title="Back to the system faces at standard weight and size"
    >
      Reset all
    </button>

    <div class="sets">
      <span class="setlabel">Sets</span>
      <div class="setrow">
        {#each sets as st (st.id)}
          {#if renaming === st.id}
            <input
              class="setname"
              value={st.label}
              onblur={(e) => rename(st.id, e.currentTarget.value)}
              onkeydown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
                if (e.key === "Escape") renaming = null;
              }}
              use:focusNow
            />
          {:else}
            <span
              class="setwrap"
              class:mine={!st.factory}
              class:on={activeSet === st.id}
              class:edit={dirty && picked === st.id}
            >
              <button
                class="set"
                class:active={activeSet === st.id}
                class:editing={dirty && picked === st.id}
                ondblclick={() => (st.factory ? undefined : (renaming = st.id))}
                onclick={() => applySet(st.id)}
                title={st.factory
                  ? "Built in: applies all three faces, weight and sizes"
                  : "Your set. Double-click to rename."}
              >
                {st.label}
              </button>
              {#if !st.factory}
                {#if dirty && picked === st.id}
                  <button
                    class="setsave"
                    onclick={() => updateSet(st.id)}
                    title="Save the current faces, weight and sizes into this set"
                  >
                    <Icon name="check" size={12} />
                  </button>
                {/if}
                <button class="setx" onclick={() => removeSet(st.id)} title="Delete this set">
                  <Icon name="x" size={11} />
                </button>
              {/if}
            </span>
          {/if}
        {/each}
        <button class="set add" onclick={cloneSet} title="Save what is on screen as your own set">
          +
        </button>
      </div>
    </div>
  </aside>

  <div class="pane">
    <div class="search">
      <input
        class="q"
        placeholder="Search {fontsFor(slot).length} bundled and {installed.length} installed fonts"
        bind:value={filter}
      />
      {#if filter}
        <button class="qx" onclick={() => (filter = "")} title="Clear">✕</button>
      {/if}
    </div>

    <div class="scroll">
  {#each list as g (g.group)}
    <div class="grouplabel">{g.group}</div>
    <div class="grid">
      {#each g.fonts as f (f.id)}
        <button
          class="card"
          class:active={chosen(slot) === f.id}
          onclick={() => pick(slot, f.id)}
          title={f.label}
        >
          <div class="pv" class:doc={slot === "mono"} style:zoom={size}>
            {@render sample(f.stack, f.adj)}
          </div>
          <div class="foot">
            <span
              class="fname"
              style:font-weight={rungs[3]}
              style:font-family={f.stack}
              style:font-size-adjust={f.adj != null ? String(f.adj) : "none"}>{f.label}</span
            >
            <span class="fw">{f.weights}</span>
            {#if chosen(slot) === f.id}<span class="check"><Icon name="check" size={12} /></span>{/if}
          </div>
        </button>
      {/each}
    </div>
  {/each}

  {#if installed.length && shown.length}
    <div class="grouplabel sysrow">
      <span>Installed on this PC ({query ? `${shown.length} of ${installed.length}` : installed.length})</span>
    </div>
    <div class="grid">
      {#each shown as fam (fam)}
        {@const f = systemFont(fam, slot)}
        <button
          class="card"
          class:active={chosen(slot) === f.id}
          onclick={() => pick(slot, f.id)}
          title={fam}
        >
          <div class="pv" class:doc={slot === "mono"} style:zoom={size}>
            {@render sample(f.stack, undefined)}
          </div>
          <div class="foot">
            <span class="fname" style:font-weight={rungs[3]} style:font-family={f.stack}
              >{fam}</span
            >
            {#if chosen(slot) === f.id}<span class="check"><Icon name="check" size={12} /></span>{/if}
          </div>
        </button>
      {/each}
    </div>
  {/if}

  {#if nothing}
    <div class="none">No font matches “{filter}”.</div>
  {/if}
    </div>
  </div>
</div>

<style>
  :global(html),
  :global(body) {
    background: var(--bg);
    color: var(--fg);
    height: 100%;
    overflow: hidden;
  }
  /* Column layout so the font list takes whatever is left over. The set row
     wraps to a second line on a narrow window, so measuring the chrome by a
     fixed pixel count would cut the list off. */
  :global(#app) {
    display: flex;
    flex-direction: column;
    height: 100%;
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 8px 6px;
    border-bottom: 1px solid var(--edge);
  }
  .title {
    font-size: calc(14px * var(--size-ui));
    font-weight: var(--w-bold);
  }
  .spacer {
    flex: 1;
  }
  .x {
    background: none;
    border: 0;
    color: var(--muted);
    cursor: pointer;
    font-size: calc(13px * var(--size-ui));
    padding: 2px 5px;
    border-radius: 4px;
  }
  .x:hover {
    background: var(--hover);
    color: var(--fg);
  }

  /* Everything that changes the configuration lives in one column, read top
     to bottom in the order the work happens: which slot you are editing, how
     bold and how big it is, then the whole configuration as a saved set. The
     list beside it is the only thing that has to scroll with the window. */
  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }
  .nav {
    flex: 0 0 auto;
    width: 196px;
    display: flex;
    flex-direction: column;
    gap: 11px;
    padding: 8px;
    border-right: 1px solid var(--edge);
    overflow-y: auto;
  }

  /* A set is the whole configuration at once, which is the harder question, so
     it stays visible rather than hiding behind the three separate decisions. */
  .sets {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .setlabel {
    font-size: calc(11px * var(--size-ui));
    color: var(--muted);
  }
  .setrow {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .set {
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(10px * var(--size-ui));
    color: var(--muted);
    background: none;
    border: 1px solid var(--edge);
    border-radius: 999px;
    padding: 2px 8px;
    cursor: pointer;
  }
  /* A saved set carries its own delete button, tucked against the chip so the
     pair reads as one control rather than two. */
  .setwrap {
    display: inline-flex;
    align-items: stretch;
  }
  .setwrap.mine {
    border: 1px solid var(--edge);
    border-radius: 999px;
    overflow: hidden;
  }
  .setwrap.mine.on {
    border-color: var(--g0);
  }
  .setwrap.mine.edit {
    border-color: var(--g1);
    border-style: dashed;
  }
  .setwrap.mine .set {
    border: none;
    border-radius: 0;
  }
  .set.editing {
    border-color: var(--g1);
    border-style: dashed;
    color: var(--fg);
  }
  .setwrap.mine .set.active,
  .setwrap.mine .set.editing {
    border: none;
  }
  .setsave {
    display: grid;
    place-items: center;
    color: var(--g1);
    background: none;
    border: none;
    border-left: 1px solid var(--edge);
    padding: 0 5px;
    cursor: pointer;
  }
  .setsave:hover {
    background: var(--hover);
  }
  .setx {
    display: grid;
    place-items: center;
    color: var(--muted);
    background: none;
    border: none;
    border-left: 1px solid var(--edge);
    padding: 0 6px;
    cursor: pointer;
  }
  .setx:hover {
    background: var(--hover);
    color: var(--red);
  }
  .setname {
    font: inherit;
    font-size: calc(10px * var(--size-ui));
    font-size-adjust: var(--font-ui-adj);
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--g0);
    border-radius: 999px;
    padding: 2px 8px;
    width: 11ch;
  }
  .set.add {
    border-style: dashed;
  }
  .set:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .set.active {
    border-color: var(--g0);
    color: var(--fg);
  }

  /* Slot picker. Each tab names the role AND the face currently in it, so the
     window doubles as a summary of the whole font configuration. */
  /* Stacked cards rather than a tab strip: each one carries the face it is
     currently set to, so the column reads as the configuration itself. */
  .slots {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .slot {
    display: flex;
    flex-direction: column;
    gap: 1px;
    align-items: flex-start;
    padding: 5px 8px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
    text-align: left;
  }
  .slot:hover {
    background: var(--hover);
  }
  .slot.active {
    border-color: var(--g0);
  }
  .slabel {
    font-size: calc(11px * var(--size-ui));
    font-weight: var(--w-semibold);
  }
  .sfont,
  .shint {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sfont {
    font-size: calc(10px * var(--size-ui));
    color: var(--muted);
  }
  .shint {
    font-size: calc(9px * var(--size-ui));
    color: var(--muted);
    opacity: 0.75;
  }

  .bold {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .brow {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
  }
  .blabel {
    font-size: calc(11px * var(--size-ui));
    color: var(--muted);
  }
  .bold input[type="range"] {
    width: 100%;
    height: 13px;
    appearance: none;
    -webkit-appearance: none;
    background: none;
    cursor: pointer;
  }
  /* The scale is laid across the whole track and then masked off at the thumb,
     so the color under the thumb is the scale's real color at that position
     rather than a squeezed copy of the whole range. --end backs the thumb's own
     travel out of the width, since a centered thumb never reaches either edge. */
  .bold input[type="range"]::-webkit-slider-runnable-track {
    --end: calc(var(--f) * (100% - 11px) + 5.5px);
    height: 5px;
    border-radius: 3px;
    background:
      linear-gradient(90deg, transparent var(--end), var(--track) var(--end)),
      linear-gradient(90deg, var(--g0), var(--g1) 52%, var(--g2));
  }
  .bold input[type="range"]::-webkit-slider-thumb {
    -webkit-appearance: none;
    width: 11px;
    height: 11px;
    margin-top: -3px;
    border-radius: 50%;
    background: var(--tc);
    border: 1px solid var(--bg);
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.55);
  }
  .bval {
    font-size: calc(11px * var(--size-ui));
  }
  .rst {
    background: none;
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--muted);
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(10px * var(--size-ui));
    padding: 2px 7px;
  }
  .rst.all {
    align-self: flex-start;
  }
  .rst:hover {
    background: var(--hover);
    color: var(--fg);
  }
  .pane {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }
  /* Pinned above the list rather than scrolling with it: with fifty bundled
     faces and every face on the machine below them, the box has to be reachable
     from wherever you have scrolled to. */
  .search {
    position: relative;
    padding: 8px 8px 0;
  }
  .q {
    width: 100%;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(11px * var(--size-ui));
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--edge);
    border-radius: 5px;
    padding: 4px 24px 4px 8px;
  }
  .q:focus {
    outline: none;
    border-color: var(--g0);
  }
  .qx {
    position: absolute;
    right: 12px;
    top: 50%;
    transform: translateY(-25%);
    background: none;
    border: 0;
    color: var(--muted);
    cursor: pointer;
    font-size: calc(10px * var(--size-ui));
    padding: 2px 4px;
  }
  .qx:hover {
    color: var(--fg);
  }
  .scroll {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 8px 8px 10px;
  }
  .grouplabel {
    font-size: calc(10px * var(--size-ui));
    font-weight: var(--w-semibold);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
    margin: 10px 0 5px;
  }
  /* The installed list is long, so its header says how much of it is showing.
     No x-height pin here: these faces are unknown, and guessing a pin for one
     is worse than letting it render at its natural size. */
  .sysrow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .none {
    font-size: calc(11px * var(--size-ui));
    color: var(--muted);
    padding: 8px 2px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(196px, 1fr));
    gap: 8px;
  }
  .card {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 8px 9px 7px;
    border: 1px solid var(--edge);
    border-radius: 7px;
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
    text-align: left;
    overflow: hidden;
  }
  .card:hover {
    background: var(--hover);
  }
  .card.active {
    outline: 2px solid var(--fg);
    outline-offset: -1px;
  }
  /* The sample is the only part rendered in the candidate face; the label row
     below stays in the interface font so names never become unreadable in a
     pixel or dot-matrix face. */
  .pv {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-height: 42px;
    justify-content: center;
    overflow: hidden;
  }
  .pvl1,
  .pvl2 {
    display: flex;
    gap: 5px;
    white-space: nowrap;
  }
  .pvl1 {
    align-items: baseline;
  }
  .pvl2 {
    align-items: center;
  }
  .pv-t {
    font-size: 12px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .pv-meta {
    font-size: 11px;
    color: var(--muted);
    margin-left: auto;
  }
  .pv-spend {
    font-size: 13px;
    color: var(--hot);
  }
  /* Same gauge gradient as a real row: the fill is a window onto the full
     scale, so 82% shows the colors an 82% row shows. Width and background-size
     move together, the second being 10000 / the first. */
  .pv-track {
    flex: 1;
    min-width: 16px;
    height: 5px;
    border-radius: 3px;
    background: var(--track);
    overflow: hidden;
  }
  .pv-fill {
    display: block;
    height: 100%;
    width: 82%;
    background: linear-gradient(90deg, var(--g0), var(--g1) 52%, var(--g2));
    background-size: 122% 100%;
  }
  .pv-pct {
    font-size: 12px;
  }
  .pv-ctx {
    font-size: 12px;
    color: var(--muted);
  }
  /* Reading-pane sample: left-aligned, wrapped, clamped to three lines. */
  .pv.doc {
    white-space: normal;
    min-height: 56px;
    justify-content: flex-start;
  }
  .pv-auth {
    font-size: 11px;
    color: var(--g0);
  }
  .pv-body {
    font-size: 11px;
    line-height: 1.45;
    opacity: 0.85;
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .foot {
    display: flex;
    align-items: baseline;
    gap: 5px;
    border-top: 1px solid var(--edge-soft);
    padding-top: 5px;
  }
  .fname {
    font-size: calc(11px * var(--size-ui));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fw {
    font-size: calc(9px * var(--size-ui));
    color: var(--muted);
    margin-left: auto;
    white-space: nowrap;
  }
  .check {
    display: flex;
    align-items: center;
    color: var(--g0);
  }
</style>
