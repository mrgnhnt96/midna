//! System windows: small windows of their own, not tied to a terminal, for something about the
//! Mac or midna itself that wants a decision. One window per kind; showing it again brings it
//! forward with fresh content.
//!
//! - Upgrade postponed: the app updated, but the Mac is too busy to move the daemon onto the new
//!   build safely (the handoff can time out and hang up every terminal). Shown once per launch;
//!   Wait closes it and the lifecycle thread updates by itself once the load drops, Update
//!   anyway forces it. Closes itself when the update goes through.
//! - Overloaded: the Mac has been busy for `guard.overload_secs` (`system.overloaded`). Design B
//!   on the "Overload dialog concepts" canvas: the terminals using the most CPU on the left
//!   (paused ones with what they used), the picked one's latest output on the right with Open
//!   terminal, Pause / Resume and Stop processes (what the terminal started; it stays open),
//!   which asks first. Closes itself on `system.calm`; the status bar's CPU item reopens it.
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
    /// Overloaded: the terminal picked in the list (the heaviest until one is clicked).
    picked: Option<String>,
    /// Its latest output (`session.read`), for the terminal it was read for.
    preview: Option<(String, String)>,
    /// Stop processes is asking to confirm, for this terminal.
    confirm: Option<String>,
}

const WIDTH: f32 = 540.;
const OVERLOAD_SIZE: (f32, f32) = (880., 560.);
/// Lines of output the overload window shows for the picked terminal.
const PREVIEW_LINES: u32 = 14;

/// Open the window (or bring it forward with this load).
pub fn show(kind: Kind, load: SystemLoad, backend: Arc<dyn Backend>, cx: &mut App) {
    if let Some(h) = cx.try_global::<Open>().and_then(|o| o.0.get(&kind).copied())
        && h.update(cx, |w, window, cx| {
            w.set_load(load.clone(), cx);
            window.activate_window();
            cx.notify();
        })
        .is_ok()
    {
        return;
    }
    let (width, height) = match kind {
        Kind::UpgradePostponed => (WIDTH, 262.),
        Kind::Overloaded => OVERLOAD_SIZE,
    };
    let title = match kind {
        Kind::UpgradePostponed => "Midna update waiting",
        Kind::Overloaded => "Your Mac is overloaded",
    };
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(width), px(height)), cx))),
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
            let mut w = SystemWindow { kind, load: SystemLoad::default(), backend, busy: HashMap::new(), error: None, picked: None, preview: None, confirm: None };
            w.set_load(load, cx);
            w
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
            w.set_load(load, cx);
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

/// "busy 4 min".
fn busy_for(l: &SystemLoad) -> String {
    let mins = l.busy_for_secs / 60;
    if mins >= 1 { format!("busy {mins} min") } else { "busy".into() }
}

impl SystemWindow {
    /// New numbers. The picked terminal stays picked while it's listed, else the heaviest is.
    fn set_load(&mut self, load: SystemLoad, cx: &mut Context<Self>) {
        self.load = load;
        let listed = |id: &str| self.load.top.iter().any(|t| t.session_id == id);
        if self.kind == Kind::Overloaded && !self.picked.as_deref().is_some_and(listed) {
            let first = self.load.top.first().map(|t| t.session_id.clone());
            self.pick(first, cx);
        } else if let Some(sid) = self.picked.clone() {
            self.read_preview(sid, cx);
        }
    }

    fn pick(&mut self, sid: Option<String>, cx: &mut Context<Self>) {
        if self.picked != sid {
            self.confirm = None;
        }
        self.picked = sid.clone();
        if let Some(sid) = sid {
            self.read_preview(sid, cx);
        }
        cx.notify();
    }

    /// The picked terminal's latest output.
    fn read_preview(&mut self, sid: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let id = sid.clone();
            let res = cx.background_executor().spawn(async move { backend.call("session.read", json!({ "id": id, "lines": PREVIEW_LINES })) }).await;
            let text = res.ok().and_then(|v| v["text"].as_str().map(|s| s.trim_end().to_string())).unwrap_or_default();
            let _ = this.update(cx, |w, cx| {
                w.preview = Some((sid, text));
                cx.notify();
            });
        })
        .detach();
    }

    /// Pause / Resume / Stop processes on one terminal, then fresh numbers from system.load.
    /// Stopping a paused terminal's processes resumes it too, so it's usable again.
    fn act(&mut self, session: String, what: &'static str, cx: &mut Context<Self>) {
        self.busy.insert(session.clone(), what);
        self.error = None;
        self.confirm = None;
        cx.notify();
        let backend = self.backend.clone();
        let sid = session.clone();
        let paused = self.load.top.iter().any(|t| t.session_id == session && t.paused);
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let p = json!({ "session_id": sid });
                    match what {
                        "pause" => _ = backend.call("session.pause", p)?,
                        "resume" => _ = backend.call("session.resume", p)?,
                        _ => {
                            backend.call("session.stop_processes", p.clone())?;
                            if paused {
                                backend.call("session.resume", p)?;
                            }
                        }
                    }
                    // Stopped processes take a sample (10s) to show; paused ones show at once.
                    Ok::<_, anyhow::Error>(serde_json::from_value::<SystemLoad>(backend.call("system.load", json!({}))?)?)
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                w.busy.remove(&session);
                match res {
                    Ok(l) => w.set_load(l, cx),
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
            .p(px(18.))
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

    /// The list on the left, the picked terminal on the right.
    fn overload_body(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let l = &self.load;
        let list = div()
            .id("overload-list")
            .w(px(300.))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(2.))
            .p(px(10.))
            .border_r_1()
            .border_color(t.line)
            .overflow_y_scroll()
            .children(l.top.iter().enumerate().map(|(i, term)| self.list_row(t, i, term, cx)));
        let picked = self.picked.as_ref().and_then(|id| l.top.iter().find(|t| &t.session_id == id));
        let detail = match picked {
            Some(term) => self.detail(t, term, cx).into_any_element(),
            None => div()
                .flex_1()
                .p(px(20.))
                .text_color(t.dim)
                .child("None of Midna's terminals is using much CPU; something outside Midna is.")
                .into_any_element(),
        };
        div().flex().flex_1().min_h_0().child(list).child(detail)
    }

    fn list_row(&self, t: &Theme, i: usize, term: &HeavyTerminal, cx: &mut Context<Self>) -> Stateful<Div> {
        let picked = self.picked.as_deref() == Some(term.session_id.as_str());
        let teal = crate::ui::paused_color(t);
        let sid = term.session_id.clone();
        let second = if term.paused {
            div().text_size(px(12.)).text_color(teal).child(format!("paused · was {}%", term.cpu_percent))
        } else {
            let heavy = term.cpu_percent >= 100;
            let share = (term.cpu_percent as f32 / (self.load.cpus.max(1) * 100) as f32).clamp(0.02, 1.);
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().w(px(90.)).h(px(5.)).rounded_full().overflow_hidden().bg(t.line).child(div().h_full().rounded_full().w(relative(share)).bg(if heavy { t.status_color("orange") } else { t.dim })))
                .child(div().text_size(px(12.)).text_color(if heavy { t.status_color("orange") } else { t.dim }).child(format!("{}%", term.cpu_percent)))
        };
        let dot = div().size(px(8.)).flex_none().rounded_full().bg(if term.paused { teal } else { t.work });
        div()
            .id(("overload-row", i))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(9.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(picked, |d| d.bg(t.raised))
            .when(!picked, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
            .on_click(cx.listener(move |w, _, _, cx| w.pick(Some(sid.clone()), cx)))
            .child(dot)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::BOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(term.name.clone()))
                    .child(second),
            )
    }

    fn detail(&self, t: &Theme, term: &HeavyTerminal, cx: &mut Context<Self>) -> Div {
        let teal = crate::ui::paused_color(t);
        let sid = term.session_id.clone();
        let busy = self.busy.get(&sid).copied();
        let confirming = self.confirm.as_deref() == Some(sid.as_str());
        let line = {
            let cpu = if term.paused { format!("{}% CPU before pausing", term.cpu_percent) } else { format!("{}% CPU", term.cpu_percent) };
            let mut parts = vec![cpu, crate::ui::paused::processes(term.processes)];
            if !term.busiest.is_empty() {
                parts.push(term.busiest.join(", "));
            }
            if term.backgrounded {
                parts.push("slowed down".into());
            }
            parts.join(" · ")
        };
        let text = self.preview.as_ref().filter(|(id, _)| *id == sid).map(|(_, x)| x.clone()).unwrap_or_default();
        let output = div()
            .id("overload-output")
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .flex()
            .flex_col()
            .justify_end()
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.line)
            .bg(t.term)
            .font_family(t.mono_font.clone())
            .text_size(px(12.))
            .line_height(px(19.))
            .text_color(t.dim)
            .children(text.lines().map(|l| div().whitespace_nowrap().child(if l.is_empty() { " ".to_string() } else { l.to_string() })))
            .when(term.paused, |d| d.child(div().pt(px(4.)).text_color(teal).child("── paused ──")));
        let (s1, s2, s3, s4) = (sid.clone(), sid.clone(), sid.clone(), sid.clone());
        let open = btn(t, "overload-open", "Open terminal").on_click(cx.listener(move |w, _, _, _| {
            let backend = w.backend.clone();
            let id = s1.clone();
            std::thread::spawn(move || _ = backend.call("session.focus", json!({ "id": id })));
        }));
        let open = open.child(crate::icons::Icon::PopOut.el(12., t.fg));
        let toggle = if term.paused {
            btn_primary(t, "overload-resume", if busy == Some("resume") { "Resuming…" } else { "Resume" }).on_click(cx.listener(move |w, _, _, cx| w.act(s2.clone(), "resume", cx)))
        } else {
            btn_primary(t, "overload-pause", if busy == Some("pause") { "Pausing…" } else { "Pause" })
                .bg(teal)
                .border_color(teal)
                .text_color(t.bg)
                .child(crate::icons::Icon::Pause.el(11., t.bg))
                .on_click(cx.listener(move |w, _, _, cx| w.act(s2.clone(), "pause", cx)))
        };
        let actions = if confirming {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.err)
                .bg(t.err.opacity(0.08))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::BOLD).child(format!("Stop {}?", crate::ui::paused::processes(term.processes))))
                        .child(div().text_color(t.dim).text_size(px(12.5)).child(crate::ui::paused::stop_line(&term.busiest))),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(8.))
                        .child(btn(t, "overload-cancel", "Cancel").on_click(cx.listener(|w, _, _, cx| {
                            w.confirm = None;
                            cx.notify();
                        })))
                        .child(btn_danger(t, "overload-stop-yes", if busy == Some("stop") { "Stopping…" } else { "Stop processes" }, 28.).on_click(cx.listener(move |w, _, _, cx| w.act(s3.clone(), "stop", cx)))),
                )
        } else {
            div().flex().items_center().gap(px(8.)).child(open).child(toggle).child(div().flex_1()).child(
                btn(t, "overload-stop", "Stop processes…").text_color(t.err).on_click(cx.listener(move |w, _, _, cx| {
                    w.confirm = Some(s4.clone());
                    cx.notify();
                })),
            )
        };
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(18.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::BOLD).text_size(px(16.)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(term.name.clone()))
                    .when(term.paused, |d| d.child(crate::ui::custom_status_label(t, &crate::model::CustomStatus { label: "paused".into(), color: "teal".into(), ..Default::default() }, 11.))),
            )
            .child(div().text_color(t.dim).text_size(px(12.5)).overflow_hidden().text_ellipsis().whitespace_nowrap().child(line))
            .child(output)
            .when(!term.paused && !confirming, |d| d.child(div().text_color(t.dim).text_size(px(12.)).child("Pause holds these processes where they are; Resume picks up where they left off.")))
            .children(self.error.as_ref().map(|e| div().text_color(t.err).text_size(px(12.)).child(e.clone())))
            .child(actions)
    }
}

impl Render for SystemWindow {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let heading = match self.kind {
            Kind::UpgradePostponed => "Midna's update is waiting for a calmer moment",
            Kind::Overloaded => "Your Mac is overloaded",
        };
        let aside = (self.kind == Kind::Overloaded).then(|| format!("CPU {}% · {}", self.load.load_percent, busy_for(&self.load)));
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
            .child(
                div()
                    .flex_none()
                    .h(px(40.))
                    .pl(px(84.))
                    .pr(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .bg(t.panel)
                    .child(div().flex_1().font_weight(FontWeight::BOLD).child(heading))
                    .children(aside.map(|a| div().text_color(t.dim).text_size(px(12.)).child(a))),
            )
            .child(div().flex().flex_col().flex_1().min_h_0().child(body))
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
        backgrounded: false,
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
