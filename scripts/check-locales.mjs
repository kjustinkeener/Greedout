// Coverage and placeholder gate for the translation catalogs.
//
// Three failure modes none of which the type checker or a look at the app in one
// language will catch:
//   1. An English key is renamed. Translations keep the old key, fall back to
//      English forever, and nobody notices. ORPHAN, hard fail.
//   2. A translation drops or mistypes a placeholder. The string renders with a
//      literal {versoin} in it or silently loses the value. Hard fail both ways.
//   3. A translation lags English. Legal, but it should be visible. Soft.
//
// Node strips TypeScript types natively, so the .ts catalogs import with no build.

import { readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, "..", "src", "lib", "locales");

const { en } = await import(pathToFileURL(join(dir, "en.ts")).href);
const keys = Object.keys(en);

// Tokens that are literal text a user reads, not slots to fill. A translation is
// expected to carry them through verbatim, so they are checked like any other.
const LITERAL_TOKENS = [];

const slots = (msg) => {
  const text = typeof msg === "string" ? msg : Object.values(msg).join(" ");
  return [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
};

const enSlots = Object.fromEntries(keys.map((k) => [k, slots(en[k])]));

let failed = false;
console.log(`en.ts: ${keys.length} keys.`);

for (const file of readdirSync(dir).sort()) {
  if (!file.endsWith(".ts") || file === "en.ts" || file === "index.ts") continue;
  const mod = await import(pathToFileURL(join(dir, file)).href);
  const cat = Object.values(mod)[0];
  const have = Object.keys(cat);

  const orphans = have.filter((k) => !keys.includes(k));
  const missing = keys.filter((k) => !have.includes(k));

  for (const k of orphans) {
    console.error(`  ${file}: ORPHAN key "${k}" (renamed or removed in en.ts)`);
    failed = true;
  }
  for (const k of have) {
    if (orphans.includes(k)) continue;
    const a = enSlots[k].join(",");
    const b = slots(cat[k]).join(",");
    if (a !== b) {
      console.error(`  ${file}: placeholder mismatch in "${k}": en {${a}} vs {${b}}`);
      failed = true;
    }
  }
  const pct = Math.round(((keys.length - missing.length) / keys.length) * 100);
  console.log(`  ${file.padEnd(14)} ${String(pct).padStart(3)}%  (${missing.length} missing)`);
  for (const k of missing) console.log(`      missing: ${k}`);
}

if (LITERAL_TOKENS.length) {
  console.log(`Literal (never-substituted) tokens: ${LITERAL_TOKENS.join(", ")}`);
}
if (failed) {
  console.error("Locale check FAILED.");
  process.exit(1);
}
console.log("Locale check passed.");
