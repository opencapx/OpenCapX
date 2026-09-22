# create-opencapx-plugin

Scaffold a TypeScript OpenCapX plugin.

After the npm package is published:

```bash
npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
```

Until then, run the same initializer straight from the repo:

```bash
node packages/create-opencapx-plugin/bin/create.mjs --id com.acme.hello --name "Hello"
```

Options: `--id` (required, reverse-DNS), `--name`, `--author`, `--license`
(default `MIT`), `--dir` (default `./<last id segment>`), `--local`.
Refuses to write into a non-empty directory.

By default the scaffolded `package.json` depends on the published
`"@opencapx/sdk": "^0.1.0"` from the registry. Pass `--local` when working
inside an OpenCapX checkout to depend on this repo's `ts-sdk` via a `file:`
path instead.

What you get (see `template/`):

```
hello/
├── opencapx-plugin.json     # id, capabilities, permissions, settings, node runtime
├── package.json             # depends on @opencapx/sdk (^0.1.0 by default, file: with --local)
├── tsconfig.json            # strict; build emits dist/src/plugin.js
├── src/plugin.ts            # capability handlers (edit me)
├── test/plugin.test.ts      # dispatch smoke tests, run with node --test
└── README.md
```

Then:

```bash
cd hello && npm install && npm test && npm run build
```

Packing and signing still go through the Python SDK / Rust CLI
(see `docs/plugin-signing.md`).
