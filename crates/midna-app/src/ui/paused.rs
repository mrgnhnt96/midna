//! A busy Mac and paused terminals in the main window ("Overload dialog concepts" canvas):
//! - the card over a paused terminal (concept 3): what it was running, Resume, and Stop
//!   processes with a confirm;
//! - the status bar item while the Mac is overloaded or anything is paused (concept i):
//!   "● CPU 474% · 2 paused", its hover the terminals' share of the CPU as one bar (h3), a
//!   click opening the overload window;
//! - `system.load`, followed while either shows.
use crate::app::{MainWindow, refresh};
use crate::icons::Icon;
use crate::model::Session;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_danger, btn_primary};
use crate::ui::system_window::{self as sw, Kind};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{HeavyTerminal, SystemLoad};
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
    let now = m.load.as_ref().map(|l| format!("The Mac is at CPU {}% now.", l.load_percent));
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

/// "● CPU 474% · 2 paused" while the Mac is overloaded or anything is paused.
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
            .when_some(over, |d, l| d.child(div().text_color(t.status_color("orange")).child("●")).child(format!("CPU {}%", l.load_percent)))
            .when(over.is_some() && paused > 0, |d| d.child(div().text_color(t.dim).child("·")))
            .when(paused > 0, |d| d.child(div().text_color(teal).child(format!("{paused} paused"))))
            .tooltip(move |_, cx| cx.new(|_| LoadTip { top: load.top.clone(), cpus: load.cpus }).into())
            .on_click(cx.listener(move |m, _, _, cx| {
                let l = m.load.clone().unwrap_or_default();
                sw::show(Kind::Overloaded, l, backend.clone(), cx);
                m.request_refresh(refresh::SESSIONS, cx);
            }))
            .into_any_element(),
    )
}

/// The status item's hover: who is using the CPU, as one bar split by terminal, with a legend.
struct LoadTip {
    top: Vec<HeavyTerminal>,
    cpus: u32,
}

/// Each row's color: paused ones in teal, the rest in blue, fading down the list.
fn shade(t: &Theme, term: &HeavyTerminal, nth: usize) -> Hsla {
    let base = if term.paused { super::paused_color(t) } else { t.work };
    base.opacity([1., 0.6, 0.38, 0.25][nth.min(3)])
}

impl Render for LoadTip {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let capacity = (self.cpus.max(1) * 100) as f32;
        let (mut np, mut nr) = (0, 0);
        let colors: Vec<Hsla> = self
            .top
            .iter()
            .map(|term| {
                let n = if term.paused { &mut np } else { &mut nr };
                *n += 1;
                shade(&t, term, *n - 1)
            })
            .collect();
        let bar = div().flex().gap(px(2.)).h(px(8.)).w_full().rounded(px(4.)).overflow_hidden().bg(t.line).children(self.top.iter().zip(&colors).map(|(term, c)| {
            div().h_full().bg(*c).w(relative((term.cpu_percent as f32 / capacity).clamp(0.01, 1.)))
        }));
        let rows = self.top.iter().zip(&colors).map(|(term, c)| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().size(px(8.)).flex_none().rounded(px(2.)).bg(*c))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(term.name.clone()))
                .child(div().flex_none().text_color(t.dim).child(format!("{}%", term.cpu_percent)))
        });
        div()
            .w(px(280.))
            .flex()
            .flex_col()
            .gap(px(9.))
            .p(px(12.))
            .rounded(px(9.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .text_color(t.fg)
            .text_size(px(12.5))
            .font_family(t.ui_font.clone())
            .when(self.top.is_empty(), |d| d.child(div().text_color(t.dim).child("Nothing in Midna is using much CPU.")))
            .when(!self.top.is_empty(), |d| d.child(bar).children(rows))
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
