//! Install the stable shim into PATH as a global `opencapx` command.
//!
//! The symlink points at `~/.opencapx/bin/opencapx` (`hooks::shim_path`), never at the app
//! bundle: the shim refreshes itself atomically (tmp+rename) at app start / connect / hook,
//! so the link survives app moves, reinstalls and updates. `/usr/local/bin` is the directory
//! on the default PATH of macOS and Linux where a user CLI conventionally lives; when it is
//! root-owned, macOS falls back to the standard authorization dialog (`osascript … with
//! administrator privileges`). Windows has no equivalent location — the feature reports
//! unsupported there and the caller prints the manual PATH step instead.

use std::path::{Path, PathBuf};

/// `/usr/local/bin/opencapx` — on the default PATH of macOS and Linux.
pub fn target_path() -> PathBuf {
    PathBuf::from("/usr/local/bin").join(exe_name())
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "opencapx.exe"
    } else {
        "opencapx"
    }
}

/// Whether a global install is possible on this platform.
pub fn supported() -> bool {
    !cfg!(windows)
}

/// State of the global command, for the Settings row.
#[derive(serde::Serialize)]
pub struct Status {
    pub supported: bool,
    pub installed: bool,
    /// A file (or foreign symlink) already occupies the target — install refuses to clobber it.
    pub foreign: bool,
    pub target: String,
    pub shim: String,
}

pub fn status() -> Status {
    status_at(&crate::hooks::shim_path(), &target_path())
}

fn status_at(shim: &Path, target: &Path) -> Status {
    let installed = is_ours(shim, target);
    Status {
        supported: supported(),
        installed,
        foreign: target.symlink_metadata().is_ok() && !installed,
        target: target.display().to_string(),
        shim: shim.display().to_string(),
    }
}

/// Ours = a symlink whose stored path is the shim (covers a broken link) or one that
/// resolves to the same file after canonicalization. A regular file is never ours.
fn is_ours(shim: &Path, target: &Path) -> bool {
    match std::fs::read_link(target) {
        Ok(stored) => stored == shim || crate::hooks::same_file(shim, target),
        Err(_) => false,
    }
}

/// Install the symlink. `elevate` (macOS) uses the system authorization dialog when the
/// target directory is not writable; without it the error carries the `sudo` command instead.
pub fn install(elevate: bool) -> Result<String, String> {
    if !supported() {
        return Err("not supported on this platform — add the shim directory to PATH manually".into());
    }
    let shim = crate::hooks::ensure_shim().map_err(|e| format!("cannot refresh the stable CLI copy: {e}"))?;
    install_at(&shim, &target_path(), elevate)
}

fn install_at(shim: &Path, target: &Path, elevate: bool) -> Result<String, String> {
    if !shim.is_file() {
        return Err(format!("stable CLI copy is missing at {}", shim.display()));
    }
    if is_ours(shim, target) {
        return Ok(format!("already installed: {}", target.display()));
    }
    if target.symlink_metadata().is_ok() {
        return Err(format!(
            "{} already exists and was not created by OpenCapX — move it away first",
            target.display()
        ));
    }
    match symlink(shim, target) {
        Ok(()) => Ok(format!("installed: {} -> {}", target.display(), shim.display())),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            if elevate {
                elevate_shell(&link_command(shim, target))?;
                Ok(format!("installed: {} -> {}", target.display(), shim.display()))
            } else {
                Err(format!(
                    "{} is not writable — run: sudo {}",
                    target.parent().unwrap_or(target).display(),
                    link_command(shim, target)
                ))
            }
        }
        Err(e) => Err(format!("failed to create the symlink: {e}")),
    }
}

/// Remove the symlink. Only ever removes our own; a foreign file is left alone.
pub fn uninstall(elevate: bool) -> Result<String, String> {
    if !supported() {
        return Err("not supported on this platform".into());
    }
    uninstall_at(&crate::hooks::shim_path(), &target_path(), elevate)
}

fn uninstall_at(shim: &Path, target: &Path, elevate: bool) -> Result<String, String> {
    if !is_ours(shim, target) {
        return if target.symlink_metadata().is_ok() {
            Err(format!(
                "{} exists but was not created by OpenCapX — leaving it alone",
                target.display()
            ))
        } else {
            Ok(format!("not installed ({})", target.display()))
        };
    }
    match std::fs::remove_file(target) {
        Ok(()) => Ok(format!("removed: {}", target.display())),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            if elevate {
                elevate_shell(&remove_command(target))?;
                Ok(format!("removed: {}", target.display()))
            } else {
                Err(format!("permission denied — run: sudo {}", remove_command(target)))
            }
        }
        Err(e) => Err(format!("failed to remove the symlink: {e}")),
    }
}

/// Whether a runnable `opencapx` already resolves on PATH. Any installation counts — a
/// different one still lets the user invoke the command.
pub fn on_path() -> bool {
    let name = exe_name();
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate = dir.join(name);
                candidate.is_file() && executable(&candidate)
            })
        })
        .unwrap_or(false)
}

#[cfg(unix)]
fn symlink(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn symlink(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "symlinks are not used on Windows"))
}

#[cfg(unix)]
fn executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(windows)]
fn executable(_p: &Path) -> bool {
    true
}

/// Single-quote a path for `sh` (apostrophes become `'\''`).
fn shell_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
}

fn link_command(shim: &Path, target: &Path) -> String {
    format!("ln -sfn {} {}", shell_quote(shim), shell_quote(target))
}

fn remove_command(target: &Path) -> String {
    format!("rm -f {}", shell_quote(target))
}

/// Run a shell command through the macOS authorization dialog.
#[cfg(target_os = "macos")]
fn elevate_shell(shell_command: &str) -> Result<(), String> {
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        shell_command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("could not start osascript: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    if err.contains("-128") {
        Err("authorization was cancelled".into())
    } else {
        Err(format!("authorization failed: {}", err.trim()))
    }
}

#[cfg(not(target_os = "macos"))]
fn elevate_shell(_shell_command: &str) -> Result<(), String> {
    Err("elevation is only available on macOS — run the printed sudo command manually".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("opencapx-cli-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn fake_shim(dir: &Path) -> PathBuf {
        let shim = dir.join("shim-opencapx");
        std::fs::write(&shim, b"#!/bin/sh\n").unwrap();
        shim
    }

    #[test]
    fn install_uninstall_roundtrip() {
        let dir = scratch("roundtrip");
        let shim = fake_shim(&dir);
        let target = dir.join("bin").join("opencapx");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();

        assert!(!status_at(&shim, &target).installed);
        let msg = install_at(&shim, &target, false).unwrap();
        assert!(msg.starts_with("installed"), "unexpected message: {msg}");
        assert_eq!(std::fs::read_link(&target).unwrap(), shim);
        assert!(is_ours(&shim, &target));

        // Idempotent: a second run reports rather than recreates.
        assert!(install_at(&shim, &target, false).unwrap().contains("already"));

        let s = status_at(&shim, &target);
        assert!(s.installed && !s.foreign);

        assert!(uninstall_at(&shim, &target, false).unwrap().starts_with("removed"));
        assert!(!target.exists());
        assert!(uninstall_at(&shim, &target, false).unwrap().contains("not installed"));
    }

    #[test]
    fn refuses_to_clobber_a_foreign_file() {
        let dir = scratch("foreign");
        let shim = fake_shim(&dir);
        let target = dir.join("opencapx");
        std::fs::write(&target, b"someone else's tool").unwrap();

        assert!(install_at(&shim, &target, false).is_err());
        assert!(uninstall_at(&shim, &target, false).is_err());
        // Untouched, and reported as foreign rather than installed.
        assert_eq!(std::fs::read(&target).unwrap(), b"someone else's tool");
        let s = status_at(&shim, &target);
        assert!(s.foreign && !s.installed);
    }

    #[test]
    fn a_missing_shim_is_an_error() {
        let dir = scratch("noshim");
        let target = dir.join("opencapx");
        assert!(install_at(&dir.join("missing"), &target, false).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn shell_quote_survives_spaces_and_apostrophes() {
        assert_eq!(shell_quote(Path::new("/Users/a b/x'y")), "'/Users/a b/x'\\''y'");
        assert_eq!(link_command(Path::new("/a b/s"), Path::new("/c d/t")), "ln -sfn '/a b/s' '/c d/t'");
    }
}
