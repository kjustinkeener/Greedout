<script lang="ts">
  import type { Session, Sample, HistHover } from "../types";
  import Speedo from "./Speedo.svelte";
  import History from "./History.svelte";
  import { gaugeColor, spendFor, spendOverride, themeTick } from "./theme";
  import { agoTip, ctxTip, limitsTip, modelTip, sizeTip, moneyTip, spendTip } from "./tips";
  import { t } from "./i18n.svelte";

  let {
    session,
    samples,
    onRename,
    onOpenSession,
    onOpenProject,
    onOpenCompact,
    showPrompt = true,
    linksEnabled = true,
  }: {
    session: Session;
    samples: Sample[];
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
</script>

<div class="panel" title={session.projectPath}>
  <div class="head">
    <span class="dot" class:live={session.live}></span>
    <button
      class="title"
      class:plain={!linksEnabled}
      onclick={titleClick}
      ondblclick={titleDbl}
      title={linksEnabled ? t("main.exploreOrRename") : t("main.rename")}
      >{session.title}</button
    >
    <span class="meta1">
      <span class="ago" title={agoTip()}>{fmtAgo(session.mtime)}</span><span class="mid">·</span>
      <button
        class="project"
        class:plain={!linksEnabled}
        onclick={() => { if (linksEnabled) onOpenProject(); }}
        title={linksEnabled ? t("main.exploreProject") : undefined}
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
    <div class="speedo" title={ctxTip()}><Speedo pct={session.pct} ctx={session.ctx} target={session.target} {fmtK} /></div>
    <div class="spend" title={hov ? moneyTip() : spendTip(session.costUsd)}>
      <span class="lbl"
        >{hov ? hov.label : t("main.spend")}{#if hov}<span class="at">{hov.time}</span>{/if}</span
      >
      <span class="val" style:color={hov ? hovColor : spendColor}
        >{hov ? hov.value : fmtUsd(session.costUsd)}</span
      >
    </div>
  </div>

  {#if session.compact}
    <button
      class="compact"
      onclick={() => onOpenCompact?.()}
      title={t("main.compactTip")}
    >
      <span class="cico">⟳</span>
      <span class="cmain"
        >{t("main.compacted")} <span class="csz">{fmtK(session.compact.pre)}<span class="carr">&rarr;</span>{fmtK(session.compact.post)}</span></span
      >
      <span class="cago">{fmtAgo(Math.floor(session.compact.tsMs / 1000))}</span>
      <span class="cread">{t("main.compactRead")}</span>
    </button>
  {/if}

  <History {samples} target={session.target} onHover={(h) => (hov = h)} />

  {#if showPrompt && session.subtitle}
    <div class="subtitle">{session.subtitle}</div>
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
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    margin-left: -20px;
  }
  .spend .lbl {
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    font-size: calc(9px * var(--size-ui));
    white-space: nowrap;
  }
  /* Clock time of the hovered point, kept on the label line so the block's
     height never changes as the readout swaps. */
  .spend .at {
    margin-left: 4px;
    opacity: 0.75;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
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
  /* Recent-compaction strip: a full-width bar under the gauge, present for ~3
     turns after a /compact. Click opens the summary reader. */
  .compact {
    display: flex;
    align-items: baseline;
    gap: 6px;
    width: 100%;
    margin: 2px 0 3px;
    padding: 3px 6px;
    background: var(--panel-2, rgba(127, 127, 127, 0.1));
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-size: calc(10px * var(--size-ui));
    text-align: left;
    cursor: pointer;
  }
  .compact:hover {
    border-color: var(--muted);
  }
  .compact .cico {
    flex: 0 0 auto;
    color: var(--g1);
    font-weight: var(--w-bold);
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
