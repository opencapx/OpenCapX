#!/usr/bin/env python3
"""OpenCapX example capability plugin (echo vision).

Rewritten on top of opencapx_sdk (see packages/plugin-sdk/).
image.analyze echoes the input back — enough to validate the whole
MCP → Core → Capability Router → Plugin pipeline without an API key.
"""
import sys
from pathlib import Path

# First try a direct import (SDK already pip-installed, or PYTHONPATH already set);
# otherwise walk up to the repo root's packages/plugin-sdk. This walk-up is
# robust whether installed to a temp dir (tests) or a system path (production).
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


class EchoVision(Plugin):
    @capability("image.analyze")
    def analyze(self, params):
        image = params.get("image", "")
        question = params.get("question", "")
        description = f"[echo] received image: {image}"
        if question:
            description += f" (question: {question})"
        return {
            "description": description,
            "text": "",
            "objects": [],
        }

    # Phase 37 — self-check endpoint. Core calls this after installing the plugin to confirm the
    # RPC handshake works; not implementing it or timing out is recorded as a probe failure and the plugin does not enter lifecycle.
    @method("core.probe.capability")
    def probe(self, params):
        return {"ok": True, "via": "echo-vision", "echoParams": params}

    # Declarative setting's list control: the plugin is the sole owner of tags, persisted via config;
    # the host only forwards the op and returns the returned array to the UI as-is.
    @method("settings.tags")
    def settings_tags(self, params):
        """op: list | add(value) | delete(index) | move(index, to); every op returns the current array.

        An unknown op raises: the SDK turns it into a JSON-RPC error and the host side gets an Err —
        a misspelled op must not be treated as a "did nothing" success. An add with an empty/whitespace-only value does not append a placeholder item.
        """
        tags = self.config_get("tags", []) or []
        op = params.get("op", "list")
        if op == "add":
            value = str(params.get("value", "")).strip()
            if value:
                tags.append(value)
        elif op == "delete":
            i = int(params.get("index", -1))
            if 0 <= i < len(tags):
                tags.pop(i)
        elif op == "move":
            i, j = int(params.get("index", -1)), int(params.get("to", -1))
            if 0 <= i < len(tags) and 0 <= j <= len(tags):
                tags.insert(j, tags.pop(i))
        elif op != "list":
            raise ValueError(f"unknown op: {op}")
        self.config_set("tags", tags)
        return tags


if __name__ == "__main__":
    EchoVision(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()