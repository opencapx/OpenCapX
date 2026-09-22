//! Per-key throttle.
//!
//! Purpose: some work that is "wanted on every event but expensive" (reading the transcript to fill in the model name, etc.) needs
//! rate limiting. The key is usually a session id; within `interval_secs` it passes only once.
//! Deliberately does not read the clock — time is passed in by the caller, which eases testing.

use std::collections::HashMap;
use std::sync::Mutex;

pub struct PerKeyThrottle {
    interval_secs: u64,
    last: Mutex<HashMap<String, u64>>,
}

impl PerKeyThrottle {
    pub fn new(interval_secs: u64) -> Self {
        Self {
            interval_secs,
            last: Mutex::new(HashMap::new()),
        }
    }

    /// Returns true and records the time when it passes; a repeat call within the window returns false.
    /// On a poisoned lock it chooses to pass (better to do it once more than to fail silently).
    pub fn should_run(&self, key: &str, now: u64) -> bool {
        let Ok(mut m) = self.last.lock() else {
            return true;
        };
        match m.get(key) {
            Some(t) if now.saturating_sub(*t) < self.interval_secs => false,
            _ => {
                m.insert(key.to_string(), now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_first_then_blocks_within_window() {
        let t = PerKeyThrottle::new(30);
        assert!(t.should_run("s1", 1000));
        assert!(!t.should_run("s1", 1010));
        assert!(!t.should_run("s1", 1029));
        assert!(t.should_run("s1", 1030));
    }

    #[test]
    fn keys_are_independent() {
        let t = PerKeyThrottle::new(30);
        assert!(t.should_run("a", 100));
        assert!(t.should_run("b", 100));
        assert!(!t.should_run("a", 110));
    }

    #[test]
    fn backwards_clock_blocks_conservatively() {
        let t = PerKeyThrottle::new(30);
        assert!(t.should_run("s", 500));
        // Clock going backwards: the delta saturates to 0 → still treated as "within the window" and kept blocked.
        // Better to skip once than to repeatedly do expensive work while the clock jitters.
        assert!(!t.should_run("s", 100));
        assert!(t.should_run("s", 600));
    }
}
