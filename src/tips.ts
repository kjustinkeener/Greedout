import { mount } from "svelte";
import Tips from "./lib/Tips.svelte";
import { initI18n } from "./lib/i18n.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { initFonts } from "./lib/fonts";
import { initTheme } from "./lib/theme";
import "./app.css";

initTheme();
void initFonts();

// Tips runs in its own window, mirroring the About window.
// Top-level await: the catalog has to be in place before the first mount, or
// every launch renders one frame of English before switching.
await initI18n();

const app = mount(Tips, {
  target: document.getElementById("app")!,
  props: { onClose: () => getCurrentWindow().close() },
});

export default app;
