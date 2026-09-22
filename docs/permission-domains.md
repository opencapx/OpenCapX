# Plugin Permission Domains (Proposal)

> Status: **Proposal v0.2** (incorporates one round of adversarial review revisions + second-round re-check during P0 landing + §9 decisions; records in §10 / §11), pending final review.
> Related: [permissions.md](permissions.md) (permission model), [capability.md](capability.md) (capability registration), [supply-chain.md](supply-chain.md) (publisher trust).

## 1. Problem

Today, adding a third-party domain (such as `things.*`) requires touching three places in the Core and shipping a release with them:

| Change point | Location | Role |
|---|---|---|
| Capability ID registration | `core/capability.rs` `CAPABILITY_IDS` | Install-time "reject unknown capability" check and routing allowlist |
| Permission name registration | `core/permission.rs` `PERMISSIONS` | Vocabulary; unknown permissions are rejected outright at install |
| Capability→permission mapping | `core/permission.rs` `capability_permission()` | Enforcement basis for the two-layer gate (Agent / Plugin) |

Goal: a third party can declare a new domain (capability + permission + mapping) within its own namespace, the user confirms item by item at install time, and the new domain **no longer requires a Core code change and release**.

## 2. Current-State Inventory: The True Extent of the Gap (revised per review)

Install-time item-by-item confirmation (`confirm_install`), plugin-page display (`permission::view()`), and auditing — **these three already exist and are not changed**. But the gap is not only "where the vocabulary comes from" (the first draft underestimated it; review C3):

1. **The vocabulary is two-layered**: the Plugin layer (`capability::execute` → `capability_permission`) and the **Agent layer** (`rpc::tool_permission` → `gate_agent` → `identity::check_agent` → `default_decision`) both read only the static table; a new permission name not in `PERMISSIONS` is **denied by default at the Agent layer and cannot be authorized from the settings page** (`agent_view` only iterates the static table).
2. **Install/update are not transactional**: swapping the directory and writing the DB happen before user confirmation; "keep the old version after a rejection" is currently impossible (review C2).
3. **Validation lacks a lexicon**: capability/permission/plugin ids have no syntactic constraints; an unvalidated plugin id caused a **path traversal vulnerability** (review C4; **fixed in P0**, see §7 / §11 re-check 1).
4. Existing surfaces this proposal does not change: the install confirmation dialog, the plugin page, audit events, and the supply-chain foundation ([supply-chain.md](supply-chain.md)).

## 3. Invariants (revised)

1. **Enforcement is in the Core**; the "capability→permission" mapping is **frozen after user confirmation** — here "frozen" is a constraint relative to the **manifest / update flow**. **Honest boundary**: the plugin process runs as the same user as the Core and can read/write the database and key files under the user directory (see permissions.md "Threat Model"); this proposal **does not claim** to provide integrity protection against a running plugin, and P1 explicitly accepts this boundary (hardening directions: a separate OS user / sandbox / keychain sealing, in later versions).
2. **Reserved names cannot be occupied**: reserved domains (the first segment of built-in capabilities/permissions) cannot be occupied by plugin declarations; object forms must not use reserved capability IDs; declarations must not reference reserved permission names.
3. **Changes must be re-confirmed**: a new capability / mapping change / default change / declaration removal, and **any plugin update during P1**, must all be re-confirmed item by item.
4. Audit throughout (event list in §4.7).
5. A resolution miss → route denied; the two layers share **the same** resolver (§4.3).

## 4. Design

### 4.1 Two Trust Classes

**Built-in domains** (machine-resource surfaces): file / clipboard / network / screen / input / power / automation / PIM / system settings / printing, and so on — the Core has real enforcement points. Keep them closed: IDs, permission names, mappings, and the high-risk set can still only be changed by the Core. Plugins may "provide" implementations of these capabilities, and the permissions continue to follow the built-in mappings.

**Officially-catalogued domains** (such as `things.*`): **stay built-in long-term, not migrated to the declarative system** (decided 2026-09-14, §9-5; string form + static mapping, the `things` segment permanently reserved).

**Plugin domains** (pure routing surfaces): limited to **non-reserved, brand-new** domains (e.g. third-party `weather.*`). The plugin declares the capability→permission mapping in the manifest.

### 4.2 Declaration Shape and Lexicon

The `capabilities` element is extended to "string | object" (untagged parsing; old manifests work unchanged):

```json
{
  "capabilities": [
    "image.analyze",
    { "id": "weather.fetch", "permission": "weather.read", "default": "ask" }
  ],
  "permissions": ["weather.read"]
}
```

**Lexicon** (all comparisons byte-exact, no case/Unicode normalization; after NFKC normalization it must still be pure ASCII, otherwise rejected). **Scope**: constrains only names **newly declared by a plugin** (capabilities/permissions declared in object form + non-built-in names in `permissions[]`); **the built-in vocabulary is exempt** (`camera` / `microphone` are existing single-segment names, see §9.1 verification revision):

- Capability / permission names: `^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$`, two or more segments, no empty segments / leading or trailing dots / double dots, length ≤ 64;
- Plugin id (P0 hotfix, **already landed**): character set limited to `[a-z0-9.-]`, no `..` / leading or trailing dots / whitespace / non-ASCII, length ≤ 128; the full reverse-DNS form is preferred (the compatibility period retains old single-segment ids, marked deprecated). **The validation point = `validate_manifest` (manifest parse time)** — one interception covers both the tmp (`.tmp-{id}-{pid}`) and dest `join`s; `dest` / `tmp` additionally add a component-level `starts_with(plugins_root)` assertion as a fallback (re-check 1);
- Reserved sets: `RESERVED_CAPABILITIES = CAPABILITY_IDS`; `RESERVED_DOMAINS = {CAPABILITY_IDS ∪ the first segment of every PERMISSIONS entry} ∪ {"opencapx"}`.

**Validation rules** (any violation rejects the install):

| Rule | Content |
|---|---|
| Domain self-consistency | An object-form capability's `id` and `permission` must be in the same domain |
| Reserved-domain closure | `id` / `permission` must not fall in a reserved domain; object forms must not use reserved capability IDs (a reserved ID can only be a provider in string form) |
| `default` restricted | Only `ask` / `denied`; and it takes effect only for **non-reserved** permission names — a declaration **must not reference a reserved permission name** (including its granted default, preventing self-granting via the default value; review H5/H6) |
| Global consistency | All providers of the same capability ID (including string-form = providers following the static mapping) must have a **consistent effective mapping** (`permission` + effective `default`); inconsistent → reject |
| 1:1 | The first version maps one capability to one permission (isomorphic to the existing static mapping) |

### 4.3 Single-Point Resolver + Six Integration Points (revised: the original "only core change" does not hold)

`resolve_permission(capability)`: ① the reserved static table → ② the frozen declaration table → ③ a miss = None (route denied).

The "open vocabulary" requires **all** of the following call sites to integrate the resolver/declaration table (enumerated in review C3; all are required):

| # | Call site | Behavior |
|---|---|---|
| 1 | `capability::known()` | Routing admission: reserved ∪ declared |
| 2 | `permission::default_decision()` | The default for a new permission name = the declared `ask/denied`; **never implicitly granted** |
| 3 | `permission::confirm_install()` | Accepts declared permission names (otherwise a new-domain plugin cannot be installed); and declared permissions **force `can_always = false`** — currently this flag only looks at `HIGH_RISK`, so without blocking it the install dialog can still offer Always, and H2's once-only would miss the install enforcement point (re-check 3) |
| 4 | `permission::set_decision()` | The settings page can adjust declared permissions |
| 5 | `identity::check_agent()` / `set_agent_decision()` | Agent layer: a new permission name starts from the declared default and can be overridden from the settings page (otherwise a new-domain permission is denied by default and cannot be authorized) |
| 6 | `permission::agent_view()` / `view()` / `heatmap()` | Display declared permissions + an "unverified domain" marker |

The decision semantics of the two-layer gate are unchanged.

**once-only enforcement points (review H2 + re-check 7; both layers in sync, missing one leaks)**: declared derived permissions (name ∈ a live `capability_declarations.permission`) take effect consistently in four places —

1. `gate()`: `can_always = !HIGH_RISK.contains && !declared-derived` (currently permission.rs only looks at HIGH_RISK, and an Always answer writes `set_decision(granted)`, forming a permanent grant);
2. `gate_agent()`: same as above (currently isomorphic, writing `set_agent_decision(granted)`);
3. `set_decision()` / `set_agent_decision()`: **refuse to write `granted`** for declared derived permissions (the settings page can only ask / denied; it can turn them off but not permanently on);
4. Downgrade semantics: an "always" answer where always is not allowed → treated as **once** (following gate()'s existing high-risk downgrade pattern), not a full rejection of the call — an outdated frontend that is not yet in sync will not break; the settings-page UI hides the granted option (belongs to §6, the frontend surface).

### 4.4 Install / Update: consent-before-commit (revised: confirm before committing)

Install:

1. Read the manifest + lexicon validation (+ P2: signature validation). **Chicken-and-egg ordering**: for install-time capability validation, object forms do not go through the `capability::known` static table (the declaration is not yet persisted at this point, so the static table cannot contain it); instead, admit on "lexically valid + non-reserved domain"; string forms still go through the static table (re-check 6);
2. Compute the declaration diff (vs the frozen table);
3. Unpack into staging (without touching the installed version);
4. **Confirmation set = `permissions[]` ∪ all inline mapped permissions** (review M1), showing `capability → required permission` item by item in a dialog;
5. After all pass: stop → atomic directory swap → a **single transaction** writing `plugins / plugin_permissions / capability_declarations / domain_registry`; **the `plugin.installed` event and alerting hints installation likewise move to after this** — currently both are emitted before confirmation, leaving dirty records in the audit stream and the hints table after a user rejection (re-check 2);
6. If any step fails: clean up staging, **the installed version is unaffected** (review C2).

Non-interactive path (no UI: tests / CI → `confirm_install_noninteractive`): declared permissions are persisted per their declared default (`ask` → ask, `denied` → denied); the install succeeds but calls will be blocked by the gate — this is expected behavior; the audit event carries `caller: "install-no-ui"` to distinguish it, with no extra prompt (re-check 4).

Update:

- P1 **re-confirms on every update** (review H7: before the signature binding D5 is complete, no "silent implementation update" channel exists);
- Mapping change → affected permissions are reset to `ask` (review M3);
- Declaration removal → audit; if the removed permission is still referenced by a frozen entry, re-confirm (review M2).

> The existing implementation is "swap directory / persist first, confirm later"; **P1 must be reordered as above** (review C2).

### 4.5 Domain Ownership

- **P1 (local)**: reserved domains are pre-seeded in `domain_registry` (`source='core'`, non-occupiable, review H1); non-reserved domains are "claimed on first install", with conflicts rejected; the UI always marks them "unverified domain". Uninstall releases + a tombstone notice.
- **P2 (distribution)**: the signed index binds publisher → domain (the D2 extension); the lifecycle — claim / transfer (with D5: changing the key = a new publisher, and the domain does not automatically follow) / revoke / offline install / migration of already-claimed domains from P1→P2 — is documented in sync with D2 (review M5).

Honest boundary: P1 only prevents same-machine conflicts, not cross-machine squatting or malicious authors; full domain trust can only come from P2.

### 4.6 Storage (revised: composite key + consistency)

```sql
capability_declarations(
  capability       TEXT NOT NULL,
  plugin_id        TEXT NOT NULL,
  permission       TEXT NOT NULL,
  default_decision TEXT NOT NULL,          -- ask | denied
  confirmed_at     INTEGER NOT NULL,
  PRIMARY KEY (capability, plugin_id)
);

domain_registry(
  domain       TEXT PRIMARY KEY,
  plugin_id    TEXT,                       -- P1: local holder
  publisher_id TEXT,                       -- P2: signature source
  source       TEXT NOT NULL               -- core | local | registry
);
```

- Resolution: a capability is routable only when **all live providers have a consistent effective mapping**; uninstall deletes that provider's row and re-checks the remaining set; inconsistent → the capability is not routable (fail closed, review H4);
- Uninstall: release the domain (write a tombstone audit); reinstall = a fresh install going through full confirmation (no implicit renewal of the old freeze is accepted; review M2).

### 4.7 Audit and UI (new)

Events: `permission.declaration_frozen / declaration_changed / declaration_removed`, `domain.claimed / released / conflict`; `agent_view / view / heatmap` extend to declared permissions, with an "unverified domain" marker (review M6).

## 5. Security Analysis (revised)

| # | Threat | Mitigation | Residual risk (honest) |
|---|---|---|---|
| 1 | Under-reporting permissions | Freeze on declaration, enforce per declaration; signing/review; audit | **The Core cannot independently verify plugin semantics** — the security ceiling of a plugin domain = the publisher's trustworthiness |
| 2 | Domain squatting | P1 pre-seeded reserved domains + local conflict rejection; P2 signed index | P1 has no cross-machine protection |
| 3 | Update secretly changing mappings | diff forces re-confirmation | None |
| 4 | Shadowing reserved names | Reserved-domain closure + strict lexicon | None |
| 5 | Self-granting via the default value | Declarations must not reference reserved permission names; the non-interactive path does not auto-allow declared permissions | None |
| 6 | **Same-user integrity** | **Undefended**: the plugin process can write the DB / read keys (review C1); P1 explicitly accepts this and records it honestly | Hardening directions: a separate OS user / sandbox / keychain sealing |
| 7 | turn-malicious update (good-then-evil) | P1 **re-confirms on every update**; P2 converges after signing + same-key constraints | P1 has no signing, so authors cannot be distinguished |
| 8 | **Always laundering** | **P1: declared permissions (non-reserved names) are always once-only, with no Always — two layers, four enforcement points** (review H2 + re-check 7, see §4.3); P2 may unlock it for signed publishers — **P1 once-only already decided (§9-1)** | Beyond confirmation fatigue, the mechanism's backstop is limited |
| 9 | Provider ordering | An attacker can name itself `aaa.*` to grab the primary slot (priority is also 100, ordered by plugin_id lexical order, review H8); P1 at least provides explicit ordering in the settings page + honest documentation | Routing quality can be perturbed |

## 6. Migration and Compatibility

- Existing plugins need **zero changes** (the new fields are optional, string forms work as before); built-in domain behavior is completely unchanged; `things.*` is not migrated in P1;
- The integration-point list (§4.3) is refactored item by item, each with its own tests; subscribe-type plugin capabilities are **not supported in P1** (`SUBSCRIBE_IDS` remains hardcoded, review M4), and the scope is declared as call-type;
- **Frontend change surface (re-check 5)**: settings.ts — the install confirmation dialog hides the Always button for declared permissions (the UI face of once-only), `agent_view` renders the "unverified domain" marker, and the plugin page shows declared mappings. §4.3 lists only the core-side call sites; the frontend must be scheduled together;
- Test surface: the lexicon rejection table (including case/homoglyph/`..`), reserved domains, the **path traversal regression (review C4)**, declaration consistency (multiple providers), diff re-confirmation, transaction rollback (old version intact after rejection), and two-layer authorization (the new permission name is authorizable at the Agent layer).

## 7. Phasing (revised)

| Phase | Content | Acceptance criteria |
|---|---|---|
| **P0** (hotfix, independent of this proposal; **landed 2026-09-14**) | Plugin id lexicon validation at `validate_manifest` parse time (one place covering both the tmp / dest `join`s) + component-level `starts_with(plugins_root)` assertions on `dest` / `tmp` | A malicious id (`../..` and 12 other counterexamples) is rejected at manifest parse time (regression tests `manifest_rejects_traversal_plugin_id` / `plugin_id_lexicon_accepts_legal_ids`); the live test suite is green (review C4; re-check 1) |
| P1 | All of §4: consent-before-commit, single-point resolver + 6 integration points, reserved domains, lexicon, **declared-permission once-only (four enforcement points)**, **full re-confirmation**, call-type scope | Third-party new-domain plugin install → call → update re-confirm → once-only (Always downgraded / no granted write from the settings page) all pass; all tests green |
| P2 | Signed-index domain attribution + lifecycle + silent updates + Always unlock | In sync with supply-chain D2/D5; cross-machine domain conflict cases |

## 8. Alternatives and Rejections

- **B. Stay closed + a catalogue process**: zero mechanism cost; the ecosystem ceiling = the Core team's throughput. Acceptable as the status quo, not as the end state.
- **C. Fully open (including built-in domains)**: plugins could remap built-in permissions → the enforcement semantics would be hollowed out; **rejected**.

## 9. Open Questions (all decided 2026-09-14, all conservative)

| # | Question | Decision |
|---|---|---|
| 1 | Always policy | **P1 once-only** (two layers, four enforcement points, §4.3); the unlock form is revisited after P2 signing |
| 2 | Provider ordering | P1 only documents it + the settings page displays declared-domain providers; the full ordering UI is deferred (§5-9 records the residual risk honestly) |
| 3 | Domain segment count | First version uses single-segment domains (the lexicon allows multi-segment names; `domain_registry` P1 accepts only single-segment), reserving room to extend |
| 4 | Declaration-table queries | Startup cache + install/uninstall invalidation hooks |
| 5 | things.* migration | **Stays built-in long-term, not migrated** (§4.1 in sync); the declarative acceptance template instead uses a third-party new domain (weather-demo) |

## 9.1 Landing Order (each step independent, rollback-safe, zero behavior change for existing plugins)

| Step | Content | Nature | Status |
|---|---|---|---|
| Step 0 | The C4 minimal hotfix: id lexicon + dest/tmp boundary assertions + regression tests | Plug the hole first | **Done** |
| Step 1 | Full validation layer: capability/permission lexicon + reserved sets | Seal the input surface (note: capability/permission names are currently already rejected by the static table; this step mainly serves Step 3; its order with Step 2 is interchangeable) | **Done** (`permission::valid_name` / `reserved_capability` / `reserved_domain` + lexicon rejection table / reserved-set tests) |
| Step 2 | Install/update reordering: consent-before-commit + single transaction + `plugin.installed`/alerting hints moved after confirmation (re-check 2) | Make "confirmation" trustworthy — **the live risk is in this step** (currently a rejection destroys the old version); if prioritized by risk this can precede Step 1 | **Done** (`confirm_install` only collects; `write_install_tx` single transaction; `after_install` side effects moved later; `.bak` fallback for the directory swap + regression `failed_install_keeps_previous_version_intact`) |
| Step 3 | Declaration-system core: resolver + 6 integration points + once-only four enforcement points (re-check 7) + new tables + update diff | The P1 body | **Done** (see §12) |
| Step 4 | Template acceptance: a third-party new-domain plugin (weather-demo) end to end — install→confirm→call→update re-confirm→once-only (Always downgraded/hidden)→no granted write path from the settings page | Prove it really works | **Done** (`plugins/weather-demo` + Rust E2E + 15 pytest; see §12) |
| P2 | Signed domain attribution | Wait for supply-chain D2/D5, not started this round | — |

"Zero behavior change" verification (2026-09-14, **revised**): the built-in vocabulary is **exempt** from the lexicon — empirically, `PERMISSIONS` contains two **single-segment** names, `camera` / `microphone`, and applying the §4.2 lexicon to the built-in vocabulary would directly mis-reject existing permissions (the demo plugin's preview case hit it first). So the lexicon constrains only names **newly declared by a plugin** (id/permission declared in object form + non-built-in names in `permissions[]`); the string-form capability names of already-installed plugins all go through the `is_builtin()` allowlist and are unaffected by the lexicon. Step 2's observable change = the `plugin.installed` event and alerting hints move later (a correctness fix, recorded in the release notes).

## 10. Review Record (basis for the v0.1 revision)

One round of adversarial review (2026-09-14) concluded: **the v0 draft cannot pass P1**; the following are handled item by item:

| ID | Severity | Summary | Handling |
|---|---|---|---|
| C1 | Critical | "Freeze the mapping, plugins cannot change it" does not hold at the same-user boundary | §3-1 / §5-6 changed to an honest boundary + later hardening directions |
| C2 | Critical | Install/update are not transactional; "keep the old version after a rejection" is impossible | §4.4 reordered to consent-before-commit |
| C3 | Critical | The Agent layer is a second closed vocabulary; new-domain permissions are denied by default and cannot be authorized | §2 / §4.3 single-point resolver + the 6 integration-point list |
| C4 | Critical | An unvalidated plugin id → install-time path traversal (an existing vulnerability) | §7 P0 hotfix (independent of the proposal) |
| H1 | High | No reserved-domain list → P1 squatting could lock up official domains | §4.2 reserved sets + §4.5 pre-seeding |
| H2 | High | Plugin-domain permissions could always be Always → authorization laundering | §5-8 once-only (pending decision) |
| H3 | High | No lexicon → case/homoglyph/trailing-dot lookalike names | §4.2 lexicon |
| H4 | High | Multi-provider declaration attribution / reference counting undefined | §4.6 composite key + consistency |
| H5 | High | The shadowing rule contradicts the example; "built-in" is defined unclearly | §4.2 reserved-domain closure (the example is changed to weather.\*) |
| H6 | High | Priority between a declared default and the built-in default; self-granting bypass | §4.2 forbids referencing reserved permission names |
| H7 | High | A malicious update with unchanged declarations (good-then-evil) | §4.4 P1 full re-confirmation; P2 signing converges |
| H8 | High | Provider ordering could have its primary slot grabbed | §5-9 + §9-2 |
| M1 | Medium | The confirmation set was not derived from inline declarations | §4.4 step 4 union |
| M2 | Medium | The diff did not cover removals; uninstall/reinstall reset the freeze | §4.4 / §4.6 |
| M3 | Medium | Reconcile default freezing with existing plugin_permissions grants | §4.4 mapping change resets to ask |
| M4 | Medium | Subscribe-type plugin capabilities are unsupported | §6 scope declaration (call type) |
| M5 | Medium | P2 depends on an unimplemented signature; domain lifecycle undefined | §4.5 P2 + in sync with D2 |
| M6 | Medium | The audit/UI surface is undefined | §4.7 |

## 11. Second-Round Re-check Record (2026-09-14, during P0 landing)

Checking against the live code item by item confirmed all §10 conclusions (C2 = `install_ocplugin` swaps the directory first, `install_from_dir` persists before `confirm_install`; C3 = `agent_view` only iterates `PERMISSIONS`, `set_agent_decision` rejects unknown names, `default_decision` denies on a miss; C4 = `validate_manifest` does not check `m.id`, `dest = root.join(&m.id)` + `remove_dir_all`; H8 = priority hardcoded to 100, `ORDER BY priority ASC, plugin_id ASC`). Six further findings were also discovered, all merged into the body:

| # | Finding | Landing | Status |
|---|---|---|---|
| 1 | P0's scope was underestimated: the tmp path `.tmp-{id}-{pid}` also embeds m.id, so a dest-only assertion is not enough; validation was moved earlier to `validate_manifest`, one place covering both `join`s | §4.2 / §7 | **Fixed along with P0** |
| 2 | Pre-consent side effects are more than the directory + DB write: the `plugin.installed` event and alerting hints are also emitted before confirmation | §4.4 step 5 | In the proposal (a P1 change item) |
| 3 | Once-only enforcement-point gap: `confirm_install`'s `can_always` only looks at `HIGH_RISK`, so without forcing false the install dialog can still offer Always | §4.3 #3 | In the proposal (a P1 change item) |
| 4 | The non-interactive install path (`confirm_install_noninteractive`) had undefined behavior for declared permissions | §4.4 | In the proposal (rule: persist per declared default, distinguished by `caller`) |
| 5 | The frontend change surface (settings.ts: removing Always from the dialog, the "unverified domain" marker) was not listed, and the effort estimate was too low | §6 | In the proposal (a scheduling item) |
| 6 | Chicken-and-egg ordering: an object-form capability cannot pass `capability::known` before the declaration is persisted; install-time validation was changed to "lexically valid + non-reserved domain" | §4.4 step 1 | In the proposal (a P1 change item) |
| 7 | Once-only covering only the install dialog is leaky: at runtime, the Always answers of `gate()` / `gate_agent()` each write to a permission table forming a permanent grant, and `set_decision` / `set_agent_decision` write granted freely for known permissions — four permanent-grant channels in total | §4.3 four enforcement points + §5-8 / §9-1 decision | In the proposal (a P1 change item; including the always→once downgrade semantics) |

## 12. Implementation Record (2026-09-14, Step 3 + Step 4)

### Landed Artifacts

| Layer | Artifact | Content |
|---|---|---|
| Storage | `storage.rs` | New tables `capability_declarations` (PK `capability,plugin_id` + `permission` index) + `domain_registry`; new fallible write entry `try_with_conn` (used by paths needing a transaction + error propagation; `with_conn` only returns the row count) |
| Resolution | **New module** `core/declaration.rs` | Single-point resolution `resolve()` (① built-in static table → ② frozen declaration table → ③ None); `is_declared_permission` / `declared_default` / `declared_permission_defaults` / `conflicting_provider`; domain `claim_domains_in_tx` / `delete_for_plugin` / `domains`; `write_in_tx` (declaration + domain claim, in the same transaction as the commit) |
| Manifest | `plugin.rs` | `CapabilityDecl` (**untagged** `String \| {id,permission,default}`) + `Manifest::capability_ids/declarations/declared_permission_names`; `validate_manifest` applies all of §4.2's rules; `install_ask_plan` = the M1 union (preview and confirmation are **same-source**) |
| Integration points | `capability.rs` | `is_builtin()` (the old `known` semantics) / `known()` = reserved ∪ declared; `execute` takes its permission from `declaration::resolve` |
| | `rpc.rs` | Agent layer `tool_permission` goes through the resolver (returns `Option<String>`) |
| | `permission.rs` | `default_decision_for` / `known_or_declared` / `is_declared` / `can_always` (the predicate function, shared by gate and gate_agent); `check`/`view`/`agent_view`/`heatmap` connect to the declaration table; `InstallAsk` + the confirmation phase only collects |
| | `identity.rs` | `check_agent` defaults to the declaration; `set_agent_decision` rejects granted for declared |
| once-only | All four enforcement points landed | 1 `gate()` can_always · 2 `gate_agent()` can_always · 3 `set_decision`/`set_agent_decision` reject granted · 4 "answered Always but Always is not allowed" → treated as once (not a full rejection). `main.rs`'s two commands give a readable reason; `settings.ts` hides the granted option for `declared` + a "third-party domain · this time only" badge (`permDeclared` ×3 locale) |
| Lifecycle | `plugin.rs` | `write_install_tx` writes `plugins` + `plugin_permissions` + declarations + domain claims in a single transaction; only `after_install` emits the event/hints/probe/start; `uninstall` releases domains + deletes declarations |
| Template | `plugins/weather-demo` | A brand-new domain `weather.*` (declared in object form); `weather.current→weather.read(ask)`, `weather.set_home→weather.write(denied)`; offline determinism; 15 pytest all green |

### Deviations and Judgments (honest record)

1. **The declaration table does not use an in-process cache** (a deviation from the §9-4 decision "startup cache + invalidation hooks"). Rationale: profile hot-switching (`replace_shared_store`) and store swaps in tests both invalidate a global cache, and the cache sits on the **authorization path**, so missing one invalidation point means privilege escalation. It was changed to per-query (the same as `check` / `providers`); a point query on in-process SQLite is microsecond-level; the function boundary is left in place, so adding a cache later will not affect callers.
2. **The lexicon only governs the plugin's newly-declared surface**, with the built-in vocabulary exempt (reason in the §9.1 verification revision).
3. **A new name in `permissions[]` must be provided by an object-form declaration in the same manifest** (it must not dangle): this preserves the "declare-then-freeze" semantics while keeping the existing test surface of "unknown permission rejects the install".
4. Fixed an **existing panic** along the way: `PluginManager::stop` used `Arc::try_unwrap(..).unwrap_or_else(unreachable!("sole owner"))`, which panics whenever an in-flight capability call or the watchdog holds the `Arc<PluginProcess>` (reproducible by a kill switch hitting a concurrent call); `PluginProcess::shutdown` actually only needs `&self`, so after changing the signature `stop` calls `p.shutdown()` directly.
5. **The commit phase is ordered before the directory swap** (opposite to the literal order in §4.4 step 5). §4.4 says "stop → atomic directory swap → single-transaction write"; the implementation is "confirm → single-transaction DB write → stop/swap directory (.bak as a fallback, rolling the directory back on failure)". Rationale: of the two failures, **DB write failure is more common** (storage unavailable / lock), and swapping the directory first would expose a half-installed state where "the old version is deleted and the new version is not persisted" — precisely the thing C2 aims to eliminate. Writing the DB first leaves the old directory untouched when the DB fails; when the directory rename fails, `.bak` moves it back. Regression: `failed_install_keeps_previous_version_intact` (the old implementation would necessarily fail).

### Remaining Gaps (out of scope this round)

- **The interactive-dialog branches lack automated coverage**: the dialog branches of `gate` / `gate_agent` / `confirm_install` need an `AppHandle` (`app_handle()` is `None` in unit tests), so the "Allow once allowed" hop can only be verified manually/E2E; the **fail-closed** behavior with no UI has tests. The existing code is the same.
- `alerting hints` belongs to the same commit phase as the plugin row but is written in two segments (hints are not among the four tables listed in §4.4); if the hints write fails, the plugin row is not rolled back.
- All P1 limitations remain: no cross-machine squatting protection, no signing (see the §4.5 honest boundary).

### Verification

- `cargo test`: 735 passed (3 `core::vision` failures are pre-existing network-dependent cases that also fail on a clean tree).
- `npx tsc --noEmit`: clean.
- `plugins/weather-demo`: 15 pytest passed.
- New regressions: `manifest_accepts_declared_domain_capability` / `manifest_rejects_reserved_domain_declarations` / `manifest_rejects_malformed_declarations` / `manifest_accepts_builtin_single_segment_permissions` / `resolve_prefers_builtin_then_declarations` / `resolve_fails_closed_on_inconsistent_providers` / `domain_claim_is_first_come_and_rejects_reserved` / `uninstall_releases_domain_and_declarations` / `once_only_judgement_covers_declared_and_high_risk` / `declared_permission_uses_manifest_default` / `weather_demo_declared_domain_end_to_end`.
