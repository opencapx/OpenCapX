# Echo Vision (TypeScript)

A TypeScript reference implementation of `image.analyze`: it echoes the input, and the response
prefix goes through one real reverse `config.get` (returns the default when the key is unset, verifying the reverse path).
It is also the node fixture for the cargo e2e test (`install_grant_execute_ts_e2e`).

```bash
# Drive it directly (simulating Core):
echo '{"jsonrpc":"2.0","id":1,"method":"plugin.initialize","params":{"coreVersion":"0.1.0","apiVersion":"1","pluginId":"com.opencapx.echo-vision-ts"}}' | node bin/plugin.cjs
```

To write a TS plugin for real, use the scaffold (`packages/create-opencapx-plugin/`), with the SDK dependency being the
regular `@opencapx/sdk`; for offline installability, this fixture requires the repo's
`packages/ts-sdk/dist` directly.
