# Tray Menu

The tray is the "visible without clicking" layer: an icon badge + a tooltip summary + a native menu that opens on click.
It and the bubble are **one source in different shells**: the same set of sessions, the same ordering, the same copy rules — but one is a native
`NSMenu` (no CSS) and the other is a WebView.

## Structure

```text
1 waiting for you · 2 working                  ← summary row (disabled); the empty state is "No Active Agents"
▸ OpenCapX · main · 1 waiting for you · 2 working   ← project submenu (parent enabled, no action)
    ⚠ Gemini CLI · needs permission to use Bash
    ● Claude Code · Edit src/bubble.ts
▸ beta · release/1.0 · 1 working
    ● Cursor · Bash pnpm build
◐ Grok Build · needs permission (no cwd)       ← sessions without a cwd stay at the top level
──────────────
Clear Finished / Show Pet / Open Settings / Quit
```

## Rules

- **Grouping**: by the full `cwd` (which distinguishes same-named projects). The group header is
  `project · branch · N waiting for you · M working`, with zero values omitted; when both waiting and working are 0 it degrades to `N done`
  — otherwise you would get an empty-shell parent with only the project name, where you cannot tell what is inside without expanding it.
  The branch comes from `core::project` (reads `<cwd>/.git/HEAD` only, caches per cwd for 30s, does not spawn a git process).
- **Ordering**: groups follow "the appearance order of the first session in the group" — sessions have already passed through
  `agent::sort_sessions` (waiting first), so projects "where someone is waiting" naturally sort to the top, using the same rule and the same ordering as the bubble
  group header. Rows inside a group **omit the project name** (the parent already shows it), so messages are truncated less often.
- **Sessions without a `cwd`** (old data / `evt-*`) stay at the **top level** rather than being forced into an "uncategorized" submenu.
- **The parent must have `enabled=true`**: on macOS a disabled parent cannot open its submenu. Tauri's `Submenu`
  has no action slot, so clicking it only expands — it cannot accidentally trigger an action and does not need permissions.
- **Rebuilds are throttled**: if the menu content signature (language / show pet / clearable / summary / structure including group headers) is unchanged,
  it returns immediately; switching branches changes the signature and triggers a refresh.
- Session rows are **disabled** (display only). This is a deliberate trade-off for now: making them clickable (jump to terminal / open project)
  belongs to the "actionability" track and is outside this design.

## Code

| Location | Responsibility |
|---|---|
| `core/tray.rs` | Pure functions + unit tests: `sections` (grouping), `project_header` (group header), `session_label` / `session_label_in_project` (row copy), `summary_text`, `badge_for`, `tooltip_text` |
| `main.rs::refresh_tray_menu` | Builds the menu (submenus, summary, action items), compares signatures, writes `TrayState` |
| `main.rs::refresh_tray_status` | Icon badge + tooltip (the part visible without clicking) |
| `core/project.rs` | The branch and short path in the group header |

## Verification

Rust unit tests cover: group ordering, group headers including the branch and zero-value omission, the fallback when everything is done, single-session projects still forming a group,
sessions without a `cwd` staying at the top level, and row copy (with/without the project name). The menu itself is native UI with no DOM to probe
— the structure is guaranteed by these pure-function tests, and `refresh_tray_menu` only wires them onto the `Submenu`.
