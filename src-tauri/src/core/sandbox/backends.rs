//! execution backends: scratch dirs, plain-run fallback, group kill, macOS seatbelt, Linux bubblewrap, installer overlays.
//! Mechanical move from core/sandbox.rs.

use super::*;

/// Flags are consumed until `--`; without `--`, the first token that is not a `--flag` starts
/// the command (so `sandbox sh -c '...'` works, while a command literally starting with `-`
/// still needs the explicit separator).
/// Same parse as `run_cli`, error as text for tests (they assert is_err, not messages).
#[cfg(test)]
pub(crate) fn parse_args(args: &[String]) -> Result<Parsed, String> {
    crate::core::cli::parse::<SandboxCli>("opencapx sandbox", args)
        .map(parsed_from)
        .map_err(|code| format!("clap exit code {code}"))
}

pub(crate) fn backend() -> Backend {
    #[cfg(target_os = "macos")]
    return if Path::new("/usr/bin/sandbox-exec").exists() {
        Backend::Seatbelt
    } else {
        Backend::Unavailable("sandbox-exec not found")
    };
    #[cfg(target_os = "linux")]
    return backend_linux();
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return Backend::Unavailable(
        "no backend on this platform in v1 (Windows: microVM tier planned, requires WHP)",
    );
}

/// Scratch dir handed to the child as its writable area; removed after the run.
/// Unique per run, not per process: concurrent runs in one process (parallel plugin calls or
/// tests) would otherwise share a dir, and the first to finish would delete another's live tree.
pub(crate) fn make_scratch() -> std::io::Result<PathBuf> {
    static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("opencapx-sandbox-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Unguarded path (fail-open). Same plumbing as the guarded path: inherited stdio,
/// forwarded exit code, optional timeout. The env policy still applies — a vanished fence
/// is no reason to also hand over the environment's secrets.
pub(crate) fn run_plain(command: &[String], timeout_secs: Option<u64>, env: EnvPolicy) -> i32 {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    apply_env_policy(&mut cmd, env);
    spawn_and_wait(&mut cmd, timeout_secs)
}

fn spawn_and_wait(cmd: &mut std::process::Command, timeout_secs: Option<u64>) -> i32 {
    // Own process group (pgid == child pid): a timeout must be able to kill the whole tree —
    // killing only the direct child leaves grandchildren (the actual script's helpers, or a
    // deliberately backgrounded survivor) running with the profile still attached. Normal
    // exits do NOT group-kill: an installer may legitimately leave a service running.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    match cmd.spawn() {
        Ok(mut child) => {
            if let Some(secs) = timeout_secs {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                loop {
                    match child.try_wait() {
                        Ok(Some(st)) => return st.code().unwrap_or(1),
                        Ok(None) => {
                            if std::time::Instant::now() >= deadline {
                                kill_group(child.id());
                                let _ = child.kill();
                                eprintln!(
                                    "opencapx sandbox: timeout after {secs}s; killed process group"
                                );
                                return 124;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        Err(e) => {
                            eprintln!("opencapx sandbox: wait failed: {e}");
                            return 1;
                        }
                    }
                }
            }
            child.wait().map(|st| st.code().unwrap_or(1)).unwrap_or(1)
        }
        Err(e) => {
            eprintln!("opencapx sandbox: spawn failed: {e}");
            127
        }
    }
}

/// SIGKILL the child's whole process group. Same idiom as core::process: probe first — a
/// reaped group's pgid can be recycled, so only kill while the group still exists (ESRCH →
/// skip); never target our own group. Windows uses taskkill /T.
fn kill_group(leader_pid: u32) {
    #[cfg(unix)]
    {
        let pgid = leader_pid as i32;
        if pgid > 1 && pgid != unsafe { libc::getpgrp() } && unsafe { libc::kill(-pgid, 0) } == 0 {
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &leader_pid.to_string(), "/T", "/F"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = leader_pid;
    }
}

/// Writes are limited to the scratch dir, the child's TMPDIR (tools expect a writable temp),
/// and explicit `--rw` dirs; reads stay open per the calibrated header. Persistence targets
/// (HOME, `~/Library/LaunchAgents`, dotfiles) and the network stay closed.
#[cfg(target_os = "macos")]
pub(crate) fn run_seatbelt(parsed: &Parsed, scratch: &Path) -> std::io::Result<i32> {
    let home = crate::core::home_dir()
        .map(|h| canonicalize_lossy(&h))
        .unwrap_or_else(|| PathBuf::from("/"));
    let tmpdir = std::env::var_os("TMPDIR").map(PathBuf::from);
    let profile = cli_profile(
        parsed.profile,
        &home,
        scratch,
        tmpdir.as_deref(),
        &parsed.policy.rw,
        parsed.policy.allow_net,
    );
    let mut cmd = std::process::Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-p").arg(&profile).arg("--").args(&parsed.command);
    apply_env_policy(&mut cmd, parsed.policy.env);
    Ok(spawn_and_wait(&mut cmd, parsed.timeout_secs))
}

/// How one deny-list entry is shadowed inside the bwrap sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    /// Directory target: an empty tmpfs — host content unreadable, writes land in memory and
    /// vanish with the sandbox.
    Tmpfs,
    /// File target (rc files, credential files): tmpfs is a *directory* — mounting it over a
    /// file mountpoint is at best a type confusion and at worst a bwrap error, which fails
    /// open to unguarded execution. A read-only /dev/null bind instead reads empty and
    /// denies writes (divergence from seatbelt: the real file's content is not readable).
    DevNull,
}

/// Installer-mode overlay targets for bwrap: bwrap has no deny rules, so the seatbelt deny
/// list is expressed as mounts that shadow the host paths. Only paths that exist on the host
/// are shadowed — bwrap needs the mountpoint present in the read-only root bind, and a
/// missing `~/.zshrc`/`~/.kube` has nothing to protect anyway (creating it stays possible:
/// the known gap vs seatbelt, which denies creation too). `INSTALLER_DENY_WRITE_ABS` is
/// skipped: those need root to touch on a real system, and mounting over system paths from
/// an unprivileged namespace risks bwrap erroring into the unguarded fallback.
// Consumed by the Linux bwrap backend; kept un-gated so the macOS dev/test build
// (the deny-list logic lives in the same constants) can exercise it directly.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
pub fn installer_overlays(home: &Path) -> Vec<(PathBuf, Overlay)> {
    INSTALLER_DENY_READ
        .iter()
        .chain(INSTALLER_DENY_WRITE.iter())
        .map(|rel| home.join(rel))
        .filter_map(|p| {
            if p.is_dir() {
                Some((p, Overlay::Tmpfs))
            } else if p.is_file() {
                Some((p, Overlay::DevNull))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn backend_linux() -> Backend {
    if which("bwrap").is_none() {
        return Backend::Unavailable("bwrap not installed (e.g. apt install bubblewrap)");
    }
    if !bwrap_probe() {
        return Backend::Unavailable(
            "bwrap present but cannot create namespaces (Ubuntu 24.04+/Debian 13+ restrict unprivileged user namespaces)",
        );
    }
    Backend::Bwrap
}

#[cfg(target_os = "linux")]
fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file())
}

/// Namespace availability is a deploy-time property (AppArmor/sysctl); probe once per run
/// instead of failing later with a confusing exec error.
#[cfg(target_os = "linux")]
fn bwrap_probe() -> bool {
    let Some(bwrap) = which("bwrap") else {
        return false;
    };
    std::process::Command::new(bwrap)
        .args([
            "--die-with-parent",
            "--unshare-all",
            "--ro-bind",
            "/",
            "/",
            "--",
            "/bin/true",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn run_bwrap(parsed: &Parsed, scratch: &Path) -> std::io::Result<i32> {
    let bwrap = which("bwrap")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "bwrap not found"))?;
    let home = crate::core::home_dir().map(|h| canonicalize_lossy(&h));
    let mut cmd = std::process::Command::new(bwrap);
    for a in bwrap_args(
        &parsed.policy,
        parsed.profile,
        scratch,
        home.as_deref(),
        &parsed.command,
    ) {
        cmd.arg(a);
    }
    apply_env_policy(&mut cmd, parsed.policy.env);
    Ok(spawn_and_wait(&mut cmd, parsed.timeout_secs))
}

/// Root is bound read-only, /dev and /proc are fresh mounts, and only the scratch dir (plus
/// `--rw` dirs) is bound read-write. `--unshare-all` + `--share-net` toggles the network
/// fence. Installer mode mirrors the seatbelt semantics: network open, `$HOME` writable, and
/// the deny list shadowed by tmpfs overlays (`installer_overlays`) — without this the
/// `--profile installer` flag silently behaved as strict on Linux (no net, read-only home),
/// breaking every staged installer the danger guard routes here.
#[cfg(target_os = "linux")]
fn bwrap_args(
    policy: &Policy,
    profile: Mode,
    scratch: &Path,
    home: Option<&Path>,
    command: &[String],
) -> Vec<String> {
    let mut a: Vec<String> = vec!["--die-with-parent".into(), "--unshare-all".into()];
    if policy.allow_net || profile == Mode::Installer {
        a.push("--share-net".into());
    }
    a.extend(["--ro-bind".into(), "/".into(), "/".into()]);
    a.extend(["--dev".into(), "/dev".into()]);
    a.extend(["--proc".into(), "/proc".into()]);
    let s = scratch.to_string_lossy().into_owned();
    a.extend(["--bind".into(), s.clone(), s]);
    for d in &policy.rw {
        let d = d.to_string_lossy().into_owned();
        a.extend(["--bind".into(), d.clone(), d]);
    }
    if profile == Mode::Installer {
        if let Some(h) = home {
            let h = h.to_string_lossy().into_owned();
            a.extend(["--bind".into(), h.clone(), h.clone()]);
            for (p, kind) in installer_overlays(h.as_ref()) {
                let p = p.to_string_lossy().into_owned();
                match kind {
                    Overlay::Tmpfs => a.extend(["--tmpfs".into(), p]),
                    Overlay::DevNull => a.extend(["--ro-bind".into(), "/dev/null".into(), p]),
                }
            }
        }
    }
    a.push("--".into());
    a.extend(command.iter().cloned());
    a
}
