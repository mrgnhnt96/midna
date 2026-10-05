//! Accessibility, asked for only when it's needed: the first time Kass starts dictating into
//! midna (`dictationWillBegin` for our pid) while midna isn't trusted, a card over the terminal
//! pane asks for the grant. It never takes focus, so the dictation carries on. "Not now" is
//! remembered (`app-state.json` `seen: ["ax-prompt"]`); after that a quiet status bar item
//! ("Kass needs Accessibility") reopens it. Once granted, nothing shows.
use crate::app::MainWindow;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;

const SEEN_KEY: &str = "ax-prompt";
const PANE_AX: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

fn trusted() -> bool {
    crate::dev::var("MIDNA_DEBUG_AX_UNTRUSTED").is_err() && crate::settings_window::accessibility_trusted()
}

/// Kass is about to dictate into midna. Shows the card unless midna is trusted or the human
/// already said "Not now".
pub fn on_kass_begin(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if !m.ax_prompt && !m.seen.contains(SEEN_KEY) && !trusted() {
        m.ax_prompt = true;
        cx.notify();
    }
}

pub fn open(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.ax_prompt = true;
    cx.notify();
}

fn dismiss(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.ax_prompt = false;
    crate::ui::statusbar::remember_seen(m, SEEN_KEY);
    cx.notify();
}

/// The quiet reminder after "Not now", while Kass is around and midna still isn't trusted.
pub fn status_item(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if m.ax_prompt || !m.seen.contains(SEEN_KEY) || !crate::kass::handshake_detected() || trusted() {
        return None;
    }
    let fg = t.fg;
    Some(
        div()
            .id("ax-needed")
            .flex()
            .gap(px(4.))
            .cursor_pointer()
            .text_color(t.need)
            .hover(move |s| s.text_color(fg))
            .child("Kass needs Accessibility")
            .on_click(cx.listener(|m, _, _, cx| open(m, cx)))
            .into_any_element(),
    )
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !m.ax_prompt {
        return None;
    }
    let asked = crate::lifecycle::snapshot().is_some_and(|s| s.ax_fix_clicked);
    let (body, primary) = if asked {
        ("Switched midna on? macOS applies the change only to a new process, so midna relaunches. Your terminals keep running.", "Relaunch midna")
    } else {
        ("Kass is dictating here. Switch midna on under Privacy & Security ▸ Accessibility so Kass can read and edit what you type.", "Open Accessibility")
    };
    Some(
        div()
            .id("ax-prompt")
            .w(px(380.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .rounded(px(10.))
            .bg(t.raised)
            .border_1()
            .border_color(t.need)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().font_weight(FontWeight::BOLD).child("Let Kass read and edit what you type"))
            .child(div().text_color(t.dim).child(body))
            .child(div().text_size(px(11.5)).text_color(t.need).child(
                "Programs in midna terminals, agents included, may inherit this permission (docs/SECURITY.md ▸ TCC responsibility).",
            ))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(btn(t, "ax-later", "Not now").on_click(cx.listener(|m, _, _, cx| dismiss(m, cx))))
                    .child(btn_primary(t, "ax-go", primary).on_click(cx.listener(move |_, _, _, cx| {
                        if asked {
                            crate::lifecycle::command(crate::lifecycle::Cmd::Relaunch, cx);
                        } else {
                            cx.open_url(PANE_AX);
                            crate::lifecycle::note_ax_fix();
                            cx.notify();
                        }
                    }))),
            )
            .into_any_element(),
    )
}
