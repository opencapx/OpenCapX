#!/usr/bin/env python3
"""OpenCapX plugin template: a minimal capability plugin (based on opencapx_sdk).

Implements image.analyze (a built-in capability name) as a placeholder; replace it with your own capability and logic:
- use a **built-in capability name** (see docs/capability.md), or
- declare a new-domain capability in object form in this directory's manifest (see docs/permission-domains.md).

Parameters come from the manifest's `settings[]` (declarative form); at runtime read them with `_setting` below,
re-read on every capability call, so saving on the settings page takes effect without restarting the plugin.
"""
import sys
from pathlib import Path
from typing import Any

try:
    from opencapx_sdk import Plugin, capability, method  # type: ignore
except ImportError:
    _here = Path(__file__).resolve()
    for _parent in [_here, *_here.parents]:
        _candidate = _parent / "packages" / "plugin-sdk"
        if (_candidate / "opencapx_sdk" / "__init__.py").is_file():
            sys.path.insert(0, str(_candidate))
            break
    from opencapx_sdk import Plugin, capability, method  # type: ignore


# List settings: declare {"key": "tags", "type": "list"} in the manifest and
# implement @method("settings.tags") to receive {"op": "list"|"add"|"delete"|"move", ...}.
# Return the current array from every op — see docs/plugin-authoring.md §5.10.
class MyPlugin(Plugin):
    # -- declarative settings (manifest `settings[]` → config.*, read per call) --

    def _setting(self, key: str, default: Any = None) -> Any:
        """Read a declarative setting; a failed reverse call (no Core / protocol error) must never fail the capability."""
        try:
            value = self.config_get(key, default)
        except Exception:  # noqa: BLE001 - a settings read must never fail a capability
            return default
        return default if value is None else value

    def _setting_str(self, key: str) -> str:
        value = self._setting(key, "")
        return value.strip() if isinstance(value, str) else ""

    @capability("image.analyze")
    def analyze(self, params):
        # Re-read on every call: when the settings page saves, Core writes config / keychain directly without restarting the plugin.
        prefix = self._setting_str("response_prefix") or "template"
        detail = self._setting_str("detail_level")
        include_notice = bool(self._setting("include_notice", True))
        notice = self._setting_str("notice")
        # For secrets, only existence is read; the value never enters the result (storage key is `secret:`-prefixed, keychain-backed).
        token_set = bool(self._setting_str("secret:api_token"))

        description = f"[{prefix}] got {params.get('image', '')}"
        if include_notice and notice:
            description += f" — {notice}"

        result = {
            "description": description,
            "text": "",
            "objects": [],
        }
        if detail == "detailed":
            result["text"] = (
                f"detail_level={detail}; include_notice={include_notice}; "
                f"api_token={'set' if token_set else 'unset'}"
            )
        return result

    @method("core.probe.capability")
    def probe(self, params):
        return {"ok": True, "via": "plugin-template"}


if __name__ == "__main__":
    MyPlugin(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()
