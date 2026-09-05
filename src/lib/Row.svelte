<script lang="ts">
  import type { Session } from "../types";
  import { spendFor, themeTick } from "./theme";
  import { agoTip, ctxTip, modelTip, pctTip, sizeTip, spendTip } from "./tips";
  import { t } from "./i18n.svelte";

  let {
    session,
    onRename,
    onOpenSession,
    onOpenProject,
    explorer = true,
    top = false,
    dimHours = 24,
  }: {
    session: Session;
    onRename: () => void;
    onOpenSession: () => void;
    onOpenProject: () => void;
    /** Context Explorer is enabled; when off, titles are plain text (rename only). */
    explorer?: boolean;
    top?: boolean;
    dimHours?: number;
  } = $props();

  // Single click opens the Explorer for this session; double click renames. A
  // short timer lets a double click cancel the pending single-click open.
  let clickTimer: ReturnType<typeof setTimeout> | undefined;
  function titleClick() {
    clearTimeout(clickTimer);
    if (!explorer) return;
    clickTimer = setTimeout(() => onOpenSession(), 220);
  }
  function titleDbl() {
    clearTimeout(clickTimer);
    onRename();
  }

  // Compact "time ago" from a unix-seconds mtime: 5s, 3m, 2h, 4d, 1w. The
  // suffixes are translated, because a language that abbreviates minutes
  // differently should be able to say so.
  const fmtAgo = (mtime: number) => {
    const s = Math.max(0, Math.floor(Date.now() / 1000 - mtime));
    if (s < 60) return t("ago.seconds", { n: s });
    if (s < 3600) return t("ago.minutes", { n: Math.floor(s / 60) });
    if (s < 86400) return t("ago.hours", { n: Math.floor(s / 3600) });
    if (s < 604800) return t("ago.days", { n: Math.floor(s / 86400) });
    return t("ago.weeks", { n: Math.floor(s / 604800) });
  };

  const fmtK = (n: number | null) => (n == null ? "--" : `${Math.round(n / 1000)}k`);
  const fmtMB = (b: number) => (b / 1_048_576).toFixed(1);
  const fmtUsd = (n: number) => `$${n < 10 ? n.toFixed(2) : Math.round(n)}`;

  // Band by pct vs target: green <70%, yellow 70-90%, red >=90%.
  const band = (pct: number | null) => {
    if (pct == null) return "var(--muted)";
    if (pct >= 0.9) return "var(--red)";
    if (pct >= 0.7) return "var(--yellow)";
    return "var(--green)";
  };

  let pct = $derived(session.pct);
  let color = $derived(band(pct));
  // Spend tracks the same cyan->yellow->red scale as the speedo, keyed off the
  // session's context fill so a hot gauge and its spend read the same color.
  let spendColor = $derived.by(() => {
    void $themeTick;
    return spendFor(pct);
  });
  let over = $derived(pct != null && pct > 1);
  // Fill always spans up to the full track (capped at 100%). Once past target the
  // redline slides left to mark where the 100%/target point sits in the fill.
  let widthPct = $derived(pct == null ? 0 : Math.min(100, pct * 100));
  let redlinePct = $derived(over ? (session.target / Math.max(1, session.ctx ?? 1)) * 100 : 100);
  // Anchor the cyan->yellow->red gradient so it spans 0..target. Under target the
  // fill reveals the left slice (gradient sized to the whole track); over target
  // the gradient compresses to the redline (target) and red holds past it.
  let fillBgSize = $derived(
    over ? redlinePct.toFixed(2) : widthPct > 0 ? (10000 / widthPct).toFixed(2) : "100",
  );

  // Age-based dimming: full brightness up to 1h old, floor at 24h+, linear between.
  // Recomputes every poll (mtime changes / snapshot re-render). Focused stays full.
  const DIM_FLOOR = 0.45;
  const FRESH_S = 3600; // 1h
  let opacity = $derived.by(() => {
    if (session.focused) return 1;
    const oldS = Math.max(FRESH_S + 1, dimHours * 3600); // floor age (setting)
    const age = Date.now() / 1000 - session.mtime;
    if (age <= FRESH_S) return 1;
    if (age >= oldS) return DIM_FLOOR;
    return 1 - ((age - FRESH_S) / (oldS - FRESH_S)) * (1 - DIM_FLOOR);
  });
</script>

<div class="row" class:focused={session.focused} style:opacity title={session.projectPath}>
  <span class="dot" class:live={session.live}></span>
  <div class="body">
    <div class="line1">
      <button
        class="title"
        onclick={titleClick}
        ondblclick={titleDbl}
        class:plain={!explorer}
        title={explorer ? t("main.exploreOrRename") : t("main.rename")}
        >{session.title}</button
      >
      <span class="meta1">
        <span class="ago" title={agoTip()}>{fmtAgo(session.mtime)}</span><span class="mid">·</span>
        <button
        class="project"
        class:plain={!explorer}
        onclick={() => explorer && onOpenProject()}
        title={explorer ? t("main.exploreProject") : session.projectPath}
        >{session.project}{#if session.subPath}<span class="sub"
            >/{session.subPath}</span
          >{/if}</button
      >
      </span>
    </div>
    <div class="line2">
      <span class="spend" style:color={spendColor} title={spendTip(session.costUsd)}
        >{fmtUsd(session.costUsd)}</span
      >
      <div class="track" title={ctxTip()}>
        <div class="fill" style:width="{widthPct}%" style:background-size="{fillBgSize}% 100%"></div>
        {#if over}<div class="over" style:left="{redlinePct}%"></div>{/if}
      </div>
      <span class="pct" style:color title={pctTip()}
        >{pct == null ? "--" : Math.round(pct * 100) + "%"}</span
      >
      <span class="ctx" style:color title={ctxTip()}
        >{fmtK(session.ctx)}/{fmtK(session.target)}</span
      >
      {#if top && session.model}<span class="model" title={modelTip()}>{session.model}{session.modelVersion ? ` ${session.modelVersion}` : ""}</span><span class="mid">·</span>{/if}
      <span class="size" title={sizeTip()}>{fmtMB(session.sizeBytes)}<span class="unit">MB</span></span>
    </div>
    {#if session.subtitle}
      <div class="subtitle">{session.subtitle}</div>
    {/if}
  </div>
</div>

<style>
  .row {
    display: flex;
    gap: 6px;
    padding: 5px 8px;
    align-items: flex-start;
    border-bottom: 1px solid var(--edge-soft);
  }
  /* Focused = open in the Claude app: bold + a subtle highlight + left accent. */
  .row.focused {
    background: var(--panel);
    box-shadow: inset 2px 0 0 var(--fg);
  }
  .row.focused .project,
  .row.focused .title {
    font-weight: var(--w-semibold);
    color: var(--fg);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    margin-top: 4px;
    flex: 0 0 auto;
    border: 1px solid var(--muted);
    background: transparent;
  }
  .dot.live {
    background: var(--live);
    border-color: var(--live);
    box-shadow: 0 0 4px var(--live);
  }
  .body {
    flex: 1 1 auto;
    min-width: 0;
  }
  .line1 {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
  }
  .meta1 {
    display: flex;
    align-items: baseline;
    gap: 6px;
    flex: 0 1 auto;
    min-width: 0;
  }
  .ago {
    flex: 0 0 auto;
    color: var(--muted);
    opacity: 0.8;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .meta1 .mid {
    flex: 0 0 auto;
    color: var(--muted);
    opacity: 0.6;
    margin: 0 -3px;
  }
  .project {
    font-weight: var(--w-regular);
    color: var(--muted);
    opacity: 0.8;
    flex: 0 1 auto;
    min-width: 6ch;
    max-width: 75%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: none;
    border: none;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
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
  .title {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-align: left;
    background: none;
    border: none;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
    font-weight: var(--w-medium);
    color: var(--fg);
    cursor: pointer;
    padding: 0;
  }
  .title:hover {
    text-decoration: underline;
  }
  /* With the Explorer off there is nothing to open, so the titles stop
     advertising themselves as links (double-click still renames). */
  .title.plain,
  .project.plain {
    cursor: default;
  }
  .title.plain:hover {
    text-decoration: none;
  }
  .project.plain:hover {
    text-decoration: none;
    color: var(--muted);
  }
  .model {
    flex: 0 0 auto;
    color: var(--muted);
    white-space: nowrap;
  }
  .mid {
    flex: 0 0 auto;
    color: var(--muted);
    opacity: 0.6;
  }
  .ctx {
    flex: 0 0 auto;
    white-space: nowrap;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
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
  .line2 {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-top: calc(-2px * var(--size-ui));
  }
  .spend {
    flex: 0 0 auto;
    width: 44px;
    color: var(--yellow);
    font-size: calc(13px * var(--size-num));
    font-weight: var(--w-semibold);
    text-shadow: 0 0 2px var(--spend-shadow);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .track {
    position: relative;
    flex: 1 1 auto;
    height: 5px;
    background: var(--track);
    border-radius: 3px;
    overflow: hidden;
    filter: drop-shadow(0 0 1px rgba(0, 0, 0, 0.6));
  }
  /* Marks the 100%-of-target line when context has run past it. */
  .over {
    position: absolute;
    top: 0;
    width: 1px;
    height: 100%;
    margin-left: -0.5px;
    background: var(--g2);
    box-shadow: 0 0 0 0.5px rgba(0, 0, 0, 0.6);
  }
  .fill {
    height: 100%;
    border-radius: 3px;
    opacity: 0.8;
    background-color: var(--g2);
    background-image: linear-gradient(90deg, var(--g0) 0%, var(--g1) 52%, var(--g2) 100%);
    background-repeat: no-repeat;
    background-position: left center;
    transition: width 0.3s ease;
  }
  .pct {
    flex: 0 0 auto;
    width: 32px;
    text-align: right;
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .subtitle {
    margin-top: calc(-3px * var(--size-ui));
    font-size: calc(9px * var(--size-ui));
    font-weight: var(--w-light);
    color: var(--fg);
    opacity: 0.93;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
