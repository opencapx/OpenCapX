# Plugin Review Handbook (F10 / D3)

> Scope: the **automated gate** and **manual channel** for registry listings, the publisher observation period, and incident handling.
> Same source as the implementation: `opencapx verify-package` (`src-tauri/src/core/verify.rs`).

## 1. Automated Gate (run in full on every submission/update)

Local pre-run (author/reviewer):

```bash
opencapx verify-package <file.ocplugin> [--keys trusted-keys.json] [--index index.json]
```

Outputs JSON (`checks[]`), exit code 0/1.

| # | Check | Contents | Failure example |
|---|---|---|---|
| 1 | manifest | schema + lexical / reserved domains / declaration consistency (same source as installation) | bad id, reserved domain, dangling permission |
| 2 | signature | package signature valid + keyId registered (publishers / trusted-keys) + not revoked | unsigned, bad signature, revoked key |
| 3 | static | .ocplugin ≤ 50MB; unpacked ≤ 256MB; path traversal; `curl\|sh`-style pipes; `shell -c` wrapping; plaintext credentials (private key blocks / AWS / GitHub / GitLab / Slack / API key patterns) | see the 10 attack samples in `core::verify::tests` |
| 4 | declaration | Mapping reconciliation: no duplicate capability IDs; mapped permissions listed in `permissions[]` | duplicate declaration, dangling mapping |
| 5 | dependency | Declared dependencies exist in registry `entries` | `com.example.ghost` |
| 6 | sandbox | Validity of the sandbox declaration (optional): `network ∈ {none,out}`; `fs.write ⊆ {plugin-data}`; pets must not carry one | `network: "host"`, `fs.write: ["/tmp"]` |

Registry CI (`registry/.github/workflows/validate.yml`) runs index signature + `generatedAt` monotonicity + the verification scripts;
the **attack-sample rejection matrix** is pinned continuously by `cargo test core::verify::tests` in the main repo (10 cases: unsigned / bad signature / revoked /
traversal / over-limit / duplicate declaration / missing dependency / shell -c / curl\|sh / plaintext credentials).

## 2. Manual Channel Triggers (D3)

> Response-time target (realistic for a single person): ≤72h, see [operations.md](operations.md) §5.

The following cases **must** be reviewed manually (the automated gate alone is not enough):

- Requests HIGH_RISK permissions (`process.execute` / broad `filesystem.write` / `microphone` / `camera` /
  `plugin.install`, etc., see `permission::HIGH_RISK`);
- A new publisher's **first release**;
- **Re-submission** after a revocation (`revokedKeys`);
- Domain conflicts (P2 `domains[]`, currently a stretch goal).

## 3. Publisher Observation Period

- Manual spot checks for a new publisher's first **2 versions** (whether contents match the declared permissions/capabilities);
- Incident process: **delist** (remove entries) ± `revokedKeys` registration ± an event for installed users
  (`plugin.revoked`; client disables by default + banner + explicit re-enable possible, see M4).

## 4. Relationship to the Client

- Client three states (M5): trusted installs directly / unsigned warns and confirms / tampered is hard-rejected;
- Revocation and key change (M4): `revokedKeys` → disabled by default; key change = new-publisher warning confirmation;
- **Review reduces risk but does not eliminate it**: the automated gate only catches known patterns and does not guarantee the absence of malicious behavior; the client three states and the revocation channel are indispensable layers of defense in depth.

## Related Documents

- [supply-chain.md](supply-chain.md) — D1/D2/D3/D4/D5 decisions
- [key-ceremony.md](key-ceremony.md) — official key ceremony (S5 index signing)
- [plugin-signing.md](plugin-signing.md) — author-side signing toolchain
- `registry/README.md` — the index and publisher process
