# Architecture

This document is assembled from the specifications under `docs/`. It is a map, not a replacement: every section points at the canonical doc under `docs/` that owns the details. If the two disagree, the doc under `docs/` wins.

The three rules the whole design follows, quoted from [docs/README.md](docs/README.md):

1. Tauri is the host, not the plugin framework. An OpenCapX plugin is not a Tauri plugin. Plugins are separate processes speaking their own JSON-RPC protocol.
2. An agent calls a capability, never a specific plugin and never a specific model. `Agent -> image.analyze -> Router -> some Vision plugin`, and the plugin can change without the agent knowing.
3. Every dangerous operation goes through the Rust Permission Manager. A WebView is never a security boundary; the Rust core is.

## At a glance

```text
                Claude Code / Codex / OpenCode / ...
                           │
              hooks (POST /event) + MCP (spawn: opencapx mcp)
                           │
                           ▼
        ┌──────────────────────────────────────────────────┐
        │                    Core (Rust)                   │
        │  HTTP entry: POST /event  POST /rpc  GET /events │
        │  Event Bus ─ Agent Manager ─ Capability Router   │
        │  Permission Manager ─ Plugin Manager ─ Process   │
        └──────┬─────────────────────────────┬─────────────┘
               │                             │
               ▼                             ▼
     Pet UI (Tauri WebView overlay)   Plugin Runtime (separate processes)
     animation / bubble / settings    JSON-RPC 2.0 over stdio, signed, sandboxed
```

The core owns the event bus, the capability registry and router, permissions, and the plugin lifecycle. The WebView renders the pet and the settings UI but holds no authority. Plugins are isolated child processes; the router picks a provider, the permission manager gates the call, and results are validated before they travel back to the agent.

Component map — the twelve core subsystems and where they sit:

```text
OpenCapX
├── Tauri App
│
├── Rust Core
│   ├── Agent Manager
│   ├── Capability Router
│   ├── Plugin Manager
│   ├── Permission Manager
│   ├── Process Manager
│   ├── Event Bus
│   ├── Context Manager
│   ├── Request Trace
│   ├── MCP Gateway
│   ├── Config Manager
│   ├── Storage
│   └── Logging
│
├── TypeScript UI (framework-free, Three.js pet)
│
└── Plugin Runtime
```

Names are subsystems, not file names — `core/` holds 60+ modules behind these twelve labels (for example Plugin Manager spans `plugin.rs` plus its dependency, metrics, signature, and trace modules; Storage includes backup and retention). The UI is hand-rolled TypeScript over Tauri APIs with Three.js for the 3D pet, deliberately without a framework.

The core does not depend on any concrete pet or vision plugin: pets are plugins too, capabilities are reached through the router rather than by name, and every cross-module hop has a versioned protocol.

## Runtime layers

| Layer | Lives in | Responsibility |
|---|---|---|
| Agent hosts | outside the repo | Claude Code, Codex, OpenCode, Oh My Pi (omp). They call hooks and MCP. |
| MCP entry | `src-tauri/src/mcp/mod.rs` | `opencapx mcp`, a stdio MCP server that is a thin forwarder to the core. |
| HTTP entry | `src-tauri/src/http.rs`, `src-tauri/src/core/rpc.rs` | Local listener on `127.0.0.1:47628`: `POST /event` for hooks, `POST /rpc` for MCP calls, `POST /agents/register` for identity, `GET /events` for SSE. |
| Core | `src-tauri/src/core/` | Event bus, capability registry and router, permission manager, plugin manager, storage. All authority sits here. |
| Pet UI | `src/` (TypeScript), rendered by Tauri | Overlay window and settings window. Presentation only. |
| Plugin runtime | child processes spawned by the core | Isolated plugin processes, one per plugin, JSON-RPC 2.0 over stdio. |

The MCP transport is `Agent -> spawn opencapx mcp -> HTTP -> Core`. The agent config only points at the binary with `args: ["mcp"]`; the CLI and core manage the bearer token, so the agent's own config file never holds a credential. See [docs/mcp.md](docs/mcp.md).

## Data flow

A capability call, from the agent to the result:

```text
Agent
  │  MCP: opencapx.execute { capability: "image.analyze", input: { ... } }
  ▼
opencapx mcp (thin forwarder)
  │  POST /rpc { tool, input }  +  Authorization: Bearer ocx1_...  +  X-OpenCapX-Agent
  ▼
Core HTTP entry  ──►  Agent identity check  (reject 401 on bad or revoked token)
  │
  ▼
Permission Manager  ──►  agent-layer gate; plugin-layer gate when a plugin is involved
  │
  ▼
Capability Router  ──►  look up providers by priority, try primary then fallbacks
  │
  ▼
Plugin process  ──►  JSON-RPC 2.0 over stdio (method = capability id)
  │
  ▼
Output validation  ──►  result travels back up the same path to the agent
```

While this runs, the event bus emits `capability.started` and then `capability.completed` or `capability.failed`, so the pet can show "analyzing image..." even though it has no part in the call. The full chain is specified in [docs/capability.md](docs/capability.md).

Event-stream capabilities (`type: "subscribe"`, such as `file.watch` and `screen.watch`) take a different path: the subscription is validated once, stored in memory, and events are pushed to the agent as `opencapx.event` notifications. Subscriptions do not survive the MCP connection. See [docs/capability.md](docs/capability.md) and [docs/events.md](docs/events.md).

## Core module map

The Rust core is 60+ modules under `src-tauri/src/core/`. They are grouped below by responsibility, with the role each file plays. File names are the source of truth for what each module does.

### Event bus and replay

- `event.rs`: the unified event structure, synchronous broadcast, and the ingest path everything else feeds.
- `event_replay.rs`: recording and replaying the event stream.
- `subscriber.rs`: registry of which plugins subscribe to which event kinds.
- `subscription.rs`: the subscription registry plus the built-in `file.watch` and `screen.watch` watchers.

### Agent surface and observability

- `agent.rs`: agent session model and session storage abstraction.
- `identity.rs`: `AgentIdentity`, the security principal behind every `/rpc` and `/event` call.
- `context.rs`: the `context.get_current` provider (active app, window title, clipboard excerpt).
- `project.rs`: project metadata, git branch and short path.
- `transcript.rs`: tail reading of an agent's transcript.
- `req_trace.rs`: one span tree per `/rpc` call, persisted as NDJSON.
- `plugin_trace.rs`: per-session JSON-RPC frame dumps for a plugin process.
- `log_search.rs`: grep, regex, and tail filtering over plugin logs.

### Capability registry and routing

- `capability.rs`: the Capability Registry and Router. Spec in [docs/capability.md](docs/capability.md).
- `registry.rs`: the v2 registry client: signed index types, verification chain, version selection, cache with replay protection, and offline fallback.
- `rpc.rs`: the core-side implementation of the MCP tools. Spec in [docs/mcp.md](docs/mcp.md).
- `probe.rs`: post-install self-check through a capability probe.

### Plugin runtime and lifecycle

- `plugin.rs`: Plugin Manager: manifest parsing, install, state machine, process lifecycle. Spec in [docs/plugin-manifest.md](docs/plugin-manifest.md).
- `process.rs`: plugin child processes, NDJSON JSON-RPC 2.0 over stdio. Spec in [docs/plugin-protocol.md](docs/plugin-protocol.md).
- `plugin_deps.rs`: pure functions for inter-plugin dependencies: parse, missing detection, cycle detection.
- `lifecycle_order.rs`: topological ordering of plugin startup and shutdown.
- `plugin_metrics.rs`: runtime metrics (CPU, memory, threads, file descriptors).
- `health.rs`: health checks and auto-restart strategy, overridable per plugin.
- `kill_switch.rs`: the global plugin disable switch.
- `safe_mode.rs`: `--safe-mode`, which starts the core without loading any third-party plugin.
- `sandbox.rs`: the macOS sandbox execution layer (seatbelt via `sandbox-exec`).

### Packaging, signing, and supply chain

- `pack.rs`: the pack tool that turns a plugin directory into a signed `.ocplugin`.
- `signing.rs`: the v2 canonical digest over the manifest.
- `plugin_sig.rs`: package signature creation and verification (HMAC v1 and Ed25519 v2), and the pinned official public keys.
- `verify.rs`: the automatic review gate: full validation before listing or update, shared by CI, the template release flow, and local dry runs.
- `marketplace.rs`: index fetch, sha256 checks, and reuse of the standard install path.
- `revocation.rs`: the revocation channel. A revoked key blocks install and update and disables an installed plugin by default.
- `declaration.rs`: the plugin permission-domain declaration table and its single parser. Spec in [docs/permission-domains.md](docs/permission-domains.md).

### Permissions and OS access

- `permission.rs`: the Permission Manager, which runs in Rust before any capability executes. Spec in [docs/permissions.md](docs/permissions.md).
- `osperm.rs`: OS permission preflight for `system.permission_status` on macOS, without prompting.

### Settings, storage, and workspace

- `config.rs`: per-plugin key/value JSON configuration.
- `settings.rs`: the `system.settings` provider (dark mode, wallpaper, volume).
- `profile.rs`: workspace profile switching.
- `backup.rs`: one-click workspace backup and restore.
- `storage.rs`: the SQLite store for sessions, the event audit ring, and the plugin, permission, and capability tables.
- `hotkey.rs`: global hotkeys and the command palette.
- `i18n.rs`: Rust-side strings for native UI.

### Built-in capability providers

These implement capabilities in-process, so they work with no plugin installed. A plugin registering the same capability id overrides the built-in. The list and per-provider behavior are in [docs/capability.md](docs/capability.md).

`clipboard.rs`, `browser.rs`, `vision.rs`, `speech.rs`, `audio.rs`, `media.rs`, `appctl.rs`, `inputctl.rs`, `pim.rs`, `power.rs`, `printer.rs`, `windowctl.rs`, `messages.rs`.

### Pet and user-facing surfaces

- `pet.rs`: the pet state machine, mapping `agent.*` events to `pet.state`.
- `petpack.rs`: pet pack loading.
- `tray.rs`: presentation logic for the tray menu (pure functions plus icon composition).
- `notification.rs`: the multi-channel notification sink (webhook, stderr, file, SMTP stub).
- `alerting.rs`: unifies the SLA, health, metrics, and retention alerts and pushes them to an external webhook.

### Automation and resource policy

- `automation.rs`: Event to Rule to Action.
- `sla.rs`: capability SLA monitoring and alert thresholds.
- `throttle.rs`: per-key rate limiting, including the reverse-call token bucket.
- `retention.rs`: keeps trace and log disk usage bounded.

### Outside the core

`main.rs` wires the app, installs the panic hook that writes `crash.log`, and starts the core. `http.rs` is the local HTTP transport. `mcp/mod.rs` is the stdio MCP forwarder. `hooks.rs` installs and removes OpenCapX hook entries in each agent's config, idempotently. `detector.rs` classifies short agent text as "waiting" via keyword hints. `queue.rs` is the bounded offline queue for hook events. `notify.rs` holds system notification copy. `admin.rs` serves a self-contained `GET /admin` page over SSE.

Outside `src-tauri/`: `packages/plugin-sdk/` is the Python SDK, `packages/ts-sdk/` (`@opencapx/sdk`) and `packages/create-opencapx-plugin/` are the TypeScript SDK and scaffolder, `plugins/` holds the example plugins, and `registry/` holds the signed-index registry tooling. All speak the wire protocol; none are required to run the core.

## Plugin boundary and the two SDKs

The plugin boundary is one language-agnostic wire protocol; the two SDKs are thin conveniences over it, and packaging is a shared toolchain:

```text
   one wire protocol: JSON-RPC 2.0 over stdio  (docs/plugin-protocol.md)
                              │
            ┌─────────────────┴─────────────────┐
            │                                   │
       Python SDK                          TypeScript SDK
       opencapx_sdk                        @opencapx/sdk
       @capability decorators              this.capability(...)
       same reverse calls:                 same reverse calls
       log / emit / requestPermission / configGet / configSet
            │                                   │
            └─────────────────┬─────────────────┘
                              ▼
              opencapx pack + sign (Rust CLI, Ed25519)
                              ▼
                        hello.ocplugin
```

Any process that speaks the protocol is a valid plugin — the SDKs are not required, which is also why new languages need no core changes. The scaffolder (`npm create opencapx-plugin`) generates the TypeScript shape with the published SDK; the Python path starts from `plugin-template/`.

## Key design decisions

- **Permission domains.** Third-party capability domains are declared and reviewed rather than inferred. The invariant, the declaration shape, the single parser, and the consent-before-commit install flow are in [docs/permission-domains.md](docs/permission-domains.md).
- **Plugin lifecycle and wire protocol.** Handshake, health checks, capability calls, shutdown, reverse calls, error codes, timeouts, and the `apiVersion` policy are in [docs/plugin-protocol.md](docs/plugin-protocol.md).
- **Signing chain.** The v1 HMAC and v2 Ed25519 channels, digest rules, trusted keys, and pack/verify commands are in [docs/plugin-signing.md](docs/plugin-signing.md). The trust model it implements is in [docs/supply-chain.md](docs/supply-chain.md).
- **Capability routing.** Provider priority, fallback ordering, metadata (`execution`, `typical_latency_ms`, `cost_tier`, `permissions`), and per-capability provider order are in [docs/capability.md](docs/capability.md).
- **Agent permission model.** Identity registration, the two-layer check, scopes, the hard global gate, and the audit trail are in [docs/permissions.md](docs/permissions.md).
- **Key custody.** The offline ceremony, sharding, public-key pinning, rotation, and compromise response are in [docs/key-ceremony.md](docs/key-ceremony.md).
- **Protocol stability.** Plugin API v1, the MCP v1 tool surface, and the registry index schema are frozen ahead of the app's 1.0; only additive changes ship within them, and the deprecation procedure is in [docs/deprecation.md](docs/deprecation.md).

## Storage and trust boundaries

- SQLite holds sessions, the event audit ring, and the plugin, permission, and capability tables. Subscriptions live in memory only, because they are bound to a live MCP connection.
- Signed package verification happens before install. Revocation is checked on install and update. Same-key updates proceed silently; a key change forces user re-confirmation.
- Secrets written through the `secret:` config channel go to the OS keychain, with a `0600` file fallback, and are never echoed back.
- The `dist/` frontend bundle is built from `src/` and loaded by the WebView. It has no authority of its own; every mutating action crosses into Rust and is checked there.
