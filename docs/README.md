# OpenCapX Protocol Documentation

OpenCapX is the desktop body and capability layer for agents: any agent (Claude Code / Codex / OpenCode, …) gains desktop status display, interaction capabilities, and abilities it does not have itself (vision, voice, etc.).

The desktop pet is the most visible UI of this system, but it is not the core. The core is **Core**: event bus + capability routing + plugin runtime + permission management, all on the Rust side.

## Documentation Index

| Document | Contents |
|---|---|
| [../README.md](../README.md) | English overview (for external developers): positioning, the three pillars, command rules, install, first plugin, connecting an agent |
| [../INSTALL.md](../INSTALL.md) | Install: downloading artifacts, building from source, prerequisites |
| [../ROADMAP.md](../ROADMAP.md) | Roadmap: 0.1.0 → production-ready 1.0 |
| [plugin-manifest.md](plugin-manifest.md) | Plugin manifest `opencapx-plugin.json` spec, package format, directory layout |
| [plugin-authoring.md](plugin-authoring.md) | Plugin authoring guide: from an empty directory to an installable, upgradable plugin with a `settings[]` graphical form |
| [plugin-signing.md](plugin-signing.md) | Plugin signing and distribution (author guide): keygen/pack/verify, exit codes, trusted-keys, v1/v2 comparison |
| [plugin-protocol.md](plugin-protocol.md) | Communication protocol between plugins and Core: JSON-RPC 2.0 over stdio |
| [capability.md](capability.md) | Capability naming, Schema, registration and routing |
| [permissions.md](permissions.md) | Permission model, scopes, runtime checks, audit |
| [events.md](events.md) | Event bus: event catalog, sources, dispatch |
| [automation.md](automation.md) | Automation (§15): rule files, match semantics, actions, CLI |
| [rules.md](rules.md) | Command rules: command interception and rewriting (`curl` → `sandbox curl`), layering and trust, audit and CLI |
| [mcp.md](mcp.md) | MCP gateway: the 6 tools agents use to call OpenCapX |
| [supply-chain.md](supply-chain.md) | Supply chain and publishing plan (decision document): trust model, publisher registration, review policy, install UX |
| [release.md](release.md) | Release process: pre-release gate, key and secrets checklist, tagging, build verification, npm publish, emergencies |
| [user-guide.md](user-guide.md) | User guide: plugin installation (three states), permission confirmation, settings (secret masking), the opencapx command, updates, revocation handling, uninstall |
| [plugin-review.md](plugin-review.md) | Plugin review handbook: 5 categories of automated gate checks, manual channel and SLA, observation period, incident process |
| [permission-domains.md](permission-domains.md) | Plugin permission domain design proposal (proposal, pending review): third-party domain declaration, mapping freeze, phased domain ownership |
| [i18n.md](i18n.md) | Internationalization: three-layer copy ownership, 3-step process for adding a language, validation rules |
| [pets.md](pets.md) | Pet package format: 2D sprite sheets / 3D glTF, state mapping, decoders, power-saving design |
| [bubble.md](bubble.md) | Bubbles: data sources, three modes, ten themes, four-way positioning and window geometry |
| [tray.md](tray.md) | Tray menu: project-grouped submenus, group header copy, ordering and throttled rebuild |
| [testing.md](testing.md) | Manual testing: three entry points for creating data, payload quick reference, observation points, pitfalls |

## Architecture Overview

```text
                     Claude / Codex / OpenCode / …
                              │  hooks (existing) + MCP
                              ▼
┌─────────────────────────────────────────────────────────┐
│                      OpenCapX Core                      │
│  Event Bus ─ Agent Manager ─ Capability Router          │
│  Permission Manager ─ Plugin Manager ─ Process Manager  │
└───────────┬──────────────────────────┬──────────────────┘
            ▼                          ▼
       Pet UI (overlay)             Plugins (separate processes)
       animations/bubbles/interactions   pet / capability (JSON-RPC stdio)
```

## Three Iron Rules

1. **Tauri is the host, not a plugin framework.** OpenCapX Plugin ≠ Tauri Plugin. Plugins are separate processes that speak their own JSON-RPC protocol.
2. **Agents call a Capability, not a specific plugin and not a specific model.** `Agent → image.analyze → Router → some Vision plugin`.
3. **Every dangerous operation must pass through the Rust Permission Manager.** The WebView is never a security boundary; the Rust Core is.

## Naming Notes

MCP tool `opencapx.say`, manifest file `opencapx-plugin.json`, official plugin ID `com.opencapx.*`.

## Versions

- Protocol version (apiVersion): `"1"`
- Status: Phase 0 draft, subject to change before implementation; after implementation it evolves by major version.
