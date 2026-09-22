# OpenCapX Plugin SDK (Python)

Python reference implementation of the OpenCapX plugin protocol (JSON-RPC 2.0 over stdio, see `docs/plugin-protocol.md`).

Use this SDK to write a plugin in ~30 lines instead of hand-rolling the JSON-RPC loop.

## Install

```bash
pip install -e packages/plugin-sdk
```

Or just copy `opencapx_sdk/` into your plugin directory.

## Minimal example: echo vision

```python
#!/usr/bin/env python3
from opencapx_sdk import Plugin, capability, load_manifest

class EchoVision(Plugin):
    @capability("image.analyze")
    def analyze(self, params):
        return {
            "description": f"[echo] received image: {params.get('image', '')}",
            "text": "",
            "objects": [],
        }

if __name__ == "__main__":
    EchoVision(manifest_path="opencapx-plugin.json").run()
```

## Lifecycle hooks

Override any of these on your `Plugin` subclass:

| Hook | Signature | Purpose |
|------|-----------|---------|
| `on_initialize(params) -> dict` | handshake reply | return plugin id + version |
| `on_ping(params) -> dict` | health check | default `{"ok": True}` |
| `on_shutdown(params) -> None` | cleanup | called when core says goodbye |

## Capability handlers

```python
class MyPlugin(Plugin):
    @capability("image.analyze")
    def analyze(self, params):
        return {"description": "..."}
```

The decorator stores the method name; `Plugin.handle()` dispatches incoming JSON-RPC calls to the right handler.

## Reverse calls (plugin → core)

Available on every `Plugin` instance:

```python
self.log("warn", "API retried 2/3")           # core.log notification
self.emit("latency", {"ms": 1820})             # core.emit notification
self.request_permission("image.read", reason="analyzing screenshot")  # blocks, returns bool
self.config_get("apiKey", default="")          # returns value or default
self.config_set("apiKey", "sk-...")            # returns bool
```

## Manifest validation

```python
from opencapx_sdk import load_manifest, ManifestError

try:
    m = load_manifest("opencapx-plugin.json")
except ManifestError as e:
    sys.exit(f"bad manifest: {e}")
```

Validation rules (from `docs/plugin-manifest.md`):
- Required: `id, name, version, apiVersion, type`
- `type` must be one of `pet | capability`
- `apiVersion` must equal `"1"` for this SDK version
- `capability` type requires `capabilities[]` and `runtime`

## Testing your plugin

You can mock stdin/stdout without spawning a subprocess:

```python
from opencapx_sdk import Plugin

class TestPlugin(Plugin):
    def __init__(self):
        super().__init__(manifest={"id": "test", "version": "0", "apiVersion": "1", "type": "capability", "capabilities": []})

    @capability("echo")
    def echo(self, params):
        return params

import io
p = TestPlugin()
out = io.StringIO()
p._send = lambda msg: out.write(json.dumps(msg) + "\n")
resp = p.handle({"jsonrpc": "2.0", "id": 1, "method": "echo", "params": {"hi": 1}})
assert resp["result"] == {"hi": 1}
```

## Layout

```
packages/plugin-sdk/
├── opencapx_sdk/
│   ├── __init__.py      public API
│   ├── protocol.py      JSON-RPC framing helpers
│   ├── manifest.py      load + validate opencapx-plugin.json
│   └── plugin.py        Plugin base class + @capability decorator
├── tests/
│   ├── test_protocol.py
│   └── test_plugin.py
├── pyproject.toml
└── README.md
```