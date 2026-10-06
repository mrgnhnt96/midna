//! Notifications screen (B on the toast + history canvas): every notification midna recorded
//! (`notify.history`), newest first, with the one you pick on the right. The status bar's bell
//! opens it (`statusbar::bell`) and shows how many are unread; opening it marks every one read
//! (`notify.read`), and what was new stays marked New until you leave.
use super::screen_kit::{ago, clock, clock_or_day, day_label};
use crate::app::{MainWindow, Screen};
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{NotifyHistoryItem, NotifyHistoryResult};
use serde_json::json;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    All,
    Needs,
    Finished,
    Failed,
    Sent,
    Other,
}

const TABS: [(Tab, &str); 6] = [(Tab::All, "All"), (Tab::Needs, "Needs you"), (Tab::Finished, "Finished"), (Tab::Failed, "Failed"), (Tab::Sent, "Sent to you"), (Tab::Other, "Other")];

#[derive(Default)]
pub struct Inbox {
    pub items: Vec<NotifyHistoryItem>,
    pub unread: u32,
    pub read_seq: u64,
    /// `read_seq` when the screen opened: what came after it shows New until you leave.
    new_after: Option<u64>,
    /// The notification shown on the right (its seq); None = the newest.
    sel: Option<u64>,
    tab: Tab,
}

pub fn tab_of(category: &str) -> Tab {
    match category {
        "approval" | "attention" | "requests" => Tab::Needs,
        "turn_done" | "background" => Tab::Finished,
        "failed" => Tab::Failed,
        "agent" | "from_trigger" => Tab::Sent,
        _ => Tab::Other,
    }
}

/// The kind's name (as in Settings) and its color.
pub fn kind(t: &Theme, category: &str) -> (&'static str, Hsla) {
    let label = midna_proto::notify::category(category).map(|c| c.label).unwrap_or("Notification");
    let color = match tab_of(category) {
        Tab::Needs => t.need,
        Tab::Finished => t.ok,
        Tab::Failed => t.err,
        Tab::Sent => t.accent,
        _ => t.dim,
    };
    (label, color)
}

/// The terminal's name, else its project's, else "midna".
pub fn source(m: &MainWindow, session: Option<&str>, project: Option<&str>) -> String {
    let s = session.and_then(|id| m.sessions.iter().find(|s| s.id == id));
    let p = project.or(s.and_then(|s| s.project_id.as_deref())).and_then(|id| m.projects.iter().find(|p| p.id == id));
    s.map(|s| s.name.clone()).or_else(|| p.map(|p| p.name.clone())).unwrap_or_else(|| "midna".into())
}

pub fn fetch(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.rpc("notify.history", json!({ "limit": 300 }), cx, |m, v, _, cx| {
        let Ok(r) = serde_json::from_value::<NotifyHistoryResult>(v) else { return };
        m.inbox.items = r.items;
        m.inbox.unread = r.unread;
        m.inbox.read_seq = r.read_seq;
        // Looking at the screen: what just came in is read already.
        if m.screen == Screen::Notifications && r.unread > 0 {
            mark_read(m, cx);
        }
        cx.notify();
    });
}

fn mark_read(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.rpc("notify.read", json!({}), cx, |m, v, _, cx| {
        m.inbox.read_seq = v["read_seq"].as_u64().unwrap_or(m.inbox.read_seq);
        m.inbox.unread = v["unread"].as_u64().unwrap_or(0) as u32;
        cx.notify();
    });
}

/// `notify.posted` (a new one: fetch) and `notify.read` (another window looked: clear the badge).
pub fn on_event(m: &mut MainWindow, e: &Event, cx: &mut Context<MainWindow>) {
    if e.kind == midna_proto::kinds::NOTIFY_POSTED && e.data.get("test") != Some(&json!(true)) {
        fetch(m, cx);
    } else if e.kind == midna_proto::kinds::NOTIFY_READ {
        m.inbox.read_seq = e.data["read_seq"].as_u64().unwrap_or(m.inbox.read_seq);
        m.inbox.unread = e.data["unread"].as_u64().unwrap_or(0) as u32;
        let read = m.inbox.read_seq;
        m.inbox.items.iter_mut().for_each(|i| i.unread = i.seq > read);
        cx.notify();
    }
}

/// The screen opened: remember what was new, then mark everything read.
pub fn opened(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.inbox.new_after = Some(m.inbox.read_seq);
    m.inbox.sel = None;
    mark_read(m, cx);
    fetch(m, cx);
}

pub fn closed(m: &mut MainWindow) {
    m.inbox.new_after = None;
}

fn shown(m: &MainWindow) -> Vec<&NotifyHistoryItem> {
    m.inbox.items.iter().filter(|i| m.inbox.tab == Tab::All || tab_of(&i.notification.category) == m.inbox.tab).collect()
}

fn is_new(m: &MainWindow, i: &NotifyHistoryItem) -> bool {
    m.inbox.new_after.is_some_and(|s| i.seq > s)
}

/// The open needs-you item a notification is about, while it still waits.
fn waiting<'a>(m: &'a MainWindow, i: &NotifyHistoryItem) -> Option<&'a NeedsYou> {
    i.notification.needs_you_id.as_deref().and_then(|id| m.needs.iter().find(|n| n.id == id))
}

fn go_to(m: &mut MainWindow, session: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.screen != Screen::Terminal {
        m.set_screen(Screen::Terminal, window, cx);
    }
    cx.defer(move |cx| crate::windows::reveal(session, cx));
}

fn step(m: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    let list = shown(m);
    if list.is_empty() {
        return;
    }
    let at = m.inbox.sel.and_then(|s| list.iter().position(|i| i.seq == s)).unwrap_or(0) as isize;
    let next = (at + by).clamp(0, list.len() as isize - 1) as usize;
    m.inbox.sel = Some(list[next].seq);
    cx.notify();
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    match ev.keystroke.key.as_str() {
        "down" | "j" => step(m, 1, cx),
        "up" | "k" => step(m, -1, cx),
        "enter" => {
            let list = shown(m);
            let cur = m.inbox.sel.and_then(|s| list.iter().find(|i| i.seq == s)).or(list.first()).and_then(|i| i.session_id.clone());
            if let Some(sid) = cur {
                go_to(m, sid, window, cx);
            }
        }
        _ => return,
    }
    cx.stop_propagation();
}

pub fn render(m: &mut MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let m = &*m;
    let list = shown(m);
    let cur = m.inbox.sel.and_then(|s| list.iter().copied().find(|i| i.seq == s)).or(list.first().copied()).cloned();
    let new_n = m.inbox.items.iter().filter(|i| is_new(m, i)).count();
    let subtitle = match new_n {
        0 => "Everything midna told you, newest first".to_string(),
        n => format!("{n} new since you last looked · all marked read"),
    };

    let mut tabs = div().flex().flex_none().flex_wrap().gap(px(4.)).px(px(14.)).py(px(8.)).border_b_1().border_color(t.line);
    for (tab, label) in TABS {
        let n = m.inbox.items.iter().filter(|i| tab == Tab::All || tab_of(&i.notification.category) == tab).count();
        if tab != Tab::All && n == 0 {
            continue;
        }
        let on = m.inbox.tab == tab;
        tabs = tabs.child(
            div()
                .id(SharedString::from(format!("nt-tab-{label}")))
                .flex()
                .items_center()
                .gap(px(5.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(6.))
                .when(on, |d| d.bg(t.accent_soft))
                .text_color(if on { t.accent } else { t.dim })
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .child(label)
                .child(div().opacity(0.7).child(n.to_string()))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.inbox.tab = tab;
                    m.inbox.sel = None;
                    cx.notify();
                })),
        );
    }

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
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Notifications"))
        .child(div().text_color(t.dim).child(subtitle))
        .child(div().flex_1())
        .child(div().text_color(t.dim).text_size(px(11.5)).child("↑↓ move · ↵ go to terminal · esc back"));

    let body: AnyElement = if m.inbox.items.is_empty() {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .text_color(t.dim)
            .child(svg().path(Icon::Bell.path()).size(px(22.)).text_color(t.dim))
            .child("Nothing yet. Approvals, questions, failures and finished turns land here.")
            .into_any_element()
    } else {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .child(rows(m, t, &list, cur.as_ref().map(|c| c.seq), cx))
            .child(div().flex_1().min_w_0().h_full().children(cur.map(|c| detail(m, t, &c, cx))))
            .into_any_element()
    };

    div()
        .id("notifications")
        .key_context("MidnaOverlay")
        .track_focus(&m.overlay_focus)
        .on_key_down(cx.listener(on_key))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| m.set_screen(Screen::Terminal, w, cx)))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(t.bg)
        .child(header)
        .child(tabs)
        .child(body)
        .into_any_element()
}

/// The left column: one row per notification, under a day heading.
fn rows(m: &MainWindow, t: &Theme, list: &[&NotifyHistoryItem], cur: Option<u64>, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mut col = div().id("nt-list").flex_none().w(px(480.)).h_full().overflow_y_scroll().border_r_1().border_color(t.line).pb(px(12.));
    let mut last_day = String::new();
    for i in list {
        let day = day_label(&i.at);
        let day = day.split(' ').next().unwrap_or("").to_string();
        let day = if day == "today" { "Today".to_string() } else { day_label(&i.at) };
        if day != last_day {
            col = col.child(div().px(px(16.)).pt(px(12.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(day.to_uppercase()));
            last_day = day;
        }
        let (label, color) = kind(t, &i.notification.category);
        let seq = i.seq;
        let on = cur == Some(seq);
        let body = i.notification.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        let waits = waiting(m, i).is_some();
        col = col.child(
            div()
                .id(SharedString::from(format!("nt-row-{seq}")))
                .flex()
                .gap(px(10.))
                .px(px(16.))
                .py(px(9.))
                .when(on, |d| d.bg(t.accent_soft))
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.inbox.sel = Some(seq);
                    cx.notify();
                }))
                .child(div().flex_none().w(px(52.)).text_size(px(12.)).text_color(t.dim).child(clock(&i.at).chars().take(5).collect::<String>()))
                .child(div().flex_none().mt(px(6.)).size(px(8.)).rounded_full().bg(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().font_weight(FontWeight::BOLD).child(source(m, i.session_id.as_deref(), i.project_id.as_deref())))
                                .child(div().text_size(px(11.5)).text_color(t.dim).child(label))
                                .when(is_new(m, i), |d| d.child(badge(t, "New", t.accent, t.accent_fg)))
                                .when(waits, |d| d.child(badge(t, "Waiting", t.need_soft, t.need))),
                        )
                        .child(div().text_size(px(12.5)).text_color(t.dim).truncate().child(body)),
                ),
        );
    }
    col
}

fn badge(_t: &Theme, text: &'static str, bg: Hsla, fg: Hsla) -> Div {
    div().flex_none().px(px(5.)).rounded(px(4.)).bg(bg).text_color(fg).text_size(px(10.5)).font_weight(FontWeight::BOLD).child(text)
}

/// The right side: everything about one notification, and what you can do about it.
fn detail(m: &MainWindow, t: &Theme, i: &NotifyHistoryItem, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let (label, color) = kind(t, &i.notification.category);
    let need = waiting(m, i).cloned();
    let session = i.session_id.clone().filter(|s| m.sessions.iter().any(|x| &x.id == s));
    let project = i.project_id.as_deref().or(session.as_deref().and_then(|s| m.sessions.iter().find(|x| x.id == s)).and_then(|s| s.project_id.as_deref()));
    let project = project.and_then(|p| m.projects.iter().find(|x| x.id == p)).map(|p| p.name.clone());
    let name = source(m, i.session_id.as_deref(), i.project_id.as_deref());
    let shown_as = match (i.notification.push, i.notification.push_focused) {
        (_, true) => "a banner, even for the terminal in front of you",
        (true, false) => "a banner (or just its sound when you were looking at that terminal)",
        (false, false) => "recorded only (no banner)",
    };
    let category = i.notification.category.clone();

    let mut actions = div().flex().flex_wrap().gap(px(8.));
    if let Some(sid) = session.clone() {
        actions = actions.child(super::screen_kit::btn_primary(t, "nt-go", "Go to terminal").on_click(cx.listener(move |m, _, w, cx| go_to(m, sid.clone(), w, cx))));
    }
    if let Some(n) = need.clone().filter(|n| n.approval.is_some()) {
        let (a, d) = (n.id.clone(), n.id.clone());
        actions = actions
            .child(super::screen_kit::btn(t, "nt-approve", "Approve once").on_click(cx.listener(move |m, _, _, cx| m.resolve(a.clone(), Resolution::Approve { scope: ApprovalScope::Once }, cx))))
            .child(super::screen_kit::btn(t, "nt-deny", "Deny").on_click(cx.listener(move |m, _, _, cx| m.resolve(d.clone(), Resolution::Deny, cx))));
    }
    if let Some(sid) = session {
        let mute = format!("Mute “{label}” for {name}");
        actions = actions.child(super::screen_kit::btn(t, "nt-mute", mute).on_click(cx.listener(move |m, _, _, cx| {
            let params = json!({ "session": sid, "key": category, "value": false });
            m.rpc("notify.set", params, cx, move |m, _, _, cx| m.toast(format!("Muted “{label}” for this terminal. Its … menu turns it back on."), cx));
        })));
    }

    div()
        .id("nt-detail")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(14.))
        .px(px(26.))
        .py(px(22.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().size(px(9.)).rounded_full().bg(color))
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(label.to_uppercase()))
                .child(div().text_color(t.dim).child(format!("· {} · {}", ago(Some(&i.at)), clock_or_day(&i.at))))
                .when(need.is_some(), |d| d.child(badge(t, "Still waiting", t.need_soft, t.need))),
        )
        .child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).child(name))
                .children(project.map(|p| div().text_color(t.dim).child(format!("in {p}")))),
        )
        .when(!i.notification.body.is_empty(), |d| d.child(div().text_size(px(13.5)).line_height(px(21.)).child(i.notification.body.clone())))
        .children(need.and_then(|n| n.screen_excerpt).filter(|e| !e.is_empty()).map(|lines| {
            div()
                .p(px(12.))
                .rounded(px(9.))
                .bg(t.term)
                .border_1()
                .border_color(t.line)
                .font_family(t.mono_font.clone())
                .text_size(px(12.))
                .text_color(t.dim)
                .children(lines.into_iter().map(|l| div().whitespace_nowrap().child(l)))
        }))
        .child(actions)
        .child(div().text_size(px(12.)).text_color(t.dim).child(format!("Shown as {shown_as}.")))
}

/// The status bar's bell: far left, with the unread count; opens (or closes) the screen.
pub fn bell(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let open = m.screen == Screen::Notifications;
    let unread = m.inbox.unread;
    let tip = match unread {
        0 => "Notifications".to_string(),
        1 => "Notifications: 1 unread".to_string(),
        n => format!("Notifications: {n} unread"),
    };
    div()
        .id("nt-bell")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(20.))
        .px(px(6.))
        .ml(px(-8.))
        .rounded(px(5.))
        .border_1()
        .border_color(if open { t.accent } else { t.panel })
        .when(open, |d| d.bg(t.accent_soft))
        .text_color(if open { t.accent } else if unread > 0 { t.fg } else { t.dim })
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .tooltip(super::header::tip(tip))
        .on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Notifications, w, cx)))
        .child(svg().path(Icon::Bell.path()).size(px(13.)).text_color(if open { t.accent } else if unread > 0 { t.fg } else { t.dim }))
        .when(unread > 0 && !open, |d| {
            d.child(
                div()
                    .px(px(5.))
                    .rounded_full()
                    .bg(t.accent)
                    .text_color(t.accent_fg)
                    .text_size(px(10.5))
                    .font_weight(FontWeight::BOLD)
                    .child(if unread > 99 { "99+".to_string() } else { unread.to_string() }),
            )
        })
        .into_any_element()
}
