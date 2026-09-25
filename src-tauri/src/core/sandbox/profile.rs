//! seatbelt profile generation: effective-profile policy, SBPL emission, the installer deny lists, and the CLI profile shape.
//! Mechanical move from core/sandbox.rs.

use super::*;

/// Per-plugin data directory root (`~/.opencapx/plugin-data/<id>/`); tests can override with `OPENCAPX_PLUGIN_DATA_DIR`.
pub fn plugin_data_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCAPX_PLUGIN_DATA_DIR") {
        return PathBuf::from(dir);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("plugin-data"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-plugin-data"))
}

/// review F2 — the in-sandbox TMPDIR target (`plugin-data/<id>/tmp`); the caller creates the directory then passes it along with SandboxSpec.
pub fn plugin_tmp_dir(plugin_id: &str) -> PathBuf {
    plugin_data_root().join(plugin_id).join("tmp")
}

/// S5c — enforcement decision (for the seam tests, see the policy matrix in tests):
/// not declared → None; not macOS → None; trusted and switch off → None (opt-out allowed);
/// trusted and switch on / not trusted (**forced**) → Some(profile).
pub fn effective_profile(
    plugin_id: &str,
    decl: Option<&SandboxDecl>,
    trusted: bool,
    enforcement_switch: bool,
) -> Option<String> {
    let decl = decl?;
    if !cfg!(target_os = "macos") {
        return None;
    }
    if trusted && !enforcement_switch {
        return None;
    }
    Some(sandbox_profile(plugin_id, decl))
}

/// Production entry: canonicalize data_dir (symlink chains `/tmp→/private/tmp`, `/var→/private/var`
/// would make seatbelt subpath matching miss; in real testing raw-path writes were wrongly denied).
pub fn sandbox_profile(plugin_id: &str, decl: &SandboxDecl) -> String {
    let raw = plugin_data_root().join(plugin_id);
    let canonical = canonicalize_lossy(&raw);
    sandbox_profile_for(&canonical, decl)
}

/// Canonicalize the nearest existing ancestor and append the remaining components as-is (works even when the path does not exist yet).
pub(crate) fn canonicalize_lossy(path: &Path) -> PathBuf {
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(c) = std::fs::canonicalize(&cur) {
            let mut out = c;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            cur.parent().map(|p| p.to_path_buf()),
            cur.file_name().map(|n| n.to_os_string()),
        ) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                suffix.push(name);
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// S5b — profile generation (pure function; the golden snapshot test passes data_dir directly).
/// Calibrated rules (measured): **reads are globally allowed** — macOS firmlink/synthetic paths make a read-subpath allowlist
/// miss shared libraries (once causing homebrew python to SIGABRT); reads are not what this layer guards anyway, **write and network are the boundary**.
/// write = plugin-data (when the declaration contains `plugin-data`; path uses the canonical form); network follows the declaration.
pub fn sandbox_profile_for(data_dir: &Path, decl: &SandboxDecl) -> String {
    let write_data = decl
        .fs
        .as_ref()
        .map(|f| f.write.iter().any(|w| w == "plugin-data"))
        .unwrap_or(false);
    let writes: Vec<PathBuf> = if write_data {
        vec![data_dir.to_path_buf()]
    } else {
        Vec::new()
    };
    let allow_network = decl.network.as_deref().unwrap_or("none") == "out";
    profile_with(&writes, allow_network)
}

/// Shared seatbelt profile: the calibrated header (process exec/fork + sysctl + mach-lookup +
/// global reads) then one `file-write*` rule per writable path, then the network rule last.
/// Quote/backslash escaping keeps paths containing `"` from breaking the SBPL syntax.
pub fn profile_with(write_paths: &[PathBuf], allow_network: bool) -> String {
    let mut p = String::from("(version 1)\n(deny default)\n");
    p.push_str("(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n");
    p.push_str("(allow file-read*)\n");
    // Device exemptions: `> /dev/null` opens the path (a plain fd write would not be checked),
    // and `/dev/fd` backs process substitution — denying those breaks ordinary shell idioms.
    p.push_str("(allow file-write* (literal \"/dev/null\") (subpath \"/dev/fd\"))\n");
    for w in write_paths {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(w)
        ));
    }
    if allow_network {
        p.push_str("(allow network*)\n");
    } else {
        p.push_str("(deny network*)\n");
    }
    p
}

fn sbpl_escape(p: &Path) -> String {
    p.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// Sandbox modes for the CLI runner.
/// - `Strict`: network denied, writes limited to scratch/TMPDIR/`--rw` — the "run this unknown
///   thing" fence. Breaks installers (they need the network and write into $HOME).
/// - `Installer`: network open and `$HOME` writable (installers install), but a curated deny
///   list keeps secret material unreadable and persistence hot spots unwritable. Seatbelt
///   applies later rules last, so the denies below override the broad allows (verified live).
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Strict,
    Installer,
}

/// Secret material: denied for reads in installer mode. The script gets the network, so
/// anything readable can be exfiltrated — these are the crown jewels an installer has no
/// business reading. Paths are relative to `$HOME`.
pub(crate) const INSTALLER_DENY_READ: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".netrc",
    ".npmrc",
    ".docker/config.json",
    ".config/gh",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".pypirc",
    // cloud CLIs / VCS credential stores (kubeconfig carries cluster creds; git-credentials
    // is what `git config credential.helper store` writes; gcloud/azure hold refresh tokens)
    ".kube",
    ".git-credentials",
    ".config/gcloud",
    ".azure",
    ".zsh_history",
    ".bash_history",
    "Library/Keychains",
    "Library/Cookies",
    // browser profiles: cookies are Keychain-encrypted, but history/logins/sessions are not
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Firefox",
];

/// Persistence hot spots: denied for writes in installer mode. An installer that edits shell
/// rc files only loses the automatic PATH hint (it can print the line instead); malware that
/// wants to survive a reboot loses its footing. Paths are relative to `$HOME`.
pub(crate) const INSTALLER_DENY_WRITE: &[&str] = &[
    ".zshrc",
    ".zprofile",
    ".zshenv",
    ".zlogin",
    ".bashrc",
    ".bash_profile",
    ".profile",
    ".config/fish/config.fish",
    ".gitconfig",
    "Library/LaunchAgents",
];

/// Same idea, for absolute system persistence paths (all of these need root anyway — the deny
/// is belt-and-braces).
const INSTALLER_DENY_WRITE_ABS: &[&str] = &[
    "/Library/LaunchAgents",
    "/Library/LaunchDaemons",
    "/usr/lib/cron",
    "/var/at",
    "/etc/periodic",
];

/// Seatbelt profile for the CLI runner. See [`Mode`] for the two stances; `home` is passed in
/// (rather than read from the environment) so tests can exercise the deny list against a
/// throwaway home.
pub fn cli_profile(
    mode: Mode,
    home: &Path,
    scratch: &Path,
    tmpdir: Option<&Path>,
    rw: &[PathBuf],
    allow_net: bool,
) -> String {
    let mut p = String::from("(version 1)\n(deny default)\n");
    p.push_str("(allow process*)\n(allow sysctl-read)\n(allow mach-lookup)\n");
    p.push_str("(allow file-read*)\n");
    p.push_str("(allow file-write* (literal \"/dev/null\") (subpath \"/dev/fd\"))\n");
    p.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        sbpl_escape(&canonicalize_lossy(scratch))
    ));
    if let Some(t) = tmpdir {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(&canonicalize_lossy(t))
        ));
    }
    if mode == Mode::Installer {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(home)
        ));
    }
    for d in rw {
        p.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sbpl_escape(&canonicalize_lossy(d))
        ));
    }
    if mode == Mode::Installer {
        for rel in INSTALLER_DENY_READ {
            p.push_str(&format!(
                "(deny file-read* (subpath \"{}\"))\n",
                sbpl_escape(&home.join(rel))
            ));
        }
        for rel in INSTALLER_DENY_WRITE {
            p.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                sbpl_escape(&home.join(rel))
            ));
        }
        for abs in INSTALLER_DENY_WRITE_ABS {
            p.push_str(&format!(
                "(deny file-write* (subpath \"{}\"))\n",
                sbpl_escape(Path::new(abs))
            ));
        }
    }
    if mode == Mode::Installer || allow_net {
        p.push_str("(allow network*)\n");
    } else {
        p.push_str("(deny network*)\n");
    }
    p
}
