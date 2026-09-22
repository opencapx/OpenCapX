#!/usr/bin/env node
// i18n tooling: validates every locale pack against en.json, and can add a new language in one command.
//
//   node scripts/i18n-check.mjs                  # validate key sets/placeholders; exit code 1 on mismatch
//   node scripts/i18n-check.mjs --report         # additionally print each language's untranslated count
//   node scripts/i18n-check.mjs --json           # machine-readable (CI)
//   node scripts/i18n-check.mjs --scaffold fr --name "Français"
//                                                # generate src/locales/fr.json (filled with English) + register the name
//
// Conventions in src/i18n.ts: en is the sole baseline; key sets must match exactly; placeholders `{x}` must match.

import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const LOCALES = join(ROOT, "src", "locales");
const BASE = "en";
const NAMES_FILE = join(LOCALES, "locales.json");

const args = process.argv.slice(2);
const has = (flag) => args.includes(flag);
const valueOf = (flag) => {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : undefined;
};

const readJson = (file) => JSON.parse(readFileSync(file, "utf8"));
const packFiles = () =>
  readdirSync(LOCALES)
    .filter((f) => f.endsWith(".json") && f !== "locales.json")
    .map((f) => f.slice(0, -".json".length))
    .sort();

const base = readJson(join(LOCALES, `${BASE}.json`));
const baseKeys = Object.keys(base);
const placeholders = (s) => (String(s).match(/\{[^}]+\}/g) ?? []).sort().join(",");

/** The differences of one locale pack relative to the baseline. */
function diff(code) {
  const pack = readJson(join(LOCALES, `${code}.json`));
  const keys = Object.keys(pack);
  const missing = baseKeys.filter((k) => !(k in pack));
  const extra = keys.filter((k) => !(k in base));
  const placeholderMismatch = baseKeys
    .filter((k) => k in pack && placeholders(base[k]) !== placeholders(pack[k]))
    .map((k) => ({ key: k, expected: placeholders(base[k]), got: placeholders(pack[k]) }));
  const untranslated = keys.filter((k) => pack[k] === base[k]);
  return { code, keys: keys.length, missing, extra, placeholderMismatch, untranslated };
}

function scaffold(code, name) {
  if (!code) {
    console.error("usage: --scaffold <code> [--name \"<self-name>\"]");
    process.exit(2);
  }
  const file = join(LOCALES, `${code}.json`);
  if (existsSync(file)) {
    console.error(`✗ ${file} already exists, not overwritten`);
    process.exit(2);
  }
  // Fill with English: the pack is usable immediately (missing translations show English); translators only replace values
  writeFileSync(file, `${JSON.stringify(base, null, 2)}\n`);
  const names = existsSync(NAMES_FILE) ? readJson(NAMES_FILE) : {};
  names[code] = name ?? code;
  writeFileSync(NAMES_FILE, `${JSON.stringify(names, null, 2)}\n`);
  console.log(`✓ generated src/locales/${code}.json (${baseKeys.length} keys, filled with English for now)`);
  console.log(`✓ registered the self-name in src/locales/locales.json: ${names[code]}`);
  console.log(`  next: replace the values to finish translating, then run npm run i18n:check`);
}

if (has("--scaffold")) {
  scaffold(valueOf("--scaffold"), valueOf("--name"));
  process.exit(0);
}

const codes = packFiles();
if (!codes.includes(BASE)) {
  console.error(`✗ missing baseline locale pack src/locales/${BASE}.json`);
  process.exit(1);
}

const results = codes.filter((c) => c !== BASE).map(diff);
const names = existsSync(NAMES_FILE) ? readJson(NAMES_FILE) : {};
const unregistered = codes.filter((c) => !(c in names));

if (has("--json")) {
  console.log(JSON.stringify({ base: BASE, baseKeys: baseKeys.length, results, unregistered }, null, 2));
} else {
  console.log(`baseline src/locales/${BASE}.json · ${baseKeys.length} keys · ${codes.length} languages\n`);
  for (const r of results) {
    const bad = r.missing.length + r.extra.length + r.placeholderMismatch.length;
    const mark = bad === 0 ? "✓" : "✗";
    console.log(`${mark} ${r.code.padEnd(10)} ${String(r.keys).padStart(4)} keys` + (bad ? `  missing ${r.missing.length} · extra ${r.extra.length} · placeholder mismatch ${r.placeholderMismatch.length}` : ""));
    if (has("--report") && r.untranslated.length > 0) {
      console.log(`   untranslated ${r.untranslated.length}/${r.keys} (value identical to English)`);
      console.log(`   e.g. ${r.untranslated.slice(0, 4).join(", ")}`);
    }
  }
  if (unregistered.length > 0) {
    console.log(`\n! not registered with a self-name in locales.json (the settings page will show the language code): ${unregistered.join(", ")}`);
  }
}

const failed =
  results.some((r) => r.missing.length || r.extra.length || r.placeholderMismatch.length) ||
  unregistered.length > 0;
if (failed) {
  console.error("\ninternationalization check failed: locale key sets/placeholders must match en.json exactly.");
  process.exit(1);
}
console.log("\ni18n check passed.");
