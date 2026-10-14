//! System windows: small windows of their own, not tied to a terminal, for something about the
//! Mac or midna itself that wants a decision. One window per kind; showing it again brings it
//! forward with fresh content.
//!
//! - Upgrade postponed: the app updated, but the Mac is too busy to move the daemon onto the new
//!   build safely (the handoff can time out and hang up every terminal). Shown once per launch;
//!   Wait closes it and the lifecycle thread updates by itself once the load drops, Update
//!   anyway forces it. Closes itself when the update goes through.
//! - Overloaded: the Mac's CPU has been busy for `guard.overload_secs` (`system.overloaded`).
//!   Design B on the "Overload: real CPU" canvas: one sentence on who is using the Mac (midna's
//!   terminals or other apps), a bar of the Mac's CPU split between the two, then a column for
//!   each, the cause first: the terminals with Pause / Resume (a name opens the terminal; a paused
//!   terminal's card has Stop processes), and the other apps, which midna can't pause. Every
//!   percent is a share of the whole Mac. Closes itself on `system.calm`; the status bar's CPU
//!   item reopens it.
//!
//! Dev: `MIDNA_DEBUG_SCREEN=system-upgrade` / `system-overload` opens them with sample data.
use crate::backend::Backend;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::SystemLoad;
use midna_proto::system::{midna_is_the_cause, share_label, share_of_mac};
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
    /// A Pause / Resume in flight, per terminal.
    busy: HashMap<String, &'static str>,
    error: Option<String>,
}

const WIDTH: f32 = 540.;
const OVERLOAD_SIZE: (f32, f32) = (880., 560.);

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

/// "busy 4 min".
fn busy_for(l: &SystemLoad) -> String {
    let mins = l.busy_for_secs / 60;
    if mins >= 1 { format!("busy {mins} min") } else { "busy".into() }
}

/// The color of other apps' share of the CPU, beside `t.work` for midna's terminals.
pub fn other_apps_color(t: &Theme) -> Hsla {
    t.dim.opacity(0.7)
}

/// A per-core percent as a share of the Mac: `5%`, `<1%`.
pub fn mac_share(cpu_percent: u32, cpus: u32) -> String {
    share_label(share_of_mac(cpu_percent, cpus))
}

/// Who is using the Mac, in one sentence, and the line under it.
pub fn verdict(l: &SystemLoad) -> (String, String) {
    let midna = share_label(l.midna_percent as f64);
    if !l.cpu_busy && !l.overloaded {
        return ("Your Mac has calmed down".into(), format!("{}% of its CPU is in use now.", l.cpu_percent));
    }
    if midna_is_the_cause(l.cpu_percent, l.midna_percent) {
        let slowed = if l.top.iter().any(|t| t.backgrounded) { ", and the busiest terminals are slowed down" } else { "" };
        ("Midna's terminals are using your Mac".into(), format!("They use {midna} of it. Agents wait before starting more builds, tests or subagents{slowed}."))
    } else {
        ("Other apps are using your Mac".into(), format!("Midna's terminals use {midna} of it, so your agents carry on as normal: nothing is held back or slowed down."))
    }
}

/// The Mac's CPU as one bar: midna's terminals, then other apps, the rest free.
pub fn cpu_bar(t: &Theme, l: &SystemLoad, h: f32) -> Div {
    let other = l.cpu_percent.saturating_sub(l.midna_percent);
    div()
        .flex()
        .gap(px(2.))
        .h(px(h))
        .w_full()
        .rounded(px(h / 2.))
        .overflow_hidden()
        .bg(t.line)
        .child(div().h_full().bg(t.work).w(relative(l.midna_percent as f32 / 100.)))
        .child(div().h_full().bg(other_apps_color(t)).w(relative(other as f32 / 100.)))
}

fn open_activity_monitor() {
    let _ = std::process::Command::new("open").args(["-a", "Activity Monitor"]).spawn();
}

/// One column of the overload window: a colored heading with its share, rows, a line under them.
fn column(t: &Theme, id: &'static str, color: Hsla, label: &str, pct: String, rows: Vec<AnyElement>, foot: String) -> Div {
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .flex()
        .flex_col()
        .gap(px(4.))
        .p(px(12.))
        .rounded(px(10.))
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(4.))
                .pb(px(6.))
                .child(div().size(px(10.)).flex_none().rounded(px(3.)).bg(color))
                .child(div().flex_1().font_weight(FontWeight::BOLD).child(label.to_string()))
                .child(div().font_weight(FontWeight::BOLD).child(pct)),
        )
        .child(div().id(id).flex_1().min_h_0().overflow_y_scroll().flex().flex_col().children(rows))
        .child(div().px(px(4.)).pt(px(6.)).text_size(px(12.)).text_color(t.dim).child(foot))
}

/// A row: its name (and what it runs), its share of the Mac, and a button.
fn row(t: &Theme, name: AnyElement, what: String, pct: String, action: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(4.))
        .py(px(7.))
        .border_t_1()
        .border_color(t.raised)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(name)
                .when(!what.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(t.dim).overflow_hidden().text_ellipsis().whitespace_nowrap().child(what))),
        )
        .child(div().flex_none().w(px(38.)).text_right().text_size(px(12.)).text_color(t.dim).child(pct))
        .children(action)
}

impl SystemWindow {
    fn set_load(&mut self, load: SystemLoad) {
        self.load = load;
    }

    /// Pause / Resume one terminal, then fresh numbers from system.load.
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
                    backend.call(if what == "pause" { "session.pause" } else { "session.resume" }, json!({ "session_id": sid }))?;
                    // A paused terminal shows at once; its CPU leaves with the next sample (10s).
                    Ok::<_, anyhow::Error>(serde_json::from_value::<SystemLoad>(backend.call("system.load", json!({}))?)?)
                })
                .await;
            let _ = this.update(cx, |w, cx| {
                w.busy.remove(&session);
                match res {
                    Ok(l) => w.set_load(l),
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

    /// The sentence and the bar, then the cause's column first.
    fn overload_body(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let l = &self.load;
        let (verdict, sub) = verdict(l);
        let head = div()
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(12.))
            .px(px(24.))
            .pt(px(22.))
            .pb(px(18.))
            .child(div().text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::BOLD).child(verdict))
            .child(div().text_color(t.dim).text_size(px(13.5)).child(sub))
            .child(div().pt(px(4.)).child(cpu_bar(t, l, 14.)));
        let terms = self.midna_column(t, cx);
        let apps = self.other_column(t);
        let cols = if midna_is_the_cause(l.cpu_percent, l.midna_percent) { [terms, apps] } else { [apps, terms] };
        let footer = div()
            .flex_none()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(24.))
            .py(px(12.))
            .border_t_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(div().text_color(t.dim).text_size(px(12.)).child(format!("Load {:.1} on {} cores", l.load1, l.cpus)))
            .children(self.error.as_ref().map(|e| div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_color(t.err).text_size(px(12.)).child(e.clone())))
            .child(div().flex_1())
            .child(btn(t, "overload-activity", "Open Activity Monitor").on_click(|_, _, _| open_activity_monitor()))
            .child(btn_primary(t, "overload-ok", "OK").on_click(|_, window, _| window.remove_window()));
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(head)
            .child(div().flex_1().min_h_0().flex().gap(px(14.)).px(px(24.)).pb(px(18.)).children(cols))
            .child(footer)
    }

    /// Midna's terminals, each with Pause or Resume; a name opens its terminal.
    fn midna_column(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let l = &self.load;
        let teal = crate::ui::paused_color(t);
        let rows: Vec<AnyElement> = l
            .top
            .iter()
            .enumerate()
            .map(|(i, term)| {
                let sid = term.session_id.clone();
                let busy = self.busy.contains_key(&sid);
                let mut what = vec![];
                if term.paused {
                    what.push(format!("paused · was {}", mac_share(term.cpu_percent, l.cpus)));
                }
                if !term.busiest.is_empty() {
                    what.push(term.busiest.join(", "));
                }
                if term.backgrounded {
                    what.push("slowed down".into());
                }
                let pct = if term.paused { "0%".to_string() } else { mac_share(term.cpu_percent, l.cpus) };
                let (s1, s2) = (sid.clone(), sid.clone());
                let name = div()
                    .id(("overload-term", i))
                    .cursor_pointer()
                    .font_weight(FontWeight::SEMIBOLD)
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .hover(|s| s.text_color(t.accent))
                    .child(term.name.clone())
                    .on_click(cx.listener(move |w, _, _, _| {
                        let backend = w.backend.clone();
                        let id = s1.clone();
                        std::thread::spawn(move || _ = backend.call("session.focus", json!({ "id": id })));
                    }));
                let action = if term.paused {
                    btn_primary(t, ("overload-resume", i), if busy { "Resuming…" } else { "Resume" }).on_click(cx.listener(move |w, _, _, cx| w.act(s2.clone(), "resume", cx)))
                } else {
                    btn(t, ("overload-pause", i), if busy { "Pausing…" } else { "Pause" }).border_color(teal).text_color(teal).on_click(cx.listener(move |w, _, _, cx| w.act(s2.clone(), "pause", cx)))
                };
                row(t, name.into_any_element(), what.join(" · "), pct, Some(action.into_any_element())).into_any_element()
            })
            .collect();
        let foot = if rows.is_empty() { "Nothing in Midna is using much CPU." } else { "Pause holds a terminal where it is; Resume picks up again." };
        column(t, "overload-midna", t.work, "Midna's terminals", share_label(l.midna_percent as f64), rows, foot.into())
    }

    /// Other apps and processes, and macOS's own; midna can't pause these.
    fn other_column(&self, t: &Theme) -> Div {
        let l = &self.load;
        let label = |s: &str| div().font_weight(FontWeight::SEMIBOLD).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.to_string()).into_any_element();
        let mut rows: Vec<AnyElement> = l.others.iter().map(|o| row(t, label(&o.name), o.busiest.join(", "), mac_share(o.cpu_percent, l.cpus), None).into_any_element()).collect();
        if l.system_percent > 0 {
            rows.push(row(t, label("macOS"), "system processes and other users".into(), share_label(l.system_percent as f64), None).into_any_element());
        }
        let other = l.cpu_percent.saturating_sub(l.midna_percent);
        column(t, "overload-other", other_apps_color(t), "Other apps", share_label(other as f64), rows, "Midna can't pause these. Quit them in their app.".into())
    }
}

impl Render for SystemWindow {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let heading = match self.kind {
            Kind::UpgradePostponed => "Midna's update is waiting for a calmer moment",
            Kind::Overloaded => "Your Mac is overloaded",
        };
        let aside = (self.kind == Kind::Overloaded).then(|| format!("Mac CPU {}% · {}", self.load.cpu_percent, busy_for(&self.load)));
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

/// Dev (`MIDNA_DEBUG_SCREEN=system-upgrade|system-overload[-midna]`): open one with sample
/// numbers (other apps keeping the Mac busy, or `-midna`: its terminals).
pub fn debug(which: &str, backend: Arc<dyn Backend>, cx: &mut App) {
    let term = |id: &str, name: &str, cpu, n, busiest: &[&str], paused, backgrounded| midna_proto::HeavyTerminal {
        session_id: id.into(),
        name: name.into(),
        cwd: String::new(),
        cpu_percent: cpu,
        processes: n,
        busiest: busiest.iter().map(|s| s.to_string()).collect(),
        paused,
        backgrounded,
    };
    let app = |name: &str, cpu, n, busiest: &[&str]| midna_proto::OtherProcess { name: name.into(), cpu_percent: cpu, processes: n, busiest: busiest.iter().map(|s| s.to_string()).collect() };
    let ours = which.ends_with("-midna");
    let (cpu, midna, top, others, system) = if ours {
        (
            94,
            86,
            vec![
                term("s1", "taskboard backlog agents", 940, 41, &["rustc ×14", "cargo ×4", "ld"], false, true),
                term("s2", "kass dictation", 72, 9, &["node ×3"], false, false),
                term("s3", "trading backtest", 85, 4, &["python3"], true, false),
            ],
            vec![app("WindowServer", 48, 1, &[]), app("Spotlight", 36, 3, &["mds_stores", "mdworker_shared ×2"])],
            1,
        )
    } else {
        (
            88,
            6,
            vec![
                term("s1", "Periodic check-ins for long-running agents", 65, 7, &["claude", "node ×3"], false, false),
                term("s2", "T254 Goals list with rows that expand", 4, 3, &["claude"], false, false),
                term("s3", "T296 Photo storage interface", 2, 3, &["claude"], false, false),
            ],
            vec![app("Xcode", 612, 24, &["clang ×22", "XCBBuildService"]), app("fseventsd", 120, 1, &[]), app("dartvm", 96, 1, &[])],
            14,
        )
    };
    let load = SystemLoad {
        load1: 46.9,
        load5: 35.9,
        load15: 29.8,
        cpus: 12,
        load_percent: 391,
        busy_at: 200,
        busy: true,
        cpu_percent: cpu,
        midna_percent: midna,
        busy_cpu_at: 90,
        cpu_busy: true,
        held: ours,
        busy_for_secs: 180,
        overloaded: true,
        top,
        others,
        system_percent: system,
    };
    let kind = if which == "system-upgrade" { Kind::UpgradePostponed } else { Kind::Overloaded };
    show(kind, load, backend, cx);
}
