//! the NDJSON trace line shapes (SpanStatus, RpcTraceLine).
//! Mechanical move from core/req_trace.rs.

use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpanStatus {
    Ok,
    Error,
}

/// Single-line NDJSON, the `ev` tag distinguishes the three states; camelCase aligns with the frontend interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum RpcTraceLine {
    Start {
        #[serde(rename = "spanId")]
        span_id: String,
        #[serde(rename = "parentId", default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        name: String,
        ts: u64,
        attrs: Value,
    },
    Event {
        #[serde(rename = "spanId")]
        span_id: String,
        name: String,
        ts: u64,
        attrs: Value,
    },
    End {
        #[serde(rename = "spanId")]
        span_id: String,
        ts: u64,
        status: SpanStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attrs: Option<Value>,
    },
}
