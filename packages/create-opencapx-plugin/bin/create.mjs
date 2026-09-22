#!/usr/bin/env node
// create-opencapx-plugin — scaffold a TypeScript OpenCapX plugin (zero deps,
// Node standard library only). Run via:
//   npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
// or directly:
//   node packages/create-opencapx-plugin/bin/create.mjs --id com.acme.hello [--name "Hello"] [--author acme] [--license MIT] [--dir ./hello] [--local]
// id rules match Core's valid_plugin_id: charset [a-z0-9.-], no "..", no leading/trailing dot, ≤128.
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const templateDir = join(here, "..", "template");

function usage(exitCode) {
  console.error(
    'usage: npm create opencapx-plugin -- --id <reverse.dns.id> [--name "Name"] [--author "Your Name"] [--license MIT] [--dir <target>] [--local]',
  );
  process.exit(exitCode);
}

const args = {};
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === "--help" || a === "-h") usage(0);
  if (!a.startsWith("--")) usage(2);
  // --local is a value-less boolean flag; every other flag takes a value.
  if (a === "--local") {
    args.local = true;
    continue;
  }
  const val = argv[++i];
  if (val === undefined) usage(2);
  args[a.slice(2)] = val;
}

const id = args.id;
if (!id) usage(2);
if (
  !/^[a-z0-9.-]{1,128}$/.test(id) ||
  id.includes("..") ||
  id.startsWith(".") ||
  id.endsWith(".")
) {
  console.error(
    `invalid id: ${id} (charset [a-z0-9.-], no "..", no leading/trailing dot, max 128 chars)`,
  );
  process.exit(2);
}

const lastSegment = id.split(".").pop() || "hello";
const toClass = (s) =>
  s
    .split(/[-_.]/)
    .filter(Boolean)
    .map((w) => w[0].toUpperCase() + w.slice(1))
    .join("") || "My";
const tokens = {
  __ID__: id,
  __NAME__: args.name ?? toClass(lastSegment),
  __AUTHOR__: args.author ?? "your-name",
  __LICENSE__: args.license ?? "Apache-2.0",
  __CLASS__: `${toClass(args.name ?? lastSegment)}Plugin`,
  __PKG_NAME__: `opencapx-${lastSegment.toLowerCase().replace(/[^a-z0-9-]/g, "-")}`,
};

const dir = resolve(args.dir ?? `./${lastSegment}`);
if (existsSync(dir) && readdirSync(dir).length > 0) {
  console.error(`refusing to overwrite non-empty directory: ${dir}`);
  process.exit(2);
}
mkdirSync(dir, { recursive: true });
cpSync(templateDir, dir, { recursive: true });

// SDK reference: default is the published registry range, because the
// installed package has no repo checkout next to it -- a file: default would
// point into node_modules and break the scaffold. Pass --local when working
// from a checkout to depend on this repo's ts-sdk offline.
tokens.__SDK_REF__ = args.local
  ? "file:" + join(here, "..", "..", "ts-sdk")
  : "^0.1.0";

const files = [
  "opencapx-plugin.json",
  "package.json",
  "tsconfig.json",
  "src/plugin.ts",
  "test/plugin.test.ts",
  "README.md",
];
for (const f of files) {
  const p = join(dir, f);
  let text = readFileSync(p, "utf-8");
  for (const [token, value] of Object.entries(tokens)) {
    text = text.split(token).join(value);
  }
  writeFileSync(p, text);
}

console.log(`created ${dir}`);
console.log("next steps:");
console.log(`  cd ${args.dir ?? `./${lastSegment}`} && npm install && npm test`);
console.log("  npm run build  # emits dist/src/plugin.js referenced by opencapx-plugin.json");
