// Shared control for the ledger sync the Context Explorer waits on: ONE
// implementation of the sync trigger, its lifecycle, and its live progress state.
//
// The backend sync is already the single shared thing (the ledger); what would be
// duplicated is the FRONTEND side, so this module owns the `listen("browse-progress")`,
// the progress bookkeeping, and the start/cancel invokes once. Hard limit: windows
// are SEPARATE webviews (separate JS contexts), so this module loads once PER window
// and cannot literally share runtime state across them. Every window still receives
// the same backend event stream, so a window that didn't start a sync still renders
// it and still gets onDone.
import { writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { BrowseProgress } from "../types";

// The sync's live state. t0/end are wall-clock (performance.now()) measured on this
// side: t0 = first event, end = when the sync finished. Both 0 until they happen;
// elapsed is (end || now) - t0, so the running sync counts up live and a finished
// one freezes.
export interface PhaseState {
  done: number;
  total: number;
  t0: number;
  end: number;
}
export interface ScanState {
  running: boolean;
  sync: PhaseState;
}

const zeroPhase = (): PhaseState => ({ done: 0, total: 0, t0: 0, end: 0 });
const initial = (): ScanState => ({ running: false, sync: zeroPhase() });

// The single source of truth for sync progress in this window. ScanProgress renders
// from it; host windows can subscribe for side effects.
export const scanState = writable<ScanState>(initial());

// Done/canceled callbacks: host windows register their data-refresh here instead
// of each keeping a second "browse-progress" listener.
const doneCbs = new Set<(canceled: boolean) => void>();
export function onScanDone(cb: (canceled: boolean) => void): () => void {
  doneCbs.add(cb);
  return () => void doneCbs.delete(cb);
}

// One listener per window, registered at module load. It lives for the window's
// lifetime, so there is no unlisten to manage.
void listen<BrowseProgress>("browse-progress", (e) => {
  const p = e.payload;
  const t = performance.now();
  scanState.update((s) => {
    // A sync is underway but this window didn't start it (it mounted mid-sync, or
    // another window started it): turn running on so the bar shows. This runs at
    // most once per run (running stays true until done/canceled).
    if (p.phase === "sync") {
      const sync = { done: p.done, total: p.total, t0: s.sync.t0 || t, end: 0 };
      return { running: true, sync };
    }
    const sync = s.sync.t0 && !s.sync.end ? { ...s.sync, end: t } : s.sync;
    return { running: false, sync };
  });
  if (p.phase === "done" || p.phase === "canceled") {
    const canceled = p.phase === "canceled";
    for (const cb of doneCbs) cb(canceled);
  }
});

// Kick off a sync. Resets the store and sets running SYNCHRONOUSLY so the bar
// appears before the first event lands.
export function startScan() {
  scanState.set({ ...initial(), running: true });
  invoke("browse_scan").catch(() => {
    scanState.update((s) => ({ ...s, running: false }));
  });
}

export function cancelScan() {
  invoke("browse_cancel").catch(() => {});
}
