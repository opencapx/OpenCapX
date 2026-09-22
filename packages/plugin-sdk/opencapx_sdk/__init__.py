"""OpenCapX plugin SDK (Python reference).

Public API:
    Plugin           — base class with stdio loop + lifecycle hooks
    capability       — decorator for capability handlers
    method           — decorator for arbitrary RPC methods (e.g. core.probe.capability)
    read_message     — raw JSON-RPC line reader
    write_message    — raw JSON-RPC line writer
    load_manifest    — parse + validate opencapx-plugin.json
    ManifestError    — manifest parse/validate error
    ProtocolError    — JSON-RPC framing error
"""

from .plugin import Plugin, capability, method
from .protocol import (
    ProtocolError,
    read_message,
    write_message,
)
from .manifest import ManifestError, load_manifest

__all__ = [
    "Plugin",
    "capability",
    "method",
    "read_message",
    "write_message",
    "load_manifest",
    "ManifestError",
    "ProtocolError",
]
