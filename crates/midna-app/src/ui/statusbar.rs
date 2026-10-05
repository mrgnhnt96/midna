//! Bottom status bar: midnad status, policy, webhooks path, triggers today, key hints.
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
/// `$MIDNA_HOME/app-state.json` `{"seen": [...], "collapsed": [...], "order": [...]}`. `seen` =
/// status bar items clicked at least once; `collapsed` = sidebar project groups folded away;
/// `order` = terminal ids in the order they were dragged to in the sidebar.
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

pub fn save_state(m: &MainWindow) {
    if let Some(p) = state_file(&m.backend) {
        let _ = std::fs::write(p, serde_json::json!({ "seen": m.seen, "collapsed": m.collapsed, "order": m.order }).to_string());
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

fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| super::header::Tip(text.into())).into()
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
