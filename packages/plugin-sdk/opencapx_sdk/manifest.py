"""OpenCapX plugin manifest parsing and validation.

Aligns with docs/plugin-manifest.md:
    id, name, version, apiVersion, type (required)
    description, author, homepage, license, capabilities, permissions, states, runtime (optional)
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any


class ManifestError(ValueError):
    """A manifest field is missing/invalid."""


# Required fields
REQUIRED_TOP = ("id", "name", "version", "apiVersion", "type")
# Allowed type values
ALLOWED_TYPES = {"pet", "capability"}
# apiVersion currently supports only "1"
SUPPORTED_API_VERSION = "1"


def load_manifest(path: str | Path) -> dict[str, Any]:
    """Read + parse + validate a manifest. Raises ManifestError on failure."""
    p = Path(path)
    if not p.is_file():
        raise ManifestError(f"manifest not found: {p}")
    try:
        raw = json.loads(p.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        raise ManifestError(f"manifest not valid json: {e.msg}") from e
    return validate_manifest(raw)


def validate_manifest(m: dict[str, Any]) -> dict[str, Any]:
    """Validate an already-parsed dict, returning the same dict."""
    for key in REQUIRED_TOP:
        if key not in m or m[key] in (None, ""):
            raise ManifestError(f"manifest missing required field: {key}")
    if m["type"] not in ALLOWED_TYPES:
        raise ManifestError(
            f"manifest type must be one of pet | capability, got {m['type']!r}"
        )
    api = str(m["apiVersion"])
    if api != SUPPORTED_API_VERSION:
        raise ManifestError(
            f"manifest apiVersion {api!r} not supported (need {SUPPORTED_API_VERSION!r})"
        )
    # capability type must declare capabilities and runtime
    if m["type"] == "capability":
        caps = m.get("capabilities") or []
        if not isinstance(caps, list) or not caps:
            raise ManifestError("capability manifest must declare non-empty capabilities[]")
        if not m.get("runtime"):
            raise ManifestError("capability manifest must declare runtime")
        # S4 — object-form timeoutSecs (seconds, 1..=600); string form uses the default 60s.
        for item in caps:
            if isinstance(item, dict) and "timeoutSecs" in item:
                t = item["timeoutSecs"]
                if isinstance(t, bool) or not isinstance(t, int) or not (1 <= t <= 600):
                    raise ManifestError(
                        "capability %r: timeoutSecs must be an integer in 1..=600, got %r"
                        % (item.get("id", "?"), t)
                    )
    return m