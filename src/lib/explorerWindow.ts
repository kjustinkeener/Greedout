// Open the Context Explorer (baseline) window targeted at a session or project,
// reusing the window if it is already open. Shared by the main window (clicking a
// session/project title) and Daily Spend (clicking a swim lane), so the reuse /
// create / tauri://error-retry dance lives in exactly one place.
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { Config } from "../types";

export interface ExplorerTarget {
  id?: string;
  title: string;
  project?: string;
  view: "session" | "project";
}

// Reuse the live Explorer (retarget it via an event) or create it with the target
// baked into the URL. Positions the new window offset from whichever window called
// this, so it opens next to the main gauge or next to Daily Spend, as appropriate.
export async function openExplorer(opts: ExplorerTarget): Promise<void> {
  const existing = await WebviewWindow.getByLabel("baseline");
  if (existing) {
    try {
      await existing.unminimize();
      await existing.show();
      await existing.setFocus();
      await existing.emit("baseline-goto", opts);
      return;
    } catch {
      try {
        await existing.close();
      } catch {
        // already gone
      }
    }
  }
  await createExplorerWindow(opts, true);
}

// A close() still in flight makes the constructor fail asynchronously via
// tauri://error ("a webview with label `baseline` already exists"), never thrown,
// so the window silently never appears. Tear down the straggler and retry once.
async function createExplorerWindow(opts: ExplorerTarget, retry: boolean): Promise<void> {
  const anchor = getCurrentWindow();
  let pos: { x: number; y: number } | undefined;
  try {
    const sf = await anchor.scaleFactor();
    const p = (await anchor.outerPosition()).toLogical(sf);
    pos = { x: Math.round(p.x + 24), y: Math.round(p.y + 24) };
  } catch {
    // Position unavailable; let the OS place it.
  }
  const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
  const q = new URLSearchParams({
    id: opts.id ?? "",
    title: opts.title,
    view: opts.view,
    ...(opts.project ? { project: opts.project } : {}),
  });
  const w = new WebviewWindow("baseline", {
    url: `baseline.html?${q.toString()}`,
    title: "Context Explorer",
    width: 720,
    height: 560,
    resizable: true,
    alwaysOnTop: aot,
    focus: true,
    ...(pos ? { x: pos.x, y: pos.y } : {}),
  });
  if (retry) {
    void w.once("tauri://error", async () => {
      const stale = await WebviewWindow.getByLabel("baseline");
      if (stale) {
        try {
          await stale.close();
        } catch {
          // already gone
        }
      }
      setTimeout(() => void createExplorerWindow(opts, false), 150);
    });
  }
}
