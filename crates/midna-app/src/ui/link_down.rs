//! Another Mac's window with its link down after it showed its terminals (design 3 Reconnecting
//! and Offline, option A): a strip under the header, the last screen dimmed with typing off, and
//! the sidebar's rows greyed "as of" when the link dropped. midnad's drive link tries again on
//! its own (every few seconds; once it's been offline a minute, every 30 s while the Mac can be
//! seen, and otherwise when it's seen again or this Mac's network changes), so the window comes
//! back by itself; Try now tries it at once. See `MainWindow::remote_down`.
use crate::app::{Down, MainWindow};
use crate::icons::Icon;
use crate::theme::Theme;
use crate::ui::screen_kit;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// "14:02": when the link dropped (local time).
pub fn when(m: &MainWindow) -> String {
    m.lost_at.map(screen_kit::hm).unwrap_or_default()
}

/// The sidebar's heading over its greyed rows: "As of 14:02" while it reconnects, "Last seen
/// 14:02" once it's offline.
pub fn stale_label(down: Down, at: &str) -> String {
    match down {
        Down::Reconnecting => format!("As of {at}"),
        Down::Offline => format!("Last seen {at}"),
    }
}

/// "14 min ago": how long ago the link dropped, `secs` seconds back.
pub fn ago(secs: i64) -> String {
    match secs.max(0) {
        0..=59 => "just now".into(),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s @ 3600..=86399 => format!("{} h ago", s / 3600),
        s if s < 2 * 86400 => "1 day ago".into(),
        s => format!("{} days ago", s / 86400),
    }
}

/// The strip's message: the bold part, then the rest.
pub fn message(down: Down, name: &str, at: &str, secs: i64) -> (String, String) {
    match down {
        Down::Reconnecting => {
            (format!("Lost {name}. Reconnecting…"), "Its terminals keep running there. Typing is off until it's back, and isn't saved up.".into())
        }
        Down::Offline => (format!("{name} is offline."), format!("Last seen {at} ({}). It connects again when it's back.", ago(secs))),
    }
}

fn button(t: &Theme, id: &'static str, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(28.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .border_1()
        .text_size(px(12.5))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .map(|d| if primary { d.bg(t.accent).border_color(t.accent).text_color(t.accent_fg) } else { d.bg(t.raised).border_color(t.line).text_color(t.fg) })
        .hover(|s| s.opacity(0.85))
        .child(label)
}

/// The strip under the header while another Mac's link is down: amber and spinning while it
/// reconnects, red once it's offline (with Close window). Try now asks for a try at once.
pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let down = m.remote_down()?;
    let name = m.backend.mac()?.name.clone();
    let secs = m.lost_at.map_or(0, |at| screen_kit::now_unix() - at);
    let (bold, rest) = message(down, &name, &when(m), secs);
    let (tint, icon) = match down {
        Down::Reconnecting => (t.need.opacity(0.12), crate::icons::spinner("link-down-spin", 14., t.need).into_any_element()),
        Down::Offline => (t.err.opacity(0.10), Icon::Mac.el(15., t.err).into_any_element()),
    };
    let try_now = button(t, "link-try-now", "Try now", down == Down::Offline).on_click(cx.listener(|m, _, _, cx| {
        m.retry_link();
        cx.notify();
    }));
    let close = (down == Down::Offline).then(|| {
        button(t, "link-close-window", "Close window", false).on_click(cx.listener(|m, _, window, cx| crate::ui::close_window::request(m, window, cx)))
    });
    Some(
        div()
            .flex_none()
            .bg(t.bg)
            .border_b_1()
            .border_color(t.line)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .min_h(px(36.))
                    .py(px(6.))
                    .pl(px(16.))
                    .pr(px(10.))
                    .bg(tint)
                    .text_size(px(13.))
                    .text_color(t.fg)
                    .child(div().flex_none().flex().items_center().child(icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_wrap()
                            .gap_x(px(4.))
                            .child(div().flex_none().font_weight(FontWeight::BOLD).child(bold))
                            .child(div().min_w_0().child(rest)),
                    )
                    .children(close)
                    .child(try_now),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::{ago, message, stale_label};
    use crate::app::{down, Down};
    use crate::backend::{ConnState, LinkProgress};
    use midna_proto::nearby::DriveState;

    fn link(state: DriveState) -> ConnState {
        ConnState::Link(LinkProgress { state, ..LinkProgress::starting() })
    }

    #[test]
    fn only_another_macs_window_that_showed_its_terminals_goes_down() {
        let reconnecting = link(DriveState::Reconnecting);
        assert_eq!(down(true, true, &reconnecting), Some(Down::Reconnecting));
        assert_eq!(down(true, true, &ConnState::Connecting), Some(Down::Reconnecting));
        assert_eq!(down(true, true, &link(DriveState::Offline)), Some(Down::Offline));
        // Its first list still to come: the connecting card, not a strip.
        assert_eq!(down(true, false, &reconnecting), None);
        // This Mac's own window, and a link that's up.
        assert_eq!(down(false, true, &reconnecting), None);
        assert_eq!(down(true, true, &ConnState::Connected), None);
        // A link that closed says why on its card instead.
        let closed = ConnState::NotRunning { socket: "/x".into(), error: "Bravo stopped letting this Mac drive it".into() };
        assert_eq!(down(true, true, &closed), None);
    }

    #[test]
    fn the_strip_says_how_long_its_been_gone() {
        assert_eq!(ago(20), "just now");
        assert_eq!(ago(14 * 60 + 30), "14 min ago");
        assert_eq!(ago(3 * 3600), "3 h ago");
        assert_eq!(ago(30 * 3600), "1 day ago");
        assert_eq!(ago(3 * 86400), "3 days ago");
        let (bold, rest) = message(Down::Offline, "Bravo", "14:02", 14 * 60);
        assert_eq!(bold, "Bravo is offline.");
        assert_eq!(rest, "Last seen 14:02 (14 min ago). It connects again when it's back.");
        assert_eq!(message(Down::Reconnecting, "Bravo", "14:02", 5).0, "Lost Bravo. Reconnecting…");
        assert_eq!(stale_label(Down::Reconnecting, "14:02"), "As of 14:02");
        assert_eq!(stale_label(Down::Offline, "14:02"), "Last seen 14:02");
    }
}
