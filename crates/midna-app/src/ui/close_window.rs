//! Closing a main window that still has terminals while another main window is open asks
//! whether to close its terminals or move them to the window you used last. Ticking "Don't
//! ask again" saves the answer in `windows.close_with_terminals` (Settings ▸ General changes
//! it back). The last main window just closes: midna quits and the shells keep running in midnad.
use crate::app::MainWindow;
use crate::model::StatusState;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_danger, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};

/// The open question (on `MainWindow.close_ask`).
#[derive(Clone, Default)]
pub struct CloseAsk {
    pub dont_ask: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Choice {
    Close,
    Move,
}

/// The terminals this window would take with it (pop-outs that came from it included).
fn own(m: &MainWindow) -> Vec<String> {
    m.sessions.iter().filter(|s| m.shows(&s.id)).map(|s| s.id.clone()).collect()
}

/// The window is about to close (traffic light, ⌘W in an empty window, the menu). True =
/// let it; false = it asked, or it's busy closing.
pub fn should_close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let ids = own(m);
    if ids.is_empty() || !m.windows.borrow().has_other(m.id) {
        if m.windows.borrow().has_other(m.id) {
            crate::windows::leaving(m.id, cx);
        }
        return true;
    }
    match m.settings.get("windows.close_with_terminals").and_then(Value::as_str) {
        Some("close") => finish(m, Choice::Close, ids, cx),
        Some("move") => finish(m, Choice::Move, ids, cx),
        _ => {
            m.close_ask = Some(CloseAsk::default());
            m.overlay_focus.focus(window, cx);
            cx.notify();
            return false;
        }
    }
    true
}

/// ⌘W with nothing selected: close the window the same way the traffic light does.
pub fn request(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if should_close(m, window, cx) {
        window.remove_window();
    }
}

/// Close (`session.close`) or hand over the terminals; the window closes right after.
/// Moving needs nothing here: a closed window's terminals go to the home window.
fn finish(m: &mut MainWindow, choice: Choice, ids: Vec<String>, cx: &mut Context<MainWindow>) {
    crate::windows::leaving(m.id, cx);
    if choice == Choice::Close {
        for id in &ids {
            crate::ui::popout::close(id, cx);
        }
        let backend = m.backend.clone();
        std::thread::spawn(move || {
            for id in ids {
                let _ = backend.call("session.close", json!({ "id": id }));
            }
        });
    }
}

fn decide(m: &mut MainWindow, choice: Choice, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(ask) = m.close_ask.take() else { return };
    if ask.dont_ask {
        let value = if choice == Choice::Close { "close" } else { "move" };
        let backend = m.backend.clone();
        std::thread::spawn(move || {
            let _ = backend.call("settings.set", json!({ "key": "windows.close_with_terminals", "value": value }));
        });
    }
    finish(m, choice, own(m), cx);
    window.remove_window();
}

fn cancel(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.close_ask = None;
    m.focus_terminal(window, cx);
    cx.notify();
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let ask = m.close_ask.as_ref()?;
    let ids = own(m);
    let n = ids.len();
    let busy = m.sessions.iter().filter(|s| ids.contains(&s.id) && matches!(m.effective_state(s), StatusState::Working | StatusState::NeedsYou)).count();
    let terms = if n == 1 { "1 terminal".to_string() } else { format!("{n} terminals") };
    let busy_note = if busy > 0 { format!(" ({busy} busy)") } else { String::new() };
    let dont_ask = ask.dont_ask;
    let check = div()
        .id("close-dont-ask")
        .flex()
        .items_center()
        .gap(px(8.))
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(t.dim)
        .child(
            div()
                .size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1()
                .border_color(if dont_ask { t.accent } else { t.line })
                .when(dont_ask, |d| d.bg(t.accent).text_color(t.bg).text_size(px(10.)).child("✓")),
        )
        .child("Don't ask again")
        .on_click(cx.listener(|m, _, _, cx| {
            if let Some(a) = m.close_ask.as_mut() {
                a.dont_ask = !a.dont_ask;
            }
            cx.notify();
        }));
    let card = div()
        .id("close-window-card")
        .w(px(440.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(20.))
        .rounded(px(12.))
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Close this window?"))
        .child(div().text_color(t.dim).child(format!("It has {terms}{busy_note}. Move them to the window you used last, or close them.")))
        .child(check)
        .child(
            div()
                .flex()
                .justify_end()
                .gap(px(8.))
                .child(btn(t, "close-cancel", "Cancel").on_click(cx.listener(|m, _, w, cx| cancel(m, w, cx))))
                .child(btn_danger(t, "close-terms", format!("Close {terms}"), 28.).on_click(cx.listener(|m, _, w, cx| decide(m, Choice::Close, w, cx))))
                .child(btn_primary(t, "close-move", "Move to other window  ↩").on_click(cx.listener(|m, _, w, cx| decide(m, Choice::Move, w, cx)))),
        )
        .child(div().text_size(px(11.)).text_color(t.dim).child("You can change the remembered answer in Settings ▸ General."));
    Some(
        div()
            .id("close-window")
            .track_focus(&m.overlay_focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_mouse_down(MouseButton::Left, cx.listener(|m, _, w, cx| cancel(m, w, cx)))
            .on_key_down(cx.listener(|m, ev: &KeyDownEvent, w, cx| match ev.keystroke.key.as_str() {
                "escape" => cancel(m, w, cx),
                "enter" => decide(m, Choice::Move, w, cx),
                _ => {}
            }))
            .child(card)
            .into_any_element(),
    )
}
