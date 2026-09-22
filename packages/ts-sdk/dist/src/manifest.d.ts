/** OpenCapX manifest parse + validate.
 *
 * Mirrors `packages/plugin-sdk/opencapx_sdk/manifest.py`, aligned with
 * `docs/plugin-manifest.md`:
 *     id, name, version, apiVersion, type (required)
 *     description, author, homepage, license, capabilities, permissions,
 *     states, runtime (optional)
 */
/** Manifest field missing/illegal. */
export declare class ManifestError extends Error {
    constructor(message: string);
}
/** Required top-level fields. */
export declare const REQUIRED_TOP: readonly string[];
/** Allowed `type` values. */
export declare const ALLOWED_TYPES: ReadonlySet<string>;
/** Only `"1"` for this SDK version. */
export declare const SUPPORTED_API_VERSION = "1";
/** A parsed manifest (kept loose — Core owns deep validation). */
export type Manifest = Record<string, unknown>;
/** Read + parse + validate a manifest. Throws `ManifestError` on failure. */
export declare function loadManifest(path: string): Manifest;
/** Validate an already-parsed dict, return the same dict. */
export declare function validateManifest(m: unknown): Manifest;
