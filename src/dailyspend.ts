import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";
import DailySpend from "./lib/DailySpend.svelte";
import { applyTheme, initTheme } from "./lib/theme";
import { initI18n } from "./lib/i18n.svelte";
import type { Config } from "./types";
import { initFonts } from "./lib/fonts";
import "./app.css";

// Stamp the last-known palette before mount; the get_config call below re-applies
// the authoritative value (a no-op when they agree). Mirrors baseline.ts so the
// window opens in the right theme with no cold-start flash.
initTheme();
void initFonts();

invoke<Config>("get_config")
  .then((c) => {
    applyTheme((c.theme ?? "dark") as never);
    const scale = c.ui_scale ?? 1;
    if (scale !== 1) {
      document.documentElement.style.zoom = String(scale);
    }
  })
  .catch(() => {});

// Top-level await: the catalog has to be in place before the first mount, or the
// window renders one frame of English before switching.
await initI18n();

const app = mount(DailySpend, {
  target: document.getElementById("app")!,
});

export default app;
