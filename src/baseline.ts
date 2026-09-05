import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";
import Baseline from "./lib/Baseline.svelte";
import { applyTheme, initTheme } from "./lib/theme";
import type { Config } from "./types";
import { initFonts } from "./lib/fonts";
import "./app.css";

// Stamp the last-known palette before mount; the get_config call below re-applies
// the authoritative value (a no-op when they agree).
initTheme();
void initFonts();

// Match the main window's theme so the gauge color vars (--g0/g1/g2) used to
// color the treemap reflect the palette chosen in Settings, and inherit the
// saved zoom. Fixed dialogs can reasonably stay at 1.0, but the Explorer is a
// dense data window: someone who zoomed the gauge up did it to read text.
invoke<Config>("get_config")
  .then((c) => {
    applyTheme((c.theme ?? "dark") as never);
    const scale = c.ui_scale ?? 1;
    if (scale !== 1) {
      document.documentElement.style.zoom = String(scale);
    }
  })
  .catch(() => {});

// The baseline analysis runs in its own window (separate from the gauge window
// and 2x larger). The target session's id/title arrive as query params.
const params = new URLSearchParams(location.search);

const app = mount(Baseline, {
  target: document.getElementById("app")!,
  props: {
    id: params.get("id") ?? "",
    title: params.get("title") ?? "",
    initProject: params.get("project") ?? "",
    initView: params.get("view") ?? "",
  },
});

export default app;
