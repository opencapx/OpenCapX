# Third-party tool adapters

This holds the **per-tool directory** adapter source files — written to the user's machine by `opencapx connect <agent>`,
so the tool can connect to OpenCapX (report session state, rewrite commands by rule, and so on).

**The source files live here, not as inline Rust strings.** Earlier all templates were inlined in `src-tauri/src/hooks.rs`,
which meant every `{` had to be written `{{`, indentation as `\x20`, and quotes escaped — the output had no syntax highlighting, no lint,
and could not be diffed. Now they are real source files embedded at compile time with `include_str!`: still a single binary with no runtime dependencies,
but readable, reviewable, and diffable.

## Directory

| Directory | Target tool | Install location | Registration |
|---|---|---|---|
| `opencode/` | opencode | `~/.local/share/opencapx/adapters/opencode/` | the `plugin` array of `~/.config/opencode/opencode.json` (**prepended**) |
| `omp/` | Oh My Pi (omp) | `~/.omp/agent/extensions/opencapx.ts` | none — the host discovers the file itself |

## Placeholder

The `"__OPENCAPX_BIN__"` in the source files (quotes included) is replaced at write time with a JSON string literal of the OpenCapX binary.
The replacement **swaps the quotes along with it**, so backslashes in Windows paths are handled by JSON encoding too and cannot produce a malformed string.

## Why the opencode plugin must come first

opencode's execution order is "config plugins (in array order) → directory plugins". If this plugin comes after
one that **replaces `output.args` wholesale**, its `tool.execute.before` will stop taking effect:
the latter swaps `output.args` for a new object, while the host executes the original `args` captured in the closure — a wholesale replacement is silently
ignored, and our subsequent in-place edits land on the discarded object as well. So `connect` **prepends** this directory to the front.

## Adding an adapter

1. Create `adapters/<agent>/` and put the real source files there;
2. Add/change the corresponding entry in `spec()` in `src-tauri/src/hooks.rs`, and pull it in with `include_str!`;
3. If the tool's registration is not "edit the hooks field of settings.json", add the corresponding branch in `install` / `uninstall` /
   `is_installed` (see opencode's `OpencodePluginModule`).
