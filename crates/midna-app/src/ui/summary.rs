//! A project's Summary (design C on the Project Summary canvas,
//! https://claude.ai/artifact/DPdaEwsdrQgP263HeWc76c): the terminals that closed in it lately
//! (`summary.list`), a list on the left and the one you pick on the right, with apps' panels
//! (`summary.annotate`) beside it.
//!
//! The project's … menu opens it (`keys.project_summary`). A project whose terminals have all
//! closed keeps its heading with a Summary row instead of leaving the sidebar; closing that row
//! (its ✕, or ⌘W on the Summary) lets the project go until another terminal closes in it.
use super::screen_kit::{clock, day_label};
use crate::actions::{Kind, overlay, overlay_context};
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::model::Glyph;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{ActorKind, AgentKind, ClosedTerminal, LinkKind, SessionKind, StatusState, SummaryListResult, SummaryPanel};
use serde_json::json;
use std::collections::HashMap;

#[derive(Default)]
pub struct SummaryPage {
    /// Every project's records, newest first.
    pub closed: Vec<ClosedTerminal>,
    pub keep_days: u32,
    /// The project on screen.
    pub project: Option<String>,
    /// The record on the right (its id); None = the newest.
    sel: Option<String>,
    /// Projects whose Summary row was closed, with the newest record then (`app-state.json`
    /// `summary_closed`). The row comes back once a newer one closes.
    pub dismissed: HashMap<String, String>,
}

impl SummaryPage {
    pub fn new(dismissed: HashMap<String, String>) -> SummaryPage {
        SummaryPage { dismissed, ..Default::default() }
    }

    pub fn records<'a>(&'a self, pid: &'a str) -> impl Iterator<Item = &'a ClosedTerminal> + 'a {
        self.closed.iter().filter(move |c| c.project_id == pid)
    }

    fn newest(&self, pid: &str) -> Option<&str> {
        self.closed.iter().find(|c| c.project_id == pid).map(|c| c.closed_at.as_str())
    }

    /// A project with no terminals keeps its heading for its Summary row: it has records, and
    /// none closed since the row was closed.
    pub fn keeps(&self, pid: &str) -> bool {
        let Some(newest) = self.newest(pid) else { return false };
        let seconds = |ts: &str| midna_proto::time::parse_rfc3339(ts).unwrap_or(0);
        self.dismissed.get(pid).is_none_or(|d| seconds(newest) > seconds(d))
    }
}

/// The project on screen, when the Summary is.
pub fn shown(m: &MainWindow) -> Option<&str> {
    m.summary.project.as_deref().filter(|_| m.screen == Screen::Summary)
}

/// Whether `pid`'s group shows a Summary row: its Summary is on screen, or it has no
/// terminals and keeps its heading for one.
pub fn row_wanted(m: &MainWindow, pid: &str, empty: bool) -> bool {
    shown(m) == Some(pid) || (empty && m.summary.keeps(pid))
}

pub fn fetch(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.rpc("summary.list", json!({}), cx, |m, v, _, cx| {
        let Ok(r) = serde_json::from_value::<SummaryListResult>(v) else { return };
        apply(m, r, cx);
    });
}

pub fn apply(m: &mut MainWindow, r: SummaryListResult, cx: &mut Context<MainWindow>) {
    m.summary.closed = r.closed;
    m.summary.keep_days = r.keep_days;
    if m.summary.sel.as_ref().is_some_and(|s| !m.summary.closed.iter().any(|c| &c.id == s)) {
        m.summary.sel = None;
    }
    cx.notify();
}

/// Show `pid`'s Summary (from the project's … menu, its Summary row or ⌥⌘Y).
pub fn open(m: &mut MainWindow, pid: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if shown(m) == Some(pid.as_str()) {
        return;
    }
    m.summary.project = Some(pid);
    m.summary.sel = None;
    if m.screen != Screen::Summary {
        m.set_screen(Screen::Summary, window, cx);
    }
    fetch(m, cx);
    cx.notify();
}

/// Close `pid`'s Summary row: a project with no terminals leaves the sidebar until another
/// terminal closes in it. Leaves its Summary when it's on screen.
pub fn close_row(m: &mut MainWindow, pid: &str, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(newest) = m.summary.newest(pid).map(str::to_string) {
        m.summary.dismissed.insert(pid.to_string(), newest);
        crate::ui::statusbar::save_state(m);
    }
    if shown(m) == Some(pid) {
        m.set_screen(Screen::Terminal, window, cx);
    }
    cx.notify();
}

fn list(m: &MainWindow) -> Vec<&ClosedTerminal> {
    m.summary.project.as_deref().map(|p| m.summary.records(p).collect()).unwrap_or_default()
}

fn current(m: &MainWindow) -> Option<ClosedTerminal> {
    let list = list(m);
    m.summary.sel.as_ref().and_then(|s| list.iter().find(|c| &c.id == s)).or(list.first()).map(|c| (*c).clone())
}

fn step(m: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    let ids: Vec<String> = list(m).iter().map(|c| c.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    let at = m.summary.sel.as_ref().and_then(|s| ids.iter().position(|i| i == s)).unwrap_or(0) as isize;
    m.summary.sel = Some(ids[(at + by).clamp(0, ids.len() as isize - 1) as usize].clone());
    cx.notify();
}

fn resume(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.rpc("summary.resume", json!({ "id": id }), cx, |m, v, window, cx| {
        m.summary.closed.retain(|c| Some(&c.id) != v["session"]["id"].as_str().map(str::to_string).as_ref());
        m.set_screen(Screen::Terminal, window, cx);
        m.show_opened(v, window, cx);
        fetch(m, cx);
    });
    cx.notify();
}

fn remove(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    // Optimistic; `summary.changed` refetches. The next one down takes its place.
    let ids: Vec<String> = list(m).iter().map(|c| c.id.clone()).collect();
    let at = ids.iter().position(|i| *i == id).unwrap_or(0);
    m.summary.sel = ids.iter().filter(|i| **i != id).nth(at.min(ids.len().saturating_sub(2))).cloned();
    m.summary.closed.retain(|c| c.id != id);
    m.rpc("summary.remove", json!({ "id": id }), cx, |_, _, _, _| {});
    cx.notify();
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<MainWindow>) {
    match ev.keystroke.key.as_str() {
        "down" | "j" => step(m, 1, cx),
        "up" | "k" => step(m, -1, cx),
        _ => return,
    }
    cx.stop_propagation();
}

/// The status a record ended in, as its dot's color.
fn state_color(t: &Theme, c: &ClosedTerminal) -> Hsla {
    match c.state {
        StatusState::Done => t.ok,
        StatusState::Failed => t.err,
        StatusState::NeedsYou => t.need,
        StatusState::Working => t.work,
        _ => t.dim,
    }
}

fn ring(color: Hsla, size: f32) -> Div {
    super::border_w(div().size(px(size)).flex_none().rounded_full(), 2.).border_color(color)
}

/// Who closed it, as the detail's "Closed by" says it.
fn closed_by(m: &MainWindow, c: &ClosedTerminal) -> String {
    if c.how == "exited" {
        return match c.exit_code {
            Some(code) => format!("Its process exited ({code})"),
            None => "Its process exited".into(),
        };
    }
    match (&c.by.kind, c.by.session.as_deref()) {
        (ActorKind::Human, _) => "You".into(),
        (ActorKind::Agent, Some(s)) if s == c.id => "Its agent".into(),
        (ActorKind::Agent, Some(s)) if m.sessions.iter().any(|x| x.id == s) => super::notifications::source(m, Some(s), None),
        (ActorKind::Agent, _) => "An agent".into(),
        _ => "midna".into(),
    }
}

/// "14:02" today, else "26 Oct".
fn when(ts: &str) -> String {
    let day = day_label(ts);
    if day.starts_with("today") { clock(ts).chars().take(5).collect() } else { day }
}

/// "6m", "2h 5m", "less than a minute".
fn ran(c: &ClosedTerminal) -> String {
    let secs = |ts: &str| midna_proto::time::parse_rfc3339(ts).unwrap_or(0);
    let d = (secs(&c.closed_at) - secs(&c.opened_at)).max(0);
    match d {
        0..=59 => "less than a minute".into(),
        60..=3599 => format!("{}m", d / 60),
        _ if d % 3600 < 60 => format!("{}h", d / 3600),
        _ => format!("{}h {}m", d / 3600, d % 3600 / 60),
    }
}

/// The list row's second line: the first app's `line` in its color, else how it ended.
fn line(m: &MainWindow, t: &Theme, c: &ClosedTerminal) -> (String, Hsla) {
    let at = when(&c.closed_at);
    if let Some(p) = c.line() {
        let color = p.color.as_deref().map_or(t.dim, |x| t.status_color(x));
        return (format!("{} · {at}", p.line.clone().unwrap_or_default()), color);
    }
    let (text, color) = match c.state {
        StatusState::Failed => (c.reason.clone().unwrap_or_else(|| "failed".into()), t.err),
        _ if c.how == "exited" => (c.exit_code.map_or("exited".into(), |e| format!("exited {e}")), t.dim),
        _ => (format!("closed by {}", closed_by(m, c).to_lowercase()), t.dim),
    };
    (format!("{text} · {at}"), color)
}

fn glyph(c: &ClosedTerminal) -> Glyph {
    match (c.kind, c.agent) {
        (_, Some(AgentKind::Claude)) => Glyph::Claude,
        (_, Some(AgentKind::Codex)) => Glyph::Codex,
        (SessionKind::Monitor, _) => Glyph::Monitor,
        _ => Glyph::Shell,
    }
}

fn caps(t: &Theme, text: &str) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).mb(px(6.)).child(text.to_uppercase())
}

fn card(t: &Theme) -> Div {
    div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).bg(t.panel).overflow_hidden()
}

pub fn render(m: &mut MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let m = &*m;
    let pid = m.summary.project.clone().unwrap_or_default();
    let project = m.projects.iter().find(|p| p.id == pid).map(|p| p.name.clone()).unwrap_or_default();
    let records = list(m);
    let cur = current(m);
    let days = match m.summary.keep_days {
        0 | 1 => "today".to_string(),
        n => format!("last {n} days"),
    };

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(16.))
        .bg(t.panel)
        .border_b_1()
        .border_color(t.line)
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Summary"))
        .child(div().text_color(t.dim).truncate().child(project))
        .child(div().flex_1())
        .child(div().flex_none().text_size(px(12.)).text_color(t.dim).child(days.clone()));

    let mut rows = div().id("sum-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll().py(px(6.));
    for c in &records {
        let on = cur.as_ref().is_some_and(|x| x.id == c.id);
        let (text, color) = line(m, t, c);
        let id = c.id.clone();
        rows = rows.child(
            div()
                .id(SharedString::from(format!("sum-row-{}", c.id)))
                .relative()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(14.))
                .py(px(8.))
                .cursor_pointer()
                .when(on, |d| d.bg(t.raised).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
                .when(!on, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.summary.sel = Some(id.clone());
                    m.menu = Menu::None;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .child(div().w(px(12.)).flex_none().flex().items_center().child(ring(state_color(t, c), 10.)))
                        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(c.name.clone()))
                        .child(Icon::from_glyph(glyph(c)).el(14., t.dim)),
                )
                .child(div().pl(px(21.)).text_size(px(12.)).text_color(color).truncate().child(text)),
        );
    }

    let left = div().flex().flex_col().flex_none().w(px(330.)).h_full().border_r_1().border_color(t.line).child(header).child(rows);

    let right: AnyElement = match cur {
        Some(c) => detail(m, t, &c, cx).into_any_element(),
        None => div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_color(t.dim)
            .child(Icon::Summary.el(22., t.dim))
            .child(format!("No terminals closed here in the {days}."))
            .into_any_element(),
    };

    div()
        .id("summary")
        .key_context(overlay_context(Kind::Screen, overlay::SUMMARY))
        .track_focus(&m.overlay_focus)
        .on_key_down(cx.listener(on_key))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| {
            if m.menu == Menu::Summary {
                m.menu = Menu::None;
                cx.notify();
            } else {
                m.set_screen(Screen::Terminal, w, cx);
            }
        }))
        // ⌘↩ and ⌘⌫ (the approve and deny keys) act on the record on screen.
        .on_action(cx.listener(|m, _: &crate::actions::ApproveOnce, _, cx| {
            if let Some(c) = current(m) {
                resume(m, c.id, cx);
            }
        }))
        .on_action(cx.listener(|m, _: &crate::actions::Deny, _, cx| {
            if let Some(c) = current(m) {
                remove(m, c.id, cx);
            }
        }))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .bg(t.bg)
        .child(left)
        .child(right)
        .into_any_element()
}

/// The right side: everything about one closed terminal.
fn detail(m: &MainWindow, t: &Theme, c: &ClosedTerminal, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let menu_open = m.menu == Menu::Summary;
    let resume_label = if c.conversation_id.is_some() { "Resume conversation" } else { "Open again" };
    let (resume_keys, remove_keys) = (m.key_label("keys.approve"), m.key_label("keys.deny"));
    let (r_id, x_id) = (c.id.clone(), c.id.clone());

    let menu = deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            super::sidebar::menu_box(t)
                .occlude()
                .mt(px(34.))
                .child(super::sidebar::menu_item(t, "sum-resume", resume_label, "", cx.listener(move |m, _, _, cx| resume(m, r_id.clone(), cx))).child(super::header::key_chip(t, resume_keys.into())))
                .child(div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line))
                .child(super::sidebar::menu_item(t, "sum-remove", "Remove from summary", "", cx.listener(move |m, _, _, cx| remove(m, x_id.clone(), cx))).child(super::header::key_chip(t, remove_keys.into()))),
        ),
    )
    .with_priority(1);

    let more = div()
        .id("sum-more")
        .relative()
        .size(px(28.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .when(menu_open, |d| d.bg(t.raised))
        .tooltip(super::header::tip("Actions"))
        .on_click(cx.listener(|m, _, _, cx| {
            cx.stop_propagation();
            m.menu = if m.menu == Menu::Summary { Menu::None } else { Menu::Summary };
            cx.notify();
        }))
        .child(Icon::Dots.el(14., t.dim))
        // Anchored at the button's right edge, so the menu hangs under it.
        .when(menu_open, |d| d.child(div().absolute().top_0().right_0().child(menu)));

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .h(px(52.))
        .px(px(20.))
        .bg(t.panel)
        .border_b_1()
        .border_color(t.line)
        .child(ring(state_color(t, c), 12.))
        .child(div().min_w_0().text_size(px(15.)).font_weight(FontWeight::BOLD).truncate().child(c.name.clone()))
        .child(Icon::from_glyph(glyph(c)).el(14., t.dim))
        .child(div().flex_1())
        .child(more);

    let mut facts: Vec<(&str, String, bool)> = vec![("Closed", when_long(&c.closed_at), false), ("Closed by", closed_by(m, c), false)];
    if c.state == StatusState::Failed {
        facts.push(("Ended", c.reason.clone().unwrap_or_else(|| "failed".into()), false));
    }
    let mut run = vec![ran(c)];
    if c.turns > 0 {
        run.push(if c.turns == 1 { "1 prompt".into() } else { format!("{} prompts", c.turns) });
    }
    if let Some(cost) = c.cost_usd {
        run.push(format!("${cost:.2}"));
    }
    facts.push(("Ran", run.join(" · "), false));
    if let Some(w) = &c.worktree {
        facts.push(("Worktree", w.clone(), true));
    }
    if let Some(b) = &c.branch {
        facts.push(("Branch", b.clone(), true));
    }
    let mut grid = div().flex().flex_col().gap(px(6.));
    for (label, value, mono) in facts {
        grid = grid.child(
            div()
                .flex()
                .gap(px(12.))
                .child(div().flex_none().w(px(110.)).text_color(t.dim).child(label))
                .child(div().min_w_0().when(mono, |d| d.font_family(t.mono_font.clone()).text_size(px(12.))).child(value)),
        );
    }

    let prompts = (!c.prompts.is_empty()).then(|| {
        let mut col = div().flex().flex_col().gap(px(4.));
        for p in &c.prompts {
            col = col.child(
                div()
                    .flex()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .bg(t.panel)
                    .child(div().flex_none().w(px(52.)).text_size(px(12.)).text_color(t.dim).child(when(&p.at)))
                    .child(div().min_w_0().child(p.text.lines().next().unwrap_or("").to_string())),
            );
        }
        let more = (c.turns as usize > c.prompts.len()).then(|| div().text_size(px(12.)).text_color(t.dim).child(format!("and {} more", c.turns as usize - c.prompts.len())));
        div().flex().flex_col().child(caps(t, "Your prompts")).child(col).children(more)
    });

    let main = div().flex_1().min_w_0().flex().flex_col().gap(px(18.)).child(grid).children(prompts);

    let mut side = div().flex_none().w(px(300.)).flex().flex_col().gap(px(12.));
    for (owner, p) in &c.apps {
        side = side.child(panel(t, owner, p));
    }
    if !c.links.is_empty() {
        let mut links = card(t).p(px(12.)).gap(px(6.)).child(caps(t, "Links").mb(px(0.)));
        for (i, l) in c.links.iter().enumerate() {
            let (target, file) = (l.target.clone(), l.kind == LinkKind::File);
            links = links.child(
                div()
                    .id(SharedString::from(format!("sum-link-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .cursor_pointer()
                    .text_color(t.accent)
                    .hover(|s| s.text_color(t.fg))
                    .tooltip(super::header::tip(l.target.clone()))
                    .on_click(cx.listener(move |m, _, _, cx| open_link(m, &target, file, cx)))
                    .child(link_icon(l.kind).el(13., t.dim))
                    .child(div().min_w_0().truncate().child(l.title.clone())),
            );
        }
        side = side.child(links);
    }

    div()
        .id("sum-detail")
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .child(header)
        .child(div().id("sum-detail-body").flex_1().min_h_0().overflow_y_scroll().flex().items_start().gap(px(18.)).px(px(20.)).py(px(18.)).child(main).child(side))
}

/// "today 14:02" or "26 Oct 14:02".
fn when_long(ts: &str) -> String {
    let day = day_label(ts);
    if day.starts_with("today") { day } else { format!("{day} {}", clock(ts).chars().take(5).collect::<String>()) }
}

fn link_icon(k: LinkKind) -> Icon {
    match k {
        LinkKind::Pr => Icon::Pr,
        LinkKind::Artifact => Icon::Artifact,
        LinkKind::Web => Icon::Globe,
        LinkKind::File => Icon::File,
    }
}

/// A link from a closed terminal: URLs open in their app; a file opens in the IDE rules pick,
/// as a click in a terminal would (its terminal is gone, so any open one stands in for it).
fn open_link(m: &mut MainWindow, target: &str, file: bool, cx: &mut Context<MainWindow>) {
    if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
        eprintln!("midna-app debug-term: link {target}"); // dev runs never launch apps
    } else if file {
        match m.sessions.first().map(|s| s.id.clone()) {
            Some(sid) => crate::terminal::open_file(m.backend.clone(), sid, target.to_string(), None),
            None => cx.open_url(&format!("file://{target}")),
        }
    } else {
        cx.open_url(target);
    }
}

/// An app's panel: its name, the title (a link when it has a URL), a colored badge with a
/// subtitle, and its buttons.
fn panel(t: &Theme, owner: &str, p: &SummaryPanel) -> impl IntoElement + use<> {
    let color = p.color.as_deref().map_or(t.accent, |c| t.status_color(c));
    let app = p.app.clone().unwrap_or_else(|| owner.to_string());
    let initial: String = app.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let link = |id: String, text: String, url: Option<String>, fg: Hsla, bold: bool| {
        let d = div().id(SharedString::from(id)).min_w_0().text_color(fg).when(bold, |d| d.font_weight(FontWeight::BOLD)).child(text);
        match url {
            Some(u) => d.cursor_pointer().hover(|s| s.text_color(t.accent)).on_click(move |_, _, cx| cx.open_url(&u)),
            None => d,
        }
    };
    let mut buttons = div().flex().flex_wrap().gap(px(6.));
    for (i, b) in p.buttons.iter().enumerate() {
        let url = b.open.clone();
        buttons = buttons.child(super::screen_kit::btn(t, SharedString::from(format!("sum-app-{owner}-{i}")), b.label.clone()).on_click(move |_, _, cx| cx.open_url(&url)));
    }
    card(t)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .border_b_1()
                .border_color(t.line)
                .child(
                    div()
                        .size(px(18.))
                        .flex_none()
                        .rounded(px(4.))
                        .bg(t.accent)
                        .text_color(t.accent_fg)
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(initial),
                )
                .child(div().flex_1().min_w_0().font_weight(FontWeight::BOLD).truncate().child(app)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(12.))
                .py(px(10.))
                .child(link(format!("sum-app-{owner}-title"), p.title.clone(), p.url.clone(), t.fg, true))
                .when(p.badge.is_some() || p.subtitle.is_some(), |d| {
                    d.child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(px(8.))
                            .children(p.badge.clone().map(|b| div().px(px(8.)).rounded(px(10.)).bg(color.opacity(0.16)).text_color(color).text_size(px(12.)).font_weight(FontWeight::BOLD).child(b)))
                            .children(p.subtitle.clone().map(|s| link(format!("sum-app-{owner}-sub"), s, p.subtitle_url.clone(), t.accent, false).text_size(px(12.5)))),
                    )
                })
                .when(!p.buttons.is_empty(), |d| d.child(buttons)),
        )
}

/// The Summary row in a project's group: the list icon, "Summary", and when the last
/// terminal closed. A click shows it; its ✕ closes the row (`close_row`).
pub fn sidebar_row(m: &MainWindow, t: &Theme, pid: &str, compact: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let on = shown(m) == Some(pid);
    let n = m.summary.records(pid).count();
    let last = m.summary.newest(pid).map(|ts| super::screen_kit::ago(Some(ts))).map(|a| format!("last closed {a}"));
    let sub = match (n, last) {
        (0, _) | (_, None) => "nothing closed lately".to_string(),
        (_, Some(l)) => l,
    };
    let (open_id, close_id) = (pid.to_string(), pid.to_string());
    let group = SharedString::from(format!("sum-side-{pid}"));
    div()
        .id(SharedString::from(format!("summary-row-{pid}")))
        .relative()
        .flex()
        .flex_col()
        .gap(px(2.))
        .w_full()
        .px(px(14.))
        .py(px(if compact { 5. } else { 8. }))
        .cursor_pointer()
        .group(group.clone())
        .when(on, |d| d.bg(t.raised).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .when(!on, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
        .tooltip(super::header::tip_keys("This project's closed terminals", "keys.project_summary"))
        .on_click(cx.listener(move |m, _, w, cx| open(m, open_id.clone(), w, cx)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .child(div().w(px(12.)).flex_none().flex().items_center().child(Icon::Summary.el(12., if on { t.accent } else { t.dim })))
                .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child("Summary"))
                .child(
                    div()
                        .id(SharedString::from(format!("summary-x-{pid}")))
                        .flex_none()
                        .size(px(16.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover(group, |s| s.opacity(1.))
                        .hover(|s| s.bg(t.line))
                        .tooltip(super::header::tip("Close the Summary"))
                        .on_click(cx.listener(move |m, _, w, cx| {
                            cx.stop_propagation();
                            close_row(m, &close_id, w, cx);
                        }))
                        .child(Icon::Cross.el(10., t.dim)),
                ),
        )
        .when(!compact, |d| d.child(div().pl(px(21.)).text_size(px(12.)).text_color(t.dim).truncate().child(sub)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::SummaryPage;
    use midna_proto::{Actor, ClosedTerminal, SessionKind, StatusState};

    fn rec(project: &str, closed_at: &str) -> ClosedTerminal {
        ClosedTerminal {
            id: format!("s-{closed_at}"),
            project_id: project.into(),
            name: "t".into(),
            kind: SessionKind::Shell,
            agent: None,
            cwd: "/tmp".into(),
            conversation_id: None,
            agent_args: vec![],
            worktree: None,
            branch: None,
            opened_at: closed_at.into(),
            closed_at: closed_at.into(),
            how: "close".into(),
            by: Actor::human(),
            state: StatusState::Idle,
            exit_code: None,
            reason: None,
            prompts: vec![],
            turns: 0,
            cost_usd: None,
            links: vec![],
            apps: Default::default(),
        }
    }

    #[test]
    fn a_closed_row_comes_back_when_a_newer_terminal_closes() {
        let mut page = SummaryPage { closed: vec![rec("p1", "2026-10-27T12:00:00Z")], ..Default::default() };
        assert!(page.keeps("p1"));
        assert!(!page.keeps("p2"), "nothing closed there");
        page.dismissed.insert("p1".into(), "2026-10-27T12:00:00Z".into());
        assert!(!page.keeps("p1"));
        page.closed.insert(0, rec("p1", "2026-10-27T13:00:00Z"));
        assert!(page.keeps("p1"));
    }
}
