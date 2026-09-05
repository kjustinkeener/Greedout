<script lang="ts">
  // Shared header branding: themed speedo mark + "greedout" wordmark, both with a
  // centered narrow black drop shadow. Used across the main window, Context
  // Explorer, Settings and About so they read as one app.
  import BrandIcon from "./BrandIcon.svelte";
  let { size = 16, font = 15 }: { size?: number; font?: number } = $props();
</script>

<span class="brand-wrap" data-tauri-drag-region>
  <span class="icon" style:--bi-size="{size}px" data-tauri-drag-region><BrandIcon {size} /></span>
  <span class="brand" style:font-size="{font}px" data-tauri-drag-region>greedout</span>
</span>

<style>
  .brand-wrap {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
  }
  .icon {
    display: block;
    flex: 0 0 auto;
    pointer-events: none;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.85));
  }
  .brand {
    /* The wordmark is the brand mark, so it is deliberately exempt from the
       font preferences: fixed face, fixed weight, and no inherited x-height
       pin from a display interface font. */
    font-family: "Doto", ui-monospace, monospace;
    font-size-adjust: none;
    font-weight: 900;
    line-height: 1.35;
    padding-bottom: 1px;
    letter-spacing: 0.01em;
    text-transform: lowercase;
    /* Vertical fill in the shared speedo/gauge gradient (cyan→yellow@52%→red). */
    background: linear-gradient(0deg, var(--g0), var(--g1) 52%, var(--g2));
    -webkit-background-clip: text;
    background-clip: text;
    -webkit-text-fill-color: transparent;
    color: transparent;
    pointer-events: none;
    filter: drop-shadow(0 1px 1px rgba(0, 0, 0, 0.85));
  }
</style>
