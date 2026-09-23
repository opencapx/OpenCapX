# Contributing to OpenCapX

Thanks for helping. This is a Tauri app with a Rust core, a TypeScript settings UI, and two plugin SDKs (Python and TypeScript). The notes below are the working agreement for changes in this repository.

## Development environment

- Rust stable, installed through `rustup`. The crate lives in `src-tauri/`.
- Node.js 22 or newer, with `pnpm` 10. The TypeScript SDK's tooling needs 22+ (glob and type stripping); CI runs the frontend jobs on 20 and the ts job on 24. `pnpm-lock.yaml` is committed, so use pnpm for the root workspace.
- Python 3.12 or newer for the Python SDK and the Python example plugins.
- Tauri platform dependencies for your OS. On macOS the Xcode command line tools are enough; on Linux install the WebKitGTK and related packages listed in `.github/workflows/ci.yml`.

```bash
pnpm install
pnpm tauri dev
```

The frontend builds into `dist/`, which the Tauri build script expects. Run `pnpm exec vite build` once if you only want to compile the Rust side.

## Verification before you commit

All five command sets run from the repository root. A red result in any of them means do not commit.

```bash
cd src-tauri && cargo test --bin opencapx && cargo clippy --all-targets; cd ..
npx tsc --noEmit && pnpm run i18n:check
cd packages/ts-sdk && npm test; cd ../..
PYTHONPATH=packages/plugin-sdk python3 -m pytest packages/plugin-sdk/tests plugins/*/tests -q
# when node and python3 are both on PATH:
cd src-tauri && cargo test --bin opencapx install_grant_execute -- --ignored --test-threads=1; cd ..
```

Notes on the gate:

- The two `install_grant_execute` end-to-end tests are `#[ignore]` by default because they spawn real plugin processes. They must run serially (`--test-threads=1`).
- `pnpm run build` runs `i18n:check`, `tsc --noEmit`, and the Vite build in one step. `tsc --noEmit` alone is faster when you are iterating on the UI.
- The i18n check enforces that every key exists in all three catalogs (`en`, `zh-Hans`, and `vi`). Add the translations with the code, not after.
- `node scripts/bump-version.mjs --check` confirms the version is consistent across `package.json`, `src-tauri/Cargo.toml`, and `src-tauri/tauri.conf.json` before a release.

If a database-backed test fails, suspect the environment before the code. There was a corrupt-workspace incident on 2026-09-18; re-check with a temporary directory fixture when that class of test goes red.

### Git hooks

The gate also runs through [lefthook](https://lefthook.dev), installed by `pnpm install` (the `prepare`
script runs `lefthook install`; manual alternative: `pnpm exec lefthook install`).

- **pre-commit** runs the part of the gate that matches what you staged: Rust changes get
  `cargo clippy --all-targets` plus `cargo test --bin opencapx`, TypeScript changes get
  `tsc --noEmit`, catalog changes get `i18n:check`.
- **pre-push** runs the full five-set gate, serial end-to-end tests included.

`git commit --no-verify` / `git push --no-verify` bypass a hook for one run; do not make that a
habit. rustfmt is not part of the hooks yet — the tree predates it, and a whole-tree formatting
pass is a change of its own.

## Commit style

[Conventional Commits](https://www.conventionalcommits.org) — enforced by the `commit-msg` lefthook
(`pnpm exec commitlint --edit`, rules in `commitlint.config.mjs`); a rejected commit prints the
failing rule.

```text
feat(bubble): add a focus mode that promotes the most urgent session
fix(tray): sync the Show Bubble check when the settings page writes bubbleEnabled
docs(rules): document the danger guard's trust rule
chore(release): 0.1.1
```

Shape: `<type>[(scope)][!]: <description>`.

- **type** — one of `build chore ci docs feat fix perf refactor revert style test`.
- **scope** — optional, lowercase (`bubble`, `tray`, `rules`, `plugin`, `http`, `i18n`, …); use the
  subsystem the change lives in. Omit it when the commit genuinely spans the tree (version bumps).
- **`!`** — marks a breaking change; also add a `BREAKING CHANGE:` footer paragraph in the body.
- **description** — imperative mood, lowercase, no trailing period, ≤ 100 chars.

Rules:

- One logical change per commit. Do not mix a refactor with a behavior change.
- Use the body to explain why, not what. Focus on the constraint or failure the change addresses.
- Keep the diff focused. Revert unrelated formatting churn.
- Merge commits, reverts, and rebase fixups are exempt — commitlint's default ignore list, which
  recognizes the subjects git and the hosting platform write: `Merge branch …`, `Merge … into …`,
  `Merge pull request …`, `Merge remote-tracking branch …`, `Merged PR …`, `Automatic merge`,
  `Revert …`, `fixup!`/`squash!`/`amend!`. A hand-written `Merge …` subject matching none of those
  shapes is rejected.

History note: commits before this rule (and the v0.1.1 tag) predate it and were not rewritten.

## Where plugins live

- `plugins/` holds the example and fixture plugins used by the end-to-end tests: `echo-vision` (Python), `echo-vision-ts` (TypeScript, the node e2e fixture), `weather-demo`, `things-demo`, and `pet-blank`.
- `plugin-template/` is the minimal Python plugin that the repo-local scaffolder copies.
- `packages/create-opencapx-plugin/` is the `npm create opencapx-plugin` initializer and the TypeScript template it copies.
- `packages/plugin-sdk/` is the Python SDK (`opencapx_sdk`).
- `packages/ts-sdk/` is the TypeScript SDK (`@opencapx/sdk`).

When you add a capability to a plugin, register it in the capability table too. Capability names are a global namespace, and a new name needs to be recorded before anything implements it.

## Documentation sync

These documents are the specs, not commentary. If a change touches behavior they describe, update them in the same commit.

| If you change | Update |
|---|---|
| The stdio JSON-RPC wire protocol, lifecycle, error codes, or timeouts | [docs/plugin-protocol.md](docs/plugin-protocol.md) |
| Manifest fields or the `settings[]` control set | [docs/plugin-manifest.md](docs/plugin-manifest.md) |
| The capability list, schemas, metadata, or routing | [docs/capability.md](docs/capability.md) |
| Permission vocabulary, two-layer checks, audit, or scopes | [docs/permissions.md](docs/permissions.md) |
| Third-party permission domains | [docs/permission-domains.md](docs/permission-domains.md) |
| The MCP tool surface or subscription behavior | [docs/mcp.md](docs/mcp.md) |
| Event bus types or dispatch | [docs/events.md](docs/events.md) |
| Signing, digests, pack/verify, or trusted keys | [docs/plugin-signing.md](docs/plugin-signing.md) |
| The trust model or review policy | [docs/supply-chain.md](docs/supply-chain.md) and [docs/plugin-review.md](docs/plugin-review.md) |
| Anything user-visible in the UI | [docs/user-guide.md](docs/user-guide.md) and the i18n catalogs |

Language conventions:

- `docs/` is written in English.
- Code comments are in English.
- The root `README.md`, `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md`, `ARCHITECTURE.md`, `INSTALL.md`, and `ROADMAP.md` are in English, because they are the entry point for people who have not cloned the repo yet.
- User-facing strings go through the i18n layer. Never hardcode display text in the frontend.

## Change scope

The project keeps a narrow focus. Before proposing a large feature, check the plan for what is deliberately out of scope. Marketplace and plugin-store work, cloud sync, telemetry, and Linux/Windows sandbox backends are parked; they need a decision, not a pull request.

Security-relevant changes (permission checks, signature verification, sandbox behavior, install and update flows) should explain the threat they address in the commit body and add a regression test. A fix without a test that fails first is not finished.

For anything involving keys, releases, or signing identities, coordinate with the owner first. Private material must never enter the repository or CI logs. The current registry official key is CI-generated by an explicit owner decision (see the header note in [docs/key-ceremony.md](docs/key-ceremony.md)); the air-gapped ceremony key is reserved for the planned rotation and still follows the offline runbook.
