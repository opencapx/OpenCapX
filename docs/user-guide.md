# User Guide: Plugin Installation, Permissions, and Updates

Aimed at everyday users. Developers should see `plugin-signing.md` (signing toolchain) and `plugin-manifest.md` (manifest spec).

## Installing Plugins

Entry point: Settings page "Plugins" → Install, then choose a `.ocplugin` file (or a marketplace entry). Installation falls into three classes by trust state:

| Package state | Behavior |
|---|---|
| **Officially signed** (verified by the registry) | Installs directly |
| **Unsigned / unknown publisher** | Warning dialog, manual confirmation required; the "allow unsigned" toggle on the plugin page can be turned off, after which such packages are always rejected |
| **Tampered signature / revoked publisher** | **Never installs** (even the master toggle cannot bypass this) |

## Permission Confirmation

Plugins declare the permissions they need in the manifest (such as `image.read`). They are shown item by item at install time, each with a default:

- `ask`: prompt on every call (can be changed to "always allow / always deny" on the plugin page)
- `allow` / `deny`: the default behavior declared in the manifest

Decisions already made are remembered; when a plugin **adds** permissions, the update only re-prompts for the new ones.

## Plugin Settings

Plugins can declare settings (toggle / text / dropdown / number / secret, etc.); clicking the corresponding plugin in the sidebar's "Plugin Settings" area renders them as a graphical form:

- **Secret settings** are written to the OS keychain; the UI only shows "set / not set" and never echoes the contents
- Plugins that declare no settings can still have their configuration edited directly with the JSON editor

## Hotkeys

Entry point: Settings page "Hotkeys". Each bindable action is one row, with the current key combination shown in a pill on the right (blank means unbound); hotkeys are global (including when the app is in the background):

- Clicking a pill starts recording: press the combination and it saves directly; `Esc` cancels, and pressing `Delete` while recording unbinds it
- An already-used combination asks for confirmation before being overwritten; disabling a row stops that key from triggering and greys out the row
- Rows that failed to register are marked "not in effect" (usually because another app holds the combination); click to retry
- "Restore defaults" returns to the 3 factory bindings (panel / settings / show-hide pet); custom bindings are unaffected
- Bindings are stored in `~/.opencapx/hotkeys.json` and travel with workspace backups

## Updates

- **Same publisher** update with no permission changes → silent update; configuration and data are preserved
- Permission changes → only the newly added items are re-prompted
- **Publisher change** (signing key change) → full re-confirmation with a warning shown
- A failed update (corrupted download / insufficient disk space, etc.) does not damage the installed version; the old version remains usable

## Revocation and Disabling

When a publisher key is officially revoked, installed plugins are **disabled by default**: the process stops, a revocation banner is shown, and starting / updating is blocked. You can:

- Uninstall the plugin, or
- **Explicitly re-enable** it from the banner (at your own risk; recorded in the audit log) and watch for a fixed release

## Version Upgrades and Data Migration

On plugin upgrade, the host tells the plugin the "last successfully run version", and the plugin performs an **idempotent** data migration itself
(safe to re-run and to roll back); a failed upgrade does not advance the version record, so retrying is safe.

## Uninstalling

Uninstalling: stops the process → deletes the managed plugin files (the copy dropped by the `.ocplugin` install) → deletes configuration and secret fallback files → clears database records.

> Plugins installed via "directory install" (developer mode) keep their source directory as your own; uninstalling does **not** delete it.
