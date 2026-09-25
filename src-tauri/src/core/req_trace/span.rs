//! the span API: begin/span/Span (RAII close), events, finish, truncation.
//! Mechanical move from core/req_trace.rs.

use super::*;

thread_local! {
    static CTX: RefCell<Option<Ctx>> = RefCell::new(None);
}

/// thread-local trace context. stack[0] is always the root span ("s0"); child spans are pushed,
/// and popped back to their own position on end (nesting imbalance defense: intermediate unclosed spans are truncated directly).
struct Ctx {
    path: PathBuf,
    stack: Vec<String>,
    next_span: u64,
}

/// Enter the trace context of one /rpc request (called at the /rpc thread entry in http.rs).
/// Writes the root span start (name "rpc") and returns the trace_id (also the file name).
/// `project` = the short path of the CLI's project (the project::short_path form); "" = unknown,
/// and the viewer groups it as ungrouped when grouping by project (traces persisted before this change are all "").
pub fn begin(agent_id: &str, conn_id: &str, project: &str) -> String {
    let trace_id = format!("rpc-{}", nanos());
    let path = trace_path(agent_id, &trace_id);
    write_line(
        &path,
        &RpcTraceLine::Start {
            span_id: "s0".into(),
            parent_id: None,
            name: "rpc".into(),
            ts: now_ms(),
            attrs: json!({ "agent": agent_id, "conn": conn_id, "project": project }),
        },
    );
    CTX.with(|c| {
        *c.borrow_mut() = Some(Ctx {
            path,
            stack: vec!["s0".into()],
            next_span: 1,
        });
    });
    trace_id
}

/// Open a child span (parent = stack top). No trace context → a no-op Span with an empty id.
pub fn span(name: &str, attrs: Value) -> Span {
    let id = CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(ctx) = b.as_mut() else {
            return String::new();
        };
        let id = format!("s{}", ctx.next_span);
        ctx.next_span += 1;
        let parent = ctx.stack.last().cloned();
        write_line(
            &ctx.path,
            &RpcTraceLine::Start {
                span_id: id.clone(),
                parent_id: parent,
                name: name.to_string(),
                ts: now_ms(),
                attrs,
            },
        );
        ctx.stack.push(id.clone());
        id
    });
    Span {
        id,
        _not_send: PhantomData,
    }
}

/// Explicitly managed span: end consumes ownership, mem::take clears the id so the Drop fallback no longer fires.
/// _not_send: CTX is thread-local, so a Span moved across threads would end on the wrong thread and silently
/// drop the line — use !Send to block it at compile time (a bare PhantomData pointer takes no space).
pub struct Span {
    id: String,
    _not_send: PhantomData<*mut ()>,
}

impl Span {
    pub fn end(mut self, ok: bool, error: Option<&str>, attrs: Value) {
        let id = std::mem::take(&mut self.id);
        end_span(&id, ok, error, attrs);
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // panic / early return fallback: a span not explicitly ended is recorded as error, not left dangling.
        if !self.id.is_empty() {
            end_span(&self.id, false, Some("dropped without end"), json!({}));
        }
    }
}

fn end_span(id: &str, ok: bool, error: Option<&str>, attrs: Value) {
    if id.is_empty() {
        return;
    }
    CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(ctx) = b.as_mut() else { return };
        if let Some(pos) = ctx.stack.iter().rposition(|s| s == id) {
            ctx.stack.truncate(pos);
        }
        write_line(
            &ctx.path,
            &RpcTraceLine::End {
                span_id: id.to_string(),
                ts: now_ms(),
                status: if ok {
                    SpanStatus::Ok
                } else {
                    SpanStatus::Error
                },
                error: error.map(String::from),
                attrs: if attrs.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                    None
                } else if attrs.is_null() {
                    None
                } else {
                    Some(attrs)
                },
            },
        );
    });
}

/// Add an event line to the top-of-stack span (an instantaneous step with no duration: permission decision, dispatch, ask result).
pub fn event(name: &str, attrs: Value) {
    CTX.with(|c| {
        let b = c.borrow();
        let Some(ctx) = b.as_ref() else { return };
        let Some(top) = ctx.stack.last() else { return };
        write_line(
            &ctx.path,
            &RpcTraceLine::Event {
                span_id: top.clone(),
                name: name.to_string(),
                ts: now_ms(),
                attrs,
            },
        );
    });
}

/// End the trace: all remaining unclosed spans are recorded as error (a pending chain is not left dangling), then write the root end and clear the context.
pub fn finish(ok: bool, error: Option<&str>, attrs: Value) {
    CTX.with(|c| {
        let mut b = c.borrow_mut();
        let Some(mut ctx) = b.take() else { return };
        while ctx.stack.len() > 1 {
            if let Some(id) = ctx.stack.pop() {
                write_line(
                    &ctx.path,
                    &RpcTraceLine::End {
                        span_id: id,
                        ts: now_ms(),
                        status: SpanStatus::Error,
                        error: Some("unfinished at trace end".into()),
                        attrs: None,
                    },
                );
            }
        }
        let root = ctx.stack[0].clone();
        write_line(
            &ctx.path,
            &RpcTraceLine::End {
                span_id: root,
                ts: now_ms(),
                status: if ok {
                    SpanStatus::Ok
                } else {
                    SpanStatus::Error
                },
                error: error.map(String::from),
                attrs: if attrs.as_object().map(|o| o.is_empty()).unwrap_or(false) {
                    None
                } else if attrs.is_null() {
                    None
                } else {
                    Some(attrs)
                },
            },
        );
    });
}

/// input preview truncation (byte cap, backing off to a char boundary).
pub fn trunc_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…[truncated]", &s[..cut])
}
