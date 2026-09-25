//! chain export to disk (ExportedChain/ExportReport).
//! Mechanical move from core/req_trace.rs.

use super::*;
use serde::{Deserialize, Serialize};

/// One written request chain in the export manifest (frontend contract, camelCase fields).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportedChain {
    pub trace_id: String,
    /// The file name in the target directory (file name only, e.g. `rpc-1789576401787063000.ndjson`).
    pub file: String,
    pub agent_id: String,
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub line_count: u64,
    pub size_bytes: u64,
    pub status: String,
}

/// Record of a single chain copy failure: not silent, returned per item so the frontend can notify.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFailure {
    pub trace_id: String,
    pub reason: String,
}

/// Project export report. Error code convention: `no_chains` / `dir_unwritable` / `io: <detail>`,
/// for frontend localization (see export_chains_to).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub dir: String,
    pub project: String,
    /// Number of files successfully written.
    pub exported: u64,
    /// Of these, the number of files that already existed before overwrite (pre-overwrite probe).
    pub overwritten: u64,
    /// `<dir>/index.json`。
    pub manifest_path: String,
    pub chains: Vec<ExportedChain>,
    pub failures: Vec<ExportFailure>,
}

/// Copy all /rpc request chains of a project **raw bytes** to `<dir>/<sanitize(project)>/`
/// (using `_no-project` when the project string is empty), and write the index.json manifest in that subdirectory.
/// Reuses the viewer's enumeration path (list_all_in) + project_from_line ownership decision, guaranteeing the export
/// matches what the user sees; hook traces are out of scope. Copies one by one; a single failure is recorded in failures and it continues.
/// Error codes: no chains → "no_chains"; cannot create directory → "dir_unwritable"; otherwise → "io: <detail>".
pub fn export_chains_to(dir: &str, project: &str) -> Result<ExportReport, String> {
    // Same function as the viewer's full enumeration: rpc tree, grouped by the project of the first line's root start.
    let selected: Vec<(u64, TraceEntry)> = list_all_in(&rpc_traces_root(), true)
        .into_iter()
        .filter(|(_, e)| e.project == project)
        .collect();
    if selected.is_empty() {
        return Err("no_chains".into());
    }
    // The project name goes through sanitize as a whole, yielding a **single-level** directory name (not split on /, no nesting).
    // The project string may be "" (old traces before the project header): if empty after sanitize, use a stable ASCII fallback name.
    let project_dir_name = {
        let s = crate::core::plugin_trace::sanitize(project);
        if s.is_empty() {
            "_no-project".to_string()
        } else {
            s
        }
    };
    // First locate the actual write directory `<dir>/<project-dir-name>`, and only create the directory after validating the chain list
    // (no_chains already early-returns above, so no empty directory is left); if creation fails, writing is impossible.
    let out_dir = Path::new(dir).join(&project_dir_name);
    if std::fs::create_dir_all(&out_dir).is_err() {
        return Err("dir_unwritable".into());
    }

    let mut chains = Vec::with_capacity(selected.len());
    let mut failures = Vec::new();
    let mut overwritten = 0u64;
    for (_, e) in selected {
        // Source path same as read_trace_at: both the agent/trace segments go through sanitize.
        let src = rpc_traces_root()
            .join(crate::core::plugin_trace::sanitize(&e.agent_id))
            .join(format!(
                "{}.ndjson",
                crate::core::plugin_trace::sanitize(&e.trace_id)
            ));
        let file = format!(
            "{}.ndjson",
            crate::core::plugin_trace::sanitize(&e.trace_id)
        );
        let dst = out_dir.join(&file);
        // Pre-overwrite probe: existing files count toward overwritten (only counted for successfully written files).
        let pre_existing = dst.exists();
        // Byte-level copy (no JSON round-trip): std::fs::copy.
        match std::fs::copy(&src, &dst) {
            Ok(_) => {
                if pre_existing {
                    overwritten += 1;
                }
                chains.push(ExportedChain {
                    trace_id: e.trace_id,
                    file,
                    agent_id: e.agent_id,
                    started_at: e.started_at,
                    ended_at: e.ended_at,
                    line_count: e.line_count,
                    size_bytes: e.size_bytes,
                    status: e.status,
                });
            }
            Err(err) => failures.push(ExportFailure {
                trace_id: e.trace_id,
                reason: err.to_string(),
            }),
        }
    }

    let exported = chains.len() as u64;
    let manifest_path = out_dir.join("index.json");
    let manifest = json!({
        "schema": 1,
        "exportedAt": now_ms(),
        "project": project,
        "chainCount": exported,
        "chains": chains.clone(),
    });
    let manifest_text =
        serde_json::to_string_pretty(&manifest).map_err(|e| format!("io: {}", e))?;
    std::fs::write(&manifest_path, manifest_text).map_err(|e| format!("io: {}", e))?;

    Ok(ExportReport {
        dir: out_dir.to_string_lossy().into_owned(),
        project: project.to_string(),
        exported,
        overwritten,
        manifest_path: manifest_path.to_string_lossy().into_owned(),
        chains,
        failures,
    })
}
