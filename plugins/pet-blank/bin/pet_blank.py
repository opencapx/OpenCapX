#!/usr/bin/env python3
"""Minimal pet plugin.Receives plugin.initialize,emits a heartbeat animation event every 5s."""
import sys
import threading
import time
from pathlib import Path

try:
    from opencapx_sdk import Plugin  # type: ignore
except ImportError:
    _here = Path(__file__).resolve()
    for _parent in [_here, *_here.parents]:
        _candidate = _parent / "packages" / "plugin-sdk"
        if (_candidate / "opencapx_sdk" / "__init__.py").is_file():
            sys.path.insert(0, str(_candidate))
            break
    from opencapx_sdk import Plugin  # type: ignore  # noqa: E402


class PetBlank(Plugin):
    def on_initialize(self, params):
        self.log("info", "pet-blank started")
        self.emit("animation", {"name": "idle", "frame": 0})
        self._spawn_heartbeat()
        return super().on_initialize(params)

    def _spawn_heartbeat(self):
        t = threading.Thread(target=self._heartbeat, daemon=True)
        t.start()

    def _heartbeat(self):
        frame = 0
        while True:
            time.sleep(5)
            frame += 1
            self.emit("animation", {"name": "idle", "frame": frame})


if __name__ == "__main__":
    PetBlank(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()
