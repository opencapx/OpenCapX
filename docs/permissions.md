# Permission Spec

Permission checks must happen in the **Rust Core**, synchronously, before a capability executes. The WebView is only the presentation layer and is never a security boundary.

## Layers

```text
Agent identity (who is this: authentication)
  ↓
Agent permission (what this Agent can call this time)
  ↓
Plugin permission (what this plugin declared and was granted)
  ↓
Resource permission (scope: which paths, which domains)
  ↓
OS
```

The identity layer and the Agent permission layer are described below under "Agent Identity"; the Plugin + Resource layers are implemented in v1.

## Agent Identity

### Motivation

The Core listens on `127.0.0.1:47628`; without authentication, **any process on the machine can call every capability directly via `POST /rpc`**. With no caller identity, permission records have no subject, and "independent per-agent authorization" is impossible.

### Principal Model

```text
AgentIdentity (security principal, new in this section)
  ≠ Session (the existing session observation in core/agent.rs: status, project, messages)
```

Session continues to represent "what the Agent that OpenCapX sees is doing"; AgentIdentity represents "who is calling OpenCapX". One Claude host corresponds to one AgentIdentity and may have multiple Sessions.

| Field | Shape | Description |
|---|---|---|
| `agent_id` | `ag_<kind>_<4hex>` | e.g. `ag_claude_7f3a`, kind ∈ `claude \| codex \| opencode \| custom` |
| `display_name` | string | Shown in the settings page |
| `token` | `ocx1_<43 random>` | Appears only in the registration response and the token file; the Core stores only SHA-256 |
| `status` | `active \| revoked` | revoked means everything is immediately denied |

v1 has one identity per kind; multiple instances of the same host share it (the limitation is recorded here; it will be subdivided in v2).

### Registration (TOFU)

When `opencapx mcp` and `opencapx hook` start:

1. Detect the kind: environment variables (such as `CLAUDECODE`) or the parent process name; the `--agent <name>` argument / `OPEN_CAPX_AGENT` environment variable forces it (custom channel)
2. Read the local token file; if absent, `POST /agents/register {kind, display_name}`
3. The Core sees this kind for the first time: it creates the identity, generates a token, returns it, emits an `agent.registered` event, and the pet bubble shows "Claude connected" (informational only, **no permission dialog** — registration unlocks no capability; the default decision is still ask/denied)
4. The CLI writes the token to `<Core data dir>/agent-tokens/<agent_id>.token` with mode 0600

Why registration shows no dialog: the ask/denied defaults at the capability layer already block dangerous operations; registration only establishes the subject. A dialog would only create fatigue.

### Request Authentication

`POST /rpc` and `POST /event` always carry:

```text
Authorization: Bearer ocx1_…
X-OpenCapX-Agent: ag_claude_7f3a
```

The Core looks up the table by agent_id → compares the token hash in constant time → checks status. If any of the three checks fails:

```json
{ "ok": false, "error": "unauthenticated", "code": 40101 }
```

`code: 40102` means the identity is known but revoked. Both write an `auth.rejected` audit entry.

- Anonymous requests (no header): always rejected, no fallback channel
- The `/admin` page and SSE `/events` are local read-only observation; v1 keeps the status quo and does not include them in this model
- Dev mode `OPEN_CAPX_DEV=1` skips authentication, local development only

### Revocation and Recovery

- Settings page Agents view: revoke (revoked, the token is invalidated immediately), re-authorize (issues a new token; the old token becomes invalid)
- In the revoked state the CLI's automatic re-registration is rejected (40102); recovery can only happen through the settings page — a revocation can only be undone by an explicit user action

### Threat Model (Honest Boundary)

What this section defends against and what it does not:

**Does not defend against**: a malicious process running under the same user. It can read the token file; the token is not a barrier against malware.
**Provides**: a subject for permissions and auditing; per-Agent scope tightening/loosening; one-click disconnection of an Agent; independent default decisions for multiple Agents.
**v3+ hardening directions** (not promised): token in the OS keychain, short-lived per-session tokens, Unix domain socket + peer credentials.

## Permission Inventory

The Agent layer and the Plugin layer **share the same inventory** (the same vocabulary, two authorization tables).

| Permission | Description | Default |
|---|---|---|
| `pet.animation` | Drive pet animation/state (including say / set_state / ask) | granted (low risk) |
| `storage.local` | Read/write the plugin's own data directory | granted |
| `notification.post` | Post system notifications (opencapx.notify; anti-spam and anti-phishing) | ask |
| `image.read` | Read image files | ask |
| `file.read` | Read files (subject to scope) | ask |
| `clipboard.read` | Read the clipboard | ask |
| `clipboard.write` | Write the clipboard (paste surface; lower risk than file writes, not high risk) | ask |
| `network.request` | Network requests | ask |
| `browser.control` | Control the browser (open/read pages) | ask |
| `screen.capture` | Screen capture | ask |
| `microphone` | Microphone | denied |
| `camera` | Camera | denied |
| `filesystem.write` | Write files (subject to scope) | denied |
| `process.execute` | Execute arbitrary commands | denied |
| `automation.control` | Drive other applications (AppleScript automation) | ask (v1.3, high risk) |
| `input.control` | Synthesize keyboard/mouse input | denied (v1.3, high risk) |
| `plugin.install` | Install plugins | denied (v1.3, high risk) |
| `photos.read` | Read the photo library | ask (v1.3) |
| `contacts.read` | Read contacts | ask (v1.3) |
| `calendar.read` | Read the calendar | ask (v1.3) |
| `location.read` | Read geolocation | ask (v1.3) |
| `audio.output` | Play local audio (sound output) | ask (v1.3) |
| `url.scheme.open` | Open non-http(s) URL schemes | ask (v1.3) |
| `media.control` | Control media playback (play/pause/skip track) | ask (v1.4) |
| `messages.read` | Read iMessage history (full disk access surface) | denied (v1.4, high risk) |
| `window.management` | Enumerate / focus other applications' windows | denied (v1.4, high risk) |
| `power.control` | Sleep / lock the screen (machine-global; irreversible for running tasks) | denied (v1.4, high risk) |
| `notes.read` | Read notes | ask (v1.4) |
| `reminders.read` | Read reminders | ask (v1.4) |
| `reminders.write` | Write reminders | ask (v1.4) |
| `mail.read` | Read the mail inbox (phishing surface) | ask (v1.4, high risk) |
| `system.settings` | Change appearance / wallpaper / volume | ask (v1.4) |
| `printer.control` | Submit print jobs | ask (v1.4) |
| `things.read` | Read Things data (list / show / search) | ask (v1.5) |
| `things.write` | Write Things data (create / update) | ask (v1.5) |

Adding a permission = editing this inventory + minor version bump with the major version unchanged; if a plugin declares an unknown permission, installation is rejected outright. `plugin.install` is a vocabulary placeholder: no capability currently maps to it, and plugin installation goes through a manual action in the settings page; it is in the table so that a permission subject exists for a future direct CLI install.

The v1.5 batch is **purely additive**: it adds only the two rows `things.read` / `things.write` (both ask) and rewrites no existing permission mapping, so there is no migration action and no major-version break.

Core-native tools (opencapx.say / notify / set_state / ask) involve no plugin and pass only through the Agent layer: `say / set_state / ask` map to `pet.animation`, and `notify` maps to `notification.post`; `execute` / `subscribe` (v1.1) map by capability name (e.g. `image.analyze`→`image.read`, `file.watch`→`file.read`; checked once when the subscription is established, not per event push), and `unsubscribe` / `list_capabilities` read metadata only and do not pass the gate. v1.2: `speech.synthesize` is remapped to `notification.post` (formerly `storage.local`, see "Migration and Compatibility"); `system.permission_status` (upfront OS permission probe) reads metadata only and belongs to the same exempt class as `list_capabilities`, so it does not pass the gate — it produces no user data; its purpose is for an agent to check "Screen Recording: not authorized" before calling `screen.capture` and decide whether to work around it. v1.3 batch mappings: `automation.run`→`automation.control`, `input.send`→`input.control`, `photos.read`→`photos.read`, `contacts.search`→`contacts.read`, `calendar.events`→`calendar.read`, `location.get`→`location.read`, `audio.play`→`audio.output`, `url.scheme.open`→`url.scheme.open`; `screen.watch` (subscribe type)→`screen.capture` (its data surface equals frame-by-frame screenshots; checked once when the subscription is established). v1.4 batch mappings: `media.playback`→`media.control`, `system.sleep` / `system.lock`→`power.control` (high risk), `system.settings`→`system.settings`, `printer.print`→`printer.control`, `messages.recent`→`messages.read` (high risk), `window.list` / `window.focus`→`window.management` (high risk), `notes.read`→`notes.read`, `reminders.read`→`reminders.read`, `reminders.write`→`reminders.write`, `mail.recent`→`mail.read` (high risk). v1.5 batch mappings: `things.list` / `things.show` / `things.search`→`things.read`, `things.add` / `things.update` / `things.delete`→`things.write` (both read and write are ask, not high risk).

## Decision States

```text
granted | denied | ask
```

- At install time, presented from the Manifest, the user confirms item by item, and the result is persisted (Plugin layer)
- The Agent layer has no install moment; it starts from the default decision, and the user adjusts it in the settings page or the ask dialog
- `ask`: dialog at each runtime call (Allow once / **Allow for this session** (v1.5) / Always allow / Deny)
- The **session tier (v1.5)**: a third tier between once and always — the grant lives in process memory, no further dialogs while this process stays alive, cleared on restart; not persisted. Its role is a pressure valve for dialog fatigue:
  - High-risk permissions **allow** the session tier (still no Always, no permanent opening);
  - Declared derived permissions (once-only, anti-Always-laundering) **do not accept** session and are automatically downgraded to once, with the audit carrying `downgradedFrom: "session"`;
  - An explicit DB `granted`/`denied` takes precedence over the overlay; the settings page can revoke all via `session_revoke_all()`.
- High-risk permissions (`process.execute`, `filesystem.write`, `microphone`, `camera`; v1.3 adds `automation.control`, `input.control`, `plugin.install`; v1.4 adds `messages.read`, `window.management`, `power.control`, `mail.read`) do not offer Always allow; they can only be granted per call or via the session tier

## Two-Layer Decision

It passes only when the Agent layer and the Plugin layer **both pass**:

```text
effective = agent_decision ∧ plugin_decision

either denied  → denied
both granted   → granted
otherwise      → ask (when a native capability involves no plugin, only the Agent layer is consulted)
```

The ask dialog shows both principals at once: "Claude wants to read images through the Vision plugin." When the user chooses Allow always, two granted records are written at the same time; Allow once only permits this call and is not persisted.

> **v1 implementation note (honest boundary)**: the current implementation is **two sequential gates** — the Agent layer decides immediately after the `/rpc` gateway, and the Plugin layer decides inside the Capability Router. When both gates are `ask`, the first call may show two confirmation dialogs in sequence (Agent layer first, Plugin layer second). The merged single dialog described above (showing both principals at once, Allow always persisting both) is the v2 UX goal; the decision semantics (`∧`) are unchanged.

## Global Policy (Hard Gate)

Above the Agent layer sits an additional **global policy**: a `permission_policy` table that tightens or loosens Core capability permissions as a whole (third-party plugin custom capabilities are outside this section's scope). The granularity is the **permission**, not the capability — consistent with runtime enforcement; only permissions in the static inventory can be written, and unknown permissions are rejected.

This is the **third layer**; the layer order is:

```text
Global policy (hard gate: turn off a whole class; new in this document)
  ↓
Agent permission (what this Agent can call this time)
  ↓
Plugin permission (what this plugin declared and was granted)
```

**Priority (top to bottom, first match returns first)**:

| # | Condition | Result |
|---|---|---|
| 1 | Global `denied` | **Denied** — hard gate, overrides every per-agent `granted` |
| 2 | per-agent override present | That override decision |
| 3 | Global `granted` / `ask` | That decision (as a new default only) |
| 4 | None of the above match | Built-in static default (`PERMISSIONS` table) |

The difference: a global `granted` / `ask` only changes the **new default** and does not override a decision an Agent has explicitly persisted; a global `denied` is a one-way hard gate independent of per-agent decisions — step 1 wins first, with no exception for whatever step 2 matches.

- **High-risk cannot be globally granted**: HIGH_RISK permissions (`process.execute`, `filesystem.write`, `microphone`, `camera`, plus the high-risk items in each of the v1.3 / v1.4 batches) do not accept a global `granted`; only `ask` / `denied` — high risk allows only per-call grants, from the same root as the once-only rule in "Decision States"
- **Scope boundary (explicit non-goal)**: this section governs only the **Agent gate at the Core capability layer**. The plugin-layer `permission::check` semantics are unchanged and remain the agent ∧ plugin two gates described in "Two-Layer Decision"; the global policy changes neither the plugin layer nor the plugin's own switches
- **Shared permissions**: `file.watch` and `file.read`, and `screen.watch` and `screen.capture`, share the same permission and cannot be toggled independently of the base permission (a subscribe-type capability falls into the category of its base permission; splitting the mappings would change the already-published policy vocabulary, so it is not done)

**Audit**: each write emits `permission.policy.changed` (`{permission, decision, by}`); a reset (restoring the built-in default) emits the same event with `decision: "none"`.

**UI**: Settings → the "System Capabilities" tab, grouped by category, with permissions as the control granularity; each row has 4 options (follow default / granted / ask / denied; high-risk has no granted option), and `denied` is annotated next to it with "overrides all Agents' grants". The Agents tab shows a "Global: denied" badge next to the permission name, to avoid the misreading where an Agent row shows `granted` yet is rejected by the hard gate.

This tab **lists only permissions that have a capability mapping** (28 of them). Vocabulary placeholders (`process.execute` / `microphone` / `camera` / `network.request` / `plugin.install` / `pet.animation` / `storage.local`) are not triggered by any capability gate, so listing them would be a fake switch that "does nothing when clicked", and they are therefore omitted — their global policy **still takes effect** at the `check_agent` layer, there is simply no call path that reaches it today.

## Storage

```text
agents
├── agent_id      # PK
├── kind
├── display_name
├── token_hash    # SHA-256, never store plaintext
├── status        # active | revoked
├── first_seen / last_seen
└── registered_via # mcp | hook

agent_permissions
├── agent_id
├── permission
├── scope        # JSON, may be null
├── decision     # granted | denied | ask
└── updated_at

permission_policy         # global policy (hard gate)
├── permission   # PK
├── decision     # granted | denied | ask
└── updated_at

plugin_permissions        # unchanged
├── plugin_id / permission / scope / decision / updated_at
```

## Scope

Path-type permissions support scope constraints, prefix matching, `~` expansion. The Agent layer and the Plugin layer each hold their own scope, and a resource must **fall within both layers' allowed** (consistent with the two-layer decision):

```json
{
  "agent_id": "ag_claude_7f3a",
  "permission": "file.read",
  "scope": {
    "allowed": ["~/Projects/opencapx/"],
    "denied": ["~/.ssh", "~/.aws", "~/Library/"]
  },
  "decision": "granted"
}
```

Decision order: denied match → reject; allowed match → pass; neither matches → reject (closed by default).

**Enforcement status (v1.5, since 2026-09-19)**: path-type scope is now enforced at runtime, in
`core/scope.rs` (single-point matcher) + `capability.rs::execute` (two-layer enforcement point):

- Covered capabilities: `file.read` / `file.write` (parameter `path`), `file.search` (parameter `root`).
- Prefixes match on **path component boundaries**: `~/a` does not allow `~/ab`; `~` is expanded; lexical normalization, symlinks are not resolved (v1).
- **A null / absent scope = unrestricted** (compatible with existing authorization rows, progressive enablement): the layer only starts constraining after scope JSON is written from the settings page.
- A JSON parse failure is treated as a reject (fail closed).
- The two layers are independent: the agent layer decides at the capability dispatch entry, and the plugin layer decides before each provider (reject → try the next provider, isomorphic to fallback).
- A rejected path emits `capability.failed` (error `scope_denied`, including `scopeLayer`) + a `gate.scope_denied` trace event.

**Domain-type scope (v1.5, enforced)**: `browser.read` (parameter `url`) constrains the host by the scope of `browser.control`, with two-layer semantics identical to the path type. Entry rules: `example.com` = this domain + subdomains (dot boundary, `notexample.com` does not match); `.example.com` / `*.example.com` = subdomains only; a URL's port, userinfo, and case do not participate in matching; http(s) only. No configured domain = closed, null = unrestricted.

## Runtime Check Flow

```text
A request comes in (/rpc or /event)
 ↓ ① Identity: agent_id + token hash + status
    fail → 40101/40102, write auth.rejected, done
 ↓ ② Agent layer: look up agent_permissions
 ↓ ③ Plugin layer: look up plugin_permissions (two-layer decision)
    ├─ granted → check scope (both layers) → pass → allow
    ├─ either denied → reject, return error code 40001, write audit
    └─ ask → show the permission window (a plugin's reverse request via core.requestPermission also goes here)
             user choice:
               Allow once → allow this call
               Always     → write one granted record in each layer (high-risk permissions have no such option)
               Deny       → reject; 60s of inactivity is treated as Deny
```

All requests and results are written to the audit.

> **Known v1 exemption: offline queue replay**. Events that `opencapx hook` fails to deliver while the Core is offline land in `~/.opencapx/queue/`, and are replayed in-process when the Core starts — this batch does not pass through gate ① (there is no token to verify). The replayed content is only observational hook events (session state changes, no side-effecting tool calls), and the queue file and token file share the same local same-user permissions with a consistent threat model, so v1 accepts this exemption. `/rpc` has no such channel: while the Core is offline, MCP tool calls fail outright and are not queued.

## Audit

The `events` table (see [events.md](events.md)) adds and extends:

```text
agent.registered   {agentId, kind, via}
auth.rejected      {agentId?, reason: bad_token|revoked|anonymous}
agent.revoked      {agentId}
permission.requested  {agentId, pluginId, permission, scope}   // caller split into the two principal fields
permission.granted    {agentId, pluginId, permission, decision: once|always}
permission.denied     {agentId, pluginId, permission, reason: user|timeout}
```

The settings page → Permissions view groups by plugin; the new Agents view groups by Agent, and both can at any time revert to ask / revoke granted.

## Permission UI

At install time (Plugin layer, unchanged):

```text
┌─────────────────────────────────┐
│ Install Vision                  │
│                                 │
│ This plugin requests:           │
│                                 │
│ 👁 Read images                  │
│ 🌐 Internet access              │
│                                 │
│ [ Cancel ]       [ Install ]    │
└─────────────────────────────────┘
```

At runtime (bubble/dialog, two-layer copy):

```text
🐱 Claude wants to capture the screen through the Vision plugin.

   [Allow once]   [Always]   [Deny]
```

Agents view (new in the settings page):

```text
Agents
├── 🟢 Claude      last seen 2 min ago   [Permissions] [Revoke]
├── 🟢 Codex       last seen 1 h ago     [Permissions] [Revoke]
└── ⚪ opencode    revoked                [Re-authorize]
```

Reuse the existing settings window system for the permission management page; do not introduce a new window type.

## Migration and Compatibility

- `/rpc` and `/event` going from no authentication to a required header: a **breaking change**. The CLI and the Core ship from the same binary and version and are upgraded together; no compatibility window is provided
- The existing `plugin_permissions` is untouched; `agents` / `agent_permissions` are new tables
- The permission inventory adds `notification.post` and semantically extends `pet.animation` (minor version +1)
- **v1.2**: the `speech.synthesize` permission mapping changes from `storage.local` to `notification.post` (voice output is a human-facing channel whose spam/phishing surface matches notifications, so it should default to ask). **Existing decisions do not migrate**: an agent already granted `storage.local` does not automatically get voice access; its first call asks again under `notification.post` — a conservative and safe direction. Adds the read-only capability `system.permission_status` (gate-exempt, no inventory change). Built-in providers expand to thirteen (clipboard.read / file.read / browser.* / speech / image.ocr / system.permission_status, see [capability.md](capability.md) "Built-in Providers")
- **v1.3**: the permission inventory goes 14→23 (high-value: `automation.control` (ask, high risk), `input.control` (denied, high risk), `plugin.install` (denied, high risk, vocabulary placeholder with no mapping); mid-value: `photos.read` / `contacts.read` / `calendar.read` / `location.read` / `audio.output` / `url.scheme.open`, all ask). The high-risk set goes 4→7. Built-in providers go 13→21 (+ automation.run / input.send / the four PIM items / audio.play / url.scheme.open), adding the second subscribe-type `screen.watch` (mapped to `screen.capture`, see capability.md). Two deviations from the design draft: ① `audio.output` is set to **ask** (the draft had granted) — the sound channel is human-facing like notification.post / speech.synthesize, and a TOFU-registered agent should not be able to produce sound by default, the same logic as the v1.2 voice tightening; ② `system.permission_status` **does not extend to** the four surfaces photos/contacts/calendar/location — they have no "probe without a dialog" API (the probe is itself an access that triggers a TCC dialog), so the capability keeps reporting only surfaces with a clean preflight (screen_recording / accessibility)
- **v1.4**: the permission inventory goes 23→33 (+10: light-control `media.control` / `system.settings` / `printer.control` and personal-data `messages.read` / `window.management` / `power.control` / `notes.read` / `reminders.read` / `reminders.write` / `mail.read`, all ask or denied). The high-risk set goes 7→11: the four new entries all go **denied** (`messages.read` = a machine-wide TCC surface requiring full disk access, `window.management` = the starting point for computer-use operations, `power.control` = machine-global and irreversible for running tasks, `mail.read` = a phishing/C2 surface) and do not offer Always allow. Built-in providers go 21→33 (+12: media / sleep / lock / settings / printer + messages / window×2 / notes / reminders×2 / mail). This entire batch is additive new capabilities with **no existing mapping rewrites**, so there is no migration action; two new modules are added, `core/messages.rs` (chat.db read-only) and `core/windowctl.rs` (System Events), and the four PIM items are extended into the existing `core/pim.rs`. Three honest limitations (see the corresponding bullets in capability.md): ① reminders due dates are **not collected** in v1 (localized-format parsing is unreliable); ② Notes `snippet` is the flattened first 200 characters with no HTML tag stripping; ③ `messages.recent` depends on the user manually granting "Full Disk Access"; without it, opening fails immediately with an authorization hint in the error (it does not pretend to have permission)
- **v1.5**: the permission inventory goes 33→35 (+2: `things.read` / `things.write`, both **ask**, not added to the high-risk set). These two permissions hang only on the six plugin-provided capabilities `things.add` / `things.update` / `things.list` / `things.show` / `things.search` / `things.delete` (see [capability.md](capability.md)); the Core has no built-in provider — when the plugin is not installed, `execute` returns `capability_unavailable`. Purely additive, no existing mapping rewrites, no migration action, minor version +1 with the major version unchanged.
