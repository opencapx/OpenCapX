# Plugin Manifest Spec

> **🔒 schema frozen (2026-09-20, since Core 0.1.0)**: the field set is frozen per [deprecation.md](deprecation.md)
> — within v1, adding a field is optional (additive), while deleting/renaming/tightening required fields is a breaking change and is forbidden.
Every plugin root directory must contain an `opencapx-plugin.json`. This is the core of the entire plugin ecosystem: the Core recognizes only the Manifest, not the plugin body itself.

## Full Fields

| Field | Type | Required | Description |
|---|---|---|---|
| `id` | string | ✅ | Reverse domain name. Official plugins use `com.opencapx.*`; third parties use their own domain |
| `name` | string | ✅ | Display name |
| `description` | string | ❌ | One-line description |
| `version` | string | ✅ | SemVer |
| `apiVersion` | string | ✅ | Protocol major version, currently `"1"`. The Core rejects installation when incompatible |
| `type` | string | ✅ | `pet` \| `capability`. v1 has only these two; `agent` is unimplemented and rejected at parse time (SDKs kept in sync) |
| `runtime` | object | capability ✅ / pet ❌ | See below |
| `capabilities` | string[] | capability ✅ | Declares the capability IDs provided, see [capability.md](capability.md); third-party domains use the object form `{"id","permission","default"(ask\|denied),"timeoutSecs"(1..=600, defaults to 60s)}` |
| `permissions` | string[] | ❌ | Declares the permissions required, see [permissions.md](permissions.md) |
| `states` | string[] | pet ✅ | The set of states the pet supports |
| `author` | string | ❌ | |
| `homepage` | string | ❌ | |
| `license` | string | ❌ | |
| `minCoreVersion` | string | ❌ | Semantic version lower bound; installable/startable only when `coreVersion >= minCoreVersion` (F4) |
| `dependencies` | object | ❌ | Inter-plugin dependencies `{pluginId: semver requirement}`; cycles are rejected at install time, and a missing dependency at startup is rejected with the missing items shown (F5) |
| `settings` | object[] | ❌ | Declarative settings controls (12 kinds); the Core renders a generic settings page while the actual values still go through `config.*` (F8, see "Declarative Settings") |

### runtime

```json
{
  "type": "process",
  "command": "vision-plugin",
  "args": ["--verbose"],
  "env": { "OPENCAPX_PLUGIN_ID": "com.opencapx.vision" }
}
```

- `type`: v1 has only `"process"`. A plugin is a separate process, and the Core communicates with it via JSON-RPC over stdin/stdout, see [plugin-protocol.md](plugin-protocol.md)
- `command`: relative paths are relative to the plugin root; an absolute path or a command in `$PATH` (such as `python`, `node`) also works
- `env`: extra environment variables statically declared by the author, **always injected**. Host environment variables are not inherited by default (since S1, only a minimal allowlist
  + declarations here + the user's incremental allowlist), see [plugin-signing.md](plugin-signing.md) §10
- The `pet` type needs no runtime — a pure asset directory rendered directly by the Pet UI

## Example: Vision (capability)

```json
{
  "id": "com.opencapx.vision",
  "name": "Vision",
  "version": "1.0.0",
  "apiVersion": "1",
  "type": "capability",
  "runtime": {
    "type": "process",
    "command": "bin/vision-plugin"
  },
  "capabilities": ["image.analyze", "image.ocr"],
  "permissions": ["image.read", "network.request"],
  "author": "OpenCapX",
  "license": "MIT"
}
```

## Example: Cat (pet)

```json
{
  "id": "com.opencapx.cat",
  "name": "Cat",
  "version": "1.0.0",
  "apiVersion": "1",
  "type": "pet",
  "states": ["idle", "thinking", "working", "waiting", "success", "error", "sleeping"],
  "author": "OpenCapX",
  "license": "MIT"
}
```

Pet state assets are named `<state>.webp` and placed under `assets/`:

```text
com.opencapx.cat/
├── opencapx-plugin.json
└── assets/
    ├── idle.webp
    ├── thinking.webp
    ├── working.webp
    ├── waiting.webp
    ├── success.webp
    ├── error.webp
    └── sleeping.webp
```

The Core only dispatches the logical state (`agent.thinking` → `thinking`); how it is actually animated is up to the pet itself. Cat scratching its chin, Robot spinning a ring overhead, Dragon puffing smoke — the Core does not care.

## Package

Format: `<name>-<version>.ocplugin`, actually a ZIP.

```text
vision-1.0.0.ocplugin (ZIP)
├── opencapx-plugin.json
├── bin/            # plugin binary or script
├── assets/         # images and other assets
└── ui/             # optional: plugin settings page (rendered in a WebView sandbox)
```

v1 only does ZIP + SHA-256 integrity verification; code signing is deferred until there is a third-party ecosystem.

> **Wow 6 update (2026-09):** signing/verification is implemented. The Manifest may optionally carry a `sha256` field +
> a `signature: { alg, keyId, sig }` field. When `alg` is absent (`None`) or omitted = `"hmac-v1"`
> (a legacy package is backward compatible); writing `"ed25519"` uses the v2 distribution channel. When the trusted-keys file
> `~/.opencapx/trusted-keys.json` does not exist or is empty, a manifest **without a signature field**
> can still be installed (backward compatible); once enabled, a missing or bad signature is rejected. The full schema and policy are in
> `core::plugin_sig`.
>
> **The semantics of the `sha256` field are dispatched by `alg`** (the field name is unchanged):
> - `hmac-v1` / default: the v1 archive content hash, `sig = HMAC-SHA256(secret, "opencapx-v1\n" + sha256)`.
> - `ed25519`: the hex of digest_v2, `sig = Ed25519(SK, "opencapx-v2\n" + sha256)`; digest_v2
>   **covers the manifest itself** (canonical: drop the two keys `sha256`/`signature`, sort keys lexicographically at every level)
>   + the entry files, closing v1's hole of "the manifest is not part of the digest".
> - Unknown `alg` → judged `malformed-signature` and hard-rejected. The byte-exact specification, exit codes, and the two command entries are in
> [plugin-signing.md](plugin-signing.md).
>
> **Numeric constraints:** numbers in a signed manifest must be integers within 64-bit range; floats and out-of-range integers
> are rejected at pack time (an explicit boundary of the cross-language canonical digest, see plugin-signing.md §6).
>
> **The v2 route is finalized:** distribution trust (Ed25519), publisher registration/revocation, and the three-state install UX are in
> [supply-chain.md](supply-chain.md); the local HMAC flow is retained per D1 of that document.
> Field persistence (2026-09): `sha256`/`signature` are persisted into the database along with the manifest and surfaced in the preview as a signature badge; `author`/`homepage`/`license` are surfaced via the list and preview DTOs.

## Install Flow

```text
Select a .ocplugin file
 ↓ Parse the Manifest (reject on failure)
 ↓ Verify SHA-256 integrity (optional; the manifest.sha256 field)
 ↓ Verify the signature (optional; enabled when the manifest.signature field + trusted-keys exist)
 ↓ Check apiVersion compatibility
 ↓ Show the permissions declarations → user confirmation
 ↓ Extract to plugins/<id>/
 ↓ Write the SQLite plugins table
 ↓ Start the plugin process → plugin.ping health check
 ↓ Enable on success; stop in the error state on failure
```

## New Manifest Fields (Wow 6)

v1 local HMAC (old shape, `alg` omitted):

```json
{
  "id": "com.example.foo",
  "name": "Foo",
  "version": "0.1.0",
  "apiVersion": "1",
  "type": "capability",
  "capabilities": ["image.analyze"],
  "permissions": ["image.read"],
  "sha256": "<64-hex; archive hash: concatenate <name>\n<size>\n<bytes> sorted by filename, then SHA-256>",
  "signature": {
    "keyId": "alice-2026",
    "sig": "<64-hex; HMAC-SHA256(secret, \"opencapx-v1\\n\" + sha256)>"
  }
}
```

v2 distribution Ed25519 (`alg` declared explicitly; `sha256` now stores digest_v2):

```json
{
  "id": "com.example.foo",
  "name": "Foo",
  "version": "0.1.0",
  "apiVersion": "1",
  "type": "capability",
  "capabilities": ["image.analyze"],
  "permissions": ["image.read"],
  "sha256": "<64-hex; digest_v2 covers manifest (canonical) + entry files>",
  "signature": {
    "alg": "ed25519",
    "keyId": "com.example.alice",
    "sig": "<128-hex; Ed25519(SK, \"opencapx-v2\\n\" + sha256)>"
  }
}
```

Generate signatures with the toolchain, not by hand: `opencapx keygen/pack/verify` or `python3 -m opencapx_sdk.signing` — the full commands, exit codes, trusted-keys shape, and the byte-exact digest_v2 specification are in [plugin-signing.md](plugin-signing.md).

## Declarative Settings (settings[])

Optional array (≤32 items): declares settings controls for a plugin, and the Core renders a **generic form** in the settings page — the plugin needs to write no UI.
A declaration only drives the interface; reading/writing values still goes through `config.*` (the secret control uses the keychain, see plugin-protocol.md).

| Field | Type | Required | Description |
|---|---|---|---|
| `key` | string | ✅ | `^[a-z][a-z0-9_-]{0,63}$`, unique within the item; i.e. the config key name |
| `type` | string | ✅ | One of 12 kinds, see the table below |
| `label` | string \| LocalizedText | ❌ | Display name (defaults to the key); see "Localizable Text" |
| `description` | string \| LocalizedText | ❌ | One-line explanation; see "Localizable Text" |
| `default` | any | ❌ | The fallback value when the UI is "unset"; must satisfy this declaration's own `validate`; secret must not declare it |
| `options` | (string \| {value, label})[] | ❌ | Used only by dropdown / radio-group, and must be non-empty; a structured item's `label` is LocalizedText (display only), while storage and predicate comparison always use `value` |
| `visible` | Cond | ❌ | Data-driven predicate: when false the entire row is not rendered |
| `disabled` | Cond | ❌ | Data-driven predicate: when true the control is disabled (the row remains) |
| `validate` | ValidateRule[] | ❌ | Pre-write validation rules (empty by default) |
| `section` | string \| LocalizedText | ❌ | Consecutive declarations with the same section share one heading; non-empty after trim and ≤40 characters |
| `aliases` | string[] | ❌ | Search keywords (index only, never shown); ≤8 items, each non-empty after trim and ≤40 characters; the `label` of a structured option also enters the search index |
| `order` | number | ❌ | Form display order, smaller first; ties and undeclared items fall back to manifest order (stable sort, UI only) |
| `deprecated` | string \| LocalizedText | ❌ | Deprecation note: the control still works, with a "deprecated" marker and reason on the row; each translation non-empty after trim and ≤200 characters |
| `pick` | `"file"` \| `"directory"` | ❌ | Only for `path`: pick a file or a directory (directory is still the default) |
| `min` / `max` | number | ❌ | Only for `number` / `slider` |
| `step` | number | ❌ | Only for `number` / `slider`; must be > 0 |

| `type` | Control | Constraint |
|---|---|---|
| `toggle` | Switch | Value bool |
| `text` | Single-line input | Value string |
| `textarea` | Multi-line input | Value string |
| `number` | Numeric input | Value int64; `default` must be an integer and within `[min,max]` |
| `slider` | Slider | Must provide both `min`+`max` and `min < max`; `step` (if present) > 0; `default` (if present) numeric and within `[min,max]` |
| `dropdown` | Dropdown select | Must provide a non-empty `options[]`; `default` (if present) must ∈ options |
| `radio-group` | Radio group | Same as dropdown (the same `options[]` rules) |
| `color` | Color picker | Value string; `default` (if present) must match `^#[0-9a-fA-F]{3}([0-9a-fA-F]{3})?$`; must not have `options`/`min`/`max` |
| `secret` | Password input | Stores to keychain (falls back to a 0600 file); the UI only shows "set/unset", the value is never echoed; must not declare `default` |
| `path` | Path input + file/directory picker | Value string; `pick` decides file or directory |
| `button` | Action button | Stores no value; clicking calls the plugin method `settings.<key>`; must not have `default`/`options`/`validate` |
| `list` | List | Items are held by the plugin: the host renders add/delete/reorder, each operation calling the plugin method `settings.<key>` and taking back the current array; must not have `default`/`options`/`min`/`max`/`pick` |

### Localizable Text (LocalizedText) and aliases

`label` / `description` / `section`, and each `validate[]` rule's `message`, are all
**LocalizedText**: they accept both the existing **plain string** (single-language; serialized back as a string as-is, so old manifests are
byte-for-byte unchanged) and a **locale → text** map. Locale keys are **not restricted to the App's current language set**
— a plugin may carry a language the App does not yet know, and it is accepted at parse time.

```json
{
  "key": "backend_mode", "type": "dropdown",
  "options": ["local", "cloud"],
  "label": { "en": "Backend mode", "zh-Hans": "Backend mode", "vi": "Chế độ backend" },
  "description": { "en": "Where inference runs", "zh-Hans": "Where inference runs" },
  "section": { "en": "General", "zh-Hans": "General" },
  "aliases": ["mode", "backend", "backend mode"],
  "validate": [{
    "type": "required",
    "message": { "en": "Backend mode is required", "zh-Hans": "Backend mode is required" }
  }]
}
```

**Resolution order** (on the UI side): **current locale → `en` → the lexicographically smallest locale key**. The Rust-side
`pick_host_locale` uses the same order; the two implementations must not diverge.

**Host-side fallback**: the storage layer and `set_setting_value` have no locale context, so the error string when a write is rejected
is resolved in the same fixed order. Normal users go through the UI — the UI validates under the current locale and shows the localized text —
so this path only serves non-UI callers (such as MCP / CLI) and is a fallback, not the main path.

**Validation**: the map itself must be non-empty, locale keys non-empty after trim, and each translation non-empty after trim (an empty
translation is a manifest bug — the UI would render an empty label); each translation of `section` is ≤40 characters after trim.
`aliases` is used only for the search index (never shown): ≤8 items, each non-empty after trim and ≤40 characters.

> **Known limitation**: `key` is a storage key (not display text) and the **`value`** of `options[]` is a stored value;
> neither is localized — consistent with the reference implementation's trade-off. When you need localized option text, write
> `{"value": "...", "label": {...}}` for that item: `label` participates in localization and search, while `value` stays stable.

### Predicates (Cond, `visible` / `disabled`)

A predicate is **data**, not a closure: the definition must cross processes (Python plugin → Rust Core → TS UI), and functions are not serializable.
**Evaluation happens only in the UI**; Rust validates only the shape and references (it does not evaluate). `op` is the discriminant field:

| `op` | Shape | Description |
|---|---|---|
| `equals` | `{"op":"equals","key":"<declared key>","value":<json>}` | Current value === value |
| `notEquals` | `{"op":"notEquals","key":"...","value":<json>}` | Current value !== value |
| `in` | `{"op":"in","key":"...","values":[<json>, ...]}` | Current value ∈ values |
| `isSet` | `{"op":"isSet","key":"...","value":true}` | The value is set (`value` must be bool) |
| `all` | `{"op":"all","conds":[Cond, ...]}` | All true (`conds` must be non-empty) |
| `any` | `{"op":"any","conds":[Cond, ...]}` | Any true (`conds` must be non-empty) |
| `not` | `{"op":"not","cond":Cond}` | Negation |

A predicate can reference only a `key` **already declared within the same manifest's `settings[]`**; a `secret` key allows only `isSet`
(a secret's value never enters the UI, so only its existence can be tested); a `list` key **may not be referenced** (its items are held by the plugin process, the value is never sent back,
so the predicate would always read it as unset — rejected at install time); for a `dropdown`/`radio-group` key,
the `equals`/`notEquals`/`in` values must fall within that key's `options[]`; nesting depth ≤ 8.

### Validation Rules (ValidateRule, `validate[]`)

Also data. **Rust executes it when the user writes to disk** (`set_plugin_setting`); the plugin's own reverse
`config.set` is not validated — the plugin owns its own runtime configuration. `type` is the discriminant field:

| `type` | Shape | Applicable controls |
|---|---|---|
| `required` | `{"type":"required","message":"..."}` | text / textarea / secret / path / number / color |
| `minLength` | `{"type":"minLength","value":8,"message":"..."}` | text / textarea / secret / path |
| `maxLength` | `{"type":"maxLength","value":200,"message":"..."}` | text / textarea / secret / path |
| `min` | `{"type":"min","value":1,"message":"..."}` | number / slider |
| `max` | `{"type":"max","value":8,"message":"..."}` | number / slider |
| `pattern` | `{"type":"pattern","regex":"\\.json$","message":"..."}` | text / textarea / path / color / secret |

`message` is optional and may be the map form of LocalizedText (see "Localizable Text"). When a write is rejected it returns
`invalid: <message>` — the host has no locale, so it uses the result of `pick_host_locale` (use `en` if present,
otherwise the lexicographically first key); by default it returns `invalid: <rule-type>`. The UI validates under the current locale first,
so the user normally sees the UI's localized text.
`pattern` is compiled with **Rust regex syntax**; a JS-only construct (such as the lookahead `(?=x)`) is rejected at install time,
**not silently ignored**. Conversely, `pattern` must **also** be a valid JavaScript regex — the UI evaluates it with
`new RegExp`, so Rust-only constructs (inline flags `(?i)` / `(?m)` / `(?s)` / `(?x)` /
`(?U)` / `(?-…)`, Python named groups `(?P<name>…)`) are likewise rejected at install time; `(?:…)` and
`(?<name>…)` are accepted by both engines and can be used.

**Null exemption**: "unset" = the value is missing, `null`, `""` (empty string), or an empty array; **every rule except `required`
skips unset values** — an optional field can therefore be cleared, and only `required` can judge "must not be empty".
For example, a `pattern` rule on `path` will not fail because of `""` (meaning "use the default location"); `default: ""` can
therefore pass install-time validation while a `pattern` is declared.

### Two Static Checks

1. **Predicate reference integrity**: every key referenced by `visible`/`disabled` must already be declared and must obey the above
   secret / options constraints; `all`/`any` must not be empty; nesting ≤ 8.
2. **`default` self-consistency**: `default` (if present) must satisfy the declaration's own `validate[]`, otherwise the install is rejected
   — this avoids an old plugin shipping a default that can never be written back.

Validation is performed by the Core at install/parse time (`core::plugin::validate_manifest`): unknown type, unknown predicate
`op`, unknown rule `type`, duplicate key, dropdown/radio-group missing options or whose default is not in options (compared by
`value`, ignoring `label`), `deprecated` with a blank translation, button
carrying a value, list carrying `default`/`options`/`min`/`max`/`pick`, secret with a default, number with a non-integer default,
slider missing min/max or min ≥ max, a range field on something other than number/slider, `pick` on a non-path or with an invalid
value, `section` blank or over 40 characters, a predicate referencing an undeclared key, `pattern` that fails to compile, `default` violating
its own rule, more than 32 items, an empty LocalizedText map or one with a blank translation/locale key, `aliases` over 8 items
or an item that is blank/over 40 characters — any one rejects.

```json
{
  "settings": [
    {"key": "auto_play", "type": "toggle", "label": "Auto play", "default": true,
     "aliases": ["autoplay", "auto play"]},
    {"key": "api_key", "type": "secret", "label": "API Key"},
    {"key": "quality", "type": "dropdown", "options": ["low", "high"], "default": "low"},
    {"key": "retry", "type": "number", "default": 3, "min": 1, "max": 8,
     "validate": [{"type": "min", "value": 1, "message": "at least 1"}]},
    {"key": "volume", "type": "slider", "min": 0, "max": 100, "step": 5, "default": 50,
     "section": "Audio", "visible": {"op": "equals", "key": "auto_play", "value": true}},
    {"key": "theme", "type": "radio-group", "options": ["light", "dark"], "default": "light"},
    {"key": "tint", "type": "color", "default": "#3b82f6",
     "label": {"en": "Accent color", "zh-Hans": "Accent color"}, "aliases": ["theme", "accent color"]},
    {"key": "data_dir", "type": "path", "pick": "directory"},
    {"key": "template", "type": "path", "pick": "file",
     "validate": [{"type": "pattern", "regex": "\\.json$", "message": "must be a .json file"}]},
    {"key": "run_now", "type": "button", "label": "Run now"},
    {"key": "tags", "type": "list", "label": "Tags"}
  ]
}
```

## Sandbox Declaration (sandbox, S5)

Optional block (capability plugins only; a pet has no process and must not carry it):

```json
"sandbox": { "fs": { "write": ["plugin-data"] }, "network": "none" }
```

| Field | Value | Description |
|---|---|---|
| `network` | `"none"` (default) / `"out"` | `none` = deny all network; `out` = allow outbound |
| `fs.write` | `["plugin-data"]` | Only writes to the per-plugin data directory `~/.opencapx/plugin-data/<id>/` are allowed; writes to any other path are always rejected |

Enforcement semantics (macOS, S5b/S5c):

- After declaration it is started via `sandbox-exec`; **unsigned / unverified publisher → enforced** (cannot be turned off);
  for a trusted publisher it is controlled by the `sandbox_enforcement` setting (off by default, can opt out).
- Reads and process execution are unrestricted — **writes and network are the isolation boundary** (macOS firmlinks make a read allowlist unreachable and would break the runtime).
- Inside the sandbox `TMPDIR` is redirected to `plugin-data/<id>/tmp`, so temporary files work without crossing the write boundary.
- The Linux (bubblewrap) / Windows (AppContainer) enforcement layers are out of scope (roadmap); the declaration itself is cross-platform.
- The 6th class of the listing auto-gate validates the declaration's legality (see [plugin-review.md](plugin-review.md)).

## Plugin State Machine

A plugin does not have only enabled/disabled:

```text
probe_pending → starting → running
                     │          │
                     │          └─ manual stop / ordered stop → stopped
                     └─ start failure / crash retries exceeded → error
probe_pending ── install self-check failure → probe_failed (does not start)
```

The state lands in the SQLite `plugins.status` field, queryable from the settings page for debugging.
The values actually written to `plugins.status` are exactly these six; the `discovered/installing/installed/disabled` from early docs are not persisted.

## Directory Layout

Use the system AppData / Application Support directory; do not hardcode:

```text
~/.opencapx/            (on macOS, ~/Library/Application Support/opencapx also works)
├── settings.json       # existing user settings, retained in v1
├── plugins/
│   ├── com.opencapx.cat/
│   ├── com.opencapx.vision/
│   └── …
├── data/               # SQLite database
├── cache/
└── logs/
    ├── opencapx.log
    ├── mcp.log
    └── plugins/<id>.log
```
