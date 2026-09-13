<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import type { SysStats } from "../types";
  import { t } from "./i18n.svelte";

  let stats = $state<SysStats | null>(null);

  onMount(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      unlisten = await listen<SysStats>("sysstats", (e) => (stats = e.payload));
    })();
    return () => unlisten?.();
  });

  // Per-core CPU height (%) plus the reveal-size that maps the themed gauge
  // gradient onto the full track so the fill clips the bottom %-subsection of it
  // (idle = low-gradient end, saturated = the whole sweep). Same reveal trick as
  // the memory bar, rotated vertical, so the bars follow the theme with no JS color.
  const coreH = (u: number) => Math.max(2, Math.min(u, 100));

  // Memory bar reveals more of the fixed themed gauge gradient as usage climbs
  // (same trick as the Row context fill): the gradient always spans the whole
  // track, and the fill width clips it. Guard the divisor so ~0% doesn't blow up.
  const memPct = $derived(stats ? Math.min(1, Math.max(0, stats.memPct)) * 100 : 0);
  const memBgSize = $derived(10000 / Math.max(0.5, memPct));

  function gib(bytes: number): string {
    return String(Math.round(bytes / 1024 ** 3));
  }
</script>

<div class="statusbar" data-tauri-drag-region>
  {#if stats}
    <span class="tag">{t("status.cpu")}</span>
    <div class="cpustrip" title={t("status.cpuTip")}>
      {#each stats.cpus as u, i (i)}
        <span class="core">
          <span
            class="corefill"
            style="height:{coreH(u)}%; background-size:100% {10000 / coreH(u)}%"
          ></span>
        </span>
      {/each}
    </div>
    <span class="tag">{t("status.mem")}</span>
    <div
      class="membar"
      title={t("status.memTip", { used: gib(stats.memUsed), total: gib(stats.memTotal) })}
      data-tauri-drag-region
    >
      <div class="memfill" style:width="{memPct}%" style:background-size="{memBgSize}% 100%"></div>
    </div>
    <span class="memlabel">{gib(stats.memUsed)}/{gib(stats.memTotal)} GB</span>
  {/if}
</div>

<style>
  .statusbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 8px;
    background: var(--panel);
    border-top: 1px solid var(--edge);
    flex: 0 0 auto;
    /* Clip into the window's rounded bottom corners (see App.svelte main). */
    border-bottom-left-radius: 8px;
    border-bottom-right-radius: 8px;
  }
  .cpustrip {
    display: flex;
    align-items: flex-end;
    gap: 1px;
    height: 15px;
    flex: 0 0 auto;
  }
  .core {
    width: 4px;
    height: 100%;
    background: var(--track);
    border-radius: 1px;
    display: flex;
    align-items: flex-end;
    overflow: hidden;
  }
  .corefill {
    width: 100%;
    display: block;
    border-radius: 1px;
    /* Themed gauge gradient spanning the full track, revealed bottom-up by the
       fill height (background-size clips it), so a bar shows the bottom slice of
       the same gradient the speedo and memory bar use. */
    background-color: var(--g2);
    background-image: linear-gradient(0deg, var(--g0) 0%, var(--g1) 52%, var(--g2) 100%);
    background-repeat: no-repeat;
    background-position: left bottom;
    transition:
      height 0.4s ease,
      background-size 0.4s ease;
  }
  .membar {
    position: relative;
    flex: 1 1 auto;
    height: 12px;
    background: var(--track);
    border-radius: 2px;
    overflow: hidden;
  }
  .memfill {
    height: 100%;
    border-radius: 2px;
    opacity: 0.85;
    background-color: var(--g2);
    background-image: linear-gradient(90deg, var(--g0) 0%, var(--g1) 52%, var(--g2) 100%);
    background-repeat: no-repeat;
    background-position: left center;
    transition: width 0.4s ease;
  }
  .memlabel {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: calc(11px * var(--size-num));
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
  .tag {
    flex: 0 0 auto;
    color: var(--muted);
    opacity: 0.6;
    font-size: calc(9px * var(--size-ui));
    font-weight: var(--w-medium);
    letter-spacing: 0.06em;
  }
</style>
