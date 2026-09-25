//! the stable CLI copy at ~/.opencapx/bin/opencapx and the hook command strings that point at it.
//! Mechanical move from hooks.rs.

use super::*;

/// The stable CLI path that every hook and MCP config points at.
pub fn shim_path() -> PathBuf {
    crate::core::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".opencapx")
        .join("bin")
        .join("opencapx")
}

/// Materialize the shim. Test builds skip the copy itself (the test binary is ~100 MB;
/// every install()-path test would churn it through a temp HOME) — tests pin paths, and
/// one dedicated test exercises the real copy via ensure_shim_for_real().
pub fn ensure_shim() -> std::io::Result<PathBuf> {
    if cfg!(test) {
        return Ok(shim_path());
    }
    ensure_shim_for_real()
}

/// The actual copy: atomic tmp+rename, so it also works while the shim itself is executing
/// (hook processes are spawned from this very path). Skipped when the shim already matches
/// the running binary (same size and not older than the source).
pub(crate) fn ensure_shim_for_real() -> std::io::Result<PathBuf> {
    let shim = shim_path();
    let exe = std::env::current_exe()?;
    if same_file(&exe, &shim) {
        return Ok(shim);
    }
    let fresh = std::fs::metadata(&exe)
        .ok()
        .zip(std::fs::metadata(&shim).ok())
        .map(|(src, dst)| src.len() == dst.len() && modified_secs(&dst) >= modified_secs(&src))
        .unwrap_or(false);
    if !fresh {
        if let Some(dir) = shim.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = shim.with_file_name(format!("opencapx.tmp-{}", std::process::id()));
        std::fs::copy(&exe, &tmp)?;
        std::fs::rename(&tmp, &shim)?;
    }
    Ok(shim)
}

fn modified_secs(m: &std::fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    if a == b {
        return true;
    }
    matches!(
        (std::fs::canonicalize(a), std::fs::canonicalize(b)),
        (Ok(x), Ok(y)) if x == y
    )
}

fn hook_command() -> String {
    // The written path must exist by construction: materialize the shim first, and only
    // fall back to current_exe when the shim cannot be written (read-only home).
    let exe = ensure_shim()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            std::env::current_exe()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "opencapx".into())
        });
    format!("\"{}\" hook --agent", exe)
}

pub(crate) fn full_command(kind: &str) -> String {
    format!("{} {}", hook_command(), kind)
}

pub(crate) fn is_ours(cmd: &str) -> bool {
    let l = cmd.to_lowercase();
    l.contains("opencapx") && l.contains("hook")
}
