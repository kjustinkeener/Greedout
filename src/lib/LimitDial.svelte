<script lang="ts">
  import { gaugeColor, themeTick } from "./theme";
  // Plan-limit dial: two concentric 270-degree bars sharing one sweep. The outer
  // bar is how far through the window we are; the inner bar is how much of the
  // limit is used. Where the inner bar ends short of the outer, usage is under
  // pace; past it, usage is running ahead of the clock.
  let {
    label,
    pct,
    elapsed,
    hint,
    onHover,
  }: {
    label: string;
    pct: number | null;
    elapsed: number | null;
    /** Shown in place of the percentage until the dial is calibrated. */
    hint: string;
    /** Called with the usage and window shares while the pointer is over the dial (usage is null before calibration), null after. */
    onHover: (h: { usage: number | null; window: number | null; color: string } | null) => void;
  } = $props();

  const CX = 60,
    CY = 60,
    R_TIME = 46,
    R_USE = 33,
    W = 9;
  const START = 135,
    SWEEP = 270;

  const polar = (a: number, r: number) => {
    const rad = (a * Math.PI) / 180;
    return [CX + r * Math.cos(rad), CY + r * Math.sin(rad)];
  };
  // Arc from fraction f0 to f1 of the sweep at radius r.
  const arc = (r: number, f0: number, f1: number) => {
    const [x1, y1] = polar(START + SWEEP * f0, r);
    const [x2, y2] = polar(START + SWEEP * f1, r);
    const large = SWEEP * (f1 - f0) > 180 ? 1 : 0;
    return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${r} ${r} 0 ${large} 1 ${x2.toFixed(2)} ${y2.toFixed(2)}`;
  };

  const clamp = (n: number | null) => (n == null ? 0 : Math.max(0, Math.min(1, n)));
  let use = $derived(clamp(pct));
  let time = $derived(clamp(elapsed));
  // The usage bar is coloured by pace: usage over window elapsed, with the hottest
  // colour reached at 125% of pace. Without a window start it falls back to usage.
  const HOT_PACE = 1.25;
  let useColor = $derived.by(() => {
    void $themeTick;
    if (pct == null) return "var(--muted)";
    const pace = elapsed != null && elapsed > 0 ? pct / elapsed : null;
    return gaugeColor(clamp(pace == null ? pct : pace / HOT_PACE));
  });
  // The window bar uses the main gauge's colour sweep: short segments coloured by
  // their position along the dial, drawn only as far as the window has elapsed.
  const SEG = 60;
  let timeSegs = $derived.by(() => {
    void $themeTick;
    const n = Math.ceil(time * SEG);
    return Array.from({ length: n }, (_, i) => {
      const f0 = i / SEG,
        f1 = Math.min((i + 1) / SEG, time);
      return { d: arc(R_TIME, f0, f1), color: gaugeColor((f0 + f1) / 2) };
    });
  });
  let pctText =$derived(pct == null ? hint : Math.round(pct * 100) + "%");
  let over = $derived(pct != null && pct >= 1);
</script>

<svg
  viewBox="0 0 120 100"
  class="dial"
  role="img"
  aria-label={label}
  onmouseenter={() => onHover({ usage: pct, window: elapsed, color: useColor })}
  onmouseleave={() => onHover(null)}
>
  <path d={arc(R_TIME, 0, 1)} fill="none" stroke="var(--track)" stroke-width={W} stroke-linecap="round" />
  <path d={arc(R_USE, 0, 1)} fill="none" stroke="var(--track)" stroke-width={W} stroke-linecap="round" />
  {#if elapsed != null && time > 0}
    {#each timeSegs as s}
      <path d={s.d} fill="none" stroke={s.color} stroke-width={W} stroke-linecap="round" />
    {/each}
  {/if}
  {#if use > 0}
    <path d={arc(R_USE, 0, use)} fill="none" stroke={useColor} stroke-width={W} stroke-linecap="round" />
  {/if}
  <text x={CX} y={pct == null ? 74 : 66} text-anchor="middle" class="pct" class:over class:hint={pct == null}>{pctText}</text>
  <text x={CX} y="94" text-anchor="middle" class="sub">{label}</text>
</svg>

<style>
  .dial {
    width: 100%;
    height: auto;
    display: block;
  }
  .pct {
    font-size: calc(23px * var(--size-num));
    font-weight: var(--w-bold);
    fill: var(--fg);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .pct.hint {
    font-size: calc(35px * var(--size-num));
    fill: var(--muted);
  }
  .pct.over {
    fill: var(--red);
  }
  .sub {
    font-size: calc(19px * var(--size-num));
    fill: var(--fg);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
</style>
