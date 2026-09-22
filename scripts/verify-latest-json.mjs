#!/usr/bin/env node
// verify-latest-json.mjs — updater release manifest validation (zero deps, usable in CI and locally).
//   node scripts/verify-latest-json.mjs <latest.json> [--expect-version x.y.z]
//   node scripts/verify-latest-json.mjs --selftest
// Validates: version is semver and (optionally) matches the tag; platforms is non-empty; each url is
// http(s) and signature is non-empty. Any failure → exit code 1.
import { readFileSync } from "node:fs";

const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/;

function validate(doc, expectVersion) {
  const errs = [];
  if (typeof doc !== "object" || doc === null || Array.isArray(doc)) {
    return ["not a JSON object"];
  }
  if (typeof doc.version !== "string" || !SEMVER.test(doc.version)) {
    errs.push(`bad version: ${JSON.stringify(doc.version)}`);
  } else if (expectVersion && doc.version !== expectVersion) {
    errs.push(`version ${doc.version} != expected ${expectVersion}`);
  }
  const platforms = doc.platforms;
  if (
    typeof platforms !== "object" ||
    platforms === null ||
    Array.isArray(platforms) ||
    Object.keys(platforms).length === 0
  ) {
    errs.push("platforms must be a non-empty object");
  } else {
    for (const [k, v] of Object.entries(platforms)) {
      if (typeof v?.url !== "string" || !/^https?:\/\//.test(v.url)) {
        errs.push(`${k}: bad url`);
      }
      if (typeof v?.signature !== "string" || v.signature.trim() === "") {
        errs.push(`${k}: empty signature`);
      }
    }
  }
  return errs;
}

function selftest() {
  const ok = {
    version: "1.2.3",
    platforms: {
      "darwin-aarch64": { url: "https://example.com/a.tar.gz", signature: "c2ln" },
    },
  };
  const cases = [
    [ok, undefined, true],
    [ok, "1.2.3", true],
    [ok, "9.9.9", false],
    [{ ...ok, version: "1.2" }, undefined, false],
    [{ version: "1.2.3", platforms: {} }, undefined, false],
    [
      { version: "1.2.3", platforms: { "linux-x86_64": { url: "https://e.com/a", signature: "" } } },
      undefined,
      false,
    ],
  ];
  for (const [doc, expect, wantOk] of cases) {
    const isOk = validate(doc, expect).length === 0;
    if (isOk !== wantOk) {
      console.error(`selftest failed: ${JSON.stringify(doc).slice(0, 80)}`);
      process.exit(1);
    }
  }
  console.log("✓ selftest 6/6 cases (version / expect-version / platforms / signature)");
}

const [arg1, ...rest] = process.argv.slice(2);
if (arg1 === "--selftest") {
  selftest();
  process.exit(0);
}
if (!arg1) {
  console.error(
    "usage: node scripts/verify-latest-json.mjs <latest.json> [--expect-version x.y.z] | --selftest",
  );
  process.exit(2);
}
const vIdx = rest.indexOf("--expect-version");
const expect = vIdx >= 0 ? rest[vIdx + 1] : undefined;
let doc;
try {
  doc = JSON.parse(readFileSync(arg1, "utf8"));
} catch (e) {
  console.error(`verify-latest-json: cannot read/parse ${arg1}: ${e.message}`);
  process.exit(1);
}
const errs = validate(doc, expect);
if (errs.length) {
  for (const e of errs) console.error(`verify-latest-json: ${e}`);
  process.exit(1);
}
console.log(`latest.json OK: version=${doc.version} platforms=${Object.keys(doc.platforms).join(",")}`);
