//! the alerting command surface (endpoints, templates, bundles, silences, acks, recipients, routes, severity, agg/corr/esc, simulator).
//! Mechanical move from main.rs.

/// Phase 59 — write a text file (for export to disk). Phase 59 adds no fs plugin, uses std::fs directly.
#[tauri::command]
pub(crate) fn write_text_file(path: String, content: String) -> Result<(), String> {
    crate::core::alerting::write_text_file(&path, &content)
}

/// Phase 59 — read a text file (for import reading).
#[tauri::command]
pub(crate) fn read_text_file(path: String) -> Result<String, String> {
    crate::core::alerting::read_text_file(&path)
}

/// Phase 60 — copy a builtin preset into a new user preset (assign a new UUID + user kind, version=1).
/// name is passed by the caller; changelog is auto-filled with "Forked from builtin 'X' (vN)".
#[tauri::command]
pub(crate) fn fork_alerting_template_preset(
    kind: String,
    name: String,
) -> Result<crate::core::alerting::TemplatePreset, String> {
    crate::core::alerting::fork_builtin_preset(&kind, &name)
}

/// Phase 62 — export the entire alerting state (endpoints + routes + presets + silences + acks) as YAML.
/// Phase 64 — passphrase optional: None → plaintext signed YAML; Some → AES-256-GCM encrypted envelope.
#[tauri::command]
pub(crate) fn export_alerting_bundle(passphrase: Option<String>) -> Result<String, String> {
    crate::core::alerting::export_alerting_bundle(passphrase.as_deref())
}

/// Phase 62 — import an alerting state bundle (YAML or envelope). Upserts section by section, preset builtin forced = false.
/// Returns per-section counts + total; rejects when version exceeds the current schema.
/// Phase 64 — passphrase optional: if the envelope needs one but none was passed → Err("passphrase required").
#[tauri::command]
pub(crate) fn import_alerting_bundle(
    content: String,
    passphrase: Option<String>,
) -> Result<crate::core::alerting::BundleImportSummary, String> {
    crate::core::alerting::import_alerting_bundle(&content, passphrase.as_deref())
}

/// Phase 50 — list all silences (the UI filters active / expired itself).
#[tauri::command]
pub(crate) fn list_alerting_silences() -> Vec<crate::core::storage::SilenceRuleRow> {
    crate::core::alerting::list_silences()
}

/// Phase 50 — upsert silence (empty id = create).
#[tauri::command]
pub(crate) fn save_alerting_silence(
    silence: crate::core::alerting::SilenceRule,
) -> Result<crate::core::storage::SilenceRuleRow, String> {
    crate::core::alerting::save_silence(silence)
}

/// Phase 50 — delete silence; returns true when deleted successfully.
#[tauri::command]
pub(crate) fn delete_alerting_silence(id: String) -> bool {
    crate::core::alerting::delete_silence(&id)
}

/// Phase 50 — list all acks (including expired, the UI filters).
#[tauri::command]
pub(crate) fn list_alerting_acks() -> Vec<crate::core::storage::AckRuleRow> {
    crate::core::alerting::list_acks()
}

/// Phase 50 — add an ack suppression for `kind_pattern`, expiring after window_secs seconds.
#[tauri::command]
pub(crate) fn ack_alerting_kind(
    kind_pattern: String,
    window_secs: u64,
) -> Result<crate::core::storage::AckRuleRow, String> {
    crate::core::alerting::ack_kind(kind_pattern, window_secs)
}

/// Phase 50 — delete the specified ack (for the UI cancel button).
#[tauri::command]
pub(crate) fn delete_alerting_ack(id: String) -> bool {
    crate::core::alerting::delete_ack(&id)
}

/// Phase 67 — all recipients (by created_at ASC). For the frontend Recipients tab.
#[tauri::command]
pub(crate) fn list_alerting_recipients() -> Vec<crate::core::alerting::RecipientDef> {
    crate::core::alerting::list_recipients()
}

/// Phase 67 — upsert a recipient. `rec.id` empty = create; `rec.created_at == 0` = fill in now.
/// `kind` must be in the whitelist, `name` non-empty and ≤ 64 chars; name UNIQUE conflict → Err.
#[tauri::command]
pub(crate) fn save_alerting_recipient(
    rec: crate::core::alerting::RecipientDef,
) -> Result<crate::core::alerting::RecipientDef, String> {
    crate::core::alerting::save_recipient(rec)
}

/// Phase 67 — delete a recipient + cascade-clean dangling refs to this id in routes.recipients.
/// Returns (deleted, routes_cleared).
#[tauri::command]
pub(crate) fn delete_alerting_recipient(id: String) -> Result<(bool, usize), String> {
    crate::core::alerting::delete_recipient(&id)
}

/// Phase 67 — send a synthetic alert to this recipient to verify the sink works.
#[tauri::command]
pub(crate) fn test_alerting_recipient_by_id(id: String) -> Result<String, String> {
    crate::core::alerting::test_recipient(&id)
}

/// Phase 68 — detect cycles in the alerting rule graph (route + correlation).
#[tauri::command]
pub(crate) fn detect_alerting_cycles() -> Vec<crate::core::alerting::CycleReport> {
    crate::core::alerting::detect_route_cycles()
}

/// Phase 68 — read the most recent N seen events (new→old), limit capped at 256.
#[tauri::command]
pub(crate) fn recent_alerting_events(limit: usize) -> Vec<crate::core::alerting::RouteSeenEvent> {
    crate::core::alerting::recent_events_snapshot(limit)
}

/// Phase 69 — return the 4-layer severity chain for a source.
#[tauri::command]
pub(crate) fn severity_inheritance_chain(
    source: String,
) -> Vec<crate::core::alerting::SeverityLink> {
    crate::core::alerting::severity_inheritance_chain(&source)
}

/// Phase 69 — delete a user-origin severity hint and list affected routes / correlations / aggregations.
#[tauri::command]
pub(crate) fn delete_alerting_severity_hint_cascade(
    source: String,
) -> crate::core::alerting::CascadeReport {
    crate::core::alerting::cascade_delete_severity_hint(&source)
}

/// Phase 70 — for a source, list which severity the 4 main paths (route / correlation / aggregation / escalation) actually use + which link of the chain it comes from.
#[tauri::command]
pub(crate) fn severity_propagation_trace(
    source: String,
) -> crate::core::alerting::PropagationTrace {
    crate::core::alerting::propagation_trace(&source)
}

/// Phase 74 — dry-run preview endpoint severity override (pure function + reads list_alerting_endpoints, no dispatch side effects)
/// endpoint_id = None → all enabled endpoints; Some(id) → only that endpoint
#[tauri::command]
pub(crate) fn preview_alerting_endpoint_severity(
    source: String,
    endpoint_id: Option<String>,
) -> Result<crate::core::alerting::EndpointSeverityPreview, String> {
    crate::core::alerting::preview_alerting_endpoint_severity(&source, endpoint_id.as_deref())
}

/// Phase 75 — Alerting full-chain dry-run simulator: given a source + optional payload, runs side-effect-free through
/// the five links route / correlation / aggregation / escalation + endpoint fanout, returning the decision at each stage.
#[tauri::command]
pub(crate) fn simulate_alerting_dispatch(
    source: String,
    payload: Option<String>,
) -> Result<crate::core::alerting::AlertingDispatchSimulation, String> {
    crate::core::alerting::simulate_alerting_dispatch(&source, payload.as_deref())
}

/// Phase 50 — clear expired acks (can run at startup or manually).
#[tauri::command]
pub(crate) fn clear_expired_alerting_acks() -> usize {
    crate::core::alerting::clear_expired_acks()
}

/// Phase 51 — list all DSL route rules (including disabled, by priority ASC).
#[tauri::command]
pub(crate) fn list_alerting_routes() -> Vec<crate::core::storage::RouteRuleRow> {
    crate::core::alerting::list_routes()
}

/// Phase 51 — upsert a DSL route (empty id = create).
#[tauri::command]
pub(crate) fn save_alerting_route(
    rule: crate::core::alerting::RouteRule,
) -> Result<crate::core::storage::RouteRuleRow, String> {
    crate::core::alerting::save_route(rule)
}

/// Phase 51 — delete a DSL route.
#[tauri::command]
pub(crate) fn delete_alerting_route(id: String) -> bool {
    crate::core::alerting::delete_route(&id)
}

/// Phase 51 — YAML import (atomic replace).
#[tauri::command]
pub(crate) fn import_alerting_routes_yaml(yaml: String) -> Result<usize, String> {
    crate::core::alerting::import_routes_yaml(&yaml)
}

/// Phase 51 — YAML export.
#[tauri::command]
pub(crate) fn export_alerting_routes_yaml() -> Result<String, String> {
    crate::core::alerting::export_routes_yaml()
}

/// Phase 51 — Dry-run: use `(source, payload_json)` to see which route matches.
#[tauri::command]
pub(crate) fn dry_run_alerting_route(
    source: String,
    payload_json: String,
) -> Result<Option<crate::core::storage::RouteRuleRow>, String> {
    crate::core::alerting::dry_run_route(&source, &payload_json)
}

/// Phase 53 — list all severity hints (user + manifest).
#[tauri::command]
pub(crate) fn list_alerting_severity_hints() -> Vec<crate::core::alerting::SeverityHintDto> {
    crate::core::alerting::list_severity_hints()
}

/// Phase 53 — User saves a hint (source non-empty + severity valid).
#[tauri::command]
pub(crate) fn save_alerting_severity_hint(
    source: String,
    severity: String,
) -> Result<crate::core::alerting::SeverityHintDto, String> {
    crate::core::alerting::save_user_severity_hint(&source, &severity)
}

/// Phase 53 — delete a user-origin hint.
#[tauri::command]
pub(crate) fn delete_alerting_severity_hint(source: String) -> bool {
    crate::core::alerting::delete_user_severity_hint(&source)
}

/// Phase 53 — clear all user-origin hints.
#[tauri::command]
pub(crate) fn clear_alerting_severity_hints() -> usize {
    crate::core::alerting::clear_user_severity_hints()
}

/// Phase 54 — list all aggregation rules (for the settings UI).
#[tauri::command]
pub(crate) fn list_alerting_aggregations() -> Vec<crate::core::alerting::AggregationRuleDto> {
    crate::core::alerting::list_aggregations()
}

/// Phase 54 — create / update an aggregation rule (auto-fills agg-<uuid> when id is empty).
#[tauri::command]
pub(crate) fn save_alerting_aggregation(
    rule: crate::core::alerting::AggregationRule,
) -> Result<crate::core::alerting::AggregationRuleDto, String> {
    crate::core::alerting::save_aggregation(rule)
}

/// Phase 54 — delete an aggregation rule.
#[tauri::command]
pub(crate) fn delete_alerting_aggregation(id: String) -> bool {
    crate::core::alerting::delete_aggregation(&id)
}

/// Phase 54 — clear all aggregation rules + reset in-memory buckets (for the settings UI + tests).
#[tauri::command]
pub(crate) fn clear_alerting_aggregations() -> usize {
    let n = crate::core::alerting::clear_aggregations();
    crate::core::alerting::_reset_aggregations_for_tests();
    n
}

/// Phase 55 — list all correlation suppression rules (for the settings UI).
#[tauri::command]
pub(crate) fn list_alerting_correlations() -> Vec<crate::core::alerting::CorrelationRuleDto> {
    crate::core::alerting::list_correlations()
}

/// Phase 55 — create / update a correlation rule (auto-fills cor-<uuid> when id is empty).
#[tauri::command]
pub(crate) fn save_alerting_correlation(
    rule: crate::core::alerting::CorrelationRule,
) -> Result<crate::core::alerting::CorrelationRuleDto, String> {
    crate::core::alerting::save_correlation(rule)
}

/// Phase 55 — delete a correlation rule.
#[tauri::command]
pub(crate) fn delete_alerting_correlation(id: String) -> bool {
    crate::core::alerting::delete_correlation(&id)
}

/// Phase 55 — clear all correlation rules + reset last_a memory (for the settings UI + tests).
#[tauri::command]
pub(crate) fn clear_alerting_correlations() -> usize {
    let n = crate::core::alerting::clear_correlations();
    crate::core::alerting::_reset_correlations_for_tests();
    n
}

/// Phase 56 — list all escalation rules (for the settings UI).
#[tauri::command]
pub(crate) fn list_alerting_escalations() -> Vec<crate::core::alerting::EscalationRuleDto> {
    crate::core::alerting::list_escalations()
}

/// Phase 56 — create / update an escalation rule (auto-fills esc-<uuid> when id is empty).
#[tauri::command]
pub(crate) fn save_alerting_escalation(
    rule: crate::core::alerting::EscalationRule,
) -> Result<crate::core::alerting::EscalationRuleDto, String> {
    crate::core::alerting::save_escalation(rule)
}

/// Phase 56 — delete an escalation rule.
#[tauri::command]
pub(crate) fn delete_alerting_escalation(id: String) -> bool {
    crate::core::alerting::delete_escalation(&id)
}

/// Phase 56 — clear all escalation rules + reset last_by_source memory (for the settings UI + tests).
#[tauri::command]
pub(crate) fn clear_alerting_escalations() -> usize {
    let n = crate::core::alerting::clear_escalations();
    crate::core::alerting::_reset_escalations_for_tests();
    n
}
