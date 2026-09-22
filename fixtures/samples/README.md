# fixtures/samples — official sample signing artifacts (public test material)

⚠️ **Tests only.** The signing key is `fixtures/signing/key.seed.hex` (a fixed test seed),
which **must never** be used for real releases; `trusted-keys.json` is likewise a test trust table.

## Contents

| File | Description |
|---|---|
| `things-demo-0.1.0.ocplugin` | signed build of `plugins/things-demo` through the full pipeline (pack → sign → auto-gate) |
| `weather-demo-0.1.0.ocplugin` | same for `plugins/weather-demo` (a sample object-form capability declaration) |

## Regeneration

```bash
cargo run -q --manifest-path src-tauri/Cargo.toml --bin opencapx -- \
  pack plugins/things-demo --key @fixtures/signing/key.seed.hex \
  --key-id com.opencapx.test-signing --out fixtures/samples/things-demo-0.1.0.ocplugin
# same for weather-demo
```

## Auto-gate pre-run

```bash
cargo run -q --manifest-path src-tauri/Cargo.toml --bin opencapx -- \
  verify-package fixtures/samples/weather-demo-0.1.0.ocplugin \
  --keys fixtures/signing/trusted-keys.json
```

Covered test: `core::plugin::tests::signed_samples_install_and_update` (installable and updatable).
