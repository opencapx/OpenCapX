/** OpenCapX manifest parse + validate.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/manifest.py`, aligned with
 * `docs/plugin-manifest.md`:
 *     id, name, version, apiVersion, type (required)
 *     description, author, homepage, license, capabilities, permissions,
 *     states, runtime (optional)
 */

import { readFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";

/** Manifest field missing/illegal. */
export class ManifestError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ManifestError";
  }
}

/** Required top-level fields. */
export const REQUIRED_TOP: readonly string[] = [
  "id",
  "name",
  "version",
  "apiVersion",
  "type",
];

/** Allowed `type` values. */
export const ALLOWED_TYPES: ReadonlySet<string> = new Set([
  "pet",
  "capability",
]);

/** Only `"1"` for this SDK version. */
export const SUPPORTED_API_VERSION = "1";

/** A parsed manifest (kept loose — Core owns deep validation). */
export type Manifest = Record<string, unknown>;

/** Read + parse + validate a manifest. Throws `ManifestError` on failure. */
export function loadManifest(path: string): Manifest {
  const p = resolve(path);
  if (!existsSync(p)) {
    throw new ManifestError(`manifest not found: ${p}`);
  }
  let raw: unknown;
  try {
    raw = JSON.parse(readFileSync(p, "utf-8"));
  } catch (err) {
    const reason = err instanceof Error ? err.message : String(err);
    throw new ManifestError(`manifest not valid json: ${reason}`);
  }
  return validateManifest(raw);
}

/** Validate an already-parsed dict, return the same dict. */
export function validateManifest(m: unknown): Manifest {
  if (typeof m !== "object" || m === null || Array.isArray(m)) {
    throw new ManifestError("manifest must be a JSON object");
  }
  const manifest = m as Manifest;
  for (const key of REQUIRED_TOP) {
    const v = manifest[key];
    if (!(key in manifest) || v === null || v === undefined || v === "") {
      throw new ManifestError(`manifest missing required field: ${key}`);
    }
  }
  if (!ALLOWED_TYPES.has(String(manifest["type"]))) {
    throw new ManifestError(
      `manifest type must be one of pet | capability, got ${JSON.stringify(manifest["type"])}`,
    );
  }
  const api = String(manifest["apiVersion"]);
  if (api !== SUPPORTED_API_VERSION) {
    throw new ManifestError(
      `manifest apiVersion ${JSON.stringify(api)} not supported (need ${JSON.stringify(SUPPORTED_API_VERSION)})`,
    );
  }
  // A capability manifest must declare capabilities + runtime.
  if (manifest["type"] === "capability") {
    const caps = manifest["capabilities"];
    if (!Array.isArray(caps) || caps.length === 0) {
      throw new ManifestError(
        "capability manifest must declare non-empty capabilities[]",
      );
    }
    if (!manifest["runtime"]) {
      throw new ManifestError("capability manifest must declare runtime");
    }
    // Object-form timeoutSecs (seconds, 1..=600); string form uses the 60s default.
    for (const item of caps) {
      if (
        typeof item === "object" &&
        item !== null &&
        "timeoutSecs" in (item as Record<string, unknown>)
      ) {
        const t = (item as Record<string, unknown>)["timeoutSecs"];
        const id =
          typeof (item as Record<string, unknown>)["id"] === "string"
            ? (item as Record<string, unknown>)["id"]
            : "?";
        if (typeof t === "boolean" || typeof t !== "number" || !Number.isInteger(t) || t < 1 || t > 600) {
          throw new ManifestError(
            `capability ${JSON.stringify(id)}: timeoutSecs must be an integer in 1..=600, got ${JSON.stringify(t)}`,
          );
        }
      }
    }
  }
  return manifest;
}
