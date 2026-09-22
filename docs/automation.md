# Automation Spec

Automation (i2 §15): **Event → Rule → Action**. It does not build a separate channel; it subscribes directly to the Event Bus in [events.md](events.md)
— once the B10 event-subscription lifecycle landed, events such as `capability.event` were already in the stream, and Automation is merely one consumer on that stream.

## Model

```text
Event Bus ──▶ Rule match (when: event type + payload conditions) ──▶ action (notify | say)
```

- Rules live in `~/.opencapx/automation.json`, hand-editable by humans; the settings page and the CLI share the same file
- The engine **hot-reloads** on the file's mtime: changes take effect immediately, without restarting Core
- Every trigger emits an `automation.rule_fired` event (it enters the Activity Timeline and is auditable)
- Actions expose only Core's native low-risk surface (`notify` / `say`). **Arbitrary capability execution is not exposed** — that would require a permission chain, deferred to v2

## Rule File

```json
{
  "rules": [
    {
      "id": "rule-18f0a2c3",
      "enabled": true,
      "when": {
        "event": "capability.event",
        "match": { "capability": "file.watch", "event": "created", "path_contains": "/Downloads/" }
      },
      "then": { "action": "notify", "title": "New file", "body": "A new file appeared in Downloads" }
    }
  ]
}
```

Fields:

| Field | Required | Description |
|---|---|---|
| `id` | yes | Rule identifier; `opencapx automation add` generates `rule-<hex>` automatically |
| `enabled` | no | Defaults to `true`; when `false` the engine skips the rule |
| `when.event` | yes | Event type, such as `capability.event` / `agent.completed` (see the events.md catalog) |
| `when.match` | no | Payload condition object; when omitted, matching is by event type only |
| `then.action` | yes | `notify` or `say` |

## Match Semantics

- `when.event` must be **exactly equal** to the event type
- Each entry in `when.match` matches a payload field:
  - Plain key name (such as `capability`) → field value **equality** (JSON deep comparison)
  - Key name ending in `_contains` (such as `path_contains`) → field string **substring** match
  - Field missing / type mismatch → **no match** (no error, no panic)
- No `when.match` → every occurrence of that event type matches

Path-prefix scenarios for `file.watch` use `_contains`: i2 §15's "a new file lands in ~/Downloads" is exactly
`{"path_contains": "/Downloads/"}`.

## Actions

| action | Fields | Effect |
|---|---|---|
| `notify` | `title` (optional), `body` (required) | System notification; also emits a `notification.posted` event (agentId = `automation`) |
| `say` | `text` (required) | Pet bubble (`pet.say` event + overlay text popup) |

Unknown `action`: rejected by CLI add; if one appears in a hand-edited file, the event is **recorded but not executed** (`automation.rule_failed`).

## Trigger Rate Limiting

The same rule **does not re-trigger within 5s** (`RULE_COOLDOWN`). Purpose: a batch of files landing in Downloads / high-frequency diffs from file watching
will not flood the notification list. Rate limiting only affects repeat triggers; it does not affect different rules running in parallel.

## Events

| type | payload | Description |
|---|---|---|
| `automation.rule_fired` | `ruleId`, `event`, `action` | Rule matched and executed (audit) |
| `automation.rule_failed` | `ruleId`, `error` | Bad action in a hand-edited file; the event is recorded but not executed |

Both events enter the Activity Timeline (category `system`).

## CLI

Does not depend on MCP / UI, which makes it easy to script:

```text
opencapx automation list
opencapx automation add '{"when":{"event":"agent.completed"},"then":{"action":"say","text":"All done"}}'
opencapx automation remove <rule-id>
```

## Mapping to the i2 §15 Examples

| i2 example | Rule |
|---|---|
| Editor opens → start a Coding Agent | v1 only does event→notification/bubble; starting an Agent requires capability execution, deferred to v2 |
| CPU > 90% → Notify Agent | `when.event` = the corresponding metric event, `then.action` = `notify` |
| ~/Downloads/new-file.pdf appears → Analyze PDF | `capability.event` + `path_contains` + `notify`; analysis deferred to v2 |
