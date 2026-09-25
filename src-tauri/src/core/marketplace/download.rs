//! package download with hash verification, and the sha256/hmac primitives shared with the alerting signer.
//! Mechanical move from core/marketplace.rs.

use super::*;

/// Download the target version (app side): aligned with the result of `select_market_version`,
/// guaranteeing "the version prompted" = "the version downloaded" — an entry's flat fields may disagree with versions[].
pub fn download_target(id: &str, target: &PluginMarketVersion) -> Result<PathBuf, String> {
    download_bytes(id, &target.version, &target.download_url, &target.sha256)
}

/// Legacy entry point: the whole entry's flat fields (pre-M1 format).
pub fn download(entry: &PluginMarketEntry) -> Result<PathBuf, String> {
    download_bytes(
        &entry.id,
        &entry.version,
        &entry.download_url,
        &entry.sha256,
    )
}

/// Download a .ocplugin to a temp directory and verify sha256, returning the download path.
/// N2 — streaming hash (no longer loads the whole package into memory); https-only; huge-package cap 50 MiB (aligned with the auto gate's max_package_bytes).
fn download_bytes(
    id: &str,
    version: &str,
    download_url: &str,
    sha256: &str,
) -> Result<PathBuf, String> {
    let root = marketplace_root().join("downloads");
    std::fs::create_dir_all(&root).map_err(|e| format!("mkdir: {}", e))?;
    let out = root.join(format!("{}-{}.ocplugin", id, version));
    let (final_path, digest) = if let Some(path) = download_url.strip_prefix("file://") {
        if !local_urls_allowed() {
            return Err(
                "local url disabled for production (set OPENCAPX_MARKETPLACE_ALLOW_LOCAL=1 for tests)"
                    .into(),
            );
        }
        let d = hash_copy(Path::new(path), &out)?;
        (out.clone(), d)
    } else if download_url.starts_with("http://") || download_url.starts_with("https://") {
        let tmp = out.with_extension("part");
        let st = std::process::Command::new("curl")
            .args([
                "-sSL",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--max-filesize",
                "52428800",
                "--max-time",
                "120",
                "-o",
            ])
            .arg(&tmp)
            .arg(download_url)
            .status()
            .map_err(|e| format!("curl spawn: {}", e))?;
        if !st.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("download failed: {}", download_url));
        }
        let d = sha256_file(&tmp)?;
        (tmp, d)
    } else {
        if !local_urls_allowed() {
            return Err(
                "local url disabled for production (set OPENCAPX_MARKETPLACE_ALLOW_LOCAL=1 for tests)"
                    .into(),
            );
        }
        let d = hash_copy(Path::new(download_url), &out)?;
        (out.clone(), d)
    };
    if !digest.eq_ignore_ascii_case(sha256) {
        let _ = std::fs::remove_file(&final_path);
        return Err(format!(
            "sha256 mismatch: expected {} got {}",
            sha256, digest
        ));
    }
    if final_path != out {
        std::fs::rename(&final_path, &out).map_err(|e| format!("rename: {}", e))?;
    }
    Ok(out)
}

/// N2 — streaming sha256 (64 KiB chunks), huge packages never fully held in memory.
fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {}", path.display(), e))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {}", path.display(), e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// N2 — copy + streaming hash (file:// and local-path branch; eliminates `std::fs::read`'s whole-package memory).
fn hash_copy(src: &Path, dst: &Path) -> Result<String, String> {
    use std::io::{Read, Write};
    let mut f = std::fs::File::open(src).map_err(|e| format!("open {}: {}", src.display(), e))?;
    let mut o =
        std::fs::File::create(dst).map_err(|e| format!("create {}: {}", dst.display(), e))?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("read {}: {}", src.display(), e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        o.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {}", dst.display(), e))?;
    }
    Ok(format!("{:x}", h.finalize()))
}

/// SHA-256 → 64-char lowercase hex.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// Raw 32-byte SHA-256 (for internal/consistency verification).
pub fn sha256_raw(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

/// Kept for compatibility: legacy entry point (tests and existing callers), returns hex.
pub fn sha256(data: &[u8]) -> String {
    sha256_hex(data)
}

/// HMAC-SHA256 → 64-char lowercase hex (RFC 2104; keys of any length are handled by the hmac crate).
pub fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(msg);
    let out = mac.finalize().into_bytes();
    out.iter().map(|b| format!("{:02x}", b)).collect()
}
