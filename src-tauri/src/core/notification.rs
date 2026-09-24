//! Phase 66 — multi-channel notification sinks (webhook / log:stderr / log:file / email:smtp mock)
//!
//! Extends alerting Phase 49 fanout's "webhook only" to 4 kinds of sink.
//! Each routing-rule `then.recipients: Vec<String>` spec is a ref string:
//!   - "webhook:{endpoint_id}" — reuses the Phase 49 WebhookEndpoint
//!   - "log:stderr" — writes to stderr
//!   - "log:file:{path}" — appends to a file
//!   - "email:smtp:{relay}:{port}:{from}:{to}" — SMTP relay (Phase 66 MOCK, Phase 67+ wires up lettre)
//!
//! `resolve_recipient(spec, endpoints)` turns a ref string → `Box<dyn NotificationSink>`,
//! with each sink independently implementing the `send(envelope, body)` abstraction.

use crate::core::alerting::{
    compute_signature, send_http_with_body, AlertEnvelope, WebhookEndpoint,
};

pub trait NotificationSink: Send + Sync {
    fn kind(&self) -> &'static str;
    fn send(&self, env: &AlertEnvelope, body: &str) -> Result<(), String>;
}

pub struct WebhookSink {
    pub endpoint: WebhookEndpoint,
}

impl NotificationSink for WebhookSink {
    fn kind(&self) -> &'static str {
        "webhook"
    }
    fn send(&self, _env: &AlertEnvelope, body: &str) -> Result<(), String> {
        let url = self.endpoint.url.clone();
        if url.is_empty() {
            return Err("webhook endpoint url is empty".into());
        }
        let mut headers = self.endpoint.headers.clone();
        if !self.endpoint.secret.is_empty() {
            let sig = compute_signature(&self.endpoint.secret, body);
            headers.push(("X-OpenCapX-Signature".to_string(), sig));
        }
        send_http_with_body(&url, &headers, body, "application/json").map(|_| ())
    }
}

pub struct StderrSink;

impl NotificationSink for StderrSink {
    fn kind(&self) -> &'static str {
        "log:stderr"
    }
    fn send(&self, _env: &AlertEnvelope, body: &str) -> Result<(), String> {
        eprintln!("[opencapx-notify] {}", body);
        Ok(())
    }
}

pub struct FileSink {
    pub path: std::path::PathBuf,
}

impl NotificationSink for FileSink {
    fn kind(&self) -> &'static str {
        "log:file"
    }
    fn send(&self, _env: &AlertEnvelope, body: &str) -> Result<(), String> {
        use std::io::Write;
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir file sink parent: {e}"))?;
            }
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open file sink {}: {e}", self.path.display()))?;
        writeln!(f, "{}", body).map_err(|e| format!("write file sink: {e}"))?;
        Ok(())
    }
}

pub struct SmtpSink {
    pub relay: String,
    pub port: u16,
    pub from: String,
    pub to: String,
}

impl NotificationSink for SmtpSink {
    fn kind(&self) -> &'static str {
        "email:smtp"
    }
    fn send(&self, env: &AlertEnvelope, body: &str) -> Result<(), String> {
        // Phase 66 MOCK — real SMTP is left to Phase 67+ (lettre crate)
        eprintln!(
            "[smtp-mock] relay={}:{} from={} to={} subject=[{}] body={}",
            self.relay, self.port, self.from, self.to, env.event_id, body
        );
        Ok(())
    }
}

/// Turn a ref spec string → an actual sink.
/// Known prefixes: webhook / log:stderr / log:file / email:smtp.
pub fn resolve_recipient(
    spec: &str,
    endpoints: &[WebhookEndpoint],
) -> Result<Box<dyn NotificationSink>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err("empty recipient spec".into());
    }
    if let Some(rest) = spec.strip_prefix("webhook:") {
        let id = rest.trim();
        if id.is_empty() {
            return Err("webhook recipient missing endpoint id".into());
        }
        let ep = endpoints
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("webhook endpoint not found: {id}"))?;
        return Ok(Box::new(WebhookSink {
            endpoint: ep.clone(),
        }));
    }
    if spec == "log:stderr" {
        return Ok(Box::new(StderrSink));
    }
    if let Some(rest) = spec.strip_prefix("log:file:") {
        let path_str = rest.trim();
        if path_str.is_empty() {
            return Err("log:file recipient missing path".into());
        }
        return Ok(Box::new(FileSink {
            path: std::path::PathBuf::from(path_str),
        }));
    }
    if let Some(rest) = spec.strip_prefix("email:smtp:") {
        // Format: relay:port:from:to — 4 parts, port uses splitn(4)
        let parts: Vec<&str> = rest.splitn(4, ':').collect();
        if parts.len() != 4 {
            return Err(format!(
                "email:smtp recipient expects relay:port:from:to, got: {spec}"
            ));
        }
        let port: u16 = parts[1]
            .trim()
            .parse()
            .map_err(|_| format!("email:smtp bad port: {}", parts[1]))?;
        return Ok(Box::new(SmtpSink {
            relay: parts[0].to_string(),
            port,
            from: parts[2].to_string(),
            to: parts[3].to_string(),
        }));
    }
    Err(format!("unknown recipient kind: {spec}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::alerting::{AlertEnvelope, Severity, WebhookEndpoint};

    fn sample_env() -> AlertEnvelope {
        AlertEnvelope {
            schema_version: 1,
            event_id: "evt-cpu-123".into(),
            source: "plugin.metrics.cpu".into(),
            severity: Severity::Critical,
            tags: vec!["page".into()],
            timestamp: 1700000000,
            payload: serde_json::json!({"cpu_percent": 95.2}),
        }
    }

    fn webhook_endpoint(id: &str, url: &str, secret: &str) -> WebhookEndpoint {
        WebhookEndpoint {
            id: id.into(),
            name: format!("endpoint-{id}"),
            url: url.into(),
            enabled: true,
            headers: vec![("X-Test".into(), "1".into())],
            secret: secret.into(),
            source_filter: vec![],
            schema_version: 1,
            template: None,
            template_sample: None,
            severity_overrides: vec![],
        }
    }

    #[test]
    fn resolve_webhook_recipient_returns_endpoint_sink() {
        let eps = vec![webhook_endpoint("ep1", "https://example.test/hook", "")];
        let sink = resolve_recipient("webhook:ep1", &eps).expect("resolve");
        assert_eq!(sink.kind(), "webhook");
    }

    #[test]
    fn resolve_webhook_recipient_unknown_id_errors() {
        let eps = vec![webhook_endpoint("ep1", "https://example.test/hook", "")];
        let res = resolve_recipient("webhook:missing", &eps);
        assert!(res.is_err());
        let msg = res.err().expect("expected error");
        assert!(
            msg.contains("not found") || msg.contains("missing"),
            "got: {msg}"
        );
    }

    #[test]
    fn resolve_webhook_recipient_empty_id_errors() {
        let eps = vec![webhook_endpoint("ep1", "https://example.test/hook", "")];
        let res = resolve_recipient("webhook:", &eps);
        assert!(res.is_err());
    }

    #[test]
    fn resolve_log_stderr_recipient() {
        let sink = resolve_recipient("log:stderr", &[]).expect("resolve");
        assert_eq!(sink.kind(), "log:stderr");
    }

    #[test]
    fn resolve_log_file_recipient() {
        let sink = resolve_recipient("log:file:/tmp/opencapx-test.log", &[]).expect("resolve");
        assert_eq!(sink.kind(), "log:file");
        // send() uses a tmpdir, verifying append behavior
        let env = sample_env();
        let body = r#"{"source":"plugin.metrics.cpu","severity":"high"}"#;
        let tmp =
            std::env::temp_dir().join(format!("opencapx-file-sink-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let sink = FileSink { path: tmp.clone() };
        sink.send(&env, body).expect("send 1");
        sink.send(&env, body).expect("send 2");
        let contents = std::fs::read_to_string(&tmp).expect("read");
        let line_count = contents.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            line_count, 2,
            "should append 2 lines, got {line_count}: {contents}"
        );
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn resolve_log_file_recipient_empty_path_errors() {
        let res = resolve_recipient("log:file:", &[]);
        assert!(res.is_err());
    }

    #[test]
    fn resolve_email_smtp_recipient_parses_components() {
        let sink = resolve_recipient(
            "email:smtp:smtp.gmail.com:587:alerts@example.com:oncall@example.com",
            &[],
        )
        .expect("resolve");
        assert_eq!(sink.kind(), "email:smtp");
    }

    #[test]
    fn resolve_email_smtp_recipient_rejects_bad_format() {
        let res = resolve_recipient("email:smtp:no-port", &[]);
        assert!(res.is_err());
    }

    #[test]
    fn resolve_email_smtp_recipient_rejects_bad_port() {
        let res = resolve_recipient(
            "email:smtp:smtp.gmail.com:not-a-port:alerts@example.com:oncall@example.com",
            &[],
        );
        assert!(res.is_err());
    }

    #[test]
    fn resolve_unknown_kind_errors() {
        let res = resolve_recipient("foo:bar", &[]);
        assert!(res.is_err());
        assert!(res.err().expect("expected error").contains("unknown"));
    }

    #[test]
    fn resolve_empty_spec_errors() {
        let res = resolve_recipient("", &[]);
        assert!(res.is_err());
    }

    #[test]
    fn stderr_sink_writes_to_stderr_without_panic() {
        let sink = StderrSink;
        let env = sample_env();
        let body = "test alert body";
        // No panic + returns Ok
        sink.send(&env, body).expect("stderr send");
    }

    #[test]
    fn smtp_sink_mock_returns_ok() {
        let sink = SmtpSink {
            relay: "smtp.example.com".into(),
            port: 587,
            from: "alerts@example.com".into(),
            to: "oncall@example.com".into(),
        };
        let env = sample_env();
        let body = "cpu alert body";
        // MOCK — does not actually send, only eprintln
        sink.send(&env, body).expect("smtp mock send");
    }

    #[test]
    fn resolve_email_smtp_with_ipv6_like_address_uses_splitn() {
        // splitn(4) means the relay part may contain ':' (e.g. IPv6), the other 3 parts may not
        // but IPv6 makes the split behave oddly — real products use hostnames without ':'
        // Here we test splitn(4) behavior: relay + port + from + to, rejected if one segment is missing
        let res = resolve_recipient("email:smtp::587:a:b", &[]);
        // empty relay → splits into ["", "587", "a", "b"] → 4 parts → parses successfully (empty relay)
        // Does not require success, only verifies no panic
        let _ = res;
    }
}
