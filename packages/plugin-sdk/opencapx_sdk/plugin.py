"""OpenCapX Plugin base class: owns the JSON-RPC loop and reverse calls; subclasses only write business logic.

Minimal usage:
    class EchoVision(Plugin):
        async def on_initialize(self, params):
            return {"pluginId": self.manifest["id"], "version": self.manifest["version"]}

        @capability("image.analyze")
        async def analyze(self, params):
            return {"description": f"[echo] {params.get('image','')}"}

    EchoVision().run()

Reverse calls:
    - self.request_permission(permission, reason="...") -> bool
    - self.log(level, message)
    - self.emit(kind, payload)
    - self.config_get(key, default=None)
    - self.config_set(key, value)
"""

from __future__ import annotations

import json
import sys
from typing import Any, Awaitable, Callable, Optional, Union

from .manifest import load_manifest
from .protocol import (
    ProtocolError,
    build_error,
    build_notification,
    build_request,
    build_result,
    read_message,
    write_message,
)


# JSON-RPC standard error codes
ERR_PARSE = -32700
ERR_INVALID_REQUEST = -32600
ERR_METHOD_NOT_FOUND = -32601
ERR_INVALID_PARAMS = -32602
ERR_INTERNAL = -32603


Handler = Callable[[dict[str, Any]], Union[Any, Awaitable[Any]]]


def capability(method: str) -> Callable[[Handler], Handler]:
    """Decorator: mark a method as a capability handler. Plugin.handle() dispatches automatically."""
    def deco(fn: Handler) -> Handler:
        fn.__opencapx_capability__ = method  # type: ignore[attr-defined]
        return fn
    return deco


def method(method_name: str) -> Callable[[Handler], Handler]:
    """Decorator: mark a method as an arbitrary RPC method (not a capability).

    Phase 37 — for core self-check. The typical case is `core.probe.capability`: core sends empty
    params to probe the plugin handshake, and the plugin should reply `{"ok": true, ...}`. Not
    implementing it / timing out both count as unhealthy.
    """
    def deco(fn: Handler) -> Handler:
        fn.__opencapx_method__ = method_name  # type: ignore[attr-defined]
        return fn
    return deco


class Plugin:
    """Plugin base class.

    Subclasses implement these lifecycle hooks (all optional):
        on_initialize(params) -> dict
        on_shutdown(params) -> None
        on_ping(params) -> dict  # returns {"ok": True} by default
        on_plugin_call(method, params) -> Any  # fallback

    Capability handlers are decorated with @capability("..."); the method name must equal the capability id.
    """

    def __init__(
        self,
        manifest_path: Optional[str] = None,
        manifest: Optional[dict[str, Any]] = None,
        core_version: str = "0.0.0",
        plugin_id: Optional[str] = None,
    ) -> None:
        if manifest is not None:
            self.manifest = manifest
        elif manifest_path is not None:
            self.manifest = load_manifest(manifest_path)
        else:
            # Deferred: some plugins (e.g. pet-blank) need no strict manifest; only fill in plugin_id
            self.manifest = {"id": plugin_id or "unknown", "version": "0.0.0", "apiVersion": "1"}
        self.plugin_id: str = self.manifest.get("id", plugin_id or "unknown")
        self.core_version = core_version
        # F9: version that last started successfully (injected as initialize's previousVersion; None on first start).
        self.previous_version: Optional[str] = None
        self._next_id = 1
        self._capabilities: dict[str, Handler] = {}
        self._methods: dict[str, Handler] = {}
        for name in dir(self):
            fn = getattr(self, name, None)
            if not callable(fn):
                continue
            if hasattr(fn, "__opencapx_capability__"):
                self._capabilities[fn.__opencapx_capability__] = fn
            if hasattr(fn, "__opencapx_method__"):
                self._methods[fn.__opencapx_method__] = fn

    # ---------- reverse calls ----------

    def _next_request_id(self) -> str:
        i = self._next_id
        self._next_id += 1
        return f"p{i}"

    def _send(self, msg: dict[str, Any]) -> None:
        write_message(msg)

    def log(self, level: str, message: str) -> None:
        """core.log(level, message) notification."""
        self._send(build_notification("core.log", {"level": level, "message": message}))

    def emit(self, kind: str, payload: Any) -> None:
        """core.emit({type, payload}) notification. It is recommended that kind be prefixed with the plugin id."""
        self._send(build_notification("core.emit", {"type": kind, "payload": payload}))

    def request_permission(
        self,
        permission: str,
        reason: str = "",
        timeout: float = 65.0,
    ) -> bool:
        """core.requestPermission(permission, reason) blocks synchronously waiting for a reply."""
        req_id = self._next_request_id()
        params: dict[str, str] = {"permission": permission}
        if reason:
            params["reason"] = reason
        self._send(build_request("core.requestPermission", params, req_id))
        # Read the reply inside the run() loop; this simple implementation reuses read_message until the id matches
        deadline = timeout
        while deadline > 0:
            msg = read_message()
            if msg is None:
                return False
            # This message may belong to another thread; a collision is almost impossible in the main thread's serial loop
            if msg.get("id") == req_id and "result" in msg:
                return bool(msg["result"].get("granted"))
            if msg.get("id") == req_id and "error" in msg:
                return False
            # Not our reply, hand it back to handle() (a bit involved): the base class assumes a single thread
        return False

    def config_get(self, key: str, default: Any = None) -> Any:
        """Reverse config.get {key, default}, blocks synchronously.

        Core's reverse handler replies with the **bare** stored value
        (`{"result": <value>}`), not a `{"value": ...}` envelope — see
        `core::config::dispatch` / `handle_reverse`. Pass `key="secret:<name>"`
        to read a keychain-backed secret back (owner-only plaintext).
        """
        req_id = self._next_request_id()
        self._send(build_request("config.get", {"key": key, "default": default}, req_id))
        msg = read_message()
        if msg is None or msg.get("id") != req_id:
            return default
        if "result" in msg:
            return msg["result"]
        return default

    def config_set(self, key: str, value: Any) -> bool:
        req_id = self._next_request_id()
        self._send(build_request("config.set", {"key": key, "value": value}, req_id))
        msg = read_message()
        return bool(msg and msg.get("id") == req_id and "result" in msg)

    # ---------- lifecycle hooks (subclasses override) ----------

    def on_initialize(self, params: dict[str, Any]) -> dict[str, Any]:
        # F9: injected version that last started successfully → plugins use it for idempotent data migration (the host does not move data).
        prev = params.get("previousVersion")
        self.previous_version = prev if isinstance(prev, str) else None
        # Must return the @capability-registered methods as the capabilities array —
        # the Core side's handshake uses it to register the plugin into the Capability Registry (i1.md §16).
        # The version field uses the manifest default "1" (it is a string in the protocol, not semver).
        return {
            "pluginId": self.plugin_id,
            "version": self.manifest.get("version", "0.0.0"),
            "apiVersion": self.manifest.get("apiVersion", "1"),
            "capabilities": [
                {"id": cap_id, "version": "1"}
                for cap_id in sorted(self._capabilities.keys())
            ],
        }

    def on_shutdown(self, params: dict[str, Any]) -> None:
        return None

    def on_ping(self, params: dict[str, Any]) -> dict[str, Any]:
        return {"ok": True}

    # ---------- main loop ----------

    def _make_response(self, req_id: Any, result: Any) -> dict[str, Any]:
        return build_result(req_id, result)

    def _make_error(self, req_id: Any, code: int, message: str) -> dict[str, Any]:
        return build_error(req_id, code, message)

    def _dispatch(self, method: str, params: dict[str, Any]) -> Any:
        if method == "plugin.initialize":
            return self.on_initialize(params)
        if method == "plugin.shutdown":
            self.on_shutdown(params)
            return None
        if method == "plugin.ping":
            return self.on_ping(params)
        # capability-decorated handlers
        if method in self._capabilities:
            return self._capabilities[method](params)
        # Phase 37 — any @method-registered RPC handler (e.g. core.probe.capability)
        if method in self._methods:
            return self._methods[method](params)
        raise LookupError(f"method not found: {method}")

    def handle(self, msg: dict[str, Any]) -> Optional[dict[str, Any]]:
        """Handle a single JSON-RPC message. Returns a response (or None for a notification).

        Unit tests can bypass run() and call this method directly with mocked stdin/stdout.
        """
        if msg.get("jsonrpc") != "2.0":
            return self._make_error(msg.get("id"), ERR_INVALID_REQUEST, "jsonrpc must be 2.0")
        method = msg.get("method")
        params = msg.get("params") or {}
        req_id = msg.get("id")  # notification has no id
        if not isinstance(method, str) or not method:
            if req_id is not None:
                return self._make_error(req_id, ERR_INVALID_REQUEST, "missing method")
            return None
        try:
            result = self._dispatch(method, params)
        except LookupError:
            if req_id is not None:
                return self._make_error(req_id, ERR_METHOD_NOT_FOUND, f"method not found: {method}")
            return None
        except Exception as e:  # noqa: BLE001
            if req_id is not None:
                return self._make_error(req_id, ERR_INTERNAL, f"{type(e).__name__}: {e}")
            return None
        if req_id is None:
            return None  # notification
        return self._make_response(req_id, result)

    def run(self) -> int:
        """Main loop: read JSON-RPC from stdin, handle it, write to stdout. Exit code 0."""
        while True:
            try:
                msg = read_message()
            except ProtocolError:
                continue  # skip malformed lines
            if msg is None:
                return 0
            method = msg.get("method")
            if method == "plugin.shutdown":
                # shutdown is a notification; a None response is fine
                self.handle(msg)
                return 0
            resp = self.handle(msg)
            if resp is not None:
                self._send(resp)
        return 0


__all__ = ["Plugin", "capability"]