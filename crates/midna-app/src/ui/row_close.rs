//! Closing a terminal from its sidebar row (design C on the Row Close canvas): hovering a row
//! turns its agent icon into a ✕. Clicking the ✕ opens a line under the row with Cancel and
//! Close, away from where you clicked, and says when an agent is still working or waiting on
//! you. Close ignores clicks for its first `ARM` (a double-click on the ✕ can't close), and the
//! line folds back after `REVERT`, `LEAVE` after the pointer leaves the row, on a second click
//! of the ✕ or on a click on the row.
use crate::app::MainWindow;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

/// Close takes clicks after this.
const ARM: Duration = Duration::from_millis(300);
/// The line folds back after this, untouched.
const REVERT: Duration = Duration::from_secs(3);
/// And this long after the pointer leaves the row.
const LEAVE: Duration = Duration::from_millis(600);
/// The line unfolding or folding back, in ms.
const OPEN_MS: f32 = 180.;
/// The line's height, open.
const LINE_H: f32 = 33.;

pub struct Armed {
    pub id: String,
    at: Instant,
}

#[derive(Default)]
pub struct RowClose {
    pub armed: Option<Armed>,
    /// The row whose line is folding back, and since when.
    leaving: Option<(String, Instant)>,
    /// Folds the line back when `REVERT` or `LEAVE` runs out (dropping it cancels).
    task: Option<Task<()>>,
}

impl RowClose {
    pub fn is_armed(&self, id: &str) -> bool {
        self.armed.as_ref().is_some_and(|a| a.id == id)
    }

    /// How far `id`'s line is open, 0 to 1.
    pub fn open(&self, id: &str) -> f32 {
        let p = |at: Instant| (at.elapsed().as_secs_f32() * 1000. / OPEN_MS).min(1.);
        match (&self.armed, &self.leaving) {
            (Some(a), _) if a.id == id => ease_out(p(a.at)),
            (_, Some((l, at))) if l == id => 1. - ease_out(p(*at)),
            _ => 0.,
        }
    }

    fn ready(&self, id: &str) -> bool {
        self.armed.as_ref().is_some_and(|a| a.id == id && a.at.elapsed() >= ARM)
    }

    /// Something's still moving (or Close is about to take clicks): draw another frame.
    pub fn moving(&self) -> bool {
        let busy = |at: Instant, d: Duration| at.elapsed() < d;
        let open = Duration::from_millis(OPEN_MS as u64);
        self.armed.as_ref().is_some_and(|a| busy(a.at, open.max(ARM))) || self.leaving.as_ref().is_some_and(|(_, at)| busy(*at, open))
    }
}

fn ease_out(x: f32) -> f32 {
    1. - (1. - x).powi(3)
}

/// The ✕: open `id`'s line, or fold it back when it's open.
pub fn toggle(m: &mut MainWindow, id: &str, cx: &mut Context<MainWindow>) {
    if m.row_close.is_armed(id) {
        disarm(m, cx);
        return;
    }
    disarm(m, cx);
    let at = Instant::now();
    m.row_close.armed = Some(Armed { id: id.to_string(), at });
    m.row_close.leaving = None;
    revert_in(m, at, REVERT, cx);
    cx.notify();
}

/// Fold the open line back, if any.
pub fn disarm(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.row_close.task = None;
    if let Some(a) = m.row_close.armed.take() {
        gpui_kit::base::apply_system_reduce_motion(cx);
        m.row_close.leaving = (!cx.reduce_motion()).then(|| (a.id, Instant::now()));
        cx.notify();
    }
}

/// The pointer came onto (`over`) or left `id`'s row.
pub fn hover(m: &mut MainWindow, id: &str, over: bool, cx: &mut Context<MainWindow>) {
    let Some(at) = m.row_close.armed.as_ref().filter(|a| a.id == id).map(|a| a.at) else { return };
    let left = if over { REVERT.saturating_sub(at.elapsed()) } else { LEAVE };
    revert_in(m, at, left, cx);
}

fn revert_in(m: &mut MainWindow, at: Instant, after: Duration, cx: &mut Context<MainWindow>) {
    m.row_close.task = Some(cx.spawn(async move |this, cx| {
        cx.background_executor().timer(after).await;
        let _ = this.update(cx, |m, cx| {
            if m.row_close.armed.as_ref().is_some_and(|a| a.at == at) {
                disarm(m, cx);
            }
        });
    }));
}

/// Close, once it takes clicks.
fn confirm(m: &mut MainWindow, id: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if !m.row_close.ready(id) {
        return;
    }
    m.row_close.task = None;
    m.row_close.armed = None;
    m.row_close.leaving = None;
    m.close_row(id, window, cx);
}

/// What the line says: why you might not want to, else the question.
fn ask(m: &MainWindow, s: &Session) -> (String, bool) {
    let who = s.agent.map(crate::commands::agent_name);
    match (m.effective_state(s), who) {
        (StatusState::Working, Some(a)) => (format!("{a} is still working"), true),
        (StatusState::Working, None) => ("Still running".into(), true),
        (StatusState::NeedsYou, Some(a)) => (format!("{a} is waiting on you"), true),
        (StatusState::NeedsYou, None) => ("Needs you".into(), true),
        _ => ("Close this terminal?".into(), false),
    }
}

/// The ✕ that sits on the agent icon: shown while the row is hovered (`group`) or armed.
pub fn x_button(m: &MainWindow, s: &Session, group: &'static str, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let (id, armed) = (s.id.clone(), m.row_close.is_armed(&s.id));
    div()
        .id(SharedString::from(format!("row-x-{}", s.id)))
        .absolute()
        .top(px(-4.))
        .left(px(-4.))
        .size(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .map(|d| if armed { d.bg(t.line) } else { d.opacity(0.).group_hover(group, |s| s.opacity(1.)) })
        .hover(|s| s.bg(t.line))
        .tooltip(super::header::tip(if armed { "Keep it" } else { "Close terminal" }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(cx.listener(move |m, _, _, cx| {
            cx.stop_propagation();
            toggle(m, &id, cx);
        }))
        .child(crate::icons::Icon::Cross.el(12., if armed { t.fg } else { t.dim }))
}

/// The line under an armed row (or one folding back): what closing stops, Cancel and Close.
pub fn line(m: &MainWindow, s: &Session, t: &Theme, cx: &mut Context<MainWindow>) -> Option<Div> {
    let open = m.row_close.open(&s.id);
    if open <= 0. {
        return None;
    }
    let (text, busy) = ask(m, s);
    let ready = m.row_close.ready(&s.id);
    let (cancel_id, close_id, line) = (s.id.clone(), s.id.clone(), t.line);
    Some(
        div()
            .h(px(LINE_H * open))
            .opacity(open)
            .overflow_hidden()
            .flex_none()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .pl(px(21.))
                    .pt(px(3.))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(if busy { t.need } else { t.dim }).child(text))
                    .child(
                        super::screen_kit::btn(t, SharedString::from(format!("row-keep-{}", s.id)), "Cancel")
                            .h(px(24.))
                            .px(px(9.))
                            .text_color(t.dim)
                            .hover(|s| s.bg(line))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |m, _, _, cx| {
                                cx.stop_propagation();
                                if m.row_close.is_armed(&cancel_id) {
                                    disarm(m, cx);
                                }
                            })),
                    )
                    .child(
                        super::screen_kit::btn_danger(t, SharedString::from(format!("row-close-{}", s.id)), "Close", 24.)
                            .when(!ready, |d| d.opacity(0.55).cursor(CursorStyle::Arrow))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |m, _, w, cx| {
                                cx.stop_propagation();
                                confirm(m, &close_id, w, cx);
                            })),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{ARM, Armed, RowClose};
    use std::time::{Duration, Instant};

    fn armed(id: &str, ago: Duration) -> RowClose {
        RowClose { armed: Some(Armed { id: id.into(), at: Instant::now() - ago }), ..Default::default() }
    }

    #[test]
    fn only_the_armed_row_opens() {
        let r = armed("a", Duration::from_secs(1));
        assert_eq!(r.open("a"), 1.);
        assert_eq!(r.open("b"), 0.);
        assert_eq!(RowClose::default().open("a"), 0.);
    }

    #[test]
    fn close_takes_clicks_only_after_arm() {
        assert!(!armed("a", Duration::from_millis(50)).ready("a"));
        assert!(armed("a", ARM).ready("a"));
        assert!(!armed("a", ARM).ready("b"));
    }

    #[test]
    fn folding_back_runs_from_open_to_shut() {
        let r = RowClose { leaving: Some(("a".into(), Instant::now() - Duration::from_secs(1))), ..Default::default() };
        assert_eq!(r.open("a"), 0.);
        assert!(!r.moving());
        let r = RowClose { leaving: Some(("a".into(), Instant::now())), ..Default::default() };
        assert!(r.open("a") > 0.9 && r.moving());
    }
}
