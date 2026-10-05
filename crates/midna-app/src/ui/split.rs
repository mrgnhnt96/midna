//! Split panes: a second terminal next to (or under) the selected one in the main window.
//! At most two panes. Each pane is its own `TerminalView` with its own frame stream and
//! focus; the header, banner and composer keep following the sidebar selection (the left /
//! top pane). ⌘D or the header's split button opens a new shell in the current project in
//! the second pane; its strip toggles side-by-side ⇄ stacked and closes it (the session keeps
//! running and stays in the sidebar).
use super::session_dot;
use crate::app::MainWindow;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;

pub struct Split {
    pub view: Entity<TerminalView>,
    /// Stacked (top / bottom) instead of side by side.
    pub stacked: bool,
}

impl Split {
    pub fn session_id(&self, cx: &App) -> String {
        self.view.read(cx).session_id.clone()
    }
}

/// ⌘D / the header button: open a split with a new shell, or close the open one.
pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.split.is_some() {
        close(m, window, cx);
        return;
    }
    if m.selected.is_none() {
        m.toast("Select a terminal to split.", cx);
        return;
    }
    let mut p = json!({ "kind": "shell" });
    if let Some(pid) = m.current_project_id() {
        p["project_id"] = json!(pid);
    }
    m.rpc("session.open", p, cx, |m, v, window, cx| {
        if let Some(id) = v.get("id").and_then(|v| v.as_str()) {
            show(m, id.to_string(), window, cx);
        }
    });
}

/// Show `session` in the second pane and focus it.
pub fn show(m: &mut MainWindow, session: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    let backend = m.backend.clone();
    let view = cx.new(|cx| TerminalView::new(session, backend, window, cx));
    view.read(cx).focus_handle().clone().focus(window, cx);
    let stacked = m.split.as_ref().is_some_and(|s| s.stacked);
    m.split = Some(Split { view, stacked });
    m.request_refresh(crate::app::refresh::SESSIONS, cx);
    cx.notify();
}

/// Side by side ⇄ stacked.
pub fn flip(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if let Some(s) = m.split.as_mut() {
        s.stacked = !s.stacked;
        cx.notify();
    }
}

/// Show the split's terminal in the main pane (the split closes).
pub fn to_main(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(id) = m.split.take().map(|s| s.session_id(cx)) else {
        return;
    };
    m.select(id, window, cx);
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

pub fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.split.take().is_some() {
        m.focus_terminal(window, cx);
        cx.notify();
    }
}

/// The terminal area: one pane, or two with a divider. The focused pane gets an accent edge.
pub fn render(m: &mut MainWindow, main: &Entity<TerminalView>, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    // Selecting the split's own session in the sidebar folds the split away.
    if let Some(s) = &m.split
        && m.selected.as_deref() == Some(s.session_id(cx).as_str())
    {
        m.split = None;
    }
    let Some(split) = &m.split else {
        return div().flex_1().min_h_0().child(main.clone()).into_any_element();
    };
    let main_focused = main.read(cx).focus_handle().contains_focused(window, cx);
    let split_focused = split.view.read(cx).focus_handle().contains_focused(window, cx);
    let sid = split.session_id(cx);
    let session = m.sessions.iter().find(|s| s.id == sid);
    let name = session.map(|s| s.name.clone()).unwrap_or_else(|| sid.clone());
    let state = session.map(|s| m.effective_state(s)).unwrap_or_default();
    let stacked = split.stacked;
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
        .px(px(12.))
        .border_b_1()
        .border_color(t.line)
        .bg(t.bg)
        .child(session_dot(t, state, session.and_then(|s| s.custom_status.as_ref()), 7.))
        .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(12.)).font_weight(FontWeight::MEDIUM).child(name))
        .child(button("split-orient", if stacked { "⇆" } else { "⇅" }, if stacked { "Side by side" } else { "Stack top and bottom" }, "keys.split_orientation").on_click(cx.listener(|m, _, _, cx| flip(m, cx))))
        .child(button("split-select", "↖", "Show in the main pane", "keys.split_to_main").on_click(cx.listener(|m, _, window, cx| to_main(m, window, cx))))
        .child(button("split-close", "✕", "Close this pane (the terminal keeps running)", "keys.split").on_click(cx.listener(|m, _, window, cx| close(m, window, cx))));
    let first = edge(div().flex().flex_col().flex_1().min_w_0().min_h_0(), main_focused).child(div().flex_1().min_h_0().child(main.clone()));
    let second = edge(div().flex().flex_col().flex_1().min_w_0().min_h_0(), split_focused).child(strip).child(div().flex_1().min_h_0().child(split.view.clone()));
    let divider = if stacked { div().h(px(1.)).w_full() } else { div().w(px(1.)).h_full() }.flex_none().bg(t.line);
    let row = div().flex().flex_1().min_h_0().min_w_0();
    if stacked { row.flex_col() } else { row.flex_row() }.child(first).child(divider).child(second).into_any_element()
}
