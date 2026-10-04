//! Inline rename: double-click a terminal's name (header or sidebar row) to edit it in place.
//! ↩ or clicking away saves (`session.rename`), esc cancels.
use super::screen_kit::{KeyOutcome, LineInput};
use crate::app::{MainWindow, refresh};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;

pub struct Rename {
    pub session: String,
    input: LineInput,
    _blur: Subscription,
}

pub fn start(m: &mut MainWindow, session: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.renaming.as_ref().is_some_and(|r| r.session == session) {
        return;
    }
    let Some(name) = m.sessions.iter().find(|s| s.id == session).map(|s| s.name.clone()) else { return };
    let input = LineInput::new(cx, false, "Terminal name");
    input.set_text(&name, cx);
    input.field.update(cx, |f, cx| f.select_all(cx));
    input.focus.focus(window, cx);
    let blur = cx.on_blur(&input.focus, window, |m, window, cx| commit(m, window, cx));
    m.renaming = Some(Rename { session: session.to_string(), input, _blur: blur });
    cx.notify();
}

fn commit(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(r) = m.renaming.take() else { return };
    let name = r.input.text(cx).trim().to_string();
    if let Some(s) = m.sessions.iter_mut().find(|s| s.id == r.session)
        && !name.is_empty()
        && name != s.name
    {
        s.name = name.clone();
        m.rpc("session.rename", json!({ "id": r.session, "name": name }), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS, cx));
    }
    m.focus_terminal(window, cx);
    cx.notify();
}

fn cancel(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.renaming = None;
    m.focus_terminal(window, cx);
    cx.notify();
}

/// The edit box in place of the name, when `session` is being renamed.
pub fn field(m: &MainWindow, session: &str, t: &Theme, text_size: f32, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let r = m.renaming.as_ref().filter(|r| r.session == session)?;
    Some(
        div()
            .id(SharedString::from(format!("rename-{session}")))
            .flex()
            .items_center()
            .flex_1()
            .min_w(px(80.))
            .max_w(px(260.))
            .h(px(text_size + 10.))
            .px(px(6.))
            .rounded(px(5.))
            .border_1()
            .border_color(t.accent)
            .bg(t.raised)
            .text_size(px(text_size))
            .text_color(t.fg)
            .overflow_hidden()
            .cursor_text()
            // don't let clicks inside the field reselect the row or start another rename
            .on_click(|_, _, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|m, ev: &KeyDownEvent, window, cx| {
                let Some(r) = m.renaming.as_mut() else { return };
                match r.input.on_key(ev, cx) {
                    KeyOutcome::Submit => commit(m, window, cx),
                    KeyOutcome::Cancel => cancel(m, window, cx),
                    KeyOutcome::Ignored => return,
                }
                cx.stop_propagation();
            }))
            .child(r.input.field.clone())
            .into_any_element(),
    )
}
