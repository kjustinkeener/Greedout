<script lang="ts">
  import type { Sample, HistHover } from "../types";
  import { t } from "./i18n.svelte";

  // onHover reports the hovered line to the parent, which swaps its big spend
  // readout for that line's value (the graph itself shows no floating tooltip).
  let {
    samples,
    target,
    onHover,
  }: {
    samples: Sample[];
    target: number;
    onHover?: (h: HistHover | null) => void;
  } = $props();


  // Plot area fills nearly the full viewBox; the green/yellow scale labels are
  // overlaid in the corners rather than eating into side gutters.
  const W = 240,
    H = 72,
    PX = 3,
    PT = 8,
    PB = 14;
  const x0 = PX,
    x1 = W - PX,
    yTop = PT,
    yBot = H - PB;

  const fmtUsd = (n: number) => (n >= 1 ? `$${n.toFixed(2)}` : `${(n * 100).toFixed(1)}¢`);
  const fmtK = (n: number) => `${Math.round(n / 1000)}k`;
  const fmtTok = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${Math.round(n)}`);
  const fmtTime = (t: number) =>
    new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

  // Build both polylines against their own auto-scaled axis (shared x = time).
  let plot = $derived.by(() => {
    // Draw strictly left-to-right in time; the seed/live merge can interleave.
    const sorted = [...samples].sort((a, b) => a.t - b.t);
    const n = sorted.length;
    if (n < 2) return null;
    // Context is scaled to the session's target so its height matches the speedo
    // (a flat 77k/200k sits at ~39%, not pinned to the top). Spend gets 15%
    // headroom so its latest point isn't glued to the top edge.
    const ctxVals = sorted.map((s) => s.ctx).filter((v): v is number => v != null);
    const ctxMax = Math.max(target, 1, ...ctxVals);
    const costMax = Math.max(1e-6, ...sorted.map((s) => s.cost)) * 1.15;
    // x by real time, so points hold their place and only the right edge grows.
    const t0 = sorted[0].t;
    const span = Math.max(1, sorted[n - 1].t - t0);
    const sx = (t: number) => x0 + ((t - t0) / span) * (x1 - x0);
    const sy = (v: number, max: number) => yBot - (v / max) * (yBot - yTop);

    const known = sorted.filter((s): s is Sample & { ctx: number } => s.ctx != null);
    const ctxPts = known.map((s) => ({ x: sx(s.t), y: sy(s.ctx, ctxMax) }));
    const ctxLine = ctxPts.map((p) => `${p.x.toFixed(1)},${p.y.toFixed(1)}`).join(" ");

    // The session's startup floor (first known context), drawn as a
    // reference line so a post-compact dip toward it is legible.
    const startupY = known.length ? sy(known[0].ctx, ctxMax) : null;
    const startupCtx = known.length ? known[0].ctx : null;

    // When context is unknown after a compact, the newest samples carry null ctx
    // (cost still moves), so the green line ends before the right edge, leaving a
    // gap. Treat unknown as ~0: drop straight down from the last known point, then
    // run flat to the edge, so the line and fill reach the right side.
    const lastPt = ctxPts.length ? ctxPts[ctxPts.length - 1] : null;
    const stale = lastPt ? x1 - lastPt.x > 0.5 : false;
    const tail = stale && lastPt ? ` ${lastPt.x.toFixed(1)},${yBot} ${x1.toFixed(1)},${yBot}` : "";
    const ctxStroke = ctxLine + tail;
    // Close the context line down to the baseline into a filled area (box). When
    // stale the tail already reaches the baseline at the right edge.
    const ctxArea = ctxPts.length
      ? `${ctxLine}${tail || ` ${lastPt!.x.toFixed(1)},${yBot}`} ${ctxPts[0].x.toFixed(1)},${yBot}`
      : "";
    const costLine = sorted
      .map((s) => `${sx(s.t).toFixed(1)},${sy(s.cost, costMax).toFixed(1)}`)
      .join(" ");

    // Per-sample screen coords, so hover can snap to a real sample instead of
    // interpolating along the drawn polyline.
    const pts = sorted.map((s) => ({
      t: s.t,
      x: sx(s.t),
      ctx: s.ctx,
      ctxY: s.ctx != null ? sy(s.ctx, ctxMax) : yBot,
      cost: s.cost,
      costY: sy(s.cost, costMax),
    }));

    const last = sorted[n - 1];
    return {
      ctxStroke,
      ctxArea,
      costLine,
      startupY,
      startupCtx,
      pts,
      ctxNow: last.ctx ?? 0,
      costNow: last.cost,
      ctxMax,
      costMax,
    };
  });

  type Line = HistHover["line"];
  // Hovered line plus its marker anchor in viewBox units.
  let hover = $state<{ line: Line; vx: number; vy: number } | null>(null);

  // Nearest line wins, but only within this vertical reach (viewBox units), so
  // moving away from all three clears the hover instead of sticking to one.
  const REACH = 14;

  function onMove(e: MouseEvent) {
    const p = plot;
    if (!p) return;
    const el = e.currentTarget as SVGSVGElement;
    const r = el.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const vx = ((e.clientX - r.left) / r.width) * W;
    const vy = ((e.clientY - r.top) / r.height) * H;

    // Snap to the sample nearest in time, then pick the nearest of the 3 lines.
    let pt = p.pts[0];
    for (const q of p.pts) if (Math.abs(q.x - vx) < Math.abs(pt.x - vx)) pt = q;

    const cands: { line: Line; y: number }[] = [
      { line: "ctx", y: pt.ctxY },
      { line: "cost", y: pt.costY },
    ];
    if (p.startupY != null) cands.push({ line: "floor", y: p.startupY });

    let best = cands[0];
    for (const c of cands) if (Math.abs(c.y - vy) < Math.abs(best.y - vy)) best = c;
    if (Math.abs(best.y - vy) > REACH) {
      clearHover();
      return;
    }

    if (best.line === "ctx") {
      hover = { line: "ctx", vx: pt.x, vy: pt.ctxY };
      onHover?.({
        line: "ctx",
        label: t("main.context"),
        value: pt.ctx != null ? fmtTok(pt.ctx) : "--",
        time: fmtTime(pt.t),
        pct: pt.ctx != null ? pt.ctx / Math.max(1, target) : null,
      });
    } else if (best.line === "cost") {
      hover = { line: "cost", vx: pt.x, vy: pt.costY };
      onHover?.({
        line: "cost",
        label: t("main.spend"),
        value: fmtUsd(pt.cost),
        time: fmtTime(pt.t),
        pct: null,
      });
    } else {
      hover = { line: "floor", vx, vy: p.startupY! };
      onHover?.({
        line: "floor",
        label: t("main.startupFloor"),
        value: p.startupCtx != null ? fmtTok(p.startupCtx) : "--",
        time: fmtTime(p.pts[0].t),
        pct: p.startupCtx != null ? p.startupCtx / Math.max(1, target) : null,
      });
    }
  }

  function clearHover() {
    hover = null;
    onHover?.(null);
  }

  function onLeave() {
    clearHover();
  }
</script>

<div class="hist">
  {#if !plot}
    <div class="wait">collecting history…</div>
  {:else}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <svg
      viewBox="0 0 {W} {H}"
      role="img"
      aria-label={t("tip.history")}
      onmousemove={onMove}
      onmouseleave={onLeave}
    >
      <defs>
        <!-- Same cyan->yellow->red scale as the speedo; vertical so a point's
             height (= its share of the budget) picks the same color the gauge
             would show at that fill. -->
        <linearGradient id="ctxfill" gradientUnits="userSpaceOnUse" x1="0" y1={yBot} x2="0" y2={yTop}>
          <stop offset="0" stop-color="var(--g0)" />
          <stop offset="0.52" stop-color="var(--g1)" />
          <stop offset="1" stop-color="var(--g2)" />
        </linearGradient>
      </defs>
      <line x1={x0} y1={yBot} x2={x1} y2={yBot} stroke="var(--track)" stroke-width="1" />
      <!-- Green (context) scale, overlaid left -->
      <text class="ax g" x={x0 + 1} y={yTop + 4} text-anchor="start">{fmtK(plot.ctxMax)}</text>
      <text class="ax g" x={x0 + 1} y={yBot - 1} text-anchor="start">0</text>
      <!-- Yellow (spend) scale, overlaid right -->
      <text class="ax y" x={x1 - 1} y={yTop + 4} text-anchor="end">{fmtUsd(plot.costMax)}</text>
      <text class="ax y" x={x1 - 1} y={yBot - 1} text-anchor="end">0</text>
      <polygon
        points={plot.ctxArea}
        fill="url(#ctxfill)"
        opacity={hover ? (hover.line === "ctx" ? 0.45 : 0.12) : 0.3}
        stroke="none"
      />
      {#if plot.startupY != null}
        <line
          x1={x0}
          y1={plot.startupY}
          x2={x1}
          y2={plot.startupY}
          stroke="var(--green)"
          stroke-width={hover?.line === "floor" ? 1 : 0.5}
          stroke-dasharray="2 2"
          opacity={hover ? (hover.line === "floor" ? 1 : 0.25) : 0.6}
        />
      {/if}
      <polyline
        points={plot.ctxStroke}
        fill="none"
        stroke="var(--green)"
        stroke-width={hover?.line === "ctx" ? 1.5 : 0.75}
        opacity={hover && hover.line !== "ctx" ? 0.3 : 1}
      />
      <polyline
        points={plot.costLine}
        fill="none"
        stroke="var(--yellow)"
        stroke-width={hover?.line === "cost" ? 1.5 : 0.75}
        opacity={hover ? (hover.line === "cost" ? 1 : 0.3) : 0.9}
      />
      {#if hover}
        <line
          x1={hover.vx}
          y1={yTop}
          x2={hover.vx}
          y2={yBot}
          stroke="var(--muted)"
          stroke-width="0.4"
          opacity="0.5"
        />
        <circle
          cx={hover.vx}
          cy={hover.vy}
          r="1.8"
          fill={hover.line === "cost" ? "var(--yellow)" : "var(--green)"}
          stroke="var(--bg)"
          stroke-width="0.6"
        />
      {/if}
    </svg>
    <div class="legend">
      <span class="k ctx" class:dim={hover != null && hover.line === "cost"}
        >context {fmtK(plot.ctxNow)}</span
      >
      <span class="k cost" class:dim={hover != null && hover.line !== "cost"}
        >spend {fmtUsd(plot.costNow)}</span
      >
    </div>
  {/if}
</div>

<style>
  .hist {
    width: 100%;
    margin-top: -12px;
    position: relative;
  }
  svg {
    width: 100%;
    height: auto;
    display: block;
  }
  .ax {
    font-size: calc(6px * var(--size-num));
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .ax.g {
    fill: var(--green);
  }
  .ax.y {
    fill: var(--yellow);
  }
  .wait {
    color: var(--muted);
    text-align: center;
    padding: 16px 0;
    opacity: 0.7;
  }
  .legend {
    display: flex;
    justify-content: center;
    gap: 12px;
    margin-top: -14px;
  }
  .k {
    font-family: var(--font-num);
    font-size: calc(1em * var(--size-num) / var(--size-ui));
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    transition: opacity 0.12s ease;
  }
  .k.dim {
    opacity: 0.4;
  }
  .k::before {
    content: "";
    width: 8px;
    height: 2px;
    border-radius: 1px;
  }
  .k.ctx {
    color: var(--green);
  }
  .k.ctx::before {
    background: var(--green);
  }
  .k.cost {
    color: var(--yellow);
  }
  .k.cost::before {
    background: var(--yellow);
  }
</style>
