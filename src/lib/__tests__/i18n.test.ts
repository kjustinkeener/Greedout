import { describe, it, expect, vi, beforeAll } from "vitest";

// i18n calls invoke/listen for persistence and cross-window sync; neither exists
// under jsdom. Stub them so setLocale() doesn't reject noisily.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(async () => undefined) }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import { t, resolveLocale, setLocale, activeLocale } from "../i18n.svelte";
import { en } from "../locales/en";
import { CATALOGS } from "../locales";

describe("resolveLocale (three-pass matching)", () => {
  it("matches an exact tag before anything else", () => {
    expect(resolveLocale(["en"])).toBe("en");
    expect(resolveLocale(["pt-BR"])).toBe("pt-BR");
    expect(resolveLocale(["zh-Hans"])).toBe("zh-Hans");
  });

  it("maps a region tag to its script via REGION_SCRIPT before falling to base", () => {
    // Windows reports Taiwan as zh-TW, which must land on Traditional, not on
    // whichever zh-* catalog happens to be listed first.
    expect(resolveLocale(["zh-TW"])).toBe("zh-Hant");
    expect(resolveLocale(["zh-HK"])).toBe("zh-Hant");
    expect(resolveLocale(["zh-CN"])).toBe("zh-Hans");
  });

  it("is case-insensitive on tags and script subtags", () => {
    expect(resolveLocale(["ZH-tw"])).toBe("zh-Hant");
    expect(resolveLocale(["PT-br"])).toBe("pt-BR");
  });

  it("falls back to the base language when no exact/region match exists", () => {
    expect(resolveLocale(["fr-CA"])).toBe("fr");
    expect(resolveLocale(["de-AT"])).toBe("de");
    // pt-PT has no exact catalog, but base 'pt' matches the pt-BR catalog.
    expect(resolveLocale(["pt-PT"])).toBe("pt-BR");
  });

  it("prefers an earlier preference over a later one", () => {
    expect(resolveLocale(["de", "fr"])).toBe("de");
    // Unknown first, known second.
    expect(resolveLocale(["xx", "fr"])).toBe("fr");
  });

  it("defaults to English for anything unmatched", () => {
    expect(resolveLocale(["xx-YY"])).toBe("en");
    expect(resolveLocale([])).toBe("en");
  });
});

describe("t (translate + interpolate + fallback)", () => {
  it("returns the raw key for a completely unknown key (visible bug report)", () => {
    // @ts-expect-error deliberately passing a non-existent key
    expect(t("this.key.does.not.exist")).toBe("this.key.does.not.exist");
  });

  it("interpolates named placeholders", () => {
    // en["main.renamePrompt"] = 'Rename "{title}"'
    expect(t("main.renamePrompt", { title: "Foo" })).toBe(en["main.renamePrompt"].replace("{title}", "Foo"));
    // Multiple slots.
    const limits = t("main.limits", { target: "200k", max: "1M" });
    expect(limits).toContain("200k");
    expect(limits).toContain("1M");
    expect(limits).not.toContain("{target}");
    expect(limits).not.toContain("{max}");
  });

  it("leaves an unmatched placeholder untouched (may be literal user text)", () => {
    expect(t("main.renamePrompt", {})).toBe(en["main.renamePrompt"]);
  });

  describe("English fallback for a key missing from the active locale", () => {
    beforeAll(async () => {
      // No shipped catalog is currently partial (all at 100%), so inject a
      // deliberately-incomplete locale to exercise the fallback branch. `t()`
      // reads whatever CATALOGS[active] resolves to, so this is the real path a
      // half-translated locale would take.
      CATALOGS["xx-test"] = { "menu.menu": "XX-MENU" } as unknown as (typeof CATALOGS)["en"];
      await setLocale("xx-test");
    });

    it("uses the translated string when the locale has the key", () => {
      expect(activeLocale()).toBe("xx-test");
      expect(t("menu.menu")).toBe("XX-MENU");
    });

    it("falls back to English (never the raw key) when the locale lacks the key", () => {
      // "menu.about" is absent from xx-test -> must render the English value.
      expect(t("menu.about")).toBe(en["menu.about"]);
      expect(t("menu.about")).not.toBe("menu.about");
    });
  });
});
