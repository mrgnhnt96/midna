//! A busy Mac and paused terminals in the main window ("Overload dialog concepts" canvas):
//! - the card over a paused terminal (concept 3): what it was running, Resume, and Stop
//!   processes with a confirm;
//! - the status bar item while the Mac is overloaded or anything is paused:
//!   "● Mac CPU 88% · other apps · 2 paused" (design B on the "Overload: real CPU" canvas), its
//!   hover who is using the Mac in one sentence, the CPU bar and both shares, a click opening
//!   the overload window;
//! - `system.load`, followed while either shows.
use crate::app::{MainWindow, refresh};
use crate::icons::Icon;
use crate::model::Session;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_danger, btn_primary};
use crate::ui::system_window::{self as sw, Kind};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::SystemLoad;
use serde_json::{Value, json};
use std::time::Duration;

pub const POLL: Duration = Duration::from_secs(10);

/// Whether `system.load` is worth following: the Mac is overloaded or a terminal is paused.
fn watching(m: &MainWindow) -> bool {
    m.load.as_ref().is_some_and(|l| l.overloaded) || m.sessions.iter().any(|s| s.paused.is_some())
}

/// Every `POLL`: fresh numbers while the status item (or the overload window) shows them.
pub fn poll(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if !watching(m) {
        if m.load.take().is_some() {
            cx.notify();
        }
        return;
    }
    fetch(m, cx);
}

fn fetch(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let res = cx.background_executor().spawn(async move { backend.call("system.load", json!({})) }).await;
        let Ok(l) = res.and_then(|v| Ok(serde_json::from_value::<SystemLoad>(v)?)) else { return };
        let _ = this.update(cx, |m, cx| {
            sw::refresh(Kind::Overloaded, l.clone(), cx);
            m.load = Some(l);
            cx.notify();
        });
    })
    .detach();
}

/// `system.overloaded` carries the load; `system.calm` means it dropped (fetch the rest).
pub fn on_load_event(m: &mut MainWindow, kind: &str, data: &Value, cx: &mut Context<MainWindow>) {
    if kind == midna_proto::kinds::SYSTEM_OVERLOADED {
        if let Ok(l) = serde_json::from_value::<SystemLoad>(data.clone()) {
            m.load = Some(l);
            cx.notify();
        }
    } else if let Some(l) = m.load.as_mut() {
        l.overloaded = false;
        fetch(m, cx);
    }
}

fn paused_count(m: &MainWindow) -> usize {
    m.sessions.iter().filter(|s| s.paused.is_some()).count()
}

/// "rustc ×11" → ("rustc", "×11").
fn name_and_count(b: &str) -> (&str, &str) {
    b.split_once(" ×").map_or((b, ""), |(n, c)| (n, if c.is_empty() { "" } else { &b[n.len() + 1..] }))
}

/// "rustc, zig and cargo" from the busiest list.
pub fn names(busiest: &[String]) -> String {
    let n: Vec<&str> = busiest.iter().map(|b| name_and_count(b).0).collect();
    match n.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// What Stop processes says it does.
pub fn stop_line(busiest: &[String]) -> String {
    let n = names(busiest);
    if n.is_empty() { "Ends what it started. The terminal stays open.".into() } else { format!("Ends {n}. The terminal stays open.") }
}

pub fn processes(n: u32) -> String {
    if n == 1 { "1 process".into() } else { format!("{n} processes") }
}

/// "3 min ago" from an RFC 3339 time.
fn ago(ts: &str) -> String {
    let Some(t) = crate::model::parse_rfc3339(ts) else { return String::new() };
    let d = (midna_proto::time::now_unix() - t).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86400),
    }
}

// ------------------------------------------------------------------ the card over a paused terminal

/// Resume the terminal, or stop what it started (and resume it, so the terminal is usable).
fn act(m: &mut MainWindow, sid: String, stop: bool, cx: &mut Context<MainWindow>) {
    m.pause_confirm = None;
    m.pause_busy = Some(sid.clone());
    cx.notify();
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let res = cx
            .background_executor()
            .spawn(async move {
                if stop {
                    backend.call("session.stop_processes", json!({ "session_id": sid }))?;
                }
                backend.call("session.resume", json!({ "session_id": sid }))
            })
            .await;
        let _ = this.update(cx, |m, cx| {
            m.pause_busy = None;
            if let Err(e) = res {
                m.toast(format!("{:#}", e), cx);
            }
            m.request_refresh(refresh::SESSIONS | refresh::ROWS | refresh::HEADER, cx);
        });
    })
    .detach();
}

/// The terminal pane, with the paused card over it while the selected terminal is paused.
pub fn over_pane(m: &MainWindow, pane: AnyElement, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let Some(s) = m.selected_session().filter(|s| s.paused.is_some()) else { return pane };
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(pane)
        .child(
            div()
                .id("paused-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(t.term.opacity(0.55))
                .flex()
                .items_center()
                .justify_center()
                // the card stands in for the terminal: clicks and scrolls stop here
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .child(card(m, s, t, cx)),
        )
        .into_any_element()
}

fn card(m: &MainWindow, s: &Session, t: &Theme, cx: &mut Context<MainWindow>) -> Div {
    let p = s.paused.clone().unwrap_or_default();
    let teal = super::paused_color(t);
    let busy = m.pause_busy.as_deref() == Some(s.id.as_str());
    let confirming = m.pause_confirm.as_deref() == Some(s.id.as_str());
    let mut sub = format!("Paused {}", ago(&p.since));
    if p.cpu_percent > 0 {
        sub = format!("{sub} · it was using {}% CPU", p.cpu_percent);
    }
    let held = div().flex().flex_col().gap(px(6.)).children(p.busiest.iter().map(|b| {
        let (name, count) = name_and_count(b);
        div().flex().justify_between().child(name.to_string()).child(div().text_color(t.dim).child(count.to_string()))
    }));
    let now = m.load.as_ref().filter(|l| l.cpu_percent > 0).map(|l| format!("The Mac's CPU is at {}% now.", l.cpu_percent));
    let (sid1, sid2, sid3) = (s.id.clone(), s.id.clone(), s.id.clone());
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
                    .child(div().font_weight(FontWeight::BOLD).child(format!("Stop {}?", processes(p.processes))))
                    .child(div().text_color(t.dim).text_size(px(12.5)).child(stop_line(&p.busiest))),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(btn(t, "paused-cancel", "Cancel").on_click(cx.listener(|m, _, _, cx| {
                        m.pause_confirm = None;
                        cx.notify();
                    })))
                    .child(btn_danger(t, "paused-stop-yes", "Stop processes", 28.).on_click(cx.listener(move |m, _, _, cx| act(m, sid3.clone(), true, cx)))),
            )
    } else {
        div()
            .flex()
            .gap(px(8.))
            .child(
                btn_primary(t, "paused-resume", if busy { "Resuming…" } else { "Resume" })
                    .flex_1()
                    .justify_center()
                    .h(px(34.))
                    .on_click(cx.listener(move |m, _, _, cx| act(m, sid1.clone(), false, cx))),
            )
            .child(btn(t, "paused-stop", "Stop processes").h(px(34.)).on_click(cx.listener(move |m, _, _, cx| {
                m.pause_confirm = Some(sid2.clone());
                cx.notify();
            })))
    };
    div()
        .w(px(400.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(20.))
        .rounded(px(12.))
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.45), offset: point(px(0.), px(16.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().size(px(36.)).flex_none().rounded(px(9.)).bg(teal.opacity(0.14)).flex().items_center().justify_center().child(Icon::Pause.el(16., teal)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .child(div().font_weight(FontWeight::BOLD).text_size(px(15.)).child("This terminal is paused"))
                        .child(div().text_color(t.dim).text_size(px(12.5)).child(sub)),
                ),
        )
        .when(!p.busiest.is_empty(), |d| d.child(held))
        .children(now.map(|n| div().text_color(t.dim).text_size(px(12.5)).child(n)))
        .child(actions)
}

// ------------------------------------------------------------------ status bar item

/// "● Mac CPU 88% · other apps · 2 paused" while the Mac is overloaded or anything is paused.
pub fn status_item(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let paused = paused_count(m);
    let over = m.load.as_ref().filter(|l| l.overloaded);
    if over.is_none() && paused == 0 {
        return None;
    }
    let teal = super::paused_color(t);
    let fg = t.fg;
    let load = m.load.clone().unwrap_or_default();
    let backend = m.backend.clone();
    Some(
        div()
            .id("load")
            .flex()
            .items_center()
            .gap(px(5.))
            .cursor_pointer()
            .text_color(t.fg)
            .hover(move |s| s.text_color(fg))
            .when_some(over, |d, l| {
                let theirs = !midna_proto::system::midna_is_the_cause(l.cpu_percent, l.midna_percent);
                d.child(div().text_color(t.status_color("orange")).child("●"))
                    .child(format!("Mac CPU {}%", l.cpu_percent))
                    .when(theirs, |d| d.child(div().text_color(t.dim).child("· other apps")))
            })
            .when(over.is_some() && paused > 0, |d| d.child(div().text_color(t.dim).child("·")))
            .when(paused > 0, |d| d.child(div().text_color(teal).child(format!("{paused} paused"))))
            .tooltip(move |_, cx| cx.new(|_| LoadTip { load: load.clone() }).into())
            .on_click(cx.listener(move |m, _, _, cx| {
                let l = m.load.clone().unwrap_or_default();
                sw::show(Kind::Overloaded, l, backend.clone(), cx);
                m.request_refresh(refresh::SESSIONS, cx);
            }))
            .into_any_element(),
    )
}

/// The status item's hover: who is using the Mac, the CPU bar, and the two shares, the cause first.
struct LoadTip {
    load: SystemLoad,
}

impl Render for LoadTip {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let l = &self.load;
        let ours = midna_proto::system::midna_is_the_cause(l.cpu_percent, l.midna_percent);
        let (verdict, _) = sw::verdict(l);
        let other = l.cpu_percent.saturating_sub(l.midna_percent);
        let stat = |pct: u32, color: Hsla, label: String| {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::BOLD).text_color(color).child(midna_proto::system::share_label(pct as f64)))
                .child(div().text_color(t.dim).child(label))
        };
        let apps = match l.others.first() {
            Some(o) => format!("other apps · mostly {}", o.name),
            None => "other apps".into(),
        };
        let stats = [stat(l.midna_percent, t.work, "Midna's terminals".into()), stat(other, t.fg, apps)];
        let stats = if ours { stats } else { let [a, b] = stats; [b, a] };
        let (dot, line) = match (l.cpu_busy, ours) {
            (true, true) => (t.need, "Agents wait to start heavy work."),
            _ => (t.ok, "Your agents carry on as normal."),
        };
        let paused = l.top.iter().filter(|t| t.paused).count();
        div()
            .w(px(320.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(14.))
            .rounded(px(9.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .text_color(t.fg)
            .text_size(px(12.5))
            .font_family(t.ui_font.clone())
            .child(div().font_weight(FontWeight::BOLD).text_size(px(14.)).child(verdict))
            .child(sw::cpu_bar(&t, l, 10.))
            .child(div().flex().gap(px(10.)).children(stats))
            .child(div().flex().items_center().gap(px(8.)).text_color(t.dim).child(div().size(px(8.)).flex_none().rounded_full().bg(dot)).child(line))
            .when(paused > 0, |d| d.child(div().text_color(super::paused_color(&t)).child(format!("{paused} paused"))))
            .child(div().text_size(px(12.)).text_color(t.dim.opacity(0.7)).child("Click for details"))
    }
}

#[cfg(test)]
mod tests {
    use super::{name_and_count, names, stop_line};
    use ::core::prelude::v1::test;

    #[test]
    fn busiest_names_read_as_a_list() {
        let b = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(name_and_count("rustc ×11"), ("rustc", "×11"));
        assert_eq!(name_and_count("cargo"), ("cargo", ""));
        assert_eq!(names(&b(&["rustc ×11", "zig ×11", "cargo"])), "rustc, zig and cargo");
        assert_eq!(stop_line(&b(&["node ×3"])), "Ends node. The terminal stays open.");
        assert_eq!(stop_line(&[]), "Ends what it started. The terminal stays open.");
    }
}
