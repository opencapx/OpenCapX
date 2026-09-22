// Minimal scaffolder test: runs bin/create.mjs and asserts the generated
// package.json. Verifies the registry-range default vs the --local switch
// (offline file: ref from a checkout). Zero deps — node:test + node standard library only.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const bin = join(here, "..", "bin", "create.mjs");
const repoRoot = resolve(here, "..", "..", "..");
const defaultSdkRef = "file:" + resolve(repoRoot, "packages", "ts-sdk");

function runCreate(args) {
  return spawnSync(process.execPath, [bin, ...args], { encoding: "utf-8" });
}

function withTmpDir(t) {
  const dir = mkdtempSync(join(tmpdir(), "opencapx-create-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

function readGeneratedPackageJson(target) {
  return JSON.parse(readFileSync(join(target, "package.json"), "utf-8"));
}

test("scaffolds a plugin with the registry SDK ref by default", (t) => {
  const root = withTmpDir(t);
  const target = join(root, "hello");
  const res = runCreate(["--id", "com.acme.hello", "--name", "Hello", "--dir", target]);
  assert.equal(res.status, 0, res.stderr);

  const pkg = readGeneratedPackageJson(target);
  assert.equal(pkg.name, "opencapx-hello");
  assert.equal(pkg.dependencies["@opencapx/sdk"], "^0.1.0");
  // every __TOKEN__ was substituted (no placeholder survives)
  assert.ok(!JSON.stringify(pkg).includes("__"));
});

test("--local switches the SDK ref to the checkout file: path", (t) => {
  const root = withTmpDir(t);
  const target = join(root, "hello");
  const res = runCreate([
    "--id",
    "com.acme.hello",
    "--name",
    "Hello",
    "--dir",
    target,
    "--local",
  ]);
  assert.equal(res.status, 0, res.stderr);

  const pkg = readGeneratedPackageJson(target);
  assert.equal(pkg.dependencies["@opencapx/sdk"], defaultSdkRef);
});

test("refuses to write into a non-empty directory", (t) => {
  const root = withTmpDir(t);
  writeFileSync(join(root, "occupier.txt"), "x");
  const res = runCreate(["--id", "com.acme.hello", "--dir", root]);
  assert.equal(res.status, 2);
  assert.match(res.stderr, /refusing to overwrite non-empty directory/);
});
