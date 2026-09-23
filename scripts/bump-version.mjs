#!/usr/bin/env node
// Single source of truth for the version: package.json / tauri.conf.json / Cargo.toml / Cargo.lock (opencapx) kept in sync.
//   node scripts/bump-version.mjs              # derive the next version from the commits since the last v* tag, write all four
//   node scripts/bump-version.mjs --dry-run    # derive and print, write nothing
//   node scripts/bump-version.mjs --notes      # also print the commits the derivation saw
//   node scripts/bump-version.mjs 0.5.0        # write an explicit version to all four (skips the derivation)
//   node scripts/bump-version.mjs --check      # check consistency (for CI), exit 1 on mismatch
// The derivation anchors on the last v* tag (the released state), so re-running it before tagging is
// idempotent. Pre-1.0 policy: a breaking change bumps the minor while the major is 0 — 1.0.0 stays a
// deliberate call (pass an explicit version). The full release sequence is in docs/release.md §3.
import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
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

const FLAGS = ["--check", "--dry-run", "--notes"];
const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const notes = args.includes("--notes");
const positional = args.filter((a) => !a.startsWith("--"));
const unknown = args.filter((a) => a.startsWith("--") && !FLAGS.includes(a));

function usage(why) {
  if (why) console.error(why);
  console.error("usage: node scripts/bump-version.mjs [--dry-run] [--notes] | <x.y.z> [--dry-run] | --check");
  process.exit(2);
}

function writeAll(version) {
  for (const [name, path] of Object.entries(files)) {
    const text = readFileSync(path, "utf8");
    if (dryRun) {
      console.log(`${name} -> ${version} (dry run)`);
      continue;
    }
    writeFileSync(path, replaceVersion(name, text, version));
    console.log(`${name} -> ${version}`);
  }
}

function bump(current, releaseType) {
  const [major, minor, patch] = current.split(".").map(Number);
  if (releaseType === "major") return `${major + 1}.0.0`;
  if (releaseType === "minor") return `${major}.${minor + 1}.0`;
  return `${major}.${minor}.${patch + 1}`;
}

async function derive() {
  let lastTag;
  try {
    lastTag = execFileSync("git", ["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"], {
      cwd: root,
      encoding: "utf8",
    }).trim();
  } catch {
    console.error(
      "no v* release tag reachable from HEAD — pass an explicit version: node scripts/bump-version.mjs <x.y.z>",
    );
    process.exit(1);
  }

  const released = lastTag.replace(/^v/, "");
  if (!/^\d+\.\d+\.\d+$/.test(released)) {
    console.error(
      `last tag ${lastTag} is not a plain vX.Y.Z release tag — pass an explicit version: node scripts/bump-version.mjs <x.y.z>`,
    );
    process.exit(1);
  }

  const fileVersion = readVersion("package.json", readFileSync(files["package.json"], "utf8"));
  if (!fileVersion) {
    console.error("cannot read the current version from package.json");
    process.exit(1);
  }
  if (fileVersion !== released) {
    console.log(`note: the version files say ${fileVersion}, the last tag is ${lastTag} — deriving from the tag`);
  }

  let Bumper;
  try {
    ({ Bumper } = await import("conventional-recommended-bump"));
  } catch {
    console.error("conventional-recommended-bump is not installed — run pnpm install");
    process.exit(1);
  }

  const result = await new Bumper(root).loadPreset("conventionalcommits").tag({ prefix: "v" }).bump();

  if (notes) {
    console.log(`commits since ${lastTag} (${result.commits.length}):`);
    for (const commit of result.commits) console.log(`- ${commit.header}`);
  }

  if (!result.releaseType) {
    console.log(`no releasable changes since ${lastTag} (${result.commits.length} commit(s), none with a bump)`);
    return;
  }

  const [major] = released.split(".").map(Number);
  const capped = result.releaseType === "major" && major === 0;
  const releaseType = capped ? "minor" : result.releaseType;
  const next = bump(released, releaseType);
  console.log(`derived from ${lastTag}: ${released} -> ${next} (${releaseType}; ${result.reason})`);
  if (capped) console.log("note: breaking change capped at minor while pre-1.0 (pass an explicit version to cut 1.0.0)");
  writeAll(next);
  if (!dryRun) console.log(`next: update CHANGELOG.md, then commit "chore(release): ${next}" and tag v${next}`);
}

if (unknown.length) usage(`unknown flag: ${unknown.join(", ")}`);

if (args.includes("--check")) {
  if (args.length > 1) usage("--check runs alone");
  const seen = Object.entries(files).map(([name, path]) => [name, readVersion(name, readFileSync(path, "utf8"))]);
  const versions = new Set(seen.map(([, v]) => v));
  if (versions.size !== 1 || versions.has(null)) {
    for (const [name, v] of seen) console.error(`  ${name}: ${v ?? "<not found>"}`);
    console.error("version mismatch — run: node scripts/bump-version.mjs <x.y.z>, or with no argument to derive it");
    process.exit(1);
  }
  console.log(`version consistent: ${[...versions][0]}`);
  process.exit(0);
}

if (positional.length > 1) usage();

const arg = positional[0];
if (!arg) {
  await derive();
  process.exit(0);
}

if (notes) usage("--notes only applies to the derived path (omit the explicit version)");
if (!/^\d+\.\d+\.\d+$/.test(arg)) {
  console.error(`invalid version: ${arg} (expect x.y.z)`);
  process.exit(2);
}
writeAll(arg);
if (!dryRun) console.log(`next: update CHANGELOG.md, then commit "chore(release): ${arg}" and tag v${arg}`);
