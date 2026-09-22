# Roadmap

What stands between `0.1.0` and a production-grade `1.0`, in rough order.

Packaging — deferred until there is a commercial reason to buy signing certificates:

- [ ] Apple code signing and notarization (needs a paid Apple Developer membership; not planned while the project is free and open source)
- [ ] Windows code signing (same cost/benefit position)

Before the plugin ecosystem scales:

- [ ] Extend OS-permission preflight beyond read-only macOS probes (Windows/Linux status, first-use guidance to System Settings)
- [ ] Enforce network.request domain scopes (the structure is reserved in the permission model; the matcher is not wired for domains yet)
- [ ] Rotate the registry official key to the offline ceremony key ([key-ceremony.md](docs/key-ceremony.md)) — the add-then-retire window contract is pinned by a test; execution waits until external publishers actually exist
- [x] Put a copy of the updater private key into encrypted offline storage now (password-manager attachment or a second machine)
- [ ] Shard the updater key across offline custodians ([split-seed.mjs](scripts/split-seed.mjs)) once the install base justifies it

`v1.0` — protocol stability:

- [ ] Have outside TypeScript authors verify the "first plugin in 30 minutes" bar and feed the friction back into the SDKs and docs
- [ ] A field window on `0.1.x`: plugin compatibility across updates, the update chain, and crash reports

Deliberately not planned yet: plugin marketplace, cloud sync, agent marketplace, and telemetry. They wait until the protocol is stable enough to build on.
