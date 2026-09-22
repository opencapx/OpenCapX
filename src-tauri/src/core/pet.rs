//! Pet state machine: agent.* events → pet.state events. Aligned with i1.md §15:
//! The mapping from Agent state to Pet animation lives in Core; the actual animation is decided by the Pet plugin.
//!
//! Frontend overlay.ts subscribes to `pet.state` to render the status badge + default message;
//! the Pet plugin can also plugin.subscribe("pet.state") to receive the same events and drive custom animations.

use super::event::{OpencapxEvent, EventBus};
use serde_json::json;
use std::sync::Arc;

/// agent.* / permission.* events → pet state (Agent State Protocol, i2 §12).
/// Pure function (easy to unit-test). idle is the default (no recent agent activity); success appears briefly,
/// and the frontend/pet plugin falls back on its own.
pub fn map_agent_to_pet(agent_kind: &str) -> Option<&'static str> {
    match agent_kind {
        "agent.started" => Some("idle"),
        "agent.working" => Some("working"),
        "agent.waiting" => Some("waiting"),
        "agent.completed" => Some("success"),
        "agent.error" => Some("error"),
        // When Core prompts for permission confirmation the pet enters "pleading" — a key human-computer interaction moment
        "permission.requested" => Some("permission"),
        _ => None,
    }
}

/// Start the background thread: subscribe to the EventBus and translate agent.* into pet.state pushed to the bus.
pub fn spawn_state_mapper(bus: Arc<EventBus>) {
    std::thread::spawn(move || {
        for e in bus.subscribe() {
            if let Some(pet_state) = map_agent_to_pet(&e.kind) {
                let payload = json!({
                    "state": pet_state,
                    "source": "core",
                    "fromEvent": e.kind,
                    "sessionId": e.payload.get("sessionId").cloned().unwrap_or(json!(null)),
                    // agent.* carries agent; permission.* carries agentId
                    "agent": e
                        .payload
                        .get("agent")
                        .or_else(|| e.payload.get("agentId"))
                        .cloned()
                        .unwrap_or(json!(null)),
                });
                bus.publish(&OpencapxEvent::new(
                    "pet.state",
                    "core",
                    payload,
                ));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_agent_events_to_pet_state() {
        assert_eq!(map_agent_to_pet("agent.started"), Some("idle"));
        assert_eq!(map_agent_to_pet("agent.working"), Some("working"));
        assert_eq!(map_agent_to_pet("agent.waiting"), Some("waiting"));
        assert_eq!(map_agent_to_pet("agent.completed"), Some("success"));
        assert_eq!(map_agent_to_pet("agent.error"), Some("error"));
    }

    /// i2 §12 — permission confirmation maps to a distinct `permission` state (separate from waiting).
    #[test]
    fn maps_permission_requested_to_permission_state() {
        assert_eq!(map_agent_to_pet("permission.requested"), Some("permission"));
        // granted/denied are not "pleading" and do not enter the state machine
        assert_eq!(map_agent_to_pet("permission.granted"), None);
        assert_eq!(map_agent_to_pet("permission.denied"), None);
    }

    #[test]
    fn ignores_non_agent_events() {
        assert_eq!(map_agent_to_pet("pet.bubble"), None);
        assert_eq!(map_agent_to_pet("plugin.log"), None);
        assert_eq!(map_agent_to_pet(""), None);
    }
}