#!/usr/bin/env node
// Data-driven CSS theme palettes for Greedout.
//
// The theme palettes used to be ~68 hand-written :root[data-theme] blocks in
// src/app.css. They now live as data in src/lib/themes.json and are emitted to
// src/themes.generated.css by this script (build step, committed output).
//
// Subcommands:
//   generate  read src/lib/themes.json  -> write src/themes.generated.css
//   verify    parse the ORIGINAL palette region out of git (HEAD:src/app.css)
//             AND the generated CSS, build a canonical {context -> {prop:value}}
//             map for each, and assert deep equality. Prints THEMES_LOSSLESS_OK
//             (exit 0) on match, a precise diff (exit 1) on mismatch. The gate.
//   extract   (authoring aid) parse the palette region out of the current
//             src/app.css + labels/groups out of src/lib/theme.ts -> rewrite
//             src/lib/themes.json. Not part of the build; themes.json is the
//             committed source of truth once authored.
//
// No dependencies beyond Node builtins.

import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");
const P = {
  appCss: resolve(repo, "src/app.css"),
  themeTs: resolve(repo, "src/lib/theme.ts"),
  json: resolve(repo, "src/lib/themes.json"),
  outCss: resolve(repo, "src/themes.generated.css"),
};

// --- CSS parsing ------------------------------------------------------------

// Strip /* ... */ comments (the palette region has no strings that could hold a
// literal "/*", so a plain scan is safe here).
function stripComments(css) {
  return css.replace(/\/\*[\s\S]*?\*\//g, "");
}

// Walk a CSS string and yield flat { context, decls } rules. `context` is the
// normalized selector, prefixed with any wrapping at-rule (e.g. a @media). Only
// one level of at-rule nesting occurs in this data, which is all we handle.
function parseRules(css, prefix = "") {
  const rules = [];
  let i = 0;
  const n = css.length;
  while (i < n) {
    // read a prelude up to the next '{' or '}'
    let start = i;
    while (i < n && css[i] !== "{" && css[i] !== "}") i++;
    if (i >= n) break;
    if (css[i] === "}") {
      i++;
      continue;
    }
    const prelude = css.slice(start, i).trim();
    // find matching close brace for this block
    let depth = 1;
    let bodyStart = i + 1;
    i++;
    while (i < n && depth > 0) {
      if (css[i] === "{") depth++;
      else if (css[i] === "}") depth--;
      if (depth === 0) break;
      i++;
    }
    const body = css.slice(bodyStart, i);
    i++; // consume closing '}'
    if (prelude.startsWith("@media")) {
      const inner = parseRules(body, normWs(prelude) + " ");
      rules.push(...inner);
    } else if (prelude.startsWith("@")) {
      // no other at-rules expected in the palette region; ignore body
    } else {
      rules.push({ context: prefix + normSelector(prelude), decls: parseDecls(body) });
    }
  }
  return rules;
}

function normWs(s) {
  return s.replace(/\s+/g, " ").trim();
}

// Normalize a selector list: collapse whitespace, drop spaces around commas.
function normSelector(sel) {
  return normWs(sel)
    .split(",")
    .map((s) => s.trim())
    .join(", ");
}

// Parse a declaration body into { prop: value }. Last declaration of a prop
// wins, mirroring the CSS cascade, and values are trimmed + whitespace-collapsed
// so byte differences (spacing, ordering) do not register as changes.
function parseDecls(body) {
  const out = {};
  for (const chunk of body.split(";")) {
    const c = chunk.trim();
    if (!c) continue;
    const idx = c.indexOf(":");
    if (idx < 0) continue;
    const prop = c.slice(0, idx).trim();
    const val = normWs(c.slice(idx + 1));
    out[prop] = val;
  }
  return out;
}

// Build the canonical comparison map: { context -> {prop: value} }, merging
// repeated contexts (e.g. the two :root[data-theme="light"] blocks) with
// last-wins semantics, exactly as the browser would resolve them.
function canonMap(css) {
  const map = {};
  for (const { context, decls } of parseRules(stripComments(css))) {
    map[context] = Object.assign(map[context] || {}, decls);
  }
  return map;
}

// --- region slicing ---------------------------------------------------------

// Slice the palette region out of an app.css string: from the first
// :root[data-theme="light"] block through just before the "* {" reset. Captures
// the light/auto one-liners, the @media(auto) block, the full light block and
// every theme block (comments included; the parser strips them).
function sliceRegion(css) {
  const start = css.indexOf(':root[data-theme="light"]');
  if (start < 0) throw new Error("could not find palette region start");
  const reset = css.indexOf("\n* {", start);
  if (reset < 0) throw new Error("could not find reset block (* {) after palettes");
  return css.slice(start, reset);
}

// --- extract (authoring) ----------------------------------------------------

// Map id -> {label, group} from the THEMES array in theme.ts.
function readThemeMeta() {
  const ts = readFileSync(P.themeTs, "utf8");
  const arr = ts.slice(ts.indexOf("export const THEMES"));
  const body = arr.slice(arr.indexOf("["), arr.indexOf("];") + 1);
  const meta = {};
  const re = /\{\s*id:\s*"([^"]+)",\s*label:\s*"([^"]+)",\s*group:\s*"([^"]+)"\s*\}/g;
  let m;
  while ((m = re.exec(body))) meta[m[1]] = { label: m[2], group: m[3] };
  return meta;
}

const TOKEN_MAP = [
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

function declsToColors(d) {
  const colors = {};
  const bg = d["--bg-rgb"];
  if (bg) colors.bg = bg.split(",").map((x) => parseInt(x.trim(), 10));
  for (const [css, key] of TOKEN_MAP) if (d[css] != null) colors[key] = d[css];
  return colors;
}

function extract() {
  const css = readFileSync(P.appCss, "utf8");
  const region = sliceRegion(css);
  const meta = readThemeMeta();

  // Parse the region WITH comments so we can attach each theme block's note.
  // Split into segments on top-level rule/media boundaries by re-scanning with
  // comment tracking. Simpler: strip comments for decls, but capture the
  // preceding comment per data-theme block via regex over the raw region.
  const noteFor = {};
  {
    // match an optional /* ... */ immediately before a :root[data-theme="ID"] {
    const re = /\/\*([\s\S]*?)\*\/\s*:root\[data-theme="([^"]+)"\]\s*\{/g;
    let m;
    while ((m = re.exec(region))) noteFor[m[2]] = normWs(m[1]);
  }

  const rules = parseRules(stripComments(region));

  // Collect the full palettes.
  const autoMedia = rules.find(
    (r) => r.context === '@media (prefers-color-scheme: light) :root[data-theme="auto"]'
  );
  // The full light block is the :root[data-theme="light"] rule that carries a
  // palette (the one-liner only has color-scheme).
  const lightFull = rules.find(
    (r) => r.context === ':root[data-theme="light"]' && r.decls["--bg-rgb"]
  );
  if (!autoMedia || !lightFull) throw new Error("missing auto media or light palette");

  // Verify auto's media palette equals light's palette (so auto can be emitted
  // from light without storing its own colors).
  const a = JSON.stringify(autoMedia.decls);
  const l = JSON.stringify(lightFull.decls);
  const autoEqualsLight = a === l;

  const entries = [];
  // auto: no colors, emitted from light.
  entries.push({
    id: "auto",
    label: meta.auto?.label ?? "Auto (system)",
    group: meta.auto?.group ?? "Core",
    scheme: "light dark",
    autoLightFrom: "light",
    note: null,
  });
  // light: full palette, scheme light.
  entries.push({
    id: "light",
    label: meta.light?.label ?? "Light",
    group: meta.light?.group ?? "Core",
    scheme: "light",
    note: noteFor.light ?? null,
    colors: declsToColors(lightFull.decls),
    gradient: [lightFull.decls["--g0"], lightFull.decls["--g1"], lightFull.decls["--g2"]],
    spendFg: lightFull.decls["--spend-fg"] ?? null,
    spendShadow: lightFull.decls["--spend-shadow"] ?? null,
  });

  // Every other theme block, in appearance order. Skip the light/auto contexts.
  const seen = new Set(["light", "auto"]);
  for (const r of rules) {
    const m = r.context.match(/^:root\[data-theme="([^"]+)"\]$/);
    if (!m) continue;
    const id = m[1];
    if (seen.has(id)) continue;
    if (!r.decls["--bg-rgb"]) continue; // skip color-scheme-only one-liners
    seen.add(id);
    entries.push({
      id,
      label: meta[id]?.label ?? id,
      group: meta[id]?.group ?? "",
      scheme: r.decls["color-scheme"] ?? "dark",
      note: noteFor[id] ?? null,
      colors: declsToColors(r.decls),
      gradient: [r.decls["--g0"], r.decls["--g1"], r.decls["--g2"]],
      spendFg: r.decls["--spend-fg"] ?? null,
      spendShadow: r.decls["--spend-shadow"] ?? null,
    });
  }

  writeFileSync(P.json, JSON.stringify(entries, null, 2) + "\n");
  console.log(
    `extracted ${entries.length} entries -> src/lib/themes.json ` +
      `(auto media == light palette: ${autoEqualsLight})`
  );
  if (!autoEqualsLight) {
    console.error(
      "WARNING: auto's @media palette differs from the light palette. auto should " +
        "store its own colors; adjust the schema."
    );
    process.exit(2);
  }
}

// --- generate ---------------------------------------------------------------

const BANNER =
  "/* GENERATED FILE - do not edit by hand.\n" +
  "   Source of truth: src/lib/themes.json. Regenerate with:\n" +
  "     node tools/gen-themes.mjs generate\n" +
  "   Verify losslessness against git HEAD's app.css with:\n" +
  "     node tools/gen-themes.mjs verify\n" +
  "\n" +
  "   Light-mode spend color: in light mode the spend amounts read as dark grey\n" +
  "   with no halo (the colored scale is kept for the gauges). A light theme sets\n" +
  "   --spend-fg and --spend-shadow; spendOverride() in theme.ts prefers --spend-fg\n" +
  "   over the gauge scale, and the halo is a transparent shadow rather than a\n" +
  "   removed one, so a new light palette only has to set the variable. */";

// Emit the declaration body (indented `indent`) for a theme entry. Order:
// spend-fg/spend-shadow (light only) is emitted AFTER color-scheme here; source
// order varied per block but computed style does not depend on it. Shared so
// build-time and any future runtime injection cannot drift.
export function themeToCss(t, indent = "  ", opts = {}) {
  const c = t.colors || {};
  const lines = [];
  const push = (prop, val) => lines.push(`${indent}${prop}: ${val};`);
  if (!opts.omitScheme) push("color-scheme", t.scheme);
  if (t.spendFg != null) push("--spend-fg", t.spendFg);
  if (t.spendShadow != null) push("--spend-shadow", t.spendShadow);
  if (c.bg) push("--bg-rgb", c.bg.join(", "));
  for (const [css, key] of TOKEN_MAP) if (c[key] != null) push(css, c[key]);
  if (t.gradient) {
    push("--g0", t.gradient[0]);
    push("--g1", t.gradient[1]);
    push("--g2", t.gradient[2]);
  }
  return lines.join("\n");
}

function themeBlock(t, selector) {
  return `${selector} {\n${themeToCss(t)}\n}`;
}

function generate() {
  const entries = JSON.parse(readFileSync(P.json, "utf8"));
  const byId = Object.fromEntries(entries.map((e) => [e.id, e]));
  const light = byId.light;
  const auto = byId.auto;
  if (!light) throw new Error("themes.json missing 'light' entry");
  if (!auto) throw new Error("themes.json missing 'auto' entry");

  const out = [BANNER];

  // light + auto color-scheme one-liners.
  out.push(`:root[data-theme="light"] {\n  color-scheme: ${light.scheme};\n}`);
  out.push(`:root[data-theme="auto"] {\n  color-scheme: ${auto.scheme};\n}`);

  // auto's @media(prefers-color-scheme: light) palette, sourced from light's
  // colors (light dark scheme replaced by light inside the media block).
  // color-scheme for auto is carried by the one-liner above, not the media
  // block (matching the original), so omit it here.
  const inner = themeToCss(light, "    ", { omitScheme: true });
  out.push(
    `@media (prefers-color-scheme: light) {\n` +
      `  :root[data-theme="auto"] {\n${inner}\n  }\n}`
  );

  // full light block.
  out.push(themeBlock(light, ':root[data-theme="light"]'));

  // every other theme, in themes.json order.
  for (const t of entries) {
    if (t.id === "light" || t.id === "auto") continue;
    if (t.note) out.push(`/* ${t.note} */`);
    out.push(themeBlock(t, `:root[data-theme="${t.id}"]`));
  }

  writeFileSync(P.outCss, out.join("\n\n") + "\n");
  console.log(`generated src/themes.generated.css (${entries.length} entries)`);
}

// --- verify -----------------------------------------------------------------

function gitOriginalAppCss() {
  const out = execFileSync("git", ["-C", repo, "show", "HEAD:src/app.css"], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  return out;
}

function verify() {
  const original = sliceRegion(gitOriginalAppCss());
  const generated = readFileSync(P.outCss, "utf8");

  const mapA = canonMap(original);
  const mapB = canonMap(generated);

  const contexts = new Set([...Object.keys(mapA), ...Object.keys(mapB)]);
  const diffs = [];
  for (const ctx of contexts) {
    const A = mapA[ctx];
    const B = mapB[ctx];
    if (!A) {
      diffs.push(`context only in GENERATED: ${ctx}`);
      continue;
    }
    if (!B) {
      diffs.push(`context only in ORIGINAL: ${ctx}`);
      continue;
    }
    const props = new Set([...Object.keys(A), ...Object.keys(B)]);
    for (const p of props) {
      if (A[p] !== B[p]) {
        diffs.push(
          `[${ctx}] ${p}: original=${JSON.stringify(A[p])} generated=${JSON.stringify(B[p])}`
        );
      }
    }
  }

  if (diffs.length) {
    console.error("THEMES_MISMATCH");
    for (const d of diffs) console.error("  " + d);
    process.exit(1);
  }
  console.log(`THEMES_LOSSLESS_OK (${contexts.size} selector contexts compared)`);
}

// --- main -------------------------------------------------------------------

const cmd = process.argv[2];
if (cmd === "extract") extract();
else if (cmd === "generate") generate();
else if (cmd === "verify") verify();
else {
  console.error("usage: node tools/gen-themes.mjs <extract|generate|verify>");
  process.exit(64);
}
