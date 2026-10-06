//! Bottom status bar: the items in `ui.status.items`, in order (midnad status, policy, webhooks
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
/// `$MIDNA_HOME/app-state.json` `{"seen": [...], "collapsed": [...], "order": [...], "sidebar_collapsed": bool, "background_open": bool, "background_hidden": bool}`.
/// `seen` = status bar items clicked at least once; `collapsed` = sidebar project groups folded
/// away; `order` = terminal ids in the order they were dragged to in the sidebar;
/// `sidebar_collapsed` = the sidebar is down to its rail; `background_open` = the sidebar's
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
    use super::{merge_order, scripts_wanted, toggled};
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
        "policy" => "Policy and approval rules",
        "webhooks" => "Webhooks",
        "triggers" => "Triggers today",
        "hooks" => "Agent hooks (when they need you)",
        "accessibility" => "Accessibility (when Kass needs it)",
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
            "policy" if connected => Some(policy_item(m, t)),
            "webhooks" if connected => Some(webhooks_item(m, t, cx)),
            "triggers" if connected => Some(triggers_item(m, t, cx)),
            "hooks" if connected => crate::ui::hooks::status_item(m, t, cx),
            "accessibility" if connected => crate::ui::ax_prompt::status_item(m, t, cx),
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

/// Right-click menu: a check row per built-in item and per script path in the list (saved to
/// `ui.status.items`; unchecking a script path drops it, adding one is done by asking an agent
/// or `midna settings set`). Opens upward from where the bar was clicked, and stays open so several can be toggled.
fn menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let current = items(m);
    let mut list = crate::ui::sidebar::menu_box(t).occlude().mb(px(4.)).text_size(px(12.5));
    let all = midna_proto::settings::STATUS_ITEMS.iter().map(|s| s.to_string()).chain(current.iter().filter(|i| i.starts_with('/')).cloned());
    for item in all {
        let on = current.iter().any(|i| *i == item);
        let next = toggled(&current, &item, "ui.status.items");
        list = list.child(
            crate::ui::sidebar::menu_item(t, &format!("status-item-{item}"), item_label(&item), if item.starts_with('/') { "unchecking removes it" } else { "" }, cx.listener(move |m, _, _, cx| {
                m.settings.insert("ui.status.items".into(), serde_json::json!(next));
                m.rpc("settings.set", serde_json::json!({"key": "ui.status.items", "value": next}), cx, |_, _, _, _| {});
                cx.notify();
            }))
            .child(div().size(px(14.)).flex_none().when(on, |d| d.child(Icon::Check.el(13., t.accent)))),
        );
    }
    deferred(anchored().position(m.status_menu_at).anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(list)).with_priority(1)
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
