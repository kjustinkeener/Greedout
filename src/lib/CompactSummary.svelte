<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import type { CompactSummary } from "../types";
  import BrandIcon from "./BrandIcon.svelte";
  import Icon from "./Icon.svelte";
  import { t } from "./i18n.svelte";

  let {
    id,
    heading = "",
    onClose,
  }: {
    id: string;
    heading?: string;
    onClose: () => void;
  } = $props();

  let data = $state<CompactSummary | null>(null);
  let loading = $state(true);
  let failed = $state(false);

  const fmtK = (n: number) =>
    n >= 1_000_000 ? `${(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M` : `${Math.round(n / 1000)}k`;
  const fmtWhen = (ms: number) => {
    try {
      return new Date(ms).toLocaleString();
    } catch {
      return "";
    }
  };

  $effect(() => {
    void id;
    loading = true;
    failed = false;
    data = null;
    invoke<CompactSummary | null>("get_compact_summary", { id })
      .then((r) => {
        data = r;
        loading = false;
      })
      .catch(() => {
        failed = true;
        loading = false;
      });
  });
</script>

<div class="wrap">
  <header data-tauri-drag-region>
    <BrandIcon size={16} />
    <span class="htitle">{t("compact.title")}</span>
    {#if heading}<span class="hsub" title={heading}>{heading}</span>{/if}
    <button class="x" aria-label={t("common.close")} title={t("common.close")} onclick={onClose}>
      <Icon name="x" size={13} />
    </button>
  </header>

  {#if loading}
    <div class="msg">{t("compact.loading")}</div>
  {:else if failed || !data}
    <div class="msg">{t("compact.none")}</div>
  {:else}
    <div class="meta">
      <span class="szpair"
        >{fmtK(data.pre)}<span class="arr">&rarr;</span>{fmtK(data.post)}<span class="unit"
          >{t("compact.tokens")}</span
        ></span
      >
      <span class="when">{fmtWhen(data.tsMs)}</span>
    </div>
    <div class="body">{data.text}</div>
  {/if}
</div>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg);
    color: var(--fg);
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-bottom: 1px solid var(--edge);
    background: var(--panel);
    flex: 0 0 auto;
  }
  .htitle {
    font-weight: var(--w-semibold);
    white-space: nowrap;
  }
  .hsub {
    flex: 1 1 auto;
    min-width: 0;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    flex: 0 0 auto;
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: none;
    border: none;
    color: var(--muted);
    cursor: pointer;
    padding: 2px;
    border-radius: 3px;
  }
  .x:hover {
    color: var(--fg);
    background: rgba(127, 127, 127, 0.15);
  }
  .meta {
    display: flex;
    align-items: baseline;
    gap: 10px;
    padding: 6px 12px;
    border-bottom: 1px solid var(--edge);
    color: var(--muted);
    flex: 0 0 auto;
  }
  .szpair {
    color: var(--fg);
    font-weight: var(--w-semibold);
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .szpair .arr {
    margin: 0 3px;
    color: var(--muted);
  }
  .szpair .unit {
    margin-left: 4px;
    color: var(--muted);
    font-weight: var(--w-regular);
  }
  .when {
    margin-left: auto;
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .msg {
    padding: 24px 16px;
    color: var(--muted);
    text-align: center;
  }
  /* The summary is prose the harness wrote; preserve its line breaks but wrap
     long lines so nothing scrolls horizontally. */
  .body {
    flex: 1 1 auto;
    overflow-y: auto;
    padding: 12px 14px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    line-height: 1.5;
    font-family: var(--font-mono);
    font-size-adjust: var(--font-mono-adj);
    font-size: calc(12px * var(--size-mono, 1));
  }
</style>
