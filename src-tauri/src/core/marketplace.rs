//! Plugin marketplace: index fetch + sha256 verification + reuse of install_ocplugin to complete installation.
//! See the install flow in docs/plugin-manifest.md (Phase 6).
//!
//! Data sources:
//! - a remote index.json pointed to by OPENCAPX_MARKETPLACE_URL (optional)
//! - otherwise ~/.opencapx/marketplace/seed.json as the seed (placed locally by the developer)
//!
//! index.json format:
//! ```json
//! {
//!   "entries": [
//!     {
//!       "id": "com.opencapx.echo-vision",
//!       "name": "Echo Vision",
//!       "version": "0.1.0",
//!       "description": "...",
//!       "downloadUrl": "file:///.../echo-0.1.0.ocplugin",
//!       "sha256": "<hex>",
//!       "capabilities": ["image.analyze"],
//!       "permissions": ["image.read"]
//!     }
//!   ]
//! }
//! ```

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginMarketEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub download_url: String,
    #[serde(rename = "sha256")]
    pub sha256: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Phase 38 — release channel. Entries in marketplace index.json may be marked `stable` /
    /// `beta` / `dev`. The plugin's currently subscribed min_channel determines which channels it can see updates from.
    #[serde(default = "default_channel")]
    pub channel: String,
    /// F4 — minimum semantic version (legacy index compatible: absent = no lower bound).
    #[serde(rename = "minCoreVersion", default, skip_serializing_if = "Option::is_none")]
    pub min_core_version: Option<String>,
    /// F4 — multi-version list (the M3 signed index reuses this structure). Empty array = fall back to the legacy single-version fields.
    #[serde(default)]
    pub versions: Vec<PluginMarketVersion>,
}

/// F4 — a single offerable version. M2/M3 will incrementally add signature / released_at / size_bytes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginMarketVersion {
    pub version: String,
    #[serde(rename = "minCoreVersion", default, skip_serializing_if = "Option::is_none")]
    pub min_core_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    #[serde(rename = "sha256")]
    pub sha256: String,
}

fn default_channel() -> String {
    "stable".into()
}

/// Channel priority: dev(2) ≥ beta(1) ≥ stable(0). `a` ≥ `b` means a includes/is above b.
pub fn channel_rank(name: &str) -> u8 {
    match name {
        "dev" => 2,
        "beta" => 1,
        _ => 0, // stable / unknown → 0
    }
}

/// Collapse arbitrary user input to a known channel, case-insensitively; unrecognized input falls back to stable.
pub fn normalize_channel(name: &str) -> String {
    let lower = name.trim().to_ascii_lowercase();
    match lower.as_str() {
        "dev" | "nightly" | "canary" => "dev".into(),
        "beta" | "rc" => "beta".into(),
        _ => "stable".into(),
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PluginMarketIndex {
    pub entries: Vec<PluginMarketEntry>,
}

/// A single plugin update item: id + current version + remote latest version + entry download url.
/// Used by the settings UI + `update_plugin(id)` follows this data to reinstall.
#[derive(Debug, Clone, Serialize)]
pub struct PluginUpdateInfo {
    pub id: String,
    #[serde(rename = "currentVersion")]
    pub current_version: String,
    #[serde(rename = "latestVersion")]
    pub latest_version: String,
    #[serde(rename = "downloadUrl")]
    pub download_url: String,
    #[serde(rename = "sha256")]
    pub sha256: String,
    /// Phase 38 — the channel the remote update entry belongs to. The settings UI uses it to show badges and block tier-skipping upgrades.
    pub channel: String,
}

pub fn marketplace_root() -> PathBuf {
    if let Ok(dir) = std::env::var("OPENCAPX_MARKETPLACE_DIR") {
        return PathBuf::from(dir);
    }
    crate::core::home_dir()
        .map(|h| h.join(".opencapx").join("marketplace"))
        .unwrap_or_else(|| std::env::temp_dir().join("opencapx-marketplace"))
}

pub fn cache_path() -> PathBuf {
    marketplace_root().join("cache.json")
}

pub fn seed_path() -> PathBuf {
    marketplace_root().join("seed.json")
}

pub fn remote_url() -> Option<String> {
    std::env::var("OPENCAPX_MARKETPLACE_URL").ok().filter(|s| !s.is_empty())
}

/// Read the seed (required — at least it must let users install local/already-downloaded plugins).
/// A remote index overrides and is written to the cache; with no remote, the cache is the seed.
pub fn load_index() -> PluginMarketIndex {
    if let Some(idx) = load_from_url() {
        write_cache(&idx);
        return idx;
    }
    if let Ok(text) = std::fs::read_to_string(cache_path()) {
        if let Ok(idx) = serde_json::from_str::<PluginMarketIndex>(&text) {
            return idx;
        }
    }
    if let Ok(text) = std::fs::read_to_string(seed_path()) {
        if let Ok(idx) = serde_json::from_str::<PluginMarketIndex>(&text) {
            write_cache(&idx);
            return idx;
        }
    }
    PluginMarketIndex::default()
}

pub fn refresh() -> Result<PluginMarketIndex, String> {
    let idx = if let Some(remote) = load_from_url() {
        write_cache(&remote);
        remote
    } else {
        return Err("no remote index (set OPENCAPX_MARKETPLACE_URL or write seed.json)".into());
    };
    Ok(idx)
}

fn write_cache(idx: &PluginMarketIndex) {
    let root = marketplace_root();
    let _ = std::fs::create_dir_all(&root);
    if let Ok(text) = serde_json::to_string_pretty(idx) {
        let _ = std::fs::write(cache_path(), text);
    }
}

fn load_from_url() -> Option<PluginMarketIndex> {
    let url = remote_url()?;
    fetch_url(&url).ok().and_then(|t| serde_json::from_str(&t).ok())
}

/// N2 — production allows only https distribution by default; file:// and bare local paths pass only under an explicit test switch.
fn local_urls_allowed() -> bool {
    std::env::var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn fetch_url(url: &str) -> Result<String, String> {
    if let Some(path) = url.strip_prefix("file://") {
        if !local_urls_allowed() {
            return Err(
                "local url disabled for production (set OPENCAPX_MARKETPLACE_ALLOW_LOCAL=1 for tests)"
                    .into(),
            );
        }
        return std::fs::read_to_string(path).map_err(|e| format!("read {}: {}", path, e));
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return fetch_http(url);
    }
    // Local path fallback (N2: same gate as file://)
    if !local_urls_allowed() {
        return Err(
            "local url disabled for production (set OPENCAPX_MARKETPLACE_ALLOW_LOCAL=1 for tests)"
                .into(),
        );
    }
    std::fs::read_to_string(url).map_err(|e| format!("read {}: {}", url, e))
}

fn fetch_http(url: &str) -> Result<String, String> {
    // Minimal implementation: no new HTTP client. Use std::process::Command to invoke curl — widely available.
    // N2 — https only (--proto-redir prevents downgrade redirects); index size capped at 4 MiB to prevent malicious huge-body memory spikes.
    let out = std::process::Command::new("curl")
        .args([
            "-sS",
            "-L",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-filesize",
            "4194304",
            "--max-time",
            "20",
            url,
        ])
        .output()
        .map_err(|e| format!("curl spawn: {}", e))?;
    if !out.status.success() {
        return Err(format!(
            "curl {} failed: {}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("utf8: {}", e))
}

/// Download the target version (app side): aligned with the result of `select_market_version`,
/// guaranteeing "the version prompted" = "the version downloaded" — an entry's flat fields may disagree with versions[].
pub fn download_target(id: &str, target: &PluginMarketVersion) -> Result<PathBuf, String> {
    download_bytes(id, &target.version, &target.download_url, &target.sha256)
}

/// Legacy entry point: the whole entry's flat fields (pre-M1 format).
pub fn download(entry: &PluginMarketEntry) -> Result<PathBuf, String> {
    download_bytes(&entry.id, &entry.version, &entry.download_url, &entry.sha256)
}

/// Download a .ocplugin to a temp directory and verify sha256, returning the download path.
/// N2 — streaming hash (no longer loads the whole package into memory); https-only; huge-package cap 50 MiB (aligned with the auto gate's max_package_bytes).
fn download_bytes(id: &str, version: &str, download_url: &str, sha256: &str) -> Result<PathBuf, String> {
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
    let mut f =
        std::fs::File::open(path).map_err(|e| format!("open {}: {}", path.display(), e))?;
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

pub fn find(id: &str) -> Option<PluginMarketEntry> {
    load_index().entries.into_iter().find(|e| e.id == id)
}

/// Compare installed (id, version) pairs against the marketplace index, returning the list with updates.
/// Version comparison follows `parse_version_lenient`'s semver precedence: compare major/minor/patch,
/// prerelease segments (`-rc.1` etc.) participate in precedence; build metadata (`+xxx`) does not.
/// When the current or candidate version cannot be parsed, treat it as "no update" (no error).
///
/// Phase 38 — the second parameter `channels` is each plugin's currently subscribed min_channel
/// (either a HashMap or an (id, channel) slice; a miss defaults to 'stable').
/// An upgrade is offered only when entry.channel_rank ≤ min_channel_rank.
pub fn check_updates(
    installed: &[(String, String)],
    channels: &[(String, String)],
) -> Vec<PluginUpdateInfo> {
    let idx = load_index();
    let mut out = Vec::new();
    for (id, current) in installed {
        let Some(entry) = idx.entries.iter().find(|e| &e.id == id) else {
            continue;
        };
        let min_channel = channels
            .iter()
            .find(|(pid, _)| pid == id)
            .map(|(_, c)| c.as_str())
            .unwrap_or("stable");
        if channel_rank(&entry.channel) > channel_rank(min_channel) {
            continue;
        }
        if let Some(selected) = select_market_version(entry, env!("CARGO_PKG_VERSION"), min_channel) {
            if version_is_newer(&selected.version, current) {
                out.push(PluginUpdateInfo {
                    id: id.clone(),
                    current_version: current.clone(),
                    latest_version: selected.version,
                    download_url: selected.download_url,
                    sha256: selected.sha256,
                    channel: selected.channel.unwrap_or_else(|| entry.channel.clone()),
                });
            }
        }
    }
    out
}

/// Backward-compat: degrades to the old signature when channels is absent (all plugins default to stable).
#[cfg(test)]
pub fn check_updates_default(installed: &[(String, String)]) -> Vec<PluginUpdateInfo> {
    check_updates(installed, &[])
}

/// Lenient parsing: strict semver first; on failure, pad the numeric core ("1.2"→"1.2.0", "1"→"1.0.0"),
/// allowing a `v` prefix and prerelease (`-rc.1`); build metadata (`+xxx`) does not participate in comparison.
/// Parse failure → None (callers treat it as "undecidable" and do not upgrade).
pub fn parse_version_lenient(raw: &str) -> Option<semver::Version> {
    let s = raw.trim();
    let s = s.strip_prefix('v').unwrap_or(s);
    if let Ok(v) = semver::Version::parse(s) {
        return Some(v);
    }
    let core = s.split(['-', '+']).next().unwrap_or("");
    let pre = s.split_once('-').map(|(_, rest)| rest.split('+').next().unwrap_or(""));
    let mut nums: Vec<u64> = Vec::new();
    for seg in core.split('.') {
        nums.push(seg.parse::<u64>().ok()?);
    }
    if nums.is_empty() || nums.len() > 3 {
        return None;
    }
    while nums.len() < 3 {
        nums.push(0);
    }
    let mut v = semver::Version::new(nums[0], nums[1], nums[2]);
    if let Some(p) = pre {
        v.pre = semver::Prerelease::new(p).ok()?;
    }
    Some(v)
}

/// `a > b`? (with prerelease semantics; "1.0.0" > "1.0.0-rc.1").
/// If either side cannot be parsed → false (consistent with old behavior: undecidable means no upgrade).
pub fn version_is_newer(a: &str, b: &str) -> bool {
    match (parse_version_lenient(a), parse_version_lenient(b)) {
        // Note: `semver::Version`'s `Ord` includes build metadata (total order, not semver precedence),
        // so `cmp_precedence` is needed to match the "build does not participate" semantics.
        (Some(x), Some(y)) => x.cmp_precedence(&y) == std::cmp::Ordering::Greater,
        _ => false,
    }
}

/// F4 update-supply gate: select the highest version within the entry that is "channel-allowed and core-compatible".
/// When `versions[]` is empty, fall back to the legacy single-version fields (using the whole entry's channel).
pub fn select_market_version(
    entry: &PluginMarketEntry,
    core_version: &str,
    min_channel: &str,
) -> Option<PluginMarketVersion> {
    fn core_ok(core: &str, min: Option<&str>) -> bool {
        match min {
            None => true,
            Some(min) => match (parse_version_lenient(core), parse_version_lenient(min)) {
                (Some(c), Some(m)) => c >= m,
                _ => true, // unparseable → allow (same criteria as plugin::check_core_compat)
            },
        }
    }
    let candidates: Vec<PluginMarketVersion> = if entry.versions.is_empty() {
        vec![PluginMarketVersion {
            version: entry.version.clone(),
            min_core_version: entry.min_core_version.clone(),
            channel: Some(entry.channel.clone()),
            download_url: entry.download_url.clone(),
            sha256: entry.sha256.clone(),
        }]
    } else {
        entry.versions.clone()
    };
    candidates
        .into_iter()
        .filter(|v| {
            let ch = v.channel.as_deref().unwrap_or(&entry.channel);
            channel_rank(ch) <= channel_rank(min_channel)
                && core_ok(core_version, v.min_core_version.as_deref())
        })
        .fold(None, |best: Option<PluginMarketVersion>, v| match best {
            None => Some(v),
            Some(b) => {
                if version_is_newer(&v.version, &b.version) {
                    Some(v)
                } else {
                    Some(b)
                }
            }
        })
}

/// M1 fix (N1/N2): app-side target resolution, sharing the same implementation as check_updates' selector.
/// `ceiling` is set by the caller: for updates = the plugin's subscribed channel (default `stable`, same criteria as check_updates /
/// the settings-page dropdown); for explicit install = `"dev"` (no channel-subscription gate — the user clicked on the specific entry,
/// aligned with the pre-M1 install behavior).
pub fn resolve_target_with_ceiling(
    entry: &PluginMarketEntry,
    core_version: &str,
    ceiling: &str,
) -> Result<PluginMarketVersion, String> {
    select_market_version(entry, core_version, ceiling)
        .ok_or_else(|| format!("no core-compatible version available for {}", entry.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    // Three tests share the OPENCAPX_MARKETPLACE_DIR global env var; serialize to prevent races
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        let g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // N2 — test fixtures are local files: explicitly enable the local-URL switch (off by default in production).
        std::env::set_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL", "1");
        g
    }

    /// N2 — production rejects file:// and bare local paths by default; only an explicit switch allows them.
    #[test]
    fn local_urls_are_rejected_without_test_switch() {
        let _g = lock_env();
        std::env::remove_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL");
        let dir = std::env::temp_dir().join(format!("opencapx-mk-local-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let pkg = dir.join("x.ocplugin");
        write(&pkg, "x");
        let mut e = entry_with(vec![], None);
        e.download_url = format!("file://{}", pkg.display());
        e.sha256 = sha256_hex(b"x");
        let err = download(&e).unwrap_err();
        assert!(err.contains("local url disabled"), "got: {}", err);
        let err2 = fetch_url(&e.download_url).unwrap_err();
        assert!(err2.contains("local url disabled"), "got: {}", err2);
        std::env::set_var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL", "1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write(path: &Path, v: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(v.as_bytes()).unwrap();
    }

    fn entry_with(versions: Vec<PluginMarketVersion>, min_core: Option<&str>) -> PluginMarketEntry {
        PluginMarketEntry {
            id: "com.x.m".into(),
            name: "M".into(),
            version: "0.0.0".into(),
            description: String::new(),
            download_url: "file:///dev/null".into(),
            sha256: "0".repeat(64),
            capabilities: vec![],
            permissions: vec![],
            channel: "stable".into(),
            min_core_version: min_core.map(String::from),
            versions,
        }
    }

    fn mv(v: &str, min_core: Option<&str>, ch: Option<&str>) -> PluginMarketVersion {
        PluginMarketVersion {
            version: v.into(),
            min_core_version: min_core.map(String::from),
            channel: ch.map(String::from),
            download_url: format!("file:///x-{v}.ocplugin"),
            sha256: "1".repeat(64),
        }
    }

    /// SHA-256 known-answer vector: "abc" → ba7816bf...f20015ad
    #[test]
    fn sha256_abc() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// RFC 4231 Test Case 1: key=0x0b*20, data="Hi There"
    #[test]
    fn hmac_sha256_matches_rfc4231_case1() {
        let key = vec![0x0b_u8; 20];
        assert_eq!(
            hmac_sha256_hex(&key, b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    /// RFC 4231 Test Case 2: key="Jefe"
    #[test]
    fn hmac_sha256_matches_rfc4231_case2() {
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    /// RFC 4231 Test Case 6: 131-byte over-long key (hash-first branch)
    #[test]
    fn hmac_sha256_long_key_matches_rfc4231_case6() {
        let key = vec![0xaa_u8; 131];
        assert_eq!(
            hmac_sha256_hex(&key, b"Test Using Larger Than Block-Size Key - Hash Key First"),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    /// The raw and hex entry points must agree (the easiest spot to get wrong when swapping implementations).
    #[test]
    fn sha256_raw_matches_hex() {
        let data = b"opencapx-integration-check";
        let raw = sha256_raw(data);
        let hex: String = raw.iter().map(|b| format!("{:02x}", b)).collect();
        assert_eq!(hex, sha256_hex(data));
    }

    /// Prerelease semantics: release > prerelease; prerelease segments compare segment by segment; build metadata does not participate.
    /// The old test `version_is_newer_compares_digits` must still pass unchanged.
    #[test]
    fn version_is_newer_handles_prerelease_and_metadata() {
        assert!(version_is_newer("1.0.0", "1.0.0-rc.1"));
        assert!(version_is_newer("1.0.0-rc.10", "1.0.0-rc.9"));
        assert!(!version_is_newer("1.0.0-rc.1", "1.0.0"));
        assert!(version_is_newer("v1.2.0", "1.1.9"));
        assert!(!version_is_newer("1.0.0+build5", "1.0.0"));
        assert!(!version_is_newer("1.0.0", "1.0.0+build5"));
    }

    #[test]
    fn seed_fallback_loads_and_caches() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        std::env::remove_var("OPENCAPX_MARKETPLACE_URL");

        let idx = PluginMarketIndex {
            entries: vec![PluginMarketEntry {
                id: "com.x.demo".into(),
                name: "Demo".into(),
                version: "0.1.0".into(),
                description: "demo".into(),
                download_url: "file:///tmp/x.ocplugin".into(),
                sha256: "deadbeef".into(),
                capabilities: vec!["image.analyze".into()],
                permissions: vec!["image.read".into()],
                channel: "stable".into(),
                min_core_version: None,
                versions: vec![],
            }],
        };
        write(&seed_path(), &serde_json::to_string_pretty(&idx).unwrap());
        let loaded = load_index();
        assert_eq!(loaded.entries.len(), 1);
        assert!(cache_path().exists());
        assert_eq!(find("com.x.demo").unwrap().name, "Demo");
        assert!(find("nope").is_none());

        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn version_is_newer_compares_digits() {
        assert!(version_is_newer("1.2.0", "1.1.5"));
        assert!(version_is_newer("2.0.0", "1.99.99"));
        assert!(!version_is_newer("1.1.0", "1.1.0"));
        assert!(!version_is_newer("1.1.0", "1.2.0"));
        assert!(version_is_newer("1.2", "1.1.5"));
        assert!(!version_is_newer("not-a-version", "0.0.1"));
    }

    #[test]
    fn select_prefers_highest_compatible_version() {
        let e = entry_with(
            vec![
                mv("2.0.0", Some("99.0.0"), None), // incompatible
                mv("1.5.0", Some("0.1.0"), None),
                mv("1.4.0", None, None),
            ],
            None,
        );
        let v = select_market_version(&e, "1.0.0", "stable").expect("compatible");
        assert_eq!(v.version, "1.5.0");
    }

    #[test]
    fn select_none_when_all_versions_too_new() {
        let e = entry_with(vec![mv("2.0.0", Some("99.0.0"), None)], None);
        assert!(select_market_version(&e, "1.0.0", "stable").is_none());
    }

    #[test]
    fn select_respects_channel_ceiling() {
        let e = entry_with(
            vec![
                mv("2.0.0", None, Some("beta")),
                mv("1.5.0", None, Some("stable")),
            ],
            None,
        );
        let v = select_market_version(&e, "1.0.0", "stable").expect("stable only");
        assert_eq!(v.version, "1.5.0");
        let v2 = select_market_version(&e, "1.0.0", "beta").expect("beta ok");
        assert_eq!(v2.version, "2.0.0");
    }

    #[test]
    fn select_falls_back_to_flat_entry_when_no_versions_array() {
        let e = entry_with(vec![], Some("99.0.0"));
        assert!(select_market_version(&e, "1.0.0", "stable").is_none());
        let ok = entry_with(vec![], Some("0.1.0"));
        assert_eq!(
            select_market_version(&ok, "1.0.0", "stable").map(|v| v.version),
            Some("0.0.0".to_string())
        );
    }

    /// M1 fix (N1/N2): explicit install uses a `"dev"` ceiling (no subscription gate), update uses the plugin's channel (default
    /// `stable`). The two caller semantics for the same entry must be distinguishable.
    #[test]
    fn resolve_target_ceiling_separates_install_and_update_semantics() {
        let mut beta = entry_with(vec![], Some("0.1.0"));
        beta.channel = "beta".into();
        assert!(
            resolve_target_with_ceiling(&beta, "1.0.0", "stable").is_err(),
            "flat beta entry must be gated out for a stable update"
        );
        let installed =
            resolve_target_with_ceiling(&beta, "1.0.0", "dev").expect("explicit install un-gated");
        assert_eq!(installed.channel.as_deref(), Some("beta"));

        let mut stable = entry_with(vec![], Some("0.1.0"));
        stable.channel = "stable".into();
        assert!(resolve_target_with_ceiling(&stable, "1.0.0", "stable").is_ok());
        assert!(resolve_target_with_ceiling(&stable, "1.0.0", "dev").is_ok());
    }

    #[test]
    fn resolve_target_reports_core_incompatibility() {
        let mut e = entry_with(vec![mv("1.5.0", Some("99.0.0"), None)], None);
        e.channel = "beta".into();
        for ceiling in ["stable", "dev"] {
            let err = resolve_target_with_ceiling(&e, "1.0.0", ceiling).unwrap_err();
            assert!(err.contains("core-compatible"), "got: {}", err);
        }
    }

    #[test]
    fn check_updates_returns_only_newer() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt-upd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        std::env::remove_var("OPENCAPX_MARKETPLACE_URL");
        let idx = PluginMarketIndex {
            entries: vec![
                PluginMarketEntry {
                    id: "com.x.old".into(),
                    name: "Old".into(),
                    version: "0.1.0".into(),
                    description: "".into(),
                    download_url: "file:///dev/null".into(),
                    sha256: "0".repeat(64),
                    capabilities: vec![],
                    permissions: vec![],
                    channel: "stable".into(),
                    min_core_version: None,
                    versions: vec![],
                },
                PluginMarketEntry {
                    id: "com.x.bumped".into(),
                    name: "Bumped".into(),
                    version: "1.2.0".into(),
                    description: "".into(),
                    download_url: "file:///dev/null".into(),
                    sha256: "0".repeat(64),
                    capabilities: vec![],
                    permissions: vec![],
                    channel: "stable".into(),
                    min_core_version: None,
                    versions: vec![],
                },
            ],
        };
        let text = serde_json::to_string(&idx).unwrap();
        std::fs::write(base.join("seed.json"), &text).unwrap();
        // cache.json does not exist, load_index should fall back to seed
        std::fs::remove_file(base.join("cache.json")).ok();

        let installed = vec![
            ("com.x.old".to_string(), "0.1.0".to_string()),
            ("com.x.bumped".to_string(), "1.1.0".to_string()),
            ("com.x.missing".to_string(), "0.1.0".to_string()),
        ];
        let updates = check_updates(&installed, &[]);
        assert_eq!(updates.len(), 1, "only bumped should have update");
        assert_eq!(updates[0].id, "com.x.bumped");
        assert_eq!(updates[0].current_version, "1.1.0");
        assert_eq!(updates[0].latest_version, "1.2.0");

        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn check_updates_skips_entries_requiring_newer_core() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt-core-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        std::env::remove_var("OPENCAPX_MARKETPLACE_URL");
        let idx = PluginMarketIndex {
            entries: vec![
                entry_with(vec![], None),
                PluginMarketEntry {
                    id: "com.x.future".into(),
                    version: "2.0.0".into(),
                    min_core_version: Some("99.0.0".into()),
                    ..entry_with(vec![], None)
                },
            ],
        };
        std::fs::write(base.join("seed.json"), serde_json::to_string(&idx).unwrap()).unwrap();
        let installed = vec![
            ("com.x.m".to_string(), "0.0.0".to_string()),
            ("com.x.future".to_string(), "1.0.0".to_string()),
        ];
        let updates = check_updates(&installed, &[]);
        assert_eq!(updates.len(), 0, "0.0.0 is not an update; future is skipped for core incompatibility");
        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn download_rejects_sha_mismatch() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        let pkg = base.join("pkg.ocplugin");
        std::fs::write(&pkg, b"hello world").unwrap();
        let entry = PluginMarketEntry {
            id: "com.x".into(),
            name: "X".into(),
            version: "0.1.0".into(),
            description: "".into(),
            download_url: format!("file://{}", pkg.display()),
            sha256: "0000000000000000000000000000000000000000000000000000000000000000".into(),
            capabilities: vec![],
            permissions: vec![],
            channel: "stable".into(),
            min_core_version: None,
            versions: vec![],
        };
        let err = download(&entry).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "got: {}", err);
        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn download_passes_and_returns_path() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        let pkg = base.join("good.ocplugin");
        let bytes = b"hello world";
        std::fs::write(&pkg, bytes).unwrap();
        let entry = PluginMarketEntry {
            id: "com.x".into(),
            name: "X".into(),
            version: "0.1.0".into(),
            description: "".into(),
            download_url: format!("file://{}", pkg.display()),
            sha256: "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9".into(),
            capabilities: vec![],
            permissions: vec![],
            channel: "stable".into(),
            min_core_version: None,
            versions: vec![],
        };
        let p = download(&entry).unwrap();
        assert!(p.exists());
        assert!(p.metadata().unwrap().len() == bytes.len() as u64);
        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// App-side bridge: when the flat version is incompatible (2.0.0) but versions[] has a compatible entry (1.5.0),
    /// check_updates suggests 1.5.0 → download_target must download 1.5.0, not the flat field's 2.0.0.
    #[test]
    fn download_target_uses_selected_version_not_flat_entry() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt-target-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);

        // The flat field points to the incompatible 2.0.0 (decoy); only versions[] has the compatible 1.5.0.
        let decoy = base.join("decoy.ocplugin");
        std::fs::write(&decoy, b"decoy-2.0.0").unwrap();
        let pkg150 = base.join("v150.ocplugin");
        let payload = b"payload-1.5.0";
        std::fs::write(&pkg150, payload).unwrap();

        let mut e = entry_with(
            vec![PluginMarketVersion {
                version: "1.5.0".into(),
                min_core_version: Some("0.1.0".into()),
                channel: None,
                download_url: format!("file://{}", pkg150.display()),
                sha256: sha256_hex(payload),
            }],
            Some("99.0.0"),
        );
        e.version = "2.0.0".into();
        e.download_url = format!("file://{}", decoy.display());
        e.sha256 = sha256_hex(b"decoy-2.0.0");

        let selected = select_market_version(&e, "1.0.0", "stable").expect("compatible 1.5.0");
        assert_eq!(selected.version, "1.5.0");

        let p = download_target(&e.id, &selected).expect("download target");
        assert!(
            p.file_name().unwrap().to_string_lossy().contains("1.5.0"),
            "path should name selected version: {}",
            p.display()
        );
        assert_eq!(std::fs::read(&p).unwrap(), payload);

        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn download_target_rejects_sha_mismatch() {
        let _g = lock_env();
        let base = std::env::temp_dir().join(format!("opencapx-mkt-target-sha-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        let pkg = base.join("bad.ocplugin");
        std::fs::write(&pkg, b"hello target").unwrap();
        let target = PluginMarketVersion {
            version: "1.5.0".into(),
            min_core_version: None,
            channel: None,
            download_url: format!("file://{}", pkg.display()),
            sha256: "0".repeat(64),
        };
        let err = download_target("com.x.m", &target).unwrap_err();
        assert!(err.contains("sha256 mismatch"), "got: {}", err);
        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Full chain: seed -> find -> download (sha verification) -> install_ocplugin -> start the echo plugin.
    /// Requires python3; ignored by default (avoids a mandatory CI dependency); run with `--ignored`.
    #[test]
    #[ignore]
    fn market_install_full_chain() {
        let _g = lock_env();
        let py = std::process::Command::new("python3")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok();
        if !py {
            eprintln!("skip: no python3");
            return;
        }
        let base = std::env::temp_dir().join(format!("opencapx-mktchain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::env::set_var("OPENCAPX_MARKETPLACE_DIR", &base);
        std::env::set_var("OPENCAPX_PLUGINS_DIR", base.join("plugins"));

        // install_ocplugin needs shared_store
        let store: super::super::storage::SharedStore = std::sync::Arc::new(std::sync::Mutex::new(
            super::super::storage::StoreEnum::Db(
                super::super::storage::Storage::open(&base.join("t.db")).unwrap(),
            ),
        ));
        super::super::set_shared_store(store);

        // Prepare the .ocplugin package (reuse the in-repo echo-vision)
        let ocplugin = base.join("echo.ocplugin");
        let echo_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("plugins")
            .join("echo-vision");
        {
            use std::io::Write;
            use zip::write::SimpleFileOptions;
            let f = std::fs::File::create(&ocplugin).unwrap();
            let mut z = zip::ZipWriter::new(f);
            let manifest = std::fs::read_to_string(echo_dir.join("opencapx-plugin.json")).unwrap();
            let script = std::fs::read_to_string(echo_dir.join("bin/echo_vision.py")).unwrap();
            z.start_file("opencapx-plugin.json", SimpleFileOptions::default()).unwrap();
            z.write_all(manifest.as_bytes()).unwrap();
            z.start_file("bin/echo_vision.py", SimpleFileOptions::default()).unwrap();
            z.write_all(script.as_bytes()).unwrap();
            z.finish().unwrap();
        }
        let sha = super::sha256(&std::fs::read(&ocplugin).unwrap());
        let seed = PluginMarketIndex {
            entries: vec![PluginMarketEntry {
                id: "com.opencapx.echo-vision".into(),
                name: "Echo Vision".into(),
                version: "0.1.0".into(),
                description: "smoke".into(),
                download_url: format!("file://{}", ocplugin.display()),
                sha256: sha,
                capabilities: vec!["image.analyze".into()],
                permissions: vec!["image.read".into()],
                channel: "stable".into(),
                min_core_version: None,
                versions: vec![],
            }],
        };
        std::fs::write(
            base.join("seed.json"),
            serde_json::to_string_pretty(&seed).unwrap(),
        )
        .unwrap();

        // Follow the same path as the Tauri install_marketplace command
        let entry = super::find("com.opencapx.echo-vision").expect("entry");
        let pkg = download(&entry).expect("download");
        let mgr = super::super::plugin::PluginManager::shared();
        let id = mgr.install_ocplugin(&pkg).expect("install_ocplugin");
        assert_eq!(id, "com.opencapx.echo-vision");
        assert!(mgr.list().iter().any(|p| p.id == id && p.status == "running"));
        mgr.stop(&id);
        std::env::remove_var("OPENCAPX_MARKETPLACE_DIR");
        std::env::remove_var("OPENCAPX_PLUGINS_DIR");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Phase 38 — channel rank: dev > beta > stable; unknown falls back to stable.
    /// normalize_channel is the single entry point: normalize the raw string first, then rank.
    #[test]
    fn channel_rank_ordering() {
        assert!(channel_rank("dev") > channel_rank("beta"));
        assert!(channel_rank("beta") > channel_rank("stable"));
        assert_eq!(channel_rank(""), 0);
        assert_eq!(channel_rank("unknown"), 0);
        assert_eq!(channel_rank("nightly"), 0); // the alias goes through normalize, not rank
        assert!(channel_rank(normalize_channel("nightly").as_str()) > channel_rank("stable"));
    }

    /// normalize_channel collapses case / spelling differences into one of three.
    #[test]
    fn normalize_channel_canonical() {
        assert_eq!(normalize_channel("stable"), "stable");
        assert_eq!(normalize_channel("STABLE"), "stable");
        assert_eq!(normalize_channel("Beta"), "beta");
        assert_eq!(normalize_channel("rc"), "beta");
        assert_eq!(normalize_channel("nightly"), "dev");
        assert_eq!(normalize_channel(" dev "), "dev");
        assert_eq!(normalize_channel("garbage"), "stable");
    }

    /// Filesystem-independent — use check_updates' core filter logic directly (write a helper over entries).
    /// This verifies the if branch of the channel filter: a stable user cannot see beta/dev entries.
    #[test]
    fn channel_filter_drops_higher_ranks_for_stable_user() {
        // Mirror check_updates' filter decision (without calling load_index, to avoid disk dependence):
        let entries = vec![
            ("plug-a", "1.0.0", "stable"),
            ("plug-b", "1.0.0", "beta"),
            ("plug-c", "1.0.0", "dev"),
        ];
        let installed = vec![
            ("plug-a".to_string(), "0.9.0".to_string()),
            ("plug-b".to_string(), "0.9.0".to_string()),
            ("plug-c".to_string(), "0.9.0".to_string()),
        ];
        // A stable user sees only stable
        let channels_stable: Vec<(String, String)> = vec![];
        let mut visible: Vec<&str> = entries
            .iter()
            .filter(|(id, _, ch)| {
                let min = channels_stable
                    .iter()
                    .find(|(pid, _)| pid == *id)
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("stable");
                channel_rank(ch) <= channel_rank(min)
            })
            .map(|(id, _, _)| *id)
            .collect();
        visible.sort();
        assert_eq!(visible, vec!["plug-a"]);
        // A dev user sees all
        let channels_dev: Vec<(String, String)> = vec![
            ("plug-a".into(), "dev".into()),
            ("plug-b".into(), "dev".into()),
            ("plug-c".into(), "dev".into()),
        ];
        let mut visible2: Vec<&str> = entries
            .iter()
            .filter(|(id, _, ch)| {
                let min = channels_dev
                    .iter()
                    .find(|(pid, _)| pid == *id)
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("stable");
                channel_rank(ch) <= channel_rank(min)
            })
            .map(|(id, _, _)| *id)
            .collect();
        visible2.sort();
        assert_eq!(visible2, vec!["plug-a", "plug-b", "plug-c"]);
        // A beta user sees stable + beta (not dev)
        let channels_beta: Vec<(String, String)> = vec![
            ("plug-a".into(), "beta".into()),
            ("plug-b".into(), "beta".into()),
            ("plug-c".into(), "beta".into()),
        ];
        let mut visible3: Vec<&str> = entries
            .iter()
            .filter(|(id, _, ch)| {
                let min = channels_beta
                    .iter()
                    .find(|(pid, _)| pid == *id)
                    .map(|(_, c)| c.as_str())
                    .unwrap_or("stable");
                channel_rank(ch) <= channel_rank(min)
            })
            .map(|(id, _, _)| *id)
            .collect();
        visible3.sort();
        assert_eq!(visible3, vec!["plug-a", "plug-b"]);
    }
}
