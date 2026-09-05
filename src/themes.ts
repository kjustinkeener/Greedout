import { mount } from "svelte";
import ThemeBrowser from "./lib/ThemeBrowser.svelte";
import { initFonts } from "./lib/fonts";
import { initTheme } from "./lib/theme";
import "./app.css";

initTheme();
void initFonts();

// The theme browser runs in its own window, mirroring the Settings/About pattern.
const app = mount(ThemeBrowser, {
  target: document.getElementById("app")!,
});

export default app;
