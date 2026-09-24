//! alerting bundle export/import and the signing/encryption + keychain secret layer.
//! Mechanical move from core/alerting.rs.

use super::*;
use hmac::{Hmac, Mac};
use sha2::Sha256;

pub(crate) fn endpoint_row_to_dto(
    row: &crate::core::storage::AlertingEndpointRow,
) -> WebhookEndpoint {
    // Phase 72 — deserialize the severity_overrides JSON. Missing / parse failure → empty Vec (equivalent to no override)
    let severity_overrides: Vec<(String, Severity)> = row
        .severity_overrides
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<(String, String)>>(s).ok())
        .map(|pairs| {
            pairs
                .into_iter()
                .filter_map(|(src, sev)| Severity::parse(&sev).map(|parsed| (src, parsed)))
                .collect()
        })
        .unwrap_or_default();
    WebhookEndpoint {
        id: row.id.clone(),
        name: row.name.clone(),
        url: row.url.clone(),
        enabled: row.enabled,
        headers: row.headers.clone(),
        secret: row.secret.clone(),
        source_filter: row.source_filter.clone(),
        schema_version: row.schema_version,
        template: row.template.clone(),
        template_sample: row.template_sample.clone(),
        severity_overrides,
    }
}

fn silence_row_to_dto(row: &crate::core::storage::SilenceRuleRow) -> SilenceRule {
    SilenceRule {
        id: row.id.clone(),
        name: row.name.clone(),
        kind_pattern: row.kind_pattern.clone(),
        starts_at: row.starts_at,
        ends_at: row.ends_at,
        weekdays: row.weekdays,
        start_hour: row.start_hour,
        end_hour: row.end_hour,
    }
}

fn ack_row_to_dto(row: &crate::core::storage::AckRuleRow) -> AckRule {
    // The AckRule DTO is (kind_pattern, window_secs); the row is (id, kind_pattern, ack_until, created_at).
    // Take the remaining seconds as window_secs; if already expired fall back to 1 (avoid a 0 in the bundle, which validation would reject on import).
    let now = crate::core::agent::now_secs();
    let remaining = if row.ack_until > now {
        row.ack_until - now
    } else {
        1
    };
    AckRule {
        kind_pattern: row.kind_pattern.clone(),
        window_secs: remaining,
    }
}

pub(crate) fn route_row_to_dto(row: &crate::core::storage::RouteRuleRow) -> RouteRule {
    RouteRule {
        id: row.id.clone(),
        name: row.name.clone(),
        priority: row.priority,
        enabled: row.enabled,
        kind_pattern: row.kind_pattern.clone(),
        payload_path: row.payload_path.clone(),
        payload_match: row.payload_match.clone(),
        target_endpoint_ids: row.target_endpoint_ids.clone(),
        recipients: row.recipients.clone(),
        tags: row.tags.clone(),
        seen_in_last: row
            .seen_in_last_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<SeenInLastSpec>(s).ok()),
    }
}

/// Phase 68 — `SeenInLastSpec` DTO → storage json string (None → None).
pub(crate) fn seen_in_last_to_json(spec: &Option<SeenInLastSpec>) -> Option<String> {
    spec.as_ref().and_then(|s| serde_json::to_string(s).ok())
}

/// Phase 62 — AckRule DTO → store. AckRule uses (kind_pattern, window_secs) semantics (consistent with `ack_kind`),
/// which do not align directly with the row's (id, kind_pattern, ack_until, created_at), so this thinly wraps `ack_kind`.
pub fn save_ack(rule: AckRule) -> Result<(), String> {
    if rule.window_secs == 0 || rule.window_secs > 7 * 86400 {
        return Err("ack window_secs must be 1..=604800".into());
    }
    ack_kind(rule.kind_pattern, rule.window_secs).map(|_| ())
}

/// Phase 62 — Full alerting config bundle document schema: five sections — endpoints + routes + presets + silences + acks.
/// The top carries `version` (bundle schema version) + `exportedAt` (unix seconds), for backup / cross-environment migration.
/// When a section is missing (not written in the yaml at all), serde default = empty vec, no error.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlertingBundleDoc {
    #[serde(default = "default_bundle_doc_version")]
    pub version: u32,
    #[serde(default)]
    pub exported_at: u64,
    #[serde(default)]
    pub endpoints: Vec<WebhookEndpoint>,
    #[serde(default)]
    pub routes: Vec<RouteRule>,
    #[serde(default)]
    pub presets: Vec<TemplatePreset>,
    #[serde(default)]
    pub silences: Vec<SilenceRule>,
    #[serde(default)]
    pub acks: Vec<AckRule>,
    /// Phase 66 — reusable notification recipient definitions.
    /// Kind is whitelisted: `webhook` / `log:stderr` / `log:file` / `email:smtp`,
    /// config is a serde_json::Value (each kind has a different schema).
    #[serde(default)]
    pub recipients: Vec<RecipientDef>,
}

/// Phase 62 — Current bundle schema version. When Phase 63+ adds fields, bump it + add a migrator.
pub const CURRENT_BUNDLE_VERSION: u32 = 1;

/// Phase 62 — Import result stats, per-section counts + total.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BundleImportSummary {
    pub endpoints: usize,
    pub routes: usize,
    pub presets: usize,
    pub silences: usize,
    pub acks: usize,
    /// Phase 66 — count for the top-level recipient definition section.
    /// Phase 66 only passes it through yaml (persistence is left to Phase 67).
    pub recipients: usize,
    pub total: usize,
}

/// Phase 62 — Serialize the whole alerting state (endpoints + routes + presets + silences + acks) to YAML / JSON.
/// presets include only user-defined ones; builtins are reloaded on the receiving side from backend constants.
/// Phase 64 — adds `passphrase: Option<&str>`:
/// - `None` → plaintext signed YAML (with a trailing `signature: <hex>`)
/// - `Some(pp)` → encrypted JSON envelope (salt+nonce+ciphertext, PBKDF2-derived key)
pub fn export_alerting_bundle(passphrase: Option<&str>) -> Result<String, String> {
    // endpoints: list_endpoints() returns AlertingEndpointRow, which must be converted to WebhookEndpoint before serializing
    // (otherwise AlertingEndpointRow lacks WebhookEndpoint's fields, and a future save_endpoint needs WebhookEndpoint arguments)
    let endpoint_rows = list_endpoints();
    let endpoints: Vec<WebhookEndpoint> = endpoint_rows.iter().map(endpoint_row_to_dto).collect();
    let route_rows = list_routes();
    let routes: Vec<RouteRule> = route_rows.iter().map(route_row_to_dto).collect();
    let silence_rows = list_silences();
    let silences: Vec<SilenceRule> = silence_rows.iter().map(silence_row_to_dto).collect();
    let ack_rows = list_acks();
    let acks: Vec<AckRule> = ack_rows.iter().map(ack_row_to_dto).collect();
    // builtin presets do not go into the bundle (the receiver loads them from backend constants); only user presets are included
    let presets: Vec<TemplatePreset> = list_template_presets()
        .into_iter()
        .filter(|p| !p.builtin)
        .collect();
    // Phase 67 — the recipients section is truly persisted (no longer passed through yaml)
    let recipients: Vec<RecipientDef> = list_recipients();
    let doc = AlertingBundleDoc {
        version: CURRENT_BUNDLE_VERSION,
        exported_at: crate::core::agent::now_secs(),
        endpoints,
        routes,
        presets,
        silences,
        acks,
        recipients,
    };
    let yaml_body = serde_yaml::to_string(&doc).map_err(|e| format!("bundle serialize: {}", e))?;
    let signed = sign_bundle(&yaml_body)?;
    match passphrase {
        None => Ok(signed),
        Some(pp) => encrypt_bundle(&signed, pp),
    }
}

/// Phase 62 — Import a bundle document, upserting section by section. Preset builtin is forced = false (prevents a malicious bundle from overwriting).
/// Endpoints with the same name reuse the existing id; presets / routes / silences are overwritten by id. Returns per-section counts.
/// Phase 64 — `passphrase: Option<&str>`: `None` → expects plaintext signed YAML; `Some(pp)` → expects an encrypted envelope.
/// Auto-detection: try parsing as JSON + having `algorithm: aes-...-pbkdf2-sha256` → encrypted; otherwise treat as signed plaintext.
pub fn import_alerting_bundle(
    input: &str,
    passphrase: Option<&str>,
) -> Result<BundleImportSummary, String> {
    let signed_yaml = decode_bundle_input(input, passphrase)?;
    let (body, _sig) = split_signature(&signed_yaml)?;
    // verify the signature (constant time) to prevent tampering
    verify_bundle(&signed_yaml)?;
    let doc: AlertingBundleDoc =
        serde_yaml::from_str(body).map_err(|e| format!("bundle yaml parse: {}", e))?;
    if doc.version > CURRENT_BUNDLE_VERSION {
        return Err(format!(
            "bundle version {} is newer than current {}; refusing to import future schema",
            doc.version, CURRENT_BUNDLE_VERSION
        ));
    }
    let mut summary = BundleImportSummary::default();
    // endpoints
    for ep in doc.endpoints {
        let _ = save_endpoint(ep).map_err(|e| format!("bundle endpoint: {}", e))?;
        summary.endpoints += 1;
    }
    // routes
    for rule in doc.routes {
        save_route(rule).map_err(|e| format!("bundle route: {}", e))?;
        summary.routes += 1;
    }
    // presets (builtin forced false)
    for mut p in doc.presets {
        p.builtin = false;
        if p.id.is_empty() {
            p.id = gen_event_id();
        }
        if p.kind.is_empty() {
            p.kind = format!("user:{}", p.id);
        }
        let _ = save_user_template_preset(&p).map_err(|e| format!("bundle preset: {}", e))?;
        summary.presets += 1;
    }
    // silence
    for s in doc.silences {
        let _ = save_silence(s).map_err(|e| format!("bundle silence: {}", e))?;
        summary.silences += 1;
    }
    // acks: reuse the save_ack helper (already exists, written in Phase 50; check the name)
    for a in doc.acks {
        let _ = save_ack(a).map_err(|e| format!("bundle ack: {}", e))?;
        summary.acks += 1;
    }
    // Phase 67 — the recipients section is truly persisted (upserted one by one; kind must pass the whitelist)
    for rec in doc.recipients {
        validate_recipient_kind(&rec.kind)?;
        save_recipient(rec).map_err(|e| format!("bundle recipient: {e}"))?;
        summary.recipients += 1;
    }
    summary.total = summary.endpoints
        + summary.routes
        + summary.presets
        + summary.silences
        + summary.acks
        + summary.recipients;
    Ok(summary)
}

type HmacSha256 = Hmac<Sha256>;

const KEYCHAIN_SERVICE: &str = "com.opencapx.desktop";

const KEYCHAIN_USER: &str = "bundle-signing-v1";

const FALLBACK_FILENAME: &str = "bundle-signing-v1.key";

static SECRET_BUF: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Ensure the current process secret is loaded (empty buffer → try keychain → on failure degrade to file).
pub fn ensure_secret_loaded() -> Result<(), String> {
    {
        let guard = SECRET_BUF
            .lock()
            .map_err(|_| "secret mutex poisoned".to_string())?;
        if !guard.is_empty() {
            return Ok(());
        }
    }
    // 1) prefer the keychain
    match load_from_keychain() {
        Ok(bytes) => {
            let mut guard = SECRET_BUF
                .lock()
                .map_err(|_| "secret mutex poisoned".to_string())?;
            *guard = bytes;
            return Ok(());
        }
        Err(e) => {
            eprintln!("[opencapx] keychain unavailable ({e}); falling back to file secret");
        }
    }
    // 2) degrade to file
    let bytes = load_or_create_file_secret()?;
    let mut guard = SECRET_BUF
        .lock()
        .map_err(|_| "secret mutex poisoned".to_string())?;
    *guard = bytes;
    Ok(())
}

/// Used by sign_bundle — returns an owned Vec<u8> (avoids calling Hmac while holding the lock).
pub fn current_secret_bytes() -> Result<Vec<u8>, String> {
    ensure_secret_loaded()?;
    let guard = SECRET_BUF
        .lock()
        .map_err(|_| "secret mutex poisoned".to_string())?;
    Ok(guard.clone())
}

/// Phase 65: rotate the secret (generate a new 32 bytes, write keychain + degrade to file on failure, update the in-memory buffer).
/// A bundle exported under the old secret fails verification under the new secret.
pub fn rotate_bundle_secret() -> Result<(), String> {
    let mut bytes = vec![0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("getrandom: {e}"))?;
    if let Err(e) = write_to_keychain(&bytes) {
        eprintln!("[opencapx] keychain rotate failed ({e}); falling back to file");
        write_fallback_secret(&bytes)?;
    }
    let mut guard = SECRET_BUF
        .lock()
        .map_err(|_| "secret mutex poisoned".to_string())?;
    *guard = bytes;
    Ok(())
}

fn load_from_keychain() -> Result<Vec<u8>, String> {
    // cargo test skips the keychain: the test binary's hash changes on every rebuild, and the Keychain ACL
    // does not recognize it → get_password pops an infinite GUI authorization prompt (while still holding TEST_STORE_LOCK,
    // hanging the entire alerting test suite). Tests degrade to the file secret; sign/verify semantics are unchanged.
    if cfg!(test) {
        return Err("keychain disabled under cargo test".into());
    }
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
        .map_err(|e| format!("keychain entry init: {e}"))?;
    let secret_b64 = entry
        .get_password()
        .map_err(|e| format!("keychain get_password: {e}"))?;
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &secret_b64)
        .map_err(|e| format!("base64 decode secret: {e}"))?;
    if bytes.len() != 32 {
        return Err(format!("keychain secret length {} != 32", bytes.len()));
    }
    Ok(bytes)
}

fn write_to_keychain(bytes: &[u8]) -> Result<(), String> {
    // same as load_from_keychain: the test process does not touch the keychain (set_password would also trigger an authorization prompt).
    if cfg!(test) {
        return Err("keychain disabled under cargo test".into());
    }
    let entry = keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)
        .map_err(|e| format!("keychain entry init: {e}"))?;
    let secret_b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    entry
        .set_password(&secret_b64)
        .map_err(|e| format!("keychain set_password: {e}"))
}

pub(crate) fn fallback_secret_path() -> Result<std::path::PathBuf, String> {
    let base = dirs::config_dir().ok_or_else(|| "no config_dir".to_string())?;
    Ok(base.join("opencapx").join(FALLBACK_FILENAME))
}

pub(crate) fn load_or_create_file_secret() -> Result<Vec<u8>, String> {
    use std::io::Read;
    let path = fallback_secret_path()?;
    if path.exists() {
        let mut f = std::fs::File::open(&path).map_err(|e| format!("open fallback: {e}"))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)
            .map_err(|e| format!("read fallback: {e}"))?;
        if buf.len() != 32 {
            return Err(format!("fallback secret length {} != 32", buf.len()));
        }
        return Ok(buf);
    }
    let mut bytes = vec![0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("getrandom: {e}"))?;
    write_fallback_secret(&bytes)?;
    Ok(bytes)
}

fn write_fallback_secret(bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let path = fallback_secret_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir fallback: {e}"))?;
    }
    let mut f = std::fs::File::create(&path).map_err(|e| format!("create fallback: {e}"))?;
    f.write_all(bytes)
        .map_err(|e| format!("write fallback: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 0600 fallback: {e}"))?;
    }
    Ok(())
}

/// Phase 65 test-only — clear the in-memory buffer, forcing the next access to go through keychain/file again.
/// (rotate already updates the buffer itself; this function is a reset fixture for tests.)
#[cfg(test)]
pub fn reset_secret_buffer_for_test() {
    if let Ok(mut g) = SECRET_BUF.lock() {
        g.clear();
    }
}

/// Sign the whole yaml string and append a `signature: <hex>` line at the end.
/// `sig_hex = hex(hmac_sha256(yaml_body, current_secret))`。
/// Phase 65: the secret goes through a lazy accessor; sign_bundle no longer takes a `_secret` argument.
pub fn sign_bundle(yaml_body: &str) -> Result<String, String> {
    let secret = current_secret_bytes()?;
    let mut mac = HmacSha256::new_from_slice(&secret).map_err(|e| format!("hmac init: {}", e))?;
    mac.update(yaml_body.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_hex = sig.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    let mut out = yaml_body.trim_end().to_string();
    out.push('\n');
    out.push_str(&format!("signature: \"{}\"\n", sig_hex));
    Ok(out)
}

/// Verify: split the body + sig out of a yaml string carrying `signature:`, then compare in constant time.
pub fn verify_bundle(signed_yaml: &str) -> Result<(), String> {
    let (body, sig_hex) = split_signature(signed_yaml)?;
    let secret = current_secret_bytes()?;
    let mut mac = HmacSha256::new_from_slice(&secret).map_err(|e| format!("hmac init: {}", e))?;
    mac.update(body.as_bytes());
    let expected = mac.finalize().into_bytes();
    let provided = decode_hex(&sig_hex)?;
    if provided.len() != expected.len() {
        return Err("bundle signature length mismatch".into());
    }
    // constant-time comparison (hand-rolled, to avoid verify_slice's requirement that it consume the mac)
    let mut diff: u8 = 0;
    for (a, b) in expected.iter().zip(provided.iter()) {
        diff |= a ^ b;
    }
    if diff != 0 {
        return Err("bundle signature mismatch (content tampered)".into());
    }
    Ok(())
}

/// Decode a hex string of the form "xx xx xx" back to bytes (supports plain hex and space-separated formats).
fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(format!("hex string odd length: {}", s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte = u8::from_str_radix(&s[i..i + 2], 16)
            .map_err(|e| format!("hex parse at {}: {}", i, e))?;
        out.push(byte);
    }
    Ok(out)
}

/// Split a yaml string carrying a `signature:` line into (body_before_sig, sig_hex).
/// Allows a preceding `\n` before the signature line, with or without spaces/quotes after it.
pub(crate) fn split_signature(signed_yaml: &str) -> Result<(&str, String), String> {
    let bytes = signed_yaml.as_bytes();
    let needle = b"signature:";
    // find the last `signature:` (guards against a same-named key inside the yaml body)
    let mut last = None;
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // ensure it is at line start (preceded by \n or the start)
            if i == 0 || bytes[i - 1] == b'\n' {
                last = Some(i);
            }
        }
        i += 1;
    }
    let idx = last.ok_or_else(|| "bundle missing signature: field".to_string())?;
    let body = &signed_yaml[..idx];
    // skip all spaces + optional quotes after `signature:`
    let rest = &signed_yaml[idx + needle.len()..];
    let rest = rest.trim_start();
    let rest = if let Some(stripped) = rest.strip_prefix('"') {
        stripped
    } else {
        rest
    };
    let end = rest
        .find(|c: char| c == '"' || c == '\n' || c == ' ')
        .unwrap_or(rest.len());
    let sig = rest[..end].to_string();
    Ok((body, sig))
}

/// Encrypted JSON envelope top-level schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedEnvelope {
    version: u32,
    algorithm: String,
    kdf: String,
    iterations: u32,
    salt: String,       // base64
    nonce: String,      // base64
    ciphertext: String, // base64
}

/// Phase 64 — Encrypt the whole signed YAML with a passphrase (using AES-256-GCM + a PBKDF2-SHA256-derived key).
/// Returns envelope JSON (safe to drop into git / email / any text channel).
pub fn encrypt_bundle(signed_yaml: &str, passphrase: &str) -> Result<String, String> {
    if passphrase.is_empty() {
        return Err("passphrase must not be empty".into());
    }
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use getrandom::getrandom;
    use pbkdf2::pbkdf2_hmac;

    let mut salt = [0u8; 16];
    getrandom(&mut salt).map_err(|e| format!("getrandom salt: {}", e))?;
    let mut nonce_bytes = [0u8; 12];
    getrandom(&mut nonce_bytes).map_err(|e| format!("getrandom nonce: {}", e))?;

    let iterations = 100_000u32;
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, iterations, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("aes init: {}", e))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, signed_yaml.as_bytes())
        .map_err(|e| format!("aes encrypt: {}", e))?;

    let envelope = EncryptedEnvelope {
        version: 1,
        algorithm: "aes-256-gcm-pbkdf2-sha256".into(),
        kdf: "pbkdf2-sha256".into(),
        iterations,
        salt: B64.encode(salt),
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(ciphertext),
    };
    serde_json::to_string_pretty(&envelope).map_err(|e| format!("envelope serialize: {}", e))
}

/// Phase 64 — Decrypt an envelope back to signed YAML. Requires the correct passphrase + intact nonce + salt.
pub fn decrypt_bundle(envelope_json: &str, passphrase: &str) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use pbkdf2::pbkdf2_hmac;

    let env: EncryptedEnvelope =
        serde_json::from_str(envelope_json).map_err(|e| format!("envelope parse: {}", e))?;
    if env.algorithm != "aes-256-gcm-pbkdf2-sha256" {
        return Err(format!("unsupported algorithm: {}", env.algorithm));
    }
    let salt = B64
        .decode(&env.salt)
        .map_err(|e| format!("salt decode: {}", e))?;
    let nonce_bytes = B64
        .decode(&env.nonce)
        .map_err(|e| format!("nonce decode: {}", e))?;
    let ciphertext = B64
        .decode(&env.ciphertext)
        .map_err(|e| format!("ciphertext decode: {}", e))?;
    if nonce_bytes.len() != 12 {
        return Err("nonce must be 12 bytes".into());
    }
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), &salt, env.iterations, &mut key);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("aes init: {}", e))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .map_err(|_| "decryption failed (wrong passphrase or tampered ciphertext)".to_string())?;
    String::from_utf8(plaintext).map_err(|e| format!("decoded not utf8: {}", e))
}

/// Phase 64 — Auto-detect the input format, decrypt (if encrypted) + return the final signed YAML.
/// plaintext passes straight through; encrypted requires a passphrase.
fn decode_bundle_input(input: &str, passphrase: Option<&str>) -> Result<String, String> {
    let trimmed = input.trim_start();
    if trimmed.starts_with('{') {
        // may be an encrypted envelope (JSON); if the algorithm field matches → decrypt
        if let Ok(env) = serde_json::from_str::<EncryptedEnvelope>(trimmed) {
            if env.algorithm.starts_with("aes-256-gcm") {
                let pp = passphrase.ok_or_else(|| {
                    "this bundle is passphrase-encrypted; please provide passphrase".to_string()
                })?;
                return decrypt_bundle(trimmed, pp);
            }
        }
        // not an envelope → take the plain path
    }
    Ok(input.to_string())
}
