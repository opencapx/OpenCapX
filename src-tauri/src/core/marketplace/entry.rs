//! market index entry shapes: PluginMarketEntry/Version/Index, PluginUpdateInfo, channel ranking/normalization.
//! Mechanical move from core/marketplace.rs.

use serde::{Deserialize, Serialize};

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
    #[serde(
        rename = "minCoreVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub min_core_version: Option<String>,
    /// F4 — multi-version list (the M3 signed index reuses this structure). Empty array = fall back to the legacy single-version fields.
    #[serde(default)]
    pub versions: Vec<PluginMarketVersion>,
}

/// F4 — a single offerable version. M2/M3 will incrementally add signature / released_at / size_bytes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PluginMarketVersion {
    pub version: String,
    #[serde(
        rename = "minCoreVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
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
