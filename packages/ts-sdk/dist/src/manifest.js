"use strict";
/** OpenCapX manifest parse + validate.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/manifest.py`, aligned with
 * `docs/plugin-manifest.md`:
 *     id, name, version, apiVersion, type (required)
 *     description, author, homepage, license, capabilities, permissions,
 *     states, runtime (optional)
 */
Object.defineProperty(exports, "__esModule", { value: true });
exports.SUPPORTED_API_VERSION = exports.ALLOWED_TYPES = exports.REQUIRED_TOP = exports.ManifestError = void 0;
exports.loadManifest = loadManifest;
exports.validateManifest = validateManifest;
const node_fs_1 = require("node:fs");
const node_path_1 = require("node:path");
/** Manifest field missing/illegal. */
class ManifestError extends Error {
    constructor(message) {
        super(message);
        this.name = "ManifestError";
    }
}
exports.ManifestError = ManifestError;
/** Required top-level fields. */
exports.REQUIRED_TOP = [
    "id",
    "name",
    "version",
    "apiVersion",
    "type",
];
/** Allowed `type` values. */
exports.ALLOWED_TYPES = new Set([
    "pet",
    "capability",
]);
/** Only `"1"` for this SDK version. */
exports.SUPPORTED_API_VERSION = "1";
/** Read + parse + validate a manifest. Throws `ManifestError` on failure. */
function loadManifest(path) {
    const p = (0, node_path_1.resolve)(path);
    if (!(0, node_fs_1.existsSync)(p)) {
        throw new ManifestError(`manifest not found: ${p}`);
    }
    let raw;
    try {
        raw = JSON.parse((0, node_fs_1.readFileSync)(p, "utf-8"));
    }
    catch (err) {
        const reason = err instanceof Error ? err.message : String(err);
        throw new ManifestError(`manifest not valid json: ${reason}`);
    }
    return validateManifest(raw);
}
/** Validate an already-parsed dict, return the same dict. */
function validateManifest(m) {
    if (typeof m !== "object" || m === null || Array.isArray(m)) {
        throw new ManifestError("manifest must be a JSON object");
    }
    const manifest = m;
    for (const key of exports.REQUIRED_TOP) {
        const v = manifest[key];
        if (!(key in manifest) || v === null || v === undefined || v === "") {
            throw new ManifestError(`manifest missing required field: ${key}`);
        }
    }
    if (!exports.ALLOWED_TYPES.has(String(manifest["type"]))) {
        throw new ManifestError(`manifest type must be one of pet | capability, got ${JSON.stringify(manifest["type"])}`);
    }
    const api = String(manifest["apiVersion"]);
    if (api !== exports.SUPPORTED_API_VERSION) {
        throw new ManifestError(`manifest apiVersion ${JSON.stringify(api)} not supported (need ${JSON.stringify(exports.SUPPORTED_API_VERSION)})`);
    }
    // A capability manifest must declare capabilities + runtime.
    if (manifest["type"] === "capability") {
        const caps = manifest["capabilities"];
        if (!Array.isArray(caps) || caps.length === 0) {
            throw new ManifestError("capability manifest must declare non-empty capabilities[]");
        }
        if (!manifest["runtime"]) {
            throw new ManifestError("capability manifest must declare runtime");
        }
        // Object-form timeoutSecs (seconds, 1..=600); string form uses the 60s default.
        for (const item of caps) {
            if (typeof item === "object" &&
                item !== null &&
                "timeoutSecs" in item) {
                const t = item["timeoutSecs"];
                const id = typeof item["id"] === "string"
                    ? item["id"]
                    : "?";
                if (typeof t === "boolean" || typeof t !== "number" || !Number.isInteger(t) || t < 1 || t > 600) {
                    throw new ManifestError(`capability ${JSON.stringify(id)}: timeoutSecs must be an integer in 1..=600, got ${JSON.stringify(t)}`);
                }
            }
        }
    }
    return manifest;
}
