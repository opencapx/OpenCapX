//! the per-endpoint template engine, lint, dry-run preview, and the JSON path lookup helpers.
//! Mechanical move from core/alerting.rs.

use super::*;

/// Phase 57 — Template render output type: determines the Content-Type on POST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateContentType {
    Json,
    Text,
}

impl TemplateContentType {
    pub fn as_content_type(self) -> &'static str {
        match self {
            TemplateContentType::Json => "application/json",
            TemplateContentType::Text => "text/plain; charset=utf-8",
        }
    }
}

/// Phase 57 — Given an endpoint's template + envelope, produce (rendered_body, content_type).
/// Supported syntax:
///   - `{{path}}` — top-level fields (source / severity / timestamp / event_id / schema_version / tags)
///   - `{{payload.x.y.z}}` — dot-path JSON field access (numbers / bool / null go through JSON serialization)
///   - `{{#if EXPR}}BODY{{/if}}` — conditional block
///       EXPR forms:
///         `severity == "critical"` / `payload.code == 500` — equality comparison (supports string / numeric literals)
///         `payload.x` / `severity` — truthiness check (non-empty string / non-zero number / true / non-empty array / non-empty object)
///         `!EXPR` — negation
///   - `{{#if EXPR}}A{{else}}B{{/if}}` — if / else
///   - `{{#each path}}BODY{{/each}}` — iterate an array; inside the block `{{this}}` yields the element and `{{this.x}}` a field
///
/// Failure: an unpaired `{{#if}}` / `{{#each}}` returns Err (with the template's line:col).
pub fn render_template(
    template: &str,
    env: &AlertEnvelope,
) -> Result<(String, TemplateContentType), String> {
    let mut out = String::with_capacity(template.len());
    render_into(template, env, &mut out, 0, 0)?;
    let trimmed = out.trim_start();
    let ct = if trimmed.starts_with('{') || trimmed.starts_with('[') {
        TemplateContentType::Json
    } else {
        TemplateContentType::Text
    };
    Ok((out, ct))
}

/// Internal recursive render: `template` starts at `pos`, until the end or a matching `end_token` (`end_token = 0` means no external end condition).
/// Block end tokens: `{{/if}}` and `{{/each}}`.
fn render_into(
    template: &str,
    env: &AlertEnvelope,
    out: &mut String,
    pos: usize,
    end_token: u8,
) -> Result<usize, String> {
    let bytes = template.as_bytes();
    let mut i = pos;
    while i < bytes.len() {
        // match a block end
        if bytes[i..].starts_with(b"{{/if}}") {
            if end_token == 1 {
                return Ok(i + 7);
            }
            return Err(format!(
                "template: unexpected {{/if}} at offset {} (no matching {{#if}})",
                i
            ));
        }
        if bytes[i..].starts_with(b"{{/each}}") {
            if end_token == 2 {
                return Ok(i + 9);
            }
            return Err(format!(
                "template: unexpected {{/each}} at offset {} (no matching {{#each}})",
                i
            ));
        }
        // match {{ ... }}
        if bytes[i..].starts_with(b"{{") {
            // find }}
            let close = match template[i + 2..].find("}}") {
                Some(c) => i + 2 + c,
                None => return Err(format!("template: unclosed placeholder at offset {}", i)),
            };
            let expr = template[i + 2..close].trim();
            if let Some(rest) = expr.strip_prefix("#if ") {
                // {{#if EXPR}}BODY{{/if}} / {{#if EXPR}}A{{else}}B{{/if}}
                let cond_expr = rest.trim();
                // find the BODY start (the first {{/if}} or {{else}} after close + 2)
                let body_start = close + 2;
                // first render one segment with sentinel token 1 ({{/if}}), capturing the rest
                let mut scratch = String::new();
                let after_if = render_into(template, env, &mut scratch, body_start, 1)?;
                // after_if points past the end of {{/if}}. Check whether the if-body contains {{else}}
                let if_body_str = &template[body_start..after_if - 7];
                if let Some(else_idx) = if_body_str.rfind("{{else}}") {
                    let else_body_start_in_main = body_start + else_idx + 8;
                    let mut else_scratch = String::new();
                    let _ =
                        render_into(template, env, &mut else_scratch, else_body_start_in_main, 1)?;
                    let cond = eval_condition(cond_expr, env)?;
                    if cond {
                        out.push_str(&if_body_str[..else_idx]);
                    } else {
                        out.push_str(&else_scratch);
                    }
                } else {
                    let cond = eval_condition(cond_expr, env)?;
                    if cond {
                        out.push_str(&scratch);
                    }
                }
                i = after_if; // after_if already points past {{/if}}
                continue;
            }
            if let Some(rest) = expr.strip_prefix("#each ") {
                let path = rest.trim();
                // find the BODY start + grab the raw range directly (use a sentinel to locate {{/each}})
                let body_start = close + 2;
                let end_rel = match template[body_start..].find("{{/each}}") {
                    Some(p) => p,
                    None => return Err("template: unclosed {{#each}}".into()),
                };
                let body_raw = &template[body_start..body_start + end_rel];
                let after_each = body_start + end_rel + 9; // skip past {{/each}}
                                                           // fetch the array
                let arr = lookup_path_array(env, path).unwrap_or_default();
                for item in arr {
                    // inside the block, resolve via `{{this}}` / `{{this.x}}`
                    render_each_body(body_raw, &item, env, out)?;
                }
                i = after_each;
                continue;
            }
            // plain placeholder {{path}}
            let val = lookup_path_string(env, expr);
            out.push_str(&val);
            i = close + 2;
            continue;
        }
        // ordinary character
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if end_token != 0 {
        return Err(format!(
            "template: unclosed block (expected {{/{}}})",
            match end_token {
                1 => "if",
                2 => "each",
                _ => "?",
            }
        ));
    }
    Ok(i)
}

/// Render a `{{#each}}` block body, mapping `{{this}}` / `{{this.x}}` to the current item.
fn render_each_body(
    body: &str,
    item: &serde_json::Value,
    env: &AlertEnvelope,
    out: &mut String,
) -> Result<(), String> {
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"{{this") {
            // {{this}} or {{this.x.y}}
            let close = match body[i + 2..].find("}}") {
                Some(c) => i + 2 + c,
                None => return Err(format!("unclosed {{this... at {}", i)),
            };
            let expr = body[i + 2..close].trim(); // "this" or "this.x.y"
            let val = if expr == "this" {
                value_to_string(item)
            } else if let Some(rest) = expr.strip_prefix("this.") {
                let v = lookup_json_path(item, rest).unwrap_or(&serde_json::Value::Null);
                value_to_string(v)
            } else {
                // fallback: use envelope lookup (allows referencing env inside an each block too)
                lookup_path_string(env, expr)
            };
            out.push_str(&val);
            i = close + 2;
            continue;
        }
        if bytes[i..].starts_with(b"{{") {
            // plain placeholders such as {{source}} are allowed inside the block (they use env)
            let close = match body[i + 2..].find("}}") {
                Some(c) => i + 2 + c,
                None => return Err(format!("unclosed placeholder at {}", i)),
            };
            let expr = body[i + 2..close].trim();
            // nested #if / #each inside an each block is forbidden (simplified implementation)
            if expr.starts_with('#') {
                return Err(format!("nested block not supported in each: {}", expr));
            }
            let val = lookup_path_string(env, expr);
            out.push_str(&val);
            i = close + 2;
            continue;
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(())
}

/// Parse a condition expression: `a == "b"` / `a == 42` / `a` / `!a`.
fn eval_condition(expr: &str, env: &AlertEnvelope) -> Result<bool, String> {
    let expr = expr.trim();
    if let Some(rest) = expr.strip_prefix('!') {
        return Ok(!eval_condition(rest.trim(), env)?);
    }
    if let Some(idx) = expr.find("==") {
        let left = expr[..idx].trim();
        let right = expr[idx + 2..].trim();
        let lv = lookup_path_value(env, left);
        let rv = parse_literal(right);
        return Ok(values_equal(&lv, &rv));
    }
    // truthiness check
    let v = lookup_path_value(env, expr);
    Ok(is_truthy(&v))
}

fn parse_literal(s: &str) -> serde_json::Value {
    if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
        return serde_json::Value::String(s[1..s.len() - 1].to_string());
    }
    if let Ok(n) = s.parse::<i64>() {
        return serde_json::Value::Number(n.into());
    }
    if let Ok(f) = s.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return serde_json::Value::Number(n);
        }
    }
    if s == "true" {
        return serde_json::Value::Bool(true);
    }
    if s == "false" {
        return serde_json::Value::Bool(false);
    }
    if s == "null" {
        return serde_json::Value::Null;
    }
    // fall back to treating it as a string literal
    serde_json::Value::String(s.to_string())
}

fn values_equal(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    a == b
}

fn is_truthy(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

/// Phase 58 — Static lint diagnostic info, exposed to the frontend live preview UI.
/// `severity` uses literal strings (`"error"` / `"warning"`) aligned with serde + the TS DTO;
/// `code` is a stable machine-readable category (`unexpected_close` / `unclosed_block` /
/// `nested_too_deep` / `unclosed_placeholder` / `empty_tag` / `unknown_tag` /
/// `mismatched_close` / `render_failed`)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateDiagnostic {
    pub severity: &'static str,
    pub code: String,
    pub message: String,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

/// Phase 58 — dry-run render result + lint diagnostics.
/// `body` is the rendered string (empty on failure); `content_type` is the auto-sniffed
/// (`"application/json"` or `"text/plain; charset=utf-8"`); `diagnostics`
/// is the merged list of lint + render failure info (sorted by offset).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePreviewResult {
    pub body: String,
    pub content_type: String,
    pub diagnostics: Vec<TemplateDiagnostic>,
}

/// Convert a byte offset in the template to (1-indexed line, 1-indexed column).
/// column is the user-visible column (UTF-8 chars count as 1, not bytes).
pub fn offset_to_line_col(template: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < offset && i < bytes.len() {
        // UTF-8 chars follow char boundaries
        if let Some(ch) = template[i..].chars().next() {
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    (line, col)
}

fn diag_at(
    template: &str,
    offset: usize,
    severity: &'static str,
    code: &str,
    message: &str,
) -> TemplateDiagnostic {
    let (line, column) = offset_to_line_col(template, offset);
    TemplateDiagnostic {
        severity,
        code: code.to_string(),
        message: message.to_string(),
        offset,
        line,
        column,
    }
}

/// Static lint: does not evaluate the template, only checks structure (nesting / pairing / syntax).
/// Nesting depth ≥ 9 → error; unmatched `{{/if}}` / `{{/each}}` → error;
/// unclosed `{{#if}}` / `{{#each}}` → error; unclosed `{{` → error;
/// empty placeholder / suspicious identifier syntax → warning.
pub fn lint_template(template: &str) -> Vec<TemplateDiagnostic> {
    let mut out = Vec::new();
    let mut stack: Vec<(&'static str, usize)> = Vec::new(); // (kind, open_offset)
    let mut max_depth: usize = 0;
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"{{") {
            // find }}
            let close_rel = template[i + 2..].find("}}");
            let Some(close_rel) = close_rel else {
                out.push(diag_at(
                    template,
                    i,
                    "error",
                    "unclosed_placeholder",
                    "unclosed {{ placeholder (missing }})",
                ));
                break;
            };
            let close = i + 2 + close_rel;
            let expr = template[i + 2..close].trim();
            if let Some(rest) = expr.strip_prefix("#if ") {
                if stack.len() + 1 >= 9 {
                    out.push(diag_at(
                        template,
                        i,
                        "error",
                        "nested_too_deep",
                        "block nesting too deep (max 8)",
                    ));
                }
                max_depth = max_depth.max(stack.len() + 1);
                stack.push(("if", i));
            } else if let Some(_rest) = expr.strip_prefix("#each ") {
                if stack.len() + 1 >= 9 {
                    out.push(diag_at(
                        template,
                        i,
                        "error",
                        "nested_too_deep",
                        "block nesting too deep (max 8)",
                    ));
                }
                max_depth = max_depth.max(stack.len() + 1);
                stack.push(("each", i));
            } else if expr == "/if" {
                match stack.pop() {
                    Some(("if", _)) => {}
                    Some(("each", off)) => {
                        out.push(diag_at(
                            template,
                            i,
                            "error",
                            "mismatched_close",
                            &format!("{{{{/if}}}} closes {{{{}}}} at offset {}", off),
                        ));
                    }
                    None => {
                        out.push(diag_at(
                            template,
                            i,
                            "error",
                            "unexpected_close",
                            "unexpected {{/if}} with no matching {{#if}}",
                        ));
                    }
                    _ => unreachable!(),
                }
            } else if expr == "/each" {
                match stack.pop() {
                    Some(("each", _)) => {}
                    Some(("if", off)) => {
                        out.push(diag_at(
                            template,
                            i,
                            "error",
                            "mismatched_close",
                            &format!("{{{{/each}}}} closes {{{{}}}} at offset {}", off),
                        ));
                    }
                    None => {
                        out.push(diag_at(
                            template,
                            i,
                            "error",
                            "unexpected_close",
                            "unexpected {{/each}} with no matching {{#each}}",
                        ));
                    }
                    _ => unreachable!(),
                }
            } else if expr.is_empty() {
                out.push(diag_at(
                    template,
                    i,
                    "warning",
                    "empty_tag",
                    "empty placeholder",
                ));
            } else if expr.contains(' ') && !expr.contains("==") && !expr.starts_with('#') {
                out.push(diag_at(
                    template,
                    i,
                    "warning",
                    "unknown_tag",
                    &format!("unexpected placeholder syntax: {}", expr),
                ));
            }
            i = close + 2;
            continue;
        }
        // ordinary char (UTF-8 aware)
        let ch_len = template[i..]
            .chars()
            .next()
            .map(|c| c.len_utf8())
            .unwrap_or(1);
        i += ch_len;
    }
    // stack not empty → unclosed block
    for (kind, off) in &stack {
        out.push(diag_at(
            template,
            *off,
            "error",
            "unclosed_block",
            &format!("unclosed {{{{#{}}}}} block", kind),
        ));
    }
    let _ = max_depth; // reserved for a future warning (range 5-8); an error is already enough to alert
    out
}

/// Phase 58 — Parse the `at offset N` at the tail of a `render_template` error message.
/// Used to upgrade a render failure into a diagnostic carrying line/col.
fn parse_error_offset(msg: &str) -> Option<usize> {
    let idx = msg.rfind("at offset ")?;
    let rest = &msg[idx + "at offset ".len()..];
    rest.split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|s| s.parse::<usize>().ok())
}

/// Phase 58 — dry-run preview: use the sample JSON passed by the frontend as envelope.payload,
/// run render_template + lint_template, and synthesize a TemplatePreviewResult.
/// On render failure it does not return Err, but merges diagnostics + body="" (easier for the frontend).
pub fn preview_alerting_template(
    template: &str,
    sample: &serde_json::Value,
) -> TemplatePreviewResult {
    let envelope = AlertEnvelope {
        schema_version: ALERT_SCHEMA_VERSION,
        event_id: "preview".into(),
        source: "preview".into(),
        severity: Severity::Info,
        tags: vec![],
        timestamp: 0,
        payload: sample.clone(),
    };
    let mut diags = lint_template(template);
    match render_template(template, &envelope) {
        Ok((body, ct)) => TemplatePreviewResult {
            body,
            content_type: ct.as_content_type().to_string(),
            diagnostics: diags,
        },
        Err(e) => {
            if let Some(off) = parse_error_offset(&e) {
                let (line, col) = offset_to_line_col(template, off);
                diags.push(TemplateDiagnostic {
                    severity: "error",
                    code: "render_failed".into(),
                    message: e,
                    offset: off,
                    line,
                    column: col,
                });
            } else {
                diags.push(TemplateDiagnostic {
                    severity: "error",
                    code: "render_failed".into(),
                    message: e,
                    offset: 0,
                    line: 1,
                    column: 1,
                });
            }
            TemplatePreviewResult {
                body: String::new(),
                content_type: "text/plain".to_string(),
                diagnostics: diags,
            }
        }
    }
}

/// Look up path on the envelope and return the JSON value (supports `payload.x.y`).
fn lookup_path_value<'a>(env: &'a AlertEnvelope, path: &str) -> serde_json::Value {
    let path = path.trim();
    if let Some(rest) = path.strip_prefix("payload.") {
        return lookup_json_path(&env.payload, rest)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
    }
    match path {
        "source" => serde_json::Value::String(env.source.clone()),
        "severity" => serde_json::Value::String(env.severity.as_str().to_string()),
        "timestamp" => serde_json::Value::Number(env.timestamp.into()),
        "event_id" => serde_json::Value::String(env.event_id.clone()),
        "schema_version" => serde_json::Value::Number(env.schema_version.into()),
        "tags" => serde_json::Value::Array(
            env.tags
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
        _ => serde_json::Value::Null,
    }
}

/// Stringified version used when rendering (numbers / bool go through serde_json serialization).
fn lookup_path_string(env: &AlertEnvelope, path: &str) -> String {
    value_to_string(&lookup_path_value(env, path))
}

/// Look up a dot-path on any `serde_json::Value`.
fn lookup_json_path<'a>(v: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut cur = v;
    for seg in path.split('.') {
        match cur {
            serde_json::Value::Object(m) => {
                cur = m.get(seg)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

/// Used by `{{#each path}}`: returns an array; non-arrays yield an empty array.
fn lookup_path_array(env: &AlertEnvelope, path: &str) -> Option<Vec<serde_json::Value>> {
    match lookup_path_value(env, path) {
        serde_json::Value::Array(a) => Some(a),
        _ => None,
    }
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}
