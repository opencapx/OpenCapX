# Plugin Signing and Distribution (Author Guide)

`.ocplugin` is just a ZIP — anyone can change a single byte and repackage it. Signing answers two questions:

1. **Integrity**: Has the package been touched after it was signed?
2. **Publisher identity**: Was this package really signed by the holder of a given keyId?

This guide is the complete operations manual from the **author's perspective**: keygen → pack → distribute → user imports trust. For the trust model and rulings see [supply-chain.md](supply-chain.md); for the official key ceremony see [key-ceremony.md](key-ceremony.md).

## 0. Two Channels at a Glance

| Channel | `signature.alg` | Algorithm | Trust root | Use |
|---|---|---|---|---|
| v1 local | default / `"hmac-v1"` | HMAC-SHA256 (symmetric) | The shared secret in `~/.opencapx/trusted-keys.json` | Single-machine/team internal use |
| v2 distribution | `"ed25519"` | Ed25519 (asymmetric) | Publisher public key (distributed via the official signed index, M3) | Marketplace distribution |

> **Why distribution can no longer use HMAC**: the holder of a symmetric key can both verify and forge; sending the same secret to all users = all users can impersonate each other. The distribution channel recognizes v2 only. For the full argument see supply-chain §3 D1.

## 1. Two Command Entry Points

| Entry point | Command | Suited for |
|---|---|---|
| Python SDK (primary path) | `python3 -m opencapx_sdk.signing keygen\|pack\|verify\|help` | Plugin authors (Python-first) |
| Rust CLI | `cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- keygen\|pack\|verify` | In-repo development, no Python environment |

The two paths are **byte-for-byte identical** (same digest / same signature) and can verify each other's signatures; the repo pins them down in both directions with `fixtures/signing/golden/*`.

> Not sure where to start? `python3 -m opencapx_sdk.signing help` prints the full flow (commands → typical steps → key discipline).

### Installation

```bash
# Python (primary path). genkey/pack/verify-v2 depend on cryptography:
pip install opencapx-sdk

# For in-repo development you can skip the install and point PYTHONPATH directly:
export PYTHONPATH=packages/plugin-sdk
python3 -c "import cryptography" || echo "missing cryptography: pip install cryptography"
```

The Rust CLI has no extra dependencies; `cargo run` is enough.

## 2. Full Flow: keygen → pack → verify

Each command below can be copied and run one by one from the repo root (the example plugin uses `fixtures/signing/plugin`).

### 2.1 keygen — generate an identity

```bash
# Python
python3 -m opencapx_sdk.signing keygen --out alice-2026.key.hex

# Rust
cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- keygen --out alice-2026.key.hex
```

- `--out` defaults to `opencapx-signing.key.hex`.
- The produced file is 64 lowercase hex characters (a 32-byte seed) plus a newline. **The seed is the identity root**; a leak = your identity is impersonated.
- stdout prints one line of JSON `{"out": "...", "publicKey": "<64 hex>"}`, plus a ready-to-copy suggested trusted-keys entry.
- When the file pointed to by `--out` already exists it **refuses to overwrite** and exits `1` — a silent overwrite would permanently deprive older artifacts of their corresponding private key.

### 2.2 pack — package and sign

```bash
# Python
python3 -m opencapx_sdk.signing pack fixtures/signing/plugin \
  --key @alice-2026.key.hex \
  --key-id com.example.alice \
  --out alice-plugin.ocplugin

# Rust
cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- pack fixtures/signing/plugin \
  --key @alice-2026.key.hex \
  --key-id com.example.alice \
  --out alice-plugin.ocplugin
```

| Argument | Required | Description |
|---|---|---|
| `<plugin-dir>` | ✅ | The plugin directory containing `opencapx-plugin.json` |
| `--key <seed-hex\|@file>` | ✅ | 64-hex seed; `@file` reads it from a file (after trimming) |
| `--key-id <id>` | ✅ | Reverse domain, the same namespace as the plugin id |
| `--out <file.ocplugin>` | ❌ | Defaults to `<id>-<version>.ocplugin` |

pack only reads the source directory and does not modify it; the manifest segment is fixed at the package root, and the content files follow in byte order of their relative paths. After producing the package, pack **self-verifies** (recomputes the digest + verifies the signature directly with the derived public key); on failure it deletes the half-finished product and exits `1` — a signing tool never emits a package it cannot itself verify.

> `--key-id` is written verbatim into the manifest's `signature.keyId`, as the key under which user trust is registered.

### 2.3 verify — check a signature

```bash
# Python
python3 -m opencapx_sdk.signing verify alice-plugin.ocplugin \
  --trusted-keys ~/.opencapx/trusted-keys.json

# Rust
cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- verify alice-plugin.ocplugin \
  --trusted-keys ~/.opencapx/trusted-keys.json
```

- `--trusted-keys <path>` is optional. It defaults to reading `~/.opencapx/trusted-keys.json`; you can also point it with the env var `OPENCAPX_TRUSTED_KEYS` (the CLI's `--trusted-keys` simply sets that variable).
- stdout emits only one line of machine-readable JSON `{"status": "...", "keyId": "..."}` (keyId omitted when absent); a human-readable line `verify: <file> -> <status>` goes to stderr.
- The `status` values are exactly the same table as Rust's `VerifyOutcome::label()`: `trusted` / `unsigned` / `unknown-key` / `tampered` / `bad-signature` / `malformed-signature`.

### 2.4 Script path (optional)

The in-repo `scripts/pack-ocplugin.sh` supports optional signing; it delegates the work to the Python SDK:

```bash
scripts/pack-ocplugin.sh <plugin-dir> [out.ocplugin] --sign <seed|@file> --key-id <id>
```

`--sign` and `--key-id` must be given together; if not, the behavior is byte-for-byte identical to the old version (plain packaging). It calls the `python3` on `PATH`, **so that `python3` must have `cryptography` installed** (otherwise it reports `No module named 'cryptography'` and exits 1).

## 3. Exit Code Table

verify's exit codes are a contract for CI/scripts (a common Linux convention: 0 success, 1 general error, 2 precondition not met):

| Exit code | `status` | Meaning | What to do |
|---|---|---|---|
| `0` | `trusted` | Signature valid and keyId registered | Allow (direct install) |
| `2` | `unsigned` | The manifest has no `signature` field | Three-state · allow after a yellow-badge warning is confirmed; hard-reject when "Allow unsigned packages" is OFF in the Settings page |
| `2` | `unknown-key` | The signature is present, but the keyId is not in trusted-keys (or the key type does not match) | Three-state · same as `unsigned` (warning confirmation); registering the public key after confirming the publisher is better |
| `1` | `tampered` | `sha256` does not match the actual content digest; the package was modified | **Do not install**; re-fetch from the original channel |
| `1` | `bad-signature` | The keyId is registered but the signature does not verify | **Do not install**; the package is forged or corrupted |
| `1` | `malformed-signature` | Unknown `alg`, or the signature/hex segment cannot be parsed | **Do not install** |
| `1` | — (no `status`) | IO/format error: the file does not exist, or it is not a valid ZIP | Check the path and file |

> **Note**: a package that was just `pack`ed naturally carries a signature, so **when no trusted-keys are provided, verify yields `unknown-key` (exit 2), not `unsigned`**. `unsigned` only appears on old packages with no signature field at all. Both exit 2.
>
> **Three-state rule (from M5)**: `trusted` installs directly; `unsigned` / `unknown-key` require explicit confirmation in the install dialog (install after confirmation, leaving a `plugin.installed.unsigned` audit); `tampered` / `bad-signature` / `malformed-signature` are hard-rejected, **never bypassable** (not even with confirmation).

keygen / pack are interactive tools and only ever exit `0` (success) or `1` (failure: refused overwrite / missing argument / invalid manifest / out-of-range number / self-verification failure / IO error).

## 4. trusted-keys.json: Two Shapes and User Import

File location: `~/.opencapx/trusted-keys.json` (overridable with `OPENCAPX_TRUSTED_KEYS`). It is a JSON object of `{keyId: trust entry}`, and a trust entry has two shapes:

```json
{
  "alice-local": "8f39df61...ff",
  "com.example.alice": { "alg": "ed25519", "publicKey": "03a107bf...b8" }
}
```

| Shape | Value | Channel |
|---|---|---|
| String | The hex of an HMAC secret | v1 local |
| Object `{"alg":"ed25519","publicKey":"<64 hex>"}` | An Ed25519 public key (32 bytes) | v2 distribution |

Invalid lines (bad hex / unknown `alg` / wrong public-key length / missing `publicKey`) are **silently skipped** and do not affect the other entries.

**User import flow (local trust channel)**:

1. The author publishes `keyId` and `publicKey` through a **channel independent of the .ocplugin** (official site / repo README / release announcement).
2. The user verifies this public-key fingerprint through a trusted channel (face to face with the author / cross-checking via a second channel) to guard against "the author's channel being swapped too".
3. The user writes the entry into `~/.opencapx/trusted-keys.json` (it is re-read on every install / preview, no restart needed).
4. Verify again: the same package should change from `unknown-key` (2) to `trusted` (0).

> **The CLI does not manage trusted-keys on your behalf**; importing is the user's act of explicitly editing this JSON — which is exactly the literal meaning of "the local trust root = what I put in myself". From M3, v2 public keys can also be distributed automatically via the **official signed index**, at which point the UI will label the source (registry / local / unknown).

## 5. v1 vs v2 Comparison

| Dimension | v1 local (HMAC) | v2 distribution (Ed25519) |
|---|---|---|
| `signature.alg` | default / `"hmac-v1"` | `"ed25519"` |
| Algorithm | HMAC-SHA256 (symmetric) | Ed25519 (asymmetric) |
| Trust root | A secret shared by both parties | The publisher's public key (may be public) |
| **Digest coverage** | Entry files only; **excludes the manifest** | The manifest (canonical, see below) **+** entry files |
| Signed message | `"opencapx-v1\n" + archive_hash` | `"opencapx-v2\n" + digest_v2_hex` |
| manifest `sha256` field | The hex of the archive hash | The hex of digest_v2 |
| Active protection | Content tampering | Content tampering **+ manifest tampering** |

> **v1's hole (stated honestly)**: v1's archive hash deliberately excludes `opencapx-plugin.json` from the digest. An attacker who secretly adds a high-risk permission after signing (e.g. `shell.exec`) leaves the archive hash unchanged and the signature still verifies — v1 cannot detect it. v2 brings the manifest into the digest, closing this hole.

### digest_v2 frozen specification

The semantics of the `sha256` field **are dispatched by `alg`** (the field name does not change): v1 = archive hash, v2 = digest_v2. digest_v2 is defined byte by byte:

```text
digest_v2        = SHA256( "opencapx-canon-v2\n" ‖ manifest_segment ‖ entries_segment )
manifest_segment = ascii(decimal(len(canonical_utf8))) ‖ "\n" ‖ canonical_utf8
entries_segment  = Σ(sorted by name's UTF-8 byte order, excluding opencapx-plugin.json):
                   name_utf8 ‖ "\n" ‖ ascii(decimal(size)) ‖ "\n" ‖ bytes
canonical_utf8   = manifest JSON with the "sha256" and "signature" keys removed,
                   object keys at every level in lexicographic order, no whitespace, UTF-8 (non-ASCII not \u-escaped)
```

Key points:

- `sha256`/`signature` are excluded because they are **products** of signing; including them would be self-referential and make the digest unstable.
- Both `canonical` and `entries` are **included** in the digest, so changing the manifest (changing permissions) and changing files are both hard to hide from detection.
- The entry-collection rule is shared by v1/v2: skip directories, skip the manifest, order by name bytes; directory packaging and ZIP packaging produce the same digest for the same content.

The Python and Rust implementations **reproduce the above spec byte for byte**, and `fixtures/signing/golden/*` pins them down in both directions (see §7).

## 6. Manifest Number Constraints (pack-time hard gate)

Numbers in the signed manifest must be **integers within the 64-bit range** (that is, `[-(2^63), 2^64-1]` = `i64::MIN … u64::MAX`):

- **Floats** (`1.5`, `1e16`) are always rejected;
- **Integer literals outside that range** (e.g. `100000000000000000000`) are also rejected;
- `true`/`false` are booleans, not numbers, and are allowed.

WHY: Python's `json` and Rust's `serde_json` (ryu) **serialize floats/out-of-range integers differently**, which would silently make the cross-language canonical digest diverge. The boundary is enforced at pack time, before anything is written to disk, erroring out on the spot with exit `1`, rather than producing a package that "Rust can verify but Python cannot". The two implementations share the same rule in `core::signing::ensure_signable_numbers` / `opencapx_sdk.signing.ensure_signable_numbers`.

## 7. Golden Vectors and Cross-Verification

`fixtures/signing/` commits a fixed seed/public key and `golden/signed.ocplugin`, pinning down the frame format, canonical form, and signature bytes:

- Rust: `core::plugin_sig::tests::golden_vectors_pin_digest_and_signature` and others.
- Python: `packages/plugin-sdk/tests/test_signing.py`.
- Cross-language: the Rust test recomputes the same digest using a Python SDK subprocess and verifies a Python-produced package directly with Rust.

> ⚠️ This directory is **public test material**: its seed / public key must **never** be used to sign real plugins, and must **never** be added to a production `trusted-keys.json`. See `fixtures/signing/README.md`.

## 8. Troubleshooting

| Symptom | Meaning | User action |
|---|---|---|
| `tampered` (1) | The content or manifest does not match `sha256` | The package was altered in transit/storage; re-fetch from the original channel. If the author **intentionally** changed the source without re-signing, that is a missing step in the signing flow |
| `unknown-key` (2) | The signature is valid, but this keyId is not registered locally | After confirming the publisher's identity, write the public key into trusted-keys; if you cannot confirm it, do not install |
| `bad-signature` (1) | The keyId is registered, but the signature does not verify | The package is forged or corrupted, **do not install**; beware impersonation |
| `malformed-signature` (1) | Unknown `alg` / the signature segment cannot be parsed | Update the client or check with the publisher; do not install |
| `unsigned` (2) | An old package with no signature at all | Allow after confirming the warning in the install dialog; hard-reject when "Allow unsigned packages" is OFF in the Settings page |

## 9. Honest Boundaries

- **A signature ≠ a sandbox.** The plugin runtime is still an arbitrary process; a signature only guarantees "this person signed the package and it has not been touched". Runtime behavior is governed by the Permission Manager, see [permissions.md](permissions.md).
- **v1 is a local channel.** HMAC symmetric = can verify = can forge, so it is only suited to single-machine/team internal use; distribution must use v2.
- **The local channel has no revocation mechanism.** Deleting an entry from trusted-keys invalidates it; formal revocation (revokedKeys + events for already-installed users) belongs to the registry (M3/M8).
- **This guide does not cover the official key ceremony** — that is the publisher-side flow, see [key-ceremony.md](key-ceremony.md).

## 10. Runtime Environment Variables (author notes)

**From S1 (a breaking tightening): plugins no longer inherit the host environment variables by default.** When Core spawns a plugin it `env_clear`s,
and repopulates only a minimal whitelist:

| Variable | Description |
|---|---|
| `PATH` / `HOME` / `TMPDIR` / `TZ` / `LANG` | Required for running |
| `LC_*` / `XDG_*` | Locale and desktop conventions |

Beyond that:

- **The author's `runtime.env` declarations in the manifest are always injected** (for examples see [plugin-manifest.md](plugin-manifest.md) §runtime);
- The user can add whitelist keys in Settings (`plugin_env_allowlist`, comma-separated), for example when a plugin needs to read an `OPENAI_API_KEY`-style key that the user's
  shell exports;
- Migration advice: for plugins that implicitly depended on the host env, write the key names into the README / install instructions and tell the user to add them to the whitelist;
  better still, route sensitive configuration through `config.*` (`secret:*` goes through the OS keychain, see plugin-protocol.md);
- Fallback switch: setting `plugin_env_isolation=false` restores inheriting the host env (default `true`).

## Related Documents

- [plugin-manifest.md](plugin-manifest.md) — the `sha256` / `signature` fields and the install flow
- [supply-chain.md](supply-chain.md) — trust model rulings (D1) and the launch prerequisite checklist (D6)
- [key-ceremony.md](key-ceremony.md) — offline generation of the official keys, sharded backup, rotation, and disposal
- `fixtures/signing/README.md` — the golden-vector checklist and regeneration steps
