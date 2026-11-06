<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { onDestroy } from "svelte";
  import type { Config } from "../types";
  import { applyTheme, type Theme } from "./theme";
  import Brand from "./Brand.svelte";
  import Icon from "./Icon.svelte";
  import Speedo from "./Speedo.svelte";
  import { t, watchLocale } from "./i18n.svelte";

  let { onClose }: { onClose: () => void } = $props();

  // Gauge budget from Settings (target_tokens); 200k until the config loads.
  let TARGET = $state(200_000);

  // Agents read English, so the prompt is not localized. It spells out the one
  // detail that is easy to get wrong: context size comes from the last assistant
  // usage record, and a compact_boundary resets it.
  type Part = { code: boolean; text: string };
  const prose = (text: string): Part => ({ code: false, text });
  const code = (text: string): Part => ({ code: true, text });

  const promptParts = $derived.by((): Part[] => {
    const PRINT =
      '    print(json.dumps({"hookSpecificOutput": {"hookEventName": "UserPromptSubmit", "additionalContext": msg}, "suppressOutput": True}))';
    const body = [
      `TARGET = ${String(TARGET).replace(/\B(?=(\d{3})+$)/g, "_")}`,
      "data = json.load(sys.stdin)",
      "ctx = 0",
      "fresh = False",
      'for line in open(data["transcript_path"], encoding="utf-8"):',
      "    boundary = '\"compact_boundary\"' in line",
      "    if not boundary and '\"usage\"' not in line:",
      "        continue",
      "    try:",
      "        j = json.loads(line)",
      "    except Exception:",
      "        continue",
      '    if boundary and j.get("subtype") == "compact_boundary":',
      '        ctx = (j.get("compactMetadata") or {}).get("postTokens") or 0',
      "        fresh = ctx > 0",
      "        continue",
      '    if j.get("type") == "assistant":',
      "        fresh = False",
      '        u = (j.get("message") or {}).get("usage") or {}',
      '        ctx = sum(u.get(k, 0) or 0 for k in ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")) or ctx',
      "if fresh:",
      '    msg = f"The user just compacted, so context is now about {ctx // 1000}k tokens. Do not end replies with Session size or Please type /compact until told again."',
      PRINT,
      "elif ctx >= TARGET:",
      '    msg = f"Context is about {ctx // 1000}k tokens, past the {TARGET // 1000}k budget. Finish the current step properly (no rushed or skipped work, it is not a hard limit), do not open new threads, and at the next natural stopping point end your reply with these two lines: Session size at {ctx // 1000}k, then Please type /compact. Repeat at each later stopping point until the user compacts."',
      PRINT,
    ];
    // Any failure (transcript not written yet, a half-written line, a missing field) must
    // end quietly: a crash would show the user a hook error on that turn.
    const reminder = [
      "import json, sys",
      "try:",
      ...body.map((l) => "    " + l),
      "except Exception:",
      "    pass",
    ].join("\n");

    const parts: Part[] = [
      prose("Set up a Claude Code hook that reminds me to run /compact when a session gets long."),
      prose(""),
      prose("1. Save this as ~/.claude/hooks/context-reminder.py:"),
      prose(""),
      code(reminder),
      prose(""),
      prose(
        "2. Register it in ~/.claude/settings.json as a UserPromptSubmit hook that runs: python ~/.claude/hooks/context-reminder.py (use the full path, and python3 if that is what this machine has).",
      ),
      prose(""),
    ];
    parts.push(prose("3. Tell me when it is in place. It only reminds me; it must never compact or clear on its own."));
    return parts;
  });
  const promptText = $derived(promptParts.map((p) => p.text).join("\n"));

  // The same dial as the main window, at three context sizes (fractions of the gauge target).
  const fmtK = (n: number | null) => (n == null ? "--" : `${Math.round(n / 1000)}k`);
  const dials = $derived<{ ctx: number; id: string; note: "tips.dialOk" | "tips.dialSoon" | "tips.dialNow" }[]>([
    { ctx: Math.round(TARGET * 0.225), id: "a", note: "tips.dialOk" },
    { ctx: Math.round(TARGET * 0.75), id: "b", note: "tips.dialSoon" },
    { ctx: Math.round(TARGET * 1.3), id: "c", note: "tips.dialNow" },
  ]);

  // What fills a long session. Heights are illustrative; "noise" is what a summary drops.
  const segs: { h: number; noise: boolean; label: "tips.segTools" | "tips.segFiles" | "tips.segOutput" | "tips.segDead" | "tips.segReplies" | "tips.segYou" }[] = [
    { h: 12, noise: false, label: "tips.segYou" },
    { h: 14, noise: false, label: "tips.segReplies" },
    { h: 27, noise: true, label: "tips.segTools" },
    { h: 20, noise: true, label: "tips.segFiles" },
    { h: 16, noise: true, label: "tips.segOutput" },
    { h: 11, noise: true, label: "tips.segDead" },
  ];

  // Highlighting for the Python block inside the prompt (everything between step 1 and
  // step 2); the rest is prose. Small hand-written tokenizer: no library for ~25 lines.
  type Tok = { k: string; x: string };
  const TOKEN =
    /(f?"(?:[^"\\]|\\.)*"|f?'(?:[^'\\]|\\.)*')|(#.*)|\b(import|from|for|in|if|elif|else|and|or|not|continue|return|def|try|except|True|False|None)\b|\b(\d[\d_]*)\b|\b(open|sum|print)\b/g;
  function tokenize(line: string): Tok[] {
    const out: Tok[] = [];
    let last = 0;
    for (const m of line.matchAll(TOKEN)) {
      if (m.index > last) out.push({ k: "", x: line.slice(last, m.index) });
      out.push({ k: m[1] ? "str" : m[2] ? "com" : m[3] ? "kw" : m[4] ? "num" : "fn", x: m[0] });
      last = m.index + m[0].length;
    }
    if (last < line.length) out.push({ k: "", x: line.slice(last) });
    return out;
  }
  const promptLines = $derived(
    promptParts.flatMap((p) => p.text.split("\n").map((line) => (p.code ? tokenize(line) : [{ k: "", x: line }]))),
  );

  let copied = $state(false);
  async function copyPrompt() {
    try {
      await navigator.clipboard.writeText(promptText);
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
      const cfg = await invoke<Config>("get_config");
      applyTheme(cfg.theme);
      if (cfg.target_tokens > 0) TARGET = cfg.target_tokens;
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
    <Brand size={16} font={15} />
    <span class="title" data-tauri-drag-region>{t("tips.title")}</span>
    <span class="spacer" data-tauri-drag-region></span>
    <button class="x" onclick={onClose} title={t("win.close")} aria-label={t("win.close")}>
      <Icon name="x" size={13} width={1.5} />
    </button>
  </header>

  {#snippet rich(text: string)}{#each text.split(/(\/compact|\/clear)/) as part}{#if part === "/compact" || part === "/clear"}<span class="cmd">{part}</span>{:else}{part}{/if}{/each}{/snippet}

  <div class="body">
    <p class="lead">{@render rich(t("tips.lead"))}</p>

    <!-- 1. Why: context re-read every turn. Illustrative shape, not real data. -->
    <section>
      <h2>{t("tips.whyHead")}</h2>
      <p>{t("tips.whyBody")}</p>
      <svg class="chart" viewBox="0 0 640 112" role="img" aria-label={t("tips.chartAlt")}>
        <line class="axis" x1="10" y1="96" x2="630" y2="96" />
        <line class="axis" x1="10" y1="8" x2="10" y2="96" />
        <!-- Budget line -->
        <line class="target" x1="10" y1="50" x2="630" y2="50" />
        <text class="lbl" x="16" y="45">{t("tips.chartTarget")}</text>
        <!-- Extra cost: the gap between never compacting and compacting -->
        <path class="gap" d="M318 49.6 L620 10 L620 51 L318 90 Z" />
        <polyline class="never" points="10,90 620,10" />
        <polyline class="comp" points="10,90 315,50 318,90 620,51" />
        <text class="lbl never-l" x="400" y="24" text-anchor="middle">{t("tips.chartNever")}</text>
        <text class="lbl comp-l" x="470" y="108" text-anchor="middle">{t("tips.chartCompact")}</text>
        <text class="lbl" x="630" y="108" text-anchor="end">{t("tips.chartX")}</text>
        <text class="lbl" x="16" y="18">{t("tips.chartY")}</text>
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
            <span class="note n-{d.id}">{@render rich(t(d.note))}</span>
          </div>
        {/each}
      </div>
    </section>

    <!-- 3. /compact: before and after -->
    <section>
      <h2>{@render rich(t("tips.compactHead"))}</h2>
      <p>{@render rich(t("tips.compactBody"))}</p>
      <div class="ba" aria-hidden="true">
        <div class="col">
          <div class="win before">
            {#each segs as g (g.label)}
              <div class="seg" class:noise={g.noise} style="flex:{g.h}">{t(g.label)}</div>
            {/each}
          </div>
          <span>{t("tips.before")}</span>
        </div>
        <div class="arrow"><span class="cmd">/compact</span></div>
        <div class="col">
          <div class="win after">
            <div class="seg keep" style="flex:12">{t("tips.segSummary")}</div>
            <div class="free" style="flex:88">{t("tips.segFree")}</div>
          </div>
          <span>{t("tips.after")}</span>
        </div>
      </div>
      <p class="cap"><span class="sw noise"></span>{t("tips.legendNoise")} <span class="sw"></span>{t("tips.legendKeep")}</p>
    </section>

    <section>
      <h2>{@render rich(t("tips.clearHead"))}</h2>
      <p>{@render rich(t("tips.clearBody"))}</p>
    </section>

    <!-- 4. The hook prompt -->
    <section>
      <h2>{t("tips.hookHead")}</h2>
      <p>{t("tips.hookBody")}</p>
      <div class="pbox">
        <pre class="prompt">{#each promptLines as toks, i}{#each toks as tk}{#if tk.k}<span class={tk.k}>{tk.x}</span>{:else}{tk.x}{/if}{/each}{i < promptLines.length - 1 ? "\n" : ""}{/each}</pre>
        <button class="btn primary copy" onclick={copyPrompt}>
          <Icon name={copied ? "check" : "copy"} size={12} />
          {copied ? t("tips.copied") : t("tips.copy")}
        </button>
      </div>
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
    gap: 8px;
    padding: 8px 8px 8px 16px;
    border-bottom: 1px solid var(--edge);
  }
  .title {
    color: var(--fg);
    font-weight: var(--w-semibold);
    font-size: calc(15px * var(--size-ui));
  }
  .spacer {
    flex: 1;
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
    margin-top: 32px;
  }
  h2 {
    margin: 0 0 4px;
    font-size: calc(17px * var(--size-ui));
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
    font-size: 11px;
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
    align-items: stretch;
    gap: 12px;
    margin: 6px 0 2px;
  }
  .col {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    align-items: stretch;
    gap: 4px;
    color: var(--muted);
    font-size: calc(12.5px * var(--size-ui));
    text-align: center;
  }
  .win {
    height: 210px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    border: 1px solid var(--edge);
    border-radius: 6px;
    padding: 3px;
    background: var(--track);
  }
  .seg {
    display: flex;
    align-items: center;
    padding: 0 8px;
    min-height: 0;
    border-radius: 3px;
    background: var(--g1);
    color: #1a1a00;
    font-size: calc(12.5px * var(--size-ui));
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .seg.noise {
    background: repeating-linear-gradient(135deg, #7a3b3e, #7a3b3e 6px, #6a3235 6px, #6a3235 12px);
    color: #f3d3d4;
  }
  .seg.keep {
    background: var(--green);
    color: #06210c;
    font-weight: var(--w-semibold);
  }
  .free {
    display: flex;
    align-items: center;
    justify-content: center;
    color: var(--muted);
    border: 1px dashed var(--edge);
    border-radius: 3px;
    min-height: 0;
  }
  .cmd {
    color: var(--g0);
    font-weight: var(--w-semibold);
  }
  .arrow {
    align-self: center;
    font-family: ui-monospace, monospace;
    color: var(--fg);
    font-size: calc(12.5px * var(--size-ui));
    white-space: nowrap;
  }
  .arrow::after {
    content: " \2192";
  }
  .sw {
    display: inline-block;
    width: 10px;
    height: 10px;
    margin: 0 4px 0 8px;
    vertical-align: -1px;
    border-radius: 2px;
    background: var(--g1);
  }
  .sw.noise {
    margin-left: 0;
    background: #7a3b3e;
  }

  /* Prompt */
  .pbox {
    position: relative;
  }
  .copy {
    position: absolute;
    top: 6px;
    right: 16px;
    padding: 2px 10px;
    font-size: calc(11px * var(--size-ui));
  }
  /* Reserves the corner for the Copy button so the first lines wrap around it. */
  /* The global thumb color is this box's own background, so it would be invisible here. */
  .prompt::-webkit-scrollbar-thumb {
    background: var(--muted);
  }
  .prompt .kw {
    color: var(--g0);
  }
  .prompt .str {
    color: var(--green);
  }
  .prompt .num {
    color: #ff9f43;
  }
  .prompt .fn {
    color: var(--g1);
  }
  .prompt .com {
    color: var(--muted);
    font-style: italic;
  }
  .prompt::before {
    content: "";
    float: right;
    width: 96px;
    height: 24px;
  }
  .prompt {
    margin: 0;
    padding: 8px 10px;
    max-height: 338px;
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
