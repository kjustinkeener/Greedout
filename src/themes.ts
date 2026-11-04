import { mount } from "svelte";
import ThemeBrowser from "./lib/ThemeBrowser.svelte";
import { initFonts } from "./lib/fonts";
import { initTheme } from "./lib/theme";
import { initI18n } from "./lib/i18n.svelte";
import "./app.css";

initTheme();
void initFonts();

// The Supporter group name and the locked-theme tooltip are translated.
await initI18n();

// The theme browser runs in its own window, mirroring the Settings/About pattern.
const app = mount(ThemeBrowser, {
  target: document.getElementById("app")!,
});

export default app;
