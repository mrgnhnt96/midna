//! A close you didn't ask for, on the terminal you're looking at: midnad holds an agent's
//! `session.close` (or `close_on_exit`) and asks first (`session.closing`, see midnad
//! `close_hold.rs`). Looking at it, a card at the bottom right counts down from 5 with Stop,
//! Wait until I leave and Close now; the countdown holds while the pointer is on the card.
//! Not looking, it closes at once. Waiting, it closes as soon as you select another terminal,
//! switch screens or leave midna.
use crate::app::{MainWindow, Screen};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::{Actor, ActorKind};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub const COUNTDOWN: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(50);

pub struct Closing {
    pub id: String,
    by: Option<Actor>,
    how: String,
    /// When it closes; None while waiting for you to leave.
    until: Option<Instant>,
    /// The pointer is on the card: the time that was left when it got there.
    paused: Option<Duration>,
}

impl Closing {
    fn left(&self) -> Duration {
        self.paused.or_else(|| self.until.map(|u| u.saturating_duration_since(Instant::now()))).unwrap_or_default()
    }
}

/// You're looking at `sid`: it's this window's terminal, on the terminal screen, and midna is in
/// front.
fn looking(m: &MainWindow, sid: &str, window: &Window) -> bool {
    window.is_window_active() && m.screen == Screen::Terminal && m.selected.as_deref() == Some(sid) && m.shows(sid)
}

/// The app's answer (`session.close_hold`), off the UI thread.
fn answer(m: &MainWindow, sid: &str, action: &str) {
    let (b, params) = (m.backend.clone(), json!({ "id": sid, "action": action }));
    std::thread::spawn(move || {
        if let Err(e) = b.call("session.close_hold", params) {
            eprintln!("session.close_hold: {e}");
        }
    });
}

/// midnad's `session.closing{id, by, how}` (the home window only).
pub fn on_closing(m: &mut MainWindow, params: &Value, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = params.get("id").and_then(|v| v.as_str()) else { return };
    if !looking(m, sid, window) {
        answer(m, sid, "close");
        return;
    }
    answer(m, sid, "hold");
    m.closing.retain(|c| c.id != sid);
    m.closing.push(Closing {
        id: sid.to_string(),
        by: params.get("by").and_then(|b| serde_json::from_value(b.clone()).ok()),
        how: params.get("how").and_then(|h| h.as_str()).unwrap_or("close").to_string(),
        until: Some(Instant::now() + COUNTDOWN),
        paused: None,
    });
    crate::lifecycle::log(&format!("close of {sid} held: you're looking at it"));
    start_ticking(m, window, cx);
    cx.notify();
}

fn start_ticking(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.closing_ticking {
        return;
    }
    m.closing_ticking = true;
    cx.spawn_in(window, async move |this, cx| {
        loop {
            cx.background_executor().timer(TICK).await;
            let more = this.update_in(cx, |m, window, cx| {
                tick(m, window);
                cx.notify();
                if m.closing.is_empty() {
                    m.closing_ticking = false;
                }
                !m.closing.is_empty()
            });
            if !matches!(more, Ok(true)) {
                break;
            }
        }
    })
    .detach();
}

/// Run out the countdowns, close the waiting ones you left, and drop cards whose terminal went.
fn tick(m: &mut MainWindow, window: &Window) {
    let cards = std::mem::take(&mut m.closing);
    for c in cards {
        if !m.sessions.iter().any(|s| s.id == c.id) {
            continue;
        }
        let due = match c.until {
            Some(_) => c.paused.is_none() && c.left().is_zero(),
            None => !looking(m, &c.id, window),
        };
        if due {
            answer(m, &c.id, "close");
        } else {
            m.closing.push(c);
        }
    }
}

fn act(m: &mut MainWindow, sid: &str, action: &str, cx: &mut Context<MainWindow>) {
    let Some(i) = m.closing.iter().position(|c| c.id == sid) else { return };
    match action {
        "wait" => {
            m.closing[i].until = None;
            m.closing[i].paused = None;
        }
        _ => {
            m.closing.remove(i);
            answer(m, sid, action);
        }
    }
    cx.notify();
}

fn hover(m: &mut MainWindow, sid: &str, on: bool, cx: &mut Context<MainWindow>) {
    let Some(c) = m.closing.iter_mut().find(|c| c.id == sid) else { return };
    if c.until.is_none() {
        return;
    }
    if on {
        c.paused = Some(c.left());
    } else if let Some(left) = c.paused.take() {
        c.until = Some(Instant::now() + left);
    }
    cx.notify();
}

/// Who's closing it, in a few words.
fn why(m: &MainWindow, c: &Closing) -> String {
    if c.how == "exited" {
        return "Its process exited".into();
    }
    let Some(by) = &c.by else { return "Closing it".into() };
    match (&by.kind, by.session.as_deref()) {
        (ActorKind::Agent, Some(s)) if s == c.id => "Its agent is closing it".into(),
        (ActorKind::Agent, Some(s)) => format!("{} is closing it", super::notifications::source(m, Some(s), None)),
        (ActorKind::Agent, None) => "An agent is closing it".into(),
        _ => "Closing it".into(),
    }
}

/// The cards over the terminal pane's bottom-right corner.
pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if m.closing.is_empty() {
        return None;
    }
    let mut col = div().absolute().bottom(px(40.)).right(px(12.)).flex().flex_col().items_end().gap(px(10.));
    for (i, c) in m.closing.iter().enumerate() {
        col = col.child(card(m, t, i, c, cx));
    }
    Some(col.into_any_element())
}

fn card(m: &MainWindow, t: &Theme, i: usize, c: &Closing, cx: &mut Context<MainWindow>) -> AnyElement {
    let name = super::notifications::source(m, Some(&c.id), None);
    let waiting = c.until.is_none();
    let left = c.left();
    let secs = left.as_secs_f32().ceil() as u32;
    let k = (left.as_secs_f32() / COUNTDOWN.as_secs_f32()).clamp(0., 1.);
    let color = if waiting { t.accent } else { t.need };
    let (stop, wait, now) = (c.id.clone(), c.id.clone(), c.id.clone());
    let hov = c.id.clone();
    let line = if waiting { "Closes when you leave this terminal".to_string() } else { why(m, c) };
    let head = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
        .child(div().flex_none().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(if waiting { "WAITING" } else { "CLOSING" }))
        .child(div().min_w_0().font_weight(FontWeight::BOLD).truncate().child(name))
        .child(div().flex_1())
        .when(!waiting, |d| {
            d.child(div().flex_none().text_size(px(20.)).font_weight(FontWeight::BOLD).font_family(t.mono_font.clone()).text_color(color).child(secs.to_string()))
        });
    let buttons = div()
        .flex()
        .gap(px(8.))
        .child(super::screen_kit::btn_primary(t, ("close-hold-stop", i), "Stop").h(px(32.)).flex_1().justify_center().on_click(cx.listener(
            move |m, _, _, cx| act(m, &stop, "keep", cx),
        )))
        .when(!waiting, |d| {
            d.child(super::screen_kit::btn(t, ("close-hold-wait", i), "Wait until I leave").h(px(32.)).flex_1().justify_center().on_click(cx.listener(
                move |m, _, _, cx| act(m, &wait, "wait", cx),
            )))
        })
        .child(super::screen_kit::btn(t, ("close-hold-now", i), "Close now").h(px(32.)).flex_1().justify_center().on_click(cx.listener(
            move |m, _, _, cx| act(m, &now, "close", cx),
        )));
    div()
        .id(("close-hold", i))
        .occlude()
        .w(px(380.))
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(14.))
        .pt(px(10.))
        .pb(px(12.))
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .rounded(px(12.))
        .shadow_lg()
        .overflow_hidden()
        .on_hover(cx.listener(move |m, on: &bool, _, cx| hover(m, &hov, *on, cx)))
        .child(head)
        .child(div().text_size(px(13.)).text_color(t.dim).child(line))
        .when(!waiting, |d| d.child(div().h(px(3.)).w_full().rounded_full().bg(t.panel).child(div().h_full().w(relative(k)).rounded_full().bg(color))))
        .child(buttons)
        .into_any_element()
}

/// `MIDNA_DEBUG_SCREEN=close-hold` (counting down) or `close-hold-wait`.
pub fn debug(m: &mut MainWindow, which: &str, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone().or_else(|| m.sessions.first().map(|s| s.id.clone())) else { return };
    m.closing.push(Closing {
        id: sid,
        by: Some(Actor::agent(None)),
        how: "close".into(),
        until: (which != "close-hold-wait").then(Instant::now),
        // Held as if under the pointer, so the snapshot shows it mid-countdown.
        paused: (which != "close-hold-wait").then(|| Duration::from_millis(3400)),
    });
    cx.notify();
}
