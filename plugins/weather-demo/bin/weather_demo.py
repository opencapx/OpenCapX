#!/usr/bin/env python3
"""OpenCapX Weather Demo plugin — the sample for the third-party new-domain declaration scheme.

The acceptance sample for docs/permission-domains.md Step 4: it **reuses no built-in domain**,
instead shipping a brand-new domain `weather.*`, declared in object form in the manifest:

    weather.current   → weather.read   (default ask)
    weather.set_home  → weather.write  (default denied)

Core needs no code change / release: per-item confirmation at install → declarations frozen → routing follows the declarations.

Design constraints:

* **Offline**: no network requests. "Current weather" is derived from the home city in the local
  store plus a fixed seasonal table, and the result carries a ``via`` field naming the data source,
  so tests are repeatable.
* ``weather.write`` defaults to denied: a write (changing the home city) is an explicit action and
  should not be silently allowed — demonstrating that the "declared default" really matters in the permission model.

* **Data migration (F9, since v0.1.1)**: the store upgrades from v1 (``{"home": …}``) to
  v2 (``{"schema": 2, "home_city": …}``). On startup Core injects ``previousVersion``, and this plugin
  performs an **idempotent** migration based on the store shape (safe to rerun/roll back); the host does not move data.

The JSON-RPC stdio loop, lifecycle, and reverse calls are all provided by ``opencapx_sdk``
(packages/plugin-sdk).
"""
from __future__ import annotations

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

# First try a direct import (SDK pip-installed or PYTHONPATH already set); otherwise walk up to the
# repo root's packages/plugin-sdk. Robust whether installed to a temp dir (tests) or a system path (production).
try:
    from opencapx_sdk import Plugin, capability, method  # type: ignore
except ImportError:
    _here = Path(__file__).resolve()
    for _parent in [_here, *_here.parents]:
        _candidate = _parent / "packages" / "plugin-sdk"
        if (_candidate / "opencapx_sdk" / "__init__.py").is_file():
            sys.path.insert(0, str(_candidate))
            break
    from opencapx_sdk import Plugin, capability, method  # type: ignore  # noqa: E402


# --------------------------------------------------------------------------- #
# Constants
# --------------------------------------------------------------------------- #

DEFAULT_STORE = Path.home() / ".opencapx" / "cache" / "weather-demo.json"
DEFAULT_HOME = "Beijing"

# Fixed seasonal table — demo data, not fetched from the network. Keys are months 1..12.
SEASONAL = {
    "Beijing": [1, 4, 11, 18, 24, 28, 30, 29, 24, 17, 8, 2],
    "Shanghai": [7, 9, 13, 18, 23, 27, 31, 31, 27, 22, 16, 10],
    "Shenzhen": [16, 17, 20, 24, 27, 29, 30, 30, 29, 26, 22, 18],
}
# Humidity / weather phrase: pick one bucket by (temperature // 5) % 4, guaranteeing same input → same output
HUMIDITY = [38, 52, 66, 79]
CONDITIONS = ["clear", "partly-cloudy", "cloudy", "drizzle"]


class WeatherDemo(Plugin):
    """Minimal implementation of the two capabilities weather.current / weather.set_home."""

    # F9 — store schema version: v1 = {"home": …} (no schema field); explicitly marked from v2 on.
    STORE_SCHEMA = 2

    # -- local store (demo backend, no network) --

    def _store_path(self) -> Path:
        raw = self.manifest.get("storePath")
        if isinstance(raw, str) and raw:
            return Path(raw)
        # soak/S5b: defaults to plugin-data (the sandbox's only writable area); the old default
        # ~/.opencapx/cache/ is rejected under a sandbox declaration. env overrides are still constrained by the S1 allowlist.
        env = os.environ.get("OPENCAPX_WEATHER_STORE")
        if env:
            return Path(env)
        pid = self.manifest.get("id") or "weather-demo"
        return Path.home() / ".opencapx" / "plugin-data" / pid / "store.json"

    def _load(self) -> dict[str, Any]:
        try:
            with self._store_path().open("r", encoding="utf-8") as f:
                data = json.load(f)
        except (OSError, ValueError):
            return {}
        return self._migrate(data) if isinstance(data, dict) else {}

    def _migrate(self, data: dict[str, Any]) -> dict[str, Any]:
        """F9 — idempotent v1→v2 migration: triggered by the store shape; ``previousVersion`` is only a provenance annotation.

        v1: ``{"home": "<city>"}`` → v2: ``{"schema": 2, "home_city": "<city>", "migrated_from": …}``.
        """
        if data.get("schema") == self.STORE_SCHEMA:
            return data
        if "home" not in data:
            return data  # empty/unknown shape: leave it alone and do not persist
        legacy_home = data.get("home")
        migrated = {
            "schema": self.STORE_SCHEMA,
            "home_city": legacy_home
            if isinstance(legacy_home, str) and legacy_home
            else DEFAULT_HOME,
            "migrated_from": self.previous_version,
        }
        self._save(migrated)
        return migrated

    def _save(self, data: dict[str, Any]) -> None:
        path = self._store_path()
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)

    # -- declarative settings (settings[] → config.*, read per call) --

    def _setting(self, key: str, default: Any = None) -> Any:
        """Read a declarative setting; a failed reverse call (no Core / stdin EOF / protocol error) must never fail the capability."""
        try:
            value = self.config_get(key, default)
        except Exception:  # noqa: BLE001 - a settings read must never fail a capability
            return default
        return default if value is None else value

    def _setting_str(self, key: str) -> str:
        value = self._setting(key, "")
        return value.strip() if isinstance(value, str) else ""

    def declared_home(self) -> str:
        """Declared setting; unset/blank → "" so the store can take over.

        Do not pass ``DEFAULT_HOME`` as config_get's default: Core only echoes back the default the
        caller supplied and does not materialize manifest defaults; passing it would permanently shadow the store.
        """
        return self._setting_str("home_city")

    def store_home(self) -> str:
        """The store's ``home_city`` (the fallback before settings existed); none → ""."""
        home = self._load().get("home_city")
        return home if isinstance(home, str) and home else ""

    def home(self) -> str:
        """Resolve the default city: declared setting → store ``home_city`` → ``DEFAULT_HOME``.

        Re-read on every call: after saving on the settings page the plugin need not restart; the next capability call takes effect immediately.
        """
        return self.declared_home() or self.store_home() or DEFAULT_HOME

    def _write_setting(self, key: str, value: Any) -> bool:
        """Write a declarative setting; returning False means the reverse write failed (the caller decides whether to still persist to the store)."""
        try:
            return bool(self.config_set(key, value))
        except Exception:  # noqa: BLE001 - a settings write must not fail the capability
            return False

    def _forecast(self, city: str) -> dict[str, Any]:
        """Derive today's weather from the fixed table. An unknown city falls back to the default city's table (still deterministic)."""
        table = SEASONAL.get(city) or SEASONAL[DEFAULT_HOME]
        month = datetime.now(timezone.utc).month
        celsius = table[month - 1]
        return {
            "city": city,
            "celsius": celsius,
            "fahrenheit": round(celsius * 9 / 5 + 32, 1),
            "humidity": HUMIDITY[(celsius // 5) % len(HUMIDITY)],
            "condition": CONDITIONS[(celsius // 5) % len(CONDITIONS)],
            "observedAt": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        }

    # -- capabilities --

    @capability("weather.current")
    def weather_current(self, params: dict[str, Any]) -> dict[str, Any]:
        """Read the current weather. params.city is optional; when absent, use the result resolved by ``home()``."""
        city = params.get("city")
        if city is not None and (not isinstance(city, str) or not city.strip()):
            raise ValueError("invalid input: city must be a non-empty string when provided")
        resolved = city.strip() if isinstance(city, str) else self.home()
        return {"via": "weather-demo/local-table", **self._forecast(resolved)}

    @capability("weather.set_home")
    def weather_set_home(self, params: dict[str, Any]) -> dict[str, Any]:
        """Write the home city (weather.write, denied by default — an explicit action).

        The **declared setting** is authoritative, mirrored to the store at the same time, so the settings
        page and older plugin versions always read the same city; a failed reverse write (no Core) does not
        fail the capability either, and the store still persists to preserve the old path.
        """
        city = params.get("city")
        if not isinstance(city, str) or not city.strip():
            raise ValueError("invalid input: city is required and must be a non-empty string")
        city = city.strip()
        previous = self.home()
        self._write_setting("home_city", city)
        data = self._load()
        data["schema"] = self.STORE_SCHEMA
        data["home_city"] = city
        self._save(data)
        return {"via": "weather-demo/local-store", "previous": previous, "home": city}

    # -- F9 lifecycle --

    def on_initialize(self, params: dict[str, Any]) -> dict[str, Any]:
        """F9 — migrate on startup (idempotent): inject the previousVersion annotation first, then trigger one read-migration.

        The migration trigger is based on the store shape (see ``_migrate``), so rerunning, rolling back, and reinstalling are all safe.
        """
        prev = params.get("previousVersion")
        self.previous_version = prev if isinstance(prev, str) else None
        self._load()
        return super().on_initialize(params)

    # -- Phase 37 self-check endpoint --

    @method("core.probe.capability")
    def probe(self, params: dict[str, Any]) -> dict[str, Any]:
        return {
            "ok": True,
            "via": "weather-demo",
            "home": self.home(),
            "capabilities": sorted(self._capabilities.keys()),
        }


if __name__ == "__main__":
    WeatherDemo(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()
