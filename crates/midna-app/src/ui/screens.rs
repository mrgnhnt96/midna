//! Placeholder views for screens the next agents build (Rules-B, Triggers-A, Insights-A,
//! CommandBar-A, NeedsYou-C), plus the "midnad not running" and empty states.
use crate::app::MainWindow;
use crate::backend::ConnState;
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

fn code(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().px(px(10.)).py(px(6.)).rounded(px(7.)).bg(t.raised).border_1().border_color(t.line).font_family(t.mono_font.clone()).text_size(px(12.5)).child(s.into())
}

pub fn not_running(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    if let Some(mac) = m.backend.mac() {
        if m.remote_loading() {
            return connecting(&mac.name, connect_step(&m.conn), t, cx).into_any_element();
        }
        return not_reached(&m.conn, &mac.name, t, cx).into_any_element();
    }
    local_not_running(m, t).into_any_element()
}

/// The connecting card's steps: finding the Mac on the network, proving it's mine (the key),
/// loading its terminals.
const STEPS: [&str; 3] = ["Found it on this network", "Checking it's your Mac", "Loading its terminals"];
/// While a step is the one under way it reads as what's happening.
const STEPS_NOW: [&str; 3] = ["Finding it on this network", "Checking it's your Mac", "Loading its terminals"];

/// The step another Mac's window is on (`STEPS`): the link's stage from this Mac's midnad while
/// it connects, then loading once calls go through.
pub fn connect_step(conn: &ConnState) -> usize {
    use midna_proto::nearby::{DriveStage, DriveState};
    match conn {
        ConnState::Connected => 2,
        ConnState::Link(p) if p.state == DriveState::Connected => 2,
        ConnState::Link(p) if p.stage == Some(DriveStage::Proving) => 1,
        _ => 0,
    }
}

/// Escape while another Mac's window connects: Cancel, as the card's button.
pub fn cancel_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if ev.keystroke.key != "escape" || ev.keystroke.modifiers.modified() || !m.remote_loading() {
        return false;
    }
    if m.menu != crate::app::Menu::None || m.overlay != crate::app::Overlay::None {
        return false;
    }
    crate::ui::close_window::request(m, window, cx);
    true
}

/// Another Mac's window until its terminals arrive (design "Remote Mac windows", 3 Connecting,
/// state B): a card in the pane with the steps, so a slow connection says where it's stuck.
/// Cancel (esc) closes the window.
fn connecting(name: &str, step: usize, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(44.))
        .pl(px(18.))
        .pr(px(10.))
        .border_b_1()
        .border_color(t.line)
        .child(crate::icons::spinner("connecting-header", 11., t.dim))
        .child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(format!("Connecting to {name}…")))
        .children([Icon::Split, Icon::PopOut, Icon::Restart, Icon::Dots].map(|i| div().size(px(32.)).flex().items_center().justify_center().opacity(0.4).child(i.el(16., t.dim))));
    let steps = div().flex().flex_col().gap(px(6.)).children(STEPS.iter().enumerate().map(|(i, label)| {
        let row = div().flex().items_center().gap(px(8.));
        if i < step {
            row.text_color(t.ok).child(Icon::Check.el(12., t.ok)).child(*label)
        } else if i == step {
            row.text_color(t.fg).child(crate::icons::spinner(SharedString::from(format!("connecting-step-{i}")), 12., t.fg)).child(STEPS_NOW[i])
        } else {
            row.text_color(t.dim).child(Icon::Ring.el(12., t.dim)).child(*label)
        }
    }));
    let cancel = div()
        .id("connecting-cancel")
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(28.))
        .px(px(12.))
        .rounded(px(6.))
        .bg(t.raised)
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.opacity(0.85))
        .child("Cancel")
        .child(div().font_family(t.mono_font.clone()).text_size(px(11.)).opacity(0.8).child("esc"))
        .on_click(cx.listener(|m, _, w, cx| crate::ui::close_window::request(m, w, cx)));
    let card = div()
        .w(px(416.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(20.))
        .rounded(px(12.))
        .bg(t.panel)
        .border_1()
        .border_color(t.line)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.25), offset: point(px(0.), px(12.)), blur_radius: px(40.), spread_radius: px(0.), inset: false }])
        .child(div().flex().items_center().gap(px(8.)).text_size(px(16.)).font_weight(FontWeight::BOLD).child(Icon::Mac.el(14., t.ansi[6])).child(format!("Connecting to {name}")))
        .child(steps)
        .child(div().flex().justify_end().child(cancel));
    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(div().flex_1().bg(t.term).flex().items_center().justify_center().p(px(16.)).child(card))
}

/// Another Mac's window once its link is down, or when it can't be reached: why, from this
/// Mac's daemon (reconnecting, offline, not allowed).
fn not_reached(conn: &ConnState, name: &str, t: &Theme, _cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    use midna_proto::nearby::DriveState;
    let (title, detail, dot) = match conn {
        ConnState::NotRunning { error, .. } => (format!("Can't reach {name}"), error.clone(), t.err),
        ConnState::Link(p) if p.state == DriveState::Offline => (format!("{name} is offline"), p.describe(name), t.err),
        ConnState::Link(p) => (format!("Reconnecting to {name}…"), p.describe(name), t.need),
        _ => (format!("Connecting to {name}…"), String::new(), t.dim),
    };
    div().flex_1().min_w_0().h_full().flex().flex_col().child(div().h(px(44.)).flex_none().border_b_1().border_color(t.line)).child(
        div().flex_1().bg(t.term).flex().items_center().justify_center().child(
            div()
                .w(px(460.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(20.))
                .rounded(px(12.))
                .bg(t.panel)
                .border_1()
                .border_color(t.line)
                .child(div().flex().items_center().gap(px(10.)).child(div().size(px(9.)).rounded_full().bg(dot)).child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title)))
                .child(div().text_color(t.dim).child(format!("Its terminals and agents run on {name}; this window shows them over a link from this Mac.")))
                .when(!detail.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(t.dim).child(detail)))
                .child(div().text_size(px(11.5)).text_color(t.dim).child("Retrying every second…")),
        ),
    )
}

fn local_not_running(m: &MainWindow, t: &Theme) -> impl IntoElement + use<> {
    let restarting = m.daemon_restarting();
    let (title, detail) = match &m.conn {
        ConnState::NotRunning { .. } if restarting => ("midnad restarting…", "Your terminals keep running; this window reconnects in a moment.".to_string()),
        ConnState::NotRunning { error, .. } => ("midnad not running", error.clone()),
        _ => ("Connecting to midnad…", String::new()),
    };
    let socket = m.backend.socket_path();
    let home = socket.parent().map(|p| p.display().to_string()).unwrap_or_default();
    div().flex_1().min_w_0().h_full().flex().flex_col().child(div().h(px(44.)).flex_none().border_b_1().border_color(t.line)).child(
        div().flex_1().bg(t.term).flex().items_center().justify_center().child(
            div()
                .w(px(460.))
                .flex()
                .flex_col()
                .gap(px(12.))
                .p(px(20.))
                .rounded(px(12.))
                .bg(t.panel)
                .border_1()
                .border_color(t.line)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .child(div().size(px(9.)).rounded_full().bg(if restarting {
                            t.need
                        } else if matches!(m.conn, ConnState::NotRunning { .. }) {
                            t.err
                        } else {
                            t.dim
                        }))
                        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title)),
                )
                .when(!restarting, |d| {
                    d.child(div().text_color(t.dim).child("The midna daemon owns your terminals. Start it and this window connects on its own.")).child(code(t, "midnad"))
                })
                .child(div().text_size(px(12.)).text_color(t.dim).child(format!("Socket: {}", socket.display())))
                .when(!home.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(t.dim).child(format!("MIDNA_HOME={home} midnad  (to use this home)"))))
                .when(!detail.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(t.dim).child(detail)))
                .child(div().text_size(px(11.5)).text_color(t.dim).child("Retrying every second…")),
        ),
    )
}

pub fn empty_terminal(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let hint = format!("No terminal selected. {} opens a terminal, {} an agent.", m.key_label("keys.new_terminal"), m.key_label("keys.new_agent"));
    let no_projects = m.sessions.is_empty() && m.conn == ConnState::Connected;
    let recent = if no_projects { crate::commands::recent_projects(&m.projects, &[]) } else { vec![] };
    let add_folder = div()
        .id("pane-add-folder")
        .text_size(px(12.))
        .text_color(t.accent)
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(cx.listener(|m, _, window, cx| crate::ui::command_bar::open_with(m, format!("{}~/", crate::commands::FOLDER_PREFIX), window, cx)))
        .child("Add project folder…");
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(14.))
        .items_center()
        .justify_center()
        .text_color(t.dim)
        .when(no_projects, |d| {
            d.when(!recent.is_empty(), |d| d.child(recent_list(t, recent, cx)))
                .child(crate::ui::sidebar::open_project_button(m, t, "pane-open-project", cx))
                .child(
                    div()
                        .flex()
                        .gap(px(14.))
                        .items_center()
                        .text_size(px(12.))
                        .child(format!("{} terminal at root", m.key_label("keys.new_terminal")))
                        .child("·")
                        .child(add_folder),
                )
        })
        .when(!no_projects, |d| d.child(hint))
}

/// VS Code style "recent" list of closed projects.
fn recent_list(t: &Theme, recent: Vec<crate::commands::RecentProject>, cx: &mut Context<MainWindow>) -> Div {
    let mut list = div().w(px(420.)).flex().flex_col().gap(px(2.)).child(super::caps_label(t, "Recent").px(px(10.)).pb(px(4.)));
    for (i, r) in recent.into_iter().take(crate::commands::RECENT_MAX).enumerate() {
        let path = r.path.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("recent-{i}")))
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(36.))
                .px(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, ev: &ClickEvent, window, cx| m.add_project(path.clone(), ev.modifiers().alt, window, cx)))
                .child(crate::icons::Icon::Project.el(14., t.fg))
                .child(div().flex_none().text_color(t.fg).font_weight(FontWeight::BOLD).child(r.name))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(r.sub)),
        );
    }
    list
}

#[cfg(test)]
mod tests {
    use super::connect_step;
    use crate::backend::{ConnState, LinkProgress};
    use midna_proto::nearby::{DriveStage, DriveState};

    fn link(state: DriveState, stage: Option<DriveStage>) -> ConnState {
        ConnState::Link(LinkProgress { state, stage, reason: None })
    }

    #[test]
    fn the_connecting_card_follows_the_links_stages() {
        assert_eq!(connect_step(&link(DriveState::Connecting, None)), 0, "nothing heard yet: finding it");
        assert_eq!(connect_step(&link(DriveState::Connecting, Some(DriveStage::Finding))), 0);
        assert_eq!(connect_step(&link(DriveState::Connecting, Some(DriveStage::Proving))), 1, "found: checking it's my Mac");
        assert_eq!(connect_step(&link(DriveState::Connected, None)), 2, "the link is up: loading");
        assert_eq!(connect_step(&ConnState::Connected), 2, "calls go through, the list is still to come");
    }
}
