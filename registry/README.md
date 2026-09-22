# OpenCapX Registry (hosting scaffold)

✅ **Live (2026-09-20)**: the hosted repo is [opencapx/opencapx-registry](https://github.com/opencapx/opencapx-registry)
(raw static hosting). The client's default URL already points at it (`registry.rs` `DEFAULT_INDEX_URL`);
this directory is kept as a scaffold and the document source of truth, while the real index/packages/release flow follow the hosted repo.
First signed index: weather-demo 0.1.1 (signed by the official CI key, live verification + sha256 reconciliation passed).

## Files the client consumes

The client (`src-tauri/src/core/registry.rs`) consumes only two kinds of file:

| File | Role |
|---|---|
| `index.json` | **signed index** (authoritative source): `schemaVersion=2`, `publishers[]`, `revokedKeys[]`, `entries[]`, `indexSignature` |
| `seed.json` | **built-in seed** (offline fallback): same schema as index.json, updatable with app releases; falls back to it when both network and cache are unavailable |

Cache and state are managed by the client (`OPENCAPX_REGISTRY_DIR`, default `~/.opencapx/registry`):
`cache.json` (verified index within TTL), `state.json` (`generatedAt` anti-replay watermark).

## Index schema (frozen at M3)

```jsonc
{
  "schemaVersion": 2,
  "generatedAt": 1757846400,          // monotonically non-decreasing; replaying an older value is always rejected
  "publishers": [
    { "keyId": "com.example.author",  // publisher identity (matches the package signing keyId)
      "publicKey": "<64 hex>",        // Ed25519 public key
      "verified": true,               // observation-period flag: false takes no part in version selection or trust fallback
      "since": "2026-09-14" }
  ],
  "revokedKeys": [
    { "keyId": "com.example.leaked", "at": 1757846400, "reason": "key compromise" }
  ],
  "entries": [
    { "id": "com.example.weather-demo",
      "name": "Weather Demo",
      "summary": "…",
      "author": { "keyId": "com.example.author" },
      "repo": "author/weather-demo",
      "categories": ["demo"],
      "versions": [
        { "version": "0.1.0",
          "minCoreVersion": "0.4.0",  // minimum compatible core
          "channel": "stable",        // stable|beta|dev
          "downloadUrl": "https://…/pkg.ocplugin",
          "sha256": "<64 hex>",       // SHA-256 of the .ocplugin file bytes
          "signature": { "alg": "ed25519", "keyId": "…", "sig": "<128 hex>" },
          "releasedAt": 1757846400,
          "sizeBytes": 890 }
      ] }
  ],
  "indexSignature": { "alg": "ed25519", "keyId": "com.opencapx.official", "sig": "<128 hex>" }
}
```

Field-level semantics and signature-chain details are in `../docs/supply-chain.md` (D2) and the module header of `../src-tauri/src/core/registry.rs`.

## Invariants (MUST)

1. `generatedAt` is monotonically non-decreasing — the client rejects replays using its local watermark; a rollback = publish a new index with a larger `generatedAt`, never re-publish an old file.
2. `revokedKeys[]` only grows, never shrinks; affected entries should be removed or frozen at the same time.
3. A published version's `downloadUrl`/`sha256`/`signature` cannot be rewritten in place — changing a package requires a new `version`.
4. Every release is **re-signed** by the official private key (air-gapped machine flow in `../docs/key-ceremony.md` S5); the signature covers every byte except `indexSignature`.

## Release flow

1. A PR edits `index.unsigned.json` (new publisher / version entry / revocation registration), self-checked against the PR template;
2. After merge, the official publisher signs on the air-gapped machine:

   ```bash
   opencapx sign-index index.unsigned.json \
     --key @<official-seed.hex> --key-id <official-keyId> --out index.json
   ```

3. Verify: `scripts/validate-index.sh index.json "keyId=<official-public-key-hex>"`;
4. Publish `index.json` (clients roll forward naturally once their TTL expires).

## Publisher registration / revocation / compromise

- **Registration**: the PR adds a `publishers[]` entry (`keyId`/`publicKey` from the author's `opencapx keygen`); ownership is verified by hand (automated gating added from M6), and takes effect only when `verified=true`.
- **Revocation**: append the `keyId` to `revokedKeys[]` (`at`/`reason` required), and handle the affected entries at the same time.
- **Compromise**: follow key-ceremony S7 — a new identity signs a new index + revocation registration + entries roll forward.

## Related docs

- `../docs/supply-chain.md` — D1 trust model / D2 registration and revocation / D6 launch prerequisites
- `../docs/key-ceremony.md` — S5 index signing spec / S6 rotation / S7 compromise handling
- `../docs/plugin-signing.md` — author-side signing toolchain
- `../fixtures/registry/` — client-side frozen fixtures and regeneration steps
