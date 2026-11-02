<script lang="ts">
  import type { Session, Sample, HistHover, Limits } from "../types";
  import Speedo from "./Speedo.svelte";
  import LimitDial from "./LimitDial.svelte";
  import History from "./History.svelte";
  import Icon from "./Icon.svelte";
  import { gaugeColor, spendFor, spendOverride, themeTick } from "./theme";
  import { agoTip, limitsTip, modelTip, sizeTip, moneyTip, spendTip } from "./tips";
  import { t } from "./i18n.svelte";
  import { invoke } from "@tauri-apps/api/core";

  let {
    session,
    samples,
    limits = null,
    onRename,
    onOpenSession,
    onOpenProject,
    onOpenCompact,
    showPrompt = true,
    linksEnabled = true,
  }: {
    session: Session;
    samples: Sample[];
    /** Plan-limit readings for the two dials under the spend figure. */
    limits?: Limits | null;
    onRename: () => void;
    onOpenSession: () => void;
    onOpenProject: () => void;
    /** Open the compaction summary reader for this session. */
    onOpenCompact?: () => void;
    /** Show the session's latest prompt under its title. */
    showPrompt?: boolean;
    /** Titles open the Explorer on click. Off: plain text (dbl-click still renames). */
    linksEnabled?: boolean;
  } = $props();

  // Single click explores; double click renames (a short timer lets the double
  // click cancel the pending single-click open).
  let clickTimer: ReturnType<typeof setTimeout> | undefined;
  function titleClick() {
    if (!linksEnabled) return;
    clearTimeout(clickTimer);
    clickTimer = setTimeout(() => onOpenSession(), 220);
  }
  function titleDbl() {
    clearTimeout(clickTimer);
    onRename();
  }

  // Compact "time ago" from a unix-seconds mtime: 5s, 3m, 2h, 4d, 1w. Suffixes
  // come from the catalog; see Row.svelte.
  const fmtAgo = (mtime: number) => {
    const s = Math.max(0, Math.floor(Date.now() / 1000 - mtime));
    if (s < 60) return t("ago.seconds", { n: s });
    if (s < 3600) return t("ago.minutes", { n: Math.floor(s / 60) });
    if (s < 86400) return t("ago.hours", { n: Math.floor(s / 3600) });
    if (s < 604800) return t("ago.days", { n: Math.floor(s / 86400) });
    return t("ago.weeks", { n: Math.floor(s / 604800) });
  };

  const fmtK = (n: number | null) =>
    n == null ? "--" : n >= 1_000_000 ? `${(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M` : `${Math.round(n / 1000)}k`;
  const fmtMB = (b: number) => (b / 1_048_576).toFixed(1);
  const fmtUsd = (n: number) => (n >= 1 ? `$${n.toFixed(2)}` : `${(n * 100).toFixed(1)}¢`);
  let spendColor = $derived.by(() => {
    void $themeTick;
    return spendFor(session.pct);
  });

  // Hovering a line in the history graph takes over this readout: the big number
  // becomes that line's value at the hovered point, with its clock time.
  let hov = $state<HistHover | null>(null);
  let hovColor = $derived.by(() => {
    void $themeTick;
    // Same palette override as the spend figure this readout replaces: on a light
    // theme the gauge scale is unreadable here too.
    const fg = spendOverride();
    if (fg) return fg;
    if (!hov) return "var(--yellow)";
    if (hov.line === "cost") return "var(--yellow)";
    return hov.pct == null ? "var(--muted)" : gaugeColor(hov.pct);
  });

  // Hovering a limit dial also takes over the big number: its usage share over the
  // share of its window elapsed, in the dial's own colour.
  let dialHov = $state<{ label: string; usage: number; window: number | null; color: string } | null>(null);
  const pctText = (n: number | null) => (n == null ? "-" : `${Math.round(n * 100)}%`);
  // The usage share takes the inner bar's colour; the window share takes the colour
  // at the end of the outer bar.
  let dialColor = $derived.by(() => {
    void $themeTick;
    return spendOverride() || dialHov?.color || "var(--yellow)";
  });
  let windowColor = $derived.by(() => {
    void $themeTick;
    if (dialHov?.window == null) return "var(--muted)";
    return spendOverride() || gaugeColor(Math.max(0, Math.min(1, dialHov.window)));
  });
  const hoverDial = (label: string) => (h: { usage: number; window: number | null; color: string } | null) =>
    (dialHov = h && { label, ...h });

  // Clicking the dials reads a copied /usage report off the clipboard to calibrate
  // them. The clipboard is read here and nowhere else.
  // The outcome is a single brief glow on the dials: the gauge's 20% green on
  // success, a faster 100% red on failure.
  let dialsEl = $state<HTMLButtonElement>();
  // Right-clicking a dial offers to forget the /usage readings behind it.
  let menu = $state<{ kind: "5h" | "7d"; x: number; y: number } | null>(null);
  function openMenu(kind: "5h" | "7d", e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    // The item is about 130px wide; keep it inside the window.
    menu = { kind, x: Math.min(e.clientX, window.innerWidth - 140), y: Math.min(e.clientY, window.innerHeight - 30) };
  }
  async function clearUsage() {
    const kind = menu?.kind;
    menu = null;
    if (!kind) return;
    try {
      await invoke("clear_limit", { kind });
    } catch {
      // Nothing to report; the dial keeps its last reading.
    }
  }
  async function calibrate() {
    let ok = true;
    try {
      await invoke("calibrate_limits");
    } catch {
      ok = false;
    }
    const c = gaugeColor(ok ? 0.2 : 1);
    const lit = `drop-shadow(0 0 5px ${c}) drop-shadow(0 0 10px ${c})`;
    dialsEl?.animate(
      [{ filter: "drop-shadow(0 0 0 transparent)" }, { filter: lit, offset: 0.35 }, { filter: "drop-shadow(0 0 0 transparent)" }],
      { duration: ok ? 900 : 450, easing: "ease-out" },
    );
  }
</script>

<div class="panel">
  <div class="head">
    <span class="dot" class:live={session.live}></span>
    <button
      class="title"
      class:plain={!linksEnabled}
      onclick={titleClick}
      ondblclick={titleDbl}
      title={`${t("main.sessionTip", { title: session.title })}\n${linksEnabled ? t("main.exploreOrRename") : t("main.rename")}`}
      >{session.title}</button
    >
    <span class="meta1">
      <span class="ago" title={agoTip()}>{fmtAgo(session.mtime)}</span><span class="mid">·</span>
      <button
        class="project"
        class:plain={!linksEnabled}
        onclick={() => { if (linksEnabled) onOpenProject(); }}
        title={linksEnabled ? `${t("main.projectTip", { path: session.projectPath })}\n${t("main.exploreProject")}` : t("main.projectTip", { path: session.projectPath })}
        >{session.project}{#if session.subPath}<span class="sub"
            >/{session.subPath}</span
          >{/if}</button
      >
    </span>
  </div>
  <div class="limits">
    <span class="lim" title={limitsTip()}
      >{t("main.limits", { target: fmtK(session.target), max: fmtK(session.limit) })}</span
    >
    <span class="meta">
      {#if session.model}<span class="model" title={modelTip()}>{session.model}{session.modelVersion ? ` ${session.modelVersion}` : ""}</span><span class="mid">·</span>{/if}
      <span class="size" title={sizeTip()}>{fmtMB(session.sizeBytes)}<span class="unit">MB</span></span>
    </span>
  </div>

  <div class="gauge">
    <div class="speedo"><Speedo pct={session.pct} ctx={session.ctx} target={session.target} {fmtK} /></div>
    <div class="spend">
      <div class="figure" title={hov ? moneyTip() : spendTip(session.costUsd)}>
        {#if dialHov}<span class="lbl below"><span class="who">{dialHov.label}</span> <span style:color={dialColor}>{t("main.limitUsage")}</span> / <span style:color={windowColor}>{t("main.limitWindow")}</span></span>
        {:else if hov}<span class="lbl below">{hov.label}<span class="at">{hov.time}</span></span>{/if}
        {#if dialHov}
          <span class="val pair"
            ><span style:color={dialColor}>{pctText(dialHov.usage)}</span> / <span style:color={windowColor}>{pctText(dialHov.window)}</span></span
          >
        {:else}
          <span class="val" class:lift={hov} style:color={hov ? hovColor : spendColor}
            >{hov ? hov.value : fmtUsd(session.costUsd)}</span
          >
        {/if}
      </div>
      {#if limits}
        <button type="button" class="dials" bind:this={dialsEl} onclick={calibrate}>
          <div class="dial" class:dim={dialHov && dialHov.label !== "5h"} oncontextmenu={(e) => openMenu("5h", e)} role="presentation">
            <LimitDial label="5h" pct={limits.fiveHour.pct} elapsed={limits.fiveHour.elapsed} hint={t("main.limitHint")} onHover={hoverDial("5h")} />
          </div>
          <div class="dial" class:dim={dialHov && dialHov.label !== "7d"} oncontextmenu={(e) => openMenu("7d", e)} role="presentation">
            <LimitDial label="7d" pct={limits.weekly.pct} elapsed={limits.weekly.elapsed} hint={t("main.limitHint")} onHover={hoverDial("7d")} />
          </div>
        </button>
      {/if}
    </div>
  </div>

  {#if menu}
    <div
      class="menuback"
      onclick={() => (menu = null)}
      oncontextmenu={(e) => {
        e.preventDefault();
        menu = null;
      }}
      role="presentation"
    ></div>
    <div class="menu" style:left="{menu.x}px" style:top="{menu.y}px" role="menu">
      <button type="button" role="menuitem" onclick={clearUsage}>{t("main.clearUsage")}</button>
    </div>
  {/if}

  <History {samples} target={session.target} onHover={(h) => (hov = h)} />

  {#if showPrompt && session.subtitle}
    <div class="subtitle">{session.subtitle}</div>
  {/if}

  {#if session.compact}
    <svelte:element
      this={session.compact.hasSummary ? "button" : "div"}
      class="compact"
      class:static={!session.compact.hasSummary}
      role={session.compact.hasSummary ? "button" : undefined}
      onclick={session.compact.hasSummary ? () => onOpenCompact?.() : undefined}
      title={session.compact.hasSummary ? t("main.compactTip") : t("main.compactEncryptedTip")}
    >
      <span class="cico"><Icon name="compact" size={12} /></span>
      <span class="cmain"
        >{t("main.compacted")} <span class="csz">{fmtK(session.compact.pre)}<span class="carr">&rarr;</span>{fmtK(session.compact.post)}</span></span
      >
      <span class="cago">{fmtAgo(Math.floor(session.compact.tsMs / 1000))}</span>
      <span class="cread"
        >{session.compact.hasSummary
          ? t("main.compactRead")
          : t("main.compactEncrypted")}</span
      >
    </svelte:element>
  {/if}
</div>

<style>
  .panel {
    position: relative;
    padding: 8px 10px 10px;
    background: var(--panel);
    border-bottom: 1px solid var(--edge);
  }
  /* Left accent: a static full-height bar with the gauge gradient. */
  .panel::before {
    content: "";
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 2px;
    background-image: linear-gradient(to top, var(--g0) 0%, var(--g1) 52%, var(--g2) 100%);
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    align-self: center;
    flex: 0 0 auto;
    border: 1px solid var(--muted);
    background: transparent;
  }
  .dot.live {
    background: var(--live);
    border-color: var(--live);
    box-shadow: 0 0 4px var(--live);
  }
  /* Time and project keep their natural width; the title is what gives way (it
     has a basis of 0 below), so this header reads like the rows underneath. */
  .meta1 {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex: 0 1 auto;
    min-width: 0;
    max-width: 70%;
    margin-left: auto;
  }
  .ago {
    flex: 0 0 auto;
    font-size: calc(10px * var(--size-num));
    color: var(--muted);
    opacity: 0.8;
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .meta1 .mid {
    flex: 0 0 auto;
    font-size: calc(10px * var(--size-ui));
    margin: 0 -3px;
  }
  .project {
    /* A button does not inherit the panel font, so without this the project
       renders in the browser default and reads as a different typeface. */
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-regular);
    font-size: calc(10px * var(--size-ui));
    color: var(--muted);
    opacity: 0.8;
    flex: 0 1 auto;
    min-width: 6ch;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: none;
    border: none;
    text-align: left;
    padding: 0;
    cursor: pointer;
  }
  /* Sessions started in a subdirectory keep it visible next to the project
     they were folded into, so the row still says where they actually ran. */
  .project .sub {
    opacity: 0.65;
  }
  .project:hover {
    color: var(--fg);
    text-decoration: underline;
  }
  /* Links off: titles are plain text (double-click still renames the session). */
  .title.plain,
  .project.plain {
    cursor: default;
  }
  .title.plain:hover {
    text-decoration: none;
  }
  .project.plain:hover {
    color: var(--muted);
    text-decoration: none;
  }
  .title {
    flex: 1 1 0;
    min-width: 8ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    background: none;
    border: none;
    color: var(--fg);
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-semibold);
    cursor: pointer;
    padding: 0;
  }
  .title:hover {
    text-decoration: underline;
  }
  .size {
    flex: 0 0 auto;
    white-space: nowrap;
    color: var(--fg);
    font-weight: var(--w-bold);
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .size .unit {
    margin-left: 1px;
    font-weight: var(--w-medium);
    color: var(--muted);
  }
  .model {
    color: var(--muted);
    white-space: nowrap;
  }
  .mid {
    color: var(--muted);
    opacity: 0.6;
  }
  .limits {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    color: var(--muted);
    opacity: 0.7;
    font-size: calc(10px * var(--size-num));
    margin-top: 1px;
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .lim {
    flex: 0 1 auto;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta {
    flex: 0 0 auto;
    white-space: nowrap;
    display: flex;
    align-items: baseline;
    gap: 5px;
    color: var(--fg);
  }
  .gauge {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: -10px 0 2px;
  }
  .speedo {
    width: 130px;
    flex: 0 0 auto;
  }
  .spend {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    margin-left: -20px;
  }
  .spend .figure {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
  }
  .dials {
    display: flex;
    justify-content: center;
    gap: 2px;
    width: 100%;
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .menuback {
    position: fixed;
    inset: 0;
    z-index: 50;
  }
  .menu {
    position: fixed;
    z-index: 51;
    background: var(--bg);
    border: 1px solid var(--track);
    border-radius: 4px;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.5);
  }
  .menu button {
    display: block;
    padding: 4px 10px;
    border: 0;
    background: none;
    color: var(--fg);
    font: inherit;
    font-size: calc(11px * var(--size-ui));
    white-space: nowrap;
    cursor: pointer;
  }
  .menu button:hover {
    background: var(--track);
  }
  .dials .dial.dim {
    opacity: 0.45;
  }
  .dials .dial {
    transition: opacity 0.12s;
    flex: 0 1 76px;
    min-width: 0;
  }
  /* Shown only while a graph line is hovered; floats above the number so the
     dials below never shift as the readout swaps. */
  .spend .lbl {
    position: absolute;
    bottom: 100%;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-size: calc(9px * var(--size-ui));
    white-space: nowrap;
  }
  .spend .lbl.below .who {
    color: var(--fg);
  }
  .spend .lbl.below {
    bottom: auto;
    top: calc(100% - 12px);
  }
  /* Clock time of the hovered point, kept on the label line. */
  .spend .at {
    margin-left: 4px;
    opacity: 0.75;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  /* Two shares side by side ("47% / 40%"), scaled rather than resized so the
     figure keeps its height and the dials below do not move. */
  .spend .val.lift {
    transform: translateY(-5px) scale(0.68);
  }
  .spend .val.pair {
    transform: translateY(-5px) scale(0.68);
    color: var(--muted);
    white-space: nowrap;
  }
  .spend .val {
    color: var(--yellow);
    font-size: calc(22px * var(--size-num));
    font-weight: var(--w-bold);
    text-shadow: 0 0 3px var(--spend-shadow);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  /* Recent-compaction strip: a full-bleed band at the very bottom of the panel,
     present for ~3 prompts after a /compact. It breaks out of the panel padding
     to reach both side edges and the bottom border, so the panel's left gradient
     bar (::before) runs down its left too and it reads as part of the same box.
     Click opens the summary reader. */
  .compact {
    display: flex;
    align-items: center;
    gap: 6px;
    /* Full-bleed: an auto-width block only SHIFTS under negative side margins
       (flush left, short right), so widen it explicitly to cover both paddings. */
    width: calc(100% + 20px);
    margin: 8px -10px -10px;
    padding: 5px 10px;
    background: var(--panel-2, rgba(127, 127, 127, 0.12));
    border: none;
    border-top: 1px solid var(--edge);
    color: var(--fg);
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(10px * var(--size-ui));
    text-align: left;
    cursor: pointer;
  }
  .compact:hover {
    background: var(--panel-2, rgba(127, 127, 127, 0.2));
  }
  /* Codex: no readable summary, so the band is inert and its trailing label reads
     as a note rather than a link. */
  .compact.static {
    cursor: default;
  }
  .compact.static:hover {
    background: var(--panel-2, rgba(127, 127, 127, 0.12));
  }
  .compact.static .cread {
    color: var(--muted);
    opacity: 0.8;
    font-style: italic;
  }
  .compact .cico {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    color: var(--g1);
  }
  .compact .cmain {
    flex: 0 1 auto;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .compact .csz {
    color: var(--fg);
    font-weight: var(--w-semibold);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .compact .carr {
    margin: 0 2px;
    color: var(--muted);
  }
  .compact .cago {
    flex: 0 0 auto;
    margin-left: auto;
    color: var(--muted);
    opacity: 0.8;
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .compact .cread {
    flex: 0 0 auto;
    color: var(--g1);
    white-space: nowrap;
  }
  .subtitle {
    margin-top: 4px;
    color: var(--muted);
    opacity: 0.75;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
