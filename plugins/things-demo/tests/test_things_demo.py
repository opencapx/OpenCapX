"""Tests for the things-demo OpenCapX plugin.

These call ``Plugin.handle()`` directly (no subprocess, no stdio), against a
temporary JSON store — no network and no Things3 needed.
"""
import importlib.util
import io
import json
import re
import sys
from pathlib import Path

import pytest

PLUGIN_DIR = Path(__file__).resolve().parents[1]
MANIFEST_PATH = PLUGIN_DIR / "opencapx-plugin.json"
SCRIPT_PATH = PLUGIN_DIR / "bin" / "things_demo.py"

# Load bin/things_demo.py as a module. Its own sys.path walk-up finds
# packages/plugin-sdk, so no PYTHONPATH setup is required.
_spec = importlib.util.spec_from_file_location("things_demo", SCRIPT_PATH)
things_demo = importlib.util.module_from_spec(_spec)
sys.modules["things_demo"] = things_demo
_spec.loader.exec_module(things_demo)

ThingsDemo = things_demo.ThingsDemo
EXPECTED_CAPABILITIES = {
    "things.add",
    "things.update",
    "things.list",
    "things.show",
    "things.search",
    "things.delete",
}


# --------------------------------------------------------------------------- #
# Fixtures / helpers
# --------------------------------------------------------------------------- #

@pytest.fixture
def make_plugin(tmp_path, monkeypatch):
    # Force demo mode: never touch Things3 or macOS `open`.
    monkeypatch.delenv("OPENCAPX_THINGS_MODE", raising=False)
    monkeypatch.delenv("OPENCAPX_THINGS_STORE", raising=False)
    monkeypatch.delenv("THINGS_AUTH_TOKEN", raising=False)

    def _make(store_path=None, settings=None):
        plugin = ThingsDemo(
            manifest_path=str(MANIFEST_PATH),
            store_path=str(store_path or (tmp_path / "store.json")),
            mode="demo",
        )
        plugin._send = lambda msg: None  # never emit protocol frames in tests
        return with_settings(plugin, settings)

    return _make


def with_settings(plugin, settings=None):
    """Install the reverse config.get test seam.

    ``Plugin.config_get`` blocks on stdin waiting for the Core reply; unit tests
    call ``handle()`` directly, so serve ``config.get`` from a dict instead.
    Records requested keys so the secret-path convention can be asserted.
    """
    values = dict(settings or {})
    plugin.requested_config_keys = []

    def fake_config_get(key, default=None):
        plugin.requested_config_keys.append(key)
        return values.get(key, default)

    plugin.config_get = fake_config_get
    plugin.settings_values = values
    return plugin


@pytest.fixture
def plugin(make_plugin):
    return make_plugin()


def call(plugin, method, params=None, req_id=1):
    return plugin.handle(
        {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params or {}}
    )


def ok(plugin, method, params=None):
    resp = call(plugin, method, params)
    assert "error" not in resp, resp
    return resp["result"]


# --------------------------------------------------------------------------- #
# Handshake / probe
# --------------------------------------------------------------------------- #

def test_initialize_lists_all_capabilities(plugin):
    result = ok(plugin, "plugin.initialize", {})
    assert result["pluginId"] == "com.opencapx.things-demo"
    assert result["apiVersion"] == "1"
    caps = {c["id"] for c in result["capabilities"]}
    assert caps == EXPECTED_CAPABILITIES
    assert len(result["capabilities"]) == 6


def test_ping_returns_ok(plugin):
    assert ok(plugin, "plugin.ping", {}) == {"ok": True}


def test_probe_returns_ok_and_demo_mode(plugin):
    result = ok(plugin, "core.probe.capability", {})
    assert result["ok"] is True
    assert result["mode"] == "demo"
    assert set(result["capabilities"]) == EXPECTED_CAPABILITIES


def test_manifest_store_path_takes_priority(tmp_path, monkeypatch):
    """F9 alignment: manifest storePath > env > default (a reliable override under env isolation/sandboxing)."""
    monkeypatch.delenv("OPENCAPX_THINGS_STORE", raising=False)
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    manifest["storePath"] = str(tmp_path / "manifest-store.json")
    mp = tmp_path / "opencapx-plugin.json"
    mp.write_text(json.dumps(manifest), encoding="utf-8")
    p = ThingsDemo(manifest_path=str(mp), mode="demo")
    p._send = lambda msg: None
    ok(p, "things.add", {"title": "from-manifest-store"})
    assert (tmp_path / "manifest-store.json").exists(), "the write should land in manifest storePath"
    # the env fallback is still there (when there is no storePath)
    monkeypatch.setenv("OPENCAPX_THINGS_STORE", str(tmp_path / "env-store.json"))
    manifest.pop("storePath", None)
    mp.write_text(json.dumps(manifest), encoding="utf-8")
    p2 = ThingsDemo(manifest_path=str(mp), mode="demo")
    p2._send = lambda msg: None
    ok(p2, "things.add", {"title": "from-env-store"})
    assert (tmp_path / "env-store.json").exists()


def test_unknown_method_returns_method_not_found(plugin):
    resp = call(plugin, "things.nope", {})
    assert resp["error"]["code"] == -32601


# --------------------------------------------------------------------------- #
# CRUD roundtrip
# --------------------------------------------------------------------------- #

def test_add_returns_ok_id_title_via(plugin):
    result = ok(plugin, "things.add", {"title": "Buy milk"})
    assert result["ok"] is True
    assert result["title"] == "Buy milk"
    assert isinstance(result["id"], str) and len(result["id"]) == 8
    assert result["via"] == "demo"


def test_add_show_update_search_list_roundtrip(plugin):
    added = ok(
        plugin,
        "things.add",
        {"title": "Buy milk", "notes": "2%", "when": "today", "tags": ["errand"]},
    )
    todo_id = added["id"]

    shown = ok(plugin, "things.show", {"id": todo_id})
    assert shown["todo"]["title"] == "Buy milk"
    assert shown["todo"]["when"] == "today"
    assert shown["todo"]["tags"] == ["errand"]
    assert shown["todo"]["completed"] is False

    updated = ok(plugin, "things.update", {"id": todo_id, "completed": True})
    assert updated["ok"] is True
    assert updated["id"] == todo_id
    assert updated["updated"]["completed"] is True

    found = ok(plugin, "things.search", {"query": "milk"})
    assert found["count"] == 1
    assert found["todos"][0]["id"] == todo_id

    listed_all = ok(plugin, "things.list", {"list": "all", "include_completed": True})
    assert listed_all["count"] == 1
    assert listed_all["todos"][0]["completed"] is True

    # Completed todos are hidden by default and appear under logbook.
    assert ok(plugin, "things.list", {"list": "all"})["count"] == 0
    assert ok(plugin, "things.list", {"list": "logbook"})["count"] == 1


def test_update_append_notes_and_add_tags(plugin):
    todo_id = ok(plugin, "things.add", {"title": "Task"})["id"]
    result = ok(
        plugin,
        "things.update",
        {"id": todo_id, "append_notes": "more", "add_tags": ["a", "b", "a"]},
    )
    todo = result["updated"]
    assert todo["notes"] == "more"
    assert todo["tags"] == ["a", "b"]


def test_list_filters_by_bucket_and_respects_limit(plugin):
    ok(plugin, "things.add", {"title": "A", "when": "today"})
    ok(plugin, "things.add", {"title": "B", "when": "today"})
    ok(plugin, "things.add", {"title": "C", "when": "someday"})

    today = ok(plugin, "things.list", {"list": "today"})
    assert today["count"] == 2
    assert {t["title"] for t in today["todos"]} == {"A", "B"}
    assert today["via"] == "demo"

    limited = ok(plugin, "things.list", {"list": "all", "limit": 1})
    assert limited["count"] == 1


def test_show_by_query_returns_matches(plugin):
    ok(plugin, "things.add", {"title": "Call Alice"})
    ok(plugin, "things.add", {"title": "Email Bob"})
    result = ok(plugin, "things.show", {"query": "alice"})
    assert result["count"] == 1
    assert result["todos"][0]["title"] == "Call Alice"


def test_delete_removes_todo(plugin):
    todo_id = ok(plugin, "things.add", {"title": "Doomed"})["id"]

    deleted = ok(plugin, "things.delete", {"id": todo_id})
    assert deleted["ok"] is True
    assert deleted["id"] == todo_id
    assert deleted["via"] == "demo"

    assert ok(plugin, "things.list", {"list": "all"})["count"] == 0
    resp = call(plugin, "things.delete", {"id": todo_id})
    assert "error" in resp
    assert "not found" in resp["error"]["message"]


def test_store_persists_across_instances(make_plugin):
    store = make_plugin()._store_path  # same tmp_path store for both
    first = make_plugin(store_path=store)
    todo_id = ok(first, "things.add", {"title": "Persisted"})["id"]

    second = make_plugin(store_path=store)
    shown = ok(second, "things.show", {"id": todo_id})
    assert shown["todo"]["title"] == "Persisted"


def test_store_path_from_env(tmp_path, monkeypatch):
    custom = tmp_path / "custom.json"
    monkeypatch.setenv("OPENCAPX_THINGS_STORE", str(custom))
    p = ThingsDemo(manifest_path=str(MANIFEST_PATH), mode="demo")
    assert p._store_path == custom


def test_default_store_path(monkeypatch):
    monkeypatch.delenv("OPENCAPX_THINGS_STORE", raising=False)
    p = ThingsDemo(manifest_path=str(MANIFEST_PATH), mode="demo")
    # soak/S5b: defaults to plugin-data (the sandbox's only writable area), no longer ~/.opencapx/cache.
    assert p._store_path == (
        Path.home() / ".opencapx" / "plugin-data" / "com.opencapx.things-demo" / "store.json"
    )


# --------------------------------------------------------------------------- #
# Validation
# --------------------------------------------------------------------------- #

@pytest.mark.parametrize(
    "method,params,needle",
    [
        ("things.add", {}, "title"),
        ("things.add", {"title": "   "}, "title"),
        ("things.add", {"title": "x", "when": "whenever"}, "when"),
        ("things.add", {"title": "x", "deadline": "soon"}, "deadline"),
        ("things.add", {"title": "x", "tags": "nope"}, "tags"),
        ("things.add", {"title": "x", "list": ""}, "list"),
        ("things.update", {}, "id"),
        ("things.update", {"id": "missing"}, "not found"),
        ("things.update", {"id": "x", "completed": "yes"}, "completed"),
        ("things.list", {"list": "later"}, "list"),
        ("things.list", {"limit": 0}, "limit"),
        ("things.list", {"limit": 101}, "limit"),
        ("things.list", {"limit": "5"}, "limit"),
        ("things.list", {"include_completed": "yes"}, "include_completed"),
        ("things.show", {}, "exactly one"),
        ("things.show", {"id": "x", "query": "y"}, "exactly one"),
        ("things.show", {"id": "missing"}, "not found"),
        ("things.search", {}, "query"),
        ("things.search", {"query": "   "}, "query"),
    ],
)
def test_invalid_input_returns_error(plugin, method, params, needle):
    resp = call(plugin, method, params)
    assert "error" in resp
    assert resp["error"]["code"] == -32603
    assert "invalid input" in resp["error"]["message"]
    assert needle in resp["error"]["message"]


# --------------------------------------------------------------------------- #
# Live mode never crashes when Things3 is absent
# --------------------------------------------------------------------------- #

def test_live_add_falls_back_to_demo_when_open_fails(
    make_plugin, monkeypatch, capsys
):
    def boom(*args, **kwargs):
        raise FileNotFoundError("open: Things3 not found")

    monkeypatch.setattr(things_demo.subprocess, "run", boom)

    plugin = make_plugin()
    plugin._mode = "live"
    result = ok(plugin, "things.add", {"title": "Offline todo"})
    assert result["ok"] is True
    assert result["via"].startswith("demo")

    listed = ok(plugin, "things.list", {"list": "all"})
    assert listed["count"] == 1
    assert listed["todos"][0]["title"] == "Offline todo"
    assert listed["via"].startswith("demo")


def test_live_add_creates_missing_tags_before_open(make_plugin, monkeypatch):
    calls = []

    def fake_run(cmd, **kwargs):
        calls.append(cmd)
        return None

    monkeypatch.setattr(things_demo.subprocess, "run", fake_run)

    plugin = make_plugin()
    plugin._mode = "live"
    result = ok(plugin, "things.add", {"title": "Tagged", "tags": ["fresh-tag"]})
    assert result["via"] == "things-url"

    assert len(calls) == 2
    osa, opened = calls
    assert osa[0] == "osascript"
    assert "fresh-tag" in osa[-1]
    assert "app.make" in osa[-1]
    assert opened[0] == "open"
    assert opened[1].startswith("things:///add?")
    assert "tags=fresh-tag" in opened[1]


def test_live_add_survives_tag_precreate_failure(make_plugin, monkeypatch, capsys):
    def selective_run(cmd, **kwargs):
        if cmd[0] == "osascript":
            raise RuntimeError("automation denied")
        return None

    monkeypatch.setattr(things_demo.subprocess, "run", selective_run)

    plugin = make_plugin()
    plugin._mode = "live"
    result = ok(plugin, "things.add", {"title": "Still added", "tags": ["x"]})
    assert result["ok"] is True
    assert result["via"] == "things-url"
    assert "tag pre-create failed" in capsys.readouterr().err


def test_live_read_annotates_when_from_smart_lists(make_plugin, monkeypatch):
    captured = {}

    def fake_run(cmd, **kwargs):
        captured["cmd"] = cmd
        payload = (
            '[{"id":"t1","title":"T","notes":"","completed":false,'
            '"tags":[],"when":"today","list":"things"}]'
        )
        return things_demo.subprocess.CompletedProcess(cmd, 0, payload, "")

    monkeypatch.setattr(things_demo.subprocess, "run", fake_run)

    plugin = make_plugin()
    plugin._mode = "live"
    listed = ok(plugin, "things.list", {"list": "today"})
    assert listed["via"] == "things-jxa"
    assert listed["count"] == 1
    script = captured["cmd"][-1]
    assert "Today" in script and "Tomorrow" in script


def test_live_delete_uses_jxa(make_plugin, monkeypatch):
    captured = {}

    def fake_run(cmd, **kwargs):
        captured["cmd"] = cmd
        return things_demo.subprocess.CompletedProcess(cmd, 0, '{"found":true}', "")

    monkeypatch.setattr(things_demo.subprocess, "run", fake_run)

    plugin = make_plugin()
    plugin._mode = "live"
    result = ok(plugin, "things.delete", {"id": "TpWWVNNB7WwX47ZETmTucC"})
    assert result["ok"] is True
    assert result["via"] == "things-jxa"
    script = captured["cmd"][-1]
    assert "TpWWVNNB7WwX47ZETmTucC" in script
    assert ".delete()" in script


def test_live_delete_falls_back_to_mirror(make_plugin, monkeypatch):
    def boom(cmd, **kwargs):
        raise RuntimeError("things unavailable")

    monkeypatch.setattr(things_demo.subprocess, "run", boom)

    plugin = make_plugin()
    plugin._mode = "live"
    added = ok(plugin, "things.add", {"title": "Mirror only"})
    removed = ok(plugin, "things.delete", {"id": added["id"]})
    assert removed["ok"] is True
    assert removed["via"].startswith("demo")
    assert ok(plugin, "things.list", {"list": "all"})["count"] == 0


# --------------------------------------------------------------------------- #
# Declarative settings (settings[] → config.*, read per call)
# --------------------------------------------------------------------------- #

def _fresh(monkeypatch, tmp_path, settings=None, *, mode=None, store_path=None):
    for name in ("OPENCAPX_THINGS_MODE", "OPENCAPX_THINGS_STORE", "THINGS_AUTH_TOKEN"):
        monkeypatch.delenv(name, raising=False)
    plugin = ThingsDemo(
        manifest_path=str(MANIFEST_PATH),
        store_path=str(store_path) if store_path else None,
        mode=mode,
    )
    plugin._send = lambda msg: None
    return with_settings(plugin, settings)


def test_store_path_setting_overrides_manifest_env_and_constructor(tmp_path, monkeypatch):
    monkeypatch.setenv("OPENCAPX_THINGS_STORE", str(tmp_path / "env-store.json"))
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    manifest["storePath"] = str(tmp_path / "manifest-store.json")
    mp = tmp_path / "opencapx-plugin.json"
    mp.write_text(json.dumps(manifest), encoding="utf-8")

    target = tmp_path / "setting-store.json"
    p = ThingsDemo(
        manifest_path=str(mp),
        store_path=str(tmp_path / "ctor-store.json"),
        mode="demo",
    )
    p._send = lambda msg: None
    with_settings(p, {"store_path": str(target)})
    ok(p, "things.add", {"title": "from-setting"})

    assert target.exists(), "setting store_path must win"
    assert not (tmp_path / "manifest-store.json").exists()
    assert not (tmp_path / "env-store.json").exists()
    assert not (tmp_path / "ctor-store.json").exists()
    assert "store_path" in p.requested_config_keys


def test_manifest_store_path_still_wins_over_env_without_setting(tmp_path, monkeypatch):
    # Regression guard: the legacy manifest → env chain is untouched.
    monkeypatch.setenv("OPENCAPX_THINGS_STORE", str(tmp_path / "env-store.json"))
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    manifest["storePath"] = str(tmp_path / "manifest-store.json")
    mp = tmp_path / "opencapx-plugin.json"
    mp.write_text(json.dumps(manifest), encoding="utf-8")
    p = ThingsDemo(manifest_path=str(mp), mode="demo")
    p._send = lambda msg: None
    p = with_settings(p, {})
    ok(p, "things.add", {"title": "from-manifest"})
    assert (tmp_path / "manifest-store.json").exists()
    assert not (tmp_path / "env-store.json").exists()


def test_store_path_change_takes_effect_without_restart(tmp_path, monkeypatch):
    first = tmp_path / "first.json"
    second = tmp_path / "second.json"
    p = _fresh(monkeypatch, tmp_path, {"store_path": str(first)})
    ok(p, "things.add", {"title": "A"})
    assert first.exists()

    p.settings_values["store_path"] = str(second)
    ok(p, "things.add", {"title": "B"})
    assert second.exists()
    assert ok(p, "things.list", {"list": "all"})["todos"][0]["title"] == "B"


def test_mode_setting_selects_live_backend_and_flips_without_restart(tmp_path, monkeypatch):
    monkeypatch.setenv("OPENCAPX_THINGS_MODE", "demo")
    p = _fresh(monkeypatch, tmp_path, {"mode": "live"}, mode="demo")
    assert p._resolve_mode() == "live"
    assert p.backend().mode == "live"

    p.settings_values["mode"] = "demo"
    assert p.backend().mode == "demo"
    assert "mode" in p.requested_config_keys


def test_mode_setting_overrides_env(tmp_path, monkeypatch):
    p = _fresh(monkeypatch, tmp_path, {"mode": "demo"})
    monkeypatch.setenv("OPENCAPX_THINGS_MODE", "live")
    monkeypatch.setattr(things_demo.sys, "platform", "darwin")
    assert p._resolve_mode() == "demo"


def test_mode_env_fallback_still_works(tmp_path, monkeypatch):
    p = _fresh(monkeypatch, tmp_path, {})
    monkeypatch.setenv("OPENCAPX_THINGS_MODE", "live")
    monkeypatch.setattr(things_demo.sys, "platform", "darwin")
    assert p._resolve_mode() == "live"


def _live_open_urls(opened):
    return [c[1] for c in opened if c[0] == "open"]


def test_auth_token_setting_is_read_from_secret_key(tmp_path, monkeypatch):
    p = _fresh(monkeypatch, tmp_path, {"secret:auth_token": "setting-token"}, mode="live")
    assert p._resolve_auth_token() == "setting-token"
    assert "secret:auth_token" in p.requested_config_keys


def test_auth_token_setting_beats_env_in_live_update(tmp_path, monkeypatch):
    monkeypatch.setenv("THINGS_AUTH_TOKEN", "env-token")
    opened = []
    monkeypatch.setattr(
        things_demo.subprocess, "run", lambda cmd, **kw: opened.append(cmd)
    )
    p = _fresh(monkeypatch, tmp_path, {"secret:auth_token": "setting-token"}, mode="live")
    todo_id = ok(p, "things.add", {"title": "T"})["id"]
    ok(p, "things.update", {"id": todo_id, "title": "T2"})

    updates = [u for u in _live_open_urls(opened) if u.startswith("things:///update")]
    assert updates, opened
    assert "auth-token=setting-token" in updates[0]
    assert "auth-token=env-token" not in updates[0]


def test_auth_token_env_fallback_still_works(tmp_path, monkeypatch):
    opened = []
    monkeypatch.setattr(
        things_demo.subprocess, "run", lambda cmd, **kw: opened.append(cmd)
    )
    p = _fresh(monkeypatch, tmp_path, {}, mode="live")
    monkeypatch.setenv("THINGS_AUTH_TOKEN", "env-token")
    todo_id = ok(p, "things.add", {"title": "T"})["id"]
    ok(p, "things.update", {"id": todo_id, "title": "T2"})

    updates = [u for u in _live_open_urls(opened) if u.startswith("things:///update")]
    assert updates and "auth-token=env-token" in updates[0]


def test_live_update_without_token_skips_url(tmp_path, monkeypatch):
    opened = []
    monkeypatch.setattr(
        things_demo.subprocess, "run", lambda cmd, **kw: opened.append(cmd)
    )
    p = _fresh(monkeypatch, tmp_path, {}, mode="live")
    todo_id = ok(p, "things.add", {"title": "T"})["id"]
    result = ok(p, "things.update", {"id": todo_id, "title": "T2"})
    assert result["via"].startswith("demo (no THINGS_AUTH_TOKEN")
    assert not [u for u in _live_open_urls(opened) if u.startswith("things:///update")]


def test_settings_read_failure_falls_back_to_legacy_sources(tmp_path, monkeypatch):
    def boom():
        raise OSError("no core")

    monkeypatch.setattr("opencapx_sdk.plugin.read_message", boom)
    p = ThingsDemo(manifest_path=str(MANIFEST_PATH), store_path=str(tmp_path / "s.json"))
    p._send = lambda msg: None
    assert p._resolve_mode() == "demo"
    assert p._resolve_store_path() == tmp_path / "s.json"
    assert ok(p, "things.add", {"title": "resilient"})["via"] == "demo"


def test_real_sdk_config_get_roundtrip_drives_settings(tmp_path, monkeypatch):
    """Exercise the real SDK config_get against Core's bare-value reply shape."""
    setting_store = tmp_path / "setting-store.json"
    p = ThingsDemo(manifest_path=str(MANIFEST_PATH), store_path=str(tmp_path / "ctor.json"))
    sent = []
    p._send = sent.append
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO(
            '{"jsonrpc":"2.0","id":"p1","result":"live"}\n'
            f'{{"jsonrpc":"2.0","id":"p2","result":"{setting_store}"}}\n'
        ),
    )

    assert p._resolve_mode() == "live"
    assert p._resolve_store_path() == setting_store
    requests = [m for m in sent if m.get("method") == "config.get"]
    assert [r["params"]["key"] for r in requests] == ["mode", "store_path"]


def test_real_sdk_config_get_reads_secret_prefixed_key(tmp_path, monkeypatch):
    p = ThingsDemo(manifest_path=str(MANIFEST_PATH), store_path=str(tmp_path / "s.json"))
    sent = []
    p._send = sent.append
    monkeypatch.setattr(
        sys,
        "stdin",
        io.StringIO('{"jsonrpc":"2.0","id":"p1","result":"tok-from-keychain"}\n'),
    )

    assert p._resolve_auth_token() == "tok-from-keychain"
    assert sent[0]["params"]["key"] == "secret:auth_token"


# --------------------------------------------------------------------------- #
# Manifest
# --------------------------------------------------------------------------- #

def test_manifest_declares_expected_contract():
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    assert manifest["id"] == "com.opencapx.things-demo"
    assert manifest["version"] == "0.1.0"
    assert manifest["apiVersion"] == "1"
    assert manifest["type"] == "capability"
    assert set(manifest["capabilities"]) == EXPECTED_CAPABILITIES
    assert set(manifest["permissions"]) == {"url.scheme.open", "things.read", "things.write"}
    assert manifest["runtime"] == {
        "type": "process",
        "command": "python3",
        "args": ["bin/things_demo.py"],
    }
    assert (PLUGIN_DIR / manifest["runtime"]["args"][0]).is_file()


def test_manifest_settings_schema_is_install_valid():
    """Mirror core::plugin::validate_manifest's settings[] rules."""
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    settings = manifest["settings"]
    assert len(settings) <= 32
    keys = [s["key"] for s in settings]
    assert len(keys) == len(set(keys)), "keys must be unique"
    legal_types = {
        "toggle", "text", "textarea", "number",
        "dropdown", "secret", "path", "button",
    }
    for s in settings:
        assert re.fullmatch(r"[a-z][a-z0-9_-]{0,63}", s["key"]), s["key"]
        assert s["type"] in legal_types, s["type"]

    by_key = {s["key"]: s for s in settings}
    assert set(by_key) == {"mode", "store_path", "auth_token"}

    mode = by_key["mode"]
    assert mode["type"] == "dropdown"
    assert mode["options"], "dropdown requires options[]"
    assert mode["default"] in mode["options"]

    assert by_key["store_path"]["type"] == "path"
    assert "default" not in by_key["store_path"]

    assert by_key["auth_token"]["type"] == "secret"
    assert "default" not in by_key["auth_token"], "secret must not declare default"
