// Open the theme browser, optionally scrolled to a group. Reuses the window if it
// is already open (retargeting it by event); a new window gets the group in its
// URL hash instead, because its listener is not registered yet when it is created.
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { placeWindow } from "./windowGeom";
import { t } from "./i18n.svelte";
import type { Config } from "../types";

export async function openThemesWindow(group?: string): Promise<void> {
  const existing = await WebviewWindow.getByLabel("themes");
  if (existing) {
    try {
      await existing.unminimize();
      await existing.show();
      await existing.setFocus();
      if (group) await existing.emit("themes:goto", group);
      return;
    } catch {
      try {
        await existing.close();
      } catch {
        // already gone
      }
    }
  }
  await createThemesWindow(group, true);
}

// A close() still in flight makes the constructor fail asynchronously via
// tauri://error, never thrown, so the window silently never appears. Tear down the
// straggler and retry once.
async function createThemesWindow(group: string | undefined, retry: boolean): Promise<void> {
  const anchor = getCurrentWindow();
  let pos: { x: number; y: number } | undefined;
  try {
    const sf = await anchor.scaleFactor();
    const p = (await anchor.outerPosition()).toLogical(sf);
    pos = { x: Math.round(p.x + 20), y: Math.round(p.y + 20) };
  } catch {
    // Position unavailable; let the OS place it.
  }
  const aot = (await invoke<Config>("get_config").catch(() => null))?.always_on_top ?? true;
  const w = new WebviewWindow("themes", {
    url: group ? `themes.html#${encodeURIComponent(group)}` : "themes.html",
    title: `Greedout ${t("settings.theme")}`,
    width: 600,
    height: 620,
    minWidth: 320,
    minHeight: 300,
    // Borderless + transparent like the other secondary windows.
    decorations: false,
    transparent: true,
    resizable: true,
    alwaysOnTop: aot,
    focus: true,
    ...(pos ? { x: pos.x, y: pos.y } : {}),
  });
  placeWindow(w, "themes");
  if (retry) {
    void w.once("tauri://error", async () => {
      const stale = await WebviewWindow.getByLabel("themes");
      if (stale) {
        try {
          await stale.close();
        } catch {
          // already gone
        }
      }
      setTimeout(() => void createThemesWindow(group, false), 150);
    });
  }
}
