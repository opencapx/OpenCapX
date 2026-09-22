"""Tests for the weather-demo OpenCapX plugin (docs/permission-domains.md Step 4).

Calls ``Plugin.handle()`` directly (no subprocess, no stdio) against a temporary
JSON store — no network, fully deterministic.
"""
import importlib.util
import io
import json
import re
import sys
from pathlib import Path

import pytest

from opencapx_sdk import load_manifest

PLUGIN_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = PLUGIN_DIR / "opencapx-plugin.json"
SCRIPT_PATH = PLUGIN_DIR / "bin" / "weather_demo.py"

# Load bin/weather_demo.py as a module. Its own sys.path walk-up finds
# packages/plugin-sdk, so no PYTHONPATH setup is required.
_spec = importlib.util.spec_from_file_location("weather_demo", SCRIPT_PATH)
weather_demo = importlib.util.module_from_spec(_spec)
sys.modules["weather_demo"] = weather_demo
_spec.loader.exec_module(weather_demo)

WeatherDemo = weather_demo.WeatherDemo


# --------------------------------------------------------------------------- #
# Fixtures / helpers
# --------------------------------------------------------------------------- #

@pytest.fixture
def manifest():
    return load_manifest(str(MANIFEST_PATH))


@pytest.fixture
def make_plugin(tmp_path, manifest):
    def _make():
        m = dict(manifest)
        m["storePath"] = str(tmp_path / "weather.json")
        return WeatherDemo(manifest=m)
    return _make


def call(plugin, method, params=None, req_id=1):
    return plugin.handle(
        {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
    )


def ok(plugin, method, params=None):
    resp = call(plugin, method, params)
    assert "error" not in resp, resp
    return resp["result"]


def err(plugin, method, params=None):
    resp = call(plugin, method, params)
    assert "error" in resp, f"expected error, got {resp}"
    assert "invalid input" in resp["error"]["message"], resp
    return resp["error"]


# --------------------------------------------------------------------------- #
# Manifest: new-domain declaration shape
# --------------------------------------------------------------------------- #

def test_manifest_declares_own_domain(manifest):
    """Object-form capability declaration → permission + default; the domain is a brand-new weather.*."""
    caps = manifest["capabilities"]
    assert len(caps) == 2
    decls = {c["id"]: (c["permission"], c["default"]) for c in caps}
    assert decls == {
        "weather.current": ("weather.read", "ask"),
        "weather.set_home": ("weather.write", "denied"),
    }
    # Declared permissions match the inline mapping (the same manifest is self-consistent)
    assert set(manifest["permissions"]) == {p for p, _ in decls.values()}
    # Domain consistency: capability and permission share the same domain
    for cap_id, (perm, _) in decls.items():
        assert cap_id.split(".")[0] == perm.split(".")[0] == "weather"


# --------------------------------------------------------------------------- #
# Capability handlers
# --------------------------------------------------------------------------- #

def test_read_uses_explicit_city(make_plugin):
    out = ok(make_plugin(), "weather.current", {"city": "Shenzhen"})
    assert out["city"] == "Shenzhen"
    assert out["condition"] in weather_demo.CONDITIONS
    assert 1 <= out["humidity"] <= 100
    assert out["fahrenheit"] == pytest.approx(out["celsius"] * 9 / 5 + 32, abs=0.05)
    assert out["via"] == "weather-demo/local-table"


def test_read_defaults_to_stored_home(make_plugin):
    p = make_plugin()
    assert ok(p, "weather.current", {})["city"] == weather_demo.DEFAULT_HOME
    ok(p, "weather.set_home", {"city": "Shanghai"})
    assert ok(p, "weather.current", {})["city"] == "Shanghai"


def test_read_is_deterministic(make_plugin):
    p = make_plugin()
    a = ok(p, "weather.current", {"city": "Beijing"})
    b = ok(p, "weather.current", {"city": "Beijing"})
    assert a == b, "same input must give same result (demo data, no network)"


@pytest.mark.parametrize("bad", ["", "   ", 42])
def test_read_rejects_bad_city(make_plugin, bad):
    err(make_plugin(), "weather.current", {"city": bad})


def test_set_home_persists_and_reports_previous(make_plugin):
    p = make_plugin()
    first = ok(p, "weather.set_home", {"city": "Shanghai"})
    assert first["previous"] == weather_demo.DEFAULT_HOME
    assert first["home"] == "Shanghai"
    second = ok(p, "weather.set_home", {"city": "Shenzhen"})
    assert second["previous"] == "Shanghai"


@pytest.mark.parametrize("bad", [{}, {"city": ""}, {"city": "   "}, {"city": 7}])
def test_set_home_rejects_bad_input(make_plugin, bad):
    err(make_plugin(), "weather.set_home", bad)


def test_store_survives_new_instance(make_plugin, tmp_path):
    """Writes land in manifest.storePath and are still readable from a new instance (not in-memory); on-disk shape = v2."""
    ok(make_plugin(), "weather.set_home", {"city": "Shenzhen"})
    raw = json.loads((tmp_path / "weather.json").read_text(encoding="utf-8"))
    assert raw["schema"] == WeatherDemo.STORE_SCHEMA
    assert raw["home_city"] == "Shenzhen"
    assert ok(make_plugin(), "weather.current", {})["city"] == "Shenzhen"


def test_store_v1_migrates_to_v2_idempotently(make_plugin, tmp_path):
    """F9 — v1 store ({"home": …}) migrates idempotently to v2 on first read; the old city is preserved and the shape is stable."""
    (tmp_path / "weather.json").write_text(json.dumps({"home": "Chengdu"}), encoding="utf-8")
    p = make_plugin()
    assert ok(p, "weather.current", {})["city"] == "Chengdu", "old data must be preserved"
    raw = json.loads((tmp_path / "weather.json").read_text(encoding="utf-8"))
    assert raw["schema"] == WeatherDemo.STORE_SCHEMA
    assert raw["home_city"] == "Chengdu"
    assert "migrated_from" in raw
    # Idempotent: a second read (new instance) no longer rewrites the file
    before = (tmp_path / "weather.json").read_text(encoding="utf-8")
    assert ok(make_plugin(), "weather.current", {})["city"] == "Chengdu"
    assert (tmp_path / "weather.json").read_text(encoding="utf-8") == before


# --------------------------------------------------------------------------- #
# Phase 37 self-check
# --------------------------------------------------------------------------- #

def test_probe_reports_capabilities(make_plugin):
    out = ok(make_plugin(), "core.probe.capability", {})
    assert out["ok"] is True
    assert out["via"] == "weather-demo"
    assert out["capabilities"] == ["weather.current", "weather.set_home"]


def test_unknown_capability_is_method_not_found(make_plugin):
    resp = call(make_plugin(), "weather.nope", {})
    assert "error" in resp
    assert resp["error"]["code"] == -32601


# --------------------------------------------------------------------------- #
# Declarative settings (settings[] → config.*, read per call)
# --------------------------------------------------------------------------- #

def _raise(*_args, **_kwargs):
    raise OSError("no core")


def with_settings(plugin, settings=None, *, set_ok=True):
    """Install the reverse config.get/set test seam.

    ``Plugin.config_get``/``config_set`` block on stdin waiting for Core's reply;
    unit tests call ``handle()`` directly, so serve config from an in-memory dict.
    ``set_ok=False`` simulates a failed reverse write. ``settings_values`` is the
    live dict, so a test can mutate it to model a settings save mid-session.
    """
    values = dict(settings or {})
    plugin.requested_config_keys = []
    plugin.written_config = {}

    def fake_config_get(key, default=None):
        plugin.requested_config_keys.append(key)
        return values.get(key, default)

    def fake_config_set(key, value):
        plugin.written_config[key] = value
        if set_ok:
            values[key] = value
        return set_ok

    plugin.config_get = fake_config_get
    plugin.config_set = fake_config_set
    plugin.settings_values = values
    return plugin


def seed_store(tmp_path, home_city):
    (tmp_path / "weather.json").write_text(
        json.dumps({"schema": WeatherDemo.STORE_SCHEMA, "home_city": home_city}),
        encoding="utf-8",
    )


def test_declared_setting_drives_weather_current(make_plugin):
    p = with_settings(make_plugin(), {"home_city": "Shenzhen"})
    assert ok(p, "weather.current", {})["city"] == "Shenzhen"


def test_declared_setting_beats_store_home(make_plugin, tmp_path):
    seed_store(tmp_path, "Shanghai")
    p = with_settings(make_plugin(), {"home_city": "Shenzhen"})
    assert ok(p, "weather.current", {})["city"] == "Shenzhen", "setting takes precedence over store"


def test_store_home_honored_when_setting_unset(make_plugin, tmp_path):
    """Old install (predating settings) with the setting unset → the store's home_city still applies as usual."""
    seed_store(tmp_path, "Shanghai")
    p = with_settings(make_plugin(), {})
    assert ok(p, "weather.current", {})["city"] == "Shanghai"


def test_default_home_is_last_resort(make_plugin):
    p = with_settings(make_plugin(), {})
    assert weather_demo.DEFAULT_HOME == "Beijing"
    assert ok(p, "weather.current", {})["city"] == weather_demo.DEFAULT_HOME


def test_setting_takes_effect_without_restart(make_plugin):
    p = with_settings(make_plugin(), {})
    assert ok(p, "weather.current", {})["city"] == weather_demo.DEFAULT_HOME
    p.settings_values["home_city"] = "Shanghai"  # settings page save → takes effect on next call
    assert ok(p, "weather.current", {})["city"] == "Shanghai"


def test_set_home_writes_setting_and_store_agree(make_plugin, tmp_path):
    p = with_settings(make_plugin(), {})
    out = ok(p, "weather.set_home", {"city": "Shanghai"})
    assert out["previous"] == weather_demo.DEFAULT_HOME
    assert p.written_config["home_city"] == "Shanghai", "must write the declared setting (config_set)"
    assert p.settings_values["home_city"] == "Shanghai"
    raw = json.loads((tmp_path / "weather.json").read_text(encoding="utf-8"))
    assert raw["home_city"] == "Shanghai", "store is still kept as a mirror"
    assert p.home() == "Shanghai"
    assert ok(p, "weather.current", {})["city"] == "Shanghai"


def test_failed_config_get_falls_through_to_store(make_plugin, tmp_path):
    seed_store(tmp_path, "Shenzhen")
    p = make_plugin()
    p.config_get = _raise
    assert ok(p, "weather.current", {})["city"] == "Shenzhen"


def test_failed_config_get_without_store_uses_default(make_plugin):
    p = make_plugin()
    p.config_get = _raise
    assert ok(p, "weather.current", {})["city"] == weather_demo.DEFAULT_HOME


def test_failed_config_set_does_not_fail_set_home(make_plugin, tmp_path):
    p = make_plugin()
    p.config_set = _raise
    out = ok(p, "weather.set_home", {"city": "Shanghai"})
    assert out["home"] == "Shanghai"
    raw = json.loads((tmp_path / "weather.json").read_text(encoding="utf-8"))
    assert raw["home_city"] == "Shanghai", "reverse write failed, but the store must still persist"


def test_real_sdk_config_get_roundtrip_drives_home(make_plugin, monkeypatch):
    """Real SDK config_get against Core's bare-value reply shape (no seam)."""
    p = make_plugin()
    sent = []
    p._send = sent.append
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO('{"jsonrpc":"2.0","id":"p1","result":"Shenzhen"}\n'),
    )

    assert ok(p, "weather.current", {})["city"] == "Shenzhen"
    requests = [m for m in sent if m.get("method") == "config.get"]
    assert [r["params"]["key"] for r in requests] == ["home_city"]
    assert requests[0]["params"]["default"] == "", "the unset sentinel must be an empty string"


# --------------------------------------------------------------------------- #
# settings[] schema + drift guard
# --------------------------------------------------------------------------- #

def test_manifest_home_city_setting_schema_is_install_valid(manifest):
    settings = manifest.get("settings")
    assert settings, "weather-demo must declare a settings[] block"
    assert len(settings) == 1, "declare only the single home_city setting"
    decl = settings[0]
    assert decl["key"] == "home_city"
    assert decl["type"] == "dropdown"
    assert re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", decl["key"])
    assert decl["options"], "dropdown requires options[]"
    assert decl["default"] == "Beijing"
    assert decl["default"] in decl["options"]
    for field in ("label", "description"):
        text = decl[field]
        assert {"en", "zh-Hans", "vi"} <= set(text), f"{field} must cover en/zh-Hans/vi"
        assert all(isinstance(v, str) and v.strip() for v in text.values())
    assert decl["aliases"], "aliases is required"
    assert len(decl["aliases"]) <= 8
    assert any(re.search(r"[\u4e00-\u9fff]", a) for a in decl["aliases"]), (
        "at least one Chinese alias"
    )


def test_manifest_options_match_python_seasonal_table(manifest):
    """Drift test: manifest options == Python SEASONAL keys (same order).

    Adding a city to SEASONAL but forgetting to update the manifest (or vice versa) → this test fails.
    """
    decl = {s["key"]: s for s in manifest["settings"]}["home_city"]
    assert decl["options"] == list(weather_demo.SEASONAL.keys()), (
        "manifest options have drifted from the SEASONAL table in bin/weather_demo.py"
    )
    assert decl["default"] == weather_demo.DEFAULT_HOME
