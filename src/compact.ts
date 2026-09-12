import { mount } from "svelte";
import CompactSummary from "./lib/CompactSummary.svelte";
import { initI18n } from "./lib/i18n.svelte";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { initFonts } from "./lib/fonts";
import { initTheme } from "./lib/theme";
import "./app.css";

initTheme();
void initFonts();

// The compaction-summary reader runs in its own window (see the About/Settings
// pattern). Top-level await so the catalog is in place before the first mount.
await initI18n();

const params = new URLSearchParams(location.search);

const app = mount(CompactSummary, {
  target: document.getElementById("app")!,
  props: {
    id: params.get("id") ?? "",
    heading: params.get("title") ?? "",
    onClose: () => getCurrentWindow().close(),
  },
});

export default app;
