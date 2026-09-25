//! permission views for the UI: plugin/agent entry DTOs and the permission heatmap.
//! Mechanical move from core/permission.rs.

use super::*;

#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionEntryDto {
    pub permission: String,
    /// Currently effective decision (DB override > default table)
    pub decision: String,
    pub default: String,
    pub high_risk: bool,
    /// §4.7: declared-derived ("unverified domain" marker + once-only, the UI does not offer granted)
    pub declared: bool,
    /// Global policy override (permission_policy table; "" = no override). denied is a hard gate,
    /// overriding every per-agent granted — the UI shows the "global" badge based on this.
    #[serde(default)]
    pub global: String,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginPermissionsDto {
    pub plugin_id: String,
    pub name: String,
    pub status: String,
    pub permissions: Vec<PermissionEntryDto>,
}

/// Settings page Permissions view data: grouped by plugin, taking the permissions declared in the manifest
/// **∪ the plugin's permissions in the frozen declaration table** (§4.3 integration point #6: inline-mapped permissions may be absent from
/// `permissions[]`; missing them means "installed but invisible and uncontrollable on the settings page").
pub fn view(
    store: &SharedStore,
    plugins: &[crate::core::plugin::PluginStatusDto],
) -> Vec<PluginPermissionsDto> {
    let all_decls = crate::core::declaration::all(store);
    plugins
        .iter()
        .map(|p| {
            let mut perms: Vec<String> = p.permissions.clone();
            for d in all_decls.iter().filter(|d| d.plugin_id == p.id) {
                if !perms.contains(&d.permission) {
                    perms.push(d.permission.clone());
                }
            }
            perms.sort();
            PluginPermissionsDto {
                plugin_id: p.id.clone(),
                name: p.name.clone(),
                status: p.status.clone(),
                permissions: perms
                    .iter()
                    .map(|perm| PermissionEntryDto {
                        permission: perm.clone(),
                        decision: decision_str(check(store, &p.id, perm)).to_string(),
                        default: decision_str(default_decision_for(store, perm)).to_string(),
                        high_risk: HIGH_RISK.contains(&perm.as_str()),
                        declared: is_declared(store, perm),
                        global: global_override_str(store, perm).to_string(),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// Settings page Agents view data: the full permission vocabulary for a single agent (all listed,
/// decision = agent_permissions override > default table; default lets the frontend mark "unchanged").
/// §4.3 integration point #6: vocabulary = static table ∪ frozen declaration table, otherwise new-domain permissions are "denied by default and impossible to grant".
pub fn agent_view(store: &SharedStore, agent_id: &str) -> Vec<PermissionEntryDto> {
    let mut entries: Vec<PermissionEntryDto> = PERMISSIONS
        .iter()
        .map(|(perm, default)| PermissionEntryDto {
            permission: perm.to_string(),
            decision: decision_str(crate::core::identity::check_agent(store, agent_id, perm))
                .to_string(),
            default: default.to_string(),
            high_risk: HIGH_RISK.contains(perm),
            declared: false,
            global: global_override_str(store, perm).to_string(),
        })
        .collect();
    for (perm, default) in crate::core::declaration::declared_permission_defaults(store) {
        if entries.iter().any(|e| e.permission == perm) {
            continue;
        }
        entries.push(PermissionEntryDto {
            decision: decision_str(crate::core::identity::check_agent(store, agent_id, &perm))
                .to_string(),
            default: decision_str(default).to_string(),
            high_risk: HIGH_RISK.contains(&perm.as_str()),
            declared: true,
            global: global_override_str(store, &perm).to_string(),
            permission: perm,
        });
    }
    entries.sort_by(|a, b| a.permission.cmp(&b.permission));
    entries
}

/// Phase 33 — permission risk heatmap: one decision cell per (plugin, permission) +
/// a global permission-denial ranking (for the UI's "top-N most-denied permissions across your installed plugins").
#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionHeatmapCellDto {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub permission: String,
    pub decision: String,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
    /// §4.7: declared-derived (unverified domain)
    pub declared: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TopPermissionDto {
    pub permission: String,
    #[serde(rename = "grantedCount")]
    pub granted_count: i64,
    #[serde(rename = "deniedCount")]
    pub denied_count: i64,
    #[serde(rename = "askCount")]
    pub ask_count: i64,
    #[serde(rename = "highRisk")]
    pub high_risk: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PermissionHeatmapDto {
    pub cells: Vec<PermissionHeatmapCellDto>,
    #[serde(rename = "topDenied")]
    pub top_denied: Vec<TopPermissionDto>,
}

/// Reads the plugin_permissions table from shared_store and returns heatmap data.
/// Deadlock fix: the is_declared family re-locks the store internally — never call it inside a with_conn_ref closure
/// (previously calling is_declared_permission per cell inside the closure → same-thread reentrant deadlock, freezing the plugin tab instantly).
/// Now: hold the lock to read raw rows → release it → fetch the declaration set with one DISTINCT → merge in memory (which also kills the N+1).
pub fn heatmap() -> PermissionHeatmapDto {
    use crate::core::shared_store;
    let Some(store) = shared_store() else {
        return PermissionHeatmapDto {
            cells: Vec::new(),
            top_denied: Vec::new(),
        };
    };
    // (1) hold the lock only to read raw rows; do nothing inside the lock that would re-acquire it
    let raw: Vec<(String, String, String)> = {
        let Ok(s) = store.lock() else {
            return PermissionHeatmapDto {
                cells: Vec::new(),
                top_denied: Vec::new(),
            };
        };
        let mut out: Vec<(String, String, String)> = Vec::new();
        let _ = s.with_conn_ref(|c| {
            let Ok(mut stmt) = c.prepare(
                "SELECT plugin_id, permission, decision FROM plugin_permissions ORDER BY plugin_id ASC, permission ASC",
            ) else {
                return 0usize;
            };
            let Ok(rows) = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
            }) else {
                return 0usize;
            };
            out = rows.flatten().collect();
            0usize
        });
        out
        // the guard is released here
    };
    // (2) after releasing, query the declaration set once (re-locking is now safe)
    let declared = crate::core::declaration::declared_permission_set(&store);
    let cells: Vec<PermissionHeatmapCellDto> = raw
        .into_iter()
        .map(
            |(plugin_id, permission, decision)| PermissionHeatmapCellDto {
                high_risk: HIGH_RISK.contains(&permission.as_str()),
                declared: declared.contains(&permission),
                plugin_id,
                permission,
                decision,
            },
        )
        .collect();
    // aggregate by permission (granted / denied / ask counts)
    use std::collections::BTreeMap;
    let mut agg: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
    for cell in &cells {
        let e = agg.entry(cell.permission.clone()).or_insert((0, 0, 0));
        match cell.decision.as_str() {
            "granted" => e.0 += 1,
            "denied" => e.1 += 1,
            "ask" => e.2 += 1,
            _ => {}
        }
    }
    let mut top_denied: Vec<TopPermissionDto> = agg
        .into_iter()
        .map(|(permission, (granted, denied, ask))| TopPermissionDto {
            high_risk: HIGH_RISK.contains(&permission.as_str()),
            permission,
            granted_count: granted,
            denied_count: denied,
            ask_count: ask,
        })
        .collect();
    // denied descending, then granted descending, then permission ascending (stable)
    top_denied.sort_by(|a, b| {
        b.denied_count
            .cmp(&a.denied_count)
            .then(b.granted_count.cmp(&a.granted_count))
            .then(a.permission.cmp(&b.permission))
    });
    // cap at top 10
    top_denied.truncate(10);
    PermissionHeatmapDto { cells, top_denied }
}
