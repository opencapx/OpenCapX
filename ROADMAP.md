# Roadmap

What stands between `0.1.0` and a production-grade `1.0`, in rough order.

Packaging — deferred until there is a commercial reason to buy signing certificates:

- [ ] Apple code signing and notarization (needs a paid Apple Developer membership; not planned while the project is free and open source)
- [ ] Windows code signing (same cost/benefit position)

Before the plugin ecosystem scales:

- [x] A real Windows sandbox backend for `opencapx sandbox` — landed: AppContainer, the same kernel primitive Edge/Chrome use (a lowbox profile whose SID is granted per-run by ACL and revoked when the run ends; no admin rights, no virtualization). `microVM` is no longer the Windows answer — if the AppContainer tier ever proves too weak, a microVM tier via WHP is the optional stronger stance, on demand rather than by default
- [x] Give the Windows backend a read-only grant for the paths strict-mode commands legitimately need to read — landed: strict mode grants `FILE_GENERIC_READ | FILE_TRAVERSE` on `$HOME` for the run and revokes it with the rest, so all three backends now answer the calibration the same way ("reads are globally allowed, write and network are the boundary"). `FILE_TRAVERSE` is load-bearing (a lowbox has no `SeChangeNotifyPrivilege` to bypass the check), the grant is best-effort so a failed ACL write cannot fail the run open, and the residual — everything outside `$HOME` stays unreadable, narrower than seatbelt's global read — is documented in `docs/rules.md`

- [x] Extend OS-permission preflight beyond read-only macOS probes (Windows/Linux status, first-use guidance to System Settings) — landed: the probe now runs on all three platforms with a fourth area value `unavailable` (the built-in cannot work here and no user grant would fix it), Linux guidance carries no `settingsUrl` (there is no settings pane to deep-link to), and the Wayland desktop-portal capture path stays a separate feature
- [x] Enforce network.request domain scopes (the matcher shipped in v1.5; the scope write sources — manifest declaration + the agent editor — landed after)
- [ ] Rotate the registry official key to the offline ceremony key ([key-ceremony.md](docs/key-ceremony.md)) — the add-then-retire window contract is pinned by a test; execution waits until external publishers actually exist
- [x] Put a copy of the updater private key into encrypted offline storage now (password-manager attachment or a second machine)
- [ ] Shard the updater key across offline custodians ([split-seed.mjs](scripts/split-seed.mjs)) once the install base justifies it

`v1.0` — protocol stability:

- [ ] Have outside TypeScript authors verify the "first plugin in 30 minutes" bar and feed the friction back into the SDKs and docs
- [ ] A field window on `0.1.x`: plugin compatibility across updates, the update chain, and crash reports

Deliberately not planned yet: plugin marketplace, cloud sync, agent marketplace, and telemetry. They wait until the protocol is stable enough to build on.
