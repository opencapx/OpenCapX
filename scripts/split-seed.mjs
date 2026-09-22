#!/usr/bin/env node
// split-seed.mjs — key-ceremony S3 backup sharing tool (zero deps, Node standard library only).
// 2-of-3 Shamir over GF(256): f(x) = secret ^ (a1 ⊗ x); share = (x, f(x)), x∈{1,2,3}.
// Any 2 shares reconstruct the seed; a single share is zero-knowledge (degree-1, information-theoretically secure).
//   node scripts/split-seed.mjs split --in <seed.hex> --out-dir <dir>
//   node scripts/split-seed.mjs combine --shares <a.json>,<b.json>
//   node scripts/split-seed.mjs --selftest
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { randomBytes, createHash } from "node:crypto";
import { join, resolve } from "node:path";

function gfMul(a, b) {
  let p = 0;
  for (let i = 0; i < 8; i++) {
    if (b & 1) p ^= a;
    const hi = a & 0x80;
    a = (a << 1) & 0xff;
    if (hi) a ^= 0x1b;
    b >>= 1;
  }
  return p;
}

function gfInv(a) {
  let r = 1;
  let base = a;
  let e = 254;
  while (e > 0) {
    if (e & 1) r = gfMul(r, base);
    base = gfMul(base, base);
    e >>= 1;
  }
  return r;
}

const gfDiv = (a, b) => gfMul(a, gfInv(b));

function split(seed) {
  const a1 = randomBytes(seed.length);
  const shares = [];
  for (let x = 1; x <= 3; x++) {
    const y = Buffer.alloc(seed.length);
    for (let i = 0; i < seed.length; i++) y[i] = seed[i] ^ gfMul(a1[i], x);
    shares.push({ x, y });
  }
  return shares;
}

function combine(s1, s2) {
  if (s1.x === s2.x) throw new Error("shares must have distinct x");
  const denom = s1.x ^ s2.x;
  const out = Buffer.alloc(s1.y.length);
  for (let i = 0; i < out.length; i++) {
    out[i] = gfDiv(gfMul(s1.y[i], s2.x) ^ gfMul(s2.y[i], s1.x), denom);
  }
  return out;
}

function parseSeedHex(text, label) {
  const hex = text.trim();
  if (!/^[0-9a-f]+$/i.test(hex) || hex.length % 2 !== 0 || hex.length === 0) {
    throw new Error(`${label}: not valid even-length hex`);
  }
  return Buffer.from(hex, "hex");
}

function opt(args, name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
}

const sha256 = (buf) => createHash("sha256").update(buf).digest("hex");

function readShare(path) {
  const j = JSON.parse(readFileSync(path, "utf8"));
  if (typeof j.x !== "number" || typeof j.y !== "string") {
    throw new Error(`${path}: not a share file`);
  }
  return { x: j.x, y: Buffer.from(j.y, "hex") };
}

function selftest() {
  for (let iter = 0; iter < 50; iter++) {
    const seed = randomBytes(32);
    const shares = split(seed);
    for (const [i, j] of [
      [0, 1],
      [0, 2],
      [1, 2],
    ]) {
      const got = combine(shares[i], shares[j]);
      if (!got.equals(seed)) throw new Error(`iter ${iter}: pair ${i + 1}+${j + 1} mismatch`);
    }
    if (shares.some((s) => s.y.equals(seed))) throw new Error(`iter ${iter}: share equals seed`);
  }
  console.log("✓ selftest 50/50 seeds · 3/3 pairs each · no share equals seed");
}

const [cmd, ...args] = process.argv.slice(2);
try {
  if (cmd === "--selftest") {
    selftest();
  } else if (cmd === "split") {
    const inPath = opt(args, "--in");
    const outDir = opt(args, "--out-dir");
    if (!inPath || !outDir) throw new Error("usage: split --in <seed.hex> --out-dir <dir>");
    const seed = parseSeedHex(readFileSync(resolve(inPath), "utf8"), inPath);
    mkdirSync(resolve(outDir), { recursive: true });
    const shares = split(seed);
    for (const s of shares) {
      writeFileSync(
        join(resolve(outDir), `share-${s.x}.json`),
        JSON.stringify(
          {
            x: s.x,
            y: s.y.toString("hex"),
            note: "2-of-3 Shamir share; encrypt before leaving the air-gapped machine; store separately",
          },
          null,
          2,
        ) + "\n",
        { mode: 0o600 },
      );
    }
    console.log(`✓ split ${seed.length}-byte seed → 3 shares in ${resolve(outDir)}`);
    console.log(`  seed sha256 = ${sha256(seed)} (for integrity checks only; not written to share files)`);
  } else if (cmd === "combine") {
    const list = (opt(args, "--shares") ?? "").split(",").filter(Boolean);
    if (list.length !== 2) throw new Error("usage: combine --shares <a.json>,<b.json>");
    const seed = combine(readShare(resolve(list[0])), readShare(resolve(list[1])));
    console.log(`seed = ${seed.toString("hex")}`);
    console.log(`seed sha256 = ${sha256(seed)}`);
  } else {
    console.error(
      'usage: node scripts/split-seed.mjs split --in <seed.hex> --out-dir <dir> | combine --shares <a.json>,<b.json> | --selftest',
    );
    process.exit(2);
  }
} catch (e) {
  console.error(`split-seed: ${e.message}`);
  process.exit(1);
}
