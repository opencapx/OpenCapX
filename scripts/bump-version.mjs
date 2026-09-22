#!/usr/bin/env node
// Single source of truth for the version: package.json / tauri.conf.json / Cargo.toml / Cargo.lock (opencapx) kept in sync.
//   node scripts/bump-version.mjs 0.5.0   # write to all four
//   node scripts/bump-version.mjs --check # check consistency (for CI), exit 1 on mismatch
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const files = {
  "package.json": join(root, "package.json"),
  "src-tauri/tauri.conf.json": join(root, "src-tauri/tauri.conf.json"),
  "src-tauri/Cargo.toml": join(root, "src-tauri/Cargo.toml"),
  "src-tauri/Cargo.lock": join(root, "src-tauri/Cargo.lock"),
};

function readVersion(name, text) {
  if (name.endsWith("Cargo.lock")) {
    const m = text.match(/\[\[package\]\]\nname = "opencapx"\nversion = "([^"]+)"/);
    return m?.[1] ?? null;
  }
  const m = text.match(/"version":\s*"([^"]+)"/) ?? text.match(/^version = "([^"]+)"/m);
  return m?.[1] ?? null;
}

function replaceVersion(name, text, next) {
  if (name.endsWith("Cargo.lock")) {
    return text.replace(
      /(\[\[package\]\]\nname = "opencapx"\nversion = ")[^"]+(")/,
      `$1${next}$2`,
    );
  }
  if (name.endsWith("Cargo.toml")) {
    return text.replace(/^version = "[^"]+"/m, `version = "${next}"`);
  }
  return text.replace(/"version":\s*"[^"]+"/, `"version": "${next}"`);
}

const arg = process.argv[2];
if (!arg) {
  console.error("usage: node scripts/bump-version.mjs <x.y.z> | --check");
  process.exit(2);
}

if (arg === "--check") {
  const seen = Object.entries(files).map(([name, path]) => [name, readVersion(name, readFileSync(path, "utf8"))]);
  const versions = new Set(seen.map(([, v]) => v));
  if (versions.size !== 1 || versions.has(null)) {
    for (const [name, v] of seen) console.error(`  ${name}: ${v ?? "<not found>"}`);
    console.error("version mismatch — run: node scripts/bump-version.mjs <x.y.z>");
    process.exit(1);
  }
  console.log(`version consistent: ${[...versions][0]}`);
  process.exit(0);
}

if (!/^\d+\.\d+\.\d+$/.test(arg)) {
  console.error(`invalid version: ${arg} (expect x.y.z)`);
  process.exit(2);
}
for (const [name, path] of Object.entries(files)) {
  const text = readFileSync(path, "utf8");
  writeFileSync(path, replaceVersion(name, text, arg));
  console.log(`${name} -> ${arg}`);
}
