# MCP Gateway Specification


## Transport Model

```text
Agent host (Claude Code)
   │  spawns a stdio child process (the same pattern as the existing opencapx hook subcommands)
   ▼
opencapx mcp ──HTTP──▶ Core (127.0.0.1:47628, existing listener)
```

- The Agent is configured in the standard MCP stdio way: command points to the OpenCapX binary, args is `["mcp"]`
- `opencapx mcp` is a thin forwarding layer: MCP protocol ⇄ Core's `POST /rpc`
- When Core is not running, `opencapx mcp` returns a clear error for every tool call ("OpenCapX app is not running") rather than failing silently
- The Rust-side implementation uses the official `rmcp` crate; Core adds a `POST /rpc {tool, input}` endpoint that returns `{ok, result|error}`
- **Authentication (mandatory)**: at startup it completes Agent identity registration via `POST /agents/register` (TOFU, the token is written to disk automatically, zero configuration for the user), after which every `/rpc` request carries `Authorization: Bearer ocx1_…` and `X-OpenCapX-Agent`. For details see "Agent Identity" in [permissions.md](permissions.md); an unauthenticated request is always 40101
- The host configuration examples need no changes at all — the token is managed by the CLI and Core themselves and never enters the Agent host's configuration file

## Tool Set (v1)

### opencapx.say

Makes the pet bubble speak.

```json
// input
{ "text": "Got the task!", "duration": 5 }
// output
{ "ok": true }
```

`duration` is in seconds and optional; it defaults to `bubbleDuration` from Settings.

### opencapx.notify

System notification: it sends an OS toast while also recording a `notification.posted` event (the data source for the i2 §13 Notification
Center, aggregated in the Settings page's "Notification Center" tab, with an unread badge).

```json
{ "title": "Build", "body": "Build complete", "severity": "info" }   →   { "ok": true }
```

`severity` is optional, `info | warn | error` (default `info`); an invalid value is rejected outright.

### opencapx.set_state

Directly sets the pet's logical state.

```json
// input
{ "state": "thinking", "message": "Analyzing image…" }
// output
{ "ok": true }
```

`state` values are listed under "Agent State Protocol" below; an invalid value is rejected outright.

### Agent State Protocol (v1.1, i2 §12)

The pet is the Agent's "emotion/state engine": the Agent only reports a state, and the animation/bubble/sound are
rendered by OpenCapX; third-party pets are all compatible with the same enumeration.

| state | Semantics | Trigger source |
|---|---|---|
| `idle` | Idle (default) | `agent.started`; falls back after a long period of no activity |
| `thinking` | Thinking | The Agent reports it actively via `set_state` |
| `working` | Working | `agent.working`; hook events map to it automatically |
| `waiting` | Waiting for user input | `agent.waiting` |
| `permission` | Waiting for user authorization (🥺 pleading) | `permission.requested` (when Core pops the permission confirmation) |
| `success` | Completed (brief, falls back on its own) | `agent.completed` |
| `error` | Errored | `agent.error` |
| `sleeping` | Sleeping (night/manual) | The Agent reports it actively via `set_state` |

Conventions:

- `permission` and `waiting` are kept separate: waiting for authorization is a key moment of human-computer interaction, so the pet's expression is stronger (animation + breathing pulse).
- The Core-internal session has only 4 states (`working/waiting/done/idle`), and the protocol boundary normalizes:
  `done ≙ success`, `thinking ≙ working` (emotional granularity), `sleeping ≙ idle`.
- Event mapping happens in Core (`core::pet::map_agent_to_pet`), and the specific animation is decided by the pet plugin;
  the front-end overlay badge renders its text and colors from `pet.state`.

### opencapx.ask

Asks the user a question and blocks until the user answers. v1 only had single choice; v1.1 extends it to **three question types + timeout policy**.

```json
// single choice (v1 as-is, the default type)
{ "question": "Which database?", "options": ["SQLite", "Postgres"], "timeout": 300 }
// multiple choice
{ "question": "Which components to install?", "options": ["server", "cli", "docs"], "multi": true }
// form
{ "question": "Connect to the database",
  "fields": [
    { "name": "host", "label": "Host", "type": "text", "placeholder": "localhost", "required": true },
    { "name": "port", "label": "Port", "type": "number", "default": 5432 },
    { "name": "mode", "type": "select", "options": ["a", "b"] },
    { "name": "verbose", "type": "checkbox", "default": false }
  ] }
// timeout policy
{ "question": "Continue?", "options": ["Continue", "Stop"],
  "timeout": 60, "onTimeout": "default", "defaultAnswer": "Stop" }
```

```json
// output: single choice / multiple choice / form
{ "answer": "SQLite" }
{ "answer": ["server", "cli"] }
{ "answer": { "host": "db.local", "port": 5432, "mode": "a", "verbose": false } }
// timeout + onTimeout=error (default, v1 behavior)
{ "ok": false, "error": "timeout" }
// timeout + onTimeout=default
{ "ok": true, "answer": "Stop", "timedOut": true }
```

Rules:

- `options` and `fields` are **mutually exclusive**; giving both or neither is rejected outright; `multi` pairs only with `options`
- A `fields` entry's `type` ∈ `text | number | select | checkbox`; `name` is unique; `select` must carry a non-empty `options`; `required` only applies to text/number/select
- `onTimeout` ∈ `error` (default, v1 behavior) | `default`; `default` must carry a `defaultAnswer` whose **shape matches the question type** (single choice = a string ∈ options; multiple choice = a non-empty array with each item ∈ options; form = covers every required field) — this prevents a misspelled default from silently taking effect
- `timeout` is in seconds, default 300, ceiling 900
- Note for the Agent side: this is a long-blocking tool call, so plan your timing; for cancellation semantics see "Cancellation (notifications/cancelled)"

### opencapx.list_capabilities

```json
// input  {}
// output
{
  "capabilities": [
    { "id": "image.analyze", "version": "1",
      "providers": ["com.opencapx.vision"] }
  ]
}
```

### opencapx.execute

Executes a capability through the complete chain (Router + Permission + plugin).

```json
// input
{ "capability": "image.analyze", "input": { "image": "/tmp/test.png" } }
// output = the envelope wraps the plugin result (v1 does no outputSchema validation, passes it through as-is)
{ "ok": true, "result": { "description": "Screenshot of a server monitoring UI", "text": "CPU 92%", "objects": [] } }
// no provider / all providers failed
{ "ok": false, "error": "capability_unavailable" } | { "ok": false, "error": "capability_failed" }
```

**Permission-denial error surface (v1.5, Agent self-healing)** — a denial is no longer a bare `capability_failed`; the error body carries
`code` + `detail` (layer, permission, remediation hint), and the Agent can use it to retry to trigger the dialog / change the path / ask the user for authorization:

```json
// 40001: agent-layer gate denial (before handle dispatch)
{ "ok": false, "error": "permission_denied", "code": 40001,
  "detail": "agent layer: file.read — ask the user to grant it (OpenCapX Settings → Permissions) or retry to trigger the ask prompt" }
// 40002: plugin-layer permission all-denied (every provider was denied on permission)
{ "ok": false, "error": "permission_denied", "code": 40002,
  "detail": "plugin layer: weather.read — the user must grant this permission to the plugin (OpenCapX Settings → Plugins), then retry" }
// 40003: path outside scope (permissions.md §Scope; layer ∈ agent|plugin)
{ "ok": false, "error": "scope_denied", "code": 40003,
  "detail": "path is outside the allowed scope (agent layer, file.read) — retry with a path under the allowed scope, or ask the user to widen it (OpenCapX Settings → Permissions)" }
```

A permission denial manifests as `capability_failed` (the Router records attemptedProviders including `permission_denied`), and there is a `permission.denied` audit in the event stream.

## Subscription Tool Pair (v1.1)

### opencapx.subscribe

Subscribes to a capability with `type: "subscribe"` (for the types see "Capability Types" in [capability.md](capability.md)); after subscribing, Core keeps pushing events.

```json
// input
{ "capability": "file.watch", "input": { "path": "~/project", "recursive": true } }
// output
{ "ok": true, "subscriptionId": "sub_01HX…" }
// the capability is call type
{ "ok": false, "error": "capability is not subscribable" }
```

Events are pushed as a server→client notification (no id, no response needed):

```json
{ "jsonrpc": "2.0", "method": "opencapx.event",
  "params": { "subscriptionId": "sub_01HX…", "capability": "file.watch",
              "payload": { "event": "modified", "path": "~/project/src/main.rs" } } }
```

- The event payload shape is defined by that capability's `eventSchema`
- A subscription never times out; the subscription count is capped at 32 per Agent (v1, leak prevention)
- When the Agent disconnects (the MCP process exits), Core automatically cleans up the subscription and the underlying watcher; subscriptions do not survive across connections

Implementation (landed in v1.1): events are relayed through the `opencapx mcp` process — the first `opencapx.subscribe` triggers it to establish a long-lived SSE `GET /events` connection to Core (carrying the `X-OpenCapX-Conn` connection identifier + Agent credentials), and `capability.event` on the Bus is filtered by subscriptionId and forwarded as `opencapx.event`. That SSE connection is the anchor for the connection lifecycle: the CLI process exits → the SSE disconnects → Core cleans up all subscriptions under that connection.

### opencapx.unsubscribe

```json
// input
{ "subscriptionId": "sub_01HX…" }
// output (echoes back subscriptionId, so the client can clear its bookkeeping)
{ "ok": true, "subscriptionId": "sub_01HX…" }
```

Idempotent: a non-existent subscriptionId also returns `ok: true` (repeat unsubscribes and subscriptions already cleaned up by a disconnect are not errors).

## Cancellation (notifications/cancelled)

During a long-blocking call (`opencapx.ask` up to 900s; a slow plugin's `opencapx.execute`), the Agent host can send a cancellation notification per the MCP standard:

```json
// client → server, notification, no response
{ "jsonrpc": "2.0", "method": "notifications/cancelled",
  "params": { "requestId": 42, "reason": "user interrupt" } }
// the cancelled original request then finishes with this
{ "jsonrpc": "2.0", "id": 42, "error": { "code": -32800, "message": "Request cancelled" } }
```

Semantics, and how they differ from timeout:

- **timeout is Core's timer firing**, returning `{ "error": "timeout" }`; **cancelled is the Agent actively giving up**, happening at any moment and finishing with -32800 — so the Agent can tell "waited too long" apart from "I don't want it anymore"
- Cancel = stop waiting + discard subsequent results, **not a transaction rollback**: the plugin call Core already issued runs to completion, only its result is no longer delivered
- A cancelled `opencapx.ask` that is already on screen finishes the same way as a timeout (the bubble is cleared) and does not report an error to the user
- Cancelling an `opencapx.subscribe` only affects the single call that is "establishing the subscription"; once the subscription exists, end it with `opencapx.unsubscribe`

Propagation boundaries (half landed in v1.1, by layer):

- CLI layer, implemented: `opencapx mcp` maintains a cancellation table; on receiving a cancellation notification it immediately returns -32800 and discards the original result if it arrives late
- Core layer, implemented: the `/rpc` request body can carry a top-level `requestId` (the CLI automatically passes through the JSON-RPC id, stringified); on cancellation the CLI fire-and-forgets `POST /rpc/cancel {requestId}` and Core sets a cancellation flag — a pending `opencapx.ask` finishes within 50ms, the bubble closes immediately, and `cancelled` is returned
- Plugin-process calls (an in-flight `opencapx.execute` going through the Router) are not interrupted for now: Core still produces the result as usual but the CLI discards it; process-level interruption is left to v2

## Request-Chain Trace

Every `POST /rpc` (that is, every tool call by an Agent) records a span tree, written as NDJSON to
`~/.opencapx/traces/rpc/<agent_id>/<trace_id>.ndjson`, append-only, silently ignoring write failures,
not affecting the main path. Where to view it: Settings → Agents → a given agent card → "Request Chain".

Line protocol (`ev` has three states, camelCase):

| ev     | Fields                                          | Meaning                       |
| ------ | ----------------------------------------------- | -------------------------- |
| start  | spanId / parentId? / name / ts / attrs        | span begins; no end = still pending |
| event  | spanId / name / ts / attrs                    | an instantaneous event attached to the top-of-stack span   |
| end    | spanId / ts / status(ok/error) / error? / attrs? | span ends               |

Tree shape:

- `rpc` (root; attrs include agent/conn, the end line carries durMs)
  - `dispatch` event: tool / requestId / input preview (4KB truncation)
  - `permission.agent` event: Agent-layer gate decision (including denial)
  - `capability.<name>` span: total dispatch duration/outcome
    - `gate.denied` event: the plugin-layer gate denies that provider
    - `plugin.<pluginId>` span: a single plugin JSON-RPC, attrs.sessionId
      maps directly to the frame-level dump in `traces/<plugin_id>/<session_id>.ndjson`
  - `ask.shown / ask.answered / ask.timeout / ask.cancelled` events

retention follows the same policy as the plugin traces: the most recent 20 per agent directory, 14 days, 50MB
(`retention.rs::prune_traces`).

Privacy: the dispatch event contains a tool input preview (4KB truncation) and is written to local disk as-is — the
same exposure level as the plugin trace frame-level dump and event replay (a local-machine debugging artifact, expiring with
retention, never leaving the device). No redaction is done; if needed, that is a separate requirement.

Hook-side events (`POST /event` → `agent.*`) are written separately to `traces/hooks/<agent_kind>/<session_id>.ndjson`
and displayed side by side in the same viewer; the rpc trace and the hook session have no exact foreign key and are aligned by same agent + time window.

## Agent Host Configuration

Preferred one-command setup (from v1.5, idempotent, repeatable):

```bash
opencapx connect claude   # or: codex / opencode / omp
```

It writes both the hook and the MCP server entry for that agent in one go — for hosts that have an MCP target (claude / codex / opencode / omp); hooks-only hosts (pi) get the hook side and a note; the auth token is handled by `opencapx mcp`
itself via TOFU at startup, and the host configuration contains no credentials. Manual configuration is equivalent to the examples below.

**Stable CLI path.** The entries `connect` writes point at `~/.opencapx/bin/opencapx` — a stable
copy of the running binary — never at the binary's current location. A dev checkout that
moves or gets `cargo clean`ed, or an app reinstall, therefore never leaves agents firing a
dead path: the shim is refreshed at app start, at `connect`, and on every hook invocation,
and any config still referencing an older binary path is rewritten in place. To use
`opencapx` yourself from a terminal, install the same shim into `PATH` once — `opencapx
install-cli`, or **Settings → General → Command line** (see [INSTALL.md](../INSTALL.md#the-opencapx-command)).

Claude Code (`mcpServers` in `~/.claude.json`, or a project `.mcp.json`):

```json
{
  "mcpServers": {
    "opencapx": {
      "command": "~/.opencapx/bin/opencapx",
      "args": ["mcp"]
    }
  }
}
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.opencapx]
command = "~/.opencapx/bin/opencapx"
args = ["mcp"]
env = { OPEN_CAPX_AGENT = "codex" }
```

The `env` line pins the agent identity: Codex exports no host-signature variable
(unlike Claude Code's `CLAUDECODE=1` or OpenCode's `OPENCODE=1`) to stdio MCP children, so
without it the MCP process would register as `custom` while the hooks register as `codex` —
one session split across two agents. `connect` writes it, and the repair pass backfills it
into blocks written by older builds.

OpenCode (`opencode.json`):

```json
{
  "mcp": {
    "opencapx": {
      "type": "local",
      "command": ["/Applications/OpenCapX.app/Contents/MacOS/opencapx", "mcp"]
    }
  }
}
```

In dev mode, point directly at `target/debug/opencapx`.

OMP (`~/.omp/agent/mcp.json`):

```json
{
  "mcpServers": {
    "opencapx": {
      "type": "stdio",
      "command": "~/.opencapx/bin/opencapx",
      "args": ["mcp"],
      "env": { "OPEN_CAPX_AGENT": "omp" }
    }
  }
}
```

The `env` line pins the agent identity for the same reason as Codex: OMP exports no
host-signature variable to stdio MCP children, so without it the MCP process would register as
`custom` while the hooks register as `omp` — one session split across two agents.

## Session-start Capability Injection

`list_capabilities` requires the agent to already know OpenCapX exists. To close that gap,
the SessionStart hook injects a capability digest into the agent's context, so a fresh
session knows what it can call before anything else happens:

```
SessionStart hook → POST /event → Core replies
  {"ok":true,"additionalContext":"OpenCapX (desktop layer for agents) is connected. …
   - image.analyze — execute — providers: core
   - file.watch — subscribe — providers: core …"}
→ hook prints {"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":…}} to stdout
→ the host merges it into the session context
```

Properties:

- **Same data source as `list_capabilities`** (the capability registry), read at session
  start, so it is never stale beyond the session.
- **Capped**: at most 60 entries / 4 KiB; the remainder collapses into a
  `(+N more — call opencapx.list_capabilities)` marker.
- **Host whitelist**: only hosts whose SessionStart hook documents `additionalContext`
  receive the injection — Claude Code, Codex, Factory Droid, and Grok Build today. All
  other hosts keep the historical zero-stdout behavior (dumb pipe).
- **Opt-out**: Settings → General → "Session-start capability brief" (default on).
  Disabling it returns every host to zero stdout.
- **Fail-open**: Core not running, or nothing registered → no stdout, no injection, the
  hook never blocks.
- **Audited**: each injection emits a `session.context.injected` event into the Activity
  Timeline.

## Evolution

When additional capabilities such as `video.analyze` or `document.parse` are added later, only the capability and the plugin are added; the MCP tool set stays unchanged. `opencapx.execute` is the universal exit, which is exactly why it must pass through the Permission Manager.
