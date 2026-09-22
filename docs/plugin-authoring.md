# Plugin Authoring Guide

For authors **writing an OpenCapX plugin for the first time**: from an empty directory to a plugin that installs, upgrades, and has a settings form.

This guide covers only "how to do it"; field-level specifications are not repeated here:

| When you need exact specs | See |
|---|---|
| All Manifest fields, the 12 `settings[]` controls, predicate/validation rules | [plugin-manifest.md](plugin-manifest.md) |
| The stdio JSON-RPC protocol, reverse calls, error codes | [plugin-protocol.md](plugin-protocol.md) |
| Capability naming, Schema, registration and routing | [capability.md](capability.md) |
| The permission vocabulary, two-layer decision, consent | [permissions.md](permissions.md) |
| The third-party new-domain declarative system | [permission-domains.md](permission-domains.md) |
| Signing, packaging, environment variables, and §10 environment isolation | [plugin-signing.md](plugin-signing.md) |

---

## 0. 30-Second Start: Use the Scaffold

Run from the OpenCapX repo root:

```bash
node scripts/create-opencapx-plugin.mjs --id com.acme.hello --name "Hello" --author acme --dir ./hello
```

It copies `plugin-template/` to `--dir` (default `./<last segment of id>`) and writes `id/name/author/license/description` into `opencapx-plugin.json`, renaming the class from `MyPlugin` to `<Name>Plugin`. The id rules match the Core's `valid_plugin_id`: `[a-z0-9.-]`, no `..`, no leading or trailing dots, ≤128 characters.

Without Node, copying `plugin-template/` directly also works. The following describes what each step does.

## 0.5. The TypeScript Path: Write a Plugin in TS

The protocol is language-agnostic, and TS has an official SDK (`packages/ts-sdk/`, package name `@opencapx/sdk`, zero runtime dependencies). Two deliberate differences from the Python SDK: reverse calls are `async` (you must `await`), and registration uses explicit `this.capability(...)` / `this.method(...)` (no decorator switch). Packaging and signing still go through the Python SDK / Rust CLI (see `plugin-signing.md`).

```bash
# scaffold (published; --local switches back to file: deps inside the repo)
npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
cd hello && npm install && npm test && npm run build
```

The scaffold produces: `opencapx-plugin.json` (runtime `node dist/src/plugin.js`), `src/plugin.ts`, `test/plugin.test.ts` (run with `node --test` after compilation), and `tsconfig.json` (strict). The SDK dependency in `package.json` defaults to the registry's `^0.1.0`; when developing next to a repo checkout, add `--local` to generate a `file:` reference that installs offline.

Minimal shape (same semantics as the Python version):

```ts
import { Plugin } from "@opencapx/sdk";

class Hello extends Plugin {
  constructor() {
    super({ manifestPath: "opencapx-plugin.json" });
    this.capability("image.analyze", async (params) => ({
      description: `[hello] got ${String(params["image"] ?? "")}`,
    }));
  }
}

await new Hello().run();
```

Reverse calls (`log` / `emit` / `requestPermission` / `configGet` / `configSet`) are all `async`, with semantics identical to Python (including the `secret:` read form and the bare-value reply); on timeout they resolve to a safe default (permission: 65s → `false`; config kind: 30s → default/`false`) rather than hanging.

### Packaging and Signing (TS authors)

The TS SDK implements only the protocol and does not include Ed25519; packaging and signing are handled uniformly by the `opencapx` CLI, and the output is byte-for-byte the same format as a Python plugin (a TS plugin just needs its `runtime` to point at `node dist/src/plugin.js`).

```bash
# pack and sign
opencapx pack plugins/hello \
  --key @alice-2026.key.hex --key-id com.example.alice \
  --out dist/hello.ocplugin

# sign the plugin index
opencapx sign-index unsigned.json \
  --key @alice-2026.key.hex --key-id com.example.alice \
  --out index.json
```

Install: the main path for `pack` is `pip install opencapx-sdk` (needs `cryptography`), with the equivalent form `python3 -m opencapx_sdk.signing pack ...`; `sign-index` currently exists only in the Rust CLI — use it without installing directly from the repo checkout (`cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- sign-index ...`, no extra dependencies). Arguments, exit codes, and trusted-keys are in [plugin-signing.md](plugin-signing.md).

---

## 1. What a Plugin Is

An OpenCapX plugin is a **standalone process launched by the Core that speaks JSON-RPC 2.0 over stdin/stdout**. The protocol is NDJSON: one complete JSON object per line, ending with `\n`, UTF-8; stdout carries only the protocol, and stderr carries only logs (collected to `logs/plugins/<id>.log`). Any language that can read and write stdin/stdout can write a plugin; the official reference implementation is the Python SDK (`packages/plugin-sdk/`).

A few boundaries you must internalize:

- **The Manifest is the entire contract.** The Core reads only `opencapx-plugin.json`: capabilities, permissions, the runtime command, and settings controls are all in it. The Core does not read your code and does not import your modules.
- **A Capability is declared before it is implemented.** The Manifest's `capabilities[]` is the basis for routing and the UI; the capability list returned during the process handshake must match it, otherwise registration fails.
- **Permissions are enforced by the Core, not by the plugin itself.** The plugin process runs as the same user as the Core and cannot protect itself; you declare `permissions[]` and the Permission Manager decides at call time (see [§6](#6-step-4-permissions-and-domains)).
- **The sandbox declaration is optional.** `sandbox.network` and `sandbox.fs.write` are declarative write/network boundaries that only capability plugins may carry; a pet must not.
- **Signing determines the install UX.** A `.ocplugin` is a ZIP; signatures come in v1 local (HMAC) and v2 distribution (Ed25519) flavors, with three install states: trusted installs directly / unsigned and unknown-key prompt for confirmation / tampered is hard-rejected. Details in [plugin-signing.md](plugin-signing.md).

The plugin state machine (`probe_pending → starting → running → stopped / error / probe_failed`) and health checks are in [plugin-manifest.md](plugin-manifest.md).

---

## 2. Directory Layout and `.ocplugin`

A plugin in the repo looks like this (based on `plugins/things-demo/`):

```text
my-plugin/
├── opencapx-plugin.json     # required, and must be named exactly this
├── bin/
│   └── plugin.py            # runtime.command points at it (relative paths are based on the plugin root)
├── tests/
│   └── test_plugin.py
└── README.md
```

After packaging it is a ZIP with the extension `.ocplugin`:

```text
hello-0.1.0.ocplugin (ZIP)
├── opencapx-plugin.json     # must be at the archive root
├── bin/plugin.py
└── README.md
```

### Real Pitfall: the manifest must be at the archive **root**

At install time the Core scans entries one by one and requires an entry name **exactly equal to** `opencapx-plugin.json`. If you zip the whole plugin directory, you get `hello/opencapx-plugin.json`, the install is rejected, and it reports:

```text
missing opencapx-plugin.json at archive root
```

So do not include an outer directory when zipping. Using the repo script `scripts/pack-ocplugin.sh` avoids this: `os.walk` + `relpath` ensure files land at the root. Entry order does not matter; the manifest need not be first.

Two related facts:

- **The install entry accepts only archives.** The settings-page file picker filters only `.ocplugin`; the Core parses solely by "is this a valid ZIP" and does not check the extension. A directory itself is not a distributable form (there is a dev/test-only `install_from_dir` path in the repo, but the settings page does not expose it; production installs go through `.ocplugin` or a marketplace).
- **A non-ZIP file** is reported as `bad zip: <io error>`.

The full packaging and install commands are in [§8](#8-step-6-package-install-update).

---

## 3. Step 1: Write the manifest

A minimal working manifest (listing only required fields; `plugin-template/opencapx-plugin.json` additionally carries a `settings[]` example):

```json
{
  "id": "com.acme.hello",
  "name": "Hello",
  "description": "One-line description",
  "version": "0.1.0",
  "apiVersion": "1",
  "type": "capability",
  "runtime": {
    "type": "process",
    "command": "python3",
    "args": ["bin/plugin.py"]
  },
  "capabilities": ["image.analyze"],
  "permissions": ["image.read"],
  "author": "your-name",
  "license": "MIT"
}
```

Key points (the full field table is in [plugin-manifest.md](plugin-manifest.md) §Full Fields):

- `type` is only `pet | capability`; `capability` must provide both `capabilities[]` and `runtime`, while `pet` must provide `states[]` and must not have `runtime`.
- `apiVersion` currently matches `"1"` exactly; the Core rejects installation when incompatible.
- `runtime.command` relative paths are relative to the **plugin root directory**; a command in `$PATH` also works (the template uses `python3`). `runtime.args` uses whatever relative paths it wants.
- After publishing, `id` cannot be changed; it is the plugin's identity (updates, data, and keys all hang off it).
- `minCoreVersion` / `dependencies` are optional, for a version lower bound and inter-plugin dependencies.

> To declare your own new domain (such as `weather.*`) instead of reusing built-in capabilities, the elements of `capabilities[]` can use the object form `{"id","permission","default"}`. See [§6](#6-step-4-permissions-and-domains) and [permission-domains.md](permission-domains.md).

---

## 4. Step 2: Write the handler process

With the official SDK, the process skeleton is about 30 lines. Template `plugin-template/bin/plugin.py`:

```python
#!/usr/bin/env python3
import sys
from pathlib import Path

try:
    from opencapx_sdk import Plugin, capability, method
except ImportError:
    # dev mode: walk up to find packages/plugin-sdk in the repo; also works when installed to a temp dir.
    _here = Path(__file__).resolve()
    for _parent in [_here, *_here.parents]:
        _candidate = _parent / "packages" / "plugin-sdk"
        if (_candidate / "opencapx_sdk" / "__init__.py").is_file():
            sys.path.insert(0, str(_candidate))
            break
    from opencapx_sdk import Plugin, capability, method


class HelloPlugin(Plugin):
    @capability("image.analyze")
    def analyze(self, params):
        return {"description": f"[hello] {params.get('image', '')}", "text": "", "objects": []}

    @method("core.probe.capability")
    def probe(self, params):
        return {"ok": True, "via": "hello"}


if __name__ == "__main__":
    HelloPlugin(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()
```

The SDK surface (`packages/plugin-sdk/opencapx_sdk/`):

| What you use | What it does |
|---|---|
| `Plugin` | Takes over the stdio loop and the `plugin.initialize` / `plugin.ping` / `plugin.shutdown` lifecycle |
| `@capability("<id>")` | Marks a method as a capability handler; the method name must equal the capability id |
| `@method("<name>")` | Marks any RPC method (such as `core.probe.capability`, `settings.<key>`) |
| `self.log(level, msg)` | Reverse `core.log` notification |
| `self.emit(kind, payload)` | Reverse `core.emit`; kind is recommended to be prefixed with the plugin id |
| `self.request_permission(permission, reason="")` | Reverse `core.requestPermission`, blocking, returns bool |
| `self.config_get(key, default=None)` | Reverse `config.get`, blocking, returns the bare value |
| `self.config_set(key, value)` | Reverse `config.set`, returns bool |
| `self.previous_version` | The version of the last successful start; `None` on first install |
| `on_initialize` / `on_ping` / `on_shutdown` | Lifecycle hooks, overridable |

### Handshake

`Plugin.run()` handles `plugin.initialize` automatically: `on_initialize` returns `pluginId`, `version`, `apiVersion`, and **the method list collected by the `@capability` decorator** by default:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "pluginId":"com.acme.hello","version":"0.1.0","apiVersion":"1",
  "capabilities":[{"id":"image.analyze","version":"1"}]}}
```

The `capabilities` in the return value must match the manifest; the Core registers capabilities based on it. `previousVersion` is injected during the upgrade handshake so you can perform **idempotent** data migration (see the store v1→v2 in `plugins/weather-demo`).

### How Errors Surface to the Caller

- `raise` any exception in a handler: the SDK converts it to `-32603` (internal error), with `message` like `ValueError: invalid input: title is required`. Business errors can simply be thrown as exceptions; `things-demo` does this throughout.
- Unregistered method: `-32601` method not found.
- `jsonrpc != "2.0"`: `-32600`.
- The Core side additionally has `40001` permission denied, `40002` capability execution failure (a plugin business error, with details in `data`), `40003` timeout, and `40004` apiVersion incompatible. The full table is in [plugin-protocol.md](plugin-protocol.md) §Error Codes.
- **Input schema validation happens in the Core** (a mismatch with `inputSchema` returns `-32602` directly, without disturbing the plugin); output that does not match `outputSchema` records `capability.failed` and is not passed through.

Implementing `core.probe.capability` is recommended: after installing a plugin the Core probes it; not implementing it or timing out is recorded as a probe failure.

---

## 5. Step 3: Give It Parameters (declarative settings `settings[]`)

This is the step authors ask about most. The conclusion first:

> **Declare `settings[]` in the manifest and the settings page renders a graphical form. Do not declare it, and the settings page gives you a raw JSON editor; when the config is empty that box shows `{}`.** This is "why some plugins have only a `{}` box".

**Why declare: without a declaration, the user sees only a key-value editor and the page explicitly says "see the README for meaning" — the host has no idea what `threshold` is or what its legal values are.** With a declaration, the user directly sees localized labels, descriptions, dropdown options, and validation hints; modified items carry a "modified" marker and can be restored to default in one click. This is the standard practice of mainstream editors/launchers: the description lives in the manifest and the host handles rendering.

A declaration also supports three presentation-layer fields (they do not affect storage or `config.*` reads/writes):

- Inside `"options"` you can mix bare strings with `{"value": "...", "label": {"en": "Fast", "zh-Hans": "Fast"}}` — `label` is only for display, while what is stored in config and compared in predicates is always `value`;
- `"order": 1` — the display order in the form, smaller first, with undeclared items falling back to manifest order;
- `"deprecated": {"en": "Use mode instead.", "zh-Hans": "Use mode instead."}` — the control still works, with a "deprecated" marker and reason on the row.

A declaration drives only the **interface**; reading and writing values still goes through `config.*`, and the plugin reads with its own `config_get`. `settings[]` has at most 32 items.

### 5.1 An Example with Three Controls

Excerpted from `plugins/things-demo/opencapx-plugin.json` (abridged):

```json
{
  "settings": [
    {
      "key": "mode", "type": "dropdown",
      "options": ["demo", "live"], "default": "demo",
      "label": { "en": "Backend mode", "zh-Hans": "Backend mode" },
      "description": { "en": "Where the data comes from", "zh-Hans": "Where the data comes from" },
      "aliases": ["backend", "run mode"]
    },
    {
      "key": "store_path", "type": "path",
      "label": { "en": "Store path", "zh-Hans": "Store path" },
      "validate": [
        { "type": "pattern", "regex": "\\.json$",
          "message": { "en": "must end in .json", "zh-Hans": "must end in .json" } }
      ]
    },
    {
      "key": "auth_token", "type": "secret",
      "label": { "en": "Auth token", "zh-Hans": "Auth token" },
      "visible": { "op": "equals", "key": "mode", "value": "live" }
    }
  ]
}
```

For the full field set and the constraints of the 12 controls, see [plugin-manifest.md](plugin-manifest.md) §Declarative Settings — not repeated here. Below, choose a control by "what you want".

### 5.2 Choose a Control by Purpose

| What you want the user to configure | Use `type` | Key constraints |
|---|---|---|
| On/off | `toggle` | Value bool |
| A short text | `text` | Value string |
| A long text (prompt, JSON fragment) | `textarea` | Value string |
| An integer (count, port) | `number` | Value int64; `min`/`max`/`step` configurable |
| A numeric value within a range | `slider` | Must provide both `min`+`max` and `min < max` |
| Pick one of fixed items | `dropdown` or `radio-group` | `options[]` must be non-empty; `default` must ∈ options |
| Accent color | `color` | `default` must match `^#rgb` or `^#rrggbb` |
| A key, token | `secret` | Stored in the keychain; must not declare `default`; the UI only shows set/unset |
| A file or directory path | `path` | Use `pick: "file"` / `"directory"` to decide what is picked (default directory) |
| An action button (run now, test connection) | `button` | Stores no value; must not have `default`/`options`/`validate`; clicking calls the plugin method `settings.<key>` |
| Let the plugin hold a set of string items itself (tags, allowlist) | `list` | Items are persisted by the plugin; must not have `default`/`options`/`min`/`max`/`pick`; add/delete/reorder all call `settings.<key>` (see §5.10) |

`key` rules: `^[a-z][a-z0-9_-]{0,63}$`, unique within the item. **key is a storage key and is not localized**; `options[]` values are stored values and also not localized. To localize display text, use `label`/`description`.

### 5.3 `LocalizedText` and `aliases`

`label` / `description` / `section`, and each `validate[].message`, all accept two forms:

- A plain string (single-language; an old manifest is byte-for-byte unchanged);
- A `{ "en": "...", "zh-Hans": "...", "vi": "..." }` map (locale keys are not restricted to the App's current language).

Resolution order: **current locale → `en` → the lexicographically smallest locale key**. The map must be non-empty, and each translation non-empty after trim.

`aliases` are search keywords (index only, never shown): ≤8 items, each non-empty after trim and ≤40 characters. Put several synonyms across languages so the settings-page search can hit them.

```json
{ "key": "tint", "type": "color", "default": "#3b82f6",
  "label": { "en": "Accent color", "zh-Hans": "Accent color" },
  "aliases": ["theme", "accent color"] }
```

### 5.4 Conditional Display: `visible` / `disabled`

A predicate is **data, not a function** (it must cross the Python → Rust → TS three-process boundary). Evaluation **happens only in the UI**; Rust validates only the shape and references.

| `op` | Shape | Meaning |
|---|---|---|
| `equals` | `{"op":"equals","key":"<declared key>","value":<json>}` | Current value === value |
| `notEquals` | Same as above, `notEquals` | Current value !== value |
| `in` | `{"op":"in","key":"...","values":[...]}` | Current value ∈ values |
| `isSet` | `{"op":"isSet","key":"...","value":true}` | The value is set (`value` must be bool) |
| `all` | `{"op":"all","conds":[...]}` | All true (`conds` non-empty) |
| `any` | `{"op":"any","conds":[...]}` | Any true (`conds` non-empty) |
| `not` | `{"op":"not","cond":{...}}` | Negation |

Rules:

- It may reference only a **key already declared in the same manifest**;
- A `secret` key allows only `isSet` (the value never enters the UI);
- The `equals`/`notEquals`/`in` values of a `dropdown`/`radio-group` must fall within that key's `options[]`;
- Nesting ≤ 8;
- `visible: false` → the entire row is not rendered; `disabled: true` → disabled but still present.

The `visible` of `auth_token` above is a typical usage: it appears only when `mode == "live"`.

### 5.5 Write Validation: `validate[]` and the Null Exemption

`validate` is data; Rust executes it when the user writes to disk (`set_plugin_setting`); the plugin's own reverse `config.set` is **not validated** (the plugin owns its own runtime configuration).

| `type` | Shape | Applicable controls |
|---|---|---|
| `required` | `{"type":"required","message":"..."}` | text / textarea / secret / path / number / color |
| `minLength` | `{"type":"minLength","value":8,...}` | text / textarea / secret / path |
| `maxLength` | `{"type":"maxLength","value":200,...}` | text / textarea / secret / path |
| `min` / `max` | `{"type":"min","value":1,...}` | number / slider |
| `pattern` | `{"type":"pattern","regex":"\\.json$",...}` | text / textarea / path / color / secret |

**Null exemption (the easiest to trip over):** "unset" = missing, `null`, `""`, or an empty array. **Every rule except `required` skips unset values**. So an optional field can be cleared, and only `required` can judge "must not be empty"; a `pattern` on `path` will not fail because of `""` (meaning use the default location).

`pattern` must be a valid Rust regex **and** a valid JavaScript regex: lookahead, inline flags `(?i)`, and Python named groups `(?P<name>…)` are all rejected at install time, not silently ignored. `(?:…)` and `(?<name>…)` are accepted by both.

When a write is rejected the error is `invalid: <message>` (`invalid: <rule-type>` when there is no message). The UI validates under the current locale first and shows localized text, so a normal user sees the localized text rather than this machine string.

### 5.6 Two Install-Time Static Checks

1. **Predicate reference integrity:** every referenced key must already be declared in `settings[]` and obey the secret / options constraints; `all`/`any` must not be empty; nesting ≤ 8.
2. **`default` self-consistency:** `default` (if present) must satisfy the declaration's own `validate[]`. Otherwise the install is rejected, avoiding a default that can never be written back.

In addition, an unknown type, unknown predicate `op`, unknown rule `type`, duplicate key, dropdown missing options, button carrying a value, list carrying `default`/`options`/`min`/`max`/`pick`, secret with a default, number with a non-integer default, slider missing min/max, `section` over 40 characters, `aliases` over 8 items, and so on — any one rejects.

> Note: the Python SDK's `load_manifest()` **validates only top-level fields** (`id/name/version/apiVersion/type`, and a capability's `capabilities[]`/`runtime`) and **does not validate `settings[]`**; the deep validation of `settings[]` happens at Core install time. This is why the demo's tests hand-write a schema assertion (see [§7](#7-step-5-write-tests)).

### 5.7 How to Read Your Own Settings at Runtime

**Re-read on every capability call**; do not read once in `on_initialize` and cache it. Rationale: when the user saves in the settings page, the Core writes config / keychain directly and **does not restart the plugin**. Only by re-reading on every call does saving take effect immediately.

What `things-demo` does (copy this pattern):

```python
def _setting(self, key, default=None):
    try:
        value = self.config_get(key, default)
    except Exception:  # a failed reverse read must never fail a capability call
        return default
    return default if value is None else value

def _setting_str(self, key) -> str:
    value = self._setting(key, "")
    return value.strip() if isinstance(value, str) else ""
```

Two disciplines:

1. **A reverse read must be fault-tolerant.** `config_get` depends on the Core replying; the Core being absent or timing out must not hang an ordinary capability call. Catch the exception and fall back to `default`. `things-demo` has a test (`test_settings_read_failure_falls_back_to_legacy_sources`) specifically pinning this.
2. **Recompute per call, rebuild only on change.** When you need to construct a backend/client from settings, cache "the last resolved result" and compare it with this call's resolved values before deciding whether to rebuild:

```python
def backend(self):
    mode = self._resolve_mode()          # read every time
    store = self._resolve_store_path()   # read every time
    if self._backend is not None and self._backend_mode == mode and self._backend_store == store:
        return self._backend             # unchanged, reuse
    self._backend = ThingsBackend(store) if mode == "live" else DemoBackend(store)
    self._backend_mode, self._backend_store = mode, store
    return self._backend
```

### 5.8 The `secret` Control and `secret:<key>`

- A `secret` control in the Manifest **writes** to the OS keychain (falling back to a 0600 file when the keychain is unavailable); the UI only shows "set / unset" and never echoes the value.
- On the plugin side, **read** with the key prefix `secret:`:

```python
token = self._setting_str("secret:auth_token")   # keychain plaintext, readable only by the plugin itself
```

- On the plugin side, **write** a secret with the same name: `config_set("secret:api_key", "sk-...")`, whose receipt is a fixed mask `"********"` (write-only, never read back).
- On uninstall the fallback secret file is cleaned up; the keychain entry is **not** deleted with the uninstall (the platform cannot enumerate it, see §9).
- A `secret` control must not declare `default` (there is no default value to show, nor should there be).

### 5.9 The `button` Control: Calling `settings.<key>`

A `button` stores no value. On click the Core calls the plugin's reserved method `settings.<key>` with an empty params object, a 10-second timeout, and lazily starts the plugin if needed:

```python
@method("settings.run_now")
def run_now(self, params):
    # return a result quickly or throw a business error
    return {"ok": True}
```

Corresponding manifest:

```json
{ "key": "run_now", "type": "button", "label": "Run now" }
```

The same plugin can both declare settings controls and implement `settings.*` methods; the two do not conflict.

### 5.10 The `list` Control: List Items Are Held by the Plugin Itself

`list` hands a short string list to the plugin process: the host only renders numbered rows + add/delete/reorder buttons, and every action is one RPC to the method `settings.<key>` — the plugin is the sole holder and persists the data itself.

Declare it in the manifest:

```json
{ "key": "tags", "type": "list", "label": { "en": "Tags", "zh-Hans": "Tags" } }
```

Plugin-side implementation:

```python
@method("settings.tags")
def _settings_tags(self, params: dict) -> list:
    tags = self.config_get("tags", []) or []
    op = params.get("op", "list")
    if op == "add":
        value = str(params.get("value", "")).strip()
        if value:
            tags.append(value)
    elif op == "delete":
        i = int(params.get("index", -1))
        if 0 <= i < len(tags):
            tags.pop(i)
    elif op == "move":
        i, j = int(params.get("index", -1)), int(params.get("to", -1))
        if 0 <= i < len(tags) and 0 <= j <= len(tags):
            tags.insert(j, tags.pop(i))
    elif op != "list":
        raise ValueError(f"unknown op: {op}")  # the SDK turns this into a JSON-RPC error; an unknown op must not silently succeed
    self.config_set("tags", tags)
    return tags  # every op returns the current array
```

Rules:

- The method name is fixed as `settings.<key>`, and params is `{"op": ...}` plus the fields that op needs: `add` carries `value`, `delete` carries `index`, `move` carries `index` and `to` (`to` is the index **after** the pop), and `list` carries only `op`. Timeout 10 seconds (the same surface as `button`); the Core lazily starts the plugin if needed.
- **Every op must return the current array** — the host redraws from the return value and keeps no copy itself.
- The host does not persist: **`settings_view` does not contain list values** (like `button`, a predicate cannot reference it), and `set_plugin_setting` rejects a list key outright (`list setting <key> is managed by its plugin`). Persistence uses the plugin's own `config_get`/`config_set` — the example above lands on the `tags` config key.
- `list` must not declare `default`/`options`/`min`/`max`/`pick`; for field-level constraints and install-time rejections see [plugin-manifest.md](plugin-manifest.md) §Declarative Settings.

`plugins/echo-vision/` has an implementation you can compare directly (`settings.tags`; the Rust e2e `settings_list_ops_round_trip` pins the round-trip behavior).

---

## 6. Step 4: Permissions and Domains

### 6.1 Declaring

- **Use a built-in domain**: `capabilities` uses strings, and `permissions` lists permission names. The permission vocabulary and mappings are in [permissions.md](permissions.md). Declaring an unknown permission rejects the install outright.
- **Declare your own new domain**: `capabilities` uses the object form, carrying the mapping and default decision:

```json
{
  "capabilities": [
    { "id": "weather.current",  "permission": "weather.read",  "default": "ask" },
    { "id": "weather.set_home", "permission": "weather.write", "default": "denied" }
  ],
  "permissions": ["weather.read", "weather.write"]
}
```

(Full sample in `plugins/weather-demo/`; rules in [permission-domains.md](permission-domains.md).)

New-domain declaration key points:

- Capability/permission names have two or more segments, `^[a-z][a-z0-9_-]*(\.[a-z][a-z0-9_-]*)+$`, length ≤ 64;
- They must not fall in a reserved domain / reserved capability ID;
- The capability id and the permission must be in the same domain;
- `default` can only be `ask` or `denied`, never implicitly granted;
- All providers of the same capability ID must have a consistent mapping;
- The object form is usable only in a **non-reserved, brand-new** domain.

### 6.2 The Consent Flow

At install time, the Core shows a dialog for each item of the **confirmation set = `permissions[]` ∪ all inline mapped permissions**, displaying "capability → required permission"; it commits only after all pass (confirm first, then persist, then swap the directory, so the old version is intact on rejection).

At runtime, `ask` shows a dialog with the options:

```text
[Allow once]   [Always]   [Deny]
```

- **Built-in permissions**: high-risk permissions do not offer Always; they can only be granted per call.
- **Declared derived permissions (third-party new domains)**: always **once-only**, never offering Always; the settings page also cannot set them permanently to granted, only `ask` / `denied`. `weather-demo`'s `weather.read` prompts on every call by design, not as a bug.

60 seconds of inactivity is treated as Deny and written to the audit.

### 6.3 What Happens on Update

**An update re-confirms permissions and shows a diff.** Before signature binding is complete, there is no silent implementation-replacement channel:

- The preview phase computes `permissionDiff` (added / removed) and shows it in the confirmation dialog; changing the publisher (a keyId change) triggers a full re-confirmation with a warning.
- Mapping change → affected permissions are reset to `ask`;
- Declaration removal → audit; if still referenced by a frozen entry, re-confirm;
- When the publisher is the same throughout and the mapping is completely unchanged, the plan is empty = a silent update that reuses the existing consent.

For the change policy of built-in permissions see [user-guide.md](user-guide.md) (new permissions ask only about the new items); third-party declared domains follow the update section of [permission-domains.md](permission-domains.md).

### 6.4 Sandbox Declaration

```json
"sandbox": { "network": "none", "fs": { "write": ["plugin-data"] } }
```

| Field | Value | Description |
|---|---|---|
| `network` | `"none"` (default) / `"out"` | `none` denies all network; `out` allows outbound |
| `fs.write` | `["plugin-data"]` | Only writes to `~/.opencapx/plugin-data/<id>/` are allowed; all other writes are rejected |

A pet has no process and must not carry it. Enforcement semantics (macOS `sandbox-exec`, unsigned enforcement, `TMPDIR` redirection) are in [plugin-manifest.md](plugin-manifest.md) §Sandbox Declaration.

---

## 7. Step 5: Write Tests

The demos' test pattern: **spawn no subprocess, touch no network, call `Plugin.handle()` directly**, and replace the reverse `config.get` with an in-memory dict. Based on `plugins/things-demo/tests/test_things_demo.py`:

```python
def with_settings(plugin, settings=None):
    """Stand in for reverse config.get: unit tests call handle() directly, so config_get must not block waiting on stdin."""
    values = dict(settings or {})
    plugin.requested_config_keys = []

    def fake_config_get(key, default=None):
        plugin.requested_config_keys.append(key)
        return values.get(key, default)

    plugin.config_get = fake_config_get
    plugin.settings_values = values
    return plugin


def call(plugin, method, params=None, req_id=1):
    return plugin.handle({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}})
```

Things to cover:

- Handshake: the returned capability set matches the manifest;
- `core.probe.capability` returns `{"ok": True}`;
- The happy path of each capability + invalid input (`-32603` with `invalid input` in the message);
- Settings drive behavior: each setting item really changes the result;
- Precedence: setting → manifest/env → default;
- **Re-read on every call**: after changing `settings_values`, the next call takes effect immediately (no restart);
- Fault tolerance: on a reverse-read exception, fall back to the default.

### Drift Tests (recommended)

The Manifest is the contract, and when the code has another table (options, enums, validation) the two drift. Pin the contract with tests. `things-demo`'s `test_manifest_settings_schema_is_install_valid` hand-writes a mirror assertion of the Core's rules; `weather-demo`'s `test_manifest_declares_own_domain` asserts "the declared permission set == the inline mapping set". A step further is to assert that **the manifest's `options[]` equals the constant table in the code**:

```python
def test_manifest_mode_options_match_code_table():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    mode = next(s for s in manifest["settings"] if s["key"] == "mode")
    assert mode["options"] == ["demo", "live"]          # consistent with the branches of _resolve_mode
    assert mode["default"] in mode["options"]
```

Then if you change the enum in the code but forget the manifest, the test goes red.

Run the tests (both entries are in the demo README; pick either):

```bash
# inside the plugin directory, no PYTHONPATH needed (the plugin walks up to find packages/plugin-sdk)
cd plugins/things-demo && python3 -m pytest tests/ -q

# repo root, matching CI
PYTHONPATH=packages/plugin-sdk python -m pytest plugins/things-demo/tests -q
```

Manual smoke test (optional, verifies the real stdio protocol):

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"plugin.initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"things.add","params":{"title":"Buy milk","when":"today"}}' \
  | python3 plugins/things-demo/bin/things_demo.py
```

---

## 8. Step 6: Package, Install, Update

### Packaging

Repo script (packaging only; the manifest lands at the archive root; `<plugin-dir>` must contain `opencapx-plugin.json`):

```bash
scripts/pack-ocplugin.sh plugins/things-demo things-demo.ocplugin
```

To sign, pass both `--sign` and `--key-id` (delegating to the Python SDK's Ed25519 v2 channel):

```bash
scripts/pack-ocplugin.sh plugins/things-demo dist/things-demo.ocplugin \
  --sign @alice-2026.key.hex --key-id com.example.alice
```

Or go directly through the SDK / Rust CLI:

```bash
python3 -m opencapx_sdk.signing pack plugins/things-demo \
  --key @alice-2026.key.hex --key-id com.example.alice \
  --out dist/things-demo.ocplugin

cargo run --manifest-path src-tauri/Cargo.toml --bin opencapx -- keygen --out alice-2026.key.hex
```

For the full arguments, exit codes, and trusted-keys shape of keygen / pack / verify, see [plugin-signing.md](plugin-signing.md).

Before publishing, run the auto-gate pre-check (same source as the listing check; 5 classes: manifest / signature chain / static scan / declaration reconciliation / dependency existence):

```bash
opencapx verify-package dist/things-demo.ocplugin
```

### Install (from file)

Settings page "Plugins" → "Install", and pick a `.ocplugin`. Three trust states:

| Package state | Behavior |
|---|---|
| trusted (valid signature and a registered keyId) | Installs directly |
| unsigned / unknown-key | Warns in a dialog and installs after confirmation |
| tampered / bad-signature / malformed | Hard-rejected, cannot be bypassed |

After install the Core starts the process for a `plugin.ping` health check; on success it is enabled, on failure it stops in `error`.

### Update (from file) vs Uninstall

The plugin card in the settings page has "update from file". It follows the **same non-destructive path** as "install": verify the package id, preview and confirm, then replace the plugin directory in place.

- **An update preserves config and secrets.** The plugin config lives in `~/.opencapx/config/<plugin_id>.json`, and secrets live in the keychain or a fallback file, both separate from the plugin directory; the update only swaps the directory and touches neither.
- **An update re-confirms permissions and shows a diff** (see [§6.3](#63-what-happens-on-update)); changing the publisher triggers a full confirmation with a warning.
- **Uninstall clears the config and the fallback secret file** and deletes the database record. But **secret entries in the OS keychain cannot be enumerated and are not deleted with the uninstall**; an explicit single-key delete clears both the keychain and the fallback file.
- For a plugin installed via dev's "directory install", uninstalling does not delete your source directory.

---

## 9. Pitfalls

### 9.1 Environment Isolation: Plugins Do Not Inherit the Host Environment

Since S1, when the Core spawns a plugin it calls `env_clear` and back-fills only a minimal allowlist: `PATH` / `HOME` / `TMPDIR` / `TZ` / `LANG`, plus `LC_*`, `XDG_*`, and any keys the user appends in `plugin_env_allowlist` (comma-separated). Setting `plugin_env_isolation=false` restores inheritance (default `true`).

- **`runtime.env` (statically declared by the author in the manifest) is always injected** and is unaffected by isolation. This is the most reliable override.
- **Declarative settings** are preferable: put configurable things in `settings[]` and have the plugin read them with `config_get`; sensitive values use the `secret` control (keychain). This way nothing depends on environment variables.
- For a legacy environment variable to take effect, **its name must be in the user's allowlist**. `things-demo`'s README marks legacy env as legacy and prompts you to add it to the allowlist.
- When a plugin genuinely needs an environment variable, write the key name in the README and prompt the user to add it to `plugin_env_allowlist`; do not assume it can read it by default.
- Honest boundary: the Core side provides the `get_plugin_env_policy` / `set_plugin_env_policy` commands, but `src/` currently has **no corresponding settings-page control**, so users must edit the settings file to fill the allowlist. Go by the version in your hands.

### 9.2 Declare Settings, or the User Gets Only a JSON Box

When `settings[]` is not declared, the settings page renders an **editable raw JSON editor** whose content is the plugin's entire config; when the config is empty it shows `{}`. A plugin that declares `settings[]` renders a graphical form, and the raw JSON degrades to a read-only view. So "a plugin with only a `{}` box" usually just means the manifest has no `settings[]`.

### 9.3 Data Migration: Do Not Destroy Data You Cannot Rebuild

On upgrade the Core tells you the version of the last successful start via `previousVersion`, but **the host does not move your data**; the store and config are kept as-is, and cross-version reading is the plugin's own responsibility to make compatible. Disciplines:

- Migrate in `on_initialize`, triggering on the **data shape** rather than the version number alone (safe across reruns, rollbacks, and downgrades);
- Migration must be **idempotent**;
- **Do not delete fields you cannot rebuild from the current data**; prefer keeping old fields to washing away user data;
- A failed handshake does not advance the recorded version, so migration code must tolerate retries.

`plugins/weather-demo` is the reference: store v1 `{"home": …}` → v2 `{"schema": 2, "home_city": …}`, decided by the `schema` field, with `previousVersion` used only to annotate the source.

### 9.4 Other

- **`storePath` is not a Core manifest field.** `things-demo`/`weather-demo` read it from their own manifest; it is just a plugin-specific convention, and the Core neither validates nor consumes it. Do not treat it as a supported public field.
- **The manifest is at the archive root**, not a subdirectory inside it (see [§2](#2-directory-layout-and-ocplugin)).
- **A setting's `validate` only takes effect when the UI/Core writes**, not when the plugin itself calls `config_set`; so the plugin must treat runtime input as untrusted data and validate it again.
- **`secret` must not have `default`; `button` must not have `default`/`options`/`validate`;** both are rejected at install time.
- **Do not assume stdout can be printed to freely.** stdout carries only the protocol; for debugging use `self.log` or stderr.

---

## 10. Next Steps

Four samples you can read directly:

| Plugin | What it demonstrates |
|---|---|
| `plugins/echo-vision/` | Minimal capability + probe reference; settings have only a `list` control (`tags` options are held by the plugin process itself, with add/delete/reorder round-tripping through `settings.tags`) |
| `plugins/weather-demo/` | Minimal two capabilities + declaring its own permission domain + idempotent store migration |
| `plugins/things-demo/` | The best-practice template: three declarative settings (dropdown / path / secret), a `visible` predicate, a `validate` pattern, `LocalizedText`, `aliases`, and the actual pattern for re-reading settings on every call |
| `plugin-template/` | The scaffold source + tag-triggered signed-release CI (`.github/workflows/release.yml`) |

Docs:

- [plugin-manifest.md](plugin-manifest.md): field-level specs (start here for fields you haven't looked up yet)
- [plugin-protocol.md](plugin-protocol.md): the protocol, reverse calls, `settings.<key>`
- [capability.md](capability.md): capability naming, Schema, registration and routing
- [permissions.md](permissions.md) / [permission-domains.md](permission-domains.md): the permission model and declarative new domains
- [plugin-signing.md](plugin-signing.md): keygen / pack / verify, exit codes, env isolation (§10)
- [plugin-review.md](plugin-review.md): the listing auto-gate's 5 classes of checks
