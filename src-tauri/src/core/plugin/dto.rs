//! Plugin DTOs (Phase 32 preview/status/graph shapes) shared with the frontend over /rpc.
//! Mechanical move from core/plugin.rs.

use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct PluginStatusDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub status: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Whether auto-reload polling is enabled (manifest mtime change → automatic stop+start).
    /// Default false; settings.ts exposes a toggle that calls `set_auto_reload` to flip it.
    #[serde(rename = "autoReload")]
    pub auto_reload: bool,
    /// Phase 37 — most recent probe status ("passed" / "failed" / "pending" / "skipped").
    /// None if never run (old database / Mem mode).
    #[serde(rename = "probeStatus", skip_serializing_if = "Option::is_none")]
    pub probe_status: Option<String>,
    /// Phase 37 — probe timestamp (epoch seconds).
    #[serde(rename = "probeAt", skip_serializing_if = "Option::is_none")]
    pub probe_at: Option<u64>,
    /// Phase 38 — plugin update channel ("stable" / "beta" / "dev").
    /// None if never stored (falls back to the global default channel).
    #[serde(rename = "channel", skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// S5 — whether a sandbox block was declared (UI badge; enforcement is still bound by the switch/trust constraints).
    #[serde(rename = "sandboxDeclared")]
    pub sandbox_declared: bool,
    /// Phase 40 — heartbeat_sec (0 = ping off, default behavior). None = never configured → watchdog uses a 500ms tick.
    #[serde(rename = "healthHeartbeatSec", skip_serializing_if = "Option::is_none")]
    pub health_heartbeat_sec: Option<u32>,
    /// Phase 40 — max_retries (0 = watchdog disabled).
    #[serde(rename = "healthMaxRetries", skip_serializing_if = "Option::is_none")]
    pub health_max_retries: Option<u32>,
    /// Phase 40 — whether the watchdog is enabled (default true).
    #[serde(rename = "healthEnabled", skip_serializing_if = "Option::is_none")]
    pub health_enabled: Option<bool>,
    /// F5 — unmet plugin dependencies (used for the settings-page warning).
    #[serde(
        rename = "missingDependencies",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub missing_dependencies: Vec<MissingDepDto>,
    /// M4 — revocation hit: the publisher key is in the registry revokedKeys (settings-page banner + reopen button).
    #[serde(rename = "revokedKey", skip_serializing_if = "Option::is_none")]
    pub revoked_key: Option<String>,
    /// M4 — revocation hit time (epoch seconds).
    #[serde(rename = "revokedAt", skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
}

/// F5 — a single unmet dependency (id + requirement literal).
#[derive(Debug, Clone, Serialize)]
pub struct MissingDepDto {
    pub id: String,
    pub requirement: String,
}

/// For the Wow 5 install-preview dialog. Opens the zip, reads the manifest, **without extracting**, and returns to the frontend
/// for display; only when the user clicks Install does it go through `install_ocplugin`. The permissions array carries the `high_risk`
/// marker (aligned with core::permission::HIGH_RISK); the frontend highlights sensitive permissions in red.
#[derive(Debug, Clone, Serialize)]
pub struct PluginPreviewDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub capabilities: Vec<String>,
    pub permissions: Vec<PermissionPreviewDto>,
    /// Wow 6 — signature/integrity status (used by the settings badge).
    /// label: "trusted" / "unsigned" / "tampered" / "bad-signature" / "unknown-key" / "malformed-signature"
    /// keyId: signer identifier (returned when trusted)
    #[serde(rename = "signature")]
    pub signature: SignaturePreviewDto,
    /// F7 — registry verification: keyId is a registered publisher (verified and not revoked). None = unsigned / registry unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// S5 — whether a sandbox block was declared (used by the enforcement popup when unsigned).
    #[serde(rename = "sandboxDeclared")]
    pub sandbox_declared: bool,
    /// F7 — official keyId marker (com.opencapx prefix).
    pub official: bool,
    /// F7 — core compatibility (a rendering fact; the real gate is in the install flow).
    pub compat: CompatPreviewDto,
    /// F7 — permission diff against the installed version (not installed → None).
    #[serde(rename = "permissionDiff", skip_serializing_if = "Option::is_none")]
    pub permission_diff: Option<PermissionDiffDto>,
}

/// F7 — compatibility summary for preview.
#[derive(Debug, Clone, Serialize)]
pub struct CompatPreviewDto {
    pub ok: bool,
    #[serde(rename = "minCoreVersion", skip_serializing_if = "Option::is_none")]
    pub min_core_version: Option<String>,
    pub current: String,
}

/// F7 — permission diff for preview (update case: added / removed).
#[derive(Debug, Clone, Serialize)]
pub struct PermissionDiffDto {
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// M7/F8 — generic settings-page view: declarations + current values + set secret keys (values are not returned).
#[derive(Debug, Clone, Serialize)]
pub struct SettingsViewDto {
    pub settings: Vec<SettingDecl>,
    pub values: serde_json::Map<String, serde_json::Value>,
    #[serde(rename = "secretsSet")]
    pub secrets_set: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignaturePreviewDto {
    #[serde(rename = "status")]
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "keyId")]
    pub key_id: Option<String>,
}

/// Wow 7 — for the uninstall confirmation dialog. Shows a few extra things that will be cleared, beyond `window.confirm`:
/// - auto_reload switch state (the user may have forgotten to turn it off)
/// - whether the config file exists (unrecoverable if lost)
/// - permissions / capabilities counts (indirectly reflect the footprint)
/// - dependents: ids of other installed plugins whose capabilities overlap this plugin's
///   (the user should first assess whether these dependents would break)
#[derive(Debug, Clone, Serialize)]
pub struct UninstallPreviewDto {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(rename = "autoReload")]
    pub auto_reload: bool,
    #[serde(rename = "configExists")]
    pub config_exists: bool,
    #[serde(rename = "permissionCount")]
    pub permission_count: usize,
    #[serde(rename = "capabilityCount")]
    pub capability_count: usize,
    /// ids of other installed plugins with overlapping capabilities (describes the dependency relationship)
    pub dependents: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionPreviewDto {
    pub name: String,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
    /// §4.3: declaration-derived → once-only, the UI does not offer Always
    #[serde(rename = "declared")]
    pub declared: bool,
}

/// Phase 32 — capability dependency graph node (each installed plugin = one node).
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphNodeDto {
    pub id: String,
    pub name: String,
    pub capabilities: Vec<String>,
}

/// Phase 32 — capability dependency graph edge. An edge is created when two plugins share at least one capability,
/// `shared` lists every shared item (for the tooltip).
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphEdgeDto {
    pub from: String,
    pub to: String,
    pub shared: Vec<String>,
}

/// Phase 32 — the full dependency graph; the frontend draws the node-edge chart.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityGraphDto {
    pub nodes: Vec<CapabilityGraphNodeDto>,
    pub edges: Vec<CapabilityGraphEdgeDto>,
}
