//! Bottom status bar: midnad status, policy, webhooks path, triggers today, key hints.
use super::header::tip;
use crate::app::{MainWindow, Screen};
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
/// `$MIDNA_HOME/app-state.json` `{"seen": [...], "collapsed": [...], "order": [...], "sidebar_collapsed": bool}`.
/// `seen` = status bar items clicked at least once; `collapsed` = sidebar project groups folded
/// away; `order` = terminal ids in the order they were dragged to in the sidebar;
/// `sidebar_collapsed` = the sidebar is down to its rail.
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
    for (k, v) in [("seen", serde_json::json!(m.seen)), ("collapsed", serde_json::json!(m.collapsed)), ("order", serde_json::json!(m.order)), ("sidebar_collapsed", serde_json::json!(m.sidebar_collapsed))] {
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
    use super::merge_order;
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


pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let dot = |c: Hsla| div().text_color(c).child("●");
    let daemon = match &m.conn {
        ConnState::Connected => div().flex().gap(px(4.)).child(dot(t.ok)).child(m.backend.label()),
        ConnState::Connecting => div().flex().gap(px(4.)).child(dot(t.dim)).child("connecting to midnad…"),
        ConnState::NotRunning { .. } if m.daemon_restarting() => div().flex().gap(px(4.)).child(dot(t.need)).child("midnad restarting…"),
        ConnState::NotRunning { .. } => div().flex().gap(px(4.)).child(dot(t.err)).child("midnad not running"),
    };
    let connected = m.conn == ConnState::Connected;
    let policy = m.setting_str("policy.default").unwrap_or_else(|| "default".into());
    let rules = m.rules_count;
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
    let triggers = m.today.triggers_fired;

    div()
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
        .child(div().id("daemon").child(daemon).tooltip(tip("midnad: the background daemon that runs your terminals. They keep running when this window closes.")))
        .when(connected, |d| {
            let wh = if wh_off || !m.seen.contains("webhooks") {
                link("webhooks", "webhooks", Screen::Triggers, t, cx).text_color(t.accent).child("Set up webhooks")
            } else {
                link("webhooks", "webhooks", Screen::Triggers, t, cx).child(dot(wh_color)).child(webhooks_label(&wh_path))
            };
            let tr = if m.triggers_count == 0 || !m.seen.contains("triggers") {
                link("triggers", "triggers", Screen::Triggers, t, cx).text_color(t.accent).child("Add a trigger")
            } else {
                link("triggers", "triggers", Screen::Triggers, t, cx).child(format!("{triggers} trigger{} today", if triggers == 1 { "" } else { "s" }))
            };
            d.child(
                div()
                    .flex()
                    .gap(px(4.))
                    .child("Policy")
                    .child(div().text_color(t.fg).child(policy))
                    .child("·")
                    .child(format!("{rules} approval rule{}", if rules == 1 { "" } else { "s" })),
            )
            .child(wh)
            .child(tr)
        })
        .child(div().flex_1())
        .when_some(update_item(t, cx), |d, item| d.child(item))
        .child(format!("{} commands · {} next", m.key_label("keys.command_bar"), m.key_label("keys.next_needs_you")))
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
