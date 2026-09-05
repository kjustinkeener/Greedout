<script lang="ts">
  import { t } from "./i18n.svelte";
  import { gaugeColor, themeTick } from "./theme";
  // Context-fill gauge, styled after the logo mark's dial (cyan->yellow->red
  // arc + needle + hub), without the shield behind it. 270-degree sweep with
  // the gap at the bottom; needle maps 0..100% of the context budget.
  let {
    pct,
    ctx,
    target,
    fmtK,
  }: {
    pct: number | null;
    ctx: number | null;
    target: number;
    fmtK: (n: number | null) => string;
  } = $props();

  const CX = 60,
    CY = 60,
    R = 44;
  const START = 135, // lower-left
    SWEEP = 270; // clockwise to lower-right

  const polar = (a: number, r: number) => {
    const rad = (a * Math.PI) / 180;
    return [CX + r * Math.cos(rad), CY + r * Math.sin(rad)];
  };

  const [sx, sy] = polar(START, R);
  const [ex, ey] = polar(START + SWEEP, R);
  const arc = `M ${sx.toFixed(2)} ${sy.toFixed(2)} A ${R} ${R} 0 1 1 ${ex.toFixed(2)} ${ey.toFixed(2)}`;

  // Angular colour sweep along the dial (SVG has no conic gradient): the arc is
  // drawn as many short segments, each coloured by the shared cyan->yellow->red
  // gauge scale (see theme.ts), so colour tracks the fill position.
  const colorAt = gaugeColor;
  const SEG = 60;
  // Recomputed when the theme changes ($themeTick) so the ring picks up the new
  // gauge CSS vars.
  let segs = $derived.by(() => {
    void $themeTick;
    return Array.from({ length: SEG }, (_, i) => {
      const f0 = i / SEG,
        f1 = (i + 1) / SEG;
      const [x1s, y1s] = polar(START + SWEEP * f0, R);
      const [x2s, y2s] = polar(START + SWEEP * f1, R);
      const large = SWEEP * (f1 - f0) > 180 ? 1 : 0;
      return {
        d: `M ${x1s.toFixed(2)} ${y1s.toFixed(2)} A ${R} ${R} 0 ${large} 1 ${x2s.toFixed(2)} ${y2s.toFixed(2)}`,
        color: colorAt((f0 + f1) / 2),
      };
    });
  });

  // Tick marks at 0/25/50/75/100%.
  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => {
    const a = START + SWEEP * f;
    const [x1, y1] = polar(a, R - 2);
    const [x2, y2] = polar(a, R - 8);
    return { x1, y1, x2, y2 };
  });

  let clamped = $derived(pct == null ? 0 : Math.max(0, Math.min(1, pct)));
  let needleAngle = $derived(START + SWEEP * clamped);
  let pctText = $derived(pct == null ? "--" : Math.round(pct * 100) + "%");
  let over = $derived(pct != null && pct >= 1);
</script>

<svg viewBox="0 0 120 100" class="speedo" role="img" aria-label={t("tip.gauge")}>
  <defs>
    <!-- narrow, centered shadow directly under the needle -->
    <filter id="needleShadow" x="-50%" y="-50%" width="200%" height="200%">
      <feDropShadow dx="0" dy="0" stdDeviation="1" flood-color="#000" flood-opacity="0.6" />
    </filter>
  </defs>
  <!-- dial graphics scaled to 80% around the center; the readouts below stay full size -->
  <g transform="translate({CX} {CY}) scale(0.8) translate({-CX} {-CY})">
  <!-- track under the colored arc -->
  <path d={arc} fill="none" stroke="var(--track)" stroke-width="12" stroke-linecap="round" />
  <!-- colored dial: angular sweep built from short segments -->
  <g opacity="0.95" filter="url(#needleShadow)">
    {#each segs as s}
      <path d={s.d} fill="none" stroke={s.color} stroke-width="12" stroke-linecap="round" />
    {/each}
  </g>

  {#each ticks as t}
    <line x1={t.x1} y1={t.y1} x2={t.x2} y2={t.y2} stroke="var(--bg)" stroke-width="2" opacity="0.6" />
  {/each}

  <!-- needle -->
  <g transform="rotate({needleAngle} {CX} {CY})" filter="url(#needleShadow)">
    <path d="M {CX} {CY - 3.2} L {CX + R} {CY} L {CX} {CY + 3.2} Z" fill="#e6edf3" />
  </g>
  <circle cx={CX} cy={CY} r="6.5" fill="#3fb950" filter="url(#needleShadow)" />
  <circle cx={CX} cy={CY} r="2.6" fill="var(--bg)" />
  </g>

  <!-- readouts in the bottom gap -->
  <text x={CX} y="82" text-anchor="middle" class="pct" class:over>{pctText}</text>
  <text x={CX} y="95" text-anchor="middle" class="sub">{fmtK(ctx)}/{fmtK(target)}</text>
</svg>

<style>
  .speedo {
    width: 100%;
    height: auto;
    display: block;
  }
  .pct {
    font-size: calc(15px * var(--size-num));
    font-weight: var(--w-bold);
    fill: var(--fg);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .pct.over {
    fill: var(--red);
  }
  .sub {
    font-size: calc(8.5px * var(--size-num));
    fill: var(--muted);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
</style>
