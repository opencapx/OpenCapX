//! row types shared with the alerting and ratings layers.
//! Mechanical move from core/storage.rs.

/// Plugin rating (shared by the marketplace / installed plugins). Plugin Rating DTO, exposed to Tauri.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PluginRating {
    pub id: String,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub score: i64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub comment: Option<String>,
    pub ts: u64,
}

/// Phase 48 — one dead-letter delivery record. `payload` is a JSON string; the frontend parses it as needed.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailedDeliveryRow {
    pub id: String,
    pub source: String,
    pub url: String,
    pub payload: String,
    pub first_attempt_ts: u64,
    pub last_attempt_ts: u64,
    pub attempts: u32,
    pub max_attempts: u32,
    pub last_error: String,
    pub next_retry_ts: u64,
    pub state: String,
    /// Phase 49: the corresponding endpoint's id (may be None = legacy data from the Phase 47 single-endpoint path).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_id: Option<String>,
}

/// Phase 67 — alert recipient config (webhook / log:stderr / log:file / email:smtp).
/// `config` is a `serde_json::Value`; the schema differs per kind (jointly constrained by the UI form + import bundle).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipientRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    #[serde(default = "default_recipient_config_json")]
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: u64,
}

fn default_recipient_config_json() -> serde_json::Value {
    serde_json::json!({})
}

/// Phase 49 — a webhook endpoint config.
/// `headers_json` / `source_filter_json` are the JSON-serialized forms of Vec<(String,String)> / Vec<String>;
/// read out by deserializing, written in by serializing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertingEndpointRow {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub headers: Vec<(String, String)>,
    /// HMAC-SHA256 shared secret (empty = no signing). Stored in plaintext in SQLite (single-machine local, consistent with the other settings_kv).
    pub secret: String,
    /// Empty = accept all sources; non-empty = only these sources trigger.
    pub source_filter: Vec<String>,
    pub created_at: u64,
    /// Phase 52: `0` = Phase 47/49 legacy payload; `1` = canonical envelope. Default 0.
    #[serde(default)]
    pub schema_version: u32,
    /// Phase 57: per-endpoint template (`None` = use the Phase 47-56 default envelope; `Some(...)` = render this template
    /// to override the default body; supports `{{source}}` / `{{severity}}` / `{{timestamp}}` / `{{payload.x}}` + `{{#if expr}}`
    /// + `{{#each path}}`). See `core::alerting::render_template`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Phase 58: the sample JSON the user fills in while editing a template in the settings UI, used as the input envelope for live preview.
    /// `None` = use an empty `{}` as the payload during preview (only top-level fields are rendered). 16 KB limit.
    #[serde(
        default,
        rename = "templateSample",
        skip_serializing_if = "Option::is_none"
    )]
    pub template_sample: Option<String>,
    /// Phase 72 — per-source severity override, a JSON-serialized `Vec<(source, severity)>`.
    /// `None` or a parse failure → the endpoint is treated as having no override (uses the propagation result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity_overrides: Option<String>,
}

/// Phase 50 — a silence rule.
/// `starts_at`/`ends_at` are unix seconds; `weekdays` is a bitmask (Mon=1, Tue=2, ..., Sun=64);
/// `start_hour`/`end_hour` are local hours in 0..=24 (>=0 and <= end; for crossing midnight use start<end,
/// the current implementation does not support a 23→1 wrap). `kind_pattern` supports `*` match-all / `prefix.*` prefix match / exact match.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SilenceRuleRow {
    pub id: String,
    pub name: String,
    pub kind_pattern: String,
    pub starts_at: u64,
    pub ends_at: u64,
    pub weekdays: u8,
    pub start_hour: u8,
    pub end_hour: u8,
    pub created_at: u64,
}

/// Phase 50 — an ack suppression record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AckRuleRow {
    pub id: String,
    pub kind_pattern: String,
    pub ack_until: u64,
    pub created_at: u64,
}

/// Phase 51 — a DSL route rule.
/// `kind_pattern` matches the source kind (same Phase 50 semantics); `payload_path` + `payload_match` optionally
/// match payload fields (simple substring/regex); `target_endpoint_ids` is the endpoint id list (only these are sent on a hit),
/// `tags` are attached labels (can be surfaced in payload wrapping; currently metadata only). Lower `priority` matches first; the first enabled matching rule wins.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRuleRow {
    pub id: String,
    pub name: String,
    pub priority: i32,
    pub enabled: bool,
    pub kind_pattern: String,
    pub payload_path: Option<String>,
    pub payload_match: Option<String>,
    pub target_endpoint_ids: Vec<String>,
    /// Phase 66 — multi-channel recipient refs (each spec resolved by notification::resolve_recipient)
    pub recipients: Vec<String>,
    pub tags: Vec<String>,
    /// Phase 68 — time-window condition (a serde_json-serialized `SeenInLastSpec` or `null`).
    pub seen_in_last_json: Option<String>,
    pub created_at: u64,
}

/// Phase 59 — a template preset (user-defined; the 5 built-ins live in the `core::alerting::BUILTIN_PRESETS` constant).
/// `kind` distinguishes builtin / user: builtin uses `"builtin:<slug>"`, user uses `"user:<uuid>"` (user-modifiable);
/// `builtin=true` is also stored in SQLite, but `delete_template_preset` only deletes builtin=0 rows (to avoid accidental deletion).
/// Phase 60 — adds `version: u32` (auto-bumps on each save of the same id) + `changelog: Option<String>` (optional note).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePresetRow {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub kind: String,
    pub template: String,
    pub sample: Option<String>,
    pub builtin: bool,
    pub version: u32,
    pub changelog: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 53 — Per-source severity hint。
/// `origin = "manifest"` comes from the plugin manifest (`alerting.severityHints`) and is cleaned up when the plugin is uninstalled;
/// `origin = "user"` comes from manual user config in the settings UI and can be added/removed independently; `severity` is a lowercase string.
/// `plugin_id` has a value only for the manifest origin; `updated_at` is epoch seconds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityHintRow {
    pub source: String,
    pub severity: String,
    pub origin: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_id: Option<String>,
    pub updated_at: i64,
}

/// Phase 54 — frequency-threshold aggregation rule. `action` decides what happens after M hits within N seconds:
/// - `downgrade` → downgrade envelope.severity to `target_severity`
/// - `suppress` → skip this dispatch entirely (no webhook sent)
/// - `merge` → rewrite envelope.payload into a `{merged_count, since, last_payload}` summary description
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregationRuleRow {
    pub id: String,
    pub name: String,
    pub kind_pattern: String,
    pub window_secs: u64,
    pub threshold_count: u32,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_severity: Option<String>,
    pub enabled: bool,
    pub created_at: u64,
}

/// Phase 55 — a correlation suppression rule: B is suppressed within window_secs after A occurs.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorrelationRuleRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPatternA")]
    pub kind_pattern_a: String,
    #[serde(rename = "kindPatternB")]
    pub kind_pattern_b: String,
    #[serde(rename = "windowSecs")]
    pub window_secs: u64,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Phase 56 — an escalation rule: while source keeps firing within escalate_after_secs and is unacked, escalate to target_severity and send to target_endpoint_ids (empty fans out like a normal dispatch).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRuleRow {
    pub id: String,
    pub name: String,
    #[serde(rename = "kindPattern")]
    pub kind_pattern: String,
    #[serde(rename = "escalateAfterSecs")]
    pub escalate_after_secs: u64,
    #[serde(rename = "targetSeverity")]
    pub target_severity: String,
    #[serde(rename = "targetEndpointIds", skip_serializing_if = "Option::is_none")]
    pub target_endpoint_ids: Option<Vec<String>>,
    pub enabled: bool,
    #[serde(rename = "createdAt")]
    pub created_at: u64,
}

/// Rating summary (count + average, rounded to 2 decimals).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PluginRatingSummary {
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub count: i64,
    pub avg: f64,
}

/// A single capability stat row (for the Phase 30 Dashboard).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CapabilityStat {
    pub capability: String,
    #[serde(rename = "pluginId")]
    pub plugin_id: String,
    pub count: i64,
    #[serde(rename = "avgMs")]
    pub avg_ms: f64,
    #[serde(rename = "p50Ms")]
    pub p50_ms: i64,
    #[serde(rename = "p95Ms")]
    pub p95_ms: i64,
    #[serde(rename = "lastUsedAt")]
    pub last_used_at: u64,
    /// Number of non-ok samples (result != 'ok').
    #[serde(rename = "failCount")]
    pub fail_count: i64,
    /// Failure counts aggregated by error_kind (only samples with result != 'ok').
    #[serde(
        rename = "errors",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub errors: std::collections::BTreeMap<String, i64>,
}
