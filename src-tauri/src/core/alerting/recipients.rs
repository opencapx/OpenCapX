//! recipient CRUD + the recipient DTO.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 66 — recipient DTO shared by bundle + storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientDef {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default = "default_recipient_config")]
    pub config: serde_json::Value,
    #[serde(default = "default_true_for_recipient")]
    pub enabled: bool,
    #[serde(default)]
    pub created_at: u64,
}

fn default_recipient_config() -> serde_json::Value {
    serde_json::json!({})
}

fn default_true_for_recipient() -> bool {
    true
}

pub(crate) fn default_bundle_doc_version() -> u32 {
    1
}

/// Phase 67 — recipient kind whitelist. import bundle / save_recipient validate strictly,
/// preventing malicious yaml from writing arbitrary kinds such as cmd:rce. Extensible in Phase 68+.
pub const RECIPIENT_KIND_WHITELIST: &[&str] = &["webhook", "log:stderr", "log:file", "email:smtp"];

/// Validate kind ∈ whitelist; if absent → Err (with a hint listing available kinds).
pub fn validate_recipient_kind(kind: &str) -> Result<(), String> {
    if RECIPIENT_KIND_WHITELIST.contains(&kind) {
        Ok(())
    } else {
        Err(format!(
            "recipient kind '{kind}' not in whitelist ({}); refusing",
            RECIPIENT_KIND_WHITELIST.join(", ")
        ))
    }
}

/// Phase 67 — all recipients (by created_at ASC).
pub fn list_recipients() -> Vec<RecipientDef> {
    let Some(store) = crate::core::shared_store() else {
        return Vec::new();
    };
    let Ok(st) = store.lock() else {
        return Vec::new();
    };
    let StoreEnum::Db(db) = &*st else {
        return Vec::new();
    };
    db.list_alerting_recipients()
        .iter()
        .map(recipient_row_to_dto)
        .collect()
}

/// Phase 67 — upsert a recipient (name validation + UNIQUE constraint turned into Err by storage).
/// `rec.id` empty = create (auto-assigned); `rec.created_at == 0` = fill with now.
pub fn save_recipient(mut rec: RecipientDef) -> Result<RecipientDef, String> {
    validate_recipient_kind(&rec.kind)?;
    let name = rec.name.trim();
    if name.is_empty() {
        return Err("recipient name required".into());
    }
    if name.len() > 64 {
        return Err("recipient name too long (max 64)".into());
    }
    rec.name = name.to_string();
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut st) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *st else {
        return Err("sqlite store required".into());
    };
    if rec.id.is_empty() {
        rec.id = gen_event_id();
    }
    if rec.created_at == 0 {
        rec.created_at = crate::core::agent::now_secs();
    }
    let row = crate::core::storage::RecipientRow {
        id: rec.id.clone(),
        name: rec.name.clone(),
        kind: rec.kind.clone(),
        config: rec.config.clone(),
        enabled: rec.enabled,
        created_at: rec.created_at,
    };
    db.upsert_alerting_recipient(&row)
        .map_err(|e| format!("save recipient: {e}"))?;
    Ok(rec)
}

/// Phase 67 — Delete a recipient + cascade-clear refs to this id in routes.recipients.
/// Returns (deleted, routes_cleared).
pub fn delete_recipient(id: &str) -> Result<(bool, usize), String> {
    let Some(store) = crate::core::shared_store() else {
        return Err("store not initialized".into());
    };
    let Ok(mut st) = store.lock() else {
        return Err("store poisoned".into());
    };
    let StoreEnum::Db(db) = &mut *st else {
        return Err("sqlite store required".into());
    };
    Ok(db.delete_alerting_recipient(id))
}

/// Phase 67 — Try sending a synthetic alert to this recipient to verify the sink works.
/// Reuses Phase 66 `notification::resolve_recipient`; the spec is assembled from RecipientDef.config.
pub fn test_recipient(id: &str) -> Result<String, String> {
    let recipients = list_recipients();
    let rec = recipients
        .iter()
        .find(|r| r.id == id)
        .ok_or_else(|| format!("recipient not found: {id}"))?;
    if !rec.enabled {
        return Err(format!("recipient '{}' is disabled", rec.name));
    }
    // synthetic envelope
    let env = AlertEnvelope {
        schema_version: 1,
        event_id: format!("test-{}", crate::core::agent::now_secs()),
        source: "manual.test".into(),
        severity: Severity::Info,
        tags: vec!["phase67".into(), "manual-test".into()],
        timestamp: crate::core::agent::now_secs(),
        payload: serde_json::json!({"test": true, "recipient": rec.name}),
    };
    let body = serde_json::to_string(&env).unwrap_or_else(|_| "{}".into());
    // assemble the spec string
    let spec = match rec.kind.as_str() {
        "webhook" => {
            let ep_id = rec
                .config
                .get("endpoint_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "webhook recipient missing endpoint_id in config".to_string())?;
            format!("webhook:{ep_id}")
        }
        "log:stderr" => "log:stderr".to_string(),
        "log:file" => {
            let path = rec
                .config
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "log:file recipient missing path in config".to_string())?;
            format!("log:file:{path}")
        }
        "email:smtp" => {
            let relay = rec
                .config
                .get("relay")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let port = rec
                .config
                .get("port")
                .and_then(|v| v.as_u64())
                .unwrap_or(587);
            let from = rec
                .config
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let to = rec.config.get("to").and_then(|v| v.as_str()).unwrap_or("");
            format!("email:smtp:{relay}:{port}:{from}:{to}")
        }
        other => return Err(format!("unknown recipient kind: {other}")),
    };
    let endpoints = list_endpoints();
    let webhooks: Vec<WebhookEndpoint> = endpoints.iter().map(endpoint_row_to_dto).collect();
    let sink = crate::core::notification::resolve_recipient(&spec, &webhooks)?;
    sink.send(&env, &body)?;
    Ok(format!("sent via {}", sink.kind()))
}

/// Phase 67 — storage::RecipientRow → alerting::RecipientDef (DTO conversion).
fn recipient_row_to_dto(row: &crate::core::storage::RecipientRow) -> RecipientDef {
    RecipientDef {
        id: row.id.clone(),
        name: row.name.clone(),
        kind: row.kind.clone(),
        config: row.config.clone(),
        enabled: row.enabled,
        created_at: row.created_at,
    }
}
