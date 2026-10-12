//! Split panes: a second terminal next to (or under) the selected one in the main window.
//! A split belongs to the terminal it was opened from (its host), like a pane inside a tab:
//! `MainWindow.splits` maps each host to its split's session, and the pane shows only while
//! its host is selected. Switching away shows the other terminal alone (or its own split);
//! switching back brings the split back. The split's session has no sidebar row of its own:
//! it sits indented under its host's row (`ui/sidebar.rs`, design A on the Midna Splits
//! canvas). Splits are saved with the window layout (`windows.rs`), so they survive a restart.
//!
//! ⌘D or the header's split button opens a new shell in the current project beside the
//! selected terminal, then hides and shows that split (it keeps running while hidden). The
//! pane's strip toggles side-by-side ⇄ stacked, gives its terminal a row of its own (↗), and
//! its ✕ (or ⌘W in the pane) closes its terminal. The header, banner and composer keep
//! following the sidebar selection (the left / top pane).
use crate::app::MainWindow;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;
use std::collections::HashMap;

/// A terminal's split: the session beside it and how it shows.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SplitOf {
    pub session: String,
    /// Stacked (top / bottom) instead of side by side.
    #[serde(default)]
    pub stacked: bool,
    /// Folded away with ⌘D: the session keeps running and stays the host's.
    #[serde(default)]
    pub hidden: bool,
}

/// The split pane on screen: the selected terminal's split, attached.
pub struct Split {
    pub view: Entity<TerminalView>,
}

impl Split {
    pub fn session_id(&self, cx: &App) -> String {
        self.view.read(cx).session_id.clone()
    }
}

/// The terminal `session` is the split of, if any.
pub fn host_of(m: &MainWindow, session: &str) -> Option<String> {
    m.splits.iter().find(|(_, s)| s.session == session).map(|(h, _)| h.clone())
}

pub fn is_split(m: &MainWindow, session: &str) -> bool {
    m.splits.values().any(|s| s.session == session)
}

/// The selected terminal's split, when it isn't hidden.
fn shown(m: &MainWindow) -> Option<&SplitOf> {
    m.selected.as_ref().and_then(|h| m.splits.get(h)).filter(|s| !s.hidden)
}

fn stacked(m: &MainWindow) -> bool {
    shown(m).is_some_and(|s| s.stacked)
}

/// Splits whose host or session is gone, popped out or in another window: dropped, so a
/// session left behind gets its own row again. Call with a fresh session list.
pub fn prune(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let keep = |id: &str| m.sessions.iter().any(|s| s.id == id) && m.shows(id) && !crate::ui::popout::is_popped(id, cx);
    let gone = stale(&m.splits, keep);
    if gone.is_empty() {
        return;
    }
    for h in gone {
        m.splits.remove(&h);
    }
    crate::windows::save_soon(cx);
}

/// Hosts whose split can't stay: either end isn't `keep`, or it splits itself.
fn stale(splits: &HashMap<String, SplitOf>, keep: impl Fn(&str) -> bool) -> Vec<String> {
    splits.iter().filter(|(h, s)| !keep(h) || !keep(&s.session) || **h == s.session).map(|(h, _)| h.clone()).collect()
}

/// `session` becomes `host`'s split. It stops being anyone else's split and stops hosting
/// one; a split `host` had before is let go. Each of those gets its own row again.
fn put(splits: &mut HashMap<String, SplitOf>, host: String, session: String, stacked: bool) {
    splits.retain(|h, s| s.session != session && *h != session && s.session != host);
    splits.insert(host, SplitOf { session, stacked, hidden: false });
}

/// Make the pane on screen match the selected terminal's split: attach it, swap it or take
/// it down. Focuses it when `focus_split` asked for that.
pub fn sync(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let want = shown(m).map(|s| s.session.clone());
    let have = m.split.as_ref().map(|s| s.session_id(cx));
    if want != have {
        m.split = want.map(|session| Split { view: cx.new(|cx| TerminalView::new(session, m.backend.clone(), window, cx)) });
    }
    if std::mem::take(&mut m.focus_split) {
        match &m.split {
            Some(s) => s.view.read(cx).focus_handle().clone().focus(window, cx),
            None => m.focus_terminal(window, cx),
        }
    }
}

/// Changed `m.splits`: save the layout and redraw.
fn changed(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    sync(m, window, cx);
    crate::windows::save_soon(cx);
    cx.notify();
}

/// ⌘D / the header button: open a split with a new shell beside the selected terminal, or
/// hide / show the split it has.
pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(host) = m.selected.clone() else {
        m.toast("Select a terminal to split.", cx);
        return;
    };
    if let Some(s) = m.splits.get_mut(&host) {
        s.hidden = !s.hidden;
        m.focus_split = !s.hidden;
        if s.hidden {
            m.focus_terminal(window, cx);
        }
        changed(m, window, cx);
        return;
    }
    let mut p = json!({ "kind": "shell" });
    if let Some(pid) = m.current_project_id() {
        p["project_id"] = json!(pid);
    }
    m.rpc("session.open", p, cx, move |m, v, window, cx| {
        if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
            m.windows.borrow_mut().claim(id, m.id);
            attach(m, host, id.to_string(), false, window, cx);
            m.sessions_changed(cx);
        }
    });
}

/// Make `session` the split of `host` and focus it. A split `host` had before, and one
/// `session` hosted itself, get rows of their own.
pub fn attach(m: &mut MainWindow, host: String, session: String, stacked: bool, window: &mut Window, cx: &mut Context<MainWindow>) {
    put(&mut m.splits, host, session, stacked);
    m.focus_split = true;
    changed(m, window, cx);
}

/// `window.command{action: split}`: show `target` beside the selected terminal.
pub fn show(m: &mut MainWindow, target: String, stacked: bool, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(host) = m.selected.clone() else {
        m.toast("Select a terminal to split.", cx);
        return;
    };
    attach(m, host, target, stacked, window, cx);
}

/// Side by side ⇄ stacked.
pub fn flip(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if let Some(s) = m.selected.as_ref().and_then(|h| m.splits.get_mut(h)) {
        s.stacked = !s.stacked;
        crate::windows::save_soon(cx);
        cx.notify();
    }
}

/// The split's terminal gets a row of its own (↗): it stops being the selected terminal's
/// split, keeps running, and the selection stays where it is.
pub fn to_own_row(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(host) = m.selected.clone().filter(|h| m.splits.get(h).is_some_and(|s| !s.hidden)) else {
        return;
    };
    m.splits.remove(&host);
    m.focus_terminal(window, cx);
    changed(m, window, cx);
}

/// Focus the other pane.
pub fn focus_other(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(split) = &m.split else {
        return;
    };
    let focus = split.view.read(cx).focus_handle().clone();
    if focus.contains_focused(window, cx) {
        m.focus_terminal(window, cx);
    } else {
        focus.focus(window, cx);
    }
    cx.notify();
}

/// Hide the selected terminal's split (`window.command` `close`).
pub fn hide(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(s) = m.selected.as_ref().and_then(|h| m.splits.get_mut(h)).filter(|s| !s.hidden) {
        s.hidden = true;
        m.focus_terminal(window, cx);
        changed(m, window, cx);
    }
}

/// The split's terminal is closing: it's no one's split any more.
pub fn forget(m: &mut MainWindow, session: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(h) = host_of(m, session) {
        m.splits.remove(&h);
        if focused(m, window, cx) && m.split.as_ref().is_some_and(|s| s.session_id(cx) == session) {
            m.focus_terminal(window, cx);
        }
        changed(m, window, cx);
    }
}

/// `host` is leaving this window's main pane (popped out, moved to another window): its
/// split's terminal gets a row of its own.
pub fn forget_host(m: &mut MainWindow, host: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.splits.remove(host).is_some() {
        changed(m, window, cx);
    }
}

/// The sidebar row under a host for its split: clicking it shows the host with its split
/// (unhiding it) and puts the cursor in the split.
pub fn open_from_row(m: &mut MainWindow, host: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.select_only(host.clone(), window, cx);
    if let Some(s) = m.splits.get_mut(&host) {
        s.hidden = false;
    }
    m.focus_split = true;
    changed(m, window, cx);
}

/// The terminal area: one pane, or two with a divider. The focused pane gets an accent edge.
pub fn render(m: &mut MainWindow, main: &Entity<TerminalView>, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    sync(m, window, cx);
    let Some(split) = &m.split else {
        return div().flex_1().min_h_0().child(main.clone()).into_any_element();
    };
    let main_focused = main.read(cx).focus_handle().contains_focused(window, cx);
    let split_focused = split.view.read(cx).focus_handle().contains_focused(window, cx);
    let sid = split.session_id(cx);
    let session = m.sessions.iter().find(|s| s.id == sid);
    let name = session.map(|s| s.name.clone()).unwrap_or_else(|| sid.clone());
    let state = session.map(|s| m.effective_state(s)).unwrap_or_default();
    let stacked = stacked(m);
    let edge = |d: Div, on: bool| if stacked { d.border_l_2() } else { d.border_t_2() }.border_color(if on { t.accent.opacity(0.7) } else { t.term.opacity(0.) });
    let button = |id: &'static str, label: &'static str, tip: &'static str, setting: &'static str| {
        div()
            .id(id)
            .px(px(6.))
            .h(px(20.))
            .rounded(px(5.))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(12.))
            .text_color(t.dim)
            .hover(|st| st.bg(t.raised))
            .tooltip(super::header::tip_keys(tip, setting))
            .child(label)
    };
    let strip = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(28.))
        .pl(px(12.))
        // Side by side, the strip runs along the top right, where the badge docks.
        .pr(px(if stacked { 12. } else { super::badge::docked_room(cx).max(12.) }))
        .border_b_1()
        .border_color(t.line)
        .bg(t.bg)
        .child(match session {
            Some(s) => super::terminal_dot(m, t, s, 7.),
            None => super::status_dot(t, state, 7.),
        })
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(name))
        .child(button("split-orient", if stacked { "⇆" } else { "⇅" }, if stacked { "Side by side" } else { "Stack top and bottom" }, "keys.split_orientation").on_click(cx.listener(|m, _, _, cx| flip(m, cx))))
        .child(button("split-own-row", "↗", "Give it its own row", "keys.split_to_main").on_click(cx.listener(|m, _, window, cx| to_own_row(m, window, cx))))
        .child(button("split-close", "✕", "Close this terminal", "keys.close").on_click(cx.listener(|m, _, window, cx| m.close_split_terminal("Click ✕", window, cx))));
    let first = edge(div().flex().flex_col().flex_1().min_w_0().min_h_0(), main_focused).child(div().flex_1().min_h_0().child(main.clone()));
    let tray = super::annotate::tray(&sid, t, cx, |m, id, w, cx| _ = crate::annotate::open(m, Some(id), w, cx), |m, w, cx| {
        if let Some(s) = &m.split {
            let fh = s.view.read(cx).focus_handle().clone();
            fh.focus(w, cx);
        }
    });
    let second = edge(div().flex().flex_col().flex_1().min_w_0().min_h_0(), split_focused).child(strip).child(div().flex_1().min_h_0().child(split.view.clone())).children(tray);
    let divider = if stacked { div().h(px(1.)).w_full() } else { div().w(px(1.)).h_full() }.flex_none().bg(t.line);
    let row = div().flex().flex_1().min_h_0().min_w_0();
    if stacked { row.flex_col() } else { row.flex_row() }.child(first).child(divider).child(second).into_any_element()
}

/// Whether the split pane has the keyboard (the sidebar lights the split's row then).
pub fn focused(m: &MainWindow, window: &Window, cx: &App) -> bool {
    m.split.as_ref().is_some_and(|s| s.view.read(cx).focus_handle().contains_focused(window, cx))
}

/// Header button / menu label for ⌘D on the selected terminal.
pub fn label(m: &MainWindow) -> &'static str {
    match m.selected.as_ref().and_then(|h| m.splits.get(h)) {
        Some(s) if s.hidden => "Show split",
        Some(_) => "Hide split",
        None => "Split",
    }
}

#[cfg(test)]
mod tests {
    use super::{SplitOf, put, stale};
    use std::collections::HashMap;

    fn split(session: &str) -> SplitOf {
        SplitOf { session: session.into(), stacked: false, hidden: false }
    }

    #[test]
    fn put_moves_a_split_between_hosts() {
        let mut m = HashMap::new();
        put(&mut m, "a".into(), "z".into(), false);
        put(&mut m, "b".into(), "z".into(), true);
        assert_eq!(m.len(), 1);
        assert_eq!(m["b"], SplitOf { session: "z".into(), stacked: true, hidden: false });
    }

    #[test]
    fn put_replaces_the_hosts_split_and_unhosts_the_new_one() {
        let mut m = HashMap::from([("a".to_string(), split("old")), ("z".to_string(), split("y"))]);
        put(&mut m, "a".into(), "z".into(), false);
        assert_eq!(m, HashMap::from([("a".to_string(), split("z"))]));
    }

    #[test]
    fn put_never_nests_a_host_inside_its_split() {
        let mut m = HashMap::from([("a".to_string(), split("b"))]);
        put(&mut m, "b".into(), "c".into(), false);
        assert_eq!(m, HashMap::from([("b".to_string(), split("c"))]));
    }

    #[test]
    fn stale_drops_splits_missing_either_end() {
        let m = HashMap::from([("a".to_string(), split("x")), ("b".to_string(), split("y")), ("c".to_string(), split("c")), ("gone".to_string(), split("z"))]);
        let mut out = stale(&m, |id| id != "gone" && id != "y");
        out.sort();
        assert_eq!(out, ["b", "c", "gone"]);
    }

    #[test]
    fn saved_split_reads_without_optional_fields() {
        let s: SplitOf = serde_json::from_str(r#"{"session":"s1"}"#).unwrap();
        assert_eq!(s, split("s1"));
    }
}
