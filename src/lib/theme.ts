// Only the background alpha is user-adjustable (opacity slider); the base RGB
// comes from the active theme's palette in app.css.
export function applyOpacity(o: number) {
  const a = Math.min(1, Math.max(0.3, o));
  document.documentElement.style.setProperty("--bg-alpha", String(a));
}

// A theme id; "auto" follows the OS, the rest are fixed palettes in app.css.
export type Theme = string;

// Picker options (id + label + group). Add a theme here and give it a block in
// app.css. `group` drives the <optgroup> sections; GROUP_ORDER sets their order.
export const GROUP_ORDER = [
  "Core",
  "Neon",
  "Warm",
  "Cool",
  "Greens",
  "Neutral",
  "Light",
  "Blush",
  "Bright",
  "Light Pastel",
  "Pastel",
] as const;

export const THEMES: { id: string; label: string; group: string }[] = [
  // Core
  { id: "auto", label: "Auto (system)", group: "Core" },
  { id: "dark", label: "Dark", group: "Core" },
  { id: "light", label: "Light", group: "Core" },
  { id: "ash", label: "Ash", group: "Core" },
  // Neon
  { id: "cyberpunk", label: "Neon Grid", group: "Neon" },
  { id: "vaporwave", label: "Vaporwave", group: "Neon" },
  { id: "crimson", label: "Crimson", group: "Neon" },
  { id: "deepsea", label: "Deep Sea", group: "Neon" },
  { id: "purple", label: "Midnight Purple", group: "Neon" },
  { id: "dusk", label: "Dusk", group: "Neon" },
  { id: "amethyst", label: "Amethyst", group: "Neon" },
  { id: "ultraviolet", label: "Ultraviolet", group: "Neon" },
  { id: "orchid", label: "Orchid", group: "Neon" },
  // Warm
  { id: "amber", label: "Amber", group: "Warm" },
  { id: "sunset", label: "Sunset", group: "Warm" },
  { id: "coffee", label: "Coffee", group: "Warm" },
  { id: "gruvbox", label: "Retro", group: "Warm" },
  { id: "lofi", label: "Lofi", group: "Warm" },
  { id: "rosewood", label: "Rosewood", group: "Warm" },
  { id: "ember", label: "Ember", group: "Warm" },
  { id: "sienna", label: "Sienna", group: "Warm" },
  { id: "terracotta", label: "Terracotta", group: "Warm" },
  // Cool
  { id: "ocean", label: "Ocean", group: "Cool" },
  { id: "graphite", label: "Graphite", group: "Cool" },
  { id: "flux", label: "Flux", group: "Cool" },
  { id: "steelblue", label: "Steel Blue", group: "Cool" },
  { id: "glacier", label: "Glacier", group: "Cool" },
  { id: "tide", label: "Tide", group: "Cool" },
  { id: "aurora", label: "Aurora", group: "Cool" },
  // Greens
  { id: "matrix", label: "Terminal", group: "Greens" },
  { id: "forest", label: "Forest", group: "Greens" },
  { id: "solarized", label: "Solar", group: "Greens" },
  { id: "monokai", label: "Acid", group: "Greens" },
  { id: "reef", label: "Reef", group: "Greens" },
  { id: "moss", label: "Moss", group: "Greens" },
  // Neutral
  { id: "linen", label: "Linen", group: "Neutral" },
  { id: "oat", label: "Oat", group: "Neutral" },
  { id: "greige", label: "Greige", group: "Neutral" },
  { id: "stone", label: "Stone", group: "Neutral" },
  { id: "taupe", label: "Taupe", group: "Neutral" },
  { id: "clay", label: "Clay", group: "Neutral" },
  // Light
  { id: "paper", label: "Paper", group: "Light" },
  { id: "sky", label: "Sky", group: "Light" },
  { id: "sepia", label: "Sepia", group: "Light" },
  { id: "sage", label: "Sage", group: "Light" },
  { id: "mint", label: "Mint", group: "Light" },
  { id: "fog", label: "Fog", group: "Light" },
  // Blush
  { id: "lavender", label: "Lavender", group: "Blush" },
  { id: "bubblegum", label: "Bubblegum", group: "Blush" },
  { id: "cottoncandy", label: "Cotton Candy", group: "Blush" },
  { id: "sakura", label: "Sakura", group: "Blush" },
  { id: "peach", label: "Peach", group: "Blush" },
  { id: "dustyrose", label: "Dusty Rose", group: "Blush" },
  // Bright
  { id: "pop", label: "Pop", group: "Bright" },
  { id: "citrus", label: "Citrus", group: "Bright" },
  { id: "candy", label: "Candy", group: "Bright" },
  { id: "volt", label: "Volt", group: "Bright" },
  { id: "azure", label: "Azure", group: "Bright" },
  // Light Pastel
  { id: "cornflower", label: "Cornflower", group: "Light Pastel" },
  { id: "seafoam", label: "Seafoam", group: "Light Pastel" },
  { id: "pistachio", label: "Pistachio", group: "Light Pastel" },
  { id: "butter", label: "Butter", group: "Light Pastel" },
  { id: "apricot", label: "Apricot", group: "Light Pastel" },
  // Pastel
  { id: "rose", label: "Rose", group: "Pastel" },
  { id: "nord", label: "Arctic", group: "Pastel" },
  { id: "slate", label: "Slate", group: "Pastel" },
  { id: "dracula", label: "Nocturne", group: "Pastel" },
  { id: "tokyonight", label: "Skyline", group: "Pastel" },
  { id: "mauve", label: "Mauve", group: "Pastel" },
];

import { writable } from "svelte/store";

// Bumped whenever the theme changes; components read it to recompute gauge
// colors (which come from CSS vars and are otherwise invisible to reactivity).
export const themeTick = writable(0);

// Every id app.css actually has a palette block for. A config file can be
// hand-edited or left behind by an older build, and setting an unknown id on the
// root leaves the app on no palette at all (unstyled, not merely mistuned), so
// anything unrecognized falls back to "auto".
const KNOWN = new Set(THEMES.map((t) => t.id));

export function resolveTheme(t: string | null | undefined): Theme {
  return t && KNOWN.has(t) ? t : "auto";
}

// The chosen theme lives in the backend config, which is an async read, so the
// first paint would land on the bare :root palette (a dark flash for light-theme
// users) before that read returns. Mirror the choice into localStorage, which IS
// synchronous, so every entry module can stamp the palette before mount.
const THEME_LS_KEY = "greedout:theme";

// Select the color palette. "auto" follows the OS via prefers-color-scheme.
// Stores the raw id (including "auto") so a reload re-resolves against the
// current prefers-color-scheme rather than freezing a resolved light/dark.
export function applyTheme(t: Theme) {
  document.documentElement.setAttribute("data-theme", resolveTheme(t));
  themeTick.update((n) => n + 1);
  try {
    localStorage.setItem(THEME_LS_KEY, t ?? "auto");
  } catch {
    // Private mode / blocked storage: the async config read is still the source
    // of truth, so a missing mirror only means the flash returns, never a wrong
    // palette.
  }
}

/** Paint a palette without remembering it.
 *
 *  applyTheme mirrors into localStorage, which initTheme reads back on every
 *  cold start. That is right for a chosen theme and wrong for a temporary one:
 *  a preview that persists is indistinguishable from a preference, and the next
 *  launch comes up wearing it. Use this for anything the user is only trying on.
 */
export function previewTheme(t: Theme) {
  document.documentElement.setAttribute("data-theme", resolveTheme(t));
  themeTick.update((n) => n + 1);
}

// Apply the last-known theme synchronously, before the first mount. Reads the
// localStorage mirror; the async config load re-applies the authoritative value
// a moment later (a no-op when they agree). Call at the top of every entry module.
export function initTheme() {
  let stored: string | null = null;
  try {
    stored = localStorage.getItem(THEME_LS_KEY);
  } catch {
    // ignore; fall through to the default palette until config loads
  }
  document.documentElement.setAttribute("data-theme", resolveTheme(stored));
}

// Under "auto" the OS flipping light/dark swaps every CSS variable without
// anything calling applyTheme, so the gauge colors sampled out of the palette
// stay on the old scale until the next manual theme change. Nothing to re-apply
// (the media query does that itself); we just have to bump the tick so the
// samplers re-read. Registered once per document, which means once per window.
if (typeof window !== "undefined" && window.matchMedia) {
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", () => {
      if (document.documentElement.getAttribute("data-theme") === "auto") {
        themeTick.update((n) => n + 1);
      }
    });
}

// Shared gauge color scale, sourced from the active theme's --g0/--g1/--g2 CSS
// vars (@ 0 / 0.52 / 1) so the speedo dial, bars, history, and spend text all
// track the theme. `f` is a 0..1 fill fraction (context / target).
const GAUGE_OFFSETS = [0, 0.52, 1];
const FALLBACK: [number, number, number][] = [
  [0x00, 0xff, 0xe8],
  [0xff, 0xfe, 0x00],
  [0xff, 0x00, 0x00],
];

function hexToRgb(hex: string): [number, number, number] | null {
  const m = hex.trim().replace("#", "");
  if (m.length !== 6) return null;
  const n = parseInt(m, 16);
  if (Number.isNaN(n)) return null;
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function gaugeStops(): [number, [number, number, number]][] {
  const cs = getComputedStyle(document.documentElement);
  return GAUGE_OFFSETS.map((o, i) => {
    const rgb = hexToRgb(cs.getPropertyValue(`--g${i}`)) ?? FALLBACK[i];
    return [o, rgb] as [number, [number, number, number]];
  });
}

// Color for a spend amount. Light palettes declare --spend-fg because the
// cyan/yellow/red gauge scale is unreadable on a pale ground; dark palettes
// leave it unset and get the scale, so the gauge and its spend figure match.
// Palette-driven on purpose: a new light theme only has to set the variable.
export function spendOverride(): string {
  return getComputedStyle(document.documentElement)
    .getPropertyValue("--spend-fg")
    .trim();
}

export function spendFor(f: number | null): string {
  return spendOverride() || (f == null ? "var(--muted)" : gaugeColor(f));
}

export function gaugeColor(f: number): string {
  const stops = gaugeStops();
  const x = Math.min(1, Math.max(0, f));
  let a = stops[0];
  let b = stops[stops.length - 1];
  for (let i = 0; i < stops.length - 1; i++) {
    if (x >= stops[i][0] && x <= stops[i + 1][0]) {
      a = stops[i];
      b = stops[i + 1];
      break;
    }
  }
  const t = b[0] === a[0] ? 0 : (x - a[0]) / (b[0] - a[0]);
  const c = a[1].map((v, k) => Math.round(v + (b[1][k] - v) * t));
  return `rgb(${c[0]},${c[1]},${c[2]})`;
}
