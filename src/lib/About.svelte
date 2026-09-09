<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  const REPO = "https://github.com/kjustinkeener/Greedout";

  /// The webview would navigate the About window itself; hand the URL to the shell
  /// instead so it opens in the user's browser.
  function openLink(e: MouseEvent) {
    e.preventDefault();
    const href = (e.currentTarget as HTMLAnchorElement).href;
    void invoke("open_url", { url: href });
  }
  import { getVersion } from "@tauri-apps/api/app";
  import { listen } from "@tauri-apps/api/event";
  import type { Config } from "../types";
  import { applyTheme, type Theme } from "./theme";
  import BrandIcon from "./BrandIcon.svelte";
  import { onDestroy } from "svelte";
  import { t, watchLocale } from "./i18n.svelte";

  type UpdateInfo = { version: string; notes: string; url: string; signature: string };
  let checking = $state(false);
  let updateMsg = $state("");

  /// Checks, then installs straight away if there is something to install: the
  /// user pressed a button that says what it does, so a confirm step here would
  /// be asking the same question twice.
  async function checkForUpdate() {
    if (checking) return;
    checking = true;
    updateMsg = "";
    try {
      const r = await invoke<{ current: string; available: UpdateInfo | null }>("update_check");
      if (!r.available) {
        updateMsg = t("update.upToDate");
      } else {
        updateMsg = t("update.downloading", { version: r.available.version });
        await invoke("update_apply", { info: r.available });
        updateMsg = t("update.installed");
      }
    } catch (e) {
      updateMsg = t("update.failed", { error: String(e) });
    }
    checking = false;
  }

  let { onClose }: { onClose: () => void } = $props();

  // This window can be open while the language is changed in Settings. Held as a
  // promise rather than started in onMount: an async onMount callback in Svelte 5
  // cannot return a cleanup function.
  const unlistenLocale = watchLocale();
  onDestroy(() => void unlistenLocale.then((u) => u()));

  let version = $state("");
  let built = $state("");

  // Match the app's current theme so this window doesn't flash a default palette.
  (async () => {
    try {
      const cfg = await invoke<Config>("get_config");
      applyTheme(cfg.theme);
    } catch {
      // Keep the default theme if config isn't reachable.
    }
    try {
      version = await getVersion();
    } catch {
      // Leave version blank if unavailable.
    }
    try {
      // Stamped at compile time by build.rs. A version alone does not identify
      // which build a bug report came from while releases are frequent.
      const secs = await invoke<number>("build_epoch");
      if (secs > 0) {
        built = new Date(secs * 1000).toISOString().slice(0, 10);
      }
    } catch {
      // Leave the build date off if the command is unavailable.
    }
  })();

  // Follow live theme changes. Without this, switching theme with About open
  // leaves it on the palette it happened to load with.
  $effect(() => {
    let un: (() => void) | undefined;
    listen<Theme>("theme", (e) => applyTheme(e.payload)).then((f) => (un = f));
    return () => un?.();
  });
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === "Escape") onClose();
  }}
/>

<div class="panel">
  <span class="mark"><BrandIcon size={64} /></span>
  <div class="name">greedout</div>
  {#if version}
    <div class="ver">
      {t("about.version", { version })}{#if built}&nbsp;&middot; {t("about.built", { date: built })}{/if}
    </div>
  {/if}

  <p class="tagline">{t("about.tagline")}</p>

  <div class="sep"></div>

  <p class="body">{t("about.body")}</p>

  <div class="meta">
    <div class="mrow"><span class="k">{t("about.madeBy")}</span><span class="v">Justin Keener</span></div>
    <div class="mrow"><span class="k">{t("about.builtWith")}</span><span class="v">Rust · Tauri · Svelte</span></div>
    <!-- The bundled fonts are OFL-1.1 and ship inside the exe, so their notices have to
         be reachable from the installed app, not only from the repo. -->
    <div class="mrow">
      <span class="k">{t("about.license")}</span>
      <span class="v"><a href={REPO + "/blob/main/LICENSE"} onclick={openLink}>MIT</a></span>
    </div>
    <div class="mrow">
      <span class="k">{t("about.notices")}</span>
      <span class="v"><a href={REPO + "/blob/main/THIRD-PARTY-NOTICES.md"} onclick={openLink}>THIRD-PARTY-NOTICES.md</a></span>
    </div>
  </div>

  <!-- Full URLs and a contact address, shown in full and opened in the shell (the
       webview would otherwise navigate this window). The email is a mailto: link,
       which open_url now allows alongside https. -->
  <div class="links">
    <a href={REPO} onclick={openLink}>{REPO}</a>
    <a href="https://fasterdb.com/software/greedout/" onclick={openLink}>https://fasterdb.com/software/greedout</a>
    <a href="mailto:gofast@fasterdb.com" onclick={openLink}>gofast@fasterdb.com</a>
  </div>

  {#if updateMsg}
    <p class="umsg">{updateMsg}</p>
  {/if}

  <div class="actions">
    <!-- The manual check exists so turning the launch check off is not the same
         as giving up updates: this button ignores the setting. -->
    <button class="btn" onclick={checkForUpdate} disabled={checking}>
      {checking ? t("update.checking") : t("update.check")}
    </button>
    <button class="btn primary" onclick={onClose}>{t("common.close")}</button>
  </div>
</div>

<style>
  .mrow a {
    color: inherit;
    text-decoration: underline;
    text-underline-offset: 2px;
    cursor: pointer;
  }
  .mrow a:hover {
    color: var(--g1);
  }

  .links {
    align-self: stretch;
    margin-top: 12px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 3px;
    font-size: calc(10.5px * var(--size-ui));
    /* Full URLs are one long unbreakable token; let them wrap at the window edge. */
    overflow-wrap: anywhere;
  }
  .links a {
    color: var(--muted);
    text-decoration: none;
    cursor: pointer;
  }
  .links a:hover {
    color: var(--g1);
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  .umsg {
    margin: 10px 0 0;
    font-size: 11.5px;
    color: var(--muted);
    text-align: center;
    /* Update errors carry whatever the network layer said, which usually means a
       URL: one unbreakable token that otherwise runs past the window edge. */
    overflow-wrap: anywhere;
  }
  :global(html),
  :global(body) {
    background: var(--bg);
  }
  .panel {
    min-height: 100vh;
    box-sizing: border-box;
    padding: 16px;
    color: var(--fg);
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
  }
  .mark {
    display: block;
    margin: 4px 0 8px;
    filter: drop-shadow(0 1px 1.5px rgba(0, 0, 0, 0.85));
  }
  .name {
    /* Exempt from the font preferences, same as the header wordmark. */
    font-family: "Doto", ui-monospace, monospace;
    font-size-adjust: none;
    font-weight: 900;
    font-size: 24px;
    letter-spacing: 0.01em;
    text-transform: lowercase;
    background: linear-gradient(0deg, var(--g0), var(--g1) 52%, var(--g2));
    -webkit-background-clip: text;
    background-clip: text;
    -webkit-text-fill-color: transparent;
    color: transparent;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.85));
  }
  .ver {
    color: var(--muted);
    font-size: calc(12px * var(--size-num));
    margin-top: 2px;
    font-family: var(--font-num);
    font-size-adjust: var(--font-num-adj);
    font-variant-numeric: tabular-nums;
  }
  .tagline {
    color: var(--fg);
    font-size: calc(12px * var(--size-ui));
    margin: 10px 0 0;
    line-height: 1.4;
  }
  .sep {
    height: 1px;
    align-self: stretch;
    background: var(--edge);
    margin: 12px 0;
  }
  .body {
    color: var(--muted);
    font-size: calc(11px * var(--size-ui));
    line-height: 1.5;
    margin: 0;
  }
  .meta {
    align-self: stretch;
    margin-top: 12px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .mrow {
    display: flex;
    justify-content: space-between;
    gap: 8px;
    font-size: calc(11px * var(--size-ui));
  }
  .k {
    color: var(--muted);
  }
  .v {
    color: var(--fg);
  }
  .actions {
    margin-top: auto;
    padding-top: 14px;
  }
  .btn {
    background: var(--track);
    border: 1px solid var(--edge);
    border-radius: 4px;
    color: var(--fg);
    padding: 3px 14px;
    cursor: pointer;
    font: inherit;
    font-size-adjust: var(--font-ui-adj);
  }
  .btn.primary {
    background: var(--green);
    border-color: var(--green);
    color: #06210c;
    font-weight: var(--w-semibold);
  }
</style>
