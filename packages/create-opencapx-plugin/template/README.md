# __NAME__

OpenCapX plugin written in TypeScript with [`@opencapx/sdk`](../../packages/ts-sdk/).

## Develop

```bash
npm install
npm test     # tsc + node --test
```

## Build + install into OpenCapX

```bash
npm run build  # emits dist/src/plugin.js referenced by opencapx-plugin.json
```

Then install the plugin directory from the OpenCapX settings → Plugins page
(or pack it — packing/signing still go through the Python SDK / Rust CLI,
see `docs/plugin-signing.md`).

## Layout

```
├── opencapx-plugin.json     # id, capabilities, permissions, settings, runtime (node dist/src/plugin.js)
├── src/plugin.ts            # capability handlers
└── test/plugin.test.ts      # dispatch smoke tests (no subprocess needed)
```
