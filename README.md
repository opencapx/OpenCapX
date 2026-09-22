<p align="center">
 <img src="public/pet-logo.png?raw=true" alt="OpenCapX" height="100px"/>
<h1 align="center">OpenCapX</h1>
<div align="center">
 <strong>
    Intercepts your AI agents' requests, keeps every system permission safe and controllable, and hands any model the multimodal abilities it lacks — taking your agents further, with capabilities you define through plugins and rules.
 </strong>
</div>
<br/>
<p align="center">
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="macOS" src="https://img.shields.io/badge/-macOS-black?style=for-the-badge&logo=apple&logoColor=white" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Windows" src="https://img.shields.io/badge/Windows-0078D6?style=for-the-badge&logo=windows&logoColor=green" />
</a>
<a href="https://github.com/opencapx/OpenCapX/releases/latest" target="_blank">
<img alt="Linux" src="https://img.shields.io/badge/Linux-FCC624?style=for-the-badge&logo=linux&logoColor=black" />
</a>
</p>

<p align="center">
    English | <a href="./README-CN.md">简体中文</a> | <a href="./README-VI.md">Tiếng Việt</a>
</p>

Claude Code, Codex, and OpenCode already know how to code. They cannot see your screen, speak out loud, watch a folder, or show a status on your desktop. OpenCapX is the desktop body and capability layer that fills that gap — and the gate that stands in front of it. Built-in multimodal providers give any model sight, speech, clipboard, browser and media; everything else arrives as signed, sandboxed plugins. Agents reach it over MCP, and every request they make is intercepted and decided in the Rust core before anything runs — capabilities, plugins and shell commands alike, Command rules let you rewrite what an agent executes.

<p align="center">
  <img src="public/diagram.jpeg?raw=true" alt="OpenCapX architecture: agent CLIs (Claude Code, Codex, OpenCode, Gemini CLI, Cursor, Copilot CLI, Factory Droid, Oh My Pi) reach OpenCapX through the MCP gateway and the PreToolUse hook; the Rust core routes capabilities to built-in providers and signed plugins" width="100%" />
</p>

## Features

**Pet and UI**
- **2D and 3D pets** — animated sprite sheets or glTF/VRM models, with working / waiting for you / finished / idle states.
- **Status bubbles** — a themed bubble shows what each agent is doing, and turns into choice buttons when an agent asks you something.
- **Tray menu** — a status dot, per-project session grouping, and a tooltip naming the agent that needs you.

**Agent surface**
- **MCP gateway** — six tools (`say`, `notify`, `set_state`, `ask`, `list_capabilities`, `execute`) plus event subscriptions, so any MCP host can drive the desktop.
- **One-command connect** — `opencapx connect claude|codex|opencode|omp` wires the agent's hooks and MCP entry, idempotently. Entries point at a stable CLI copy (`~/.opencapx/bin/opencapx`) that survives rebuilds and reinstalls.
- **Session-start capability brief** — supported agents receive OpenCapX's capability list in their context at session start, before they have to ask.
- **Notifications** — OS toasts when an agent finishes or waits for input, collected in a Notification Center.

**Plugins**
- **Isolated processes** — plugins are ordinary processes speaking JSON-RPC 2.0 over stdio, never Tauri plugins.
- **Signed and reviewed** — Ed25519 for distribution, HMAC for local/team use; third-party capability domains are declared and reviewed before they are trusted.
- **Declarative settings** — a manifest `settings[]` block renders a graphical form, with secrets kept in the OS keychain.

**Governance**
- **Two-layer permissions** — every dangerous operation passes through the Rust Permission Manager before any plugin sees it.
- **Command rules** — a `PreToolUse` hook intercepts the agent's shell commands and rewrites them to an executor you choose.
- **Audit and control** — grants, denials and rule hits land in an Activity Timeline, and a kill switch stops every plugin at once.

**Capabilities**
- **Built-in providers** — vision, speech, clipboard, browser reading, media, and macOS PIM (photos, contacts, calendar, location, notes, reminders, mail).

**Automation and alerting**
- **Event → Rule → Action** — turn agent activity into desktop actions with rule files.
- **Webhooks** — Slack, Discord or a custom endpoint, with severity routing, deduplication, retries and a dead-letter queue.

**Operations**
- **Workspace profiles** — keep separate plugin and permission sets and switch between them.
- **Backup and restore** — snapshot workspace state to a file and restore it in one click.
- **Hotkeys and command palette** — global shortcuts plus a searchable palette.

**Distribution**
- **Marketplace and registry** — install from a signed index, with publisher revocation and stable / beta / dev channels.

**Platform**
- **Cross-platform** — macOS, Windows and Linux builds.
- **Three UI languages** — English, Simplified Chinese and Vietnamese.

## Three pillars

### Plugin System

Plugins are ordinary processes, not Tauri plugins. Each one speaks JSON-RPC 2.0 over stdio, declares the capabilities and permissions it needs in `opencapx-plugin.json`, and stays isolated from the core. Packages are signed (Ed25519 for distribution, HMAC for local/team use), and third-party capability domains are declared and reviewed before they are trusted.

- Author guide: [docs/plugin-authoring.md](docs/plugin-authoring.md)
- Protocol: [docs/plugin-protocol.md](docs/plugin-protocol.md)
- Manifest spec: [docs/plugin-manifest.md](docs/plugin-manifest.md)
- Signing and distribution: [docs/plugin-signing.md](docs/plugin-signing.md)
- Permission domains: [docs/permission-domains.md](docs/permission-domains.md)

### MCP Gateway

Agents spawn `opencapx mcp` as a stdio MCP server. That process forwards every tool call to the local core over HTTP. The v1 surface is six tools: `opencapx.say`, `opencapx.notify`, `opencapx.set_state`, `opencapx.ask`, `opencapx.list_capabilities`, and `opencapx.execute`. New abilities do not add tools: `opencapx.execute` reaches any registered capability through the router. `opencapx.subscribe` / `opencapx.unsubscribe` cover event-stream capabilities.

- Spec: [docs/mcp.md](docs/mcp.md)

### Permission System

Every dangerous operation passes through the Rust Permission Manager before any plugin sees it. A WebView is never a security boundary; the core is. Agents identify themselves through a TOFU registration flow, plugins declare permissions up front, and the two layers are checked separately. Denials and grants land in an audit trail.

- Model and scopes: [docs/permissions.md](docs/permissions.md)

## Command Rules

OpenCapX also sits in front of the agent's own shell commands. A `PreToolUse` hook intercepts each Bash tool call before it runs and can **rewrite** it into the form of an executor you choose: `curl https://x` becomes `sandbox curl https://x`. Claude Code, Codex, Gemini, Factory Droid, Cursor, Copilot CLI and Oh My Pi (omp) are rewritten today; opencode rewrites through the plugin `connect` installs (audit-only on stdout); the remaining hosts pass through as-is.

**OpenCapX only routes; it does not adjudicate.** The command goes to the executor you configured, and the boundary (sandbox, proxy, container) is that executor's responsibility. This is a separate pipeline from the Permission System above: permissions gate `/rpc` capability calls, while command rules govern the commands the agent issues itself.

Rules are layered — built-in (empty by default), global (`~/.opencapx/rules.json`), and project-level (`<project>/.opencapx/rules.json`). Project-level rules are an injection surface, so they stay **ignored until you explicitly trust the project** (`opencapx rules trust`). A rewrite emits a `rule.applied` audit event into the Activity Timeline, and a missing or corrupt rule file fails open: the command runs as-is and the agent is never blocked.

- Spec: [docs/rules.md](docs/rules.md)
- CLI: `opencapx rewrite`, plus `opencapx rules list | explain | trust | untrust`
- Settings page: the **Command Rules** tab adds rules, toggles global rules, and manages trusted projects.

## For User

### Install

Download the latest build from the [releases page](https://github.com/opencapx/OpenCapX/releases/latest): a `.dmg` for macOS (Apple Silicon), an `x64-setup.exe` or `.msi` for Windows, and `.deb` / `.rpm` / `.AppImage` for Linux. The macOS builds are not code-signed or notarized; if Gatekeeper blocks the first launch, right-click the app and choose Open.

Build from source, the prerequisites, and the live development window are in [INSTALL.md](INSTALL.md).

### Connect your agent

One command wires the agent's hooks and MCP server entry (idempotent, no credentials land in the agent's config — the token flow is handled between `opencapx mcp` and the core at startup):

```bash
opencapx connect claude   # or: codex | opencode | omp
```

Restart the agent, then ask it to call `opencapx.list_capabilities` to verify. The manual per-host config shapes are in [docs/mcp.md](docs/mcp.md).

### Your first plugin in five minutes

Scaffold a TypeScript plugin from the published initializer:

```bash
npm create opencapx-plugin -- --id com.acme.hello --name "Hello"
```

Then build and test it:

```bash
cd hello && npm install && npm test && npm run build
```

The result is a plugin with a manifest, one `image.analyze` capability, and a test. Edit `src/plugin.ts` to change what it does, and install the folder from the Settings window. The full walkthrough, including the Python path, is in [docs/plugin-authoring.md](docs/plugin-authoring.md).

## For Developers

### Architecture

Agents reach the core over hooks and MCP. The core owns the event bus, the capability registry and router, the permission manager, and the plugin lifecycle; plugins run as isolated child processes and the WebView is presentation only.

The diagrams, the runtime layers, the data flow of a capability call, and the full module map are in [ARCHITECTURE.md](ARCHITECTURE.md). The protocol docs start at [docs/README.md](docs/README.md).

### Development

The development environment, the pre-commit verification gate, and commit style are in [CONTRIBUTING.md](CONTRIBUTING.md).

## Technology Stack and Credits

- [Tauri 2](https://tauri.app/): desktop shell (tray icon, macOS private API, PNG image support).
- [Rust](https://www.rust-lang.org/): the Core, covering the event bus, capability registry/router, permission manager, plugin runtime, and process manager.
- [TypeScript](https://www.typescriptlang.org/) + [Vite](https://vitejs.dev/): the WebView UI.
- [Three.js](https://threejs.org/) + [@pixiv/three-vrm](https://github.com/pixiv/three-vrm): 3D pet rendering (glTF/VRM).
- [rusqlite](https://github.com/rusqlite/rusqlite): local SQLite storage.
- [tiny_http](https://github.com/tiny-http/tiny-http): local HTTP entry for hook events and MCP forwarding.
- [ed25519-dalek](https://github.com/dalek-cryptography/ed25519-dalek) / [hmac](https://github.com/RustCrypto/MACs) / [sha2](https://github.com/RustCrypto/hashes): plugin package signing and verification.
- [keyring](https://github.com/hwchen/keyring-rs): OS keychain for plugin secrets.
- [DOMPurify](https://github.com/cure53/DOMPurify) + [marked](https://github.com/markedjs/marked): safe markdown rendering in the UI.
- [serde](https://serde.rs/) / serde_json / serde_yaml: serialization.

Any form of PR is welcome (documentation, UI, code).

## Roadmap

[ROADMAP.md](ROADMAP.md).

## License

Apache-2.0. See [LICENSE](LICENSE).

## Security

Do not open a public issue for a vulnerability. The reporting channel, supported versions, and the key-ceremony references are in [SECURITY.md](SECURITY.md).

## More

- [INSTALL.md](INSTALL.md) for download and build-from-source instructions
- [ARCHITECTURE.md](ARCHITECTURE.md) for the runtime layers, data flow, and module map
- [CONTRIBUTING.md](CONTRIBUTING.md) for how to work in this repo
- [ROADMAP.md](ROADMAP.md) for what's next
- [CHANGELOG.md](CHANGELOG.md) for what changed
- [docs/](docs/README.md) for the full spec set
