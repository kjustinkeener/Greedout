// Shared control for the shared-cache scan: ONE implementation of the scan
// trigger, its lifecycle, and its live progress state, so the Daily Spend and
// Context Explorer windows stop diverging.
//
// The backend scan itself is already the single shared thing (a guarded
// `run_scan`); what was duplicated was the FRONTEND side -- each window wired its
// own `listen("browse-progress")`, its own phase bookkeeping, and its own
// start/cancel invokes. This module owns all of that once. Note the hard limit:
// DS and CE are SEPARATE webviews (separate JS contexts), so this module loads
// once PER window and cannot literally share runtime state across them. "Shared
// control" therefore means one code module + one listener per window (not
// duplicated inline); both windows still receive the same backend event stream,
// so a window that didn't start a scan still renders it and still gets onDone.
import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { BrowseProgress } from "../types";

// One scan phase's live state. t0/end are wall-clock (performance.now()) measured
// on this side: t0 = first event for the phase, end = when a later phase (or the
// scan's end) started. Both 0 until they happen; elapsed is (end || now) - t0, so
// the active phase counts up live and finished ones freeze.
export interface PhaseState {
  done: number;
  total: number;
  t0: number;
  end: number;
}
// One log line. `id` is a stable, monotonic key so the {#each} keys by identity:
// a newest-first prepend then inserts a real node at the top (existing nodes keep
// their text and shift down) rather than reusing nodes and rewriting text in
// place, which would make the log crawl through a fixed viewport. `t` is its
// position along the OVERALL 3-bar gradient sweep (0..1) at the moment it was
// emitted; the view colors the line by mixing the theme gauge stops
// (--g0/--g1/--g2) at `t`, so a line's color matches the bar it came from.
export interface LogLine {
  id: number;
  text: string;
  t: number;
}
let logSeq = 0;
export interface ScanState {
  running: boolean;
  index: PhaseState;
  enrich: PhaseState;
  write: PhaseState;
  log: LogLine[];
}

const zeroPhase = (): PhaseState => ({ done: 0, total: 0, t0: 0, end: 0 });
const initial = (): ScanState => ({
  running: false,
  index: zeroPhase(),
  enrich: zeroPhase(),
  write: zeroPhase(),
  log: [],
});

// The single source of truth for scan progress in this window. ScanProgress
// renders from it; host windows can subscribe for side effects (e.g. throttled
// tile refresh during enrichment).
export const scanState = writable<ScanState>(initial());

// Done/canceled callbacks: host windows register their data-refresh here instead
// of each keeping a second "browse-progress" listener.
const doneCbs = new Set<(canceled: boolean) => void>();
export function onScanDone(cb: (canceled: boolean) => void): () => void {
  doneCbs.add(cb);
  return () => void doneCbs.delete(cb);
}

// Freeze a started-but-unfinished earlier phase when the next one (or the end)
// fires, so its elapsed stops ticking.
function finish(st: PhaseState, t: number): PhaseState {
  return st.t0 && !st.end ? { ...st, end: t } : st;
}

// One listener per window, registered at module load. It lives for the window's
// lifetime, so there is no unlisten to manage.
void listen<BrowseProgress>("browse-progress", (e) => {
  const p = e.payload;
  const t = performance.now();
  scanState.update((s) => {
    let { running, index, enrich, write, log } = s;
    // A scan is underway but this window didn't start it (it mounted mid-scan, or
    // the other window / a background enrich triggered it): reset and turn running
    // on so a fresh set of bars shows. This runs at most once per run (running
    // stays true until done/canceled), so it never wipes a scan already in flight.
    if (!running && p.phase !== "done" && p.phase !== "canceled") {
      index = zeroPhase();
      enrich = zeroPhase();
      write = zeroPhase();
      log = [];
      running = true;
    }
    if (p.phase === "index") {
      index = { done: p.done, total: p.total, t0: index.t0 || t, end: 0 };
    } else if (p.phase === "enrich") {
      index = finish(index, t);
      enrich = { done: p.done, total: p.total, t0: enrich.t0 || t, end: 0 };
    } else if (p.phase === "write") {
      index = finish(index, t);
      enrich = finish(enrich, t);
      write = { done: p.done, total: p.total, t0: write.t0 || t, end: 0 };
    } else if (p.phase === "done" || p.phase === "canceled") {
      index = finish(index, t);
      enrich = finish(enrich, t);
      write = finish(write, t);
      running = false;
    }
    // Human-readable line for the active file; newest first, deduped against the
    // current head. Capped generously so the fill-mode log (empty-state build) has
    // enough scrollback; the compact top-bar view clips it with max-height. Each
    // line carries its overall-sweep fraction `t` = (phaseIndex + done/total)/3, so
    // the view can color it the same as the bar's fill at emit time.
    if (p.current && p.current !== log[0]?.text) {
      const seg = p.phase === "enrich" ? 1 : p.phase === "write" ? 2 : 0;
      const frac = p.total ? p.done / p.total : 0;
      const t = (seg + frac) / 3;
      log = [{ id: logSeq++, text: p.current, t }, ...log].slice(0, 200);
    }
    return { running, index, enrich, write, log };
  });
  if (p.phase === "done" || p.phase === "canceled") {
    const canceled = p.phase === "canceled";
    for (const cb of doneCbs) cb(canceled);
  }
});

// Kick off a fresh scan. Resets the store and sets running SYNCHRONOUSLY so the
// bars appear before the first event lands. Both windows use browse_scan (DS used
// to call spend_scan; both funnel into the same guarded run_scan(deep=true), so
// they are equivalent -- unifying here keeps one command).
export function startScan(deep = true) {
  scanState.set({ ...initial(), running: true });
  invoke("browse_scan", { deep }).catch(() => {
    scanState.update((s) => ({ ...s, running: false }));
  });
}

export function cancelScan() {
  invoke("browse_cancel").catch(() => {});
}
