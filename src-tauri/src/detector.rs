use crate::core::agent::AgentState;

const QUESTION_HINTS: [&str; 6] = [
    "?",
    "should i",
    "confirm",
    "approve",
    "waiting for",
    "need input",
];

/// Used on short text (an explicitly passed text / `--event`): a keyword hit is treated as waiting.
/// Note it uses substring matching and false-positives on long recaps (see `looks_like_question`).
pub fn detect_claude_stop(transcript_text: &str) -> AgentState {
    let t = transcript_text.to_lowercase();
    if QUESTION_HINTS.iter().any(|h| t.contains(h)) {
        AgentState::Waiting
    } else {
        AgentState::Done
    }
}

/// Closing pleasantries: their presence means it is not asking a question, just a polite sign-off.
const OPTIONAL_FOLLOW_UPS: [&str; 8] = [
    "let me know if",
    "let me know when",
    "feel free to",
    "if you'd like",
    "if you want",
    "say the word",
    "happy to help",
    "just let me know",
];

/// Question starters (another way of asking besides ending with a question mark).
const QUESTION_STARTERS: [&str; 11] = [
    "which ",
    "what ",
    "how ",
    "should i",
    "do you",
    "want me to",
    "shall i",
    "would you",
    "can you",
    "could you",
    "are you ",
];

/// Whether a long assistant text is "asking the user something".
///
/// Claude's Stop hook fires whether the "task is done" or it "stopped to ask"; the event name alone
/// cannot tell them apart. Here we look only at the **last sentence**: first rule out a polite sign-off, then check whether it is a question.
/// The rule is stricter than `detect_claude_stop`'s substring matching, purpose-built for the transcript summary.
pub fn looks_like_question(text: &str) -> bool {
    let last = last_sentence(text);
    if last.is_empty() {
        return false;
    }
    let l = last.to_lowercase();
    if OPTIONAL_FOLLOW_UPS.iter().any(|p| l.contains(p)) {
        return false;
    }
    let trimmed = l.trim();
    trimmed.ends_with('?') || QUESTION_STARTERS.iter().any(|s| trimmed.starts_with(s))
}

/// Take the last sentence (split on . ! ?, keeping the terminating punctuation); newlines count as spaces.
fn last_sentence(text: &str) -> String {
    let flat = text.replace(['\n', '\r'], " ");
    let mut last = String::new();
    let mut cur = String::new();
    for ch in flat.chars() {
        cur.push(ch);
        if ch == '.' || ch == '!' || ch == '?' {
            let t = cur.trim();
            if !t.is_empty() {
                last = t.to_string();
            }
            cur.clear();
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        last = t.to_string();
    }
    last
}

pub fn title_from_transcript(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("untitled");
    line.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn question_mark_counts_as_waiting() {
        assert_eq!(detect_claude_stop("Should I proceed?"), AgentState::Waiting);
    }

    #[test]
    fn confirm_hint_counts_as_waiting() {
        assert_eq!(
            detect_claude_stop("Please confirm to continue"),
            AgentState::Waiting
        );
    }

    #[test]
    fn plain_summary_is_done() {
        assert_eq!(
            detect_claude_stop("Refactored auth module."),
            AgentState::Done
        );
    }

    #[test]
    fn empty_transcript_is_done() {
        assert_eq!(detect_claude_stop(""), AgentState::Done);
    }

    #[test]
    fn title_uses_first_line_capped() {
        assert_eq!(title_from_transcript("\nFix login\nmore"), "Fix login");
    }

    #[test]
    fn title_falls_back_to_untitled() {
        assert_eq!(title_from_transcript("\n   \n"), "untitled");
    }

    #[test]
    fn title_caps_at_80_chars() {
        let long = "x".repeat(200);
        assert_eq!(title_from_transcript(&long).chars().count(), 80);
    }

    #[test]
    fn question_mark_at_the_end_is_a_question() {
        assert!(looks_like_question("Fixed it. Should I also add a test?"));
        assert!(looks_like_question("Which approach do you prefer?"));
    }

    #[test]
    fn question_starter_without_mark_is_a_question() {
        assert!(looks_like_question(
            "I can either refactor the parser or patch it.\nWant me to do the refactor"
        ));
        assert!(looks_like_question("Should I proceed"));
    }

    #[test]
    fn polite_follow_up_is_not_a_question() {
        // A typical "done sign-off": the last sentence is a pleasantry and must not be judged as waiting
        assert!(!looks_like_question(
            "Refactored the auth module and tests pass. Let me know if you need anything else."
        ));
        assert!(!looks_like_question("All green. Feel free to review."));
    }

    #[test]
    fn plain_summary_is_not_a_question() {
        assert!(!looks_like_question("Refactored auth module."));
        assert!(!looks_like_question(""));
    }

    #[test]
    fn only_the_last_sentence_counts() {
        // The earlier question has passed and the last sentence is a statement → not waiting
        assert!(!looks_like_question("What about tests? They pass now."));
    }
}
