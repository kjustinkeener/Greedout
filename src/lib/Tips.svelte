<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";
  import type { Config } from "../types";
  import { applyTheme, type Theme } from "./theme";
  import Icon from "./Icon.svelte";
  import Speedo from "./Speedo.svelte";
  import { t, watchLocale } from "./i18n.svelte";

  let { onClose }: { onClose: () => void } = $props();

  // Agents read English, so the prompt is not localized. It spells out the one
  // detail that is easy to get wrong: context size comes from the last assistant
  // usage record, and a compact_boundary resets it.
  const PROMPT = `Set up a Claude Code hook that reminds me to run /compact when a session gets long.

1. Save this as ~/.claude/hooks/context-reminder.py:

import json, sys
TARGET = 200_000
data = json.load(sys.stdin)
ctx = 0
for line in open(data["transcript_path"], encoding="utf-8"):
    if '"compact_boundary"' in line:
        j = json.loads(line)
        if j.get("subtype") == "compact_boundary":
            ctx = (j.get("compactMetadata") or {}).get("postTokens") or 0
        continue
    if '"usage"' in line:
        j = json.loads(line)
        if j.get("type") == "assistant":
            u = (j.get("message") or {}).get("usage") or {}
            ctx = sum(u.get(k, 0) or 0 for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")) or ctx
if ctx >= TARGET:
    msg = f"Context is about {ctx // 1000}k tokens, past the {TARGET // 1000}k budget. Finish the current step, then at the next natural stopping point end your reply with: Please type /compact"
    print(json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": msg}}))

2. Register it in ~/.claude/settings.json as a UserPromptSubmit hook that runs: python ~/.claude/hooks/context-reminder.py (use the full path, and python3 if that is what this machine has).

3. Tell me when it is in place. It only reminds me; it must never compact or clear on its own.`;

  // The same dial as the main window, at three context sizes. 200k is the gauge target.
  const TARGET = 200_000;
  const fmtK = (n: number | null) => (n == null ? "--" : `${Math.round(n / 1000)}k`);
  const dials: { ctx: number; id: string; note: "tips.dialOk" | "tips.dialSoon" | "tips.dialNow" }[] = [
    { ctx: 45_000, id: "a", note: "tips.dialOk" },
    { ctx: 150_000, id: "b", note: "tips.dialSoon" },
    { ctx: 260_000, id: "c", note: "tips.dialNow" },
  ];

  let copied = $state(false);
  async function copyPrompt() {
    try {
      await navigator.clipboard.writeText(PROMPT);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch {
      // Clipboard blocked; the text stays selectable in the box.
    }
  }

  const unlistenLocale = watchLocale();
  onDestroy(() => void unlistenLocale.then((u) => u()));

  (async () => {
    try {
      applyTheme((await invoke<Config>("get_config")).theme);
    } catch {
      // Keep the default theme if config isn't reachable.
    }
  })();

  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => applyTheme(e.payload)).then((f) => (un = f));
    return () => un?.();
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") onClose();
  }}
/>

<div class="panel">
  <header class="bar" data-tauri-drag-region>
    <span class="title" data-tauri-drag-region>{t("tips.title")}</span>
    <button class="x" onclick={onClose} title={t("win.close")} aria-label={t("win.close")}>
      <Icon name="x" size={13} width={1.5} />
    </button>
  </header>

  <div class="body">
    <p class="lead">{t("tips.lead")}</p>

    <!-- 1. Why: context re-read every turn. Illustrative shape, not real data. -->
    <section>
      <h2>{t("tips.whyHead")}</h2>
      <p>{t("tips.whyBody")}</p>
      <svg class="chart" viewBox="0 0 320 150" role="img" aria-label={t("tips.chartAlt")}>
        <line class="axis" x1="10" y1="124" x2="310" y2="124" />
        <line class="axis" x1="10" y1="10" x2="10" y2="124" />
        <!-- Budget line -->
        <line class="target" x1="10" y1="64" x2="310" y2="64" />
        <text class="lbl" x="14" y="58">{t("tips.chartTarget")}</text>
        <!-- Extra cost: the gap between never compacting and compacting -->
        <path class="gap" d="M147 64 L300 14 L286 64 Z" />
        <polyline class="never" points="10,116 300,14" />
        <polyline class="comp" points="10,116 147,64 149,116 286,64 288,116 300,111" />
        <text class="lbl never-l" x="206" y="30" text-anchor="end">{t("tips.chartNever")}</text>
        <text class="lbl comp-l" x="212" y="140">{t("tips.chartCompact")}</text>
        <text class="lbl" x="310" y="138" text-anchor="end">{t("tips.chartX")}</text>
        <text class="lbl" x="14" y="20">{t("tips.chartY")}</text>
      </svg>
      <p class="cap">{t("tips.chartCap")}</p>
    </section>

    <!-- 2. The dial: where to act. A static mini of the gauge bar. -->
    <section>
      <h2>{t("tips.dialHead")}</h2>
      <p>{t("tips.dialBody")}</p>
      <div class="dials" aria-hidden="true">
        {#each dials as d (d.id)}
          <div class="dcol">
            <Speedo pct={d.ctx / TARGET} ctx={d.ctx} target={TARGET} {fmtK} />
            <span class="note n-{d.id}">{t(d.note)}</span>
          </div>
        {/each}
      </div>
    </section>

    <!-- 3. /compact: before and after -->
    <section>
      <h2>{t("tips.compactHead")}</h2>
      <p>{t("tips.compactBody")}</p>
      <div class="ba" aria-hidden="true">
        <div class="col">
          <div class="stack before">
            <i style="height:34%"></i><i style="height:30%"></i><i style="height:36%"></i>
          </div>
          <span>{t("tips.before")}</span>
        </div>
        <div class="arrow">/compact</div>
        <div class="col">
          <div class="stack after"><i style="height:100%"></i></div>
          <span>{t("tips.after")}</span>
        </div>
      </div>
    </section>

    <section>
      <h2>{t("tips.clearHead")}</h2>
      <p>{t("tips.clearBody")}</p>
    </section>

    <!-- 4. The hook prompt -->
    <section>
      <h2>{t("tips.hookHead")}</h2>
      <p>{t("tips.hookBody")}</p>
      <pre class="prompt">{PROMPT}</pre>
      <button class="btn primary" onclick={copyPrompt}>
        <Icon name={copied ? "check" : "copy"} size={12} />
        {copied ? t("tips.copied") : t("tips.copy")}
      </button>
    </section>
  </div>
</div>

<style>
  :global(html),
  :global(body) {
    background: var(--bg);
  }
  .panel {
    min-height: 100vh;
    box-sizing: border-box;
    display: flex;
    flex-direction: column;
    color: var(--fg);
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 8px 8px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .title {
    font-weight: var(--w-semibold);
    font-size: calc(13px * var(--size-ui));
  }
  .x {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
    color: var(--muted);
    background: none;
    border: none;
    border-radius: 4px;
    cursor: pointer;
  }
  .x:hover {
    color: var(--fg);
    background: var(--hover);
  }
  .body {
    padding: 12px 16px 18px;
    overflow-y: auto;
    font-size: calc(12px * var(--size-ui));
    line-height: 1.5;
  }
  .lead {
    margin: 0 0 6px;
    font-size: calc(13px * var(--size-ui));
  }
  section {
    margin-top: 18px;
  }
  h2 {
    margin: 0 0 4px;
    font-size: calc(12.5px * var(--size-ui));
    font-weight: var(--w-semibold);
    color: var(--g1);
  }
  p {
    margin: 0 0 8px;
  }
  .cap {
    color: var(--muted);
    font-size: calc(10.5px * var(--size-ui));
  }

  /* Chart */
  .chart {
    width: 100%;
    height: auto;
    display: block;
  }
  .axis {
    stroke: var(--edge);
    stroke-width: 1;
  }
  .target {
    stroke: var(--muted);
    stroke-width: 1;
    stroke-dasharray: 3 3;
  }
  .never {
    fill: none;
    stroke: #e5484d;
    stroke-width: 2;
  }
  .comp {
    fill: none;
    stroke: var(--green);
    stroke-width: 2;
    stroke-linejoin: round;
  }
  .gap {
    fill: #e5484d;
    opacity: 0.18;
  }
  .lbl {
    fill: var(--muted);
    font-size: 8.5px;
  }
  .never-l {
    fill: #e5484d;
  }
  .comp-l {
    fill: var(--green);
  }

  /* Dials */
  .dials {
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }
  .dcol {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    gap: 4px;
  }
  .dcol :global(svg) {
    width: 100%;
    max-width: 130px;
    height: auto;
  }
  .note {
    font-size: calc(10.5px * var(--size-ui));
    color: var(--muted);
  }
  .n-c {
    color: var(--red);
    font-weight: var(--w-semibold);
  }

  /* Before / after */
  .ba {
    display: flex;
    align-items: flex-end;
    justify-content: center;
    gap: 18px;
    margin: 6px 0 2px;
  }
  .col {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    color: var(--muted);
    font-size: calc(10.5px * var(--size-ui));
  }
  .stack {
    width: 54px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 1px solid var(--edge);
    border-radius: 4px;
    padding: 2px;
    background: var(--track);
  }
  .stack i {
    display: block;
    border-radius: 2px;
    background: var(--g1);
    opacity: 0.8;
  }
  .stack.before {
    height: 90px;
  }
  .stack.after {
    height: 14px;
  }
  .stack.after i {
    background: var(--green);
    opacity: 1;
  }
  .arrow {
    align-self: center;
    font-family: ui-monospace, monospace;
    color: var(--fg);
    font-size: calc(11px * var(--size-ui));
    padding-bottom: 14px;
  }
  .arrow::after {
    content: " \2192";
  }

  /* Prompt */
  .prompt {
    margin: 0 0 8px;
    padding: 8px 10px;
    max-height: 150px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    font-family: ui-monospace, monospace;
    font-size: 10.5px;
    line-height: 1.4;
    user-select: text;
  }
  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 3px 14px;
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
  }
  .btn.primary {
    background: var(--green);
    border-color: var(--green);
    color: #06210c;
    font-weight: var(--w-semibold);
  }
</style>
