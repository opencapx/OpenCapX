//! v1.4 printer.print builtin provider.
//!
//! Permission `printer.control` (ask): submit a job to the print queue. Physical-world side effects + possible
//! cost (consumables), so ask; but there is no data surface, so it is not high-risk.
//!
//! macOS / Linux share CUPS `lpr`. **Honest boundary**: lpr only confirms enqueue, not
//! that physical printing succeeded (jams/out-of-paper/offline printers all make lpr return 0). Windows has no matching
//! command-line path (`print` cmd only takes text), so it reports an honest error; plugins can override.

use serde_json::{json, Value};
use std::time::Duration;

const PRINT_TIMEOUT: Duration = Duration::from_secs(30);

/// Printer-name cap (embedded in argv, never a shell, so no injection surface; the cap only guards against misuse).
const PRINTER_MAX: usize = 200;

/// Input validation (pure function): (path, printer, copies). The file must exist; copies 1..=10.
fn validate(input: &Value) -> Result<(String, Option<String>, i64), String> {
    let Some(path) = input.get("path").and_then(|p| p.as_str()) else {
        return Err("invalid input: path (string) required".into());
    };
    if path.is_empty() {
        return Err("invalid input: path must be non-empty".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| format!("file unavailable: {}", e))?;
    if !meta.is_file() {
        return Err(format!("not a file: {}", path));
    }
    let printer = input
        .get("printer")
        .and_then(|p| p.as_str())
        .filter(|p| !p.is_empty());
    if let Some(p) = printer {
        if p.len() > PRINTER_MAX {
            return Err(format!(
                "invalid input: printer must be ≤{} chars",
                PRINTER_MAX
            ));
        }
    }
    let copies = input.get("copies").and_then(|c| c.as_i64()).unwrap_or(1);
    if !(1..=10).contains(&copies) {
        return Err("invalid input: copies must be 1..=10 (default 1)".into());
    }
    Ok((path.to_string(), printer.map(String::from), copies))
}

/// printer.print builtin implementation.
pub fn print(input: &Value) -> Result<Value, String> {
    let (path, printer, copies) = validate(input)?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let mut cmd = std::process::Command::new("lpr");
        if let Some(p) = &printer {
            cmd.arg("-P").arg(p);
        }
        if copies > 1 {
            cmd.arg(format!("-#{}", copies));
        }
        cmd.arg(&path);
        let out = super::appctl::run_with_timeout(cmd, PRINT_TIMEOUT)
            .map_err(|e| format!("print failed: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "lpr exited {:?}: {} (printer name from: lpstat -p)",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
                    .chars()
                    .take(200)
                    .collect::<String>()
            ));
        }
        Ok(json!({ "ok": true, "queued": true }))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (path, printer, copies);
        Err("printer.print builtin is macOS/Linux-only (CUPS lpr); a plugin can override".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_path_printer_copies() {
        let dir = std::env::temp_dir().join(format!("opencapx-print-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.pdf");
        std::fs::write(&f, b"%PDF-1.4").unwrap();

        let (p, pr, c) = validate(&json!({ "path": f.to_str().unwrap() })).unwrap();
        assert_eq!(p, f.to_str().unwrap());
        assert!(pr.is_none());
        assert_eq!(c, 1);
        let (_, pr, c) = validate(
            &json!({ "path": f.to_str().unwrap(), "printer": "HP-LaserJet", "copies": 3 }),
        )
        .unwrap();
        assert_eq!(pr.as_deref(), Some("HP-LaserJet"));
        assert_eq!(c, 3);
        // path missing / nonexistent / directory / copies out of range / printer over-long
        assert!(validate(&json!({})).unwrap_err().contains("path"));
        assert!(
            validate(&json!({ "path": dir.join("no.pdf").to_str().unwrap() }))
                .unwrap_err()
                .contains("unavailable")
        );
        assert!(validate(&json!({ "path": dir.to_str().unwrap() }))
            .unwrap_err()
            .contains("not a file"));
        assert!(
            validate(&json!({ "path": f.to_str().unwrap(), "copies": 11 }))
                .unwrap_err()
                .contains("1..=10")
        );
        assert!(
            validate(&json!({ "path": f.to_str().unwrap(), "printer": "x".repeat(201) }))
                .unwrap_err()
                .contains("200")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn print_reports_platform_limit() {
        // validate() runs before the platform branch and requires an existing absolute
        // file, so a made-up "/x.pdf" trips path validation on Windows before the
        // platform limit is ever reported. Use a real per-OS temp file.
        let dir = std::env::temp_dir().join(format!("opencapx-print-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.pdf");
        std::fs::write(&f, b"%PDF-1.4\n%%EOF\n").unwrap();
        assert!(print(&json!({ "path": f.to_str().unwrap() }))
            .unwrap_err()
            .contains("macOS/Linux-only"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
