//! The sidebar footer: the Insights card (`sidebar.footer.stats`, `sidebar.footer.range`) and
//! the buttons under it (`sidebar.footer.buttons`), full or compact (`sidebar.footer.compact`,
//! independent of the terminal rows' `density`). Right-click either for the menu; Customize
//! swaps stats in place and adds or removes buttons. Design: docs/DECISIONS.md "Insights widgets".
use super::caps_label;
use super::sidebar::{menu_box, menu_item};
use crate::actions::*;
use crate::app::{MainWindow, Menu, Screen};
use crate::model::Today;
use crate::theme::Theme;
use crate::icons::Icon;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::settings::{DEFAULT_FOOTER_BUTTONS, DEFAULT_FOOTER_STATS, FOOTER_BUTTONS};
use serde_json::{Value, json};

/// Card slots shown.
const SLOTS: usize = 3;
const MAX_BUTTONS: usize = 4;

/// The picker's groups.
const STAT_GROUPS: [(&str, &[&str]); 2] = [("Activity", &["turns", "messages", "spend", "approvals", "triggers"]), ("Time", &["working", "waiting", "reply", "longest", "peak"])];

fn list(m: &MainWindow, key: &str, default: &[&str]) -> Vec<String> {
    match m.settings.get(key) {
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_default(),
        None => default.iter().map(|s| s.to_string()).collect(),
    }
}

fn stats(m: &MainWindow) -> Vec<String> {
    let mut s = list(m, "sidebar.footer.stats", DEFAULT_FOOTER_STATS);
    s.truncate(SLOTS);
    s
}

fn buttons(m: &MainWindow) -> Vec<String> {
    let mut b = list(m, "sidebar.footer.buttons", DEFAULT_FOOTER_BUTTONS);
    b.truncate(MAX_BUTTONS);
    b
}

fn week(m: &MainWindow) -> bool {
    m.settings.get("sidebar.footer.range").and_then(Value::as_str) == Some("week")
}

fn compact(m: &MainWindow) -> bool {
    m.settings.get("sidebar.footer.compact").and_then(Value::as_bool).unwrap_or(false) || crate::dev::var("MIDNA_DEBUG_FOOTER").as_deref() == Ok("compact")
}

/// What the refresh should fetch for the card: its range, and whether `insights.detail` is
/// needed (a stat that comes from it, or Customize, whose picker shows every value).
pub fn wants(m: &MainWindow) -> (&'static str, bool) {
    let detail = m.footer_editing || stats(m).iter().any(|s| matches!(s.as_str(), "peak" | "reply" | "longest"));
    (if week(m) { "week" } else { "today" }, detail)
}

/// (value, label, short label) of stat `key`.
fn stat(today: &Today, key: &str) -> (String, &'static str, &'static str) {
    let dur = |s: Option<i64>| s.map(|s| crate::ui::charts::duration(s as f64)).unwrap_or_else(|| "–".into());
    match key {
        "turns" => (today.turns.to_string(), "agent turns", "turns"),
        "messages" => (today.messages.to_string(), "messages sent", "msgs"),
        "spend" => (format!("${:.2}", today.spend_usd), "spent", ""),
        "working" => (dur(Some(today.working_secs as i64)), "agents working", "working"),
        "waiting" => (dur(Some(today.waiting_secs as i64)), "waited on you", "waited"),
        "approvals" => (today.approvals.to_string(), "approvals", "approved"),
        "triggers" => (today.triggers_fired.to_string(), "triggers fired", "triggers"),
        "peak" => (today.peak.map(|p| p.to_string()).unwrap_or_else(|| "–".into()), "peak agents", "peak"),
        "reply" => (dur(today.reply_secs), "median reply", "reply"),
        "longest" => (dur(today.longest_secs), "longest turn", "longest"),
        _ => (String::new(), "", ""),
    }
}

fn stat_name(key: &str) -> &'static str {
    match key {
        "turns" => "Agent turns",
        "messages" => "Messages sent",
        "spend" => "Spent",
        "working" => "Agents working",
        "waiting" => "Waited on you",
        "approvals" => "Approvals",
        "triggers" => "Triggers fired",
        "peak" => "Peak agents at once",
        "reply" => "Median reply to needs-you",
        "longest" => "Longest turn",
        _ => "",
    }
}

fn button_info(key: &str) -> (Icon, &'static str, &'static str) {
    match key {
        "triggers" => (Icon::Triggers, "Triggers", "keys.triggers"),
        "rules" => (Icon::Rules, "Rules", "keys.rules"),
        "settings" => (Icon::Settings, "Settings", "keys.settings"),
        "insights" => (Icon::Screen, "Insights", "keys.insights"),
        _ => (Icon::Bell, "Needs you", "keys.needs_you"),
    }
}

fn press(m: &mut MainWindow, key: &str, w: &mut Window, cx: &mut Context<MainWindow>) {
    match key {
        "triggers" => m.set_screen(Screen::Triggers, w, cx),
        "rules" => m.set_screen(Screen::Rules, w, cx),
        "settings" => crate::ui::settings::open(m.backend.clone(), cx),
        "insights" => m.set_screen(Screen::Insights, w, cx),
        _ => w.dispatch_action(Box::new(OpenNeedsYou), cx),
    }
}

fn active(m: &MainWindow, key: &str) -> bool {
    match key {
        "triggers" => m.screen == Screen::Triggers,
        "rules" => m.screen == Screen::Rules,
        "insights" => m.screen == Screen::Insights,
        _ => false,
    }
}

fn save(m: &mut MainWindow, key: &'static str, value: Value, cx: &mut Context<MainWindow>) {
    m.settings.insert(key.into(), value.clone());
    m.rpc("settings.set", json!({"key": key, "value": value}), cx, |_, _, _, _| {});
    if matches!(key, "sidebar.footer.range" | "sidebar.footer.stats") {
        m.request_refresh(crate::app::refresh::INSIGHTS, cx);
    }
    cx.notify();
}

fn reset(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    for k in ["sidebar.footer.stats", "sidebar.footer.range", "sidebar.footer.buttons", "sidebar.footer.compact"] {
        m.settings.remove(k);
        m.rpc("settings.reset", json!({"key": k}), cx, |_, _, _, _| {});
    }
    m.request_refresh(crate::app::refresh::INSIGHTS | crate::app::refresh::SETTINGS, cx);
    cx.notify();
}

fn open_menu(m: &mut MainWindow, ev: &MouseDownEvent, cx: &mut Context<MainWindow>) {
    cx.stop_propagation();
    m.status_menu_at = ev.position;
    m.menu = if m.menu == Menu::Footer { Menu::None } else { Menu::Footer };
    cx.notify();
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let editing = m.footer_editing;
    let stats = stats(m);
    let buttons = buttons(m);
    let compact = compact(m) && !editing;
    let mut col = div()
        .id("sidebar-footer")
        .relative()
        .flex()
        .flex_col()
        .gap(px(if compact { 4. } else { 8. }))
        .p(px(if compact { 6. } else { 10. }))
        .border_t_1()
        .border_color(t.line)
        .on_mouse_down(MouseButton::Right, cx.listener(|m, ev: &MouseDownEvent, _, cx| open_menu(m, ev, cx)))
        .children(super::onboarding::checklist(m, t, cx));
    if editing {
        col = col.child(edit_strip(t, cx));
    }
    if editing {
        col = col.child(edit_card(m, t, &stats, cx));
    } else if !stats.is_empty() {
        col = col.child(if compact { compact_card(m, t, &stats, cx) } else { card(m, t, &stats, cx) });
    }
    if editing {
        col = col.child(edit_buttons(m, t, &buttons, cx));
    } else if !buttons.is_empty() {
        col = col.child(button_row(m, t, &buttons, compact, cx));
    }
    if !editing && stats.is_empty() && buttons.is_empty() {
        // Nothing left to right-click: a thin strip keeps the menu reachable.
        col = col.p(px(0.)).h(px(8.)).cursor_context_menu();
    }
    col.when(m.menu == Menu::Footer, |d| d.child(context_menu(m, t, cx)))
}

fn card(m: &MainWindow, t: &Theme, stats: &[String], cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let mut row = div().flex().justify_between().gap(px(8.));
    for k in stats {
        let (v, label, _) = stat(&m.today, k);
        row = row.child(
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(v))
                .child(div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child(label)),
        );
    }
    div()
        .id("today")
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(9.))
        .border_1()
        .border_color(if m.screen == Screen::Insights { t.accent } else { t.line })
        .bg(t.raised)
        .cursor_pointer()
        .tooltip(super::header::tip_keys("Insights", "keys.insights"))
        .on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Insights, w, cx)))
        .child(div().flex().items_baseline().gap(px(8.)).child(caps_label(t, if week(m) { "This week" } else { "Today" }).flex_1()).child(div().text_size(px(11.)).text_color(t.dim).child("Insights ›")))
        .child(row)
}

fn compact_card(m: &MainWindow, t: &Theme, stats: &[String], cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let mut row = div()
        .id("today")
        .h(px(28.))
        .px(px(8.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(7.))
        .bg(t.raised)
        .border_1()
        .border_color(if m.screen == Screen::Insights { t.accent } else { t.raised })
        .overflow_hidden()
        .whitespace_nowrap()
        .text_size(px(11.5))
        .text_color(t.dim)
        .cursor_pointer()
        .tooltip(super::header::tip_keys("Insights", "keys.insights"))
        .on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Insights, w, cx)))
        .child(div().text_size(px(10.)).font_weight(FontWeight::BOLD).child(if week(m) { "WEEK" } else { "TODAY" }));
    for k in stats {
        let (v, _, short) = stat(&m.today, k);
        row = row.child(div().flex().items_baseline().gap(px(3.)).child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(t.fg).child(v)).child(short));
    }
    row
}

fn button_row(m: &MainWindow, t: &Theme, buttons: &[String], compact: bool, cx: &mut Context<MainWindow>) -> Div {
    let mut row = div().flex().gap(px(if compact { 2. } else { 4. }));
    for b in buttons {
        let (icon, label, setting) = button_info(b);
        let on = active(m, b);
        let key = b.clone();
        let btn = div()
            .id(SharedString::from(format!("btn-{b}")))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .text_color(if on { t.fg } else { t.dim })
            .when(on, |d| d.bg(t.raised))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .tooltip(super::header::tip_keys(label, setting))
            .on_click(cx.listener(move |m, _, w, cx| press(m, &key, w, cx)));
        row = row.child(if compact {
            btn.w(px(32.)).h(px(28.)).child(icon.el(16., if on { t.fg } else { t.dim }))
        } else {
            btn.flex_1().flex_col().gap(px(3.)).pt(px(7.)).pb(px(5.)).text_size(px(10.5)).child(icon.el(16., if on { t.fg } else { t.dim })).child(label)
        });
    }
    row
}

// ------------------------------------------------------------------ customize

fn small_button(t: &Theme, id: &str, label: &str, primary: bool) -> Stateful<Div> {
    div()
        .id(SharedString::from(id.to_string()))
        .h(px(24.))
        .px(px(9.))
        .flex()
        .items_center()
        .rounded(px(6.))
        .text_size(px(11.5))
        .cursor_pointer()
        .when(primary, |d| d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD))
        .when(!primary, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
        .child(label.to_string())
}

fn edit_strip(t: &Theme, cx: &mut Context<MainWindow>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .px(px(2.))
        .child(div().flex_1().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.accent).child("CUSTOMIZING"))
        .child(small_button(t, "footer-reset", "Reset", false).on_click(cx.listener(|m, _, _, cx| reset(m, cx))))
        .child(small_button(t, "footer-done", "Done", true).on_click(cx.listener(|m, _, _, cx| {
            m.footer_editing = false;
            m.menu = Menu::None;
            cx.notify();
        })))
}

fn edit_card(m: &MainWindow, t: &Theme, stats: &[String], cx: &mut Context<MainWindow>) -> Div {
    let wk = week(m);
    let seg = |id: &'static str, label: &'static str, on: bool, value: &'static str| {
        div()
            .id(id)
            .h(px(22.))
            .px(px(8.))
            .flex()
            .items_center()
            .rounded(px(5.))
            .text_size(px(10.5))
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .when(on, |d| d.bg(t.accent_soft).text_color(t.fg))
            .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
            .on_click(cx.listener(move |m, _, _, cx| save(m, "sidebar.footer.range", json!(value), cx)))
            .child(label)
    };
    let hide = div()
        .id("footer-hide-stats")
        .size(px(24.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.))
        .text_size(px(11.))
        .text_color(t.dim)
        .cursor_pointer()
        .hover(|s| s.bg(t.panel).text_color(t.fg))
        .tooltip(super::header::tip("Hide the stats"))
        .on_click(cx.listener(|m, _, _, cx| save(m, "sidebar.footer.stats", json!([]), cx)))
        .child("✕");
    let range_row = div().flex().items_center().gap(px(2.)).child(seg("footer-today", "TODAY", !wk, "today")).child(seg("footer-week", "WEEK", wk, "week")).child(div().flex_1()).child(hide);
    if stats.is_empty() {
        return div().child(
            div()
                .id("footer-show-stats")
                .h(px(34.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(9.))
                .border_1()
                .border_dashed()
                .border_color(t.line)
                .text_size(px(12.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg))
                .on_click(cx.listener(|m, _, _, cx| save(m, "sidebar.footer.stats", json!(DEFAULT_FOOTER_STATS), cx)))
                .child("Stats hidden · Show"),
        );
    }
    let mut slots = div().flex().gap(px(6.));
    for (i, k) in stats.iter().enumerate() {
        let (v, label, _) = stat(&m.today, k);
        let open = m.menu == Menu::FooterStat(i);
        slots = slots.child(
            div()
                .id(SharedString::from(format!("footer-slot-{i}")))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .px(px(6.))
                .py(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_dashed()
                .border_color(if open { t.accent } else { t.line })
                .when(open, |d| d.bg(t.accent_soft))
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent))
                .on_click(cx.listener(move |m, _, _, cx| {
                    cx.stop_propagation();
                    m.menu = if m.menu == Menu::FooterStat(i) { Menu::None } else { Menu::FooterStat(i) };
                    cx.notify();
                }))
                .child(div().flex().items_center().gap(px(4.)).text_size(px(15.)).font_weight(FontWeight::BOLD).child(v).child(Icon::Chevron.el(9., t.dim)))
                .child(div().text_size(px(11.)).text_color(t.dim).truncate().child(label)),
        );
    }
    let picker = match m.menu {
        Menu::FooterStat(i) if i < stats.len() => Some(stat_picker(m, t, stats, i, cx)),
        _ => None,
    };
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(9.))
        .border_1()
        .border_dashed()
        .border_color(t.accent)
        .bg(t.raised)
        .child(range_row)
        .child(slots)
        .children(picker)
}

fn stat_picker(m: &MainWindow, t: &Theme, stats: &[String], slot: usize, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mut list = menu_box(t).occlude().w(px(290.)).text_size(px(12.5));
    for (group, keys) in STAT_GROUPS {
        list = list.child(div().px(px(10.)).pt(px(6.)).pb(px(2.)).text_size(px(10.5)).font_weight(FontWeight::BOLD).text_color(t.dim).child(group.to_uppercase()));
        for k in keys.iter() {
            let current = stats.get(slot).is_some_and(|s| s == k);
            let shown = stats.iter().any(|s| s == k);
            let (v, _, _) = stat(&m.today, k);
            let mut next: Vec<String> = stats.to_vec();
            if let Some(j) = next.iter().position(|s| s == k) {
                next[j] = next[slot].clone();
            }
            next[slot] = k.to_string();
            list = list.child(
                menu_item(t, &format!("pick-{k}"), stat_name(k), if current { "current" } else if shown { "shown" } else { "" }, cx.listener(move |m, _, _, cx| {
                    m.menu = Menu::None;
                    save(m, "sidebar.footer.stats", json!(next.clone()), cx);
                }))
                .when(current, |d| d.bg(t.accent_soft))
                .child(div().min_w(px(48.)).text_right().font_weight(FontWeight::BOLD).child(v)),
            );
        }
    }
    deferred(anchored().anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(list.mb(px(6.)))).with_priority(1)
}

fn edit_buttons(m: &MainWindow, t: &Theme, buttons: &[String], cx: &mut Context<MainWindow>) -> Div {
    let mut row = div().relative().flex().gap(px(4.));
    for b in buttons {
        let (icon, label, _) = button_info(b);
        let next: Vec<String> = buttons.iter().filter(|x| *x != b).cloned().collect();
        row = row.child(
            div()
                .relative()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(3.))
                .pt(px(7.))
                .pb(px(5.))
                .rounded(px(7.))
                .border_1()
                .border_dashed()
                .border_color(t.line)
                .text_size(px(10.5))
                .text_color(t.dim)
                .child(icon.el(16., t.dim))
                .child(label)
                .child(
                    div()
                        .id(SharedString::from(format!("footer-rm-{b}")))
                        .absolute()
                        .top(px(-7.))
                        .right(px(-6.))
                        .size(px(18.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(t.line)
                        .bg(t.raised)
                        .text_size(px(10.))
                        .text_color(t.fg)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.accent_soft))
                        .tooltip(super::header::tip(format!("Remove {label}")))
                        .on_click(cx.listener(move |m, _, _, cx| save(m, "sidebar.footer.buttons", json!(next.clone()), cx)))
                        .child("✕"),
                ),
        );
    }
    let addable: Vec<&str> = FOOTER_BUTTONS.iter().copied().filter(|b| !buttons.iter().any(|x| x == b)).collect();
    if buttons.len() < MAX_BUTTONS && !addable.is_empty() {
        let open = m.menu == Menu::FooterAdd;
        row = row.child(
            div()
                .id("footer-add")
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(3.))
                .pt(px(7.))
                .pb(px(5.))
                .rounded(px(7.))
                .border_1()
                .border_dashed()
                .border_color(t.accent)
                .when(open, |d| d.bg(t.accent_soft))
                .text_size(px(10.5))
                .text_color(t.accent)
                .cursor_pointer()
                .on_click(cx.listener(|m, _, _, cx| {
                    cx.stop_propagation();
                    m.menu = if m.menu == Menu::FooterAdd { Menu::None } else { Menu::FooterAdd };
                    cx.notify();
                }))
                .child(Icon::Plus.el(16., t.accent))
                .child("Add"),
        );
        if open {
            let mut list = menu_box(t).occlude().text_size(px(12.5));
            for b in addable {
                let (icon, label, setting) = button_info(b);
                let mut next = buttons.to_vec();
                next.push(b.to_string());
                let keys = m.key_label(setting);
                list = list.child(
                    menu_item(t, &format!("footer-add-{b}"), label, "", cx.listener(move |m, _, _, cx| {
                        m.menu = Menu::None;
                        save(m, "sidebar.footer.buttons", json!(next.clone()), cx);
                    }))
                    .child(icon.el(14., t.dim))
                    .children((!keys.is_empty()).then(|| super::header::key_chip(t, keys.into()))),
                );
            }
            row = row.child(deferred(anchored().anchor(Anchor::BottomRight).snap_to_window_with_margin(px(8.)).child(list.mb(px(6.)))).with_priority(1));
        }
    }
    row
}

fn context_menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let wk = week(m);
    let comp = compact(m);
    let has_stats = !stats(m).is_empty();
    let has_buttons = !buttons(m).is_empty();
    let check = |on: bool| div().size(px(14.)).flex_none().when(on, |d| d.child(Icon::Check.el(13., t.accent)));
    let list = menu_box(t)
        .occlude()
        .text_size(px(12.5))
        .child(menu_item(t, "footer-customize", "Customize footer…", "", cx.listener(|m, _, _, cx| {
            m.menu = Menu::None;
            m.footer_editing = true;
            m.request_refresh(crate::app::refresh::INSIGHTS, cx);
            cx.notify();
        })))
        .child(
            menu_item(t, "footer-range", "Count the last 7 days", "", cx.listener(move |m, _, _, cx| {
                m.menu = Menu::None;
                save(m, "sidebar.footer.range", json!(if wk { "today" } else { "week" }), cx);
            }))
            .child(check(wk)),
        )
        .child(
            menu_item(t, "footer-compact", "Compact footer", "", cx.listener(move |m, _, _, cx| {
                m.menu = Menu::None;
                save(m, "sidebar.footer.compact", json!(!comp), cx);
            }))
            .child(check(comp)),
        )
        .child(div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line))
        .child(menu_item(t, "footer-stats", if has_stats { "Hide stats" } else { "Show stats" }, "", cx.listener(move |m, _, _, cx| {
            m.menu = Menu::None;
            save(m, "sidebar.footer.stats", if has_stats { json!([]) } else { json!(DEFAULT_FOOTER_STATS) }, cx);
        })))
        .child(menu_item(t, "footer-buttons", if has_buttons { "Hide buttons" } else { "Show buttons" }, "", cx.listener(move |m, _, _, cx| {
            m.menu = Menu::None;
            save(m, "sidebar.footer.buttons", if has_buttons { json!([]) } else { json!(DEFAULT_FOOTER_BUTTONS) }, cx);
        })))
        .child(menu_item(t, "footer-reset-menu", "Reset to default", "", cx.listener(|m, _, _, cx| {
            m.menu = Menu::None;
            reset(m, cx);
        })));
    deferred(anchored().position(m.status_menu_at).anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)
}
