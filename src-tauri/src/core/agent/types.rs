//! session vocabulary: AgentState, Session/SessionDto/ArchivedSession, Choice, the SessionSink trait, and the dto↔session conversions.
//! Mechanical move from core/agent.rs.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Waiting,
    Done,
    Idle,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub agent: String,
    pub project: String,
    /// Full working directory (the cwd of the hook payload). Used as the bubble grouping key + to read the git branch; may be empty.
    pub cwd: String,
    pub message: String,
    pub state: AgentState,
    /// Time the session first appeared (the old value is kept on upsert, not refreshed by later events).
    pub started_at: u64,
    pub updated_at: u64,
    /// Current model name (backfilled from the transcript tail when absent from the hook payload, see enrich_from_transcript).
    pub model: String,
    /// Agent body text (the last assistant text at the transcript tail), the bubble's second line.
    pub speech: String,
    /// Choice prompt (the agent offers A/B/C for the user to pick). None = a plain text bubble.
    pub choices: Option<Vec<Choice>>,
    /// The id the user has chosen; if Some, buttons are no longer shown.
    pub answered: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDto {
    pub id: String,
    pub agent: String,
    pub project: String,
    /// Full working directory (may be empty). Bubbles are grouped by it, and the group header reads the branch from it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub cwd: String,
    pub message: String,
    pub state: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    /// Sort key (the policy lives in Rust; the frontend only does lexicographic comparison). Not persisted.
    pub order: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// What the Agent itself said (the last assistant text at the transcript tail).
    /// `message` is "what it's doing" (tool/prompt); this is "what it said" — shown as the second line in the bubble,
    /// because the message of a done line gets replaced by the celebration text, and only this preserves the closing body.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub speech: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<Choice>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered: Option<String>,
}

/// One entry in the session history: the archived copy after a session expires from the active list.
#[derive(Debug, Clone, Serialize)]
pub struct ArchivedSession {
    pub id: String,
    pub agent: String,
    pub project: String,
    pub message: String,
    pub state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(rename = "startedAt")]
    pub started_at: u64,
    #[serde(rename = "endedAt")]
    pub ended_at: u64,
    /// Duration in seconds (endedAt - startedAt, clamped to zero on negatives/clock rollback).
    pub duration: u64,
}

/// Session storage abstraction: the in-memory implementation (tests/degraded mode) and the SQLite implementation share the interface.
pub trait SessionSink: Send {
    fn upsert(&mut self, s: Session);
    fn get(&self, id: &str) -> Option<Session>;
    fn active(&self, now: u64) -> Vec<Session>;
    fn all(&self) -> Vec<Session>;
    fn dismiss(&mut self, id: &str);
    fn clear(&mut self);
    /// Clear expired sessions out of the active list (the SQLite implementation archives them first), returning the number processed.
    fn sweep(&mut self, now: u64) -> usize;
}

pub fn session_to_dto(s: &Session) -> SessionDto {
    SessionDto {
        id: s.id.clone(),
        agent: s.agent.clone(),
        project: s.project.clone(),
        cwd: s.cwd.clone(),
        message: s.message.clone(),
        state: state_str(s.state).to_string(),
        started_at: s.started_at,
        updated_at: s.updated_at,
        order: order_key(s),
        model: s.model.clone(),
        speech: s.speech.clone(),
        choices: s.choices.clone(),
        answered: s.answered.clone(),
    }
}

pub fn dto_to_session(d: &SessionDto) -> Session {
    let state = match d.state.as_str() {
        "working" => AgentState::Working,
        "waiting" => AgentState::Waiting,
        "done" => AgentState::Done,
        _ => AgentState::Idle,
    };
    Session {
        id: d.id.clone(),
        agent: d.agent.clone(),
        project: d.project.clone(),
        cwd: d.cwd.clone(),
        message: d.message.clone(),
        state,
        started_at: d.started_at,
        updated_at: d.updated_at,
        model: d.model.clone(),
        speech: d.speech.clone(),
        choices: d.choices.clone(),
        answered: d.answered.clone(),
    }
}
