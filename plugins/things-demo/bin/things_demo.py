#!/usr/bin/env python3
"""OpenCapX Things Demo plugin.

Things3-style todo CRUD exposed as five capabilities:

    things.add     create a todo
    things.update  patch an existing todo
    things.list    list todos by bucket
    things.show    fetch one todo by id, or many by query
    things.search  full-text search

Design goal: the plugin must work **without Things3 installed**, so it has two
backends (see README.md):

    DemoBackend   default; a local JSON file store (stdlib only)
    ThingsBackend opt-in live mode: builds things:/// URLs, opens them with the
                  macOS ``open`` command, and reads via JXA (osascript). Any
                  failure falls back to the demo mirror and is reported in the
                  result's ``via`` field — the plugin never crashes because
                  Things3 is missing.

The JSON-RPC stdio loop, lifecycle hooks, and reverse calls come from
``opencapx_sdk`` (packages/plugin-sdk).
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable
from urllib.parse import quote, urlencode

# First try a direct import (SDK pip-installed or PYTHONPATH already set); otherwise walk up to the
# repo root's packages/plugin-sdk. Robust whether installed to a temp dir (tests) or a system path (production).
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


# --------------------------------------------------------------------------- #
# Constants
# --------------------------------------------------------------------------- #

DEFAULT_STORE = Path.home() / ".opencapx" / "cache" / "things-demo.json"

# Things "when" buckets; a todo's when may also be a concrete YYYY-MM-DD date.
WHEN_BUCKETS = ("inbox", "today", "tomorrow", "anytime", "someday")
# Accepted values for things.list's `list` filter.
LIST_FILTERS = ("inbox", "today", "anytime", "someday", "logbook", "all")


# --------------------------------------------------------------------------- #
# Small helpers
# --------------------------------------------------------------------------- #

def _now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _is_iso_date(value: str) -> bool:
    try:
        datetime.strptime(value, "%Y-%m-%d")
        return True
    except ValueError:
        return False


def _validate_when(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError("invalid input: when must be a non-empty string")
    value = value.strip()
    if value not in WHEN_BUCKETS and not _is_iso_date(value):
        raise ValueError(
            "invalid input: when must be one of "
            + "/".join(WHEN_BUCKETS)
            + " or a YYYY-MM-DD date"
        )
    return value


def _validate_deadline(value: Any) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError("invalid input: deadline must be a non-empty string")
    value = value.strip()
    if not _is_iso_date(value):
        raise ValueError("invalid input: deadline must be a YYYY-MM-DD date")
    return value


def _validate_str_list(value: Any, field: str) -> list[str]:
    if not isinstance(value, list):
        raise ValueError(f"invalid input: {field} must be a list of strings")
    out: list[str] = []
    for item in value:
        if not isinstance(item, str) or not item.strip():
            raise ValueError(f"invalid input: {field} must only contain non-empty strings")
        out.append(item.strip())
    return out


def _filter_todos(
    todos: list[dict[str, Any]],
    list_filter: str,
    limit: int,
    include_completed: bool,
) -> list[dict[str, Any]]:
    """Bucket filter shared by both backends."""
    out: list[dict[str, Any]] = []
    for todo in todos:
        completed = bool(todo.get("completed"))
        if list_filter == "logbook":
            if not completed:
                continue
        else:
            if completed and not include_completed:
                continue
            if list_filter != "all" and list_filter not in (
                todo.get("when"),
                todo.get("list"),
            ):
                continue
        out.append(todo)
    out.sort(key=lambda t: t.get("updated_at") or "", reverse=True)
    return out[:limit]


def _search_todos(todos: list[dict[str, Any]], query: str) -> list[dict[str, Any]]:
    needle = query.lower()
    out: list[dict[str, Any]] = []
    for todo in todos:
        tags = todo.get("tags") or []
        haystack = " ".join(
            [str(todo.get("title") or ""), str(todo.get("notes") or ""), " ".join(map(str, tags))]
        ).lower()
        if needle in haystack:
            out.append(todo)
    out.sort(key=lambda t: t.get("updated_at") or "", reverse=True)
    return out


# --------------------------------------------------------------------------- #
# Backend: demo (default) — local JSON file store
# --------------------------------------------------------------------------- #

class DemoBackend:
    """Local JSON file store. Stdlib only, no Things3, no network."""

    mode = "demo"

    def __init__(self, store_path: str | os.PathLike[str]) -> None:
        self.store_path = Path(store_path)
        self.last_via = "demo"

    # -- persistence --

    def _load(self) -> dict[str, Any]:
        try:
            if self.store_path.is_file():
                data = json.loads(self.store_path.read_text(encoding="utf-8"))
                if isinstance(data, dict) and isinstance(data.get("todos"), list):
                    return data
        except (OSError, json.JSONDecodeError):
            pass
        return {"todos": []}

    def _save(self, data: dict[str, Any]) -> None:
        self.store_path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.store_path.with_suffix(self.store_path.suffix + ".tmp")
        tmp.write_text(json.dumps(data, ensure_ascii=False, indent=2), encoding="utf-8")
        tmp.replace(self.store_path)

    # -- CRUD --

    def add(self, fields: dict[str, Any]) -> dict[str, Any]:
        data = self._load()
        now = _now_iso()
        todo = {
            "id": uuid.uuid4().hex[:8],
            "title": fields["title"],
            "notes": fields.get("notes") or "",
            "when": fields.get("when") or "inbox",
            "deadline": fields.get("deadline") or None,
            "tags": list(fields.get("tags") or []),
            "list": fields.get("list") or "inbox",
            "completed": False,
            "created_at": now,
            "updated_at": now,
        }
        data["todos"].append(todo)
        self._save(data)
        self.last_via = "demo"
        return todo

    def get(self, todo_id: str) -> dict[str, Any] | None:
        for todo in self._load()["todos"]:
            if todo.get("id") == todo_id:
                return todo
        return None

    def update(self, todo_id: str, fields: dict[str, Any]) -> dict[str, Any] | None:
        data = self._load()
        target = None
        for todo in data["todos"]:
            if todo.get("id") == todo_id:
                target = todo
                break
        if target is None:
            return None

        patch = dict(fields)
        if "append_notes" in patch:
            extra = patch.pop("append_notes") or ""
            if extra:
                target["notes"] = (target.get("notes") or "") + extra
        if "add_tags" in patch:
            for tag in patch.pop("add_tags") or []:
                if tag not in target["tags"]:
                    target["tags"].append(tag)
        for key in ("title", "notes", "when", "deadline", "tags", "list", "completed"):
            if key in patch and patch[key] is not None:
                target[key] = patch[key]
        target["updated_at"] = _now_iso()
        self._save(data)
        self.last_via = "demo"
        return target

    def delete(self, todo_id: str) -> dict[str, Any] | None:
        data = self._load()
        for i, todo in enumerate(data["todos"]):
            if todo.get("id") == todo_id:
                removed = data["todos"].pop(i)
                self._save(data)
                self.last_via = "demo"
                return removed
        return None

    def list_todos(
        self, list_filter: str, limit: int, include_completed: bool
    ) -> list[dict[str, Any]]:
        self.last_via = "demo"
        return _filter_todos(self._load()["todos"], list_filter, limit, include_completed)

    def search(self, query: str) -> list[dict[str, Any]]:
        self.last_via = "demo"
        return _search_todos(self._load()["todos"], query)


# --------------------------------------------------------------------------- #
# Backend: live — Things3 via things:/// URLs + JXA reads (opt-in, macOS)
# --------------------------------------------------------------------------- #

class ThingsBackend:
    """Live Things3 backend.

    Writes build a ``things:///`` URL and open it with the macOS ``open``
    command (Things handles the mutation). Reads try JXA via ``osascript``.
    Every operation mirrors to a DemoBackend so reads still work when Things3
    is absent, and any failure falls back to that mirror with a warning.
    """

    mode = "live"

    def __init__(
        self,
        mirror_path: str | os.PathLike[str],
        token_provider: "Callable[[], str] | None" = None,
    ) -> None:
        self.mirror = DemoBackend(mirror_path)
        self.last_via = "things-live"
        # Settings-aware callers inject a provider that reads the declared
        # `auth_token` secret per call; standalone users fall back to the
        # allowlisted env var.
        self._token_provider = token_provider or (
            lambda: os.environ.get("THINGS_AUTH_TOKEN", "")
        )

    # -- low-level plumbing --

    def _open_url(self, url: str) -> None:
        subprocess.run(
            ["open", url],
            check=True,
            capture_output=True,
            timeout=10,
        )

    def _ensure_tags(self, tags: list[str]) -> None:
        """things:///add only applies tags that already exist — create missing ones."""
        wanted = [t for t in tags if t and t.strip()]
        if not wanted:
            return
        script = (
            'const app = Application("Things3");'
            f"const wanted = {json.dumps(wanted)};"
            "const existing = app.tags().map(function (t) { return t.name(); });"
            "wanted.forEach(function (name) {"
            "  if (existing.indexOf(name) < 0) {"
            '    app.make({new: "tag", withProperties: {name: name}});'
            "  }"
            "});"
        )
        subprocess.run(
            ["osascript", "-l", "JavaScript", "-e", script],
            capture_output=True,
            text=True,
            timeout=20,
        )

    def _jxa_read_todos(self) -> list[dict[str, Any]]:
        # Smart list names follow the system language — probe (bucket, candidate name…) in order; on a hit, record the assignment
        script = (
            'const app = Application("Things3");'
            "const buckets = [];"
            'const pairs = [["today","Today","Today"],["tomorrow","Tomorrow","Tomorrow"],'
            '["inbox","Inbox","Inbox"],["someday","Someday","Someday"]];'
            "pairs.forEach(function (pair) {"
            "  for (let i = 1; i < pair.length; i++) {"
            "    try {"
            "      const lst = app.lists.byName(pair[i]);"
            "      if (lst.exists()) {"
            "        lst.toDos().forEach(function (t) {"
            "          try { buckets.push([t.id(), pair[0]]); } catch (e) {}"
            "        });"
            "        break;"
            "      }"
            "    } catch (e) {}"
            "  }"
            "});"
            "const prio = {today: 0, tomorrow: 1, inbox: 2, someday: 3};"
            "const whenById = {};"
            "buckets.forEach(function (b) {"
            "  const id = b[0], k = b[1];"
            "  if (!(id in whenById) || prio[k] < prio[whenById[id]]) whenById[id] = k;"
            "});"
            "const todos = app.toDos();"
            "JSON.stringify(todos.map(function (t) {"
            "  const id = t.id();"
            "  return {"
            "    id: id,"
            "    title: t.name(),"
            "    notes: t.notes(),"
            '    completed: t.status() === "completed",'
            "    tags: t.tagNames(),"
            '    when: whenById[id] || "anytime",'
            '    list: "things"'
            "  };"
            "}));"
        )
        proc = subprocess.run(
            ["osascript", "-l", "JavaScript", "-e", script],
            capture_output=True,
            text=True,
            timeout=20,
        )
        if proc.returncode != 0:
            raise RuntimeError((proc.stderr or "osascript failed").strip())
        for line in reversed(proc.stdout.strip().splitlines()):
            line = line.strip()
            if line.startswith("["):
                parsed = json.loads(line)
                if isinstance(parsed, list):
                    return parsed
        raise RuntimeError("osascript returned no JSON array")

    def _read(self) -> list[dict[str, Any]] | None:
        """Try a live JXA read; on any failure set `via` and return None."""
        try:
            todos = self._jxa_read_todos()
            self.last_via = "things-jxa"
            return todos
        except Exception as exc:  # noqa: BLE001 - never crash when Things is absent
            self.last_via = f"demo (things read failed: {exc})"
            return None

    def _note_fallback(self, exc: Exception) -> None:
        self.last_via = f"demo (things url failed: {exc})"
        try:
            print(f"[things-demo] warn: {exc}", file=sys.stderr)
        except Exception:  # noqa: BLE001 - logging must never raise
            pass

    # -- CRUD --

    def add(self, fields: dict[str, Any]) -> dict[str, Any]:
        todo = self.mirror.add(fields)
        params: dict[str, str] = {"title": todo["title"]}
        if fields.get("notes"):
            params["notes"] = fields["notes"]
        params["when"] = fields.get("when") or "inbox"
        if fields.get("deadline"):
            params["deadline"] = fields["deadline"]
        if fields.get("tags"):
            params["tags"] = ",".join(fields["tags"])
            try:
                self._ensure_tags(fields["tags"])
            except Exception as exc:  # noqa: BLE001 - best-effort; URL add still proceeds
                print(f"[things-demo] warn: tag pre-create failed: {exc}", file=sys.stderr)
        if fields.get("list"):
            params["list"] = fields["list"]
        url = "things:///add?" + urlencode(params, quote_via=quote)
        try:
            self._open_url(url)
            self.last_via = "things-url"
        except Exception as exc:  # noqa: BLE001
            self._note_fallback(exc)
        return todo

    def update(self, todo_id: str, fields: dict[str, Any]) -> dict[str, Any] | None:
        todo = self.mirror.update(todo_id, fields)
        if todo is None:
            return None
        token = self._token_provider() or ""
        if not token:
            self.last_via = "demo (no THINGS_AUTH_TOKEN; live update skipped)"
            return todo
        params: dict[str, str] = {"id": todo_id, "auth-token": token}
        if fields.get("title"):
            params["title"] = fields["title"]
        if "notes" in fields or "append_notes" in fields:
            params["notes"] = todo.get("notes") or ""
        if fields.get("when"):
            params["when"] = fields["when"]
        if fields.get("deadline"):
            params["deadline"] = fields["deadline"]
        if fields.get("tags"):
            params["tags"] = ",".join(fields["tags"])
        url = "things:///update?" + urlencode(params, quote_via=quote)
        try:
            self._open_url(url)
            self.last_via = "things-url"
        except Exception as exc:  # noqa: BLE001
            self._note_fallback(exc)
        return todo

    def delete(self, todo_id: str) -> dict[str, Any] | None:
        # The Things URL scheme has no delete command — only JXA can truly delete (irreversible on the app side)
        try:
            script = (
                'const app = Application("Things3");'
                f"const target = {json.dumps(todo_id)};"
                "const hits = app.toDos().filter(function (t) {"
                "  try { return t.id() === target; } catch (e) { return false; }"
                "});"
                "if (hits.length > 0) { hits[0].delete(); }"
                "JSON.stringify({found: hits.length > 0});"
            )
            proc = subprocess.run(
                ["osascript", "-l", "JavaScript", "-e", script],
                capture_output=True,
                text=True,
                timeout=20,
            )
            if proc.returncode != 0:
                raise RuntimeError((proc.stderr or "osascript failed").strip())
            found = False
            for line in reversed(proc.stdout.strip().splitlines()):
                line = line.strip()
                if line.startswith("{"):
                    found = bool(json.loads(line).get("found"))
                    break
            if found:
                self.mirror.delete(todo_id)
                self.last_via = "things-jxa"
                return {"id": todo_id}
        except Exception as exc:  # noqa: BLE001 - never crash when Things is absent
            self._note_fallback(exc)
        removed = self.mirror.delete(todo_id)
        if removed is not None:
            self.last_via = "demo (things delete)"
            return removed
        return None

    def get(self, todo_id: str) -> dict[str, Any] | None:
        todos = self._read()
        if todos is not None:
            for todo in todos:
                if todo.get("id") == todo_id:
                    return todo
            return None
        return self.mirror.get(todo_id)

    def list_todos(
        self, list_filter: str, limit: int, include_completed: bool
    ) -> list[dict[str, Any]]:
        todos = self._read()
        if todos is None:
            return self.mirror.list_todos(list_filter, limit, include_completed)
        return _filter_todos(todos, list_filter, limit, include_completed)

    def search(self, query: str) -> list[dict[str, Any]]:
        todos = self._read()
        if todos is None:
            return self.mirror.search(query)
        return _search_todos(todos, query)


# --------------------------------------------------------------------------- #
# Plugin
# --------------------------------------------------------------------------- #

class ThingsDemo(Plugin):
    """OpenCapX capability plugin exposing things.* CRUD."""

    def __init__(
        self,
        manifest_path: str | None = None,
        store_path: str | None = None,
        mode: str | None = None,
    ) -> None:
        super().__init__(manifest_path=manifest_path)
        # Base store = explicit override → manifest storePath → allowlisted env
        # → plugin-data default. The declarative `store_path` setting is applied
        # per call on top of this (see _resolve_store_path).
        self._store_path = Path(store_path) if store_path else self._default_store()
        self._mode = mode
        self._backend: DemoBackend | ThingsBackend | None = None
        self._backend_mode: str | None = None
        self._backend_store: Path | None = None

    # -- configuration --

    def _default_store(self) -> Path:
        # Same as weather-demo: manifest storePath first (the only reliable override under S1 env
        # isolation/sandbox), then the dev env var (must be in the allowlist); the default lands in
        # plugin-data (the sandbox's only writable area; the old ~/.opencapx/cache/ is rejected under a sandbox declaration).
        raw = self.manifest.get("storePath")
        if isinstance(raw, str) and raw:
            return Path(raw)
        env = os.environ.get("OPENCAPX_THINGS_STORE")
        if env:
            return Path(env)
        pid = self.manifest.get("id") or "things-demo"
        return Path.home() / ".opencapx" / "plugin-data" / pid / "store.json"

    def _setting(self, key: str, default: Any = None) -> Any:
        try:
            value = self.config_get(key, default)
        except Exception:  # noqa: BLE001 - a settings read must never fail a capability
            return default
        return default if value is None else value

    def _setting_str(self, key: str) -> str:
        value = self._setting(key, "")
        return value.strip() if isinstance(value, str) else ""

    def _resolve_store_path(self) -> Path:
        # Precedence: declared setting → manifest storePath → env → default.
        setting = self._setting_str("store_path")
        if setting:
            return Path(setting)
        return self._store_path

    def _resolve_mode(self) -> str:
        # Precedence: declared setting → explicit constructor override → env → demo.
        setting = self._setting_str("mode")
        if setting in ("demo", "live"):
            return setting
        if self._mode in ("demo", "live"):
            return self._mode
        if os.environ.get("OPENCAPX_THINGS_MODE") == "live" and sys.platform == "darwin":
            return "live"
        return "demo"

    def _resolve_auth_token(self) -> str:
        # Precedence: declared secret setting (`secret:auth_token`) → env → unset.
        token = self._setting_str("secret:auth_token")
        if token:
            return token
        return os.environ.get("THINGS_AUTH_TOKEN") or ""

    def backend(self) -> DemoBackend | ThingsBackend:
        # Settings are re-read every call: the app's set_setting_value writes
        # config/keychain without restarting the plugin, so a save must take
        # effect on the next capability invocation. Rebuild only on change.
        mode = self._resolve_mode()
        store = self._resolve_store_path()
        if (
            self._backend is not None
            and self._backend_mode == mode
            and self._backend_store == store
        ):
            return self._backend
        if mode == "live":
            self._backend = ThingsBackend(store, token_provider=self._resolve_auth_token)
        else:
            self._backend = DemoBackend(store)
        self._backend_mode = mode
        self._backend_store = store
        return self._backend

    # -- validation helpers --

    @staticmethod
    def _validate_id(params: dict[str, Any], field: str = "id") -> str:
        value = params.get(field)
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"invalid input: {field} is required")
        return value.strip()

    @staticmethod
    def _validate_title(params: dict[str, Any]) -> str:
        title = params.get("title")
        if not isinstance(title, str) or not title.strip():
            raise ValueError("invalid input: title is required and must be a non-empty string")
        return title.strip()

    # -- capabilities --

    @capability("things.add")
    def things_add(self, params: dict[str, Any]) -> dict[str, Any]:
        fields: dict[str, Any] = {"title": self._validate_title(params)}

        notes = params.get("notes")
        if notes is not None:
            if not isinstance(notes, str):
                raise ValueError("invalid input: notes must be a string")
            fields["notes"] = notes

        if params.get("when") is not None:
            fields["when"] = _validate_when(params["when"])
        if params.get("deadline") is not None:
            fields["deadline"] = _validate_deadline(params["deadline"])
        if params.get("tags") is not None:
            fields["tags"] = _validate_str_list(params["tags"], "tags")
        if params.get("list") is not None:
            list_name = params["list"]
            if not isinstance(list_name, str) or not list_name.strip():
                raise ValueError("invalid input: list must be a non-empty string")
            fields["list"] = list_name.strip()

        backend = self.backend()
        todo = backend.add(fields)
        return {
            "ok": True,
            "id": todo["id"],
            "title": todo["title"],
            "via": backend.last_via,
        }

    @capability("things.update")
    def things_update(self, params: dict[str, Any]) -> dict[str, Any]:
        todo_id = self._validate_id(params)
        fields: dict[str, Any] = {}

        if params.get("title") is not None:
            fields["title"] = self._validate_title(params)
        if params.get("notes") is not None:
            if not isinstance(params["notes"], str):
                raise ValueError("invalid input: notes must be a string")
            fields["notes"] = params["notes"]
        if params.get("append_notes") is not None:
            if not isinstance(params["append_notes"], str):
                raise ValueError("invalid input: append_notes must be a string")
            fields["append_notes"] = params["append_notes"]
        if params.get("when") is not None:
            fields["when"] = _validate_when(params["when"])
        if params.get("deadline") is not None:
            fields["deadline"] = _validate_deadline(params["deadline"])
        if params.get("tags") is not None:
            fields["tags"] = _validate_str_list(params["tags"], "tags")
        if params.get("add_tags") is not None:
            fields["add_tags"] = _validate_str_list(params["add_tags"], "add_tags")
        if params.get("completed") is not None:
            if not isinstance(params["completed"], bool):
                raise ValueError("invalid input: completed must be a boolean")
            fields["completed"] = params["completed"]

        backend = self.backend()
        todo = backend.update(todo_id, fields)
        if todo is None:
            raise ValueError(f"invalid input: todo not found: {todo_id}")
        return {
            "ok": True,
            "id": todo["id"],
            "updated": todo,
            "via": backend.last_via,
        }

    @capability("things.delete")
    def things_delete(self, params: dict[str, Any]) -> dict[str, Any]:
        todo_id = self._validate_id(params)
        backend = self.backend()
        removed = backend.delete(todo_id)
        if removed is None:
            raise ValueError(f"invalid input: todo not found: {todo_id}")
        return {
            "ok": True,
            "id": todo_id,
            "via": backend.last_via,
        }

    @capability("things.list")
    def things_list(self, params: dict[str, Any]) -> dict[str, Any]:
        list_filter = params.get("list", "all")
        if list_filter is None:
            list_filter = "all"
        if not isinstance(list_filter, str) or list_filter not in LIST_FILTERS:
            raise ValueError(
                "invalid input: list must be one of " + "/".join(LIST_FILTERS)
            )

        limit = params.get("limit", 20)
        if limit is None:
            limit = 20
        if isinstance(limit, bool) or not isinstance(limit, int):
            raise ValueError("invalid input: limit must be an integer")
        if limit < 1 or limit > 100:
            raise ValueError("invalid input: limit must be between 1 and 100")

        include_completed = params.get("include_completed", False)
        if include_completed is None:
            include_completed = False
        if not isinstance(include_completed, bool):
            raise ValueError("invalid input: include_completed must be a boolean")

        backend = self.backend()
        todos = backend.list_todos(list_filter, limit, include_completed)
        return {
            "count": len(todos),
            "todos": todos,
            "via": backend.last_via,
        }

    @capability("things.show")
    def things_show(self, params: dict[str, Any]) -> dict[str, Any]:
        has_id = params.get("id") is not None
        has_query = params.get("query") is not None
        if has_id == has_query:
            raise ValueError("invalid input: provide exactly one of id or query")

        backend = self.backend()
        if has_id:
            todo_id = self._validate_id(params)
            todo = backend.get(todo_id)
            if todo is None:
                raise ValueError(f"invalid input: todo not found: {todo_id}")
            return {"todo": todo, "via": backend.last_via}

        query = params.get("query")
        if not isinstance(query, str) or not query.strip():
            raise ValueError("invalid input: query is required and must be a non-empty string")
        todos = backend.search(query.strip())
        return {"count": len(todos), "todos": todos, "via": backend.last_via}

    @capability("things.search")
    def things_search(self, params: dict[str, Any]) -> dict[str, Any]:
        query = params.get("query")
        if not isinstance(query, str) or not query.strip():
            raise ValueError("invalid input: query is required and must be a non-empty string")

        backend = self.backend()
        todos = backend.search(query.strip())
        return {
            "count": len(todos),
            "todos": todos,
            "via": backend.last_via,
        }

    # -- Phase 37 self-check endpoint --

    @method("core.probe.capability")
    def probe(self, params: dict[str, Any]) -> dict[str, Any]:
        return {
            "ok": True,
            "via": "things-demo",
            "mode": self._resolve_mode(),
            "capabilities": sorted(self._capabilities.keys()),
        }


if __name__ == "__main__":
    ThingsDemo(
        manifest_path=str(Path(__file__).resolve().parent.parent / "opencapx-plugin.json"),
    ).run()
