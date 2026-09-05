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

  // Per-core CPU color: green (idle) → amber → red (saturated), matching the
  // Hue sweeps 120→0 as usage goes 0→100.
  const coreColor = (u: number) => `hsl(${Math.round(120 - 1.2 * Math.min(u, 100))} 65% 48%)`;

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
            style="height:{Math.max(2, Math.min(u, 100))}%; background:{coreColor(u)}"
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
    transition:
      height 0.4s ease,
      background 0.4s ease;
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
