# Install

Download the latest build from the [releases page](https://github.com/opencapx/OpenCapX/releases/latest): a `.dmg` for macOS (Apple Silicon), an `x64-setup.exe` or `.msi` for Windows, and `.deb` / `.rpm` / `.AppImage` for Linux. Each asset carries a `sha256` digest on its details page, and the release includes a signed `latest.json` updater manifest.

The macOS builds are not code-signed or notarized, and will stay that way until there is a commercial reason to buy an Apple Developer membership. If Gatekeeper blocks the first launch, right-click the app and choose Open, or run `xattr -dr com.apple.quarantine /Applications/OpenCapX.app` after copying it to Applications.

## Build from source

Build from source when you need a local binary.

Prerequisites:

- Rust stable toolchain (`rustup`)
- Node.js 18 or newer with `pnpm` 10
- Python 3.12 or newer (only for the Python plugin SDK and its tests)
- Tauri platform dependencies for your OS (see the [Tauri prerequisites](https://tauri.app/start/prerequisites/))

From a checkout of this repository:

```bash
pnpm install
pnpm tauri build --no-bundle
```

That produces a local binary at `src-tauri/target/release/opencapx`. For a live development window with hot reload:

```bash
pnpm tauri dev
```

Bundling a distributable `.app` or `.dmg` needs the release signing key and happens in CI; locally, plain `pnpm tauri build` fails without that key, so use `--no-bundle`.
