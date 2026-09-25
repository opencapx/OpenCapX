//! SessionStore: the in-memory live-session map behind the SessionSink trait.
//! Mechanical move from core/agent.rs.

use super::*;

#[derive(Debug, Default)]
pub struct SessionStore {
    inner: HashMap<String, Session>,
}

impl SessionStore {
    pub fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }
}

impl SessionSink for SessionStore {
    fn upsert(&mut self, mut s: Session) {
        // started_at is the "time the session first appeared"; keep the old value when it already exists
        if let Some(old) = self.inner.get(&s.id) {
            if old.started_at > 0 {
                s.started_at = old.started_at;
            }
        }
        self.inner.insert(s.id.clone(), s);
    }

    fn get(&self, id: &str) -> Option<Session> {
        self.inner.get(id).cloned()
    }

    fn active(&self, now: u64) -> Vec<Session> {
        self.inner
            .values()
            .filter(|s| is_active(s, now))
            .cloned()
            .collect()
    }

    fn all(&self) -> Vec<Session> {
        self.inner.values().cloned().collect()
    }

    fn dismiss(&mut self, id: &str) {
        self.inner.remove(id);
    }

    fn clear(&mut self) {
        self.inner.clear();
    }

    fn sweep(&mut self, now: u64) -> usize {
        let expired: Vec<String> = self
            .inner
            .values()
            .filter(|s| !is_active(s, now))
            .map(|s| s.id.clone())
            .collect();
        for id in &expired {
            self.inner.remove(id);
        }
        expired.len()
    }
}
