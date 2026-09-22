# Plugin Protocol Specification

> **🔒 v1 frozen (2026-09-20, as of Core 0.1.0)**: this protocol surface (apiVersion `"1"`) has been frozen per
> [deprecation.md](deprecation.md) — within v1 only additive changes are allowed; breaking changes may appear only in
> the migration window of apiVersion `"2"`. Third-party plugins can depend on it with confidence.
The communication protocol between Core and plugin processes: **JSON-RPC 2.0 over stdio, line-delimited (NDJSON)**.

- One complete JSON object per line, terminated by `\n`, UTF-8, no BOM
- stdout carries only protocol; stderr carries only logs (Core collects them into `logs/plugins/<id>.log`)
- Bidirectional: Core → plugin (invoking capabilities, lifecycle management), plugin → Core (reverse requests for permissions, logging, emitting events)

Any language (Rust / Go / Python / Node / TS) that can read and write stdin/stdout can write a plugin.

## Request ID Rules (avoiding bidirectional conflicts)

- Requests from Core: numeric IDs, incrementing from 1
- Requests from the plugin: string IDs, e.g. `p1`, `p2` incrementing

## Lifecycle

### 1. Startup handshake

Right after Core spawns the plugin it sends:

```json
{"jsonrpc":"2.0","id":1,"method":"plugin.initialize","params":{"coreVersion":"0.2.0","apiVersion":"1","pluginId":"com.opencapx.vision"}}
```

The plugin must respond (10s timeout; a timeout is treated as a startup failure):

```json
{"jsonrpc":"2.0","id":1,"result":{"pluginId":"com.opencapx.vision","version":"1.0.0","capabilities":[{"id":"image.analyze","version":"1"}]}}
```

The returned `capabilities` must match the Manifest; Core registers the handshake result into the Capability Registry.

**Upgrade handshake injection (M7/F9)**: if the plugin has started successfully before, initialize params will contain an extra `previousVersion`
field (e.g. `{"coreVersion":"0.1.0","apiVersion":"1","pluginId":"com.x","previousVersion":"0.1.0"}`).
Semantic contract:

- **The host does not move data**: the plugin's store (`core.store.*`) and config are left as is; the plugin itself handles cross-version read compatibility.
- **The plugin migrates itself, and it must be idempotent**: migrate inside `on_initialize` based on data shape (not merely on the version number),
  so that re-running/rollback/downgrade are all safe. See `plugins/weather-demo` (store v1→v2).
- `previousVersion` is **absent** on first install (no field); only after a successful handshake does Core record this version as last_version,
  so an upgrade whose handshake fails does not advance the recorded version.

### 2. Health check

Core may send at any time:

```json
{"jsonrpc":"2.0","id":2,"method":"plugin.ping"}
```

```json
{"jsonrpc":"2.0","id":2,"result":{"ok":true}}
```

ping defaults to a 5s timeout. Three consecutive failures → the plugin enters the `error` state.

### 3. Capability invocation

The method is the capability ID:

```json
{"jsonrpc":"2.0","id":1001,"method":"image.analyze","params":{"image":"/tmp/monitor.png"}}
```

```json
{"jsonrpc":"2.0","id":1001,"result":{"description":"Server monitoring UI screenshot","text":"CPU 92%","objects":[]}}
```

For params/result structure see [capability.md](capability.md).

### 4. Shutdown

Core sends a notification (no id), then waits 3s and kills if the process has not exited:

```json
{"jsonrpc":"2.0","method":"plugin.shutdown"}
```

The plugin should finish its cleanup before exiting. Incomplete cleanup after being killed is the plugin's own responsibility; Core does not replay.

## Plugin → Core Reverse Calls

### core.requestPermission (request)

When runtime needs a permission it was not pre-authorized for:

```json
{"jsonrpc":"2.0","id":"p1","method":"core.requestPermission","params":{"permission":"network.request","reason":"upload image to the vision model API"}}
```

```json
{"jsonrpc":"2.0","id":"p1","result":{"granted":true}}
```

Core shows a permission window (Allow once / Always / Deny) and blocks until the user decides; after 60s with no action it is treated as Deny and written to audit. See [permissions.md](permissions.md).

### core.log (notification)

```json
{"jsonrpc":"2.0","method":"core.log","params":{"level":"warn","message":"API retry 2/3"}}
```

### core.emit (notification)

Events the plugin produces go to the Event Bus; `type` must be prefixed with the plugin ID:

```json
{"jsonrpc":"2.0","method":"core.emit","params":{"type":"com.opencapx.vision.model_latency","payload":{"ms":1820}}}
```

### Reverse Limits (S3)

- **Rate limiting**: `core.emit` + `core.log` share a per-plugin token bucket (default 50 events/sec, burst 100;
  override with `reverse_rate_per_sec` / `reverse_burst`). Over-limit events are dropped; when the window closes
  (≤1 per second, to prevent feedback loops) a `plugin.throttled` summary event is emitted (`dropped` / `windowSecs`).
- **requestPermission deduplication**: duplicate **in-flight** requests for the same permission from the same plugin share a single decision;
  when the decision completes it replies to all waiters together — spamming N times only pops one window.
- **Subscription limit**: each plugin may subscribe to at most 32 kinds (override with `subscribe_max_kinds`);
  when exceeded, `plugin.subscribe` returns `{"ok": false, "error": "subscribe limit reached ..."}`.

### config.get / config.set / config.delete (config read/write)

The plugin reads and writes its own config namespace (both keys and values are JSON):

```json
{"jsonrpc":"2.0","id":7,"method":"config.get","params":{"key":"apiKey","default":""}}
{"jsonrpc":"2.0","id":8,"method":"config.set","params":{"key":"apiKey","value":"sk-..."}}
{"jsonrpc":"2.0","id":9,"method":"config.delete","params":{"key":"apiKey"}}
```

**Secret channel (M7/F8)**: when the key name starts with `secret:` (passing `"secret:api_key"` as `config.set`'s key),
the value is not written to the JSON file but goes into the OS keychain (falling back to a 0600-permission file when unavailable); the `config.set` receipt is always the mask
`"********"` (write-only, never read back), and only the plugin itself can read the plaintext back via `config.get`; when the plugin is uninstalled, its secret fallback file is cleaned up with it.
The `secret` control in the Manifest's declarative settings reads and writes exactly `secret:<key>`.

## Declarative Settings Actions (Core → plugin: `settings.<key>`)

A settings control with `type: "button"` in the Manifest stores no value; when the user clicks it on the Settings page, Core calls the plugin's reserved method
`settings.<key>` (params is an empty object), and the plugin should quickly return a result or a business error:

```json
{"jsonrpc":"2.0","id":20,"method":"settings.run_now","params":{}}
```

A settings control with `type: "list"` reuses the same method name: the data is held by the plugin process and the host only forwards the action (for control semantics see
[plugin-authoring.md](plugin-authoring.md) §5.10). `params` is `{"op": ...}` plus the fields that op needs,
and there are four ops — `list` (`op` only), `add` (with `value`), `delete` (with `index`),
`move` (with `index` and `to`, where `to` is the index **after the pop**):

```json
{"jsonrpc":"2.0","id":21,"method":"settings.tags","params":{"op":"add","value":"alpha"}}
{"jsonrpc":"2.0","id":22,"method":"settings.tags","params":{"op":"move","index":2,"to":0}}
```

Every op must return the current array (the host redraws from the return value and keeps no copy of its own); an unknown op is an error — the plugin side reports
a JSON-RPC error and must not silently succeed as a no-op. Both controls share the same timeout surface: 10 seconds, and Core lazily starts the plugin if needed.

Implementation: `@method("settings.run_now")` (the SDK method decorator, the same channel as `core.probe.*`).

## Error Codes

| code | Meaning |
|---|---|
| -32700 | Parse error |
| -32600 | Invalid Request |
| -32601 | Method not found (the capability is not implemented) |
| -32602 | Invalid params (does not conform to inputSchema) |
| -32603 | Internal error |
| 40001 | Permission denied (Core check failed) |
| 40002 | Capability execution failed (plugin business error; `data` carries details) |
| 40003 | Timeout |
| 40004 | apiVersion incompatible |

## Version Policy

- `apiVersion` must **exactly match** the `"1"` currently supported by Core; it is raised to `"2"` only when the protocol shape changes in a breaking way.
- When raising to `"2"`, a **dual-support window of ≥1 minor version** is mandatory: Core accepts both `"1"` and `"2"` for a period so plugins can migrate gradually; abruptly dropping support for the old version is forbidden.
- `minCoreVersion` is a **semantic version floor** (the plugin's minimum requirement on Core behavior), complementary to `apiVersion`: one governs protocol shape, the other governs the behavior baseline.

## Timeouts and Restarts

- Capability invocation defaults to a 60s timeout; the Manifest can override it per capability (object form `timeoutSecs`, in seconds, 1..=600;
  out-of-range values are rejected at install). In the multi-provider fallback loop, each provider uses its **own declared** timeout.
- **Input gate (S4)**: if the serialized invocation params of a plugin-path call exceed **1 MiB** it is rejected outright,
  `capability.failed` carries `payload_too_large`, and the payload never reaches the plugin's stdin
  (in-process built-in capabilities do not go through stdin and are not subject to this limit).
- Plugin process crash: Core restarts with exponential backoff (1s / 2s / 4s; the initial backoff is tunable via `plugin_health_config.backoff_initial_ms`, range 0–30000ms; the 30s backoff ceiling is fixed and not tunable), gives up after 3 attempts, and sets the state to `error`
- Timed-out requests are not resent — the caller (the MCP layer) decides whether to retry

## Minimal Plugin Reference (Python)

> The official SDK is recommended; see `packages/plugin-sdk/` (README + pytest).
> The hand-written version is only a reference; for production use the `@capability` decorator + `Plugin.run()`.

```python
import json, sys

def send(msg): print(json.dumps(msg), flush=True)

for line in sys.stdin:
    req = json.loads(line)
    if req.get("method") == "plugin.initialize":
        send({"jsonrpc":"2.0","id":req["id"],"result":{
            "pluginId":"com.example.echo","version":"0.1.0",
            "capabilities":[{"id":"image.analyze","version":"1"}]}})
    elif req.get("method") == "plugin.ping":
        send({"jsonrpc":"2.0","id":req["id"],"result":{"ok":True}})
    elif req.get("method") == "image.analyze":
        send({"jsonrpc":"2.0","id":req["id"],"result":{
            "description":"echo: "+req["params"].get("image",""),
            "text":"","objects":[]}})
    elif "id" in req:
        send({"jsonrpc":"2.0","id":req["id"],"error":{"code":-32601,"message":"method not found"}})
```

## Pet Plugins: Background Heartbeat Mode

Pet plugins often need to emit periodically (animation, heartbeat). `Plugin.run()` blocks on stdin,
so background logic should live in a separate thread, with daemon=True so shutdown exits cleanly:

```python
import sys, threading, time
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[3] / "packages" / "plugin-sdk"))
from opencapx_sdk import Plugin

class PetBlank(Plugin):
    def on_initialize(self, params):
        self.log("info", "pet-blank started")
        self.emit("animation", {"name": "idle", "frame": 0})
        threading.Thread(target=self._heartbeat, daemon=True).start()
        return super().on_initialize(params)

    def _heartbeat(self):
        frame = 0
        while True:
            time.sleep(5)
            frame += 1
            self.emit("animation", {"name": "idle", "frame": frame})
```
