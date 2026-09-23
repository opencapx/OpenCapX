# Release Process (applicable from the first release, v0.1.x, onward)

From code freeze to a user being able to install: who does each step, in what order, and what happens if a step is skipped. This document only covers verified paths; the detailed evidence is in `docs/launch-checklist.md`.

## 0. Responsibility Overview

| Step | Who | Notes |
|---|---|---|
| §1 Pre-release gate check | agent or owner | All items are script-verifiable |
| §2 Keys and secrets in place | owner | Involves private keys and accounts; the agent does not handle them |
| §3 Version, changelog, tagging | owner | Requires push permission |
| §4 Build and artifact verification | agent (watches CI) | Fix directly on failure |
| §5 npm publish | automatic in CI (Trusted Publishing) | owner for first-time setup and exceptions |

## 1. Pre-release Gate (all mandatory)

```bash
node scripts/bump-version.mjs --check          # the four version locations agree
cd src-tauri && cargo test --bin opencapx && cargo clippy --all-targets; cd ..
npx tsc --noEmit && npm run i18n:check
cd packages/ts-sdk && npm test; cd ../..
PYTHONPATH=packages/plugin-sdk python3 -m pytest packages/plugin-sdk/tests plugins/*/tests -q
```

Plus remote CI (`ci.yml`: rust / frontend / python / ts) all green. Local 970 passing does not mean it passed remotely (there are precedents: Linux-specific code, Node versions, and headless clipboard all surfaced only remotely); the remote result is authoritative.

## 2. Keys and Secrets in Place (before tagging in §3)

| Item | Location | Consequence if missing |
|---|---|---|
| updater private key + password | GitHub repo secrets `TAURI_SIGNING_PRIVATE_KEY` / `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | `v*` tag builds fail loud; **installed users cannot rotate keys smoothly — an irreversible incident** |
| Registry production public key | `plugin_sig.rs` `OFFICIAL_ED25519_PUBKEYS` (keyId `com.opencapx`) | ✅ In place: 2026-09-19 decision C, CI-PROD key (seed in secret `OPENCAPX_REGISTRY_SIGNING_SEED`); rotate to an air-gapped key per key-ceremony §6 before the ecosystem scales up |
| Apple certificate six-piece set | **Postponed indefinitely; no membership purchase without a commercial need** (2026-09-20 decision, see launch-checklist exemption 7) | No impact: when unconfigured the signing env is not exported, unsigned artifacts are produced automatically, and users clear them manually through Gatekeeper |

Private key generation rules: `.gitignore` locks down `*.key.hex`; the seed never enters the repository. The air-gapped ceremony key (key-ceremony S1–S5) is reserved for future rotation; the current CI key is generated locally, the seed goes into a secret, and the local copy has 600 permissions.

## 3. Version, Changelog, Tagging

```bash
node scripts/bump-version.mjs                  # derive from the commits since the last v* tag; writes the four version files
# ... edit CHANGELOG.md by hand ...
node scripts/bump-version.mjs --check          # the four version locations agree
git commit -am "chore(release): 0.2.0"
git tag v0.2.0 && git push origin v0.2.0
```

The version is derived from the conventional commits since the last tag — `feat` → minor, `fix`/`perf`/`revert` → patch, and a `BREAKING CHANGE` footer → minor while the major is 0. The derivation anchors on the last tag, so re-running it before tagging derives the same version again. `--dry-run` previews without writing; `--notes` prints the commits the derivation saw. 1.0.0 is a deliberate call: pass an explicit version (`node scripts/bump-version.mjs 1.0.0`) when the time comes.

The tag name is the version number (the `v` prefix is required; the `latest.json` verification step compares against it). Re-read the table in §2 before tagging.

## 4. Build and Artifact Verification (agent watches CI)

A tag push triggers `build.yml`: three-platform builds + updater artifacts + a `latest.json` verification step.
Verification points:

- All three platform jobs green
- `latest.json` exists and `--expect-version` passes (compared with the `v` prefix stripped)
- `verify-latest-json.mjs` self-test 6/6 (can be run locally first: `node scripts/verify-latest-json.mjs`)

Stop on any red; after fixing, tag again (increment the version number, never reuse a tag name).

## 5. npm Publish (Trusted Publishing, automatic in CI)

The `npm-publish` job in `build.yml` publishes two packages at the end of the tag build under an OIDC identity, with no token and no OTP.
Prerequisites (one-time, already configured): both packages' Settings → Trusted Publisher point to
repository `opencapx/OpenCapX`, workflow `build.yml`, environment left blank
(completed by the owner on 2026-09-20). Note: **a real OIDC publish has not happened yet** (all 0.1.x were a manual first publish plus an idempotent dispatch skip); the first tag build that bumps a package version is its first real test — if it goes red, check the publisher configuration first.

Version rules: changing package contents requires bumping the corresponding package version (npm forbids overwriting the same version; the server hard-rejects it);
an app release does not force an SDK version bump. The scaffolder depends on registry `^0.1.0` by default;
in-repo development uses `--local` to go through `file:` (since 0.1.1).

History: the first publish (0.1.0) was done manually by the owner because a granular token could not bypass OTP
(classic Automation tokens are no longer issued); the NPM_TOKEN secret has been deleted.

## 6. Emergencies

- **updater private key lost**: no more signed updates can be published; there is no remedy — you can only rotate the key and ask users to reinstall manually. Record the incident in the checklist.
- **Registry key leak/rotation**: add then revoke (see `docs/key-ceremony.md` S6); publish a new index with a larger `generatedAt`; clients use their cache as a watermark and reject older indexes.
- **Plugin publisher revocation/delisting**: follow `docs/operations.md` §3; the observation period and incident process are in `docs/plugin-review.md`.

## 7. Related Documents

- Checklist and evidence: `docs/launch-checklist.md` (§9 first-release checklist, §6.1 remote CI evidence)
- Key ceremony and rotation: `docs/key-ceremony.md`
- Supply chain and trust model: `docs/supply-chain.md`
- Package signing format and verification: `docs/plugin-signing.md`
- Operations incidents: `docs/operations.md`
