<script lang="ts">
  // Shared scan-progress panel for the shared-cache build, used by both the Daily
  // Spend window and the Context Explorer so the two render identically. It is
  // pure PRESENTATION: all scan state (phases, log, running) lives in the shared
  // scanControl store; this component only draws it as three stacked, labeled
  // phase bars (index -> enrich -> write) plus a rolling log of the files being
  // read. Each host window drives the DATA side-effects via scanControl.onScanDone.
  import { scanState, cancelScan, type PhaseState } from "./scanControl";

  // Optional: when given, Cancel calls this; otherwise it cancels the scan itself.
  let { oncancel }: { oncancel?: () => void } = $props();

  // A clock that ticks only while a scan runs, so the active phase's ms updates
  // smoothly. The store stays pure data; the ticking lives here in the view.
  let now = $state(0);
  $effect(() => {
    if (!$scanState.running) return;
    now = performance.now();
    const id = setInterval(() => (now = performance.now()), 150);
    return () => clearInterval(id);
  });

  // The three phases in emission order, with their user-facing label.
  const phases = $derived([
    { label: "Indexing", st: $scanState.index },
    { label: "Reading transcripts", st: $scanState.enrich },
    { label: "Saving", st: $scanState.write },
  ]);

  // Remaining width for the mask that obscures the unfilled part of the static
  // gradient track. 100% = empty; a phase not yet reached (total 0) reads empty.
  function remaining(st: PhaseState): string {
    return st.total ? `${100 - (st.done / st.total) * 100}%` : "100%";
  }
  function count(st: PhaseState): string {
    return st.total ? `${st.done}/${st.total}` : "-";
  }
  function elapsed(st: PhaseState): string {
    if (!st.t0) return "";
    return `${Math.round((st.end || now) - st.t0)}ms`;
  }

  function cancel() {
    if (oncancel) oncancel();
    else cancelScan();
  }
</script>

{#if $scanState.running}
  <div class="scanbar">
    {#each phases as ph (ph.label)}
      <div class="prow" class:inactive={ph.st.total === 0}>
        <span class="plabel">{ph.label}</span>
        <div class="ptrack"><div class="pmask" style:width={remaining(ph.st)}></div></div>
        <span class="pcount">{count(ph.st)}</span>
        <span class="pms">{elapsed(ph.st)}</span>
      </div>
    {/each}
    <div class="pfoot">
      <button class="pcancel" onclick={cancel}>Cancel</button>
    </div>
    {#if $scanState.log.length}
      <ul class="plog">
        {#each $scanState.log as line (line)}
          <li>{line}</li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .scanbar {
    flex: none;
    padding: 8px 14px 6px;
    border-bottom: 1px solid var(--panel);
  }
  .prow {
    display: grid;
    grid-template-columns: 118px 1fr auto auto;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
  }
  /* A phase not yet reached (total 0) is dimmed until it starts. */
  .prow.inactive {
    opacity: 0.4;
  }
  .plabel {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
  }
  .ptrack {
    position: relative;
    height: 6px;
    border-radius: 3px;
    overflow: hidden;
    /* Full theme gauge gradient, painted statically across the whole track. */
    background: linear-gradient(to right, var(--g0), var(--g1), var(--g2));
  }
  /* Obscures the unfilled (right) part; shrinking it reveals the static gradient.
     Must be OPAQUE -- var(--panel)/--hover are semi-transparent in some themes and
     would let the gradient show through. --bg is the solid window ground. */
  .pmask {
    position: absolute;
    top: 0;
    right: 0;
    height: 100%;
    background: var(--bg);
    transition: width 0.2s;
  }
  .pcount {
    font-size: 11px;
    color: var(--muted);
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  /* Live/frozen wall-clock for the phase, right of the count. */
  .pms {
    font-size: 11px;
    color: var(--muted);
    opacity: 0.75;
    text-align: right;
    min-width: 52px;
    font-variant-numeric: tabular-nums;
  }
  .pfoot {
    display: flex;
    justify-content: flex-end;
    margin-top: 4px;
  }
  .pcancel {
    background: none;
    border: 1px solid var(--panel);
    color: var(--muted);
    border-radius: 5px;
    padding: 2px 8px;
    font: inherit;
    font-size: 11px;
    cursor: pointer;
  }
  .pcancel:hover {
    color: var(--fg);
    border-color: var(--muted);
  }
  .plog {
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
    font-size: 9px;
    color: var(--muted);
    opacity: 0.7;
    font-variant-numeric: tabular-nums;
    max-height: 66px;
    overflow: hidden;
  }
  .plog li {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    line-height: 1.35;
  }
</style>
