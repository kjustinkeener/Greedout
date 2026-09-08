// Interface font customization.
//
// Three independent slots so a readout app can be tuned the way a dashboard is:
//   ui    - every label, title and menu (the app's voice)
//   num   - the live numbers (context, spend, percent, sizes, timestamps)
//   mono  - message bodies and error text in the Context Explorer
//
// The logo wordmark is deliberately NOT customizable: it is the brand mark and
// stays Doto in every configuration.
//
// Weights are indirected through a five-rung ladder (--w-light .. --w-bold) so
// one "boldness" preference can shift the whole interface at once. Components
// never hard-code a numeric weight; they name a rung.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Slot = "ui" | "num" | "mono";

export interface FontDef {
  id: string;
  label: string;
  /** CSS font-family value, already including fallbacks. */
  stack: string;
  group: string;
  /** Which slots offer this face. */
  slots: Slot[];
  /** Human note about the weights the face actually ships. */
  weights: string;
  /**
   * font-size-adjust value. Display faces render far larger or smaller than a
   * text face at the same px size; pinning the x-height keeps a swap from
   * blowing up the layout. Omitted (= `none`) for normally-proportioned faces.
   */
  adj?: number;
  note?: string;
}

/** x-height / em to pin display faces to, close to Segoe UI's own. */
const TEXT_XH = 0.53;

const SANS = ", system-ui, sans-serif";
const MONO = ", ui-monospace, monospace";

export const FONTS: FontDef[] = [
  // --- System -------------------------------------------------------------
  {
    id: "system",
    label: "System UI",
    stack: '"Segoe UI", system-ui, sans-serif',
    group: "System",
    slots: ["ui", "num"],
    weights: "full",
    note: "Windows default",
  },
  {
    id: "system-mono",
    label: "System Mono",
    stack: 'ui-monospace, "Cascadia Mono", Consolas, monospace',
    group: "System",
    slots: ["num", "mono"],
    weights: "full",
    note: "Windows default",
  },

  // --- Sans ---------------------------------------------------------------
  {
    id: "inter",
    label: "Inter",
    stack: '"Inter Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "100-900",
  },
  {
    id: "ibm-plex-sans",
    label: "IBM Plex Sans",
    stack: '"IBM Plex Sans Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "100-700",
  },
  {
    id: "archivo",
    label: "Archivo",
    stack: '"Archivo Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "100-900",
  },
  {
    id: "roboto-condensed",
    label: "Roboto Condensed",
    stack: '"Roboto Condensed Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "100-900",
    note: "fits more text in a narrow window",
  },
  {
    id: "space-grotesk",
    label: "Space Grotesk",
    stack: '"Space Grotesk Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "300-700",
  },
  {
    id: "figtree",
    label: "Figtree",
    stack: '"Figtree Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "300-900",
  },
  {
    id: "manrope",
    label: "Manrope",
    stack: '"Manrope Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "200-800",
  },

  {
    id: "barlow-semi-condensed",
    label: "Barlow Semi Cond",
    stack: '"Barlow Semi Condensed"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "300-700",
    note: "narrow, fits long names",
  },
  {
    id: "titillium-web",
    label: "Titillium Web",
    stack: '"Titillium Web"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "300-700",
    note: "technical signage",
  },

  {
    id: "b612",
    label: "B612",
    stack: '"B612"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "400, 700",
    note: "the cockpit face, proportional",
  },
  {
    id: "sora",
    label: "Sora",
    stack: '"Sora Variable"' + SANS,
    group: "Sans",
    slots: ["ui", "num"],
    weights: "100-800",
    note: "geometric, even color",
  },

  // --- Techno / instrument ------------------------------------------------
  {
    id: "chakra-petch",
    label: "Chakra Petch",
    stack: '"Chakra Petch"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "300-700",
  },
  {
    id: "rajdhani",
    label: "Rajdhani",
    stack: '"Rajdhani"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "300-700",
    note: "squared gauge lettering",
  },
  {
    id: "oxanium",
    label: "Oxanium",
    stack: '"Oxanium Variable"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "200-800",
  },
  {
    id: "orbitron",
    label: "Orbitron",
    stack: '"Orbitron Variable"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "400-900",
    adj: TEXT_XH,
    note: "dashboard numerals",
  },
  {
    id: "teko",
    label: "Teko",
    stack: '"Teko Variable"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "300-700",
    adj: TEXT_XH,
    note: "tall condensed readout",
  },
  {
    id: "michroma",
    label: "Michroma",
    stack: '"Michroma"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "wide; the logo exploration face",
  },

  {
    id: "exo-2",
    label: "Exo 2",
    stack: '"Exo 2 Variable"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "100-900",
    note: "rounded techno",
  },
  {
    id: "saira",
    label: "Saira",
    stack: '"Saira Variable"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "100-900",
    note: "wide instrument sans",
  },
  {
    id: "tektur",
    label: "Tektur",
    stack: '"Tektur Variable"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "400-900",
    note: "squared, industrial",
  },
  {
    id: "jura",
    label: "Jura",
    stack: '"Jura Variable"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "300-700",
    note: "light, open counters",
  },
  {
    id: "jura-light",
    label: "Jura Light",
    stack: '"Jura Light"' + SANS,
    group: "Techno",
    slots: ["ui", "num"],
    weights: "300 only",
    note: "Jura held at its lightest, whatever the boldness",
  },
  {
    id: "big-shoulders",
    label: "Big Shoulders",
    stack: '"Big Shoulders Display Variable"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "100-900",
    adj: TEXT_XH,
    note: "very condensed display",
  },

  {
    id: "audiowide",
    label: "Audiowide",
    stack: '"Audiowide"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "wide speedway lettering",
  },
  {
    id: "bebas-neue",
    label: "Bebas Neue",
    stack: '"Bebas Neue"' + SANS,
    group: "Techno",
    slots: ["num"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "tall condensed caps",
  },

  // --- Mono ---------------------------------------------------------------
  {
    id: "jetbrains-mono",
    label: "JetBrains Mono",
    stack: '"JetBrains Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "100-800",
  },
  {
    id: "ibm-plex-mono",
    label: "IBM Plex Mono",
    stack: '"IBM Plex Mono"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "300-700",
  },
  {
    id: "roboto-mono",
    label: "Roboto Mono",
    stack: '"Roboto Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "100-700",
  },
  {
    id: "geist-mono",
    label: "Geist Mono",
    stack: '"Geist Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "100-900",
  },
  {
    id: "martian-mono",
    label: "Martian Mono",
    stack: '"Martian Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "100-800",
    note: "very wide",
  },
  {
    id: "b612-mono",
    label: "B612 Mono",
    stack: '"B612 Mono"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "400, 700",
    note: "designed for aircraft cockpit displays",
  },

  {
    id: "source-code-pro",
    label: "Source Code Pro",
    stack: '"Source Code Pro Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "200-900",
    note: "classic screen mono",
  },
  {
    id: "fira-code",
    label: "Fira Code",
    stack: '"Fira Code Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "300-700",
    note: "warm, wide mono",
  },
  {
    id: "red-hat-mono",
    label: "Red Hat Mono",
    stack: '"Red Hat Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "300-700",
    note: "neutral mono",
  },
  {
    id: "kode-mono",
    label: "Kode Mono",
    stack: '"Kode Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "400-700",
    note: "techno mono",
  },
  {
    id: "space-mono",
    label: "Space Mono",
    stack: '"Space Mono"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "400, 700",
    note: "quirky, retro-technical",
  },

  {
    id: "dm-mono",
    label: "DM Mono",
    stack: '"DM Mono"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "300-500",
    note: "light, quiet mono",
  },
  {
    id: "azeret-mono",
    label: "Azeret Mono",
    stack: '"Azeret Mono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "100-900",
    note: "square, high contrast",
  },
  {
    id: "sono",
    label: "Sono",
    stack: '"Sono Variable"' + MONO,
    group: "Mono",
    slots: ["ui", "num", "mono"],
    weights: "200-800",
    note: "soft rounded mono",
  },

  // --- Retro readout ------------------------------------------------------
  {
    id: "share-tech-mono",
    label: "Share Tech Mono",
    stack: '"Share Tech Mono"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "LCD panel",
  },
  {
    id: "doto",
    label: "Doto",
    stack: '"Doto"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "300-900",
    adj: TEXT_XH,
    note: "dot matrix; the logo face",
  },
  {
    id: "vt323",
    label: "VT323",
    stack: '"VT323"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "CRT terminal",
  },
  {
    id: "silkscreen",
    label: "Silkscreen",
    stack: '"Silkscreen"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400, 700",
    adj: TEXT_XH,
    note: "pixel bitmap",
  },
  {
    id: "handjet",
    label: "Handjet",
    stack: '"Handjet Variable"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "100-900",
    adj: TEXT_XH,
    note: "dot-matrix, adjustable",
  },
  {
    id: "workbench",
    label: "Workbench",
    stack: '"Workbench Variable"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "segment display",
  },
  {
    id: "pixelify-sans",
    label: "Pixelify Sans",
    stack: '"Pixelify Sans Variable"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400-700",
    adj: TEXT_XH,
    note: "pixel grid, readable",
  },
  {
    id: "nova-mono",
    label: "Nova Mono",
    stack: '"Nova Mono"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "angular, futuristic",
  },

  {
    id: "major-mono",
    label: "Major Mono",
    stack: '"Major Mono Display"' + MONO,
    group: "Readout",
    slots: ["num", "mono"],
    weights: "400 only",
    adj: TEXT_XH,
    note: "display face; capitals are decorative, so units read oddly",
  },
];

export const GROUP_ORDER = ["System", "Sans", "Techno", "Mono", "Readout"] as const;

/**
 * A named configuration: all three faces plus the weight and size trims that
 * make them sit together. Picking a face by itself is easy; picking three that
 * agree is not, so these are the combinations that already work.
 */
export interface FontSet {
  id: string;
  label: string;
  prefs: FontPrefs;
  /** Shipped with the app: applied and cloned, never edited or deleted. */
  factory: boolean;
}

const set = (
  id: string,
  label: string,
  ui: string,
  num: string,
  mono: string,
  font_weight = 0,
  size_ui = 1,
  size_num = 1,
  size_mono = 1,
): FontSet => ({
  id,
  label,
  factory: true,
  prefs: { font_ui: ui, font_num: num, font_mono: mono, font_weight, size_ui, size_num, size_mono },
});

/** The stored shape of a saved set, flattened, as the config file holds it. */
export interface StoredSet extends FontPrefs {
  id: string;
  label: string;
}

export function toStored(id: string, label: string, p: FontPrefs): StoredSet {
  return { id, label, ...p };
}

/** Saved sets, in the same shape as the shipped ones. */
export function userSets(stored: StoredSet[] | undefined): FontSet[] {
  return (stored ?? []).map((r) => ({
    id: r.id,
    label: r.label,
    factory: false,
    prefs: {
      font_ui: r.font_ui,
      font_num: r.font_num,
      font_mono: r.font_mono,
      font_weight: r.font_weight,
      size_ui: r.size_ui,
      size_num: r.size_num,
      size_mono: r.size_mono,
    },
  }));
}

/** A name not already taken, so two clones of one set stay tellable apart. */
export function freeLabel(base: string, taken: string[]): string {
  if (!taken.includes(base)) return base;
  for (let i = 2; ; i++) {
    const t = `${base} ${i}`;
    if (!taken.includes(t)) return t;
  }
}

export const FONT_SETS: FontSet[] = [
  set("stock", "Stock", "system", "system", "system-mono"),
  set("cockpit", "Cockpit", "chakra-petch", "rajdhani", "jetbrains-mono", 1, 1, 1.1),
  set("instrument", "Instrument", "saira", "oxanium", "geist-mono", 0, 1, 1.05),
  set("compact", "Compact", "barlow-semi-condensed", "barlow-semi-condensed", "martian-mono", 1, 1.05, 1.1),
  set("terminal", "Terminal", "ibm-plex-sans", "ibm-plex-mono", "ibm-plex-mono"),
  set("blueprint", "Blueprint", "titillium-web", "share-tech-mono", "source-code-pro"),
  set("dotmatrix", "Dot Matrix", "space-grotesk", "handjet", "space-mono", 1, 1, 1.15),
  set("arcade", "Arcade", "pixelify-sans", "silkscreen", "space-mono", 0, 0.95, 0.95),
  set("segment", "Segment", "exo-2", "workbench", "kode-mono", 0, 1, 1.1),
  set("crt", "CRT", "jura", "vt323", "nova-mono", 0, 1, 1.15),
  set("editorial", "Editorial", "figtree", "inter", "fira-code"),
  set("aviation", "Aviation", "b612", "b612-mono", "b612-mono"),
  set("telemetry", "Telemetry", "roboto-condensed", "orbitron", "roboto-mono", 0, 1, 0.95),
  set("speedway", "Speedway", "tektur", "audiowide", "kode-mono", 0, 1, 0.95),
  set("poster", "Poster", "archivo", "bebas-neue", "azeret-mono", 1, 1, 1.15),
  set("studio", "Studio", "sora", "sora", "dm-mono"),
  set("softline", "Softline", "manrope", "manrope", "sono"),
  set("ledger", "Ledger", "inter", "jetbrains-mono", "jetbrains-mono", 0, 1, 0.95),
  set("scope", "Oscilloscope", "jura", "jura-light", "source-code-pro", 0, 1, 1.05),
  set("tower", "Tower", "saira", "teko", "martian-mono", 0, 1, 1.1),
];

/** The set whose every value matches these prefs, if any. */
export function matchSet(p: FontPrefs, sets: FontSet[] = FONT_SETS): string | null {
  const keys = Object.keys(DEFAULT_FONTS) as (keyof FontPrefs)[];
  return sets.find((s) => keys.every((k) => s.prefs[k] === p[k]))?.id ?? null;
}

export const SLOTS: { id: Slot; label: string; hint: string }[] = [
  { id: "ui", label: "Interface", hint: "titles, labels, menus" },
  { id: "num", label: "Numbers", hint: "context, spend, percent, sizes" },
  { id: "mono", label: "Mono", hint: "message bodies, errors" },
];

// Out of the box the app wears the Oscilloscope set rather than the system
// faces: it is a gauge, and it should look like one before anyone opens the
// picker. Keep this in step with FONT_SETS' "scope" entry and with the Rust
// config defaults, which have to agree for a fresh install.
export const DEFAULT_FONTS = {
  font_ui: "jura",
  font_num: "jura-light",
  font_mono: "source-code-pro",
  font_weight: 0,
  size_ui: 1,
  size_num: 1.05,
  size_mono: 1,
};

/**
 * Size trim: a multiplier on every size in the slot, centered on 1 so the
 * control reads as an offset from standard rather than an absolute size. Most
 * faces need nothing; a few just need a nudge to sit right next to the others.
 */
export const SIZE_MIN = 0.5;
export const SIZE_MAX = 1.5;

/** "+10%", "-5%", or "standard" at 1. */
export function sizeLabel(v: number): string {
  const pct = Math.round((clampSize(v) - 1) * 100);
  return pct === 0 ? "standard" : (pct > 0 ? "+" : "") + pct + "%";
}

export const SIZE_KEY = {
  ui: "size_ui",
  num: "size_num",
  mono: "size_mono",
} as const;

export interface FontPrefs {
  font_ui: string;
  font_num: string;
  font_mono: string;
  /** Boldness preference, -2 (lighter) .. +2 (bolder); each step is 100. */
  font_weight: number;
  /** Per-slot size trim, 0.5 .. 1.5. */
  size_ui: number;
  size_num: number;
  size_mono: number;
}

/**
 * Ids of faces installed on the machine carry this prefix; everything after it
 * is the family name Windows resolves. They are not part of the bundled
 * catalog, so they are built on demand rather than listed in FONTS.
 */
export const SYS_PREFIX = "sys:";

export function systemFont(family: string, slot: Slot): FontDef {
  return {
    id: SYS_PREFIX + family,
    label: family,
    // Quoted so family names with spaces or digits resolve, with the usual
    // fallback behind them in case the font is uninstalled later.
    stack: `"${family}"` + (slot === "mono" ? MONO : SANS),
    group: "Installed",
    slots: ["ui", "num", "mono"],
    weights: "varies",
  };
}

export function fontById(id: string, slot: Slot): FontDef {
  if (id?.startsWith(SYS_PREFIX)) return systemFont(id.slice(SYS_PREFIX.length), slot);
  const hit = FONTS.find((f) => f.id === id && f.slots.includes(slot));
  if (hit) return hit;
  const fallback = slot === "mono" ? "system-mono" : "system";
  return FONTS.find((f) => f.id === fallback)!;
}

export function fontsFor(slot: Slot): FontDef[] {
  return FONTS.filter((f) => f.slots.includes(slot));
}

/** The five weight rungs the whole interface names, shifted by the preference. */
export function ladder(shift: number): number[] {
  const s = Math.max(-2, Math.min(2, Math.round(shift || 0))) * 100;
  return [300, 400, 500, 600, 700].map((w) => Math.max(100, Math.min(900, w + s)));
}

export function clampSize(v: number | undefined): number {
  const n = Number(v);
  return Number.isFinite(n) ? Math.max(SIZE_MIN, Math.min(SIZE_MAX, n)) : 1;
}

const RUNGS = ["--w-light", "--w-regular", "--w-medium", "--w-semibold", "--w-bold"];

export const BOLDNESS_LABELS = ["Lightest", "Lighter", "Normal", "Bolder", "Boldest"];

/**
 * Apply the saved preferences in a window and keep following changes. Every
 * window entry calls this; the picker broadcasts "fonts" when a face or the
 * boldness changes, so a pick lands everywhere at once with no restart.
 */
export async function initFonts(): Promise<void> {
  try {
    applyFonts(await invoke<FontPrefs>("get_config"));
  } catch {
    // No backend (or an older config): the CSS defaults already stand.
  }
  void listen<FontPrefs>("fonts", (e) => applyFonts(e.payload));
}

/** Write the slot + weight preferences onto <html> as CSS variables. */
export function applyFonts(p: Partial<FontPrefs>, root?: HTMLElement): void {
  const el = root ?? document.documentElement;
  const set = (slot: Slot, id: string | undefined) => {
    const f = fontById(id ?? "", slot);
    el.style.setProperty(`--font-${slot}`, f.stack);
    el.style.setProperty(`--font-${slot}-adj`, f.adj != null ? String(f.adj) : "none");
  };
  set("ui", p.font_ui);
  set("num", p.font_num);
  set("mono", p.font_mono);
  for (const s of ["ui", "num", "mono"] as Slot[]) {
    const v = p[SIZE_KEY[s]];
    el.style.setProperty(`--size-${s}`, String(clampSize(v)));
  }
  ladder(p.font_weight ?? 0).forEach((w, i) => el.style.setProperty(RUNGS[i], String(w)));
}
