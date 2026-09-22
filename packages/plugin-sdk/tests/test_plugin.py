"""Unit tests for opencapx_sdk.Plugin base class."""
import io
import json

import pytest

from opencapx_sdk import Plugin, ManifestError, capability, load_manifest
from opencapx_sdk.plugin import (
    ERR_INTERNAL,
    ERR_INVALID_REQUEST,
    ERR_METHOD_NOT_FOUND,
)


# ----- construction and manifest -----

def test_plugin_from_manifest_dict():
    p = Plugin(manifest={
        "id": "com.x",
        "name": "X",
        "version": "0.1.0",
        "apiVersion": "1",
        "type": "capability",
        "capabilities": ["echo"],
        "runtime": {"type": "process", "command": "x"},
    })
    assert p.plugin_id == "com.x"
    # Bare Plugin (no subclass) declares no @capability methods; only subclasses register them.
    assert p._capabilities == {}


def test_plugin_default_manifest_when_none_given():
    p = Plugin(plugin_id="com.fallback")
    assert p.plugin_id == "com.fallback"


def test_load_manifest_rejects_missing_file(tmp_path):
    with pytest.raises(ManifestError):
        load_manifest(tmp_path / "nope.json")


def test_load_manifest_rejects_bad_json(tmp_path):
    f = tmp_path / "m.json"
    f.write_text("{not json")
    with pytest.raises(ManifestError):
        load_manifest(f)


def test_load_manifest_rejects_missing_required(tmp_path):
    f = tmp_path / "m.json"
    f.write_text(json.dumps({"id": "x", "name": "X"}))
    with pytest.raises(ManifestError, match="version"):
        load_manifest(f)


def test_load_manifest_rejects_bad_api_version(tmp_path):
    f = tmp_path / "m.json"
    f.write_text(json.dumps({
        "id": "x", "name": "X", "version": "0.1.0", "apiVersion": "9", "type": "capability",
        "capabilities": ["a"], "runtime": {"type": "process", "command": "x"},
    }))
    with pytest.raises(ManifestError, match="apiVersion"):
        load_manifest(f)


def test_load_manifest_rejects_capability_without_runtime(tmp_path):
    f = tmp_path / "m.json"
    f.write_text(json.dumps({
        "id": "x", "name": "X", "version": "0.1.0", "apiVersion": "1", "type": "capability",
        "capabilities": ["a"],
    }))
    with pytest.raises(ManifestError, match="runtime"):
        load_manifest(f)


def _cap_manifest(tmp_path, caps):
    f = tmp_path / "timeout-plugin.json"
    f.write_text(json.dumps({
        "id": "com.example.slow", "name": "Slow", "version": "0.1.0", "apiVersion": "1",
        "type": "capability",
        "capabilities": caps,
        "runtime": {"type": "process", "command": "python3", "args": ["bin/plugin.py"]},
        "permissions": ["demo.run"],
    }))
    return f


def test_manifest_accepts_timeout_secs_bounds(tmp_path):
    for t in (None, 1, 600):
        cap = {"id": "demo.slow", "permission": "demo.run"}
        if t is not None:
            cap["timeoutSecs"] = t
        load_manifest(_cap_manifest(tmp_path, [cap]))


def test_manifest_rejects_bad_timeout_secs(tmp_path):
    for bad in (0, 601, 1.5, True, "60"):
        cap = {"id": "demo.slow", "permission": "demo.run", "timeoutSecs": bad}
        with pytest.raises(ManifestError, match="timeoutSecs"):
            load_manifest(_cap_manifest(tmp_path, [cap]))


def test_load_manifest_accepts_pet_type(tmp_path):
    f = tmp_path / "m.json"
    f.write_text(json.dumps({
        "id": "x", "name": "X", "version": "0.1.0", "apiVersion": "1", "type": "pet",
    }))
    m = load_manifest(f)
    assert m["type"] == "pet"


# ----- handle() behavior -----

class _Echo(Plugin):
    def __init__(self):
        super().__init__(manifest={
            "id": "com.echo", "name": "Echo", "version": "0.1.0",
            "apiVersion": "1", "type": "capability",
            "capabilities": ["echo"],
            "runtime": {"type": "process", "command": "x"},
        })

    @capability("echo")
    def echo(self, params):
        return {"got": params}


def test_handle_dispatches_to_capability():
    p = _Echo()
    resp = p.handle({"jsonrpc": "2.0", "id": 1, "method": "echo", "params": {"hi": 1}})
    assert resp == {"jsonrpc": "2.0", "id": 1, "result": {"got": {"hi": 1}}}


def test_handle_returns_none_for_notification():
    p = _Echo()
    resp = p.handle({"jsonrpc": "2.0", "method": "core.log", "params": {"level": "info", "message": "x"}})
    assert resp is None


def test_handle_returns_method_not_found_for_unknown():
    p = _Echo()
    resp = p.handle({"jsonrpc": "2.0", "id": 7, "method": "nope.method"})
    assert resp is not None
    assert resp["error"]["code"] == ERR_METHOD_NOT_FOUND


def test_handle_validates_jsonrpc_version():
    p = _Echo()
    resp = p.handle({"jsonrpc": "1.0", "id": 1, "method": "echo"})
    assert resp["error"]["code"] == ERR_INVALID_REQUEST


def test_handle_returns_internal_error_on_exception():
    class Boom(Plugin):
        def __init__(self):
            super().__init__(manifest={
                "id": "com.boom", "name": "Boom", "version": "0.1.0",
                "apiVersion": "1", "type": "capability",
                "capabilities": ["boom"],
                "runtime": {"type": "process", "command": "x"},
            })

        @capability("boom")
        def boom(self, params):
            raise RuntimeError("kaboom")

    p = Boom()
    resp = p.handle({"jsonrpc": "2.0", "id": 1, "method": "boom", "params": {}})
    assert resp["error"]["code"] == ERR_INTERNAL
    assert "kaboom" in resp["error"]["message"]


def test_handle_initialize_returns_default_dict():
    p = _Echo()
    resp = p.handle({"jsonrpc": "2.0", "id": 1, "method": "plugin.initialize", "params": {}})
    assert resp["result"]["pluginId"] == "com.echo"
    assert resp["result"]["apiVersion"] == "1"


def test_handle_ping_default_returns_ok():
    p = _Echo()
    resp = p.handle({"jsonrpc": "2.0", "id": 1, "method": "plugin.ping", "params": {}})
    assert resp["result"] == {"ok": True}


def test_handle_shutdown_invokes_lifecycle():
    seen = []

    class HasShutdown(Plugin):
        def __init__(self):
            super().__init__(manifest={
                "id": "com.s", "name": "S", "version": "0.1.0",
                "apiVersion": "1", "type": "pet",
            })

        def on_shutdown(self, params):
            seen.append(params)

    p = HasShutdown()
    resp = p.handle({"jsonrpc": "2.0", "method": "plugin.shutdown", "params": {}})
    assert resp is None  # notification
    assert seen == [{}]


def test_capability_decorator_registers_method():
    @capability("foo")
    def fn(params):
        return {"ok": True}
    assert getattr(fn, "__opencapx_capability__") == "foo"


def test_request_id_counter_increments():
    p = _Echo()
    a = p._next_request_id()
    b = p._next_request_id()
    assert a == "p1"
    assert b == "p2"


# ----- config_get / config_set reverse calls -----
#
# Core's `config.*` reverse handler replies with the bare stored value
# (`handle_reverse` → `{"result": value}`), not a `{"value": ...}` envelope.

def test_config_get_returns_bare_result(monkeypatch):
    p = _Echo()
    sent = []
    p._send = lambda msg: sent.append(msg)
    monkeypatch.setattr(
        "opencapx_sdk.plugin.read_message",
        lambda: {"jsonrpc": "2.0", "id": "p1", "result": "stored-value"},
    )
    assert p.config_get("mode") == "stored-value"
    assert sent[0]["method"] == "config.get"
    assert sent[0]["params"] == {"key": "mode", "default": None}


def test_config_get_secret_key_passthrough(monkeypatch):
    p = _Echo()
    sent = []
    p._send = lambda msg: sent.append(msg)
    monkeypatch.setattr(
        "opencapx_sdk.plugin.read_message",
        lambda: {"jsonrpc": "2.0", "id": "p1", "result": "tok-123"},
    )
    assert p.config_get("secret:auth_token", default="") == "tok-123"
    assert sent[0]["params"]["key"] == "secret:auth_token"


def test_config_get_falls_back_to_default_on_eof(monkeypatch):
    p = _Echo()
    p._send = lambda msg: None
    monkeypatch.setattr("opencapx_sdk.plugin.read_message", lambda: None)
    assert p.config_get("missing", default="fallback") == "fallback"


def test_config_get_returns_null_result_as_is(monkeypatch):
    # Key present with a JSON null value (no default) — must not be replaced.
    p = _Echo()
    p._send = lambda msg: None
    monkeypatch.setattr(
        "opencapx_sdk.plugin.read_message",
        lambda: {"jsonrpc": "2.0", "id": "p1", "result": None},
    )
    assert p.config_get("k", default="ignored") is None