//! System notification copy.
//!
//! The notification body uses **sentence case** (not a menu; Title Case reads like a heading), and must follow
//! the user's language — the copy tables live in `core::i18n`. The agent name uses the display name (claude →
//! "Claude Code"), never throw the id at the user.

use crate::core::i18n::{fill, truncate, Strings};

/// Notification body. waiting: "<agent> needs input — <msg>", done: "<agent> finished".
/// Other states do not notify (returns an empty string). The message is truncated, avoiding stuffing a whole assistant recap into the notification.
pub fn notify_copy(s: &Strings, agent: &str, state: &str, msg: &str) -> String {
    let who = crate::hooks::display_name(agent);
    match state {
        "waiting" => fill(s.notify_waiting, &[&who, &truncate(msg, 80)]),
        "done" => fill(s.notify_done, &[&who]),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::i18n::{EN, ZH_HANS};

    #[test]
    fn notify_waiting_copy_mentions_input_and_product_name() {
        let s = notify_copy(&EN, "claude", "waiting", "Should I proceed?");
        assert!(s.contains("needs input"));
        assert!(
            s.contains("Claude Code"),
            "should use the display name, not the id: {s}"
        );
        assert!(s.contains("Should I proceed?"));
    }

    #[test]
    fn notify_done_copy_mentions_finished() {
        assert_eq!(notify_copy(&EN, "codex", "done", "x"), "Codex finished");
    }

    #[test]
    fn notify_copy_follows_the_language() {
        assert_eq!(
            notify_copy(&ZH_HANS, "gemini", "done", "x"),
            "Gemini CLI 已完成"
        );
        assert_eq!(
            notify_copy(&ZH_HANS, "gemini", "waiting", "确认一下?"),
            "Gemini CLI 需要你输入 — 确认一下?"
        );
    }

    #[test]
    fn notify_other_states_are_silent() {
        assert_eq!(notify_copy(&EN, "claude", "working", "x"), "");
        assert_eq!(notify_copy(&EN, "claude", "idle", "x"), "");
    }

    #[test]
    fn notify_waiting_body_is_truncated() {
        let long = "x".repeat(300);
        let s = notify_copy(&EN, "claude", "waiting", &long);
        assert!(
            s.chars().count() < 120,
            "the notification body should not be a whole recap: {}",
            s.len()
        );
        assert!(s.contains('…'));
    }
}
