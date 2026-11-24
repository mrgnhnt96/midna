//! Monitor grants (the Monitor grant canvas, options B and C,
//! https://claude.ai/artifact/V8QTV2YcVeeSXXqjCkXkyY): a terminal whose agent may answer other
//! terminals' prompts and approvals, until a time or until it's revoked.
//!
//! A row's right-click menu grants one ("Monitor other tabs ›": what it answers, then for how
//! long), and so does ⌘K ("monitor 2am", `commands::monitor_rows`). A live grant gets a line of
//! its own on the row (an eye, the time left and what it answers) with Revoke always showing.
//! Granting and revoking are human-only calls; the app makes them as the human.
use crate::app::{MainWindow, Menu, refresh};
use crate::icons::Icon;
use crate::model::Session;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{MonitorGrant, MonitorScope, time};
use serde_json::{Value, json};

/// With less than this left, the row's grant line takes the needs-you colour.
const SOON_SECS: i64 = 15 * 60;

/// The grant `s` has in force at unix time `now`.
pub fn live(s: &Session, now: i64) -> Option<&MonitorGrant> {
    s.monitor_grant.as_ref().filter(|g| g.live_at(now))
}

/// What a grant answers, as the row says it.
pub fn scope_label(scope: MonitorScope) -> &'static str {
    match scope {
        MonitorScope::All => "prompts + approvals",
        MonitorScope::Prompts => "prompts",
        MonitorScope::Approvals => "approvals",
    }
}

/// "45m", "4h", "4h 52m", "2d 3h" (rounded up to the minute).
pub fn span(secs: i64) -> String {
    let mins = (secs.max(0) + 59) / 60;
    match (mins / 1440, mins % 1440 / 60, mins % 60) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

/// The row's grant line ("4h 52m · prompts + approvals", "8m left · prompts" near its end,
/// "no end · approvals"), and whether it ends soon.
pub fn line_text(g: &MonitorGrant, now: i64) -> (String, bool) {
    let what = scope_label(g.scope);
    match g.until.as_deref().and_then(time::parse_rfc3339) {
        None => (format!("no end · {what}"), false),
        Some(t) if t - now < SOON_SECS => (format!("{} left · {what}", span(t - now)), true),
        Some(t) => (format!("{} · {what}", span(t - now)), false),
    }
}

/// "10:08 PM" (local), with the weekday ("Wed 10:08 PM") when it's a day or more after `now`.
pub fn clock(t: i64, now: i64) -> String {
    clock_at(t, now, time::local_offset_at(t))
}

/// `clock` at `offset` seconds east of UTC.
pub fn clock_at(t: i64, now: i64, offset: i64) -> String {
    let (day, secs) = ((t + offset).div_euclid(86_400), (t + offset).rem_euclid(86_400));
    let (h, mi) = (secs / 3600, secs % 3600 / 60);
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    // 1 January 1970 was a Thursday.
    let wd = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][(day + 4).rem_euclid(7) as usize];
    let day = if t - now >= 86_400 { format!("{wd} ") } else { String::new() };
    format!("{day}{h12}:{mi:02} {half}")
}

/// A typed end: a length from `now` ("8h", "90m", "1h30m", "2d") or a time of day ("2am",
/// "5:30pm", "17:30", "noon"), the next time the local clock reads it. `offset` is seconds
/// east of UTC.
pub fn parse_until(word: &str, now: i64, offset: i64) -> Option<i64> {
    if let Some(secs) = length(word) {
        return (secs > 0).then_some(now + secs);
    }
    let mins = midna_proto::keep_awake::parse_time(word).ok()?;
    let day = (now + offset).div_euclid(86_400) * 86_400 - offset;
    let t = day + i64::from(mins) * 60;
    Some(if t <= now { t + 86_400 } else { t })
}

/// "8h", "90m", "1h30m", "2d" in seconds; None for anything else (a bare number is a time).
pub fn length(word: &str) -> Option<i64> {
    let (mut total, mut num, mut unit) = (0i64, String::new(), false);
    for c in word.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let secs = match c {
            'm' => 60,
            'h' => 3600,
            'd' => 86_400,
            _ => return None,
        };
        total += num.parse::<i64>().ok()? * secs;
        num.clear();
        unit = true;
    }
    (unit && num.is_empty()).then_some(total)
}

/// The params `session.monitor_grant` takes.
pub fn grant_params(id: &str, scope: MonitorScope, until: Option<i64>) -> Value {
    let mut p = json!({"id": id, "scope": scope.as_str()});
    if let Some(t) = until {
        p["until"] = json!(time::format_unix(t));
    }
    p
}

/// Grant terminal `id` (the app is the human, so midnad takes it at once).
pub fn grant(m: &mut MainWindow, id: &str, scope: MonitorScope, until: Option<i64>, cx: &mut Context<MainWindow>) {
    let name = m.sessions.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
    let what = match scope {
        MonitorScope::All => "other tabs".to_string(),
        s => format!("other tabs' {}", scope_label(s)),
    };
    let when = match until {
        Some(t) => format!("until {}", clock(t, time::now_unix())),
        None => "until you revoke it".into(),
    };
    m.rpc("session.monitor_grant", grant_params(id, scope, until), cx, move |m, v, _, cx| {
        apply(m, &v);
        m.request_refresh(refresh::SESSIONS, cx);
        m.toast(format!("{name} can answer {what} {when}"), cx);
    });
}

/// End terminal `id`'s grant now.
pub fn revoke(m: &mut MainWindow, id: &str, cx: &mut Context<MainWindow>) {
    let name = m.sessions.iter().find(|s| s.id == id).map(|s| s.name.clone()).unwrap_or_default();
    m.rpc("session.monitor_revoke", json!({"id": id}), cx, move |m, v, _, cx| {
        apply(m, &v);
        m.request_refresh(refresh::SESSIONS, cx);
        m.toast(format!("{name} can no longer answer other tabs"), cx);
    });
}

/// Show the session a grant or revoke returned now; the event's refresh confirms it.
fn apply(m: &mut MainWindow, v: &Value) {
    let Some(id) = v.get("id").and_then(Value::as_str) else {
        return;
    };
    let grant = v.get("monitor_grant").cloned().and_then(|g| serde_json::from_value(g).ok());
    if let Some(s) = m.sessions.iter_mut().find(|s| s.id == id) {
        s.monitor_grant = grant;
    }
}

/// The row's grant line: an eye, the time left and what it answers, and Revoke.
pub fn row_line(s: &Session, t: &Theme, cx: &mut Context<MainWindow>) -> Option<Div> {
    let now = time::now_unix();
    let g = live(s, now)?;
    let (text, soon) = line_text(g, now);
    let color = if soon { t.need } else { t.accent };
    let ends = match g.until.as_deref().and_then(time::parse_rfc3339) {
        Some(u) => format!("until {}", clock(u, now)),
        None => "until you revoke it".into(),
    };
    let tip = format!("Its agent can answer other tabs' {} {ends}", scope_label(g.scope));
    let (id, soft, fg) = (s.id.clone(), t.accent_soft, t.fg);
    Some(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .pl(px(21.))
            .text_size(px(12.))
            .text_color(color)
            .child(Icon::Eye.el(12., color))
            .child(div().id(SharedString::from(format!("row-monitor-{}", s.id))).flex_1().min_w_0().truncate().tooltip(super::header::tip(tip)).child(text))
            .child(
                div()
                    .id(SharedString::from(format!("row-monitor-revoke-{}", s.id)))
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(24.))
                    .px(px(8.))
                    .my(px(-4.))
                    .mr(px(-6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.dim)
                    .hover(move |st| st.bg(soft).text_color(fg))
                    .tooltip(super::header::tip("Revoke the monitor grant"))
                    .child("Revoke")
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |m, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        revoke(m, &id, cx);
                    })),
            ),
    )
}

/// Right-click on a row: its menu opens at the pointer (a second right-click closes it).
pub fn open_menu(m: &mut MainWindow, id: &str, at: Point<Pixels>, cx: &mut Context<MainWindow>) {
    let open = matches!(&m.menu, Menu::Row(x) | Menu::RowMonitor(x, _) if x == id);
    m.status_menu_at = at;
    m.menu = if open { Menu::None } else { Menu::Row(id.to_string()) };
    cx.notify();
}

/// Item height in `sidebar::menu_item` (8px padding round 13px text), to line the submenu up
/// with "Monitor other tabs".
const ITEM_H: f32 = 33.;

/// The row's right-click menu: Rename, Move to background, Monitor other tabs ›, Revoke (while
/// a grant is live) and Close terminal. "Monitor other tabs" opens its submenu beside it.
pub fn row_menu(m: &MainWindow, s: &Session, t: &Theme, cx: &mut Context<MainWindow>) -> Option<impl IntoElement + use<>> {
    let sub = match &m.menu {
        Menu::Row(id) if *id == s.id => None,
        Menu::RowMonitor(id, scope) if *id == s.id => Some(*scope),
        _ => return None,
    };
    let now = time::now_unix();
    let granted = live(s, now).is_some();
    let id = s.id.clone();
    let mut list = super::sidebar::menu_box(t).occlude();
    let rid = id.clone();
    list = list.child(super::sidebar::menu_item(t, &format!("row-menu-rename-{id}"), "Rename", "", cx.listener(move |m, _: &ClickEvent, w, cx| {
        m.menu = Menu::None;
        crate::ui::rename::start(m, &rid, crate::ui::rename::At::Sidebar, w, cx);
    })));
    let (bid, bg) = (id.clone(), !s.background);
    list = list.child(super::sidebar::menu_item(t, &format!("row-menu-bg-{id}"), if bg { "Move to background" } else { "Move to foreground" }, "", cx.listener(move |m, _: &ClickEvent, _, cx| {
        m.menu = Menu::None;
        m.rpc("session.set_background", json!({"id": bid.clone(), "background": bg}), cx, |m: &mut MainWindow, _, _, cx| m.request_refresh(refresh::SESSIONS, cx));
        cx.notify();
    })));
    // A shell has no agent to answer with; a grant it already has can still be revoked.
    let mut row_of_monitor = None;
    if s.agent.is_some() {
        let (cid, hid, accent_soft) = (id.clone(), id.clone(), t.accent_soft);
        row_of_monitor = Some(2.);
        list = list.child(
            super::sidebar::menu_item(t, &format!("row-menu-monitor-{id}"), "Monitor other tabs", "›", cx.listener(move |m, _: &ClickEvent, _, cx| {
                m.menu = if matches!(m.menu, Menu::RowMonitor(..)) { Menu::Row(cid.clone()) } else { Menu::RowMonitor(cid.clone(), MonitorScope::All) };
                cx.notify();
            }))
            .when(sub.is_some(), |d| d.bg(accent_soft))
            .on_hover(cx.listener(move |m, over: &bool, _, cx| {
                if *over && m.menu == Menu::Row(hid.clone()) {
                    m.menu = Menu::RowMonitor(hid.clone(), MonitorScope::All);
                    cx.notify();
                }
            })),
        );
    }
    if granted {
        let vid = id.clone();
        list = list.child(super::sidebar::menu_item(t, &format!("row-menu-revoke-{id}"), "Revoke monitor grant", "", cx.listener(move |m, _: &ClickEvent, _, cx| {
            m.menu = Menu::None;
            revoke(m, &vid, cx);
            cx.notify();
        })));
    }
    let xid = id.clone();
    list = list.child(sep(t)).child(super::sidebar::menu_item(t, &format!("row-menu-close-{id}"), "Close terminal…", "", cx.listener(move |m, _: &ClickEvent, _, cx| {
        m.menu = Menu::None;
        crate::ui::row_close::toggle(m, &xid, cx);
        cx.notify();
    })));
    let side = sub.zip(row_of_monitor).map(|(scope, at)| submenu(s, scope, now, t, cx).mt(px(at * ITEM_H)));
    Some(deferred(anchored().position(m.status_menu_at).snap_to_window_with_margin(px(8.)).child(div().flex().items_start().gap(px(4.)).child(list).children(side))).with_priority(1))
}

/// "Monitor other tabs ›": what it answers (a choice kept while the menu is open), then how
/// long, each of which grants at once.
fn submenu(s: &Session, scope: MonitorScope, now: i64, t: &Theme, cx: &mut Context<MainWindow>) -> Div {
    let mut b = super::sidebar::menu_box(t).occlude().child(cap(t, "IT ANSWERS"));
    for (sc, label) in [(MonitorScope::All, "Prompts and approvals"), (MonitorScope::Prompts, "Prompts only"), (MonitorScope::Approvals, "Approvals only")] {
        let id = s.id.clone();
        b = b.child(
            choice(t, &format!("row-monitor-scope-{}-{}", sc.as_str(), s.id), label, sc == scope).on_click(cx.listener(move |m, _: &ClickEvent, _, cx| {
                m.menu = Menu::RowMonitor(id.clone(), sc);
                cx.notify();
            })),
        );
    }
    b = b.child(sep(t)).child(cap(t, "GRANT"));
    let offset = time::local_offset_secs();
    let eight = parse_until("8am", now, offset).unwrap_or(now + 8 * 3600);
    let lengths: [(&str, String, Option<i64>); 4] = [
        ("For 1 hour", format!("until {}", clock(now + 3600, now)), Some(3600)),
        ("For 4 hours", format!("until {}", clock(now + 4 * 3600, now)), Some(4 * 3600)),
        ("Until 8 AM", span(eight - now), None),
        ("Until I revoke it", String::new(), None),
    ];
    for (n, (label, hint, secs)) in lengths.into_iter().enumerate() {
        let id = s.id.clone();
        b = b.child(super::sidebar::menu_item(t, &format!("row-monitor-grant-{n}-{}", s.id), label, &hint, cx.listener(move |m, _: &ClickEvent, _, cx| {
            m.menu = Menu::None;
            let now = time::now_unix();
            let until = match (n, secs) {
                (_, Some(secs)) => Some(now + secs),
                (2, None) => parse_until("8am", now, time::local_offset_secs()),
                _ => None,
            };
            grant(m, &id, scope, until, cx);
            cx.notify();
        })));
    }
    let (id, name) = (s.id.clone(), s.name.clone());
    b.child(super::sidebar::menu_item(t, &format!("row-monitor-grant-at-{}", s.id), "Until a time…", "", cx.listener(move |m, _: &ClickEvent, w, cx| {
        m.menu = Menu::None;
        let words = match scope {
            MonitorScope::All => String::new(),
            s => format!("{} ", s.as_str()),
        };
        // Select it so "monitor <time>" aims at it even when its name doesn't parse cleanly.
        if m.selected.as_deref() != Some(id.as_str()) {
            m.select(id.clone(), w, cx);
        }
        crate::ui::command_bar::open_with(m, format!("monitor {name} {words}"), w, cx);
    })))
}

/// A submenu caption ("IT ANSWERS").
fn cap(t: &Theme, text: &'static str) -> Div {
    div().px(px(10.)).pt(px(6.)).pb(px(2.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(text)
}

fn sep(t: &Theme) -> Div {
    div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line)
}

/// A radio item: a check beside the one picked.
fn choice(t: &Theme, id: &str, label: &str, on: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id.to_string()))
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(t.accent_soft))
        .child(div().w(px(14.)).flex_none().when(on, |d| d.child(Icon::Check.el(14., t.accent))))
        .child(div().flex_1().child(label.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{clock_at, grant_params, line_text, live, parse_until, span};
    use crate::model::Session;
    use midna_proto::{MonitorGrant, MonitorScope, time};
    use serde_json::json;

    // 2026-11-23T22:30:00Z
    const NOW: i64 = 1_795_473_000;

    fn grant(scope: MonitorScope, until: Option<i64>) -> MonitorGrant {
        MonitorGrant { scope, granted_at: time::format_unix(NOW - 60), until: until.map(time::format_unix) }
    }

    #[test]
    fn spans_round_up_to_the_minute() {
        assert_eq!(span(30), "1m");
        assert_eq!(span(45 * 60), "45m");
        assert_eq!(span(4 * 3600), "4h");
        assert_eq!(span(4 * 3600 + 52 * 60 - 20), "4h 52m");
        assert_eq!(span(2 * 86_400 + 3 * 3600), "2d 3h");
        assert_eq!(span(-5), "0m");
    }

    #[test]
    fn the_row_line_says_time_left_and_what_it_answers() {
        assert_eq!(line_text(&grant(MonitorScope::All, Some(NOW + 4 * 3600 + 52 * 60)), NOW), ("4h 52m · prompts + approvals".into(), false));
        assert_eq!(line_text(&grant(MonitorScope::Prompts, Some(NOW + 8 * 60)), NOW), ("8m left · prompts".into(), true));
        assert_eq!(line_text(&grant(MonitorScope::Approvals, None), NOW), ("no end · approvals".into(), false));
    }

    #[test]
    fn only_a_grant_still_in_force_shows() {
        let mut s = Session { monitor_grant: Some(grant(MonitorScope::All, Some(NOW + 60))), ..Default::default() };
        assert!(live(&s, NOW).is_some());
        assert!(live(&s, NOW + 60).is_none());
        s.monitor_grant = None;
        assert!(live(&s, NOW).is_none());
    }

    #[test]
    fn typed_ends_are_lengths_or_the_next_time_the_clock_reads_it() {
        assert_eq!(parse_until("8h", NOW, 0), Some(NOW + 8 * 3600));
        assert_eq!(parse_until("90m", NOW, 0), Some(NOW + 90 * 60));
        assert_eq!(parse_until("1h30m", NOW, 0), Some(NOW + 5400));
        // 22:30 UTC: 2am is tomorrow; 11pm is in half an hour.
        assert_eq!(parse_until("2am", NOW, 0), time::parse_rfc3339("2026-11-24T02:00:00Z"));
        assert_eq!(parse_until("11pm", NOW, 0), time::parse_rfc3339("2026-11-23T23:00:00Z"));
        // UTC-7: it's 15:30 there, so 5pm is today.
        assert_eq!(parse_until("5pm", NOW, -7 * 3600), time::parse_rfc3339("2026-11-24T00:00:00Z"));
        for word in ["prompts", "approvals", "revoke", "fix", "0m", ""] {
            assert_eq!(parse_until(word, NOW, 0), None, "{word}");
        }
    }

    #[test]
    fn clock_labels_say_the_weekday_a_day_or_more_ahead() {
        assert_eq!(clock_at(NOW, NOW, 0), "10:30 PM");
        assert_eq!(clock_at(NOW + 3 * 3600 + 1800, NOW, 0), "2:00 AM");
        assert_eq!(clock_at(NOW + 13 * 3600 + 30 * 60, NOW, 0), "12:00 PM");
        // 2026-11-23 is a Monday.
        assert_eq!(clock_at(NOW + 86_400, NOW, 0), "Tue 10:30 PM");
        assert_eq!(clock_at(NOW, NOW, -7 * 3600), "3:30 PM");
    }

    #[test]
    fn grant_params_leave_out_until_for_no_end() {
        assert_eq!(grant_params("s1", MonitorScope::Prompts, None), json!({"id": "s1", "scope": "prompts"}));
        assert_eq!(grant_params("s1", MonitorScope::All, Some(NOW)), json!({"id": "s1", "scope": "all", "until": "2026-11-23T22:30:00Z"}));
    }
}
