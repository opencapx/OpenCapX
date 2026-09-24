//! Plugin manifest schema: RuntimeManifest, Manifest, CapabilityDecl, the sandbox declaration, and Manifest helpers.
//! Mechanical move from core/plugin.rs.

use super::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeManifest {
    #[serde(rename = "type")]
    pub rtype: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    #[serde(rename = "type")]
    pub ptype: String,
    pub runtime: Option<RuntimeManifest>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityDecl>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub states: Vec<String>,
    /// Phase 53 — optional alerting sub-config (severityHints table).
    /// Old manifests lack this field → `#[serde(default)]` → None.
    #[serde(default)]
    pub alerting: Option<crate::core::alerting::AlertingManifest>,
    /// F4 — semantic version floor: install/start only when core_version >= minCoreVersion.
    #[serde(
        rename = "minCoreVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub min_core_version: Option<String>,
    /// F5 — inter-plugin dependencies: pluginId -> semver VersionReq.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependencies: BTreeMap<String, String>,
    /// M7/F8 — declarative settings: Core renders the generic settings page; the real values still live in config.* (the declaration only drives the UI).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<SettingDecl>,
    /// S5 — sandbox declaration (optional); whether it is enforced is also bound by the sandbox_enforcement switch and trust level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxDecl>,
    /// Wow 6 signature/provenance fields. Previously dropped by serde → now fully persisted (old manifests unaffected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// Shape is owned by plugin_sig: keep the raw JSON, no strong typing here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<serde_json::Value>,
}

/// §4.2 capability declaration shape: "string | object" (untagged parse; old manifests work as-is).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CapabilityDecl {
    /// String form: provides an implementation of a **built-in capability**; permissions follow the static mapping.
    Plain(String),
    /// Object form: plugin-domain declaration (capability → permission + default); permissions are frozen by this declaration.
    Mapping {
        id: String,
        permission: String,
        /// ask | denied (§4.2 default is restricted); default = ask
        #[serde(default)]
        default: Option<String>,
        /// S4 — call timeout for this capability (seconds, 1..=600); default = CALL_TIMEOUT(60s).
        #[serde(rename = "timeoutSecs", default)]
        timeout_secs: Option<u32>,
    },
}

impl CapabilityDecl {
    pub fn id(&self) -> &str {
        match self {
            CapabilityDecl::Plain(s) => s,
            CapabilityDecl::Mapping { id, .. } => id,
        }
    }

    /// The (permission, default) of the object form; returns None for the string form.
    pub fn mapping(&self) -> Option<(&str, &str)> {
        match self {
            CapabilityDecl::Plain(_) => None,
            CapabilityDecl::Mapping {
                permission,
                default,
                ..
            } => Some((permission, default.as_deref().unwrap_or("ask"))),
        }
    }

    /// Whether this is a plugin-domain declaration (→ declaration table / once-only / "unverified domain" marker).
    pub fn is_declared(&self) -> bool {
        matches!(self, CapabilityDecl::Mapping { .. })
    }
}

/// S5 — sandbox declaration (macOS execution layer in core::sandbox; the declaration itself is portable across platforms).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxDecl {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<SandboxFs>,
    /// "none" | "out"; enforced as none (least privilege) by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SandboxFs {
    /// Currently only `["plugin-data"]` is supported (= `~/.opencapx/plugin-data/<id>/`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub write: Vec<String>,
}

/// S5a — sandbox declaration validation (shared by install time + auto-gate category 6).
pub(crate) fn validate_sandbox(s: &SandboxDecl) -> Result<(), String> {
    if let Some(net) = &s.network {
        if net != "none" && net != "out" {
            return Err(format!(
                "invalid sandbox.network {:?} (expect none|out)",
                net
            ));
        }
    }
    if let Some(fs) = &s.fs {
        for entry in &fs.write {
            if entry != "plugin-data" {
                return Err(format!(
                    "unsupported sandbox.fs.write entry {:?} (only \"plugin-data\" supported)",
                    entry
                ));
            }
        }
    }
    Ok(())
}

impl Manifest {
    /// List of declared capability IDs (string form + object form; the latter has no duplicates).
    pub fn capability_ids(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .map(|c| c.id().to_string())
            .collect()
    }

    /// Object-form declarations → (capability, permission, default), used by the commit phase to write the declaration table.
    pub fn declarations(&self) -> Vec<(String, String, String, Option<u32>)> {
        self.capabilities
            .iter()
            .filter_map(|c| {
                c.mapping().map(|(perm, default)| {
                    let timeout = match c {
                        CapabilityDecl::Mapping { timeout_secs, .. } => *timeout_secs,
                        CapabilityDecl::Plain(_) => None,
                    };
                    (
                        c.id().to_string(),
                        perm.to_string(),
                        default.to_string(),
                        timeout,
                    )
                })
            })
            .collect()
    }

    /// Set of permission names from inline-mapping declarations (half of the §4.4 step 4 confirmation set).
    pub fn declared_permission_names(&self) -> Vec<String> {
        self.capabilities
            .iter()
            .filter_map(|c| c.mapping().map(|(p, _)| p.to_string()))
            .collect()
    }
}
