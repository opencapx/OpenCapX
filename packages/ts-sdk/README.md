# OpenCapX Plugin SDK (TypeScript)

TypeScript implementation of the OpenCapX plugin protocol (JSON-RPC 2.0 over stdio, see `docs/plugin-protocol.md`).

Use this SDK to write a plugin in ~30 lines instead of hand-rolling the JSON-RPC loop. Zero runtime dependencies (Node ≥ 18; tests use the built-in `node:test` runner).

> Python authors: this mirrors `packages/plugin-sdk/` (`opencapx_sdk`).
> Two deliberate differences: reverse calls are `async` (await them), and
> handler registration is explicit (`this.capability(...)`) instead of
> decorators, so no `experimentalDecorators` flag is needed. Packing and
> signing still go through the Python SDK / Rust CLI; see
> [docs/plugin-authoring.md §0.5 "Packing and signing (TS authors)"](../../docs/plugin-authoring.md#05-the-typescript-path-write-a-plugin-in-ts)
> for the two commands and install requirements.

## Install

From the npm registry (once published):

```bash
npm install @opencapx/sdk
```

Before first publish — or to track a local checkout — install from the repo path:

```bash
npm install /path/to/OpenCapX/packages/ts-sdk
```

Both resolve to the same package; scaffolds created with
`create-opencapx-plugin` use the repo path by default and the registry range
with `--published` (see `packages/create-opencapx-plugin/`).

## Minimal example: echo vision

```ts
import { Plugin } from "@opencapx/sdk";

class EchoVision extends Plugin {
  constructor() {
    super({ manifestPath: "opencapx-plugin.json" });
    this.capability("image.analyze", async (params) => ({
      description: `[echo] received image: ${String(params["image"] ?? "")}`,
      text: "",
      objects: [],
    }));
  }
}

await new EchoVision().run();
```

## Lifecycle hooks

Override any of these on your `Plugin` subclass:

| Hook | Signature | Purpose |
|------|-----------|---------|
| `onInitialize(params)` | handshake reply | returns plugin id + version + capabilities |
| `onPing(params)` | health check | default `{ ok: true }` |
| `onShutdown(params)` | cleanup | called when core says goodbye |
| `onPluginCall(method, params)` | fallback | throws `MethodNotFoundError` (`-32601`) by default |

## Capability handlers

```ts
class MyPlugin extends Plugin {
  constructor() {
    super({ manifestPath: "opencapx-plugin.json" });
    this.capability("image.analyze", async (params) => ({
      description: "...",
    }));
    this.method("core.probe.capability", async () => ({ ok: true }));
  }
}
```

## Reverse calls (plugin → core)

All `async` — await them:

```ts
await this.log("warn", "API retried 2/3");              // core.log notification
await this.emit("latency", { ms: 1820 });               // core.emit notification
await this.requestPermission("image.read", "analyzing screenshot"); // boolean
await this.configGet("apiKey", "");                     // value or default
await this.configSet("apiKey", "sk-...");               // boolean
```

Timeouts resolve with the safe default instead of hanging (`requestPermission` defaults to 65s, config ops to 30s).

## Manifest validation

```ts
import { loadManifest, ManifestError } from "@opencapx/sdk";

try {
  const m = loadManifest("opencapx-plugin.json");
} catch (e) {
  if (e instanceof ManifestError) process.exit(`bad manifest: ${e.message}`);
}
```

Same rules as the Python SDK (from `docs/plugin-manifest.md`):
- Required: `id, name, version, apiVersion, type`
- `type` must be one of `pet | capability`
- `apiVersion` must equal `"1"` for this SDK version
- `capability` type requires `capabilities[]` and `runtime`

## Testing your plugin

`handle()` takes a single message — no subprocess needed.
Pass in-memory streams to the constructor for reverse-call tests:

```ts
import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { Plugin } from "@opencapx/sdk";

const input = new PassThrough();
const output = new PassThrough();
const plugin = new MyPlugin({ manifest: {...}, input, output });
const running = plugin.run(); // pumps input while you await reverse calls
// ... write a core reply into `input`, await your call ...
input.end();
assert.equal(await running, 0);
```

Run this package's own suite with `npm test` (`tsc` + `node --test`).

## Layout

```
packages/ts-sdk/
├── src/
│   ├── index.ts         public API
│   ├── protocol.ts      JSON-RPC framing helpers
│   ├── manifest.ts      load + validate opencapx-plugin.json
│   └── plugin.ts        Plugin base class + registration
├── test/
│   ├── protocol.test.ts
│   ├── manifest.test.ts
│   └── plugin.test.ts
├── package.json
└── tsconfig.json
```
