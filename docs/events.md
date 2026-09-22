# Event Bus Specification

Every state change in the system goes through the Event Bus: agent state, capability execution, pet interaction, plugin lifecycle, permission decisions. Pet animation, the Settings page, notifications, and audit are all subscribers to events.

## Event Structure

```typescript
interface OpencapxEvent {
  id: string        // "evt-<nanos>", consistent with how existing SessionDto.id is generated
  type: string      // see the catalog below
  source: string    // "hooks:claude" | "mcp" | "pet" | "plugin:<id>" | "core"
  timestamp: number // Unix seconds
  payload: unknown  // defined per type
}
```

## Event Catalog

### agent.* (source: hooks, MCP)

| type | payload key points | Description |
|---|---|---|
| `agent.started` | `agent`, `sessionId`, `project` | Session started |
| `agent.thinking` | Same as above | Thinking |
| `agent.working` | Same as above + `message` | Working (tool call) |
| `agent.tool_call` | + `tool`, `input` summary | A single tool call |
| `agent.waiting` | + `message` | Waiting for user input/permission confirmation |
| `agent.completed` | + `message` | Completed |
| `agent.error` | + `error` | Error |

The `agent.*` payload is exactly `SessionDto`: besides `message` (what it is doing) it has two optional fields —
`model` (the model badge) and `speech` (**the agent's body text**, the last assistant segment at the tail of the transcript,
the second line of the bubble; `message` and `speech` are two different things, and the latter is not overridden by the done celebration copy).

### capability.* (source: core)

Since v1.1 all three events carry `agent` (the calling agentId, from /rpc authentication; an empty string = non-Agent context) for Activity Timeline attribution.

| type | payload |
|---|---|
| `capability.started` | `capability`, `pluginId`, `requestId`, `agent` |
| `capability.completed` | + `elapsedMs` |
| `capability.failed` | + `attemptedProviders[]` (`{pluginId, error}`) |
| `capability.subscribed` | `subscriptionId`, `capability`, `agentId` (v1.1 subscription lifecycle, goes into Timeline) |
| `capability.unsubscribed` | + `reason` (`agent` unsubscribe initiated by the agent / `disconnect` cleanup on disconnect) |
| `capability.event` | + `event`, `path` (file.watch's created/modified/removed; high frequency, does not go into Timeline) |

### notification.* (source: core)

`notification.posted` (i2 §13): when `opencapx.notify` sends a system notification it is written to the database at the same time,
payload `agentId`, `title`, `body`, `severity` (info|warn|error).

### pet.* (source: pet UI)

| type | payload |
|---|---|
| `pet.clicked` | `x`, `y` |
| `pet.state_changed` | `from`, `to` |

### plugin.* (source: core)

`plugin.installed` / `plugin.enabled` / `plugin.disabled` / `plugin.uninstalled` / `plugin.error` / `plugin.state_changed` (every state-machine transition)

| type | payload | Trigger |
|---|---|---|
| `plugin.start.rejected` | `pluginId`, `reason` | kill switch hit / Core incompatible with plugin / missing dependency |

### automation.* (source: core)

Trigger audit for i2 §15 Automation; for the spec see [automation.md](automation.md); both go into the Activity Timeline.

| type | payload |
|---|---|
| `automation.rule_fired` | `ruleId`, `event`, `action` |
| `automation.rule_failed` | `ruleId`, `error` |

### rule.* (source: core)

Audit for command rules (see [rules.md](rules.md)). When the CLI hits a rewrite it injects the rule id as `__rule`,
and Core extracts and publishes it during `ingest`; it goes into the Activity Timeline.

| type | payload |
|---|---|
| `rule.applied` | `ruleId`, `agent`, `command` (the original command that was rewritten) |

### permission.* (source: core)

`permission.requested` / `permission.granted` / `permission.denied`; for payloads see [permissions.md](permissions.md).

Custom events: events a plugin sends via `core.emit` are prefixed with the plugin ID, e.g. `com.opencapx.vision.model_latency`.

## Relationship to the Existing Implementation

The existing pipeline is unchanged, absorbed as one entry point into the Bus:

```text
Agent hooks ──POST /event──▶ http::ingest (127.0.0.1:47628, existing)
                               │
                               ▼
                        process_body() parses {agent, event, text, session_id, project}
                               │
                               ▼
                     Event Bus.publish(agent.*)
                               │
        ┌──────────────────────┼──────────────────────┐
        ▼                      ▼                      ▼
  SessionStore (SQLite-backed)  Tauri emit → window     System notification / Tray
```

- The existing `map_state()`'s `working/waiting/done/idle` are collapsed into the event-type decisions `agent.working/agent.waiting/agent.completed/agent.started`
- The frontend uniformly subscribes to the `opencapx-event` dispatched by the Bus, with payload `OpencapxEvent`; the `agent.*` events' `payload` is exactly `SessionDto`
- The offline queue (`~/.opencapx/queue`) is unchanged: if a hook cannot be sent it is first written to disk, and the app drains it on startup

### Delivery Contract (CLI → Core)

`opencapx hook --agent <kind>` is a **dumb pipe**: it POSTs the agent's **raw hook payload** as-is to
Core, injecting only three fields:

| Injected field | Purpose |
|---|---|
| `agent` | Declares the source agent (if already present, the original value is kept) |
| `__sent_at` | Delivery time (Unix seconds). Offline-queue replay writes to the database by this, otherwise "working at the time" would become "idle at the moment of replay" |
| `__rule` | The command-rule id hit by this rewrite (injected only on a hit). Core uses it to emit `rule.applied` (see [rules.md](rules.md)) |

**Second channel (stdout)**: when **the event is PreToolUse and the command hits a rule**, `opencapx hook` writes to stdout
the host's `updatedInput` response (the rewritten command + `permissionDecision: "allow"`) for the agent to write back into
the tool input. It runs **in parallel** with the HTTP uplink: the uplink is still the raw payload (the dumb-pipe contract is unchanged), and stdout is written only on a hit.
For rules and trust boundaries see [rules.md](rules.md).

Parsing, state determination, and transcript completion are **done once, only on the Core side**. The CLI side does no normalization — once
the CLI converted it to a DTO, raw fields like `transcript_path` / `tool_input` would be lost forever (without reading
the transcript you cannot tell "finished" from "asking a question").

### Event Name Mapping (per-agent)

Known agents go through an **event-name table** (compared after lowercasing uniformly, compatible with PascalCase / camelCase / snake_case):

| Event name | State |
|---|---|
| `SessionStart` / `agentSpawn` / `SessionEnd` | `idle` |
| `UserPromptSubmit` / `BeforeAgent` / `PreToolUse` / `PostToolUse` / `BeforeTool` / `AfterTool` | `working` |
| `Notification` / `PermissionRequest` | `waiting` |
| `Stop` / `AfterAgent` | `done` |
| `SubagentStop` | **Ignore the entire event** (a sub-agent ending does not mean the main session changed, otherwise there is a done→working flicker) |

Cursor (`conversation_id` / `workspace_roots`), Windsurf (`trajectory_id`), Grok (camelCase),
and Antigravity (**no event names; inferred from "which fields are present"**) each have their own field conventions. An event name not in the table falls back to
keyword heuristics, and custom agents go entirely through the heuristics.

### transcript Completion

Claude / Droid payloads carry `transcript_path`. Core reads only a **tail window** of the file (512KB):

- The last assistant segment → decide whether Stop means "finished" or "asking a question" (`looks_like_question`: it looks only at the last sentence,
  first excluding polite sign-offs); on a match it rewrites `done` back to `waiting`; when the message is empty it fills in a closing summary.
- `model` → most hook payloads do not carry the model name, so it is filled in from the transcript tail (throttled to 30s per session).

### Retention and Archiving

Retention is **state-machine-based**, not a one-size-fits-all TTL:

| State | Active duration |
|---|---|
| `done` | 30s (linger a moment so the user sees it) |
| `idle` | 600s |
| `working` / `waiting` | 900s (a timeout is treated as the agent being dead: no Stop sent) |

Expired sessions are first written to `session_archive` by a background sweep (every 60s) and then deleted from `sessions`,
and the archive is kept for 90 days (`ARCHIVE_KEEP_DAYS`). The session history UI reads `session_archive`.

## Distribution

```text
Event Bus (Rust, tokio broadcast channel)
 ├── Tauri emit("opencapx-event")   → pet overlay / settings window
 ├── SQLite events table             → audit/debug, ring-buffer retention 14 days
 ├── plugin broadcast core.event notification → plugins subscribed to that type
 └── core internal subscribers        → Agent Manager state, notification decisions, Tray tooltip
```

Subscriptions are filtered by `type` prefix (e.g. the pet subscribes only to `agent.*` + `capability.*`).

## Pet State Mapping

Core maintains only logical state, mapped to the animation states declared in the pet's Manifest:

```text
agent.started   → idle      agent.working   → working
agent.thinking  → thinking  agent.waiting   → waiting
agent.completed → success   agent.error     → error
(60s no events)    → sleeping
```

The same state can look completely different across pets — this is the pet plugin's decoupling point, and Core is unaware of it.
