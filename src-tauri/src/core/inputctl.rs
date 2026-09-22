//! v1.3 input.send builtin provider: synthetic keyboard/mouse events (docs/capability.md).
//!
//! Permission `input.control` (denied + high-risk): the most direct "act on the user's behalf" surface;
//! denied by default, only allow-once at runtime, per call (high-risk never gets Always).
//!
//! - macOS: CoreGraphics CGEvent FFI (keyboard/text/mouse), with an up-front Accessibility probe
//!   (AXIsProcessTrusted, a no-prompt preflight)
//! - Linux: xdotool subprocess (missing one prompts install)
//! - Windows: user32 SendInput FFI (INPUT's union payload is copied byte-wise into [u64;4])
//!
//! Known limitation (noted in the docs): synthetic input on all three platforms may be silently dropped by the host environment
//! (macOS missing Accessibility / Wayland not supporting xdotool / Windows UIPI elevated windows),
//! and the builtin layer cannot confirm afterward; the Agent must observe the result to judge whether it took effect.

use serde_json::{json, Value};
use std::time::Duration;

/// Upper limit for type=text input.
const TEXT_MAX: usize = 500;
/// Event gap: synthetic events fired too quickly get dropped by some apps.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const EVENT_GAP: Duration = Duration::from_millis(15);

/// Key name → per-platform key codes. A pure table, shared by the three platforms' unit tests.
/// Modifier combinations (cmd+c etc.) are not done in v1 — combo semantics differ greatly per platform, plugins can override.
#[allow(dead_code)] // Platform fields are read only in their own platform build
pub(crate) struct KeySpec {
    /// macOS CGKeyCode (HIToolbox virtual key code)
    pub mac: u16,
    /// xdotool key name
    pub linux: &'static str,
    /// Windows Virtual-Key code
    pub win: u16,
}

pub(crate) fn key_spec(name: &str) -> Option<KeySpec> {
    let s = match name {
        "return" => KeySpec { mac: 36, linux: "Return", win: 0x0D },
        "tab" => KeySpec { mac: 48, linux: "Tab", win: 0x09 },
        "esc" => KeySpec { mac: 53, linux: "Escape", win: 0x1B },
        "delete" => KeySpec { mac: 51, linux: "BackSpace", win: 0x08 },
        "forward_delete" => KeySpec { mac: 117, linux: "Delete", win: 0x2E },
        "space" => KeySpec { mac: 49, linux: "space", win: 0x20 },
        "left" => KeySpec { mac: 123, linux: "Left", win: 0x25 },
        "right" => KeySpec { mac: 124, linux: "Right", win: 0x27 },
        "up" => KeySpec { mac: 126, linux: "Up", win: 0x26 },
        "down" => KeySpec { mac: 125, linux: "Down", win: 0x28 },
        "home" => KeySpec { mac: 115, linux: "Home", win: 0x24 },
        "end" => KeySpec { mac: 119, linux: "End", win: 0x23 },
        "pageup" => KeySpec { mac: 116, linux: "Prior", win: 0x21 },
        "pagedown" => KeySpec { mac: 121, linux: "Next", win: 0x22 },
        "f1" => KeySpec { mac: 122, linux: "F1", win: 0x70 },
        "f2" => KeySpec { mac: 120, linux: "F2", win: 0x71 },
        "f3" => KeySpec { mac: 99, linux: "F3", win: 0x72 },
        "f4" => KeySpec { mac: 118, linux: "F4", win: 0x73 },
        "f5" => KeySpec { mac: 96, linux: "F5", win: 0x74 },
        "f6" => KeySpec { mac: 97, linux: "F6", win: 0x75 },
        "f7" => KeySpec { mac: 98, linux: "F7", win: 0x76 },
        "f8" => KeySpec { mac: 100, linux: "F8", win: 0x77 },
        "f9" => KeySpec { mac: 101, linux: "F9", win: 0x78 },
        "f10" => KeySpec { mac: 109, linux: "F10", win: 0x79 },
        "f11" => KeySpec { mac: 103, linux: "F11", win: 0x7A },
        "f12" => KeySpec { mac: 111, linux: "F12", win: 0x7B },
        _ => return None,
    };
    Some(s)
}

#[derive(Debug)]
enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug)]
enum Op {
    Key(String),
    Text(String),
    MouseMove { x: i64, y: i64 },
    Click { button: MouseButton, x: Option<i64>, y: Option<i64>, double: bool },
}

/// Input validation (pure function). type is required, one of four; key names are table-looked-up; coordinates must be non-negative.
fn validate(input: &Value) -> Result<Op, String> {
    let Some(t) = input.get("type").and_then(|t| t.as_str()) else {
        return Err("invalid input: type (string) required, one of key|text|mouse_move|mouse_click".into());
    };
    match t {
        "key" => {
            let Some(k) = input.get("key").and_then(|k| k.as_str()) else {
                return Err("invalid input: key (string) required for type=key".into());
            };
            if key_spec(k).is_none() {
                return Err(format!(
                    "invalid input: unknown key name {} (see docs/capability.md key table)",
                    k
                ));
            }
            Ok(Op::Key(k.to_string()))
        }
        "text" => {
            let Some(s) = input.get("text").and_then(|s| s.as_str()) else {
                return Err("invalid input: text (string) required for type=text".into());
            };
            if s.is_empty() {
                return Err("invalid input: text must be non-empty".into());
            }
            if s.chars().count() > TEXT_MAX {
                return Err(format!("invalid input: text must be ≤{} chars", TEXT_MAX));
            }
            Ok(Op::Text(s.to_string()))
        }
        "mouse_move" => {
            let (Some(x), Some(y)) = (
                input.get("x").and_then(|v| v.as_i64()),
                input.get("y").and_then(|v| v.as_i64()),
            ) else {
                return Err("invalid input: x and y (integers) required for type=mouse_move".into());
            };
            if x < 0 || y < 0 {
                return Err("invalid input: x and y must be ≥0".into());
            }
            Ok(Op::MouseMove { x, y })
        }
        "mouse_click" => {
            let button = match input.get("button").and_then(|b| b.as_str()).unwrap_or("left") {
                "left" => MouseButton::Left,
                "middle" => MouseButton::Middle,
                "right" => MouseButton::Right,
                other => {
                    return Err(format!("invalid input: button must be left|middle|right, got {}", other))
                }
            };
            let x = input.get("x").and_then(|v| v.as_i64());
            let y = input.get("y").and_then(|v| v.as_i64());
            match (x, y) {
                (None, Some(_)) | (Some(_), None) => {
                    return Err("invalid input: x and y come together".into())
                }
                (Some(x), Some(y)) if x < 0 || y < 0 => {
                    return Err("invalid input: x and y must be ≥0".into())
                }
                _ => {}
            }
            let double = input.get("double").and_then(|d| d.as_bool()).unwrap_or(false);
            Ok(Op::Click { button, x, y, double })
        }
        other => Err(format!(
            "invalid input: type must be key|text|mouse_move|mouse_click, got {}",
            other
        )),
    }
}

/// macOS CoreGraphics / ApplicationServices FFI.
#[cfg(target_os = "macos")]
mod cg {
    use std::os::raw::{c_int, c_void};

    #[repr(C)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventCreateKeyboardEvent(
            source: *const c_void,
            virtual_key: u16,
            key_down: bool,
        ) -> *mut c_void;
        pub fn CGEventCreateMouseEvent(
            source: *const c_void,
            mouse_type: c_int,
            position: CGPoint,
        ) -> *mut c_void;
        pub fn CGEventCreate(source: *const c_void) -> *mut c_void;
        pub fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
        pub fn CGEventKeyboardSetUnicodeString(event: *mut c_void, length: usize, string: *const u16);
        pub fn CGEventPost(tap: c_int, event: *mut c_void);
        pub fn CFRelease(cf: *mut c_void);
    }

    // AXIsProcessTrusted is not redeclared here: reuse osperm::ffi::ax_trusted
    // (ApplicationServices, a single extern site, preventing clashing_extern_declarations)

    pub const HID_TAP: c_int = 0; // kCGHIDEventTap
    pub const MOUSE_MOVED: c_int = 5; // kCGEventMouseMoved
    pub const LEFT_DOWN: c_int = 1;
    pub const LEFT_UP: c_int = 2;
    pub const RIGHT_DOWN: c_int = 3;
    pub const RIGHT_UP: c_int = 4;
    pub const OTHER_DOWN: c_int = 25; // middle button maps to the other event
    pub const OTHER_UP: c_int = 26;

    /// Post one key event.
    pub(crate) unsafe fn post_key(vk: u16, key_down: bool) {
        let ev = CGEventCreateKeyboardEvent(std::ptr::null(), vk, key_down);
        if !ev.is_null() {
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }

    /// Text events: at most 20 UTF-16 code units per call (an old Carbon limitation); send in chunks.
    /// Both down/up carry the same string — some apps read the text from the up event.
    pub(crate) unsafe fn post_text_chunk(units: &[u16]) {
        for key_down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null(), 0, key_down);
            if ev.is_null() {
                continue;
            }
            CGEventKeyboardSetUnicodeString(ev, units.len(), units.as_ptr());
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }

    pub(crate) unsafe fn post_mouse(mouse_type: c_int, x: f64, y: f64) {
        let ev = CGEventCreateMouseEvent(std::ptr::null(), mouse_type, CGPoint { x, y });
        if !ev.is_null() {
            CGEventPost(HID_TAP, ev);
            CFRelease(ev);
        }
    }

    /// Current mouse position (used for a click without coordinates).
    pub(crate) unsafe fn current_pos() -> (f64, f64) {
        let ev = CGEventCreate(std::ptr::null());
        if ev.is_null() {
            return (0.0, 0.0);
        }
        let p = CGEventGetLocation(ev);
        CFRelease(ev);
        (p.x, p.y)
    }
}

#[cfg(target_os = "macos")]
fn dispatch_mac(op: &Op) -> Result<Value, String> {
    // Up-front probe: without Accessibility the host process silently drops CGEventPost events; report honestly first
    if !super::osperm::ffi::ax_trusted() {
        return Err(
            "input requires Accessibility permission for OpenCapX (System Settings → Privacy & Security → Accessibility)"
                .into(),
        );
    }
    unsafe {
        match op {
            Op::Key(name) => {
                let k = key_spec(name).unwrap();
                cg::post_key(k.mac, true);
                std::thread::sleep(EVENT_GAP);
                cg::post_key(k.mac, false);
            }
            Op::Text(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                for chunk in units.chunks(20) {
                    cg::post_text_chunk(chunk);
                    std::thread::sleep(EVENT_GAP);
                }
            }
            Op::MouseMove { x, y } => {
                cg::post_mouse(cg::MOUSE_MOVED, *x as f64, *y as f64);
            }
            Op::Click { button, x, y, double } => {
                let (cx, cy) = match (x, y) {
                    (Some(x), Some(y)) => (*x as f64, *y as f64),
                    _ => cg::current_pos(),
                };
                let (down, up) = match button {
                    MouseButton::Left => (cg::LEFT_DOWN, cg::LEFT_UP),
                    MouseButton::Right => (cg::RIGHT_DOWN, cg::RIGHT_UP),
                    MouseButton::Middle => (cg::OTHER_DOWN, cg::OTHER_UP),
                };
                let rounds = if *double { 2 } else { 1 };
                for _ in 0..rounds {
                    cg::post_mouse(down, cx, cy);
                    std::thread::sleep(EVENT_GAP);
                    cg::post_mouse(up, cx, cy);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
    Ok(json!({ "ok": true }))
}

/// Linux: xdotool. Arguments go through argv, not a shell, so there is no injection surface.
#[cfg(target_os = "linux")]
fn dispatch_linux(op: &Op) -> Result<Value, String> {
    fn run(cmd: std::process::Command) -> Result<(), String> {
        let out = super::appctl::run_with_timeout(cmd, std::time::Duration::from_secs(10))
            .map_err(|e| format!("xdotool failed: {}", e))?;
        if !out.status.success() {
            return Err(format!(
                "xdotool exited {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).chars().take(200).collect::<String>()
            ));
        }
        Ok(())
    }
    match op {
        Op::Key(name) => run({
            let mut c = std::process::Command::new("xdotool");
            c.args(["key", key_spec(name).unwrap().linux]);
            c
        })
        .map_err(|e| format!("{} (install xdotool)", e))?,
        Op::Text(text) => run({
            let mut c = std::process::Command::new("xdotool");
            c.args(["type", "--", text]);
            c
        })
        .map_err(|e| format!("{} (install xdotool)", e))?,
        Op::MouseMove { x, y } => run({
            let mut c = std::process::Command::new("xdotool");
            c.args(["mousemove", &x.to_string(), &y.to_string()]);
            c
        })
        .map_err(|e| format!("{} (install xdotool)", e))?,
        Op::Click { button, x, y, double } => {
            if let (Some(x), Some(y)) = (x, y) {
                let mut c = std::process::Command::new("xdotool");
                c.args(["mousemove", &x.to_string(), &y.to_string()]);
                run(c).map_err(|e| format!("{} (install xdotool)", e))?;
            }
            let b = match button {
                MouseButton::Left => "1",
                MouseButton::Middle => "2",
                MouseButton::Right => "3",
            };
            let mut c = std::process::Command::new("xdotool");
            c.arg("click");
            if *double {
                c.args(["--repeat", "2", "--delay", "100"]);
            }
            c.arg(b);
            run(c).map_err(|e| format!("{} (install xdotool)", e))?;
        }
    }
    Ok(json!({ "ok": true }))
}

/// Windows:user32 SendInput.
#[cfg(target_os = "windows")]
mod win {
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MOUSEINPUT {
        dx: i32,
        dy: i32,
        mouse_data: u32,
        flags: u32,
        time: u32,
        extra: usize,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KEYBDINPUT {
        vk: u16,
        scan: u16,
        flags: u32,
        time: u32,
        extra: usize,
    }

    pub const INPUT_MOUSE: u32 = 0;
    pub const INPUT_KEYBOARD: u32 = 1;
    // MOUSEEVENTF_*
    pub const MF_MOVE: u32 = 0x0001;
    pub const MF_LEFTDOWN: u32 = 0x0002;
    pub const MF_LEFTUP: u32 = 0x0004;
    pub const MF_RIGHTDOWN: u32 = 0x0008;
    pub const MF_RIGHTUP: u32 = 0x0010;
    pub const MF_MIDDLEDOWN: u32 = 0x0020;
    pub const MF_MIDDLEUP: u32 = 0x0040;
    pub const MF_ABSOLUTE: u32 = 0x8000;
    // KEYEVENTF_*
    pub const KF_KEYUP: u32 = 0x0002;
    pub const KF_UNICODE: u32 = 0x0004;

    #[link(name = "user32")]
    extern "system" {
        fn SendInput(count: u32, inputs: *const Input, size: i32) -> u32;
        fn GetSystemMetrics(index: i32) -> i32;
    }

    /// C INPUT { DWORD type; union { MOUSEINPUT; KEYBDINPUT } }.
    /// The union payload is at most 32 bytes, carried in [u64;4]: zero-initialized then copied in byte-wise.
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct Input {
        kind: u32,
        payload: [u64; 4],
    }

    impl Input {
        fn from_bytes(kind: u32, bytes: &[u8]) -> Input {
            let mut payload = [0u64; 4];
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    payload.as_mut_ptr() as *mut u8,
                    bytes.len(),
                );
            }
            Input { kind, payload }
        }
        fn mouse(mi: MOUSEINPUT) -> Input {
            Input::from_bytes(INPUT_MOUSE, unsafe {
                std::slice::from_raw_parts(
                    &mi as *const MOUSEINPUT as *const u8,
                    std::mem::size_of::<MOUSEINPUT>(),
                )
            })
        }
        fn key(ki: KEYBDINPUT) -> Input {
            Input::from_bytes(INPUT_KEYBOARD, unsafe {
                std::slice::from_raw_parts(
                    &ki as *const KEYBDINPUT as *const u8,
                    std::mem::size_of::<KEYBDINPUT>(),
                )
            })
        }
        pub fn key_down(vk: u16) -> Input {
            Input::key(KEYBDINPUT { vk, scan: 0, flags: 0, time: 0, extra: 0 })
        }
        pub fn key_up(vk: u16) -> Input {
            Input::key(KEYBDINPUT { vk, scan: 0, flags: KF_KEYUP, time: 0, extra: 0 })
        }
        pub fn unicode_down(cp: u16) -> Input {
            Input::key(KEYBDINPUT { vk: 0, scan: cp, flags: KF_UNICODE, time: 0, extra: 0 })
        }
        pub fn unicode_up(cp: u16) -> Input {
            Input::key(KEYBDINPUT { vk: 0, scan: cp, flags: KF_UNICODE | KF_KEYUP, time: 0, extra: 0 })
        }
        /// Normalize absolute coordinates to 0..=65535 (SendInput's full-screen absolute coordinate system).
        pub fn absolute_move(x: i64, y: i64) -> Input {
            let (w, h) = screen_size();
            let (x, y) = (x.min(w.saturating_sub(1)).max(0), y.min(h.saturating_sub(1)).max(0));
            let nx = if w > 1 { (x * 65535) / (w - 1) } else { 0 };
            let ny = if h > 1 { (y * 65535) / (h - 1) } else { 0 };
            Input::mouse(MOUSEINPUT {
                dx: nx as i32,
                dy: ny as i32,
                mouse_data: 0,
                flags: MF_MOVE | MF_ABSOLUTE,
                time: 0,
                extra: 0,
            })
        }
        pub fn mouse_btn(flags: u32) -> Input {
            Input::mouse(MOUSEINPUT { dx: 0, dy: 0, mouse_data: 0, flags, time: 0, extra: 0 })
        }
    }

    fn screen_size() -> (i64, i64) {
        unsafe { (GetSystemMetrics(0) as i64, GetSystemMetrics(1) as i64) }
    }

    /// Send a batch of events; SendInput returns the number actually injected, fewer than requested means rejected (UIPI etc.).
    pub fn send(inputs: &[Input]) -> bool {
        unsafe {
            SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<Input>() as i32)
                == inputs.len() as u32
        }
    }
}

#[cfg(target_os = "windows")]
fn dispatch_win(op: &Op) -> Result<Value, String> {
    use win::*;
    match op {
        Op::Key(name) => {
            let vk = key_spec(name).unwrap().win;
            if !send(&[Input::key_down(vk)]) || !send(&[Input::key_up(vk)]) {
                return Err("SendInput rejected (elevated window?)".into());
            }
            std::thread::sleep(EVENT_GAP);
        }
        Op::Text(text) => {
            for cp in text.encode_utf16() {
                if !send(&[Input::unicode_down(cp)]) || !send(&[Input::unicode_up(cp)]) {
                    return Err("SendInput rejected (elevated window?)".into());
                }
                std::thread::sleep(EVENT_GAP);
            }
        }
        Op::MouseMove { x, y } => {
            if !send(&[Input::absolute_move(*x, *y)]) {
                return Err("SendInput rejected".into());
            }
        }
        Op::Click { button, x, y, double } => {
            if let (Some(x), Some(y)) = (x, y) {
                if !send(&[Input::absolute_move(*x, *y)]) {
                    return Err("SendInput rejected".into());
                }
                std::thread::sleep(EVENT_GAP);
            }
            let (down, up) = match button {
                MouseButton::Left => (MF_LEFTDOWN, MF_LEFTUP),
                MouseButton::Right => (MF_RIGHTDOWN, MF_RIGHTUP),
                MouseButton::Middle => (MF_MIDDLEDOWN, MF_MIDDLEUP),
            };
            let rounds = if *double { 2 } else { 1 };
            for _ in 0..rounds {
                if !send(&[Input::mouse_btn(down)]) || !send(&[Input::mouse_btn(up)]) {
                    return Err("SendInput rejected (elevated window?)".into());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(json!({ "ok": true }))
}

/// Tap one Virtual-Key (Windows). Reused by v1.4 media.playback media keys.
#[cfg(target_os = "windows")]
pub(crate) fn tap_key(vk: u16) -> bool {
    win::send(&[win::Input::key_down(vk)]) && win::send(&[win::Input::key_up(vk)])
}

/// input.send builtin implementation.
pub fn send(input: &Value) -> Result<Value, String> {
    let op = validate(input)?;
    #[cfg(target_os = "macos")]
    return dispatch_mac(&op);
    #[cfg(target_os = "linux")]
    return dispatch_linux(&op);
    #[cfg(target_os = "windows")]
    return dispatch_win(&op);
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = op;
        Err("input.send builtin not supported on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KEYS: &[&str] = &[
        "return", "tab", "esc", "delete", "forward_delete", "space", "left", "right", "up",
        "down", "home", "end", "pageup", "pagedown", "f1", "f2", "f3", "f4", "f5", "f6", "f7",
        "f8", "f9", "f10", "f11", "f12",
    ];

    #[test]
    fn key_table_covers_documented_names() {
        for name in ALL_KEYS {
            assert!(key_spec(name).is_some(), "{} missing", name);
        }
        // Modifier combinations are not done in v1
        assert!(key_spec("cmd").is_none());
        assert!(key_spec("ctrl").is_none());
        assert!(key_spec("nope").is_none());
        // Spot-check the per-platform key codes
        let k = key_spec("return").unwrap();
        assert_eq!(k.mac, 36);
        assert_eq!(k.linux, "Return");
        assert_eq!(k.win, 0x0D);
    }

    #[test]
    fn validates_op_branches() {
        assert!(validate(&json!({ "type": "key", "key": "return" })).is_ok());
        assert!(validate(&json!({ "type": "text", "text": "hi" })).is_ok());
        assert!(validate(&json!({ "type": "mouse_move", "x": 1, "y": 2 })).is_ok());
        assert!(validate(&json!({ "type": "mouse_click", "button": "right", "double": true })).is_ok());
        // type missing / unknown
        assert!(validate(&json!({})).unwrap_err().contains("type"));
        assert!(validate(&json!({ "type": "scroll" })).unwrap_err().contains("key|text|mouse_move|mouse_click"));
        // key missing / unknown
        assert!(validate(&json!({ "type": "key" })).unwrap_err().contains("key"));
        assert!(validate(&json!({ "type": "key", "key": "meta" })).unwrap_err().contains("unknown key"));
        // text empty / over-long
        assert!(validate(&json!({ "type": "text", "text": "" })).unwrap_err().contains("non-empty"));
        assert!(validate(&json!({ "type": "text", "text": "x".repeat(501) })).unwrap_err().contains("500"));
        // Coordinates missing / negative / only one of the pair
        assert!(validate(&json!({ "type": "mouse_move", "x": 1 })).unwrap_err().contains("x and y"));
        assert!(validate(&json!({ "type": "mouse_move", "x": -1, "y": 1 })).unwrap_err().contains("≥0"));
        assert!(validate(&json!({ "type": "mouse_click", "x": 1 })).unwrap_err().contains("together"));
        // button unknown
        assert!(validate(&json!({ "type": "mouse_click", "button": "side" })).unwrap_err().contains("left|middle|right"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "sends real keystrokes; focus a text field first, run with --ignored manually"]
    fn send_text_manual() {
        send(&json!({ "type": "text", "text": "OPENCAPX-42" })).unwrap();
    }
}
