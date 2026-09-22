# Security Policy

OpenCapX runs other people's plugin code and passes agent requests to it. We take reports about that boundary seriously, and we would rather hear about a problem early than argue about whether it counts.

## Reporting a vulnerability

Please report privately. Do not open a public issue, pull request, or discussion for a security problem.

- Preferred channel: GitHub private vulnerability reporting — the repository's **Security** tab, then **Report a vulnerability**. This keeps the report, the discussion, and the fix in one place.
- Fallback: open a GitHub issue titled `Security: contact` with no details, and a private channel will be arranged. A direct email address will be added here once it is set up.

Include what you can:

- Affected version or commit, and platform
- A minimal reproduction, or the exact request / package that triggers it
- The impact you believe it has: code execution, permission bypass, signature bypass, data exposure, denial of service
- Any proof-of-concept files, with a note on whether they are safe to run

What to expect: an acknowledgement within 7 days, a first assessment within 14 days, and credit in the changelog once a fix ships, unless you ask to stay anonymous. This is a small project, so these are targets rather than a contract.

## Supported versions

Only the latest minor line is supported. At the moment that is the `0.4.x` series. Releases are cut from `v*` tags; security fixes land on `main` and ship in the next patch release of the current line. Older lines do not receive backports.

## In scope

- The Rust core: permission checks, capability routing, plugin lifecycle, the local HTTP and MCP entry points
- Package verification: signature checks, digest computation, registry index verification, trusted-key handling
- Plugin isolation: the macOS sandbox layer, process boundaries, reverse-call limits
- Handling of agent identity, secrets (the `secret:` config channel), and local request traces

## Out of scope

- A user who intentionally installs an unsigned or self-signed plugin after the three-state warning. Runtime behavior is then bounded by the Permission Manager, and install review is not a sandbox.
- Plugins doing what their declared permissions allow. If a permission is too broad for what it grants, that is a design report, please still send it.
- Vulnerabilities in third-party agents (Claude Code, Codex, OpenCode) or in macOS, Windows, or Linux themselves.
- Denial of service through resource exhaustion on a machine the attacker already controls.
- Anything that requires the user to disable a documented security control.

## Signing, keys, and supply chain

The trust model is described in these documents. Read them before reporting a signature or supply-chain issue so we are working from the same design:

- [docs/plugin-signing.md](docs/plugin-signing.md): the v1 (HMAC) and v2 (Ed25519) channels, package digests, exit codes, trusted keys, and the honest limits of each channel
- [docs/supply-chain.md](docs/supply-chain.md): the two-layer trust model, publisher key registration, revocation lists, the automatic review gate, and the three-state install policy
- [docs/key-ceremony.md](docs/key-ceremony.md): the offline key ceremony, key sharding and backup, public-key pinning, rotation (S6), and compromise response (S7)
- [docs/permissions.md](docs/permissions.md): the permission vocabulary, the two-layer check, and the audit trail
- [docs/launch-checklist.md](docs/launch-checklist.md): current evidence status for the signing and review mechanisms

If you find a way to make a package verify when it should not, to install without the required user confirmation, or to reach a capability the declared permissions do not allow, that is exactly the kind of report we want.

## Hardening baseline

Recent work tightened several edges. Reports that show any of the following can still be bypassed are valuable:

- Input size gate on plugin calls
- Reverse-call throttling and permission-request deduplication
- Trace and log retention bounds
- Safe mode (`--safe-mode`), which starts the core without loading any third-party plugin
- macOS sandbox enforcement for unsigned plugins

## No bounty

There is no paid bug bounty program. We will credit reporters in the changelog and in the release notes when they want it.
