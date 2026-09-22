# Things Demo (OpenCapX plugin)

A **demo capability plugin** that exposes Things3-style todo CRUD to any Agent
(Claude Code / Codex / OpenCode…) through OpenCapX.

**It works without Things3 installed.** By default it stores todos in a local
JSON file, so you can install it, call it, and see the full capability pipeline
work end-to-end with zero setup. Point it at Things3 later with one setting.

- Plugin id: `com.opencapx.things-demo`
- Type: `capability`
- Capabilities: `things.add`, `things.update`, `things.list`, `things.show`, `things.search`, `things.delete`
- Permissions: `url.scheme.open`, `things.read`, `things.write`

> `things.read` / `things.write` are declared in `opencapx-plugin.json` but are
> only *enforced* once the Core capability registry lands. Until then the demo
> store needs no permission.

---

## Backend modes

| Mode | When | What happens |
|---|---|---|
| **demo** (default) | always, no Things3 required | Reads/writes a local JSON file, resolved in order: setting `store_path` → manifest `storePath` → `${OPENCAPX_THINGS_STORE}` (allowlisted env) → `${HOME}/.opencapx/plugin-data/com.opencapx.things-demo/store.json`. Stdlib only. |
| **live** | `mode` setting = `live` (Things3 is macOS-only; elsewhere calls fall back to the mirror), or legacy `OPENCAPX_THINGS_MODE=live` **and** macOS (`darwin`) | Writes build a `things:///` URL and open it with the macOS `open` command (Things performs the mutation). Missing `tags` are auto-created via JXA first — the URL scheme only applies tags that already exist. Reads try JXA via `osascript`. Everything is mirrored to the demo store, and **any failure falls back to demo** and is reported in the result's `via` field. Never crashes when Things3 is absent. |

Every capability result carries a `via` field describing how it was served,
e.g. `"demo"`, `"things-url"`, `"things-jxa"`, or
`"demo (things read failed: …)"`.

> **Configure via Settings, not env.** Settings are declared in
> `opencapx-plugin.json` and render in OpenCapX **Settings → Plugins → Things
> Demo**; the plugin re-reads them on every capability call, so a save takes
> effect immediately — no plugin restart. The legacy env vars below still work
> but require their names in the user env allowlist (`docs/plugin-signing.md`
> §10), because plugins do not inherit the host environment by default (S1).

## Settings

Declared in `opencapx-plugin.json` (`settings[]`) and edited in the Plugin config tab.
Resolution is **setting first**, then the legacy manifest / env source, then the
built-in default.

| Setting | Type | Default | Effect |
|---|---|---|---|
| `mode` | dropdown (`demo` / `live`) | `demo` | Selects `DemoBackend` vs `ThingsBackend`. Replaces `OPENCAPX_THINGS_MODE`. |
| `store_path` | path | *(empty → built-in plugin-data store)* | Overrides the demo JSON store location. Replaces `OPENCAPX_THINGS_STORE`. |
| `auth_token` | secret | *(unset)* | Things3 auth token for `things:///update`, stored in the OS keychain (masked on write; readable back only by the plugin). Without it, live updates are skipped and only the local mirror is updated. Replaces `THINGS_AUTH_TOKEN`. |

## Legacy environment variables

Kept for backward compatibility; **legacy, requires an env allowlist entry
(`docs/plugin-signing.md` §10)**. Each is only consulted when the matching
setting above is unset/empty.

| Variable | Default | Purpose |
|---|---|---|
| `OPENCAPX_THINGS_STORE` | built-in plugin-data store | Path to the demo JSON store. Used only when `store_path` is empty. |
| `OPENCAPX_THINGS_MODE` | `demo` | Set to `live` to enable the Things3 URL/JXA backend (macOS only). Used only when `mode` is unset. |
| `THINGS_AUTH_TOKEN` | *(unset)* | Required by Things3 for `things:///update`. Used only when the `auth_token` secret is unset. |

## Install

**From settings (packaged plugin)**

1. Package the folder as `things-demo.ocplugin` (zip of `opencapx-plugin.json` +
   `bin/`).
2. Open OpenCapX **Settings → Plugins → Install…** and pick the `.ocplugin`
   file.
3. Confirm the `url.scheme.open` / `things.*` permission prompts.

**Dev run (from the repo)**

The plugin is launched by Core as:

```bash
python3 bin/things_demo.py   # cwd = plugins/things-demo/
```

It speaks JSON-RPC 2.0 over stdio (`docs/plugin-protocol.md`). The SDK is
imported directly; if it isn't installed, the plugin walks up to
`packages/plugin-sdk/` automatically. To run it by hand:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"plugin.initialize","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"things.add","params":{"title":"Buy milk","when":"today"}}' \
  | python3 plugins/things-demo/bin/things_demo.py
```

## Example AI prompts

Ask your Agent (which calls OpenCapX capabilities) in plain language:

| Language | Prompt | Capability used |
|---|---|---|
| 🇨🇳 中文 | “查一下我**今天**要做的事” | `things.list` (`list: "today"`) |
| 🇨🇳 中文 | “帮我**新建**一个任务：明天交房租” | `things.add` |
| 🇨🇳 中文 | “把‘买牛奶’标记为**完成**” | `things.search` → `things.update` |
| 🇬🇧 English | “Show me my **today** to-dos” | `things.list` (`list: "today"`) |
| 🇬🇧 English | “**Create** a task: pay rent tomorrow” | `things.add` |
| 🇬🇧 English | “Mark ‘buy milk’ as **done**” | `things.search` → `things.update` |

## CRUD walkthrough

All params and results are JSON-serializable. Examples below are raw JSON-RPC
requests (what Core would send).

**1. add** — `things.add`

```json
{"jsonrpc":"2.0","id":10,"method":"things.add",
 "params":{"title":"Buy milk","notes":"2%","when":"today","tags":["errand"],"list":"Errands"}}
```

```json
{"result":{"ok":true,"id":"9f3a1c2d","title":"Buy milk","via":"demo"}}
```

**2. list** — `things.list`

```json
{"jsonrpc":"2.0","id":11,"method":"things.list",
 "params":{"list":"today","limit":20,"include_completed":false}}
```

```json
{"result":{"count":1,"todos":[{"id":"9f3a1c2d","title":"Buy milk","when":"today","completed":false,"tags":["errand"],"list":"Errands","created_at":"...","updated_at":"..."}],"via":"demo"}}
```

**3. show** — `things.show` (by `id`, or by `query` — exactly one)

```json
{"jsonrpc":"2.0","id":12,"method":"things.show","params":{"id":"9f3a1c2d"}}
```

```json
{"result":{"todo":{"id":"9f3a1c2d","title":"Buy milk","completed":false,"...":"..."},"via":"demo"}}
```

**4. update** — `things.update` (mark completed, append notes, add tags)

```json
{"jsonrpc":"2.0","id":13,"method":"things.update",
 "params":{"id":"9f3a1c2d","completed":true,"append_notes":"got oat milk instead","add_tags":["shopping"]}}
```

```json
{"result":{"ok":true,"id":"9f3a1c2d","updated":{"id":"9f3a1c2d","completed":true,"notes":"2%got oat milk instead","tags":["errand","shopping"],"updated_at":"..."},"via":"demo"}}
```

**5. search** — `things.search`

```json
{"jsonrpc":"2.0","id":14,"method":"things.search","params":{"query":"milk"}}
```

```json
{"result":{"count":1,"todos":[{"id":"9f3a1c2d","title":"Buy milk","completed":true,"...":"..."}],"via":"demo"}}
```

**6. delete** — `things.delete`

```json
{"jsonrpc":"2.0","id":15,"method":"things.delete","params":{"id":"9f3a1c2d"}}
```

```json
{"result":{"ok":true,"id":"9f3a1c2d","via":"demo"}}
```

In live mode delete runs via JXA (`Application("Things3")`) — the Things URL
scheme has no delete command; the demo store removes the entry directly.

### Todo shape (demo store)

```json
{
  "id": "9f3a1c2d",
  "title": "Buy milk",
  "notes": "",
  "when": "inbox | today | tomorrow | anytime | someday | YYYY-MM-DD",
  "deadline": "YYYY-MM-DD | null",
  "tags": ["errand"],
  "list": "Errands",
  "completed": false,
  "created_at": "2026-01-01T00:00:00+00:00",
  "updated_at": "2026-01-01T00:00:00+00:00"
}
```

### Validation errors

Invalid input raises an exception whose message starts with `invalid input: …`;
the SDK converts it to a JSON-RPC `-32603` (internal error) response. Examples:
missing `title`, empty `query`, unknown `list` filter, `limit` outside `1..100`,
`things.show` with neither/both of `id` and `query`, and updates or deletes to
an unknown `id`.

## Tests

Run from the plugin directory — the plugin walks up to `packages/plugin-sdk`, so
no `PYTHONPATH` is needed:

```bash
cd plugins/things-demo && python3 -m pytest tests/ -q
```

Or from the repo root, matching CI:

```bash
PYTHONPATH=packages/plugin-sdk python -m pytest plugins/things-demo/tests -q
```

Tests call `Plugin.handle()` directly — **no subprocess, no network, no Things3**
— against a temporary store via `tmp_path`, with the reverse `config.get`
served from an in-memory dict. They cover the initialize handshake
(6 capabilities), `core.probe.capability`, the full
add → list → show → update(completed) → search → delete roundtrip, invalid
input, and the declarative `settings[]` contract: each setting driving behavior,
the setting → manifest/env → default precedence, the `secret:auth_token` read
path, and per-call reload (a changed setting takes effect on the next call
without restarting the plugin).
