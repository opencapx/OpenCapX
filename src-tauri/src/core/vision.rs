//! A9 Vision/Screenshot built-in providers (docs/capability.md "Built-in providers").
//! The Core backstops when no plugin registers a capability of the same name; plugin
//! registration overrides it.
//!
//! - `image.analyze`: local-first — Ollama HTTP API (`/api/generate`). The default model
//!   is the first installed model from `/api/tags` (works out of the box; use whatever the
//!   user installed); env `OPEN_CAPX_VISION_MODEL` pins the model and
//!   `OPEN_CAPX_OLLAMA_URL` changes the address. No key, no cloud dependency (the first
//!   piece of Privacy First, i2 §2.5 Local AI Gateway); reports honestly when Ollama is
//!   not running, no silent downgrade.
//! - `screen.capture`: platform command (macOS `screencapture` / Linux `scrot`) writes to
//!   `~/.opencapx/cache/` and returns the path. macOS needs the system "Screen Recording"
//!   permission, otherwise it captures only the desktop wallpaper. v1.2 adds Windows:
//!   in-process capture via xcap (no TCC equivalent), compile-verified only, real-device
//!   behavior pending verification.

use base64::Engine;
use serde_json::{json, Value};
use std::time::Duration;

/// Ollama default address (localhost, no TLS needed).
const DEFAULT_OLLAMA: &str = "http://127.0.0.1:11434";
/// Overall vision-generation timeout: a local model loads weights before the first token, so 60s is not enough.
const VISION_TIMEOUT: Duration = Duration::from_secs(180);

/// Screenshot cache directory `~/.opencapx/cache/` (auto-created).
/// Since v1.2 clipboard images / speech audio also land here, hence pub(crate).
pub(crate) fn cache_dir() -> Result<std::path::PathBuf, String> {
    let Some(home) = crate::core::home_dir() else {
        return Err("cannot resolve home directory".into());
    };
    let dir = home.join(".opencapx").join("cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
    Ok(dir)
}

/// image.analyze: local path / data: base64 / http(s) URL → base64.
/// URL downloads use reqwest (dependency already present, with TLS); local files use zero network.
fn load_image_b64(image: &str) -> Result<String, String> {
    if let Some(b64) = image.strip_prefix("data:") {
        // data:image/png;base64,xxxx — everything after the comma is the payload
        let payload = b64.split_once(',').map(|(_, p)| p).unwrap_or(b64);
        return Ok(payload.to_string());
    }
    if image.starts_with("http://") || image.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| format!("client build: {}", e))?;
        let bytes = client
            .get(image)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .map_err(|e| format!("image download failed: {}", e))?;
        // Cap at 20MB to stop an Agent from stuffing in a giant file
        if bytes.len() > 20 * 1024 * 1024 {
            return Err("image too large (max 20MB)".into());
        }
        return Ok(base64::engine::general_purpose::STANDARD.encode(&bytes));
    }
    // Local path
    let bytes = std::fs::read(image).map_err(|e| format!("image read failed: {}", e))?;
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("image too large (max 20MB)".into());
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

/// Default model = the first installed model from /api/tags (no hardcoded name: use
/// whatever the user installed; vision capability depends on the model, and a text model
/// will error out on the /api/generate side and surface it).
fn ollama_default_model(base_url: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let resp = client
        .get(format!("{}/api/tags", base_url.trim_end_matches('/')))
        .send()
        .map_err(|e| {
            format!(
                "vision provider unreachable: {} (start Ollama or set OPEN_CAPX_OLLAMA_URL)",
                e
            )
        })?;
    let v: Value = resp
        .json()
        .map_err(|e| format!("vision provider bad tags response: {}", e))?;
    v.get("models")
        .and_then(|m| m.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "no Ollama models installed (ollama pull a vision model, e.g. llava)".into())
}

/// Ollama /api/generate (non-streaming). Returns the model text.
fn ollama_generate(
    base_url: &str,
    model: &str,
    prompt: &str,
    image_b64: &str,
) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(VISION_TIMEOUT)
        .build()
        .map_err(|e| format!("client build: {}", e))?;
    let body = json!({
        "model": model,
        "prompt": prompt,
        "images": [image_b64],
        "stream": false,
    });
    let resp = client
        .post(format!("{}/api/generate", base_url.trim_end_matches('/')))
        .json(&body)
        .send()
        .map_err(|e| {
            format!(
                "vision provider unreachable: {} (start Ollama or set OPEN_CAPX_OLLAMA_URL)",
                e
            )
        })?;
    let status = resp.status();
    let text = resp
        .text()
        .map_err(|e| format!("vision provider read failed: {}", e))?;
    if !status.is_success() {
        // Model not pulled, etc.: pass through Ollama's error body verbatim (usually contains "model not found")
        return Err(format!(
            "vision provider HTTP {}: {}",
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        ));
    }
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("vision provider bad response: {}", e))?;
    v.get("response")
        .and_then(|r| r.as_str())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| "vision provider response missing .response".into())
}

/// Model resolution: env `OPEN_CAPX_VISION_MODEL` pins it, otherwise the first installed
/// model from /api/tags. Extracted from analyze in v1.2; ocr reuses the same path.
fn resolve_vision_model(base: &str) -> Result<String, String> {
    match std::env::var("OPEN_CAPX_VISION_MODEL") {
        Ok(m) if !m.is_empty() => Ok(m),
        _ => ollama_default_model(base),
    }
}

/// image.analyze built-in implementation. Output aligns with the docs/capability.md
/// schema: description (required) / text / objects[] (local models do not detect, always empty).
pub fn analyze(input: &Value) -> Result<Value, String> {
    let Some(image) = input.get("image").and_then(|i| i.as_str()) else {
        return Err("invalid input: image (string) required".into());
    };
    if image.is_empty() {
        return Err("invalid input: image must not be empty".into());
    }
    let b64 = load_image_b64(image)?;
    let base = std::env::var("OPEN_CAPX_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA.into());
    let model = resolve_vision_model(&base)?;
    let prompt = input
        .get("question")
        .and_then(|q| q.as_str())
        .filter(|q| !q.is_empty())
        .unwrap_or("Describe this image concisely.");
    let text = ollama_generate(&base, &model, prompt, &b64)?;
    if text.is_empty() {
        return Err("vision provider returned empty description".into());
    }
    Ok(json!({ "description": text, "text": text, "objects": [] }))
}

/// image.ocr built-in implementation (v1.2): the same Ollama path with a fixed
/// transcription prompt. blocks is always [] (the built-in has no position info; install a
/// Vision plugin to override if you want position blocks).
pub fn ocr(input: &Value) -> Result<Value, String> {
    let Some(image) = input.get("image").and_then(|i| i.as_str()) else {
        return Err("invalid input: image (string) required".into());
    };
    if image.is_empty() {
        return Err("invalid input: image must not be empty".into());
    }
    let b64 = load_image_b64(image)?;
    let base = std::env::var("OPEN_CAPX_OLLAMA_URL").unwrap_or_else(|_| DEFAULT_OLLAMA.into());
    let model = resolve_vision_model(&base)?;
    let text = ollama_generate(
        &base,
        &model,
        "Transcribe all text visible in this image exactly. Output only the transcription, preserving line breaks.",
        &b64,
    )?;
    if text.is_empty() {
        return Err("vision provider returned empty transcription".into());
    }
    Ok(json!({ "text": text, "blocks": [] }))
}

/// screen.capture's platform arguments (pure function, test-friendly): by default a silent full-screen PNG capture.
#[cfg(target_os = "macos")]
fn capture_args(
    out: &str,
    region: Option<&Value>,
    window: Option<i64>,
) -> Result<Vec<String>, String> {
    let mut args = vec!["-x".to_string()];
    if let Some(w) = window {
        args.push(format!("-l{}", w));
    }
    if let Some(r) = region {
        let x = r.get("x").and_then(|v| v.as_i64());
        let y = r.get("y").and_then(|v| v.as_i64());
        let w = r.get("width").and_then(|v| v.as_i64());
        let h = r.get("height").and_then(|v| v.as_i64());
        match (x, y, w, h) {
            (Some(x), Some(y), Some(w), Some(h)) => args.push(format!("-R{},{},{},{}", x, y, w, h)),
            _ => return Err("invalid region: x, y, width, height (integers) all required".into()),
        }
    }
    args.push(out.to_string());
    Ok(args)
}

#[cfg(target_os = "macos")]
fn capture_command(args: Vec<String>) -> std::process::Command {
    let mut c = std::process::Command::new("screencapture");
    c.args(&args);
    c
}

#[cfg(target_os = "linux")]
fn capture_args(
    out: &str,
    region: Option<&Value>,
    _window: Option<i64>,
) -> Result<Vec<String>, String> {
    if _window.is_some() {
        return Err(
            "window capture not supported on Linux builtin (use region or full screen)".into(),
        );
    }
    let mut args = Vec::new();
    if let Some(r) = region {
        let x = r.get("x").and_then(|v| v.as_i64());
        let y = r.get("y").and_then(|v| v.as_i64());
        let w = r.get("width").and_then(|v| v.as_i64());
        let h = r.get("height").and_then(|v| v.as_i64());
        match (x, y, w, h) {
            (Some(x), Some(y), Some(w), Some(h)) => {
                args.push(format!("-a={},{},{},{}", x, y, w, h))
            }
            _ => return Err("invalid region: x, y, width, height (integers) all required".into()),
        }
    }
    args.push(out.to_string());
    Ok(args)
}

#[cfg(target_os = "linux")]
fn capture_command(args: Vec<String>) -> std::process::Command {
    let mut c = std::process::Command::new("scrot");
    c.args(&args);
    c
}

/// screen.capture built-in implementation: the platform command writes to the cache
/// directory and returns `{image: path}`. macOS/Linux use the platform command (a TCC
/// denial = a clean non-zero exit, better than silently handing back only the wallpaper
/// in-process); Windows uses in-process capture via xcap (no TCC equivalent).
pub fn capture(input: &Value) -> Result<Value, String> {
    #[cfg(target_os = "windows")]
    {
        return capture_windows(input);
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = input;
        return Err("screen.capture builtin not supported on this platform".into());
    }
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let region = input.get("region").filter(|r| r.is_object());
        if let Some(r) = region {
            if r.get("x").is_none() && r.get("width").is_none() {
                return Err("invalid region: expected {x, y, width, height}".into());
            }
        }
        let window = input.get("window").and_then(|w| w.as_i64());
        let dir = cache_dir()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let out = dir.join(format!("screen-{}.png", nanos));
        let out_str = out.to_string_lossy().to_string();
        let args = capture_args(&out_str, region, window)?;
        let status = capture_command(args).status().map_err(|e| {
            #[cfg(target_os = "linux")]
            let hint = " (install scrot)";
            #[cfg(not(target_os = "linux"))]
            let hint = "";
            format!("capture command failed{}: {}", hint, e)
        })?;
        if !status.success() {
            // The most common macOS cause is a missing "Screen Recording" TCC permission (the capture is only the wallpaper or fails outright)
            return Err(format!(
                "capture exited {:?} (macOS: grant Screen Recording permission in System Settings)",
                status.code()
            ));
        }
        if !out.exists() {
            return Err("capture produced no file".into());
        }
        Ok(json!({ "image": out_str }))
    }
}

/// Pure function: clamp region into the image bounds. w/h ≤ 0, or empty after clamping →
/// error; negative/out-of-bounds origins clamp to [0, bound], keeping crop_imm in bounds.
/// Compiles on all three platforms (unit tests must run on mac too); only the Windows path
/// calls it.
fn clamp_region(
    iw: i64,
    ih: i64,
    x: i64,
    y: i64,
    w: i64,
    h: i64,
) -> Result<(u32, u32, u32, u32), String> {
    if w <= 0 || h <= 0 {
        return Err("invalid region: width and height must be positive".into());
    }
    if iw <= 0 || ih <= 0 {
        return Err("invalid region: source image is empty".into());
    }
    let x = x.clamp(0, iw);
    let y = y.clamp(0, ih);
    let w = w.min(iw - x);
    let h = h.min(ih - y);
    if w == 0 || h == 0 {
        return Err("invalid region: fully outside image bounds".into());
    }
    Ok((x as u32, y as u32, w as u32, h as u32))
}

/// Windows capture: full screen of the first xcap monitor / region crop, persisted with
/// the same naming. Note: compile-verified only (this repo has no Windows machine);
/// behavior pending CI / user verification.
#[cfg(target_os = "windows")]
fn capture_windows(input: &Value) -> Result<Value, String> {
    if input.get("window").and_then(|w| w.as_i64()).is_some() {
        return Err(
            "window capture not supported on Windows builtin (use region or full screen)".into(),
        );
    }
    let monitors = xcap::Monitor::all().map_err(|e| format!("capture monitors failed: {}", e))?;
    let monitor = monitors.first().ok_or("no monitor found")?;
    let mut img = monitor
        .capture_image()
        .map_err(|e| format!("capture failed: {}", e))?;
    if let Some(r) = input.get("region").filter(|r| r.is_object()) {
        let (x, y, w, h) = (
            r.get("x").and_then(|v| v.as_i64()),
            r.get("y").and_then(|v| v.as_i64()),
            r.get("width").and_then(|v| v.as_i64()),
            r.get("height").and_then(|v| v.as_i64()),
        );
        match (x, y, w, h) {
            (Some(x), Some(y), Some(w), Some(h)) => {
                let (cx, cy, cw, ch) =
                    clamp_region(img.width() as i64, img.height() as i64, x, y, w, h)?;
                img = image::imageops::crop_imm(&img, cx, cy, cw, ch).to_image();
            }
            _ => return Err("invalid region: x, y, width, height (integers) all required".into()),
        }
    }
    let dir = cache_dir()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let out = dir.join(format!("screen-{}.png", nanos));
    let out_str = out.to_string_lossy().to_string();
    img.save(&out)
        .map_err(|e| format!("capture save failed: {}", e))?;
    Ok(json!({ "image": out_str }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under parallel tests env overrides step on each other, so serialize every case that touches env.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
        M.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn analyze_requires_image() {
        assert!(analyze(&json!({})).unwrap_err().contains("image"));
        assert!(analyze(&json!({ "image": "" }))
            .unwrap_err()
            .contains("empty"));
    }

    /// Ollama unreachable (nothing listening on port 1) → honest error with guidance.
    #[ignore = "environment-dependent: needs a local HTTP endpoint behaving as unreachable; run with --ignored on a dev machine"]
    #[test]
    fn analyze_reports_unreachable_provider() {
        let _g = lock();
        std::env::set_var("OPEN_CAPX_OLLAMA_URL", "http://127.0.0.1:1");
        let err = analyze(&json!({ "image": "data:image/png;base64,aGVsbG8=" })).unwrap_err();
        std::env::remove_var("OPEN_CAPX_OLLAMA_URL");
        assert!(err.contains("unreachable"), "{}", err);
    }

    /// load_image_b64's three inputs: local path / data: URI / empty path.
    #[test]
    fn load_image_b64_variants() {
        let dir = std::env::temp_dir().join(format!("opencapx-vis-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("i.png");
        std::fs::write(&p, [1u8, 2, 3]).unwrap();
        assert_eq!(
            load_image_b64(p.to_str().unwrap()).unwrap(),
            base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3])
        );
        assert_eq!(
            load_image_b64("data:image/png;base64,QUJD").unwrap(),
            "QUJD"
        );
        assert!(load_image_b64("/nonexistent/opencapx.png")
            .unwrap_err()
            .contains("read failed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn capture_args_full_region_window() {
        let a = capture_args("/tmp/x.png", None, None).unwrap();
        assert_eq!(a, vec!["-x", "/tmp/x.png"]);
        let a = capture_args(
            "/tmp/x.png",
            Some(&json!({"x":1,"y":2,"width":3,"height":4})),
            None,
        )
        .unwrap();
        assert_eq!(a, vec!["-x", "-R1,2,3,4", "/tmp/x.png"]);
        let a = capture_args("/tmp/x.png", None, Some(77)).unwrap();
        assert_eq!(a, vec!["-x", "-l77", "/tmp/x.png"]);
        assert!(capture_args("/tmp/x.png", Some(&json!({"x":1})), None).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn capture_args_region_and_window_rejected() {
        let a = capture_args("/tmp/x.png", None, None).unwrap();
        assert_eq!(a, vec!["/tmp/x.png"]);
        let a = capture_args(
            "/tmp/x.png",
            Some(&json!({"x":1,"y":2,"width":3,"height":4})),
            None,
        )
        .unwrap();
        assert_eq!(a, vec!["-a=1,2,3,4", "/tmp/x.png"]);
        assert!(capture_args("/tmp/x.png", None, Some(7)).is_err());
    }

    /// capture actually takes one screenshot (macOS local dev only; CI has no screen permission and would fail, hence ignore).
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "captures the real screen; run with --ignored manually"]
    fn capture_roundtrip_manual() {
        let out = capture(&json!({})).unwrap();
        let p = out["image"].as_str().unwrap();
        assert!(std::path::Path::new(p).exists());
        let _ = std::fs::remove_file(p);
    }

    /// v1.2 Windows region-crop clamp math: runs on all three platforms (pure function).
    #[test]
    fn clamp_region_clamps_and_rejects() {
        // Pass through as-is
        assert_eq!(
            clamp_region(1920, 1080, 0, 0, 800, 600).unwrap(),
            (0, 0, 800, 600)
        );
        // Negative origin clamps to 0
        assert_eq!(
            clamp_region(1920, 1080, -50, -50, 800, 600).unwrap(),
            (0, 0, 800, 600)
        );
        // Out-of-bounds origin + oversize → clamps to the bound, width/height shrink
        assert_eq!(
            clamp_region(1920, 1080, 1900, 1000, 800, 600).unwrap(),
            (1900, 1000, 20, 80)
        );
        // w/h ≤ 0 errors
        assert!(clamp_region(1920, 1080, 0, 0, 0, 600).is_err());
        assert!(clamp_region(1920, 1080, 0, 0, 800, -1).is_err());
        // Origin entirely outside → empty after clamping → error (rather than a 0-size panic)
        assert!(clamp_region(100, 100, 500, 500, 10, 10).is_err());
    }

    /// Default model resolution: unreachable → error; real Ollama → the first model name (local manual run only).
    #[ignore = "environment-dependent: needs a local HTTP endpoint behaving as unreachable; run with --ignored on a dev machine"]
    #[test]
    fn default_model_unreachable_errors() {
        let _g = lock();
        assert!(ollama_default_model("http://127.0.0.1:1")
            .unwrap_err()
            .contains("unreachable"));
    }

    /// v1.2 ocr: missing image errors; an unreachable provider errors honestly (env lock reuses the module's lock).
    #[test]
    fn ocr_requires_image() {
        let err = ocr(&json!({})).unwrap_err();
        assert!(err.contains("image"), "{}", err);
    }

    #[ignore = "environment-dependent: needs a local HTTP endpoint behaving as unreachable; run with --ignored on a dev machine"]
    #[test]
    fn ocr_reports_unreachable_provider() {
        let _g = lock();
        std::env::set_var("OPEN_CAPX_OLLAMA_URL", "http://127.0.0.1:1");
        let err = ocr(&json!({ "image": "data:image/png;base64,iVBORw0KGgo=" })).unwrap_err();
        std::env::remove_var("OPEN_CAPX_OLLAMA_URL");
        assert!(err.contains("unreachable"), "{}", err);
    }

    #[test]
    #[ignore = "needs local Ollama with a vision model; run with --ignored manually"]
    fn ocr_real_ollama_manual() {
        // A self-contained text fixture (640x140, black text on white, "OPENCAPX 42"); it
        // does not share analyze's pure-red probe: a textless image is correctly rejected by
        // ocr's "empty transcription error", so the fixture must contain text.
        const PROBE_PNG_B64: &str = "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";
        let dir = std::env::temp_dir().join(format!("opencapx-ocrprobe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("probe.png");
        std::fs::write(
            &p,
            base64::engine::general_purpose::STANDARD
                .decode(PROBE_PNG_B64)
                .expect("fixture b64"),
        )
        .unwrap();
        let out = ocr(&json!({ "image": p.to_str().unwrap() })).unwrap();
        let text = out["text"].as_str().unwrap().to_uppercase();
        assert!(text.contains("OPENCAPX"), "transcription: {}", text);
        assert!(text.contains("42"), "transcription: {}", text);
        assert_eq!(out["blocks"].as_array().map(|a| a.len()), Some(0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "needs local Ollama running; run with --ignored manually"]
    fn analyze_real_ollama_manual() {
        let out = analyze(&json!({
            "image": "/tmp/probe.png",
            "question": "What color is this image? One word."
        }))
        .unwrap();
        let d = out["description"].as_str().unwrap().to_lowercase();
        assert!(d.contains("red"), "expected red, got: {}", d);
    }
}
