//! Tray presentation logic (pure functions + icon composition).
//!
//! The tray menu is a **native menu** with no CSS/animation surface — so there are
//! three things we can do:
//! 1. **Icon state**: the menu-bar icon itself echoes "is any agent waiting for me"
//!    (a colored dot), visible without opening the menu;
//! 2. **Copy hierarchy**: the summary line only states what needs action (waiting
//!    first); the empty state adds no noise;
//! 3. **Single-line scanability**: each line = state glyph + agent + (project) + what
//!    it is doing, with bounded length so a long path does not stretch the menu width.
//!
//! The tooltip is the other half of "visible without opening": a waiting agent is written straight in.

use crate::core::agent::{AgentState, Session};
use crate::core::i18n::truncate;

/// The status dot on the menu-bar icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayBadge {
    /// No active state worth showing: the icon keeps its original template form.
    None,
    Working,
    Waiting,
}

/// Waiting first: someone waiting for me > someone working > show nothing.
pub fn badge_for(working: usize, waiting: usize) -> TrayBadge {
    if waiting > 0 {
        TrayBadge::Waiting
    } else if working > 0 {
        TrayBadge::Working
    } else {
        TrayBadge::None
    }
}

/// State glyph. Uses monospaced filled/hollow circles to avoid narrow glyphs like `✓` making the column look crooked.
pub fn state_glyph(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "●",
        AgentState::Waiting => "◐",
        AgentState::Done => "✓",
        AgentState::Idle => "○",
    }
}

/// Short state word (the fallback text when a session row has no message).
pub fn state_word(s: &crate::core::i18n::Strings, state: AgentState) -> &'static str {
    match state {
        AgentState::Working => s.state_working,
        AgentState::Waiting => s.state_waiting,
        AgentState::Done => s.state_done,
        AgentState::Idle => s.state_idle,
    }
}

/// Summary line: only states what needs action, with waiting first.
/// Gives no zero-value noise when everything is working; also skips the summary line when only finished sessions exist.
pub fn summary_text(
    s: &crate::core::i18n::Strings,
    working: usize,
    waiting: usize,
    done: usize,
    total: usize,
) -> String {
    use crate::core::i18n::fill;
    if waiting > 0 {
        // Don't write "0 working": a zero is just noise; readers only care about "who is waiting for me"
        let head = fill(s.waiting_for_you, &[&waiting.to_string()]);
        if working > 0 {
            format!("{head} · {}", fill(s.working, &[&working.to_string()]))
        } else {
            head
        }
    } else if working > 0 {
        fill(s.working, &[&working.to_string()])
    } else if done > 0 {
        fill(s.finished, &[&done.to_string()])
    } else if total > 0 {
        fill(s.idle, &[&total.to_string()])
    } else {
        String::new()
    }
}

/// Single-line session text: `{glyph} {product name}` + optional `· {project}` + optional `· {what it's doing}`.
/// agent uses the display name (Claude Code rather than claude); the project is omitted
/// when it is unknown (empty info would take up width); with no message it falls back to
/// the state word, avoiding a half-line like `● claude`.
pub fn session_label(s: &crate::core::i18n::Strings, session: &Session) -> String {
    let mut out = base_label(session);
    let project = session.project.trim();
    if !project.is_empty() && !project.eq_ignore_ascii_case("unknown") {
        out.push_str(" · ");
        out.push_str(project);
    }
    out.push_str(&label_tail(s, session));
    out
}

/// A row inside a submenu: the parent already shows the project name, so it is not repeated here (the message is therefore truncated less).
pub fn session_label_in_project(s: &crate::core::i18n::Strings, session: &Session) -> String {
    let mut out = base_label(session);
    out.push_str(&label_tail(s, session));
    out
}

fn base_label(session: &Session) -> String {
    format!(
        "{} {}",
        state_glyph(session.state),
        crate::hooks::display_name(&session.agent)
    )
}

fn label_tail(s: &crate::core::i18n::Strings, session: &Session) -> String {
    let msg = truncate(&session.message, 34);
    if msg.is_empty() {
        format!(" · {}", state_word(s, session.state))
    } else {
        format!(" · {msg}")
    }
}

/// Submenu: one group per project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraySection {
    pub header: String,
    pub rows: Vec<String>,
}

/// Group header: `{project} · {branch} · {N waiting for you} · {M working}`, with zero values omitted.
/// When both waiting and working are 0 it degrades to `{N finished}` — otherwise you get
/// a hollow parent showing only the project name, and you cannot tell what is inside
/// without expanding it.
pub fn project_header(
    s: &crate::core::i18n::Strings,
    project: &str,
    branch: Option<&str>,
    waiting: usize,
    working: usize,
    done: usize,
) -> String {
    use crate::core::i18n::fill;
    let mut out = if project.trim().is_empty() {
        "unknown".to_string()
    } else {
        project.trim().to_string()
    };
    if let Some(b) = branch.map(str::trim).filter(|b| !b.is_empty()) {
        out.push_str(" · ");
        out.push_str(b);
    }
    if waiting > 0 {
        out.push_str(" · ");
        out.push_str(&fill(s.waiting_for_you, &[&waiting.to_string()]));
    }
    if working > 0 {
        out.push_str(" · ");
        out.push_str(&fill(s.working, &[&working.to_string()]));
    }
    if waiting == 0 && working == 0 && done > 0 {
        out.push_str(" · ");
        out.push_str(&fill(s.finished, &[&done.to_string()]));
    }
    out
}

/// Split the sessions (already ordered by `agent::sort_sessions`) into submenu groups plus
/// top-level uncategorized rows.
///
/// The group key is the full `cwd` (distinguishing same-named projects); sessions without
/// a `cwd` (old data, `evt-*`) stay at the top level rather than being forced into an
/// "uncategorized" submenu.
/// Groups follow the order in which their first session appears — the sort already applied
/// priority, so the first group to appear is the highest-priority group, the same trick the
/// bubble uses.
/// The branch is looked up by the caller (`cwd → branch`); no I/O is left here.
pub fn sections(
    s: &crate::core::i18n::Strings,
    sessions: &[Session],
    branch_of: &dyn Fn(&str) -> Option<String>,
) -> (Vec<TraySection>, Vec<String>) {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<&Session>> = std::collections::HashMap::new();
    let mut ungrouped: Vec<String> = Vec::new();
    for session in sessions {
        let cwd = session.cwd.trim();
        if cwd.is_empty() {
            ungrouped.push(session_label(s, session));
            continue;
        }
        if !groups.contains_key(cwd) {
            order.push(cwd.to_string());
            groups.insert(cwd.to_string(), Vec::new());
        }
        if let Some(list) = groups.get_mut(cwd) {
            list.push(session);
        }
    }
    let count = |list: &[&Session], state: AgentState| list.iter().filter(|x| x.state == state).count();
    let sections = order
        .into_iter()
        .map(|cwd| {
            let list = groups.get(&cwd).cloned().unwrap_or_default();
            let project = list.first().map(|x| x.project.clone()).unwrap_or_default();
            let branch = branch_of(&cwd);
            TraySection {
                header: project_header(
                    s,
                    &project,
                    branch.as_deref(),
                    count(&list, AgentState::Waiting),
                    count(&list, AgentState::Working),
                    count(&list, AgentState::Done),
                ),
                rows: list.iter().map(|x| session_label_in_project(s, x)).collect(),
            }
        })
        .collect();
    (sections, ungrouped)
}

/// tooltip: a summary visible without opening the menu. When someone is waiting, write in "who and what for".
pub fn tooltip_text(s: &crate::core::i18n::Strings, sessions: &[Session]) -> String {
    use crate::core::i18n::fill;
    let working = sessions
        .iter()
        .filter(|s| s.state == AgentState::Working)
        .count();
    let waiting: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.state == AgentState::Waiting)
        .collect();
    if sessions.is_empty() {
        return "OpenCapX".into();
    }
    let mut out = String::from("OpenCapX · ");
    let mut parts: Vec<String> = Vec::new();
    if !waiting.is_empty() {
        parts.push(fill(s.waiting_for_you, &[&waiting.len().to_string()]));
    } else if working > 0 {
        parts.push(fill(s.working, &[&working.to_string()]));
    } else {
        parts.push(fill(s.finished, &[&sessions.len().to_string()]));
    }
    out.push_str(&parts.join(" · "));
    if let Some(first) = waiting.first() {
        let who = crate::hooks::display_name(&first.agent);
        let what = truncate(&first.message, 40);
        out.push_str(" · ");
        if what.is_empty() {
            out.push_str(&format!("{who} · {}", s.state_waiting));
        } else {
            out.push_str(&format!("{who}: {what}"));
        }
    }
    out
}

/// Draw the dot badge in the top-right corner of the icon.
///
/// Without an outer ring the badge loses contrast on one side of a light/dark menu bar
/// (same reasoning as image outlining), so beyond the fill it presses down a ring of
/// semi-transparent black.
pub fn compose_icon(base: &[u8], w: u32, h: u32, badge: TrayBadge) -> Vec<u8> {
    let mut out = base.to_vec();
    let (cr, cg, cb) = match badge {
        TrayBadge::None => return out,
        TrayBadge::Working => (48u8, 209u8, 88u8),   // system green
        TrayBadge::Waiting => (255u8, 159u8, 10u8),  // system orange
    };
    if w == 0 || h == 0 {
        return out;
    }
    // Geometry is based on 32px; other sizes scale proportionally so different icon assets look consistent
    let unit = (w.min(h) as f32) / 32.0;
    let cx = w as f32 - 8.0 * unit;
    let cy = 8.0 * unit;
    let r = 6.5 * unit;
    let ring = 1.5 * unit;
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d > r + ring {
                continue;
            }
            let idx = ((y * w + x) * 4) as usize;
            if idx + 3 >= out.len() {
                continue;
            }
            if d <= r {
                blend(&mut out[idx..idx + 4], [cr, cg, cb, 255]);
            } else {
                blend(&mut out[idx..idx + 4], [0, 0, 0, 90]);
            }
        }
    }
    out
}

/// source-over blend (src over dst), pure integer arithmetic.
fn blend(dst: &mut [u8], src: [u8; 4]) {
    let sa = src[3] as u32;
    let da = dst[3] as u32;
    let out_a = sa + da * (255 - sa) / 255;
    if out_a == 0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for i in 0..3 {
        let v = (src[i] as u32 * sa + dst[i] as u32 * da * (255 - sa) / 255) / out_a;
        dst[i] = v.min(255) as u8;
    }
    dst[3] = out_a.min(255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, agent: &str, project: &str, state: AgentState, msg: &str) -> Session {
        Session {
            id: id.into(),
            agent: agent.into(),
            project: project.into(),
            cwd: String::new(),
            message: msg.into(),
            state,
            started_at: 0,
            updated_at: 0,
            model: String::new(),
            speech: String::new(),
            choices: None,
            answered: None,
        }
    }

    /// A session with a cwd (used for grouping).
    fn sess_cwd(id: &str, agent: &str, project: &str, cwd: &str, state: AgentState, msg: &str) -> Session {
        let mut s = sess(id, agent, project, state, msg);
        s.cwd = cwd.into();
        s
    }

    fn no_branch(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn sections_group_by_project_and_keep_waiting_first() {
        let en = &crate::core::i18n::EN;
        // Already ordered by sort_sessions: waiting first
        let sessions = vec![
            sess_cwd("a1", "gemini", "Alpha", "/w/alpha", AgentState::Waiting, "needs permission"),
            sess_cwd("a2", "claude", "Alpha", "/w/alpha", AgentState::Working, "Edit a.ts"),
            sess_cwd("b1", "codex", "Beta", "/w/beta", AgentState::Working, "cargo test"),
        ];
        let (sections, ungrouped) = sections(en, &sessions, &no_branch);
        assert!(ungrouped.is_empty());
        assert_eq!(sections.len(), 2);
        // Group order = the order in which each group's first session appears (so waiting-first holds automatically)
        assert!(sections[0].header.starts_with("Alpha"), "{}", sections[0].header);
        assert!(sections[1].header.starts_with("Beta"), "{}", sections[1].header);
        assert_eq!(sections[0].rows.len(), 2);
        // Rows inside a group no longer repeat the project name
        assert!(!sections[0].rows[0].contains("Alpha"), "{}", sections[0].rows[0]);
        assert!(sections[0].rows[0].contains("Gemini CLI"), "{}", sections[0].rows[0]);
    }

    #[test]
    fn section_header_shows_branch_and_omits_zero_counts() {
        let en = &crate::core::i18n::EN;
        let sessions = vec![
            sess_cwd("a1", "claude", "Alpha", "/w/alpha", AgentState::Waiting, "needs permission"),
            sess_cwd("a2", "claude", "Alpha", "/w/alpha", AgentState::Done, "done"),
        ];
        let branch = |_: &str| Some("main".to_string());
        let (sections, _) = sections(en, &sessions, &branch);
        let h = &sections[0].header;
        assert_eq!(h, "Alpha · main · 1 Waiting for You");
        assert!(!h.contains("Working"), "zero values should not appear: {h}");
    }

    #[test]
    fn section_header_falls_back_to_finished_when_nothing_runs() {
        let en = &crate::core::i18n::EN;
        let sessions = vec![
            sess_cwd("a1", "claude", "Alpha", "/w/alpha", AgentState::Done, "done"),
            sess_cwd("a2", "claude", "Alpha", "/w/alpha", AgentState::Done, "done"),
        ];
        let (sections, _) = sections(en, &sessions, &no_branch);
        // Otherwise the parent is only "Alpha", and you cannot tell what is inside without expanding
        assert_eq!(sections[0].header, "Alpha · 2 Finished");
    }

    #[test]
    fn single_session_project_still_becomes_a_section() {
        let en = &crate::core::i18n::EN;
        let sessions = vec![sess_cwd("b1", "cursor", "Beta", "/w/beta", AgentState::Working, "pnpm build")];
        let (sections, _) = sections(en, &sessions, &no_branch);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].rows.len(), 1);
        assert_eq!(sections[0].header, "Beta · 1 Working");
    }

    #[test]
    fn sessions_without_cwd_stay_top_level() {
        let en = &crate::core::i18n::EN;
        // One has a cwd (goes into a submenu), one does not (top level, keeping the original style including the project name)
        let sessions = vec![
            sess_cwd("a1", "claude", "Alpha", "/w/alpha", AgentState::Working, "Edit a.ts"),
            sess("x1", "claude", "Legacy", AgentState::Waiting, "needs permission"),
        ];
        let (sections, ungrouped) = sections(en, &sessions, &no_branch);
        assert_eq!(sections.len(), 1);
        assert_eq!(ungrouped.len(), 1);
        // The top-level row keeps its original full style (with the project name)
        assert!(ungrouped[0].contains("Legacy"), "{}", ungrouped[0]);
    }

    #[test]
    fn badge_prefers_waiting() {
        assert_eq!(badge_for(3, 1), TrayBadge::Waiting);
        assert_eq!(badge_for(2, 0), TrayBadge::Working);
        assert_eq!(badge_for(0, 0), TrayBadge::None);
    }

    #[test]
    fn summary_puts_waiting_first_and_drops_empty_noise() {
        let en = &crate::core::i18n::EN;
        assert_eq!(
            summary_text(en, 3, 1, 0, 4),
            "1 Waiting for You · 3 Working"
        );
        assert_eq!(summary_text(en, 0, 1, 0, 1), "1 Waiting for You");
        assert_eq!(summary_text(en, 2, 0, 0, 2), "2 Working");
        assert_eq!(summary_text(en, 0, 0, 2, 2), "2 Finished");
        assert_eq!(summary_text(en, 0, 0, 0, 2), "2 Idle");
        // With no sessions at all, "0 Working · 0 Waiting" should not appear
        assert_eq!(summary_text(en, 0, 0, 0, 0), "");
    }

    #[test]
    fn summary_follows_the_language() {
        let zh = &crate::core::i18n::ZH_HANS;
        assert_eq!(summary_text(zh, 0, 2, 0, 2), "2 个在等你");
        assert_eq!(summary_text(zh, 1, 0, 0, 1), "1 个在干活");
    }

    #[test]
    fn labeled_row_uses_product_name_and_falls_back_to_state_word() {
        let en = &crate::core::i18n::EN;
        // With no message it falls back to the state word, and agent uses the display name (not the id)
        let s = sess("1", "claude", "unknown", AgentState::Working, "");
        assert_eq!(session_label(en, &s), "● Claude Code · Working");

        let s = sess("2", "codex", "OpenCapX", AgentState::Waiting, "needs permission");
        assert_eq!(session_label(en, &s), "◐ Codex · OpenCapX · needs permission");

        // Under the Chinese locale the state word changes accordingly
        let zh = &crate::core::i18n::ZH_HANS;
        let s = sess("3", "gemini", "OpenCapX", AgentState::Waiting, "");
        assert_eq!(session_label(zh, &s), "◐ Gemini CLI · OpenCapX · 等你输入");
    }

    #[test]
    fn labeled_row_is_width_bounded() {
        let en = &crate::core::i18n::EN;
        let long = "x".repeat(200);
        let s = sess("3", "claude", "OpenCapX", AgentState::Working, &long);
        let label = session_label(en, &s);
        // The prefix is fixed; the message part is truncated to 34 chars + an ellipsis
        let prefix = "● Claude Code · OpenCapX · ";
        assert!(label.starts_with(prefix), "{label}");
        assert!(
            label.chars().count() <= prefix.chars().count() + 35,
            "message not bounded: {label}"
        );
        assert!(label.contains('…'));
    }

    #[test]
    fn waiting_sorts_before_working() {
        let mut v = vec![
            sess("a", "claude", "p", AgentState::Done, ""),
            sess("b", "codex", "p", AgentState::Waiting, ""),
            sess("c", "gemini", "p", AgentState::Working, ""),
        ];
        crate::core::agent::sort_sessions(&mut v);
        let order: Vec<&str> = v.iter().map(|s| s.agent.as_str()).collect();
        assert_eq!(order, vec!["codex", "gemini", "claude"]);
    }

    #[test]
    fn tooltip_names_the_agent_that_needs_you() {
        let sessions = vec![
            sess("a", "claude", "OpenCapX", AgentState::Working, "Edit src/overlay.ts"),
            sess("b", "gemini", "OpenCapX", AgentState::Waiting, "needs permission to use Bash"),
        ];
        let en = &crate::core::i18n::EN;
        let tip = tooltip_text(en, &sessions);
        assert!(tip.starts_with("OpenCapX · 1 Waiting for You"), "{tip}");
        assert!(tip.contains("Gemini CLI: needs permission to use Bash"), "{tip}");
        // No sessions: keep only the app name, don't write 0
        assert_eq!(tooltip_text(en, &[]), "OpenCapX");
    }

    #[test]
    fn tooltip_reports_finished_when_nothing_is_running() {
        let en = &crate::core::i18n::EN;
        let sessions = vec![sess("a", "claude", "p", AgentState::Done, "")];
        assert_eq!(tooltip_text(en, &sessions), "OpenCapX · 1 Finished");
    }

    #[test]
    fn badge_none_returns_base_untouched() {
        let base: Vec<u8> = (0..8 * 4).map(|i| i as u8).collect();
        assert_eq!(compose_icon(&base, 2, 4, TrayBadge::None), base);
    }

    #[test]
    fn badge_is_drawn_top_right_and_keeps_dimensions() {
        let (w, h) = (32u32, 32u32);
        let base = vec![0u8; (w * h * 4) as usize];
        let out = compose_icon(&base, w, h, TrayBadge::Waiting);
        assert_eq!(out.len(), base.len(), "dimensions must not change");
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            (out[i], out[i + 1], out[i + 2], out[i + 3])
        };
        // Badge center: orange, opaque
        let (r, g, b, a) = px(24, 8);
        assert!(a > 200, "badge should be opaque, got a={a}");
        assert!(r > 200 && g > 100 && b < 80, "should be orange, got {r},{g},{b}");
        // Lower-left far from the badge: stays transparent
        assert_eq!(px(1, 30).3, 0);
        // working uses green
        let out2 = compose_icon(&base, w, h, TrayBadge::Working);
        let i = ((8 * w + 24) * 4) as usize;
        assert!(out2[i + 1] > out2[i], "green channel should dominate");
    }

    #[test]
    fn badge_blends_over_existing_pixels() {
        let (w, h) = (32u32, 32u32);
        // Fill with opaque red first; the badge should cover it rather than leave a hole
        let mut base = vec![0u8; (w * h * 4) as usize];
        for i in (0..base.len()).step_by(4) {
            base[i] = 255;
            base[i + 3] = 255;
        }
        let out = compose_icon(&base, w, h, TrayBadge::Working);
        let i = ((8 * w + 24) * 4) as usize;
        assert_eq!(out[i + 3], 255);
        assert!(out[i + 1] > 200, "should be badge green over red");
    }
}
