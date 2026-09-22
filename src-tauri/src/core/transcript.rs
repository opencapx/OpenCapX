//! Agent transcript tail reading.
//!
//! Claude Code / Droid hook payloads carry `transcript_path`, containing line-by-line
//! JSONL session records. We only care about the tail: the last assistant text (to judge
//! "task done" vs "asking a question"), the session title, the current model, and token usage.
//!
//! Only read a fixed window at the end of the file, never parse the whole thing — every hook event goes through here.

use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// Read window size: enough to cover "the last assistant message + one usage".
const TAIL_BYTES: u64 = 512 * 1024;
/// Cap on the retained last assistant text (enough to judge a question, and keeps the bubble short).
const MAX_TEXT: usize = 400;
const MAX_TITLE: usize = 80;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TranscriptTail {
    /// Plain text of the last assistant message (clipped to MAX_TEXT).
    pub latest_assistant_text: String,
    /// Model name used by the last assistant message (supports switching mid-session via `/model`).
    pub model: String,
    /// Session title: a summary event takes priority, otherwise the first user text in the window.
    pub title: String,
    /// The usage last reported by an assistant message.
    pub usage: Option<Usage>,
}

/// Get the transcript path from the hook payload (Claude / Droid's `transcript_path`).
pub fn path_from_payload(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let p = v
        .get("transcript_path")
        .or_else(|| v.get("transcriptPath"))
        .and_then(|x| x.as_str())?;
    if p.is_empty() {
        None
    } else {
        Some(p.to_string())
    }
}

/// Read the transcript tail. Returns None when the file is missing/unreadable (the caller skips silently).
pub fn read_tail(path: &str) -> Option<TranscriptTail> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let window = String::from_utf8_lossy(&buf);
    Some(parse_window(&window, start > 0))
}

/// Parse a JSONL window. `had_prefix` means the window starts mid-line, so the partial line must be dropped.
pub(crate) fn parse_window(text: &str, had_prefix: bool) -> TranscriptTail {
    let text = if had_prefix {
        match text.split_once('\n') {
            Some((_, rest)) => rest,
            None => "",
        }
    } else {
        text
    };
    let mut tail = TranscriptTail::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        apply_line(line, &mut tail);
    }
    tail
}

fn apply_line(line: &str, tail: &mut TranscriptTail) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return;
    };
    match v.get("type").and_then(|t| t.as_str()) {
        // Session summary (written by Claude when the session is compacted/named). The first one in the window is closest to the session start.
        Some("summary") => {
            if tail.title.is_empty() {
                if let Some(s) = v.get("summary").and_then(|x| x.as_str()) {
                    tail.title = clip_one_line(s, MAX_TITLE);
                }
            }
        }
        Some("user") => {
            // Only accept genuine human input: a tool_result user message has no text and is naturally skipped.
            if tail.title.is_empty() {
                let t = message_text(&v);
                if !t.is_empty() {
                    tail.title = clip_one_line(&t, MAX_TITLE);
                }
            }
        }
        Some("assistant") => {
            if let Some(m) = v.get("message") {
                let t = message_text(&v);
                if !t.is_empty() {
                    tail.latest_assistant_text = clip_text(&t, MAX_TEXT);
                }
                if let Some(md) = m.get("model").and_then(|x| x.as_str()) {
                    if !md.is_empty() {
                        tail.model = md.to_string();
                    }
                }
                if let Some(u) = m.get("usage") {
                    let usage = Usage {
                        input_tokens: num(u, "input_tokens"),
                        output_tokens: num(u, "output_tokens"),
                        cache_creation_tokens: num(u, "cache_creation_input_tokens"),
                        cache_read_tokens: num(u, "cache_read_input_tokens"),
                    };
                    if usage.total() > 0 {
                        tail.usage = Some(usage);
                    }
                }
            }
        }
        _ => {}
    }
}

/// `message.content` may be a string or a block array; only text blocks are taken.
fn message_text(v: &Value) -> String {
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.trim().to_string();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for b in arr {
        let is_text = b.get("type").and_then(|t| t.as_str()).map(|t| t == "text").unwrap_or(false);
        if !is_text {
            continue;
        }
        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t.trim());
        }
    }
    out.trim().to_string()
}

fn num(v: &Value, key: &str) -> u64 {
    v.get(key).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn clip_one_line(s: &str, max: usize) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .chars()
        .take(max)
        .collect()
}

fn clip_text(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        return t.to_string();
    }
    t.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpfile(tag: &str, body: &str) -> String {
        let p = std::env::temp_dir().join(format!("opencapx-tr-{}-{}", std::process::id(), tag));
        let mut f = File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p.to_string_lossy().into_owned()
    }

    const SAMPLE: &str = concat!(
        r#"{"type":"summary","summary":"Fix login redirect"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":"fix the login redirect"}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","content":[{"type":"text","text":"Let me look at the router."},{"type":"tool_use","name":"Read","input":{}}],"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":5}}}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
        "\n",
        r#"{"type":"assistant","message":{"model":"claude-sonnet-4-5","content":[{"type":"text","text":"Fixed it. Should I also add a test?"}],"usage":{"input_tokens":160,"output_tokens":40}}}"#,
        "\n",
    );

    #[test]
    fn reads_last_assistant_text_model_title_usage() {
        let path = tmpfile("basic", SAMPLE);
        let tail = read_tail(&path).expect("tail");
        assert_eq!(tail.latest_assistant_text, "Fixed it. Should I also add a test?");
        assert_eq!(tail.model, "claude-sonnet-4-5");
        assert_eq!(tail.title, "Fix login redirect");
        let usage = tail.usage.expect("usage");
        assert_eq!(usage.input_tokens, 160);
        assert_eq!(usage.output_tokens, 40);
        // The last one has no cache field → recorded as 0, not inherited from the previous line
        assert_eq!(usage.cache_read_tokens, 0);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn title_falls_back_to_first_user_text_when_no_summary() {
        let body = concat!(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"role":"user","content":"rename the pet window"}}"#,
            "\n",
        );
        let path = tmpfile("title", body);
        let tail = read_tail(&path).expect("tail");
        assert_eq!(tail.title, "rename the pet window");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn usage_zero_is_reported_as_none() {
        let body = r#"{"type":"assistant","message":{"model":"m","content":[{"type":"text","text":"hi"}]}}"#;
        let path = tmpfile("nousage", body);
        let tail = read_tail(&path).expect("tail");
        assert!(tail.usage.is_none());
        assert_eq!(tail.latest_assistant_text, "hi");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn parse_window_drops_partial_first_line() {
        let text = "alf line\n{\"type\":\"summary\",\"summary\":\"ok\"}\n";
        let tail = parse_window(text, true);
        assert_eq!(tail.title, "ok");
        // Without dropping the partial line, the later complete line still parses, just with one wasted parse
        let tail2 = parse_window(text, false);
        assert_eq!(tail2.title, "ok");
    }

    #[test]
    fn long_text_is_clipped() {
        let long = "x".repeat(500);
        let body = format!(
            r#"{{"type":"assistant","message":{{"model":"m","content":[{{"type":"text","text":"{long}"}}]}}}}"#
        );
        let path = tmpfile("clip", &body);
        let tail = read_tail(&path).expect("tail");
        assert_eq!(tail.latest_assistant_text.chars().count(), MAX_TEXT);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn missing_file_and_garbage_lines_are_ignored() {
        assert!(read_tail("/nonexistent/opencapx/transcript.jsonl").is_none());
        let path = tmpfile("garbage", "not json\n{\"type\":\"summary\",\"summary\":\"s\"}\n");
        let tail = read_tail(&path).expect("tail");
        assert_eq!(tail.title, "s");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn path_from_payload_reads_transcript_path() {
        assert_eq!(
            path_from_payload(r#"{"transcript_path":"/tmp/a.jsonl"}"#).as_deref(),
            Some("/tmp/a.jsonl")
        );
        assert_eq!(path_from_payload(r#"{"transcript_path":""}"#), None);
        assert_eq!(path_from_payload("not json"), None);
        assert_eq!(path_from_payload("{}"), None);
    }
}
