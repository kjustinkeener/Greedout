import { mount } from "svelte";
import FontBrowser from "./lib/FontBrowser.svelte";
import { initTheme } from "./lib/theme";
import "./app.css";

initTheme();

// The font picker runs in its own window, mirroring the theme browser. It loads
// and applies the current font preferences itself (see FontBrowser.svelte), so
// unlike the other entries it does not call initFonts().
const app = mount(FontBrowser, {
  target: document.getElementById("app")!,
});

export default app;
