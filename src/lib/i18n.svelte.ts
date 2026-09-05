// Hand-rolled localization: a flat catalog per language and a `t()` that reads a
// rune, so any component calling it re-renders on a language change with no store
// subscription and no context provider. See the shared localization pattern.
//
// This file must keep the `.svelte.ts` extension. Runes only compile in `.svelte`
// and `.svelte.ts`; renamed to plain `.ts` the `$state` below becomes an undefined
// identifier and the error points somewhere else entirely.
//
// It deliberately calls `invoke` directly rather than going through a shared api
// layer, so the module plus `locales/` lifts into another app as a copy.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { en, type Dict, type PartialDict, type Msg, type Plural } from "./locales/en";
import { CATALOGS } from "./locales";

/** "auto" follows the OS; anything else is a tag from LOCALES. */
export type LocaleChoice = "auto" | string;

/** Every language is named in itself: someone who landed in the wrong one cannot
 *  read "Japanese" but can always find the row that says the language's own name.
 *  Never localize this list and never sort it by the English name. */
export const LOCALES = [
  { id: "en", label: "English", dir: "ltr" },
  { id: "de", label: "Deutsch", dir: "ltr" },
  { id: "es", label: "Español", dir: "ltr" },
  { id: "fr", label: "Français", dir: "ltr" },
  { id: "it", label: "Italiano", dir: "ltr" },
  { id: "nl", label: "Nederlands", dir: "ltr" },
  { id: "pl", label: "Polski", dir: "ltr" },
  { id: "pt-BR", label: "Português (Brasil)", dir: "ltr" },
  { id: "ru", label: "Русский", dir: "ltr" },
  { id: "tr", label: "Türkçe", dir: "ltr" },
  { id: "ja", label: "日本語", dir: "ltr" },
  { id: "ko", label: "한국어", dir: "ltr" },
  { id: "zh-Hans", label: "中文（简体）", dir: "ltr" },
  { id: "zh-Hant", label: "中文（繁體）", dir: "ltr" },
] as const;

/** Region tags do not always reduce to the right script. Windows reports Taiwan as
 *  `zh-TW`, never `zh-Hant`; stripping to `zh` would hand a Taiwanese user whichever
 *  `zh-*` catalog is listed first, which is the right language in the wrong writing
 *  system. Only the pairs actually shipped belong here. */
const REGION_SCRIPT: Record<string, string> = {
  "zh-tw": "zh-Hant",
  "zh-hk": "zh-Hant",
  "zh-mo": "zh-Hant",
  "zh-cn": "zh-Hans",
  "zh-sg": "zh-Hans",
};

let choice = $state<LocaleChoice>("auto");
let active = $state<string>("en");
let dict = $state<PartialDict>(en);

/** The user's saved choice, "auto" included. For the picker. */
export function localeChoice(): LocaleChoice {
  return choice;
}

/** The concrete tag in force. For Intl and for reporting back to the backend. */
export function activeLocale(): string {
  return active;
}

/** Translate. An unknown key returns the key: a visible `settings.title` in the UI
 *  is a bug report someone files, an empty string is a bug nobody notices. */
export function t(key: keyof Dict, vars?: Record<string, string | number>): string {
  const raw: Msg | undefined = dict[key] ?? en[key];
  if (raw === undefined) return key;
  const s = typeof raw === "string" ? raw : plural(raw, active, Number(vars?.count ?? 0));
  return interpolate(s, vars);
}

/** The categories are per-language and the platform already knows them: Japanese
 *  has one form, French counts 0 as singular, Russian has four. A `count === 1`
 *  is the most common way a fully translated app still reads as broken. */
function plural(forms: Plural, locale: string, count: number): string {
  const cat = new Intl.PluralRules(locale).select(count) as keyof Plural;
  return forms[cat] ?? forms.other;
}

/** Leaving an unmatched placeholder alone is load-bearing: a string may carry a
 *  literal token that is text the user reads, not a slot to fill. */
function interpolate(s: string, vars?: Record<string, string | number>): string {
  if (!vars) return s;
  return s.replace(/\{(\w+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
}

/** "A, B and C" in whatever shape the language uses. Never `.join(" and ")`. */
export function formatList(items: readonly string[]): string {
  return new Intl.ListFormat(active, { style: "long", type: "conjunction" }).format(
    items as string[],
  );
}

export type Chunk = { text: string } | { slot: string };

/** For the one sentence that wears markup. Three sibling strings would freeze
 *  English word order into the DOM and no translator would ever see the sentence
 *  whole; this keeps it as one catalog entry and splits it at render time. */
export function tSplit(key: keyof Dict, vars?: Record<string, string | number>): Chunk[] {
  const s = t(key, vars);
  const out: Chunk[] = [];
  let last = 0;
  for (const m of s.matchAll(/\{(\w+)\}/g)) {
    const i = m.index ?? 0;
    if (i > last) out.push({ text: s.slice(last, i) });
    out.push({ slot: m[1] });
    last = i + m[0].length;
  }
  if (last < s.length) out.push({ text: s.slice(last) });
  return out;
}

/** Three passes, not one. A single pass that accepts a base-language match on the
 *  first preferred tag hands a pt-BR user whichever pt-* catalog sits first. Exact
 *  regional wins everywhere before bare language is tried anywhere. Case-insensitive,
 *  because the OS and the webview disagree on the casing of script subtags. */
export function resolveLocale(preferred: readonly string[]): string {
  const ids = LOCALES.map((l) => l.id as string);
  const lower = new Map(ids.map((id) => [id.toLowerCase(), id]));
  for (const want of preferred) {
    const exact = lower.get(want.toLowerCase());
    if (exact) return exact;
  }
  for (const want of preferred) {
    const alias = REGION_SCRIPT[want.toLowerCase()];
    if (alias && CATALOGS[alias]) return alias;
  }
  for (const want of preferred) {
    const base = want.toLowerCase().split("-")[0];
    const hit = ids.find((id) => id.toLowerCase().split("-")[0] === base);
    if (hit) return hit;
  }
  return "en";
}

function apply(c: LocaleChoice) {
  choice = c;
  active = c === "auto" ? resolveLocale(navigator.languages ?? [navigator.language]) : c;
  dict = CATALOGS[active] ?? en;
  document.documentElement.lang = active;
  document.documentElement.dir = LOCALES.find((l) => l.id === active)?.dir ?? "ltr";
}

/** Read the saved choice and apply it BEFORE the first mount. Mounting first and
 *  loading the catalog after gives a frame of English on every single launch,
 *  which is the most-noticed bug in a localization feature and the cheapest to
 *  avoid. Also reports the resolved tag back, since the tray is built in `setup`
 *  before any window exists to ask what "auto" means. */
export async function initI18n(): Promise<void> {
  let saved: LocaleChoice = "auto";
  try {
    const cfg = await invoke<{ locale?: string }>("get_config");
    saved = (cfg.locale as LocaleChoice) ?? "auto";
  } catch {
    // No backend (or it failed): English is a working app, not a broken one.
  }
  apply(saved);
  try {
    await invoke("set_locale", { locale: choice, resolved: active });
  } catch {
    // Best effort; the tray keeps last run's labels.
  }
}

/** Change the language: apply here, persist, retranslate the tray, and tell the
 *  other windows. */
export async function setLocale(c: LocaleChoice): Promise<void> {
  apply(c);
  try {
    await invoke("set_locale", { locale: choice, resolved: active });
  } catch {
    // Ignore: the UI is already in the new language.
  }
}

/** Apply only. No persist and no tray rebuild, or every open window races to
 *  write the same setting. */
export function watchLocale(): Promise<UnlistenFn> {
  return listen<LocaleChoice>("settings:locale", (e) => apply(e.payload));
}
