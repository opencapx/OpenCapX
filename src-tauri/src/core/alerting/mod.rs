//! Phase 47 — unify the alert events from Phase 36 / 40 / 44 / 45 and push them to external webhooks.
//!
//! Listens for four kinds of event on the EventBus:
//! - `plugin.metrics.exceeded` (Phase 45: CPU / memory / thread count over threshold)
//! - `capability.sla.violated` (Phase 36: p95 latency / failure rate over threshold)
//! - `plugin.kill_switch.enabled` (Phase 44: user enables the global kill switch)
//! - `plugin.lifecycle.crashed` (Phase 40: plugin process crashed)
//!
//! Each event carries a `WebhookSources` flag deciding whether to forward it; once enabled it goes through dedup → POST.
//!
//! Persistence: `settings_kv` key = `alerting_webhook_config`, the full config as JSON.
//!
//! dedup: within a `min_interval_secs` second window the same `(source, payload-hash)` is not re-sent,
//! preventing a sustained 99% CPU from flooding the webhook. The hash uses DefaultHasher (non-cryptographic, used only for
//! short-window dedup).
//!
//! URL validation: must start with `http://` or `https://`, length ≤ 2048, no `..` / newline.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::event::{EventBus, OpencapxEvent};
use super::marketplace::hmac_sha256_hex;
use super::storage::{
    AckRuleRow, AggregationRuleRow, AlertingEndpointRow, CorrelationRuleRow, EscalationRuleRow,
    FailedDeliveryRow, RouteRuleRow, SilenceRuleRow, StoreEnum,
};

mod aggregations;
mod bundle;
mod config;
mod correlations;
mod dispatch;
mod endpoints;
mod escalations;
mod presets;
mod recipients;
mod retry;
mod routes;
mod severity;
mod silences;
mod simulator;
mod template;
mod timeline;
mod types;
mod util;

pub use aggregations::*;
pub use bundle::*;
pub use config::*;
pub use correlations::*;
pub use dispatch::*;
pub use endpoints::*;
pub use escalations::*;
pub use presets::*;
pub use recipients::*;
pub use retry::*;
pub use routes::*;
pub use severity::*;
pub use silences::*;
pub use simulator::*;
pub use template::*;
pub use timeline::*;
pub use types::*;
pub use util::*;

#[cfg(test)]
mod tests;
