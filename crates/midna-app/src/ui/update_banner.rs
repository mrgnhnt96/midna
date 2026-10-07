//! Update prompt at the bottom of an agent terminal when a newer agent is installed than the
//! one it runs (`AgentInfo::update_prompt`). It shows only while that terminal is open and
//! never raises a needs-you item: Restart / When idle call `session.restart` (same
//! conversation), Not now calls `session.update_decline`, which hides it until a newer update.
//! "Don't ask again" also sets `agents.restart_on_update` for every later update: `when_idle`
//! with Restart / When idle, `off` with Not now. Clicking Claude's own "Restart to update"
//! notice in the terminal (`terminal::on_update_notice`) shows the prompt again, even after
//! Not now or before the daemon's next update check has noticed the update.
use super::border_w;
use crate::app::{MainWindow, refresh};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

/// The "Don't ask again" box: one prompt is shown at a time, so one flag for the window.
static DONT_ASK: AtomicBool = AtomicBool::new(false);

/// Terminals whose update notice was clicked: their prompt shows until it's answered.
#[derive(Default)]
pub struct Asked(HashSet<String>);

impl Global for Asked {}

/// The update notice in terminal `sid` was clicked: show its prompt.
pub fn ask(sid: &str, cx: &mut App) {
    if !cx.has_global::<Asked>() {
        cx.set_global(Asked::default());
    }
    cx.update_global::<Asked, _>(|a, _| a.0.insert(sid.to_string()));
    cx.refresh_windows();
}

fn asked(sid: &str, cx: &App) -> bool {
    cx.try_global::<Asked>().is_some_and(|a| a.0.contains(sid))
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<impl IntoElement + use<>> {
    if m.approval_for_selected().is_some() {
        return None; // the approval banner owns this spot
    }
    let s = m.selected_session()?;
    // Only an agent that reported its conversation can come back into it.
    let info = s.agent_info.as_ref().filter(|i| s.agent.is_some() && i.conversation_id.is_some())?;
    let update = match info.update_prompt() {
        Some(u) => u,
        None if asked(&s.id, cx) && info.restart.is_none() => info.update_available.as_deref().unwrap_or("update"),
        None => return None,
    };
    let title = if update == "update" { "A Claude update is installed".to_string() } else { format!("Claude {update} is installed") };
    let in_flight = info.in_flight();
    // Restart now would stop background work, so then only "When idle" restarts.
    let sub = if in_flight.is_empty() {
        "Restart to update. The same conversation comes back.".to_string()
    } else {
        let count = |n: usize, one: &str| match n {
            0 => None,
            1 => Some(format!("1 {one}")),
            n => Some(format!("{n} {one}s")),
        };
        let what = [count(info.background.len(), "background task"), count(info.subagents.len(), "subagent")].into_iter().flatten().collect::<Vec<_>>().join(" and ");
        format!("Restarts into the same conversation once {what} finish.")
    };
    let sid = s.id.clone();

    let button = |id: &'static str, label: &'static str, primary: bool| {
        let b = div().id(id).h(px(32.)).px(px(12.)).flex().items_center().rounded(px(7.)).cursor_pointer();
        if primary {
            b.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD).hover(|s| s.opacity(0.92))
        } else {
            b.border_1().border_color(t.line).hover(|s| s.bg(t.raised))
        }
        .child(label)
    };
    let restart = in_flight.is_empty().then(|| {
        let sid = sid.clone();
        button("update-restart", "Restart", true).on_click(cx.listener(move |m, _, _, cx| send_restart(m, &sid, json!({"id": sid, "resume": true}), cx)))
    });
    let idle = {
        let sid = sid.clone();
        button("update-idle", "When idle", !in_flight.is_empty()).on_click(cx.listener(move |m, _, _, cx| send_restart(m, &sid, json!({"id": sid, "when": "idle"}), cx)))
    };
    let later = button("update-later", "Not now", false).on_click(cx.listener(move |m, _, _, cx| decline(m, &sid, cx)));
    let checked = DONT_ASK.load(Ordering::Relaxed);
    let dont_ask = div()
        .id("update-dont-ask")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .text_size(px(12.))
        .text_color(t.dim)
        .cursor_pointer()
        .on_click(cx.listener(|_, _, _, cx| {
            DONT_ASK.fetch_xor(true, Ordering::Relaxed);
            cx.notify();
        }))
        .child(
            div()
                .size(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1()
                .border_color(if checked { t.accent } else { t.line })
                .when(checked, |d| d.bg(t.accent).child(Icon::Check.el(10., t.accent_fg))),
        )
        .child("Don't ask again");

    Some(
        border_w(div(), 1.)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .mx(px(16.))
            .mb(px(16.))
            .px(px(14.))
            .py(px(12.))
            .border_color(t.line)
            .rounded(px(10.))
            .bg(t.panel)
            .child(Icon::Restart.el(16., t.accent))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .flex_1()
                    .min_w(px(200.))
                    .child(div().font_weight(FontWeight::BOLD).child(title))
                    .child(div().text_size(px(12.)).text_color(t.dim).truncate().child(sub)),
            )
            .child(dont_ask)
            .children(restart)
            .child(idle)
            .child(later),
    )
}

/// Hide the prompt for this terminal now; the event-driven refresh confirms it.
fn hide(m: &mut MainWindow, sid: &str, f: impl FnOnce(&mut midna_proto::AgentInfo), cx: &mut Context<MainWindow>) {
    if cx.has_global::<Asked>() {
        cx.update_global::<Asked, _>(|a, _| a.0.remove(sid));
    }
    if let Some(i) = m.sessions.iter_mut().find(|s| s.id == sid).and_then(|s| s.agent_info.as_mut()) {
        f(i);
    }
}

/// With "Don't ask again" ticked, every later update does what this answer did.
fn remember(m: &mut MainWindow, mode: &'static str, cx: &mut Context<MainWindow>) {
    if DONT_ASK.swap(false, Ordering::Relaxed) {
        m.rpc("settings.set", json!({"key": "agents.restart_on_update", "value": mode}), cx, move |m, _, _, cx| m.toast(format!("Agent updates: {}", if mode == "off" { "never ask, never restart" } else { "restart when idle, without asking" }), cx));
    }
}

fn send_restart(m: &mut MainWindow, sid: &str, params: serde_json::Value, cx: &mut Context<MainWindow>) {
    remember(m, "when_idle", cx);
    hide(m, sid, |i| i.update_declined = i.update_available.clone(), cx);
    m.rpc("session.restart", params, cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS, cx));
    cx.notify();
}

fn decline(m: &mut MainWindow, sid: &str, cx: &mut Context<MainWindow>) {
    remember(m, "off", cx);
    hide(m, sid, |i| i.update_declined = i.update_available.clone(), cx);
    m.rpc("session.update_decline", json!({"id": sid}), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS, cx));
    cx.notify();
}
