//! state ranking/sorting keys, TTL constants, and liveness (is_active).
//! Mechanical move from core/agent.rs.

use super::*;

pub fn state_str(s: AgentState) -> &'static str {
    match s {
        AgentState::Working => "working",
        AgentState::Waiting => "waiting",
        AgentState::Done => "done",
        AgentState::Idle => "idle",
    }
}

/// State priority: waiting on you > working > just finished > idle.
fn state_rank(s: AgentState) -> u8 {
    match s {
        AgentState::Waiting => 0,
        AgentState::Working => 1,
        AgentState::Done => 2,
        AgentState::Idle => 3,
    }
}

fn rank_of_str(state: &str) -> u8 {
    match state {
        "waiting" => 0,
        "working" => 1,
        "done" => 2,
        _ => 3,
    }
}

/// Sort key = priority (1 digit) + reversed timestamp (20 digits, newest first) + id.
/// Fixed-width format, so **lexicographic comparison** is the ordering: the frontend also only compares this key,
/// rather than reimplementing the policy (the policy lives only here; changing it changes both ends together).
pub fn order_key(s: &Session) -> String {
    format!(
        "{}{:020}{}",
        state_rank(s.state),
        u64::MAX - s.updated_at,
        s.id
    )
}

/// Isomorphic to `order_key`, for DTOs not yet persisted (event broadcasting needs it).
pub fn order_key_for_dto(d: &SessionDto) -> String {
    format!(
        "{}{:020}{}",
        rank_of_str(&d.state),
        u64::MAX - d.updated_at,
        d.id
    )
}

/// The single entry point for session ordering (shared by the bubble and the tray).
/// The tray used to have its own copy (`tray::sort_key`, sorting same-state by agent name),
/// and two copies of the rule would inevitably drift, so they were consolidated here.
pub fn sort_sessions(rows: &mut [Session]) {
    rows.sort_by_key(order_key);
}

/// String → state; unknown values return None (dirty data is not accepted).
pub fn parse_state(s: &str) -> Option<AgentState> {
    match s.trim().to_ascii_lowercase().as_str() {
        "working" => Some(AgentState::Working),
        "waiting" => Some(AgentState::Waiting),
        "done" => Some(AgentState::Done),
        "idle" => Some(AgentState::Idle),
        _ => None,
    }
}

/// Retention duration (seconds). **State-machine style**, not a one-size-fits-all TTL:
/// done stays a while so the user can see it; idle sessions stay longer for easy review;
/// working/waiting with no heartbeat for a long time means the agent is already dead (it sent no Stop).
pub const DONE_TTL_SECS: u64 = 30;

pub const IDLE_TTL_SECS: u64 = 600;

pub const STALE_ACTIVE_TTL_SECS: u64 = 900;

/// Whether the session is still in the "active list" (the tray/bubble only show active sessions).
pub fn is_active(s: &Session, now: u64) -> bool {
    let age = now.saturating_sub(s.updated_at);
    match s.state {
        AgentState::Done => age <= DONE_TTL_SECS,
        AgentState::Idle => age <= IDLE_TTL_SECS,
        AgentState::Working | AgentState::Waiting => age <= STALE_ACTIVE_TTL_SECS,
    }
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub(crate) fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
