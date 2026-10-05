//! Subagents (board B on https://claude.ai/artifact/FuNu2MJ7p2k6RC1E88Dezf): the header's
//! orbit chip, shown only while the selected terminal's agent has subagents running, and its
//! popover (⌥⌘A): running subagents (with what each was asked to do), then the ones that
//! finished since the human's last prompt. ↑↓ select, ↩ or a click opens the subagent's
//! read-only window (`ui/subagent_window.rs`).
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{AgentInfo, Subagent};

actions!(
    midna,
    [
        /// ⌥⌘A: open or close the selected terminal's subagents.
        ToggleSubagents,
    ]
);

pub struct SubagentsPanel {
    pub focus: FocusHandle,
    sel: usize,
}

impl SubagentsPanel {
    pub fn new(cx: &mut App) -> SubagentsPanel {
        SubagentsPanel { focus: cx.focus_handle(), sel: 0 }
    }
}

/// Running now: live subagents, then background ones waiting between wakes (those are only
/// in the Stop snapshot, so they have no start time).
pub fn running(info: &AgentInfo) -> Vec<Subagent> {
    let mut v = info.subagents.clone();
    for t in info.background.iter().filter(|t| t.kind == "subagent" && t.status == "running") {
        if !v.iter().any(|a| a.id == t.id) {
            v.push(Subagent {
                id: t.id.clone(),
                agent_type: t.agent_type.clone().unwrap_or_default(),
                description: t.description.clone(),
                background: true,
                started_at: String::new(),
                ended_at: None,
            });
        }
    }
    v
}

/// Finished since the last prompt, newest first; not counting one that's running again.
pub fn finished(info: &AgentInfo, running: &[Subagent]) -> Vec<Subagent> {
    info.finished_subagents.iter().rev().filter(|f| !running.iter().any(|r| r.id == f.id)).cloned().collect()
}

fn lists(m: &MainWindow) -> (Vec<Subagent>, Vec<Subagent>) {
    let info = m.selected_session().and_then(|s| s.agent_info.clone()).unwrap_or_default();
    let run = running(&info);
    let done = finished(&info, &run);
    (run, done)
}

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.menu == Menu::Subagents {
        close(m, window, cx);
        return;
    }
    let (run, done) = lists(m);
    if run.is_empty() && done.is_empty() {
        return;
    }
    m.menu = Menu::Subagents;
    m.subagents.sel = 0;
    m.subagents.focus.focus(window, cx);
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.focus_terminal(window, cx);
    cx.notify();
}

fn open(m: &mut MainWindow, a: Subagent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone() else { return };
    close(m, window, cx);
    super::subagent_window::open(m, sid, a, window, cx);
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let (run, done) = lists(m);
    let all: Vec<Subagent> = run.into_iter().chain(done).collect();
    let n = all.len();
    match ks.key.as_str() {
        "up" | "down" if n > 0 => {
            let cur = m.subagents.sel.min(n - 1);
            m.subagents.sel = if ks.key == "down" { (cur + 1) % n } else { (cur + n - 1) % n };
        }
        "enter" => {
            if let Some(a) = all.get(m.subagents.sel.min(n.saturating_sub(1))).cloned() {
                open(m, a, window, cx);
            }
        }
        "escape" => close(m, window, cx),
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

// ------------------------------------------------------------------ rendering

/// The header chip: orbit icon and how many are running. None when nothing runs (unless its
/// popover is open, so it doesn't vanish under the pointer as the last one finishes).
pub fn button(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let (run, _) = lists(m);
    let open = m.menu == Menu::Subagents;
    if run.is_empty() && !open {
        return None;
    }
    let tip = match run.len() {
        1 => "1 subagent running".to_string(),
        n => format!("{n} subagents running"),
    };
    let el = div()
        .id("tb-subagents")
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(9.))
        .rounded(px(7.))
        .cursor_pointer()
        .border_1()
        .border_color(if open { t.work } else { transparent_black() })
        .when(open, |d| d.bg(t.work.opacity(0.16)))
        .hover(|st| st.bg(t.raised))
        .tooltip(super::header::tip_keys(tip, "keys.subagents"))
        .on_click(cx.listener(|m, _, window, cx| toggle(m, window, cx)))
        .child(Icon::Orbit.el(16., t.work))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(t.fg).child(run.len().to_string()))
        .when(!run.is_empty(), |d| d.child(div().size(px(6.)).rounded_full().bg(t.work)));
    Some(div().relative().child(el).when(open, |d| d.child(popover(m, t, cx))).into_any_element())
}

fn popover(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let (run, done) = lists(m);
    let n = run.len() + done.len();
    let sel = m.subagents.sel.min(n.saturating_sub(1));
    let heading = |text: &str| div().px(px(14.)).pt(px(10.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(text.to_uppercase());

    let mut list = div().id("subagents-list").flex().flex_col().max_h(px(440.)).overflow_y_scroll().pb(px(6.));
    list = list.child(heading("Running"));
    if run.is_empty() {
        list = list.child(div().px(px(14.)).py(px(6.)).text_color(t.dim).child("Nothing running now."));
    }
    for (i, a) in run.into_iter().enumerate() {
        list = list.child(row(t, a, i, i == sel, true, cx));
    }
    if !done.is_empty() {
        list = list.child(div().mt(px(4.)).border_t_1().border_color(t.line)).child(heading("Finished this turn"));
        let base = n - done.len();
        for (j, a) in done.into_iter().enumerate() {
            list = list.child(row(t, a, base + j, base + j == sel, false, cx));
        }
    }

    let footer = div()
        .flex()
        .gap(px(14.))
        .px(px(14.))
        .py(px(9.))
        .border_t_1()
        .border_color(t.line)
        .text_size(px(11.5))
        .text_color(t.dim)
        .child(div().flex().gap(px(5.)).child(div().font_family(t.mono_font.clone()).child("↩")).child("open in a window"))
        .child(div().flex_1())
        .child(div().text_color(t.dim).child("read-only"));

    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            div()
                .id("subagents-popover")
                .key_context("MidnaOverlay")
                .track_focus(&m.subagents.focus)
                .on_key_down(cx.listener(on_key))
                .occlude()
                .mt(px(38.))
                .w(px(420.))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_color(t.fg)
                .text_size(px(13.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
                .child(list)
                .child(footer),
        ),
    )
    .with_priority(1)
}

fn row(t: &Theme, a: Subagent, i: usize, selected: bool, running: bool, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let title = if a.description.is_empty() { format!("{} subagent", a.agent_type) } else { a.description.clone() };
    let when = if running { super::subagent_window::elapsed(&a.started_at, None) } else { super::subagent_window::elapsed(&a.started_at, a.ended_at.as_deref()) };
    let sub = match (running, a.background) {
        (true, true) => format!("{} · background", a.agent_type),
        _ => a.agent_type.clone(),
    };
    let opened = a.clone();
    div()
        .id(SharedString::from(format!("subagent-{}", a.id)))
        .flex()
        .items_center()
        .gap(px(10.))
        .mx(px(6.))
        .px(px(8.))
        .py(px(6.))
        .rounded(px(7.))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.accent_soft))
        .on_mouse_move(cx.listener(move |m, _, _, cx| {
            if m.subagents.sel != i {
                m.subagents.sel = i;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |m, _, window, cx| open(m, opened.clone(), window, cx)))
        .when(!running, |d| d.child(Icon::Check.el(13., t.ok)))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .when(running, |d| d.font_weight(FontWeight::MEDIUM))
                        .when(!running, |d| d.text_color(t.dim))
                        .child(title),
                )
                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.5)).text_color(t.dim).font_family(t.mono_font.clone()).child(sub)),
        )
        .child(div().flex_none().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(if running { t.work } else { t.dim }).child(when))
        .child(Icon::PopOut.el(13., t.dim))
}

#[cfg(test)]
mod tests {
    use super::{finished, running};
    use midna_proto::{AgentInfo, BackgroundTask, Subagent};

    fn sub(id: &str) -> Subagent {
        Subagent { id: id.into(), agent_type: "Explore".into(), started_at: "2026-10-05T10:00:00Z".into(), ..Default::default() }
    }

    #[test]
    fn running_includes_waiting_background_agents_once() {
        let info = AgentInfo {
            subagents: vec![sub("a1")],
            background: vec![
                BackgroundTask { id: "a1".into(), kind: "subagent".into(), status: "running".into(), ..Default::default() },
                BackgroundTask { id: "a2".into(), kind: "subagent".into(), status: "running".into(), description: "review".into(), ..Default::default() },
                BackgroundTask { id: "b1".into(), kind: "shell".into(), status: "running".into(), ..Default::default() },
            ],
            finished_subagents: vec![sub("a0"), sub("a2"), sub("a3")],
            ..Default::default()
        };
        let run = running(&info);
        assert_eq!(run.iter().map(|a| (a.id.as_str(), a.background)).collect::<Vec<_>>(), [("a1", false), ("a2", true)]);
        assert_eq!(run[1].description, "review");
        assert_eq!(finished(&info, &run).iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["a3", "a0"], "newest first, not one running again");
    }
}
