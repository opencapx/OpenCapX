import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  ManifestError,
  loadManifest,
  validateManifest,
} from "../src/manifest.js";

function writeTmp(obj: unknown): string {
  const dir = mkdtempSync(join(tmpdir(), "opencapx-ts-manifest-"));
  const p = join(dir, "opencapx-plugin.json");
  writeFileSync(p, typeof obj === "string" ? obj : JSON.stringify(obj));
  return p;
}

const base = {
  id: "com.example.x",
  name: "X",
  version: "0.1.0",
  apiVersion: "1",
  type: "capability",
  capabilities: ["image.analyze"],
  runtime: { type: "process", command: "node", args: ["dist/plugin.js"] },
};

describe("validateManifest", () => {
  it("accepts a good manifest and returns it", () => {
    assert.deepEqual(validateManifest({ ...base }), { ...base });
  });

  it("rejects missing required fields", () => {
    for (const key of ["id", "name", "version", "apiVersion", "type"]) {
      const bad = { ...base };
      delete (bad as Record<string, unknown>)[key];
      assert.throws(() => validateManifest(bad), ManifestError);
    }
  });

  it("rejects bad type and apiVersion", () => {
    assert.throws(
      () => validateManifest({ ...base, type: "widget" }),
      ManifestError,
    );
    assert.throws(
      () => validateManifest({ ...base, apiVersion: "2" }),
      ManifestError,
    );
  });

  it("capability type needs capabilities[] and runtime", () => {
    assert.throws(
      () => validateManifest({ ...base, capabilities: [] }),
      ManifestError,
    );
    const noRuntime = { ...base };
    delete (noRuntime as Record<string, unknown>)["runtime"];
    assert.throws(() => validateManifest(noRuntime), ManifestError);
  });

  it("validates object-form timeoutSecs", () => {
    const ok = {
      ...base,
      capabilities: [{ id: "image.analyze", timeoutSecs: 60 }],
    };
    assert.deepEqual(validateManifest(ok), ok);
    for (const bad of [0, 601, 1.5, true, "60"]) {
      assert.throws(
        () =>
          validateManifest({
            ...base,
            capabilities: [{ id: "image.analyze", timeoutSecs: bad }],
          }),
        ManifestError,
      );
    }
  });
});

describe("loadManifest", () => {
  it("loads from disk", () => {
    assert.deepEqual(loadManifest(writeTmp(base)), base);
  });

  it("throws on missing file and bad json", () => {
    assert.throws(
      () => loadManifest("/definitely/not/here.json"),
      ManifestError,
    );
    assert.throws(() => loadManifest(writeTmp("{ nope")), ManifestError);
  });
});
