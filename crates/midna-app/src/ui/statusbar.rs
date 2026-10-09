//! Bottom status bar: the items in `ui.status.items`, in order (midnad status, Claude usage, webhooks
//! path, triggers today, agent hooks, the `ui.status.script` segments, key hints, …).
//! Right-click shows or hides each one.
use super::header::tip;
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::backend::{Backend, ConnState};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::sync::Arc;

pub fn webhooks_label(path: &str) -> &'static str {
    match path {
        "tailscale_funnel" => "Webhooks via Tailscale Funnel",
        "self_relay" => "Webhooks via self-hosted relay",
        "midna_relay" => "Webhooks via midna relay",
        "off" | "" => "Webhooks off",
        _ => "Webhooks",
    }
}

/// App-side UI memory, kept out of the daemon's settings so it doesn't clutter Settings:
/// `$MIDNA_HOME/app-state.json` `{"seen": [...], "collapsed": [...], "order": [...], "sidebar_collapsed": bool, "sidebar_width": f32, "background_open": bool, "background_hidden": bool}`.
/// `seen` = status bar items clicked at least once; `collapsed` = sidebar project groups folded
/// away; `order` = terminal ids in the order they were dragged to in the sidebar;
/// `sidebar_collapsed` = the sidebar is down to its rail; `sidebar_width` = the full sidebar's
/// dragged width; `background_open` = the sidebar's
/// Background group is unfolded; `background_hidden` = it is left out of the sidebar.
fn state_file(backend: &Arc<dyn Backend>) -> Option<std::path::PathBuf> {
    backend.socket_path().parent().map(|h| h.join("app-state.json"))
}

pub fn load_state<T: serde::de::DeserializeOwned + Default>(backend: &Arc<dyn Backend>, key: &str) -> T {
    state_file(backend)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.get(key).cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Every main window writes the same file: keep what the others saved (their "seen" clicks,
/// their terminals' places in the order) and put this window's terminals in its order.
pub fn save_state(m: &mut MainWindow) {
    let Some(p) = state_file(&m.backend) else { return };
    let saved = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or_default();
    let seen_before: Vec<String> = saved.get("seen").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    m.seen.extend(seen_before);
    let base: Vec<String> = saved.get("order").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let mine: Vec<String> = m.order.iter().filter(|id| m.shows(id)).cloned().collect();
    m.order = merge_order(&base, &mine);
    let mut all = saved.as_object().cloned().unwrap_or_default();
    for (k, v) in [("seen", serde_json::json!(m.seen)), ("collapsed", serde_json::json!(m.collapsed)), ("order", serde_json::json!(m.order)), ("sidebar_collapsed", serde_json::json!(m.sidebar_collapsed)), ("background_open", serde_json::json!(m.background_open)), ("background_hidden", serde_json::json!(m.background_hidden))] {
        all.insert(k.into(), v);
    }
    let _ = std::fs::write(p, serde_json::Value::Object(all).to_string());
}

/// Set one key of `app-state.json`, keeping the rest.
pub fn update_state(backend: &Arc<dyn Backend>, key: &str, value: serde_json::Value) {
    let Some(p) = state_file(backend) else { return };
    let saved = std::fs::read(&p).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()).unwrap_or_default();
    let mut all = saved.as_object().cloned().unwrap_or_default();
    all.insert(key.into(), value);
    let _ = std::fs::write(p, serde_json::Value::Object(all).to_string());
}

/// `base` with the ids in `mine` re-ordered among their own slots (new ones at the end).
fn merge_order(base: &[String], mine: &[String]) -> Vec<String> {
    let mut next = mine.iter().filter(|id| base.contains(id));
    let mut out: Vec<String> = base.iter().map(|id| if mine.contains(id) { next.next().cloned().unwrap_or_else(|| id.clone()) } else { id.clone() }).collect();
    out.extend(mine.iter().filter(|id| !base.contains(id)).cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::{cache_left, merge_order, moved, pace, scripts_wanted, toggled};
    use ::core::prelude::v1::test;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn merge_keeps_other_windows_places() {
        // this window has b and d; another window's a, c, e stay put
        assert_eq!(merge_order(&v(&["a", "b", "c", "d", "e"]), &v(&["d", "b"])), v(&["a", "d", "c", "b", "e"]));
        assert_eq!(merge_order(&v(&["a"]), &v(&["x"])), v(&["a", "x"]));
    }

    #[test]
    fn reshown_items_return_to_their_default_place() {
        assert_eq!(toggled(&v(&["daemon", "policy", "keys"]), "policy", "ui.status.items"), v(&["daemon", "keys"]));
        assert_eq!(toggled(&v(&["daemon", "spacer", "keys"]), "script", "ui.status.items"), v(&["daemon", "spacer", "script", "keys"]));
        assert_eq!(toggled(&v(&["keys", "daemon"]), "webhooks", "ui.status.items"), v(&["keys", "daemon", "webhooks"]));
        assert_eq!(toggled(&v(&[]), "daemon", "ui.status.items"), v(&["daemon"]));
    }

    #[test]
    fn dragging_moves_an_item_to_the_targets_place() {
        assert_eq!(moved(&v(&["a", "b", "c", "d"]), "a", "c"), v(&["b", "c", "a", "d"]));
        assert_eq!(moved(&v(&["a", "b", "c", "d"]), "d", "b"), v(&["a", "d", "b", "c"]));
        assert_eq!(moved(&v(&["a", "b"]), "x", "b"), v(&["a", "b"]));
    }

    #[test]
    fn cache_goes_cold_a_ttl_after_the_turn() {
        assert_eq!(cache_left(true, 0, 300, 10_000), None);
        assert_eq!(cache_left(false, 1000, 300, 1100), Some(200));
        assert!(cache_left(false, 1000, 300, 1400).unwrap() <= 0);
    }

    #[test]
    fn pace_projects_the_window_linearly() {
        let h = 3600;
        // (pct, observed, resets_at, len) with the window starting at 0; 2h in at 20%: 50% by the reset
        assert_eq!(pace(20., 2 * h, 5 * h, 5 * h), Some(super::Pace::Within { at_reset: 50. }));
        // 1h in at 40%: 100% at 2.5h, 2.5h before the reset
        assert_eq!(pace(40., h, 5 * h, 5 * h), Some(super::Pace::Out { in_secs: 5 * h / 2 - h }));
        // too early to tell, or nothing used
        assert_eq!(pace(10., 60, 5 * h, 5 * h), None);
        assert_eq!(pace(0., h, 5 * h, 5 * h), None);
    }

    #[test]
    fn scripts_run_only_when_shown_and_set() {
        use serde_json::json;
        assert_eq!(scripts_wanted(None, None), v(&["script"]));
        assert_eq!(scripts_wanted(Some(&json!(["daemon"])), None), v(&[]));
        assert_eq!(scripts_wanted(Some(&json!(["script", "/x/ci.sh"])), Some(&json!("none"))), v(&["/x/ci.sh"]));
        assert_eq!(scripts_wanted(Some(&json!(["/x/ci.sh", "script"])), Some(&json!("branch"))), v(&["/x/ci.sh", "script"]));
    }
}

/// Remember a one-time UI answer (e.g. "Not now" on the Accessibility card) in `seen`.
pub fn remember_seen(m: &mut MainWindow, key: &str) {
    mark_seen(m, key);
}

fn mark_seen(m: &mut MainWindow, key: &str) {
    if m.seen.insert(key.to_string()) {
        save_state(m);
    }
}

/// Clickable status item: opens `screen` and remembers the click under `key`. "webhooks"
/// also opens the Triggers screen's delivery-path picker, so the two items land on
/// different things.
fn link(id: &'static str, key: &'static str, screen: Screen, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let fg = t.fg;
    div().id(id).flex().gap(px(4.)).cursor_pointer().hover(move |s| s.text_color(fg)).on_click(cx.listener(move |m, _, w, cx| {
        mark_seen(m, key);
        if m.screen != screen {
            m.set_screen(screen, w, cx);
        }
        if key == "webhooks" {
            crate::ui::triggers::open_delivery_path(m, cx);
        }
    }))
}


/// `ui.status.items`, falling back to its default while settings haven't loaded.
pub fn items(m: &MainWindow) -> Vec<String> {
    let v = m.settings.get("ui.status.items").cloned().or_else(|| midna_proto::settings::setting("ui.status.items").map(|s| s.default.to_json()));
    v.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// The script items to run for the selected terminal: `script` while it's shown and
/// `ui.status.script` has something to run, then each script path in the list. Missing values
/// mean the defaults (`script` shown, `worktree+branch`).
pub fn scripts_wanted(items: Option<&serde_json::Value>, script: Option<&serde_json::Value>) -> Vec<String> {
    let script_on = script.and_then(|v| v.as_str()).is_none_or(|s| !s.is_empty() && s != "none");
    let Some(items) = items.and_then(|v| v.as_array()) else {
        return if script_on { vec!["script".into()] } else { vec![] };
    };
    items.iter().filter_map(|i| i.as_str()).filter(|i| i.starts_with('/') || (*i == "script" && script_on)).map(str::to_string).collect()
}

/// What each item is called in the right-click menu (a script path: its file name).
fn item_label(item: &str) -> &str {
    match item {
        "daemon" => "midnad status",
        "usage" => "Claude usage (5-hour and weekly)",
        "cache" => "Prompt cache (selected Claude terminal)",
        "policy" => "Policy and approval rules",
        "webhooks" => "Webhooks",
        "triggers" => "Triggers today",
        "hooks" => "Agent hooks (when they need you)",
        "accessibility" => "Accessibility (when Kass needs it)",
        "awake" => "Keeping the Mac awake (while it is)",
        "script" => "Git: worktree and branch",
        "spacer" => "Spacer (push the rest right)",
        "update" => "Update ready",
        "keys" => "Shortcut hints",
        p => p.rsplit('/').next().unwrap_or(p),
    }
}

/// `items` (the list setting `key`) with `item` toggled. A re-shown item goes back to its place
/// in the default order.
pub fn toggled(items: &[String], item: &str, key: &str) -> Vec<String> {
    if items.iter().any(|i| i == item) {
        return items.iter().filter(|i| *i != item).cloned().collect();
    }
    let default: Vec<String> = midna_proto::settings::setting(key).and_then(|s| serde_json::from_value(s.default.to_json()).ok()).unwrap_or_default();
    let rank = |i: &str| default.iter().position(|d| d == i).unwrap_or(usize::MAX);
    // after the last item that comes before it by default, so a reordered bar keeps its order
    let at = items.iter().rposition(|i| rank(i) < rank(item)).map_or(0, |p| p + 1);
    let mut out = items.to_vec();
    out.insert(at, item.to_string());
    out
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let connected = m.conn == ConnState::Connected;
    let mut bar = div()
        .id("statusbar")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(18.))
        .min_h(px(28.))
        .px(px(14.))
        .bg(t.panel)
        .border_t_1()
        .border_color(t.line)
        .text_size(px(11.5))
        .text_color(t.dim)
        .whitespace_nowrap()
        .overflow_hidden()
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|m, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                m.status_menu_at = ev.position;
                m.menu = if m.menu == Menu::StatusBar { Menu::None } else { Menu::StatusBar };
                cx.notify();
            }),
        );
    // The bell is always first, like VS Code's: notifications and how many are unread.
    if connected {
        bar = bar.child(crate::ui::notifications::bell(m, t, cx));
    }
    for item in items(m) {
        // The daemon-backed items need midnad; the rest always show.
        let el = match item.as_str() {
            "daemon" => Some(daemon_item(m, t)),
            "usage" if connected => usage_item(m, t),
            "cache" if connected => cache_item(m, t),
            "policy" if connected => Some(policy_item(m, t)),
            "webhooks" if connected => Some(webhooks_item(m, t, cx)),
            "triggers" if connected => Some(triggers_item(m, t, cx)),
            "hooks" if connected => crate::ui::hooks::status_item(m, t, cx),
            "accessibility" if connected => crate::ui::ax_prompt::status_item(m, t, cx),
            "awake" if connected => awake_item(m, t, cx),
            "script" => script_item(m, &item, t, cx),
            p if p.starts_with('/') => script_item(m, p, t, cx),
            "spacer" => Some(div().flex_1().into_any_element()),
            "update" => update_item(t, cx),
            "keys" => Some(div().child(format!("{} commands · {} next", m.key_label("keys.command_bar"), m.key_label("keys.next_needs_you"))).into_any_element()),
            _ => None,
        };
        bar = bar.children(el);
    }
    bar.when(m.menu == Menu::StatusBar, |d| d.child(menu(m, t, cx)))
}

fn dot(c: Hsla) -> Div {
    div().text_color(c).child("●")
}

fn daemon_item(m: &MainWindow, t: &Theme) -> AnyElement {
    let daemon = match &m.conn {
        ConnState::Connected => div().flex().gap(px(4.)).child(dot(t.ok)).child(m.backend.label()),
        ConnState::Connecting => div().flex().gap(px(4.)).child(dot(t.dim)).child("connecting to midnad…"),
        ConnState::NotRunning { .. } if m.daemon_restarting() => div().flex().gap(px(4.)).child(dot(t.need)).child("midnad restarting…"),
        ConnState::NotRunning { .. } => div().flex().gap(px(4.)).child(dot(t.err)).child("midnad not running"),
    };
    div().id("daemon").child(daemon).tooltip(tip("midnad: the background daemon that runs your terminals. They keep running when this window closes.")).into_any_element()
}

/// "Keeping awake" while midnad holds its keep-awake assertion (nothing otherwise). Click: Settings.
fn awake_item(m: &MainWindow, t: &Theme, _cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let k = &m.keep_awake;
    if k["held"] != true {
        return None;
    }
    let line = k["line"].as_str().unwrap_or("Keeping the Mac awake").to_string();
    let backend = m.backend.clone();
    let fg = t.fg;
    Some(
        div()
            .id("keep-awake")
            .flex()
            .items_center()
            .gap(px(4.))
            .cursor_pointer()
            .hover(move |s| s.text_color(fg))
            .child(Icon::Sun.el(11., t.work))
            .child("Keeping awake")
            .tooltip(tip(format!("{line}. The display may still sleep; closing the lid still sleeps the Mac. Settings › Agents › Keep the Mac awake.")))
            .on_click(move |_, _, cx| crate::settings_window::open(backend.clone(), cx))
            .into_any_element(),
    )
}

/// Claude's plan limits as the taskboard draws them: per window a label, a 34×5 bar and the
/// percentage, amber from 75% and red from 90%. A window Claude doesn't report (the 5-hour one
/// on some plans) is left out; nothing shows until a Claude terminal has reported any.
fn usage_item(m: &MainWindow, t: &Theme) -> Option<AnyElement> {
    let u = m.usage.claude.as_ref()?;
    let now = midna_proto::time::now_unix();
    let stale = midna_proto::time::parse_rfc3339(&u.observed_at).is_some_and(|s| now - s > 3600);
    let mut row = div().id("usage").flex().items_center().gap(px(10.)).when(stale, |d| d.opacity(0.6));
    let mut tips = Vec::new();
    for (label, name, len, w) in [("5h", "5-hour", 5 * 3600, &u.five_hour), ("7d", "Weekly", 7 * 86400, &u.seven_day)] {
        let Some(w) = w else { continue };
        let pct = if w.expired { 0. } else { w.used_percentage.clamp(0., 100.) };
        let (bar_c, val_c) = if pct >= 90. {
            (t.err, t.err)
        } else if pct >= 75. {
            (t.need, t.need)
        } else {
            (t.ok, t.dim)
        };
        let value = if w.expired { "—".to_string() } else { format!("{}%", pct.round()) };
        let resets_at = w.resets_at.as_deref().and_then(midna_proto::time::parse_rfc3339);
        let resets = w.resets_at.as_deref().map(|r| format!(", resets {}", crate::ui::screen_kit::day_label(r))).unwrap_or_default();
        let observed = midna_proto::time::parse_rfc3339(&u.observed_at).unwrap_or(now);
        let projection = match resets_at.and_then(|r| pace(pct, observed, r, len)) {
            Some(Pace::Out { in_secs }) => {
                let at = midna_proto::time::format_unix(observed + in_secs);
                format!("\nAt this pace you'll run out {} ({})", crate::ui::screen_kit::day_label(&at), crate::ui::screen_kit::left(observed + in_secs - now).replace(" left", " from now"))
            }
            Some(Pace::Within { at_reset }) => format!("\nWithin range: about {}% by the reset at this pace", at_reset.round()),
            None => String::new(),
        };
        tips.push(if w.expired { format!("{name} limit: reset since the last report") } else { format!("{name} limit: {}% used{resets}{projection}", pct.round()) });
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .child(label)
                .child(div().w(px(34.)).h(px(5.)).rounded_full().overflow_hidden().bg(t.line).child(div().h_full().rounded_full().bg(bar_c).w(relative((pct / 100.) as f32))))
                .child(div().min_w(px(24.)).flex().justify_end().text_color(val_c).child(value)),
        );
    }
    if tips.is_empty() {
        return None;
    }
    if stale {
        tips.push(format!("Last reported {}", crate::ui::screen_kit::ago(Some(&u.observed_at))));
    }
    Some(row.tooltip(tip(tips.join("\n"))).into_any_element())
}

/// Whether the selected Claude terminal's prompt cache is still warm: a flame while it is, a
/// snowflake once it has gone cold. Claude keeps a conversation's cache for an hour on
/// subscription plans (they report plan limits) and five minutes on an API key, from its last
/// request; a turn in progress keeps it warm, and its last request is about when the terminal
/// last changed state (stopped, or asked you something). Nothing shows for other terminals,
/// or when no terminal is selected.
fn cache_item(m: &MainWindow, t: &Theme) -> Option<AnyElement> {
    if m.screen != Screen::Terminal {
        return None;
    }
    let s = m.selected_session()?;
    use crate::model::{AgentKind, StatusState};
    if s.agent != Some(AgentKind::Claude) || matches!(s.status.state, StatusState::Failed | StatusState::Exited) {
        return None;
    }
    let info = s.agent_info.as_ref()?;
    let ttl = if info.rate_limits.is_some() { 3600 } else { 300 };
    let since = midna_proto::time::parse_rfc3339(s.status.since.as_deref()?)?;
    let left = cache_left(s.status.state == StatusState::Working, since, ttl, midna_proto::time::now_unix());
    let ttl_text = if ttl == 3600 { "1 hour" } else { "5 minutes" };
    let (icon, color, text) = match left {
        None => (Icon::Flame, t.work, "Prompt cache warm: a turn is running".to_string()),
        Some(l) if l > 0 => (Icon::Flame, t.work, format!("Prompt cache warm: {} (it lasts {ttl_text} after the last request)", crate::ui::screen_kit::left(l))),
        Some(_) => (Icon::Snowflake, t.dim, format!("Prompt cache cold: the next message re-reads the whole conversation at full price (it lasts {ttl_text} after the last request)")),
    };
    Some(div().id("cache").flex().items_center().child(icon.el(12., color)).tooltip(tip(text)).into_any_element())
}

/// Seconds the cache stays warm (≤ 0: cold), or None while a turn keeps it warm.
fn cache_left(working: bool, since: i64, ttl: i64, now: i64) -> Option<i64> {
    (!working).then(|| since + ttl - now)
}

#[derive(Debug, PartialEq)]
enum Pace {
    /// 100% is reached `in_secs` after the reading, before the window resets.
    Out { in_secs: i64 },
    /// The window resets first, at about `at_reset`%.
    Within { at_reset: f64 },
}

/// Where a window is headed if it keeps being used at its average rate so far: `pct` used at
/// `observed` (unix secs) of a `len`-second window that resets at `resets_at`. None in its first
/// 10 minutes (too little to go on), with nothing used, or once it's over.
fn pace(pct: f64, observed: i64, resets_at: i64, len: i64) -> Option<Pace> {
    let elapsed = observed - (resets_at - len);
    let left = resets_at - observed;
    if elapsed < 600 || left <= 0 || pct <= 0. {
        return None;
    }
    let rate = pct / elapsed as f64;
    if pct >= 100. {
        return Some(Pace::Out { in_secs: 0 });
    }
    let to_full = ((100. - pct) / rate) as i64;
    if to_full < left { Some(Pace::Out { in_secs: to_full }) } else { Some(Pace::Within { at_reset: (pct + rate * left as f64).min(100.) }) }
}

fn policy_item(m: &MainWindow, t: &Theme) -> AnyElement {
    let policy = m.setting_str("policy.default").unwrap_or_else(|| "default".into());
    let rules = m.rules_count;
    div()
        .flex()
        .gap(px(4.))
        .child("Policy")
        .child(div().text_color(t.fg).child(policy))
        .child("·")
        .child(format!("{rules} approval rule{}", if rules == 1 { "" } else { "s" }))
        .into_any_element()
}

fn webhooks_item(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let wh_path = m.webhooks.get("path").and_then(|v| v.as_str()).map(str::to_string).or_else(|| m.setting_str("webhooks.path")).unwrap_or_default();
    let wh_ok = m.webhooks.get("health").and_then(|v| v.as_str()).map(|h| h == "ok" || h == "healthy").unwrap_or(false);
    let wh_off = wh_path == "off" || wh_path.is_empty();
    let wh_color = if wh_off {
        t.dim
    } else if wh_ok {
        t.ok
    } else {
        t.need
    };
    if wh_off || !m.seen.contains("webhooks") {
        link("webhooks", "webhooks", Screen::Triggers, t, cx).text_color(t.accent).child("Set up webhooks").into_any_element()
    } else {
        link("webhooks", "webhooks", Screen::Triggers, t, cx).child(dot(wh_color)).child(webhooks_label(&wh_path)).into_any_element()
    }
}

fn triggers_item(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let triggers = m.today.triggers_fired;
    if m.triggers_count == 0 || !m.seen.contains("triggers") {
        link("triggers", "triggers", Screen::Triggers, t, cx).text_color(t.accent).child("Add a trigger").into_any_element()
    } else {
        link("triggers", "triggers", Screen::Triggers, t, cx).child(format!("{triggers} trigger{} today", if triggers == 1 { "" } else { "s" })).into_any_element()
    }
}

/// A script item's segments for the selected terminal: `script` = `ui.status.script` (default:
/// worktree and branch), or a script path from the list.
fn script_item(m: &MainWindow, item: &str, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let segs = m.status_segments.get(m.selected.as_ref()?)?.get(item)?;
    (!segs.is_empty()).then(|| div().id(SharedString::from(format!("status-{item}"))).child(crate::ui::sidebar::segments(segs, t, 11.5, cx).gap(px(10.))).into_any_element())
}

/// A status bar item being dragged in the right-click menu.
struct StatusDrag(String);

/// Right-click menu: a check row per item, the shown ones first in the bar's order, then the
/// hidden ones (a check row per built-in item and per script path in the list, saved to
/// `ui.status.items`; unchecking a script path drops it, adding one is done by asking an agent
/// or `midna settings set`). Drag a shown row to move it on the bar; the order is saved when it's
/// dropped. Opens upward from where the bar was clicked, and stays open so several can be toggled.
fn menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let current = items(m);
    let mut list = crate::ui::sidebar::menu_box(t).occlude().mb(px(4.)).text_size(px(12.5));
    let hidden = midna_proto::settings::STATUS_ITEMS.iter().map(|s| s.to_string()).filter(|i| !current.contains(i));
    let all: Vec<String> = current.iter().cloned().chain(hidden).collect();
    for item in all {
        let on = current.iter().any(|i| *i == item);
        let next = toggled(&current, &item, "ui.status.items");
        let row = crate::ui::sidebar::menu_item(t, &format!("status-item-{item}"), item_label(&item), if item.starts_with('/') { "unchecking removes it" } else { "" }, cx.listener(move |m, _, _, cx| {
            save_items(m, next.clone(), cx);
        }))
        .child(div().size(px(14.)).flex_none().when(on, |d| d.child(Icon::Check.el(13., t.accent))));
        let target = item.clone();
        list = list.child(row.when(on, |d| {
            d.on_drag(StatusDrag(item.clone()), |_, _, _, cx| cx.new(|_| crate::ui::sidebar::NoGhost))
                .on_drag_move(cx.listener(move |m, ev: &DragMoveEvent<StatusDrag>, _, cx| {
                    let y = ev.event.position.y;
                    let dragged = ev.drag(cx).0.clone();
                    if dragged != target && ev.bounds.top() <= y && y < ev.bounds.bottom() {
                        let next = moved(&items(m), &dragged, &target);
                        m.settings.insert("ui.status.items".into(), serde_json::json!(next));
                        cx.notify();
                    }
                }))
                .on_drop(cx.listener(|m, _: &StatusDrag, _, cx| save_items(m, items(m), cx)))
        }));
    }
    deferred(anchored().position(m.status_menu_at).anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)
}

fn save_items(m: &mut MainWindow, next: Vec<String>, cx: &mut Context<MainWindow>) {
    m.settings.insert("ui.status.items".into(), serde_json::json!(next));
    m.rpc("settings.set", serde_json::json!({"key": "ui.status.items", "value": next}), cx, |_, _, _, _| {});
    cx.notify();
}

/// `items` with `dragged` moved to `target`'s place.
fn moved(items: &[String], dragged: &str, target: &str) -> Vec<String> {
    let mut out = items.to_vec();
    let (Some(from), Some(to)) = (out.iter().position(|i| i == dragged), out.iter().position(|i| i == target)) else { return out };
    let d = out.remove(from);
    out.insert(to, d);
    out
}

/// "Update ready · restart to apply" (quiet; click installs and relaunches). Shells live in
/// midnad, so restarting the app loses nothing.
fn update_item(t: &Theme, _cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let l = crate::lifecycle::snapshot()?;
    let text = crate::lifecycle::status_text(&l.update)?;
    let ready = matches!(l.update, crate::lifecycle::UpdateState::Ready { .. });
    Some(
        div()
            .id("update-ready")
            .flex()
            .gap(px(4.))
            .text_color(t.accent)
            .when(ready, |d| d.cursor_pointer().hover(|s| s.text_color(t.fg)).on_click(|_, _, cx| crate::lifecycle::command(crate::lifecycle::Cmd::Apply, cx)))
            .child("↻")
            .child(text)
            .into_any_element(),
    )
}
