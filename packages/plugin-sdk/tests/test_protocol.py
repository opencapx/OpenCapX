"""Unit tests for opencapx_sdk.protocol (JSON-RPC framing)."""
import io
import json
import pytest

from opencapx_sdk.protocol import (
    ProtocolError,
    build_error,
    build_notification,
    build_request,
    build_result,
    read_message,
    write_message,
)


def test_write_message_produces_single_line_json():
    buf = io.StringIO()
    write_message({"a": 1, "b": [1, 2]}, out=buf)
    raw = buf.getvalue()
    assert raw.endswith("\n")
    parsed = json.loads(raw.strip())
    assert parsed == {"a": 1, "b": [1, 2]}


def test_write_message_flushes():
    buf = io.StringIO()
    write_message({"x": 1}, out=buf)
    assert buf.getvalue() != ""  # flushed


def test_read_message_parses_valid_line():
    msg = read_message(lines=['{"jsonrpc":"2.0","id":1,"method":"ping"}'])
    assert msg == {"jsonrpc": "2.0", "id": 1, "method": "ping"}


def test_read_message_skips_blank_lines():
    msg = read_message(lines=["", "   ", '{"id":1}'])
    assert msg == {"id": 1}


def test_read_message_returns_none_on_eof():
    assert read_message(lines=[]) is None
    assert read_message(lines=iter([])) is None


def test_read_message_raises_on_malformed_json():
    with pytest.raises(ProtocolError):
        read_message(lines=["not json at all"])


def test_build_result_shape():
    r = build_result("p1", {"ok": True})
    assert r == {"jsonrpc": "2.0", "id": "p1", "result": {"ok": True}}


def test_build_error_minimal():
    e = build_error("p1", -32601, "not found")
    assert e == {"jsonrpc": "2.0", "id": "p1", "error": {"code": -32601, "message": "not found"}}


def test_build_error_with_data():
    e = build_error("p1", -32603, "boom", data={"trace": "x"})
    assert e["error"]["data"] == {"trace": "x"}


def test_build_request_has_id():
    r = build_request("core.log", {"level": "info"}, "p7")
    assert r == {"jsonrpc": "2.0", "id": "p7", "method": "core.log", "params": {"level": "info"}}


def test_build_notification_omits_id():
    n = build_notification("core.log", {"level": "info"})
    assert n == {"jsonrpc": "2.0", "method": "core.log", "params": {"level": "info"}}
    assert "id" not in n


def test_round_trip_through_io():
    buf = io.StringIO()
    write_message({"jsonrpc": "2.0", "id": 1, "method": "plugin.ping"}, out=buf)
    raw = buf.getvalue().rstrip("\n")
    msg = read_message(lines=[raw])
    assert msg is not None
    assert msg["method"] == "plugin.ping"