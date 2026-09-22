# Supply Chain and Listing Plan (Decision Document)

## 0. Conclusions

- **The trust model has two layers**: local trust (v1, HMAC, already implemented, retained) and distribution trust (Marketplace, Ed25519, v2). Core rationale: HMAC is symmetric — being able to verify means being able to forge, so it does not hold up for distribution (§3 D1)
- **Publisher key registration = official signed index + revocation list**, with the trust root pinned into the app release artifact (§3 D2)
- **Listing review = automated gates for everything + a manual channel for high risk**, promising only risk reduction, not elimination (§3 D3)
- **Unsigned-package UX moves from two states to three**: trusted direct install / warning confirmation / hard reject (§3 D4)
- **The prerequisites for launching the Marketplace are a hard checklist; missing any one means no launch** (§3 D6)

## 1. Threat Model

What we defend against:

| Attack | Scenario | Mitigation |
|---|---|---|
| Transport tampering | A mirror/man-in-the-middle replaces the package pointed to by downloadUrl | Package-level signing (hash verification + v2 Ed25519 already implemented) |
| Publisher impersonation | Signing a package with a fake keyId | Trust chain: official public key → signed index → publisher public key (D2) |
| Update poisoning | Listing a clean version, then slipping malicious code into a later update | Updates must use the same publisher key (D5) |
| Key leakage | A publisher's private key leaks | revokedKeys revocation + a local disable channel (D2/D5) |
| Key-swap attack | A malicious update swaps out the package's signing key | A key swap = a new publisher, going through user confirmation again (D5) |

What we do not defend against (honest boundaries):

- Users signing and installing their own packages (the entire point of the local trust channel)
- Malicious code installed after the user explicitly acknowledged a warning — runtime behavior is the Permission Manager's job; install review is not a sandbox
- OS-level escape; sandboxing is separate follow-up work and is not part of this plan (§4)

## 2. Current State Inventory (Already Implemented)

- `core/plugin_sig.rs`: archive hash (non-manifest entries inside the zip sorted by name, `name\nsize\nbytes` concatenated and SHA-256'd) + `signature.sig = HMAC-SHA256(secret, "opencapx-v1\n" + hash)`, with keyId → secret stored in `~/.opencapx/trusted-keys.json`
- `VerifyOutcome` six states: Unsigned / Trusted / UnknownKey / HashMismatch / BadSignature / MalformedSignature; verify before install, preview passes through status for the UI badge, and `list_trusted_keys` exposes only fingerprints
- Current policy (`is_allowed`): trusted-keys empty → unsigned is allowed (backward compatible); non-empty → missing/bad signature is **always hard-rejected**
- `marketplace.rs`: index.json (`entries[] + sha256`), remote URL or local seed; **the index itself is unsigned**
- Event: `plugin.signature.verified` is already on the EventBus

## 3. Decisions

### D1 The trust model has two layers: local HMAC (v1 retained) / distribution Ed25519 (v2)

HMAC being symmetric means: for a client to verify it must hold the secret, and holding the secret means it can forge. This holds up for a single machine or a team — the trust root is "the things I put into trusted-keys myself"; but if all Marketplace users share the secret, then all users can forge for each other, which does not hold up.

- The v1 local flow is **untouched**: HMAC, `trusted-keys.json`, the existing VerifyOutcome, the existing tests
- v2 distribution signing uses **Ed25519** (purely asymmetric: small signatures, fast verification, mature libraries):
  the manifest `signature` expands to `{ alg, keyId, sig }`, where `alg` defaults to `"hmac-v1"` for compatibility with existing packages; Ed25519 domain separation is `"opencapx-v2\n" + archive_hash`
- The archive hash algorithm is shared by both layers and does not migrate
- Decision record: before the Marketplace launches, do not introduce an asymmetric library for any intermediate need

> **Implementation note (2026-09-14):** D1's v2 is implemented — `alg:"ed25519"` dispatch + a `digest_v2` covering the manifest;
> the signed message is implemented as `Ed25519(SK, "opencapx-v2\n" ‖ digest_v2_hex)`, and the v2 `sha256` field stores the digest_v2 hex
> (v1 semantics unchanged, dispatched by alg). The local HMAC v1 channel is retained as is, with zero behavior change. For implementation details and commands, see
> [plugin-signing.md](plugin-signing.md). **This section's decision stands.**

### D2 Publisher key registration: official signed index + revocation list

- A publisher's identity = an Ed25519 keypair; keyId uses reverse-domain notation (`com.example`), in the same namespace as plugin ids
- index.json gains top-level fields:

```json
{
  "publishers": [
    { "keyId": "com.example", "publicKey": "<ed25519-hex>",
      "verified": true, "since": "2026-10-01" }
  ],
  "revokedKeys": [
    { "keyId": "com.evil", "at": 1760000000, "reason": "malware report #12" }
  ],
  "entries": [ "…(existing shape + publisherId)" ],
  "indexSignature": { "alg": "ed25519", "keyId": "com.opencapx", "sig": "…" }
}
```

- Client trust chain: **the pinned official public key** (distributed with the app release artifact; a version update is a rotation opportunity) → verify the index signature → publisher public key inside the index → verify the package signature. HTTPS is only a transport-layer optimization, **not the trust root** — mirrors, proxies, and local seed files are treated the same
- The local `trusted-keys.json` channel is retained for power users; the two coexist, and the UI labels the source (registry / local / unknown)
- Key rotation: the new keyId goes into `publishers`, the old key goes into `revokedKeys`; **an update that changes the key is treated as a new publisher** and goes through the D4 warning confirmation (to prevent key-swap)

### D3 Listing review: automated gates for everything + a manual channel for high risk

Automated gates (applied to everything, machine-run, checked on listing and re-checked on every update):

1. Manifest schema and consistency (the existing `validate_manifest`)
2. A valid signature chain (D2)
3. Static scanning: entry path traversal (already prevented) / per-package and total size limits (already present) / an install-script blacklist (the `curl … | sh` kind) / runtime commands must be explicitly declared and must not resolve user PATH injection points
4. Permission-capability consistency: the requested permissions and the provided capabilities match up per the mapping table in permissions.md

Manual channel (high risk only): plugins applying for HIGH_RISK permissions (`process.execute` / broad `filesystem.write` / `microphone` / `camera`) must meet three conditions: **source code public + manual review + publisher past an observation period**.

Publisher reputation: an observation period for new publishers (limited number of listings and update frequency); an incident → delisting + `revokedKeys` + an event to installed users (see D5).

Honest boundary: **review reduces risk, it does not eliminate it**. At runtime a plugin is still arbitrary process execution; the maximum promise of "manually reviewed" is "the source was read", not "guaranteed harmless".

### D4 Unsigned/unknown-source install UX: three states

Current problem: trusted-keys empty → unsigned is **silently allowed**; once the user installs any key → unsigned is **rejected entirely**. Between these two states there is no middle state of "source unverified but the user is informed", and the user has never been told this switch exists.

| verify result | Current behavior | Decision |
|---|---|---|
| Trusted | Direct install | Unchanged, green badge, direct install |
| Unsigned / UnknownKey | Allowed when keys are empty, hard-rejected when non-empty | **Yellow badge + explicit warning confirmation** ("source unverified, at your own risk"), with the confirmation written to audit (`plugin.installed.unsigned`) |
| HashMismatch / BadSignature / MalformedSignature | Hard reject | Unchanged, **no bypass under any circumstance** (if the package was touched, it is rejected) |

- Local trust mode (trusted-keys empty) **no longer silently allows**: it uniformly goes through the three states of "yellow badge + explicit warning confirmation"; the Settings page **"Allow unsigned packages" master switch** defaults to on, and **once off, unsigned/unknown-key is hard-rejected**.
- **Erratum (M5 implementation write-back)**: the original sentence "once off, unsigned goes through warning confirmation" conflicts with the table in the same section; the frozen master plan F7 takes precedence — **master switch OFF = hard reject, with no confirmation bypass under any circumstance**.
- Implementation surface (landed): `VerifyOutcome::allowance()` three states (Direct / SoftWarn / HardDeny); the install command's `confirmUnsigned` + `confirmKeyChange` parameters; the Settings page switch and warning dialog are wired up.
- Semantic correction record: unsigned is "source unverified", tampered/bad-signature is "verification failed" — the former is a user decision, the latter is a factual rejection, and they should not be lumped together

### D5 Update channel: same key auto-updates, key change re-confirms, revocation disables by default

- The update package's publisher key must match the installed version → silent auto-update; otherwise → treated as a new publisher and goes through the D4 warning confirmation
- A publisher key entering `revokedKeys` / a plugin being delisted: installed users receive a `plugin.revoked` event + a Settings page banner; **the plugin is disabled by default**, the user may explicitly re-enable it, and the re-enable is written to audit. Disable-on-revocation is the safe default; the right to re-enable belongs to the user

### D6 Marketplace launch prerequisites (hard checklist)

Missing any one means no launch:

1. D1 Ed25519 implementation (signing toolchain + verify) — implementation evidence in [plugin-signing.md](plugin-signing.md)
2. D2 signed index + official key pinned
3. D3 four automated gates (at least: signature chain + static scanning + permission consistency)
4. D4 three-state UX + master switch
5. D5 same-key updates + revocation channel

This checklist turns "when can we launch on the Marketplace" from a feeling into a verifiable state. Security debt compounds; fix the gates before opening the door.

## 4. Explicitly Out of Scope

- **OS-level sandboxing** (sandbox-exec/Seatbelt, containerized runtime): separate work, decoupled from the Marketplace, and does not block this plan
- **An endorsement promise for code-level manual audit**: for the high-risk channel, "the source was read" ≠ "guaranteed harmless"; the latter must not appear in marketing language
- **SBOM**: at the plugin ecosystem's current scale this is a ritual that produces no security gain
- **Third-party CA / web of trust**: the user base does not support decentralized trust; the official root + publisher registration is enough; revisit when there is a real need for cross-marketplace interoperability

## 5. Reconciliation with i2-evaluation

| i2-evaluation §3.2 gap | Corresponding decision |
|---|---|
| Publisher key registration system | D2 (official signed index + revocation list) |
| Listing review policy | D3 (automated gates + high-risk manual channel) |
| Unsigned-package install warning UX | D4 (three states + master switch) |
| (newly identified this round) HMAC symmetry fails in the distribution scenario | D1 (two-layer trust model + Ed25519) |
| (newly identified this round) update and revocation channel gap | D5 |

## 6. Suggested Implementation Schedule (Not a Commitment)

| Phase | Content | Dependency |
|---|---|---|
| v1.1 | D4 minimal version: three-state UX + master switch + `plugin.installed.unsigned` audit | None; doable on the existing HMAC |
| Early v2 | D1 Ed25519 + signing toolchain (candidate `opencapx sign` subcommand) | D1 |
| v2 | D2 signed index + D3 automated gates + D5 update/revocation | D1 |
| Marketplace launch | D6 checklist all green | All of the above |
