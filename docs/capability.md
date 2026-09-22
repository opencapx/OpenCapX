# Capability Specification

A Capability is the only interface the Agent sees. **An Agent always calls Capabilities, never a specific plugin, never a specific model.** Plugins can be swapped, models can be swapped, Capabilities stay the same.

## Naming

`<domain>.<action>`, all lowercase, one dot-separated level. The Registry is the system-wide namespace; a new capability must be registered here before it is implemented.

## v1 Standard Capabilities

| ID | Input essentials | Output essentials |
|---|---|---|
| `image.analyze` | `image` (path/URL/base64), optional `question` | `description`, `text`, `objects[]` |
| `image.ocr` | `image` | `text`, `blocks[]` (position + text) |
| `audio.transcribe` | `audio` (path), optional `language` | `text`, `segments[]` |
| `speech.synthesize` | `text`, optional `voice`, `speed` | `audio` (generated audio path, inside the Core cache directory) |
| `screen.capture` | optional `region`, `window` | `image` (screenshot written-to-disk path) |
| `clipboard.read` | none | `text`, optional `image` |
| `browser.open` | `url` | `ok` |
| `browser.read` | `url` | `title`, `text` (extracted body text) |
| `file.read` | `path` (constrained by permission scope) | `text` |
| `clipboard.write` | `text` | `ok` |
| `file.write` | `path`, `content` | `ok`, `bytes` |
| `file.search` | `root`, `pattern` | `matches[]` (paths), `truncated` |
| `file.watch` | `path`, `recursive` (subscribe type) | event stream `created`/`modified`/`removed` (see "Capability Types") |
| `context.get_current` | none | `active_app`, `window_title`, `clipboard` (capped at 500 characters), `screen_available`, `degraded` |
| `system.permission_status` | none (v1.2) | `areas` (screen_recording/accessibility, `granted\|denied\|not_required`), `guidance` (v1.5, denied areas only: `settingsUrl` + `steps`), `os` |
| `automation.run` | `app` (bundle id/app name), optional `action` (activate/launch/quit), optional `script` (inline AppleScript; when given, `action` is ignored) | `ok`, optional `output` |
| `input.send` | `type` (key/text/mouse_move/mouse_click) + the corresponding fields | `ok` |
| `photos.read` | optional `limit` (default 20, ≤100) | `count`, `photos[]` (id/name/date) |
| `contacts.search` | `query` (required), optional `limit` | `contacts[]` (id/name) |
| `calendar.events` | optional `days` (default 7, ≤31), optional `calendar` (filter by calendar name) | `events[]` (start/end/summary/calendar) |
| `location.get` | none | `lat`, `lon`, `accuracy` |
| `audio.play` | `path`, optional `volume` (0–1), optional `wait` | `ok`, `waited` |
| `url.scheme.open` | `url` (a whitelisted non-http(s) scheme) | `ok` |
| `screen.watch` | optional `interval` (seconds, 5–3600, default 30), optional `region` (subscribe type, v1.3) | event stream `changed` + `image` (frame path, see "Capability Types") |
| `media.playback` | `action` (play/pause/toggle/next/previous), optional `app` (music/spotify, v1.4) | `ok`, optional `track`/`artist` |
| `system.sleep` | none (v1.4) | `ok` |
| `system.lock` | none (v1.4) | `ok` |
| `system.settings` | `setting` (dark_mode/wallpaper/volume) + `value` (v1.4) | `ok`, `setting` |
| `printer.print` | `path` (required), optional `printer`, optional `copies` (1–10, default 1, v1.4) | `ok`, `queued` |
| `messages.recent` | optional `limit` (default 20, ≤100) (v1.4) | `count`, `messages[]` (id/text/from_me/ts/handle) |
| `window.list` | none (v1.4) | `count`, `windows[]` (process/front/x/y/w/h/title), `truncated` |
| `window.focus` | `process` (required), optional `title` (v1.4) | `ok`, `process` |
| `notes.read` | optional `limit` (default 20, ≤100) (v1.4) | `count`, `notes[]` (id/name/snippet) |
| `reminders.read` | optional `limit` (default 100, ≤200), optional `incomplete_only` (default true) (v1.4) | `reminders[]` (id/name/due/completed) |
| `reminders.write` | `name` (required), optional `body`, optional `list` (v1.4) | `ok`, `list` |
| `mail.recent` | optional `limit` (default 10, ≤50) (v1.4) | `messages[]` (id/subject/sender/date/read) |
| `things.add` | `title` (required), optional `notes`, optional `due`, optional `list` (v1.5, provided by a plugin) | `ok`, `id` |
| `things.update` | `id` (required) + fields to update: `title`/`notes`/`due`/`completed` (v1.5, provided by a plugin) | `ok`, `id` |
| `things.list` | optional `limit` (default 50, ≤200), optional `completed` (v1.5, provided by a plugin) | `count`, `things[]` (id/title/completed/due) |
| `things.show` | `id` (required) (v1.5, provided by a plugin) | `id`, `title`, `notes`, `due`, `completed` |
| `things.search` | `query` (required), optional `limit` (default 20, ≤100) (v1.5, provided by a plugin) | `count`, `things[]` (id/title) |
| `things.delete` | `id` (required) (v1.5, provided by a plugin) | `ok`, `id` |

In the table above, all rows are call type except `file.watch` and `screen.watch` (event-stream type, see "Capability Types"). The v1.3 batch: nine rows starting at `automation.run`; the v1.4 batch: twelve rows starting at `media.playback`; the v1.5 batch: six rows starting at `things.add` — **provided by plugins only, with no built-in fallback** (they are not in the "Built-in Providers" list; when the plugin is not installed, `execute` returns `capability_unavailable`).

## Built-in Providers

`clipboard.read` / `clipboard.write` / `file.read` / `file.write` / `file.search` / `image.analyze` / `image.ocr` / `screen.capture` / `speech.synthesize` / `browser.open` / `browser.read` / `context.get_current` / `system.permission_status` / `automation.run` / `input.send` / `photos.read` / `contacts.search` / `calendar.events` / `location.get` / `audio.play` / `url.scheme.open` / `media.playback` / `system.sleep` / `system.lock` / `system.settings` / `printer.print` / `messages.recent` / `window.list` / `window.focus` / `notes.read` / `reminders.read` / `reminders.write` / `mail.recent` — thirty-three capabilities for which Core ships a built-in provider: when the Registry has no plugin provider, Core executes it directly, and a plugin that registers a same-named capability overrides the built-in — **officially built in, overridable by plugins**. For a built-in capability with no plugin row, `list_capabilities` surfaces `providers: ["core"]`, and its call-chain events are the same as the plugin path (`capability.started/completed/failed`, `pluginId: "core"`).

- The built-in path only passes the Agent-layer permission (see [permissions.md](permissions.md) "Two-Layer Decision": no plugin is involved, so only the Agent layer is checked); permission mappings `clipboard.read`→`clipboard.read`, `clipboard.write`→`clipboard.write`, `file.read`→`file.read`, `file.write`→`filesystem.write` (high-risk, runtime allow-once), `file.search`→`file.read`, `image.analyze` / `image.ocr`→`image.read`, `screen.capture`→`screen.capture`, `speech.synthesize`→`notification.post` (tightened in v1.2: speech output is a human-facing channel; the original mapping `storage.local` was granted by default, see the migration section in permissions.md), `browser.open` / `browser.read`→`browser.control`, `context.get_current`→`clipboard.read` (the return contains a clipboard excerpt, so the strictest component applies); `system.permission_status` has no mapping — read-only metadata, exempt from the Agent-layer gate (see permissions.md); v1.3 batch: `automation.run`→`automation.control` (high-risk), `input.send`→`input.control` (high-risk), `photos.read`→`photos.read`, `contacts.search`→`contacts.read`, `calendar.events`→`calendar.read`, `location.get`→`location.read`, `audio.play`→`audio.output`, `url.scheme.open`→`url.scheme.open`, `screen.watch` (subscribe)→`screen.capture`; v1.4 batch: `media.playback`→`media.control`, `system.sleep` / `system.lock`→`power.control` (high-risk), `system.settings`→`system.settings`, `printer.print`→`printer.control`, `messages.recent`→`messages.read` (high-risk), `window.list` / `window.focus`→`window.management` (high-risk), `notes.read`→`notes.read`, `reminders.read`→`reminders.read`, `reminders.write`→`reminders.write`, `mail.recent`→`mail.read` (high-risk)
- `clipboard.read` / `clipboard.write` (v1.2, `core/clipboard.rs`) read and write directly via arboard on three platforms (macOS / Windows / X11+Wayland); read prefers text, and with no text it takes an image, converts it to PNG under `~/.opencapx/cache/`, and returns the `image` path; when both are empty it returns `{text:""}` (an empty clipboard is not an error); arboard access is mutex-guarded within the process (macOS NSPasteboard is not concurrency-safe)
- `file.read` (v1.2) reads text files capped at 2MB, and honestly errors on non-UTF-8; **known v1 limitation: the path is unrestricted** (the scope column exists but is not read, the same position as `file.write`, see "Scope" in permissions.md)
- `file.write` does not create parent directories automatically; `file.search` matches **name substrings** of files/directories, traverses recursively, and caps at 100 entries (`truncated` flags cut-off)
- `image.analyze` (v1.1, A9) goes through **local Ollama** (`/api/generate`, 180s timeout): the default model is the first installed model from `/api/tags`; the env `OPEN_CAPX_VISION_MODEL` pins it and `OPEN_CAPX_OLLAMA_URL` changes the address; `image` accepts a local path / `data:` base64 / http(s) URL (capped at 20MB); if Ollama is not running it honestly errors instead of degrading; the local model does no object detection, so `objects` is always `[]` — install a Vision plugin to override it for detection accuracy
- `image.ocr` (v1.2) uses the same Ollama path and a fixed transcription prompt; `blocks` is always `[]` (the built-in does no layout analysis; install an OCR plugin to override it for position blocks)
- `screen.capture` (v1.1, A9) writes to `~/.opencapx/cache/`: macOS uses a `screencapture` / Linux a `scrot` subprocess (a TCC denial = a clean non-zero exit), and Windows (v1.2) captures in-process via xcap (full screen + `region` clamped to the bounds; `window` is unsupported). macOS requires the system "Screen Recording" permission; when missing, the command fails outright with authorization guidance in the error; Windows is verified at compile time, with real-device verification pending CI/user feedback
- `speech.synthesize` (v1.2, `core/speech.rs`) is platform TTS: macOS `say` (AIFF) / Linux `espeak -w` (missing → prompts to install) / Windows PowerShell `System.Speech`; `speed` ∈ 0.5–2.0; success = the command exits 0 and the audio file exists; it is written to the cache directory and its `audio` path is returned
- `browser.open` (v1.2, `core/browser.rs`) whitelists the `http(s)://` schemes, requires URL ≤2048, and rejects control characters; macOS `open` / Linux `xdg-open` / Windows `rundll32 url.dll,FileProtocolHandler`
- `browser.read` (v1.2) fetches with reqwest (30s, UA identified) then extracts with scraper: title + body text (whitespace normalized, capped at 100,000 characters + `truncated`); rejects body >2MB or a non-text/html Content-Type; **does no JS rendering** (an SPA shell reads as empty; a plugin can override); the network egress is bound by the `browser.control` ask gate — once granted, any URL (including localhost/intranet) is reachable; domain scope is v2
- `context.get_current` (v1.1, A7, `core/context.rs`): two paths on macOS — when AppleScript (System Events, app name + window title, triggers the "Automation" / "Accessibility" TCC) fails, it degrades to `lsappinfo` (no TCC, the bundle id is used as the app name, `window_title` is set to null, `degraded: true`); the clipboard excerpt (three platforms as of v1.2, via arboard) is capped at 500 characters
- `system.permission_status` (v1.2, `core/osperm.rs`) is an **advance probe** of OS-layer permissions: on macOS it uses the FFI preflight variants (**no prompt**) to check screen recording (CGPreflightScreenCaptureAccess) and accessibility (AXIsProcessTrusted); Windows/Linux have no TCC equivalent and return `not_required`. Purpose: the agent checks the status before calling screen.capture, turning an error from "reported after the fact" into "avoided up front".
  v1.5 broadens this: **first-use guidance** — a denied area carries a `guidance` entry (`settingsUrl` deep link + `steps` human-readable steps), and the agent can use `url.scheme.open` to open the corresponding system settings pane for the user directly (e.g. `x-apple.systempreferences:…Privacy_ScreenCapture`), turning "tell the user where to click" into "open it for the user"; granted / not_required carry no guidance
- `automation.run` (v1.3, `core/appctl.rs`) is macOS-only AppleScript: an app identifier in reverse-domain style (containing `.` with no space) is matched by bundle id (`application id`), otherwise by app name; when `script` is given it wraps a tell block and runs it in the target app's context (otherwise a single-line `activate`/`launch`/`quit`); 60s timeout, stdout capped at 4,000 characters; triggers the system "Automation" TCC, and a denial puts authorization guidance in the error. It honestly errors off macOS; a plugin can override
- `input.send` (v1.3, `core/inputctl.rs`) synthesizes keyboard/mouse input: macOS CGEvent FFI (with an AXIsProcessTrusted probe beforehand; missing → honest error, not a silent drop) / Linux xdotool (missing → prompts to install) / Windows SendInput (absolute coordinates normalized to 0–65535). Key names are looked up in a table of 26 keys: `return` `tab` `esc` `delete` `forward_delete` `space` the arrow keys `home` `end` `pageup` `pagedown` `f1`–`f12` (key codes on all three platforms; **modifier-key combinations are not done in v1**, since combination semantics differ widely across platforms; a plugin can override); `text` ≤500 characters (on macOS it is sent in chunks of 20 UTF-16 code units). **Known limitation: synthesized events on all three platforms may be silently dropped by the host environment** (macOS missing accessibility / Wayland / Windows UIPI elevated windows); the built-in layer cannot confirm after the fact, so the agent judges by observing results
- `photos.read` / `contacts.search` / `calendar.events` / `location.get` (v1.3, `core/pim.rs`) are the macOS-only personal-data read surface: the first three read Photos/Contacts/Calendar directly via AppleScript (each triggering the "Automation" TCC), and their date fields are localized strings (Photos/Calendar AppleScript does not emit ISO format — an honest limitation); `location.get` runs a one-shot swift script through CoreLocation; the first call triggers the location prompt and times out after 7s for that call, and retrying once granted succeeds (it does not pretend to have obtained a location). They honestly error off macOS; osperm does not extend to these four — there is no "promptless advance probe" API (see the v1.3 note in permissions.md)
- `audio.play` (v1.3, `core/audio.rs`) plays local audio: macOS `afplay` / Linux `paplay` (missing → prompts to install) / Windows PowerShell Media.SoundPlayer (**wav-only, no volume support, forced synchronous playback** — background playback is killed when the process exits); extension whitelist (wav/mp3/aiff/aac/m4a/ogg/flac); `wait=true` waits for playback to finish (120s ceiling), and by default it spawns and returns immediately
- `url.scheme.open` (v1.3, `core/browser.rs`) invokes non-http(s) schemes (mailto/tel/custom handlers): `http(s)` is rejected (that belongs to `browser.open`); dangerous schemes are rejected by blacklist (`file` `javascript` `data` `vbscript` `about` `blob` `view-source` `jar` `ws` `wss` `chrome` `chromium` `chrome-extension` `moz-extension` `intent`); after validating the scheme's shape it reuses the platform open command, and when no handler is registered the system error is passed through
- `screen.watch` (v1.3, subscribe type, see "Capability Types") is a periodic-screenshot diff watcher: the frame path is delivered with the event, so the agent can feed it straight into `image.analyze`
- `media.playback` (v1.4, `core/media.rs`) controls playback: on macOS it drives Music (`com.apple.Music`) / Spotify (`com.spotify.client`) via AppleScript and returns the current `track`/`artist` (truncated to 300); on Linux it uses `playerctl` (missing → prompts to install), and having no track is not an error; on Windows it sends media keys via SendInput (`inputctl::tap_key`), and **media keys only toggle playback, so the track is unknown** (returns a `note` explaining this). `app` defaults to Music (macOS) / the currently active player (Linux)
- `system.sleep` / `system.lock` (v1.4, `core/power.rs`) are one system command each on three platforms: sleep is macOS `pmset sleepnow` / Linux `systemctl suspend` / Windows `rundll32 powrprof.dll,SetSuspendState` (**known limitation: Windows with hibernation enabled hibernates instead of sleeping**, a system-layer behavior the built-in cannot distinguish); lock is macOS CGSession (when present, no TCC), falling back to System Events Ctrl+Cmd+Q (requires accessibility; honest error when missing) / Linux `loginctl lock-session` / Windows `rundll32 user32.dll,LockWorkStation`
- `system.settings` (v1.4, `core/settings.rs`) makes light settings: macOS AppleScript changes dark mode / wallpaper / volume (all three go through System Events, triggering the "Automation" TCC); the `wallpaper` value must be an existing file with a whitelisted extension (png/jpg/jpeg/tiff/tif/heic/gif/bmp), and the path has backslashes and quotes escaped before it goes into the AppleScript literal; `volume` ∈ 0–100. It honestly errors off macOS; a plugin can override
- `printer.print` (v1.4, `core/printer.rs`) uses macOS / Linux `lpr` (CUPS): `path` must be an existing file, `printer` ≤200 characters (defaults to the system default queue), `copies` 1–10; a non-zero exit puts an `lpstat -p` hint in the error. **Honest boundary: `lpr` confirms "queued", not "physically printed"**, which is what `queued: true` means; Windows errors (CUPS is not used)
- `messages.recent` (v1.4, `core/messages.rs`) is macOS-only: it opens `~/Library/Messages/chat.db` read-only (SQLite, `SQLITE_OPEN_READ_ONLY`) and takes normal messages that have text (tapbacks/stickers etc. with `associated_message_type ≠ 0` are skipped; attachment contents are not read in v1), converts the `date` column from Apple epoch (2001-01-01) nanoseconds to Unix seconds for the return, and reverses the newest-first query into ascending time order (so the agent can read it in order). **The read channel = "Full Disk Access" (a machine-wide TCC)**; without the permission the open fails immediately with authorization guidance in the error (it reports honestly rather than pretending); it honestly errors off macOS, and a plugin can override
- `window.list` / `window.focus` (v1.4, `core/windowctl.rs`) are macOS-only System Events AppleScript, with an AXIsProcessTrusted precheck up front (missing → honest error): list enumerates windows of processes that are not background-only, one line per window as `proc|front|x|y|w|h|title` (the title is placed last and `splitn(7)` preserves it verbatim), capped at 200 + `truncated`; focus switches `frontmost` and can `AXRaise` a named window (a failed raise does not fail the whole call — foreground was already achieved); process names go through **validation rather than escaping** (quotes/backslashes/control characters are rejected; the process name comes from agent input, so a whitelist is more stable than escaping). Linux/Windows are not done in v1; a plugin can override
- `notes.read` (v1.4, `core/pim.rs`) reads Notes via macOS-only AppleScript: the most recent N entries (id/name/snippet), taken from the tail then reversed; `snippet` = the first 200 characters of the body (HTML) after flattening newlines/tabs; **honest limitation: it does no HTML tag stripping**; install a plugin to override it for clean body text
- `reminders.read` / `reminders.write` (v1.4, `core/pim.rs`) are macOS-only AppleScript: read lists reminders (id/name/due/completed), with `incomplete_only` defaulting to true and a cap of 200; write appends a `make new reminder` to the end of the specified list (default: the first list), with name ≤500 and body ≤2000 (newlines/tabs flattened — AppleScript string literals do not accept raw newlines). **Known limitation: v1 does not accept due dates** (natural-language/localized-format parsing is unreliable; shipping it would be worse than not shipping it)
- `mail.recent` (v1.4, `core/pim.rs`) reads the most recent N messages in the Mail inbox via macOS-only AppleScript (id/subject/sender/date/read); `date` is a localized string (Mail AppleScript does not emit ISO — the same honest limitation as Calendar), a missing subject/sender returns `""` rather than breaking the whole batch, and `limit` ≤50 (with a large inbox the count itself is slow)

## Capability Types (v1.1): call / subscribe

`type` is a top-level declaration in the schema, not a Registry table column (the Registry does not type capabilities; the type only affects the call protocol).

### call (default)

One call = one request/response; Core enforces a 60s timer, terminating in one of three states: timeout/completed/errored. Every v1 standard Capability is call type and goes through `opencapx.execute`.

### subscribe (event stream)

After subscribing it **never completes** — the "call → timeout/completed/errored" model does not apply, so it is modeled separately and goes through `opencapx.subscribe` / `opencapx.unsubscribe` (for the MCP-side spec see "Subscription Tool Pair" in [mcp.md](mcp.md)). The first case is `file.watch` (v1.1); v1.3 adds `screen.watch`.

```json
{
  "id": "file.watch",
  "version": "1",
  "type": "subscribe",
  "inputSchema": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "file or directory to watch" },
      "recursive": { "type": "boolean" }
    },
    "required": ["path"]
  },
  "eventSchema": {
    "type": "object",
    "properties": {
      "event": { "type": "string", "enum": ["created", "modified", "removed"] },
      "path": { "type": "string" }
    },
    "required": ["event", "path"]
  },
  "permissions": ["file.read"]
}
```

Lifecycle:

```text
opencapx.subscribe {capability, input}
  → Core validates inputSchema + Permission (judged once at subscription time; event pushes are not judged one by one)
  → stores it in the in-memory subscription table, returns subscriptionId
  → plugin reports an event → Core validates against eventSchema → opencapx.event pushed to the Agent
  → opencapx.unsubscribe / Agent disconnect / Core exit → clean up the subscription and the underlying watcher
```

Rules:

- Subscriptions are not persisted to SQLite; they live only in the in-memory registry; they do not survive across MCP connections, so a reconnect re-subscribes
- A failed event validation is handled the same way as call-type output validation: record the event, do not pass it through
- Everything goes through the EventBus (`capability.subscribed` / `capability.event` / `capability.unsubscribed`; the catalog goes in events.md), and Automation (evaluation §15) is built on the same stream, not a separate channel
- The subscription itself has no timeout; the single call that establishes the subscription can be cancelled — for the semantics see "Cancellation" in mcp.md

Implementation (landed in v1.1, `core/subscription.rs`):

- `file.watch` is provided by a Core built-in watcher (polling diff, 2s period; FSEvents/inotify later as needed): the first round only establishes a baseline, and files that already existed at subscription time are not reported as `created`
- Flood protection: a single diff round reports at most 100 events and a snapshot is capped at 5000 entries; directories only report `created`/`removed` (a parent directory's mtime changing with a child file does not report `modified`)
- `screen.watch` (v1.3) calls `screen.capture` periodically (default 30s, adjustable 5–3600) → decodes pixels and hashes a diff: the first round establishes a baseline and emits no event; a change emits `{event:"changed", image:frame path}`; frame files roll over and only the newest one is kept; a capture failure (TCC denied, tool missing) only skips that round and does not kill the subscription, while a bad `region` shape fails fast at subscription time
- Connection lifecycle: the `opencapx mcp` process holds a single conn id (`X-OpenCapX-Conn`, reported with /rpc and the SSE /events); when the SSE disconnects, all subscriptions and watchers under that conn are cleaned up
- The subscription limit is 32 per Agent; letting plugins act as subscribe-type providers is follow-up work (v1.1 is built-in only)

## Schema Definition

Each Capability uses JSON Schema (draft 2020-12) to define its input and output, registered in the protocol package (`packages/protocol/capabilities/*.json`, or placed directly in `docs/schemas/` for v1), and Core validates at both ends:

```json
{
  "id": "image.analyze",
  "version": "1",
  "inputSchema": {
    "type": "object",
    "properties": {
      "image": { "type": "string", "description": "local path, http(s) URL, or data: base64" },
      "question": { "type": "string", "description": "optional question about the image" }
    },
    "required": ["image"]
  },
  "outputSchema": {
    "type": "object",
    "properties": {
      "description": { "type": "string" },
      "text": { "type": "string" },
      "objects": { "type": "array", "items": { "type": "object" } }
    },
    "required": ["description"]
  }
}
```

Rules:

- The capability list returned in the plugin handshake must match the schema versions here
- Input that does not match inputSchema: Core refuses to send it and returns -32602 without disturbing the plugin
- Output that does not match outputSchema: a `capability.failed` event is recorded and the result is not passed through to the Agent

## Metadata (v1.1)

Four optional fields at registration time, surfaced verbatim by `opencapx.list_capabilities`. The purpose: what the Agent gets is not just "what exists" but also the basis for deciding "which one to use" (evaluation §2.1); the Settings page's Privacy First routing consumes the same data (all of the Local AI Gateway narrowed by §2.5).

| Field | Values | Semantics |
|---|---|---|
| `execution` | `local` / `cloud` / `hybrid` | Execution location. local = data never leaves the machine; hybrid = local preprocessing + a cloud model |
| `typical_latency_ms` | integer | The **declared** p50 estimate. The `avg_latency_ms` collected by the Registry is **measured** — routing and UI prefer measured, and fall back to declared when there is none |
| `cost_tier` | `free` / `low` / `high` | Cost tier. No concrete price is given (prices vary by provider; the tier is stable) |
| `permissions` | `string[]` | The permission scopes the call needs, aligned with the permissions.md glossary. Execution still passes through the Permission Manager every time; the metadata is only for advance hints and UI badges |

## Capability Registry

Stored in the SQLite `capabilities` table:

```text
capabilities
├── id            # image.analyze
├── version       # 1
├── plugin_id     # provider
├── priority      # routing order, smaller goes first
├── enabled
├── execution / typical_latency_ms / cost_tier   # v1.1 declared metadata (see "Metadata")
└── avg_latency_ms / last_used_at   # measured statistics, for routing and Settings page display
```

(permissions do not go in the table and travel with the schema; subscription runtime state lives in the in-memory registry and also does not go into SQLite, see "Capability Types".)

The same capability can be provided by multiple plugins (official Vision, third-party Vision, a local Qwen wrapper, …), and the Registry records all providers.

## Routing (Capability Router)

When a call comes in:

```text
image.analyze
     │
     ▼
Capability Router
     │  query the registry: sort the provider list by priority
     ▼
Primary plugin → fails (timeout/error/process hang) → next provider → …
```

Users can configure provider order per capability (drag and drop in the Settings page); v1 only does the **primary + fallback order list**. `latency / cost / privacy` weighted routing is left to v2, and the table structure already reserves a `priority` field.

```json
// settings.json excerpt
{
  "capabilityRouting": {
    "image.analyze": {
      "providers": ["com.opencapx.vision", "com.example.qwen-vision"]
    }
  }
}
```

## Call Chain (complete)

```text
Agent ──MCP──▶ opencapx.execute {capability:"image.analyze", input:{…}}
                   │
                   ▼
             Permission Manager check (see permissions.md)
                   │
                   ▼
             Capability Router selects a plugin
                   │
                   ▼
             JSON-RPC stdio call to the plugin
                   │
                   ▼
             outputSchema validation → the result returns to the Agent the same way
```

In the meantime the Event Bus emits `capability.started` / `capability.completed` (or `capability.failed`) in sequence, and the pet can subscribe to show "Analyzing image…".
