# Weather Demo (OpenCapX plugin)

**The acceptance sample for the third-party permission-domain proposal**
([docs/permission-domains.md](../../docs/permission-domains.md) §9.1 Step 4).

It is deliberately *not* built on a built-in domain. It ships its own brand-new
`weather.*` domain and declares it in `opencapx-plugin.json`:

```json
"capabilities": [
  { "id": "weather.current",  "permission": "weather.read",  "default": "ask" },
  { "id": "weather.set_home", "permission": "weather.write", "default": "denied" }
],
"permissions": ["weather.read", "weather.write"]
```

That single manifest is the whole contract. Core needs **no code change and no
release** to route `weather.current`, prompt for `weather.read`, or enforce
`weather.write`. Installing it exercises the full chain:

| Hop | What happens |
|---|---|
| install | Lexicon + reserved-domain checks → per-item consent dialog (`weather.read` × once-only, `weather.write` × once-only) → single transaction writes `plugins` + `plugin_permissions` + `capability_declarations` + `domain_registry` |
| routing | `capability::known("weather.current")` = builtin ∪ frozen declarations; the resolver maps it to `weather.read` |
| call | Plugin-layer gate asks (declared permissions are **once-only** — no Always, in either the install dialog or the runtime bubble) |
| update | Every update re-confirms; a changed mapping is a re-confirmation, not a silent implementation swap |
| uninstall | Declaration rows deleted, `weather` domain released |

- Plugin id: `com.example.weather-demo`
- Type: `capability`
- Capabilities: `weather.current`, `weather.set_home`
- Permissions: `weather.read` (default `ask`), `weather.write` (default `denied`)

## Offline by design

No network calls. `weather.current` derives conditions from a fixed seasonal
table plus a local JSON store (`~/.opencapx/cache/weather-demo.json`, overridable
via the manifest's `storePath`), so results are deterministic and tests are
hermetic. Every response carries a `via` field naming the data source.

`weather.write` defaults to **`denied`** on purpose: changing the home city is an
explicit action, and a declared default of `denied` demonstrates that the default
in the manifest is what the permission model actually enforces.

## Settings

Declared in `opencapx-plugin.json` (`settings[]`) and rendered in OpenCapX
**Settings → Plugins → Weather Demo**. The plugin re-reads them on every
capability call, so a save takes effect immediately — no plugin restart.

| Setting | Type | Default | Effect |
|---|---|---|---|
| `home_city` | dropdown (`Beijing` / `Shanghai` / `Shenzhen`) | `Beijing` | City `weather.current` reports when the caller passes no explicit `city`. The `options[]` are exactly the cities in the plugin's `SEASONAL` table; a test fails if the two drift apart. |

**Resolution order** for the default city:

1. the declared `home_city` setting (read via `config.get` each call);
2. the store's `home_city` — the pre-settings fallback, so installs created
   before `settings[]` existed keep their chosen city;
3. `DEFAULT_HOME` (`Beijing`) as the last resort.

`weather.set_home` writes **both** the declared setting (via `config.set`) and
the store, so the settings UI and the legacy store cannot disagree. The declared
setting is **authoritative**; the store is mirrored for backward compatibility
(and for plugin versions that predate `settings[]`) — never migrated away. A
failed reverse read or write never fails a capability: resolution falls through
to the next source, and the store is still persisted.

## Try it

```bash
# from the repo root — installs from a directory (dev flow)
cargo run --manifest-path src-tauri/Cargo.toml   # then install plugins/weather-demo via the UI
```

1. Open **Settings → Plugins → Weather Demo** and pick a **Home city**
   (`Beijing` / `Shanghai` / `Shenzhen`).
2. Ask any connected Agent to run `weather.current`; it now uses that city.
   Expect a permission bubble each time — that is once-only working, not a bug.
3. Change `home_city` again and re-run `weather.current`: the new city applies
   on the next call, without restarting the plugin.

To exercise the legacy store path (or a plugin version before `settings[]`),
set `OPENCAPX_WEATHER_STORE=/tmp/weather.json` and use `weather.set_home`; the
store stays readable as the pre-settings fallback.

## Tests

```bash
PYTHONPATH=../../packages/plugin-sdk python3 -m pytest tests/ -q
```

Covers the manifest's declared-domain shape, both handlers, input validation,
store persistence, the `home_city` setting (setting → store → `DEFAULT_HOME`
precedence, per-call reload, failed reverse reads/writes, and a drift guard that
keeps `options[]` in sync with the `SEASONAL` table), and the Phase 37 probe.
The Rust side has the end-to-end counterpart:
`core::plugin::tests::weather_demo_declared_domain_end_to_end`.
