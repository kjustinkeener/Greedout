import { describe, it, expect, beforeEach } from "vitest";
import { get } from "svelte/store";
import {
  resolveTheme,
  gaugeColor,
  spendFor,
  spendOverride,
  applyTheme,
  previewTheme,
  themeTick,
  THEMES,
} from "../theme";

// In jsdom getComputedStyle returns "" for the --g0/--g1/--g2 custom properties,
// so gaugeColor falls back to its hard-coded stop table:
//   0 -> #00ffe8, 0.52 -> #fffe00, 1 -> #ff0000
const CYAN = "rgb(0,255,232)";
const RED = "rgb(255,0,0)";
const YELLOW = "rgb(255,254,0)";

describe("resolveTheme", () => {
  it("keeps a known theme id unchanged", () => {
    expect(resolveTheme("dark")).toBe("dark");
    expect(resolveTheme("dracula")).toBe("dracula");
    // Every id in the registry must resolve to itself.
    for (const t of THEMES) expect(resolveTheme(t.id)).toBe(t.id);
  });

  it("falls back to 'auto' for unknown / empty / nullish ids", () => {
    // A stale config id must never leave the app on no palette at all.
    expect(resolveTheme("totally-made-up")).toBe("auto");
    expect(resolveTheme("")).toBe("auto");
    expect(resolveTheme(null)).toBe("auto");
    expect(resolveTheme(undefined)).toBe("auto");
  });
});

describe("gaugeColor", () => {
  it("returns the fallback stops at the endpoints", () => {
    expect(gaugeColor(0)).toBe(CYAN);
    expect(gaugeColor(1)).toBe(RED);
    expect(gaugeColor(0.52)).toBe(YELLOW);
  });

  it("clamps out-of-range fractions to [0,1]", () => {
    expect(gaugeColor(-5)).toBe(gaugeColor(0));
    expect(gaugeColor(2)).toBe(gaugeColor(1));
  });

  it("interpolates within a segment (always a valid rgb() triple)", () => {
    const mid = gaugeColor(0.26); // halfway between stop 0 and stop 0.52
    const m = mid.match(/^rgb\((\d+),(\d+),(\d+)\)$/);
    expect(m).not.toBeNull();
    const [r, g, b] = m!.slice(1).map(Number);
    for (const c of [r, g, b]) expect(c).toBeGreaterThanOrEqual(0);
    for (const c of [r, g, b]) expect(c).toBeLessThanOrEqual(255);
    // Between cyan (0,255,232) and yellow (255,254,0): red rises, blue falls.
    expect(r).toBeGreaterThan(0);
    expect(r).toBeLessThan(255);
    expect(b).toBeLessThan(232);
    expect(b).toBeGreaterThan(0);
  });
});

describe("spend color helpers", () => {
  it("spendOverride is empty when no --spend-fg is set (dark palettes)", () => {
    expect(spendOverride()).toBe("");
  });

  it("spendFor uses the muted var for a null fraction, else the gauge scale", () => {
    expect(spendFor(null)).toBe("var(--muted)");
    expect(spendFor(0)).toBe(gaugeColor(0));
    expect(spendFor(1)).toBe(gaugeColor(1));
  });
});

describe("applyTheme / previewTheme", () => {
  beforeEach(() => {
    document.documentElement.removeAttribute("data-theme");
  });

  it("stamps a known theme on the root and bumps themeTick", () => {
    const before = get(themeTick);
    applyTheme("dracula");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dracula");
    expect(get(themeTick)).toBe(before + 1);
  });

  it("normalizes an unknown theme to 'auto' on the root", () => {
    applyTheme("nope-not-real");
    expect(document.documentElement.getAttribute("data-theme")).toBe("auto");
  });

  it("applyTheme mirrors to localStorage; previewTheme does not", () => {
    localStorage.removeItem("greedout:theme");
    applyTheme("forest");
    expect(localStorage.getItem("greedout:theme")).toBe("forest");
    // A preview must not overwrite the remembered choice.
    previewTheme("amber");
    expect(document.documentElement.getAttribute("data-theme")).toBe("amber");
    expect(localStorage.getItem("greedout:theme")).toBe("forest");
  });
});
