# Bubble

The bubble is the status panel next to the pet: it compresses each Agent session into a one-line summary, and requests
awaiting confirmation are clicked right here. It shares the same window as the pet (`pet`) and is part of the overlay.

## Data Sources

The bubble only reads `sessions` (merged from Core's `agent.*` events) and does not issue requests itself:

| Inline element | Source |
|---|---|
| Status dot | `state` (working / waiting / done / idle …); for colors see `--bubble-accent` and `MOOD_COLORS` |
| Agent name | `agent` (the product name, e.g. `Claude Code`) |
| Summary | Prefers a real message (`text` / `prompt` / the tool call `tool_name` + `tool_input`), falling back to the theme's flavor text only when there is none |
| Body | The second line: what the agent **says itself** (the last assistant text in the transcript tail). `message` answers "what it is doing" while the body answers "what it said" — the two are kept separate and do not displace each other |
| Model | A badge: from the hook payload or the transcript tail (it can keep up even with a mid-session `/model` switch) |
| Elapsed | `updatedAt` → `elapsedText()` |

The row structure is **vertical**: the first line (who · doing what) is always a single line with ellipsis, the body starts
another line, and the option buttons take yet another — you must not use `flex-wrap` to wrap long messages, otherwise the model/metrics get squeezed onto the second line too and the main row falls apart.

The body uses the `speech` field (rather than reusing `message`), because **a done row's `message` is displaced by the
celebration text ("🎉 Done!")** — only `speech` can keep the agent's last sentence on screen. It is a sticky field:
it is produced only at the moment the transcript is read, and later tool events do not carry it, so `event::ingest` falls back
to the old value and the second line is not erased by the next `PreToolUse`. Disk reads are throttled to 30s per session.

## Grouping, Ordering, and Density

- **Grouping**: grouped by `cwd` (the full working directory) — the group header is "project · branch · N waiting for you · M working",
  with zero values omitted (the same rule as the tray). Old data with an empty `cwd` falls back to `project` (the last segment). Same-named
  projects (`~/a/OpenCapX` and `~/b/OpenCapX`) are distinguished by short path. The group header does not collapse and is shown even with
  only one project — the branch itself is the information you want to see. Slicing happens before grouping, so a "whole group being cut" does
  not leave an empty group header.
- **Ordering**: `core::agent::order_key` is the **single** expression — priority (waiting > working > done > idle)
  + most recently active first + id as a tiebreaker (the same input sorts identically twice). The DTO hands it to the front end as `order`,
  and the front end only does lexicographic comparison without reimplementing the policy; after the front end locally merges an `opencapx-event`
  into the list it re-sorts by it too, so a newly arrived waiting item floats to the top immediately. The tray's session rows use the same
  `sort_sessions` and are displayed grouped by project (see [tray.md](tray.md)).
- **Density** (Settings → Bubble): `tight` has only the main row; `standard` (default) adds the agent body;
  `rich` further adds the model badge, elapsed time, and project short path.
- **Truncation**: rows cut by `maxRows` are hinted at the bottom with "N more sessions" instead of silently disappearing.
- **Where the branch comes from**: `core::project` only reads `<cwd>/.git/HEAD` (it does not spawn a git process), supporting
  worktree / submodule `gitdir:` and detached HEAD (7-character short hash); the result is cached per cwd for 30s
  (the bubble re-renders every second). If it cannot be read, it is `None` and the group header shows only the project name — the branch is
  enhancement information, not a dependency. The full `cwd` is used only as the grouping key; only the short path enters the DOM and `title`.

Clicking a row expands the full message (the expand gesture is added only to rows that are actually truncated: `scrollWidth > clientWidth`).
The expanded state is held externally (`expandedIds`) because the bubble re-renders every second.

## Three Modes (`mode`)

| Mode | Content |
|---|---|
| `list` | Lists sessions row by row |
| `carousel` | One screen at a time, paged with dots at the bottom |
| `compact` | Shows only the two counts `N working · M waiting` |

## Ten Themes (`bubbleTheme`)

A theme = a set of CSS variables, not an image. The variables are defined in `src/bubble-themes.css` under
`[data-bubble-theme=…]`, and both `index.html` (the desktop bubble) and `settings.html` (the Settings page preview)
link this one file — so the preview block seen in Settings is the real bubble from the desktop, with no drift.
The theme list comes from `BUBBLE_THEMES` in `src/bubble.ts` (to add a theme: change this + a block of CSS).

Themes swap not just colors but also **shape**:

| Variable | Effect |
|---|---|
| `--bubble-radius` | Outline corner radius (0 → 26px) |
| `--bubble-clip` | Cut corners/bevels (`clip-path`) |
| `--bubble-border-w` / `--bubble-border-style` | Border width and line style (dashed possible) |
| `--bubble-edge-w` | The thickness of the edge facing the pet — that is the "speaking direction" indicator |
| `--bubble-pad-y` / `--bubble-pad-x` | Padding (tight/loose is also part of the outline) |
| `--bubble-bg-image` | Texture/gradient (grid, scanlines, ruled lines, dither, radial light) |
| `--bubble-blur` | Backdrop blur; turned off for opaque themes (paper/terminal/pixel) |
| `--bubble-shadow` | Outer shadow |
| `--bubble-filter` | Stroke/glow/displaced shadow for clipped shapes (see below) |
| `--bubble-chip-radius` | Corner radius of inner small controls (badges/option buttons/done row) |

**Why clipped shapes use `filter` rather than `box-shadow`**: `clip-path` clips the element's
`box-shadow` along with it (it belongs to the element's own painting), so the stroke and glow of beveled/stepped shapes use
`filter: drop-shadow(...)` instead — it follows the **silhouette** and is not clipped. Similarly, WebKit has a precedent for
rendering anomalies when `filter` and `backdrop-filter` appear on the same element, so every theme that uses
`filter` (engineer / explorer / cyber / pixel) is always `--bubble-blur: none`.
This constraint is guarded by a probe: `fi=y → bf=-` holds for all 10 themes.

Clipped shapes must also ensure that content is not cut off: the probe additionally checks both "the bubble-center hit test follows the silhouette"
and "the content rows are still inside the bubble box", and all 10 themes pass (the cut corners are all at the corners, still some distance from the text).

| id | Positioning |
|---|---|
| `chef` | Warm kitchen |
| `engineer` | Cool-blue terminal feel |
| `wizard` | Violet magic |
| `explorer` | Jungle green |
| `scientist` | Cyan monospace |
| `minimal` | Minimalist, for realistic 3D models, not stealing attention |
| `paper` | Light paper (the only light theme) |
| `cyber` | Neon magenta/cyan: 12px bevels on all four corners + no border (stroke glow via drop-shadow) |
| `terminal` | Green-on-black monospace: square corners + a 6px thick facing edge (like a cursor block) + scanlines |
| `pixel` | Pixel stepped cut corners + a 2px bright edge + a 4px hard displaced shadow + dither texture, paired with a pixel pet |

Shape overview (radius / clip / border / facing edge / padding):

| Theme | radius | clip | border | edge | padding |
|---|---|---|---|---|---|
| `chef` | 18px | — | 1px | 4px | 9/11 |
| `engineer` | 5px | 14px top-right cut | 1px dashed | 3px | 8/10 |
| `wizard` | 22px | — | 1px + inner ring | 4px | 10/13 |
| `explorer` | 9px | 12px bottom-right cut | 2px | 4px | 9/11 |
| `scientist` | 26px | — | 1px | 3px | 10/12 |
| `minimal` | 12px | — | 1px | 2px | 8/11 |
| `paper` | 2px | — | 1px | 3px | 9/12 |
| `cyber` | 0 | 12px bevels on all corners | 0 | 5px | 9/11 |
| `terminal` | 0 | — | 1px | 6px | 8/10 |
| `pixel` | 0 | 4px steps on all corners | 2px | 4px | 9/10 |

**The key to light themes**: the bubble's interior does not hardcode `rgba(255,255,255,…)` but goes through
`--bubble-fg / --bubble-dim / --bubble-sep / --bubble-chip-bg / --bubble-nav /
--bubble-warn-fg`. To add a new light theme, just override these variables; there is no need to change selectors one by one.

## Position (`bubblePos`)

`right` (default) / `left` / `top` / `bottom`. The pet is pinned to one corner of the window, and the bubble occupies the
remaining space next to it; the edge facing the pet is thickened into the accent color, which is the "speaking direction" indicator.

A border is used instead of a triangular tail: when arranged vertically the bubble needs to scroll (`overflow-y: auto`), and `overflow`
would clip the tail drawn by `::after`.

### Window Geometry

The window is `440×320` (logical pixels, see the `pet` window in `tauri.conf.json`). When switching to a vertical arrangement:

- `bottom`: the window grows to `pet border + 320` (growing downward needs no headroom), and the bubble gets the full
  height budget. The pet is at the top-left of the window, its position unchanged.
- `top`: the window stays `440×320`. The pet must move to the bottom of the window, so the window has to be pushed up by the same distance; if
  the pet is not far enough from the top of the screen, macOS clamps the window position and the pet shifts down a little (measured up to ~55px).
  This is a system limitation, not a layout bug.

When switching sides, `applyBubblePos()` moves the window in the opposite direction so the pet stays at its **original position on screen**. It
records "the position the pet should be at" (`petAnchor`) rather than the previous actual position — otherwise the offset produced by clamping would be
inherited by the next side switch. When the user drags the pet the anchor is invalidated and re-recorded.

It needs the three permissions `core:window:allow-set-position` / `allow-outer-position` / `allow-scale-factor`
(`capabilities/default.json`); if one is missing, it silently does nothing.

## Waiting for Confirmation (ask)

`renderAsk()` appends the confirmation buttons into `#bubble`, while `paintBubble()` rewrites
`innerHTML` every second. So `paintBubble` must first yield to a pending prompt (see the `askBox.isConnected`
check), otherwise the buttons are wiped within 1 second. This affects `opencapx.ask`, plugin install confirmation, and permission confirmation.

## Related Settings

| key | Description |
|---|---|
| `bubbleEnabled` | Master switch for the bubble |
| `bubblePos` | Which side of the pet |
| `bubbleTheme` | Theme |
| `mode` | Presentation mode |
| `maxRows` | Maximum number of rows (1–10) |
| `bubbleDuration` | Seconds after the last new event before auto-hiding; 0 = always visible |
