//! System windows: small windows of their own, not tied to a terminal, for something about the
//! Mac or midna itself that wants a decision. One window per kind; showing it again brings it
//! forward with fresh content.
//!
//! - Upgrade postponed: the app updated, but the Mac is too busy to move the daemon onto the new
//!   build safely (the handoff can time out and hang up every terminal). Shown once per launch;
//!   Wait closes it and the lifecycle thread updates by itself once the load drops, Update
//!   anyway forces it. Closes itself when the update goes through.
//! - Overloaded: the Mac has been busy for `guard.overload_secs` (`system.overloaded`). Lists
//!   the terminals using the most CPU with Pause / Resume and Stop processes (what the terminal
//!   started; its shell or agent keeps running). Closes itself on `system.calm`.
//!
//! Dev: `MIDNA_DEBUG_SCREEN=system-upgrade` / `system-overload` opens them with sample data.
use crate::backend::Backend;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_danger, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{HeavyTerminal, SystemLoad};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    UpgradePostponed,
    Overloaded,
}

#[derive(Default)]
struct Open(HashMap<Kind, WindowHandle<SystemWindow>>);
impl Global for Open {}

pub struct SystemWindow {
    kind: Kind,
    load: SystemLoad,
    backend: Arc<dyn Backend>,
    /// A Pause / Resume / Stop in flight, per terminal.
    busy: HashMap<String, &'static str>,
    error: Option<String>,
}

const WIDTH: f32 = 540.;

/// Open the window (or bring it forward with this load).
pub fn show(kind: Kind, load: SystemLoad, backend: Arc<dyn Backend>, cx: &mut App) {
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&kind).copied())
        && h.update(cx, |w, window, cx| {
            w.load = load.clone();
            window.activate_window();
            cx.notify();
        })
        .is_ok()
    {
        return;
    }
    let rows = if kind == Kind::Overloaded { load.top.len().max(1) as f32 } else { 0. };
    let height = match kind {
        Kind::UpgradePostponed => 262.,
        Kind::Overloaded => 210. + rows * 62.,
    };
    let title = match kind {
        Kind::UpgradePostponed => "Midna update waiting",
        Kind::Overloaded => "Your Mac is overloaded",
    };
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(height)), cx))),
        titlebar: Some(TitlebarOptions { title: Some(title.into()), appears_transparent: true, traffic_light_position: Some(point(px(14.), px(15.))) }),
        kind: WindowKind::Normal,
        is_resizable: false,
        app_id: Some("com.mrgnhnt.midna".into()),
        ..Default::default()
    };
    let opened = cx.open_window(opts, |_, cx| {
        cx.new(|cx| {
            cx.on_release(move |_: &mut SystemWindow, cx| {
                cx.default_global::<Open>().0.remove(&kind);
            })
            .detach();
            SystemWindow { kind, load, backend, busy: HashMap::new(), error: None }
        })
    });
    match opened {
        Ok(h) => {
            let _ = h.update(cx, |_, window, _| window.activate_window());
            cx.default_global::<Open>().0.insert(kind, h);
        }
        Err(e) => eprintln!("midna-app: system window: {e:#}"),
    }
}

/// New numbers for the window if it's open (it isn't brought forward).
pub fn refresh(kind: Kind, load: SystemLoad, cx: &mut App) {
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&kind).copied()) {
        let _ = h.update(cx, |w, _, cx| {
            w.load = load;
            cx.notify();
        });
    }
}

pub fn close(kind: Kind, cx: &mut App) {
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&kind).copied()) {
        let _ = h.update(cx, |_, window, _| window.remove_window());
    }
}

/// "load 48.6 on 12 cores (405%)".
fn load_line(l: &SystemLoad) -> String {
    format!("load {:.1} on {} cores ({}%)", l.load1, l.cpus, l.load_percent)
}

impl SystemWindow {
    /// Pause / Resume / Stop processes on one terminal, then fresh numbers from system.load.
    fn act(&mut self, session: String, what: &'static str, cx: &mut Context<Self>) {
        self.busy.insert(session.clone(), what);
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let sid = session.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let (method, p) = match what {
                        "pause" => ("session.pause", json!({ "session_id": sid })),
                        "resume" => ("session.resume", json!({ "session_id": sid })),
                        _ => ("session.stop_processes", json!({ "session_id": sid })),
                    };
                    backend.call(method, p)?;
                    // Stopped processes take a sample (10s) to show; paused ones show at once.
                    Ok::<_, anyhow::Error>(serde_json::from_value::<SystemLoad>(backend.call("system.load", json!({}))?)?)
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                w.busy.remove(&session);
                match res {
                    Ok(l) => w.load = l,
                    Err(e) => w.error = Some(format!("{e:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn upgrade_body(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let l = &self.load;
        let text = |s: String| div().text_color(t.fg).child(s);
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(text(format!("Your Mac is very busy right now: {}.", load_line(l))))
            .child(text(
                "Updating moves every terminal over to the new version of Midna's background service. Under this much \
                 load that handoff can time out and close your terminals, so Midna recommends waiting."
                    .into(),
            ))
            .child(div().text_color(t.dim).child("Midna finishes the update by itself once things calm down."))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .pt(px(6.))
                    .child(btn(t, "update-anyway", "Update anyway").on_click(cx.listener(|_, _, window, cx| {
                        crate::lifecycle::command(crate::lifecycle::Cmd::UpgradeDaemonNow, cx);
                        window.remove_window();
                    })))
                    .child(btn_primary(t, "wait", "Wait (recommended)").on_click(|_, window, _| window.remove_window())),
            )
    }

    fn overload_body(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let l = &self.load;
        let mins = l.busy_for_secs / 60;
        let how_long = if mins >= 1 { format!(" for {mins} min") } else { String::new() };
        let mut rows = div().flex().flex_col().gap(px(6.));
        if l.top.is_empty() {
            rows = rows.child(div().text_color(t.dim).child("None of Midna's terminals is using much CPU; something outside Midna is."));
        }
        for (i, term) in l.top.iter().enumerate() {
            rows = rows.child(self.row(t, i, term, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(div().text_color(t.fg).child(format!("The Mac has been busy{how_long}: {}. Terminals using the most CPU:", load_line(l))))
            .child(rows)
            .children(self.error.as_ref().map(|e| div().text_color(t.err).text_size(px(12.)).child(e.clone())))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .pt(px(4.))
                    .child(div().text_color(t.dim).text_size(px(12.)).child("Pause stops a terminal until you resume it; nothing is lost."))
                    .child(btn(t, "dismiss", "Dismiss").on_click(|_, window, _| window.remove_window())),
            )
    }

    fn row(&self, t: &Theme, i: usize, term: &HeavyTerminal, cx: &mut Context<Self>) -> Div {
        let sid = term.session_id.clone();
        let busy = self.busy.get(&sid).copied();
        let detail = {
            let mut parts = vec![format!("{}% CPU", term.cpu_percent), format!("{} processes", term.processes)];
            if !term.busiest.is_empty() {
                parts.push(term.busiest.join(", "));
            }
            parts.join(" · ")
        };
        let (s1, s2) = (sid.clone(), sid.clone());
        let pause = if term.paused {
            btn_primary(t, ("resume", i), if busy == Some("resume") { "Resuming…" } else { "Resume" }).on_click(cx.listener(move |w, _, _, cx| w.act(s1.clone(), "resume", cx)))
        } else {
            btn(t, ("pause", i), if busy == Some("pause") { "Pausing…" } else { "Pause" }).on_click(cx.listener(move |w, _, _, cx| w.act(s1.clone(), "pause", cx)))
        };
        let stop = btn_danger(t, ("stop", i), if busy == Some("stop") { "Stopping…" } else { "Stop processes" }, 28.)
            .on_click(cx.listener(move |w, _, _, cx| w.act(s2.clone(), "stop", cx)));
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(div().font_weight(FontWeight::BOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(term.name.clone()))
                            .children(term.paused.then(|| div().text_color(t.need).text_size(px(11.)).child("paused"))),
                    )
                    .child(div().text_color(t.dim).text_size(px(12.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(detail)),
            )
            .child(pause)
            .child(stop)
    }
}

impl Render for SystemWindow {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let heading = match self.kind {
            Kind::UpgradePostponed => "Midna's update is waiting for a calmer moment",
            Kind::Overloaded => "Your Mac is overloaded",
        };
        let body = match self.kind {
            Kind::UpgradePostponed => self.upgrade_body(&t, cx),
            Kind::Overloaded => self.overload_body(&t, cx),
        };
        div()
            .key_context(crate::actions::CTX_MAIN)
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(13.))
            .line_height(px(19.))
            .child(div().flex_none().h(px(40.)).pl(px(84.)).flex().items_center().border_b_1().border_color(t.line).bg(t.panel).font_weight(FontWeight::BOLD).child(heading))
            .child(div().flex_1().p(px(18.)).child(body))
    }
}

/// Dev (`MIDNA_DEBUG_SCREEN=system-upgrade|system-overload`): open one with sample numbers.
pub fn debug(which: &str, backend: Arc<dyn Backend>, cx: &mut App) {
    let term = |id: &str, name: &str, cpu, n, busiest: &[&str], paused| HeavyTerminal {
        session_id: id.into(),
        name: name.into(),
        cwd: String::new(),
        cpu_percent: cpu,
        processes: n,
        busiest: busiest.iter().map(|s| s.to_string()).collect(),
        paused,
    };
    let load = SystemLoad {
        load1: 48.6,
        load5: 61.2,
        load15: 40.3,
        cpus: 12,
        load_percent: 405,
        busy_at: 150,
        busy: true,
        busy_for_secs: 420,
        overloaded: true,
        top: vec![
            term("s1", "taskboard backlog agents", 940, 41, &["rustc ×14", "cargo ×4", "ld"], false),
            term("s2", "kass dictation", 120, 9, &["node ×3"], false),
            term("s3", "trading backtest", 85, 4, &["python3"], true),
        ],
    };
    let kind = if which == "system-upgrade" { Kind::UpgradePostponed } else { Kind::Overloaded };
    show(kind, load, backend, cx);
}
