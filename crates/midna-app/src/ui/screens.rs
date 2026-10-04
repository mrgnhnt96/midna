//! Placeholder views for screens the next agents build (Rules-B, Triggers-A, Insights-A,
//! CommandBar-A, NeedsYou-C), plus the "midnad not running" and empty states.
use crate::app::MainWindow;
use crate::backend::ConnState;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

fn code(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().px(px(10.)).py(px(6.)).rounded(px(7.)).bg(t.raised).border_1().border_color(t.line).font_family(t.mono_font.clone()).text_size(px(12.5)).child(s.into())
}

pub fn not_running(m: &MainWindow, t: &Theme, _cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
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
                .on_click(cx.listener(move |m, _, window, cx| m.add_project(path.clone(), window, cx)))
                .child(crate::icons::Icon::Project.el(14., t.fg))
                .child(div().flex_none().text_color(t.fg).font_weight(FontWeight::BOLD).child(r.name))
                .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).child(r.sub)),
        );
    }
    list
}
