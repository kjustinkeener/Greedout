import { mount } from "svelte";
import { invoke } from "@tauri-apps/api/core";
import App from "./App.svelte";
import Installer from "./lib/Installer.svelte";
import { initI18n } from "./lib/i18n.svelte";
import { initFonts } from "./lib/fonts";
import { initTheme, previewTheme } from "./lib/theme";
import "./app.css";

// Stamp the saved palette synchronously, before mount, so a light-theme user
// does not see a frame of the default dark palette while the async config read
// is in flight. App.svelte re-applies the authoritative value once it returns.
initTheme();
void initFonts();

// Top-level await: the catalog has to be in place before the first mount, or
// every launch renders one frame of English before switching.
await initI18n();

type SetupState = {
  needsSetup: boolean;
  installed: boolean;
  existing: boolean;
  version: string;
  installDir: string;
};

// This exe is both the installer and the app. The backend already decided which
// one it is (it sized the window for the card before the webview existed); ask
// it rather than guessing here, so there is exactly one answer.
const setup = await invoke<SetupState>("setup_state").catch(() => null);

// The stamp above restores whatever the app last used, which is right for the
// app and wrong for the card. The installer has no config to correct it with,
// and someone meeting this exe for the first time should meet the default
// palette. Mode is only knowable once the backend answers, but nothing has
// mounted yet, so resetting here costs no visible frame.
if (setup?.needsSetup) previewTheme("dark");

const target = document.getElementById("app")!;

const app = setup?.needsSetup
  ? mount(Installer, {
      target,
      props: {
        version: setup.version,
        installDir: setup.installDir,
        existing: setup.existing,
      },
    })
  : mount(App, { target });

export default app;
