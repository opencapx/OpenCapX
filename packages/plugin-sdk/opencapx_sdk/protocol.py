"""JSON-RPC 2.0 over stdio framing for OpenCapX plugins.

One JSON object per line, UTF-8, no BOM, `\\n`-separated.
stdout carries only protocol; stderr is left for logs.

See docs/plugin-protocol.md.
"""

from __future__ import annotations

import json
import sys
from typing import Any, Callable, Iterable, Iterator, Optional, TextIO


class ProtocolError(Exception):
    """Raised when a JSON-RPC frame/field is invalid."""


def write_message(msg: dict[str, Any], out: Optional[TextIO] = None) -> None:
    """Serialize + write one line of JSON to stdout (default) or the given stream."""
    stream = out if out is not None else sys.stdout
    stream.write(json.dumps(msg, ensure_ascii=False) + "\n")
    stream.flush()


def read_message(
    lines: Optional[Iterable[str]] = None,
    line_factory: Optional[Callable[[], str]] = None,
) -> Optional[dict[str, Any]]:
    """Read and parse one line of a JSON-RPC message. Returning None means EOF.

    lines: an iterable of strings (defaults to sys.stdin).
    line_factory: a callable that returns one line per call. If neither is passed, read line by line from sys.stdin.

    EOF semantics:
      - From sys.stdin: `readline()` returning "" = EOF; a line that strips to empty = skipped.
      - From iterating lines: StopIteration = EOF; an empty-string element = skipped.
      - line_factory: decided by the caller; returning "" is always treated as EOF (simple behavior).
    """
    use_iter: Optional[Iterator[str]] = None
    if line_factory is not None:
        getter: Callable[[], str] = line_factory
        stdin_mode = False
    elif lines is not None:
        use_iter = iter(lines)
        stdin_mode = False
    else:
        stdin_mode = True

    while True:
        if use_iter is not None:
            try:
                raw = next(use_iter)
            except StopIteration:
                return None
            stripped = raw.strip()
            if not stripped:
                continue  # skip empty/whitespace-only lines
            try:
                return json.loads(stripped)
            except json.JSONDecodeError as e:
                raise ProtocolError(f"malformed json: {e.msg} (line: {stripped[:80]!r})") from e
        elif stdin_mode:
            raw = sys.stdin.readline()
            if not raw:
                return None  # sys.stdin EOF
            stripped = raw.strip()
            if not stripped:
                continue
            try:
                return json.loads(stripped)
            except json.JSONDecodeError as e:
                raise ProtocolError(f"malformed json: {e.msg} (line: {stripped[:80]!r})") from e
        else:
            raw = getter()
            if not raw:
                return None
            stripped = raw.strip()
            if not stripped:
                continue
            try:
                return json.loads(stripped)
            except json.JSONDecodeError as e:
                raise ProtocolError(f"malformed json: {e.msg} (line: {stripped[:80]!r})") from e


def build_result(req_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": req_id, "result": result}


def build_error(req_id: Any, code: int, message: str, data: Any = None) -> dict[str, Any]:
    err: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        err["data"] = data
    return {"jsonrpc": "2.0", "id": req_id, "error": err}


def build_request(method: str, params: Any, req_id: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": req_id, "method": method, "params": params}


def build_notification(method: str, params: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "method": method, "params": params}