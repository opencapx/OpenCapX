# fixtures/registry — registry client fixtures (public test material)

⚠️ **Tests only.** The official keys, index, and version entries here **must never** be used in production:
the production `OFFICIAL_ED25519_PUBKEYS` is written by the M8 offline key ceremony and has nothing to do with this directory.

## Contents

| File | Description |
|---|---|
| `official.seed.hex` | fixed test seed of the official signer (64 hex) |
| `official.pub.hex` | the corresponding public key (test cases inject it via `OPENCAPX_REGISTRY_OFFICIAL_KEYS`) |
| `index.unsigned.json` | unsigned index: publisher = the M2 golden package's signing key (`com.opencapx.test-signing`), one `com.example.weather-demo` version |
| `index.signed.json` | signed by `sign-index` (the signature-verification target) |
| `index.tampered.json` | a field changed after signing (verification must reject it) |
| `index.old.unsigned.json` / `index.old.signed.json` | a valid signed index with a smaller `generatedAt` (anti-replay case) |
| `package.sha256.hex` | file-byte SHA-256 of the golden `.ocplugin` package (matches the index's `versions[].sha256`) |

Package-side material lives in `fixtures/signing/` (golden package = `golden/signed.ocplugin`); the
publisher public key in this directory is the same as `fixtures/signing/key.pub.hex` — that is what closes the loop across fixtures in the registry trust chain.

## Regeneration

```bash
# 1. Official test identity (fixed seed → public key)
python3 - <<'PY'
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization
seed = bytes.fromhex(open("fixtures/registry/official.seed.hex").read().strip())
sk = Ed25519PrivateKey.from_private_bytes(seed)
open("fixtures/registry/official.pub.hex", "w").write(
    sk.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw).hex() + "\n")
PY

# 2. Package file hash (only needed when content changed: update sha256/sizeBytes/signature in the index)
shasum -a 256 fixtures/signing/golden/signed.ocplugin

# 3. Sign (inject the official public key via OPENCAPX_REGISTRY_OFFICIAL_KEYS when verifying)
OPENCAPX_REGISTRY_OFFICIAL_KEYS="com.opencapx.test-official=$(cat fixtures/registry/official.pub.hex)" \
  cargo run -q --manifest-path src-tauri/Cargo.toml --bin opencapx -- \
  sign-index fixtures/registry/index.unsigned.json \
  --key @fixtures/registry/official.seed.hex --key-id com.opencapx.test-official \
  --out fixtures/registry/index.signed.json

# 4. Tampered sample (signature left untouched)
python3 - <<'PY'
p = "fixtures/registry/index.signed.json"
s = open(p).read().replace('"Weather Demo"', '"Weather Demo X"', 1)
open("fixtures/registry/index.tampered.json", "w").write(s)
PY

# 5. Old index (anti-replay)
OPENCAPX_REGISTRY_OFFICIAL_KEYS="com.opencapx.test-official=$(cat fixtures/registry/official.pub.hex)" \
  cargo run -q --manifest-path src-tauri/Cargo.toml --bin opencapx -- \
  sign-index fixtures/registry/index.old.unsigned.json \
  --key @fixtures/registry/official.seed.hex --key-id com.opencapx.test-official \
  --out fixtures/registry/index.old.signed.json
```
