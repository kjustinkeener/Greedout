// Restore a secondary window's saved size and position, and clamp it to the
// monitor it lands on so it can never spawn half off-screen. The off-screen bug:
// every window opened at (main's position + a fixed offset), so with the main gauge
// docked to the right edge each new window started past the edge. The clamp fixes
// that even on a first open, before any geometry has been saved.
//
// Persistence itself is Rust-side (winstate.rs, on the move/resize events); this is
// only the restore + clamp, which has to live in JS because these windows are
// created here and never pass through Rust's setup(). All math is in PHYSICAL
// pixels: that is what winstate stores and what the monitor APIs report, so nothing
// has to reason about the scale factor.
import { invoke } from "@tauri-apps/api/core";
import {
  availableMonitors,
  currentMonitor,
  primaryMonitor,
  type Monitor,
} from "@tauri-apps/api/window";
import { PhysicalPosition, PhysicalSize } from "@tauri-apps/api/dpi";
import type { WebviewWindow } from "@tauri-apps/api/webviewWindow";

interface WinState {
  x: number;
  y: number;
  width: number;
  height: number;
  maximized: boolean;
}

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

// The monitor a rectangle overlaps most. Falls back to the current, then the
// primary monitor when the rectangle overlaps none, e.g. geometry saved on a
// display that has since been unplugged.
async function pickMonitor(r: Rect): Promise<Monitor | null> {
  let mons: Monitor[] = [];
  try {
    mons = await availableMonitors();
  } catch {
    mons = [];
  }
  let best: Monitor | null = null;
  let bestArea = 0;
  for (const m of mons) {
    const ox = Math.max(
      0,
      Math.min(r.x + r.w, m.position.x + m.size.width) - Math.max(r.x, m.position.x),
    );
    const oy = Math.max(
      0,
      Math.min(r.y + r.h, m.position.y + m.size.height) - Math.max(r.y, m.position.y),
    );
    const area = ox * oy;
    if (area > bestArea) {
      bestArea = area;
      best = m;
    }
  }
  if (best) return best;
  try {
    return await currentMonitor();
  } catch {
    // fall through
  }
  try {
    return await primaryMonitor();
  } catch {
    return null;
  }
}

// Apply the saved geometry (if any), then clamp it to fit its monitor. Runs after
// the window exists; call it via placeWindow, which hooks "tauri://created".
async function applyGeometry(win: WebviewWindow, label: string): Promise<void> {
  try {
    const saved = await invoke<WinState | null>("load_window_state", { label });
    const size = await win.outerSize();
    const pos = await win.outerPosition();
    let w = saved?.width ?? size.width;
    let h = saved?.height ?? size.height;
    let x = saved?.x ?? pos.x;
    let y = saved?.y ?? pos.y;
    const mon = await pickMonitor({ x, y, w, h });
    if (mon) {
      const mx = mon.position.x;
      const my = mon.position.y;
      const mw = mon.size.width;
      const mh = mon.size.height;
      w = Math.min(w, mw);
      h = Math.min(h, mh);
      x = Math.min(Math.max(x, mx), mx + mw - w);
      y = Math.min(Math.max(y, my), my + mh - h);
    }
    await win.setSize(new PhysicalSize(w, h));
    await win.setPosition(new PhysicalPosition(x, y));
  } catch {
    // Best effort: the window keeps its create-time geometry.
  }
}

// Restore and clamp a freshly constructed secondary window once it exists.
export function placeWindow(win: WebviewWindow, label: string): void {
  void win.once("tauri://created", () => void applyGeometry(win, label));
}
