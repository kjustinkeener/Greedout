// One emitter for theme palette CSS, shared by the build-time generator
// (tools/gen-themes.mjs, run under Node) and the runtime injection path for
// user themes (loaded in the browser). Keeping a single source means a built-in
// palette in src/themes.generated.css and a hand-edited user palette are emitted
// by the exact same code and cannot drift in formatting or token order.
//
// Browser-safe: no Node builtins, and every DOM reference is inside a function
// (never at module top level) so Node can import the emitter without a `document`.

// The shape of one entry in src/lib/themes.json, and of a user theme. `auto`
// carries no colors of its own (its light palette is emitted from `light`);
// every other entry carries a full palette.
export interface ThemeColors {
  bg: number[]; // RGB triple -> --bg-rgb, kept a triple so the opacity slider only touches alpha
  fg: string;
  muted: string;
  track: string;
  green: string;
  yellow: string;
  red: string;
  live: string;
  edge: string;
  edgeSoft: string;
  hover: string;
  panel: string;
}

export interface ThemeData {
  id: string;
  label: string;
  group: string;
  scheme: string; // "dark" | "light" | "light dark"
  note?: string | null;
  colors?: ThemeColors;
  gradient?: [string, string, string];
  spendFg?: string | null;
  spendShadow?: string | null;
  autoLightFrom?: string; // "auto" only: emit its media palette from this id's colors
}

// The hex/rgba color keys: everything in ThemeColors except the bg RGB triple.
type ColorKey = Exclude<keyof ThemeColors, "bg">;

// CSS custom property <-> ThemeColors key. --bg-rgb and the gradient stops are
// handled separately (different value shapes); this covers the flat color tokens.
export const TOKEN_MAP: [string, ColorKey][] = [
  ["--fg", "fg"],
  ["--muted", "muted"],
  ["--track", "track"],
  ["--green", "green"],
  ["--yellow", "yellow"],
  ["--red", "red"],
  ["--live", "live"],
  ["--edge", "edge"],
  ["--edge-soft", "edgeSoft"],
  ["--hover", "hover"],
  ["--panel", "panel"],
];

// Emit the declaration body (each line indented `indent`) for a theme entry.
// Order: color-scheme, then spend-fg/spend-shadow (light palettes only), then
// --bg-rgb, the flat color tokens, and the three gauge stops. Source order
// varied per hand-written block but computed style does not depend on it.
// `omitScheme` drops color-scheme (auto's @media block carries it on the
// standalone one-liner, not the media rule).
export function themeToCss(
  t: ThemeData,
  indent = "  ",
  opts: { omitScheme?: boolean } = {},
): string {
  const c = t.colors;
  const lines: string[] = [];
  const push = (prop: string, val: string) => lines.push(`${indent}${prop}: ${val};`);
  if (!opts.omitScheme) push("color-scheme", t.scheme);
  if (t.spendFg != null) push("--spend-fg", t.spendFg);
  if (t.spendShadow != null) push("--spend-shadow", t.spendShadow);
  if (c) {
    push("--bg-rgb", c.bg.join(", "));
    for (const [css, key] of TOKEN_MAP) if (c[key] != null) push(css, c[key]);
  }
  if (t.gradient) {
    push("--g0", t.gradient[0]);
    push("--g1", t.gradient[1]);
    push("--g2", t.gradient[2]);
  }
  return lines.join("\n");
}

// A full `selector { ...body... }` rule for one theme. Used to build the runtime
// <style> for user themes; the build-time generator composes its own layout
// (banner, light/auto one-liners, @media block) around themeToCss directly.
export function themeRule(t: ThemeData, selector = `:root[data-theme="${t.id}"]`): string {
  return `${selector} {\n${themeToCss(t)}\n}`;
}

// Build the CSS text of a <style> element holding one rule per user theme, so a
// hand-edited palette resolves through data-theme exactly like a built-in.
export function userThemesCss(themes: ThemeData[]): string {
  return themes.map((t) => themeRule(t)).join("\n\n");
}

// Inject (or replace) the <style id> element carrying the user themes' palettes.
// Idempotent: called on load, whenever the set changes, and synchronously during
// cold start from the localStorage mirror so a selected user theme paints without
// a flash. Browser-only; never called from Node.
export const USER_THEMES_STYLE_ID = "greedout-user-themes";

export function injectUserThemes(
  themes: ThemeData[],
  styleId = USER_THEMES_STYLE_ID,
): void {
  if (typeof document === "undefined") return;
  let el = document.getElementById(styleId) as HTMLStyleElement | null;
  if (!el) {
    el = document.createElement("style");
    el.id = styleId;
    document.head.appendChild(el);
  }
  el.textContent = userThemesCss(themes);
}
