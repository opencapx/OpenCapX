//! Rust-side copy (for native UI).
//!
//! Frontend copy lives in `src/locales/*.json` (data-driven; adding a language needs no code change). This only covers
//! **the part that cannot reach the frontend i18n**: tray menu / icon tooltip / system notifications — they are
//! needed when there is no WebView, or even before the window is created.
//!
//! Because the volume is tiny (a dozen or so strings), use struct fields instead of key lookup: a typo errors at compile time.
//! Adding a language = adding one `Strings` constant + one `Lang` branch.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhHans,
    Vi,
}

/// The locale value in settings → language. Unknown values always fall back to English.
pub fn from_locale(locale: &str) -> Lang {
    match locale.trim() {
        "zh-Hans" => Lang::ZhHans,
        "vi" => Lang::Vi,
        _ => Lang::En,
    }
}

/// Native copy. Menu items use Title Case (aligned with macOS menu conventions);
/// notification bodies use sentence case (a notification is not a menu; Title Case would look like a heading).
pub struct Strings {
    pub show_pet: &'static str,
    pub show_bubble: &'static str,
    pub open_settings: &'static str,
    pub clear_finished: &'static str,
    pub quit: &'static str,
    pub no_active_agents: &'static str,
    /// `{}` = count
    pub waiting_for_you: &'static str,
    pub working: &'static str,
    pub finished: &'static str,
    pub idle: &'static str,
    /// The state word when a session row has no message
    pub state_working: &'static str,
    pub state_waiting: &'static str,
    pub state_done: &'static str,
    pub state_idle: &'static str,
    /// `{}` = agent, `{}` = detail
    pub notify_waiting: &'static str,
    /// `{}` = agent
    pub notify_done: &'static str,
    /// Tray item: install the `opencapx` command into PATH (shown only while it is missing).
    pub install_cli: &'static str,
}

pub const EN: Strings = Strings {
    show_pet: "Show Pet",
    show_bubble: "Show Bubble",
    open_settings: "Open Settings",
    clear_finished: "Clear Finished",
    quit: "Quit OpenCapX",
    no_active_agents: "No Active Agents",
    waiting_for_you: "{} Waiting for You",
    working: "{} Working",
    finished: "{} Finished",
    idle: "{} Idle",
    state_working: "Working",
    state_waiting: "Waiting for You",
    state_done: "Finished",
    state_idle: "Idle",
    notify_waiting: "{} needs input — {}",
    notify_done: "{} finished",
    install_cli: "Install opencapx Command",
};

pub const ZH_HANS: Strings = Strings {
    show_pet: "显示宠物",
    show_bubble: "显示气泡",
    open_settings: "打开设置",
    clear_finished: "清除已完成",
    quit: "退出 OpenCapX",
    no_active_agents: "没有活跃的 Agent",
    waiting_for_you: "{} 个在等你",
    working: "{} 个在干活",
    finished: "{} 个已完成",
    idle: "{} 个空闲",
    state_working: "干活中",
    state_waiting: "等你输入",
    state_done: "已完成",
    state_idle: "空闲",
    notify_waiting: "{} 需要你输入 — {}",
    notify_done: "{} 已完成",
    install_cli: "安装 opencapx 命令",
};

pub const VI: Strings = Strings {
    show_pet: "Hiện thú cưng",
    show_bubble: "Hiện bong bóng",
    open_settings: "Mở cài đặt",
    clear_finished: "Xóa mục đã xong",
    quit: "Thoát OpenCapX",
    no_active_agents: "Không có agent nào hoạt động",
    waiting_for_you: "{} đang chờ bạn",
    working: "{} đang làm việc",
    finished: "{} đã xong",
    idle: "{} đang rảnh",
    state_working: "Đang làm",
    state_waiting: "Chờ bạn nhập",
    state_done: "Đã xong",
    state_idle: "Rảnh",
    notify_waiting: "{} cần bạn nhập — {}",
    notify_done: "{} đã xong",
    install_cli: "Cài lệnh opencapx",
};

pub fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::En => &EN,
        Lang::ZhHans => &ZH_HANS,
        Lang::Vi => &VI,
    }
}

/// Single-line text convergence: take only the first non-empty line + char-based truncation with an ellipsis.
/// Native UI (menu rows, notification bodies, tooltips) all use it, to avoid long text blowing out the layout.
pub fn truncate(s: &str, max: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        return line.to_string();
    }
    let mut out: String = line.chars().take(max).collect();
    out.push('…');
    out
}

/// Replace `{}` with the arguments in order (native copy has only 0/1/2 placeholders).
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    for arg in args {
        match rest.split_once("{}") {
            Some((head, tail)) => {
                out.push_str(head);
                out.push_str(arg);
                rest = tail;
            }
            None => break,
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(&str, &Strings); 3] = [("en", &EN), ("zh-Hans", &ZH_HANS), ("vi", &VI)];

    #[test]
    fn locale_mapping_falls_back_to_english() {
        assert_eq!(from_locale("zh-Hans"), Lang::ZhHans);
        assert_eq!(from_locale("vi"), Lang::Vi);
        assert_eq!(from_locale("en"), Lang::En);
        assert_eq!(
            from_locale("zh-Hant"),
            Lang::En,
            "a not-yet-translated language falls back to English"
        );
        assert_eq!(from_locale(""), Lang::En);
    }

    #[test]
    fn every_language_is_complete_and_uses_the_same_placeholders() {
        for (name, s) in ALL {
            let fields = [
                s.show_pet,
                s.show_bubble,
                s.open_settings,
                s.clear_finished,
                s.quit,
                s.no_active_agents,
                s.waiting_for_you,
                s.working,
                s.finished,
                s.idle,
                s.state_working,
                s.state_waiting,
                s.state_done,
                s.state_idle,
                s.notify_waiting,
                s.notify_done,
            ];
            for v in fields {
                assert!(!v.trim().is_empty(), "{name} has empty copy");
            }
            // The placeholder count must match English, otherwise fill() would get too few arguments
            for (a, b) in [
                (s.waiting_for_you, EN.waiting_for_you),
                (s.working, EN.working),
                (s.finished, EN.finished),
                (s.idle, EN.idle),
                (s.notify_waiting, EN.notify_waiting),
                (s.notify_done, EN.notify_done),
            ] {
                assert_eq!(
                    a.matches("{}").count(),
                    b.matches("{}").count(),
                    "{name}: placeholder counts differ → {a}"
                );
            }
        }
    }

    #[test]
    fn english_menu_items_use_title_case() {
        // macOS menu convention: capitalize the first letter of every content word (prepositions not tested)
        for item in [
            EN.show_pet,
            EN.show_bubble,
            EN.open_settings,
            EN.clear_finished,
            EN.no_active_agents,
        ] {
            let words: Vec<&str> = item.split(' ').collect();
            assert!(
                words
                    .iter()
                    .all(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(true)),
                "not title case: {item}"
            );
        }
    }

    #[test]
    fn truncate_keeps_first_line_and_bounds_length() {
        assert_eq!(truncate("  hello  \nworld", 20), "hello");
        assert_eq!(truncate("abcdef", 3), "abc…");
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn fill_replaces_in_order_and_tolerates_missing_args() {
        assert_eq!(fill("{} waiting for you", &["2"]), "2 waiting for you");
        assert_eq!(
            fill("{} needs input — {}", &["Gemini CLI", "perm"]),
            "Gemini CLI needs input — perm"
        );
        // With too few arguments the remaining template is preserved (no panic)
        assert_eq!(fill("{} · {}", &["a"]), "a · {}");
        assert_eq!(fill("no placeholder", &["a"]), "no placeholder");
    }
}
