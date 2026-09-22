# Signing fixtures & golden vectors (M2)

> ⚠️ **TEST-ONLY.** The seed, public key and `trusted-keys.json` in this directory
> are public test material. They exist solely to pin the `digest_v2` byte format and
> the Ed25519 signature encoding across the Rust CLI and the Python SDK. **Never use
> these keys to sign a real plugin, and never add this public key to a production
> `~/.opencapx/trusted-keys.json`.**

## Files

| Path | Purpose |
|---|---|
| `key.seed.hex` | Fixed 32-byte Ed25519 seed (`00 01 02 … 1f`), lowercase hex + `\n`. |
| `key.pub.hex` | Derived public key for `key.seed.hex` (lowercase hex + `\n`). |
| `trusted-keys.json` | Trust entry `com.opencapx.test-signing → {alg:"ed25519", publicKey}`. |
| `plugin/` | Source plugin tree that is packed into the goldens. |
| `golden/signed.ocplugin` | `.ocplugin` produced by `opencapx pack` (manifest at root, Ed25519 v2 signature). |
| `golden/unsigned.ocplugin` | Same tree, manifest **without** `sha256`/`signature`; base for tamper transforms. |
| `golden/digest_v2.hex` | `signing::digest_v2(signed.ocplugin)`, taken from the pack manifest's `sha256`. |
| `golden/signature.hex` | The Ed25519 signature over `"opencapx-v2\n" ‖ digest_v2_hex`, taken from the pack manifest. |

## Consumers

- Rust: `core::plugin_sig::tests::golden_vectors_pin_digest_and_signature` and
  `golden_archive_verifies_trusted_with_fixture_keys` (read this dir via
  `CARGO_MANIFEST_DIR/../fixtures/signing`).
- Python SDK (M2 Task 4): must reproduce `digest_v2.hex` byte-for-byte and verify
  `signature.hex` with `key.pub.hex`.

## Regenerating the goldens

Run from the repo root. Only do this when the frozen `digest_v2` framing itself
changes (a deliberate format break) — otherwise these files must stay byte-stable.

```bash
# 1. Repack the signed archive (pack self-checks digest + signature before exiting).
cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- pack \
  fixtures/signing/plugin \
  --key @fixtures/signing/key.seed.hex \
  --key-id com.opencapx.test-signing \
  --out fixtures/signing/golden/signed.ocplugin

# 2. Refresh digest_v2.hex / signature.hex from the archive manifest.
python3 - <<'EOF'
import zipfile, json
g = "fixtures/signing/golden"
m = json.loads(zipfile.ZipFile(f"{g}/signed.ocplugin").read("opencapx-plugin.json"))
open(f"{g}/digest_v2.hex", "w").write(m["sha256"] + "\n")
open(f"{g}/signature.hex", "w").write(m["signature"]["sig"] + "\n")
EOF

# 3. Rebuild the unsigned base archive from the same tree (manifest unchanged).
python3 - <<'EOF'
import zipfile, os
src, out = "fixtures/signing/plugin", "fixtures/signing/golden/unsigned.ocplugin"
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    z.write(os.path.join(src, "opencapx-plugin.json"), "opencapx-plugin.json")
    for root, _, files in os.walk(src):
        for f in sorted(files):
            rel = os.path.relpath(os.path.join(root, f), src)
            if rel == "opencapx-plugin.json":
                continue
            z.write(os.path.join(root, f), rel)
EOF

# 4. Confirm the pinned vectors still verify.
cargo test --manifest-path src-tauri/Cargo.toml --bin opencapx golden_
```

`key.pub.hex` is only regenerated if `key.seed.hex` changes; derive it from the seed
with any Ed25519 implementation (it is validated against `signature.hex` by the
`golden_vectors_pin_digest_and_signature` test).
