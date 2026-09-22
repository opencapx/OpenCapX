// The OpenCapX opencode plugin. Installed by `opencapx connect opencode`; delete this directory to uninstall.
//
// Why "a standalone module + registered in opencode.json's plugin array" instead of
// a directory plugin under ~/.config/opencode/plugin/:
//
//   opencode's execution order is "config plugins (in array order) → directory plugins". This plugin must come
//   **before** any plugin that **replaces** output.args wholesale. The reason: they swap output.args for a new
//   object, while the host executes the original args captured in the closure — a wholesale replacement is silently
//   ignored. We can only reach the object that is actually executed by running first and
//   mutating properties in place.
//
// Accordingly, `connect opencode` **prepends** this directory's path to the plugin array.
import { spawnSync } from "node:child_process"

const BIN = "__OPENCAPX_BIN__"

const sessionId = (dir) => "opencode:" + (dir || "default")

const report = (dir, state) => {
  try {
    spawnSync(BIN, [
      "hook",
      "--agent",
      "opencode",
      "--event",
      state,
      "--session",
      sessionId(dir),
      "--project",
      dir || "",
    ])
  } catch (e) {}
}

export const OpenCapX = async ({ directory }) => ({
  "session.created": async () => report(directory, "working"),
  "session.idle": async () => report(directory, "done"),

  // Command rules (see docs/rules.md): on a match, hand the command to `opencapx rewrite` to rewrite it in place.
  "tool.execute.before": async (input, output) => {
    const cmd = output && output.args && output.args.command
    if (typeof cmd !== "string" || !cmd) return
    let rewritten = ""
    try {
      const p = spawnSync(BIN, ["rewrite", cmd], {
        cwd: directory || undefined,
        encoding: "utf8",
      })
      if (p.status !== 0) return
      rewritten = String(p.stdout || "").trim()
    } catch (e) {
      return
    }
    if (!rewritten || rewritten === cmd) return
    output.args.command = rewritten

    // Audit: this plugin comes first, so by the time other plugins forward the tool event to `opencapx hook`,
    // the command has already been rewritten by us and the rule no longer matches — so on a match, this
    // reports once more with the **original** command, letting Core emit rule.applied (otherwise opencode's
    // rewrite would be "unaudited"). One extra spawn only on a match; the no-match path has zero overhead.
    try {
      spawnSync(BIN, ["hook", "--agent", "opencode"], {
        input: JSON.stringify({
          hook_event_name: "PreToolUse",
          hook_source: "opencapx-opencode-plugin",
          cwd: directory || undefined,
          tool_name: "bash",
          tool_input: { command: cmd },
        }),
        encoding: "utf8",
      })
    } catch (e) {}
  },
})
