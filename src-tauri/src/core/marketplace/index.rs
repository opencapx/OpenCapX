//! index acquisition: roots/paths, cache load + remote refresh, URL policy, HTTP fetch.
//! Mechanical move from core/marketplace.rs.

use super::*;
use serde::{Deserialize, Serialize};

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
    std::env::var("OPENCAPX_MARKETPLACE_URL")
        .ok()
        .filter(|s| !s.is_empty())
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
    fetch_url(&url)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// N2 — production allows only https distribution by default; file:// and bare local paths pass only under an explicit test switch.
pub(crate) fn local_urls_allowed() -> bool {
    std::env::var("OPENCAPX_MARKETPLACE_ALLOW_LOCAL")
        .map(|v| v == "1")
        .unwrap_or(false)
}

pub(crate) fn fetch_url(url: &str) -> Result<String, String> {
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
