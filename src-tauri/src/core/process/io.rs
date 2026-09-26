//! stdio plumbing: capped line reading, raw writes, log rotation, stderr publication.
//! Mechanical move from core/process.rs.

use super::*;

pub(crate) fn write_raw(stdin: &Arc<Mutex<ChildStdin>>, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string(v).map_err(|e| e.to_string())?;
    s.push('\n');
    let mut w = stdin.lock().map_err(|_| "stdin poisoned".to_string())?;
    w.write_all(s.as_bytes()).map_err(|e| e.to_string())
}

/// review F4 — trigger a log-rotation check every N stderr lines: a 24h interval is too coarse for chatty plugins,
/// which can write well over 10MB in a day. The counter is process-global; one stat per 4096 lines is negligible.
pub(crate) static LOG_LINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(crate) const LOG_ROTATE_EVERY: u64 = 4096;

/// P3 — plugin→core single-line cap 4 MiB: a runaway plugin must not OOM the core with one line
/// (the S4 input gate only closed the opposite direction; trace persistence benefits at the same point).
pub(crate) const MAX_PLUGIN_LINE_BYTES: usize = 4 * 1024 * 1024;

pub(crate) static DROPPED_FRAMES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LineOutcome {
    Line(Vec<u8>),
    /// Line over the limit: the rest of that line's bytes have been consumed; the caller counts it and continues to the next line.
    Dropped,
    Eof,
}

/// P3 — bounded line read: a whole line exceeding `cap` (excluding the newline) is dropped, and the stream advances to the next start.
/// Line outcome with CRLF normalized: a plugin's stderr on Windows ends every line with
/// `\r\n`, and the `\r` must not ride along into plugin.log events and log files.
fn finalize_line(mut buf: Vec<u8>) -> LineOutcome {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    LineOutcome::Line(buf)
}

pub(crate) fn read_line_capped<R: std::io::BufRead>(r: &mut R, cap: usize) -> LineOutcome {
    let mut buf: Vec<u8> = Vec::new();
    let mut over = false;
    loop {
        let n = match r.fill_buf() {
            Ok(chunk) if chunk.is_empty() => {
                return match (over, buf.is_empty()) {
                    (true, _) => LineOutcome::Dropped,
                    (false, true) => LineOutcome::Eof,
                    (false, false) => finalize_line(buf),
                };
            }
            Ok(chunk) => {
                if let Some(pos) = chunk.iter().position(|b| *b == b'\n') {
                    if !over {
                        if buf.len() + pos > cap {
                            over = true;
                        } else {
                            buf.extend_from_slice(&chunk[..pos]);
                        }
                    }
                    r.consume(pos + 1);
                    return if over {
                        LineOutcome::Dropped
                    } else {
                        finalize_line(buf)
                    };
                }
                if over {
                    chunk.len()
                } else if buf.len() + chunk.len() > cap {
                    over = true;
                    buf.clear();
                    chunk.len()
                } else {
                    let m = chunk.len();
                    buf.extend_from_slice(chunk);
                    m
                }
            }
            Err(_) => return LineOutcome::Eof,
        };
        r.consume(n);
    }
}

pub(crate) fn log_line(plugin_id: &str, line: &str) {
    let dir = crate::core::retention::logs_root();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{}.log", plugin_id)))
    {
        let _ = writeln!(f, "{}", line);
    }
    if LOG_LINES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % LOG_ROTATE_EVERY == 0 {
        let _ = crate::core::retention::rotate_logs();
    }
}

/// Push one stderr line to the EventBus (consumed live by /events SSE, /admin, and settings audit).
/// The same line is also written to ~/.opencapx/logs/plugins/<id>.log (already done by log_line).
pub(crate) fn publish_stderr_log(plugin_id: &str, line: &str) {
    use serde_json::json;
    crate::core::event::EventBus::shared().publish(&crate::core::event::OpencapxEvent::new(
        "plugin.log",
        &format!("plugin:{}", plugin_id),
        json!({ "pluginId": plugin_id, "level": "info", "source": "stderr", "message": line }),
    ));
}
