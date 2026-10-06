//! In-app banner card (A on the toast + history canvas): while midna is in front, a notification
//! that would be a macOS banner about a terminal you aren't looking at shows here instead, top
//! right, with Go to terminal (⌘J), and Approve / Deny for an approval. It stays its kind's
//! `notify.stay.<kind>` (0: until it's answered, opened or dismissed), fading out as the link
//! preview card does (holding near full strength, then falling away), and stays while the
//! pointer is on it, easing back in if it had started to fade. A long question is cut at a few
//! lines with "Show all". It goes when its needs-you item is answered or you open its terminal.
//! Not in front: the floating badge (`ui/badge.rs`) or macOS banners (`app.rs` `on_notification`).
use crate::app::MainWindow;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::notify::Posted;
use std::time::{Duration, Instant};

const STAY: Duration = Duration::from_secs(8);
/// How long the card takes to come back to full when the pointer reaches it mid-fade.
const RECOVER: Duration = Duration::from_millis(160);
/// How far the card sinks as it fades.
const SINK: f32 = 6.;
/// Lines of a question shown before "Show all".
const CLAMP_LINES: usize = 6;

pub struct Card {
    pub seq: u64,
    pub session: Option<String>,
    pub posted: Posted,
    pub at: Instant,
    /// How long it shows (`notify.stay.<kind>`); None = until it's handled or dismissed.
    stay: Option<Duration>,
    /// When it goes (pushed back while hovered); None = it stays.
    until: Option<Instant>,
    /// Its needs-you item was seen open: when it's gone, so is the card.
    need_seen: bool,
}

#[derive(Default)]
pub struct Cards {
    pub list: Vec<Card>,
    expanded: bool,
    hovered: bool,
    /// Bumped to restart the fade.
    generation: u64,
    /// The pointer reached the card mid-fade: it eases back from this opacity (named by the
    /// generation it cut short).
    recover: Option<(u64, f32)>,
    ticking: bool,
}

/// The card's opacity `x` of the way through its stay: the link preview's fade
/// (`cubic-bezier(.7, 0, .84, 0)`), still 97% at the halfway mark and mostly gone in the last
/// quarter.
fn fade(x: f32) -> f32 {
    1. - super::setup_screen::bezier(0.7, 0., 0.84, 0., x.clamp(0., 1.))
}

/// How long a notification shows (`notify.stay.<kind>`, 0 = until handled or dismissed; a
/// daemon from before it: `STAY`).
pub fn stay_of(p: &Posted) -> Option<Duration> {
    match p.stay_secs {
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s.into())),
        None => Some(STAY),
    }
}

/// Show `p` as a card (newest on top).
pub fn push(m: &mut MainWindow, seq: u64, session: Option<String>, p: Posted, cx: &mut Context<MainWindow>) {
    let now = Instant::now();
    m.cards.list.retain(|c| c.session != session || c.posted.category != p.category);
    let stay = stay_of(&p);
    m.cards.list.insert(0, Card { seq, session, posted: p, at: now, stay, until: stay.map(|s| now + s), need_seen: false });
    m.cards.expanded = false;
    m.cards.generation += 1;
    tick(m, cx);
    cx.notify();
}

/// The card on top, if any: ⌘J goes to its terminal while one shows.
pub fn top_session(m: &MainWindow) -> Option<String> {
    m.cards.list.first().and_then(|c| c.session.clone())
}

/// The top card's open needs-you item, when it has no terminal to go to: "Go" opens its card.
fn top_need(m: &MainWindow) -> Option<String> {
    let c = m.cards.list.first().filter(|c| c.session.as_ref().is_none_or(|s| !m.sessions.iter().any(|x| x.id == *s)))?;
    c.posted.needs_you_id.clone().filter(|id| m.needs.iter().any(|n| n.id == *id))
}

/// The top card has somewhere to go (its terminal, or its needs-you card).
pub fn can_go(m: &MainWindow) -> bool {
    top_need(m).is_some() || top_session(m).is_some()
}

pub fn dismiss_top(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if !m.cards.list.is_empty() {
        m.cards.list.remove(0);
        m.cards.expanded = false;
        m.cards.generation += 1;
        cx.notify();
    }
}

/// Open the top card's terminal, or its needs-you card when it has none (and drop the card).
pub fn go(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(id) = top_need(m) {
        dismiss_top(m, cx);
        return super::needs_you::show(m, id, window, cx);
    }
    let Some(sid) = top_session(m) else { return };
    dismiss_top(m, cx);
    if m.screen != crate::app::Screen::Terminal {
        m.set_screen(crate::app::Screen::Terminal, window, cx);
    }
    cx.defer(move |cx| crate::windows::reveal(sid, cx));
}

/// Drop cards whose time is up, whose item was answered, or whose terminal you opened.
fn prune(m: &mut MainWindow) -> bool {
    let now = Instant::now();
    let hovered = m.cards.hovered;
    let selected = m.selected.clone();
    let needs: Vec<String> = m.needs.iter().map(|n| n.id.clone()).collect();
    let before = m.cards.list.len();
    let top = m.cards.list.first().map(|c| c.seq);
    for c in m.cards.list.iter_mut() {
        if c.posted.needs_you_id.as_ref().is_some_and(|id| needs.contains(id)) {
            c.need_seen = true;
        }
    }
    m.cards.list.retain(|c| {
        let answered = c.need_seen && !c.posted.needs_you_id.as_ref().is_some_and(|id| needs.contains(id));
        let opened = c.session.is_some() && c.session == selected;
        let expired = c.until.is_some_and(|u| now >= u) && !(hovered && Some(c.seq) == top);
        !(answered || opened || expired)
    });
    if m.cards.list.first().map(|c| c.seq) != top {
        m.cards.expanded = false;
        m.cards.generation += 1;
    }
    m.cards.list.len() != before
}

/// While cards show, check twice a second whether any should go.
fn tick(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if m.cards.ticking {
        return;
    }
    m.cards.ticking = true;
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let more = this
                .update(cx, |m, cx| {
                    if prune(m) {
                        cx.notify();
                    }
                    m.cards.ticking = !m.cards.list.is_empty();
                    m.cards.ticking
                })
                .unwrap_or(false);
            if !more {
                break;
            }
        }
    })
    .detach();
}

/// What the card says: a headline and the text under it (a question's whole text, a command).
fn texts(m: &MainWindow, c: &Card) -> (String, Option<String>, Option<String>) {
    let need = c.posted.needs_you_id.as_deref().and_then(|id| m.needs.iter().find(|n| n.id == id));
    if let Some(q) = need.and_then(|n| n.question.as_ref()).filter(|q| !q.text.trim().is_empty()) {
        let head = if q.header.trim().is_empty() { "Asked a question".to_string() } else { q.header.trim().to_string() };
        return (head, Some(q.text.trim().to_string()), None);
    }
    if let Some(n) = need {
        let command = n.approval.as_ref().map(|a| a.action.value.clone()).filter(|v| !v.is_empty());
        let detail = Some(n.detail.clone()).filter(|d| !d.trim().is_empty() && Some(d) != command.as_ref());
        return (capitalize(n.title.trim()), detail, command);
    }
    let mut lines = c.posted.body.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("").to_string();
    let rest: Vec<&str> = lines.collect();
    (first, Some(rest.join("\n")).filter(|r| !r.is_empty()), None)
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let c = m.cards.list.first()?;
    let (label, color) = super::notifications::kind(t, &c.posted);
    let needs = matches!(c.posted.category.as_str(), "approval" | "attention");
    let kind_label = if needs { "Needs you".to_string() } else { label.to_string() };
    let name = super::notifications::source(m, c.session.as_deref(), None);
    let project = c
        .session
        .as_deref()
        .and_then(|s| m.sessions.iter().find(|x| x.id == s))
        .and_then(|s| s.project_id.as_deref())
        .and_then(|p| m.projects.iter().find(|x| x.id == p))
        .map(|p| p.name.clone())
        .filter(|p| *p != name);
    let (headline, detail, command) = texts(m, c);
    let long = detail.as_ref().is_some_and(|d| d.lines().count() > CLAMP_LINES || d.chars().count() > CLAMP_LINES * 60);
    let expanded = m.cards.expanded;
    let more = m.cards.list.len() - 1;
    let need = c.posted.needs_you_id.as_deref().and_then(|id| m.needs.iter().find(|n| n.id == id)).filter(|n| n.approval.is_some()).map(|n| n.id.clone());
    let go_key = m.key_label("keys.next_needs_you");
    let question = c.posted.needs_you_id.as_deref().and_then(|id| m.needs.iter().find(|n| n.id == id)).and_then(|n| n.question.clone());
    let go_label = match () {
        _ if top_need(m).is_some() => "Open",
        _ if c.posted.category == "approval" && need.is_none() => "Answer in the terminal",
        _ => "Go to terminal",
    };
    let generation = m.cards.generation;
    let stay = c.stay;
    let hovered = m.cards.hovered;
    let recover = m.cards.recover;

    let text = detail.map(|d| {
        let shown = if long && !expanded { clamp(&d) } else { d };
        div()
            .relative()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(div().text_size(px(13.5)).line_height(px(20.)).child(shown))
            .when(long, |d| {
                d.child(
                    div()
                        .id("toast-more")
                        .text_size(px(12.5))
                        .text_color(t.accent)
                        .cursor_pointer()
                        .child(if expanded { "Show less".to_string() } else { "Show all".to_string() })
                        .on_click(cx.listener(|m, _, _, cx| {
                            m.cards.expanded = !m.cards.expanded;
                            cx.stop_propagation();
                            cx.notify();
                        })),
                )
            })
    });

    let mut buttons = div().flex().flex_none().gap(px(8.)).px(px(14.)).pt(px(12.)).pb(px(12.)).child(
        div()
            .id("toast-go")
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .h(px(34.))
            .rounded(px(8.))
            .bg(t.accent)
            .text_color(t.accent_fg)
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .child(go_label)
            .child(div().opacity(0.75).text_size(px(11.)).font_family(t.mono_font.clone()).child(go_key))
            .on_click(cx.listener(|m, _, w, cx| go(m, w, cx))),
    );
    if let Some(id) = need {
        let (a, d) = (id.clone(), id);
        buttons = buttons
            .child(super::screen_kit::btn(t, "toast-approve", "Approve").h(px(34.)).on_click(cx.listener(move |m, _, _, cx| {
                m.resolve(a.clone(), Resolution::Approve { scope: ApprovalScope::Once }, cx);
                dismiss_top(m, cx);
            })))
            .child(super::screen_kit::btn(t, "toast-deny", "Deny").h(px(34.)).on_click(cx.listener(move |m, _, _, cx| {
                m.resolve(d.clone(), Resolution::Deny, cx);
                dismiss_top(m, cx);
            })));
    }

    let card = div()
        .id("toast-card")
        .relative()
        .occlude()
        .w(px(420.))
        .max_h(px(600.))
        .flex()
        .flex_col()
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .rounded(px(12.))
        .shadow_lg()
        .overflow_hidden()
        .on_hover(cx.listener(|m, on: &bool, _, cx| {
            m.cards.hovered = *on;
            m.cards.recover = None;
            if *on {
                // Ease back in from however far it had faded.
                if let Some(c) = m.cards.list.first()
                    && let (Some(until), Some(stay)) = (c.until, c.stay)
                {
                    let left = until.saturating_duration_since(Instant::now()).as_secs_f32();
                    let from = fade(1. - left / stay.as_secs_f32());
                    m.cards.recover = (from < 0.99).then_some((m.cards.generation, from));
                }
            } else {
                // Leaving the card starts its time over.
                if let Some(c) = m.cards.list.first_mut() {
                    c.until = c.stay.map(|s| Instant::now() + s);
                }
                m.cards.generation += 1;
            }
            cx.notify();
        }))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(8.))
                .pl(px(14.))
                .pr(px(8.))
                .pt(px(10.))
                .child(div().size(px(8.)).rounded_full().bg(color))
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(kind_label.to_uppercase()))
                .children(project.map(|p| div().text_size(px(12.)).text_color(t.dim).child(format!("{p} ›"))))
                .child(div().font_weight(FontWeight::BOLD).truncate().child(name))
                .child(div().flex_1())
                .child(div().text_size(px(11.5)).text_color(t.dim).child(age(c.at.elapsed())))
                .child(
                    div()
                        .id("toast-dismiss")
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(26.))
                        .rounded(px(6.))
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.panel))
                        .child("✕")
                        .on_click(cx.listener(|m, _, _, cx| dismiss_top(m, cx))),
                ),
        )
        .child(
            div()
                .id("toast-body")
                .flex()
                .flex_col()
                .gap(px(8.))
                .min_h_0()
                .flex_shrink(1.)
                .overflow_y_scroll()
                .px(px(14.))
                .pt(px(4.))
                .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(headline))
                .children(command.map(|cmd| {
                    div().px(px(10.)).py(px(8.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.)).truncate().child(cmd)
                }))
                .children(text)
                .children(question.filter(|q| !q.options.is_empty()).map(|q| options(t, &q))),
        )
        .child(buttons)
        .child(
            div()
                .flex()
                .flex_none()
                .justify_between()
                .px(px(14.))
                .pb(px(8.))
                .text_size(px(11.5))
                .text_color(t.dim)
                .child(match more {
                    0 => String::new(),
                    1 => "+1 more".to_string(),
                    n => format!("+{n} more"),
                })
                .child(if stay.is_some() { "Stays while you hover" } else { "Stays until you're done with it" }),
        );
    // Fading out over its stay, or easing back in from wherever the pointer caught it.
    let at = |el: Stateful<Div>, o: f32| el.opacity(o).top(px(SINK * (1. - o)));
    let reduce = super::queue::reduce_motion();
    let Some(stay) = stay else { return Some(card.into_any_element()) };
    Some(match recover {
        _ if reduce => card.into_any_element(),
        Some((seq, from)) if hovered => {
            let ease = |x: f32| 1. - (1. - x).powi(3);
            card.with_animation(SharedString::from(format!("toast-back-{seq}")), Animation::new(RECOVER).with_easing(ease), move |el, d| at(el, from + (1. - from) * d))
                .into_any_element()
        }
        _ if hovered => card.into_any_element(),
        _ => card.with_animation(SharedString::from(format!("toast-fade-{generation}")), Animation::new(stay), move |el, d| at(el, fade(d))).into_any_element(),
    })
}

/// A question's options, numbered as in the terminal (where you pick one).
fn options(t: &Theme, q: &NeedsYouQuestion) -> impl IntoElement + use<> {
    let mut col = div().flex().flex_col().gap(px(6.)).pt(px(2.));
    if q.multi_select {
        col = col.child(div().text_size(px(11.5)).text_color(t.dim).child("Pick any number of these in the terminal"));
    }
    for (i, o) in q.options.iter().enumerate() {
        col = col.child(
            div()
                .flex()
                .gap(px(8.))
                .px(px(10.))
                .py(px(7.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.panel)
                .child(div().flex_none().w(px(14.)).font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child((i + 1).to_string()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().font_weight(FontWeight::BOLD).child(o.label.clone()))
                        .when(!o.description.trim().is_empty(), |d| d.child(div().text_size(px(12.)).text_color(t.dim).child(o.description.clone()))),
                ),
        );
    }
    col
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// "now", "3m", "1h".
fn age(d: Duration) -> String {
    match d.as_secs() {
        0..=59 => "now".into(),
        s @ 60..=3599 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// `MIDNA_DEBUG_SCREEN=toast` (an approval) / `toast-long` (a long question) in the fake backend.
pub fn debug(m: &mut MainWindow, which: &str, cx: &mut Context<MainWindow>) {
    let (sid, need, body) = match which {
        "toast-long" => ("a1f00005", "n_golden", "Asked a question"),
        _ => ("a1f00002", "n_migrat", "Permission: command · pnpm db:migrate --env staging"),
    };
    let p: Posted = serde_json::from_value(serde_json::json!({ "category": "approval", "title": "", "body": body, "via": "app", "needs_you_id": need, "push": true })).unwrap();
    // Older cards underneath: "+2 more".
    for (i, other) in ["a1f00008", "a1f00003"].iter().enumerate() {
        let q: Posted = serde_json::from_value(serde_json::json!({ "category": "attention", "title": "", "body": "Prompt blocked", "via": "app", "push": true })).unwrap();
        push(m, 10 + i as u64, Some(other.to_string()), q, cx);
    }
    push(m, 1, Some(sid.into()), p, cx);
}

/// The first `CLAMP_LINES` lines (long lines count as several), ending in "…".
fn clamp(text: &str) -> String {
    let mut out = String::new();
    let mut used = 0;
    for line in text.lines() {
        let cost = (line.chars().count() / 60 + 1).max(1);
        if used + cost > CLAMP_LINES {
            let room = (CLAMP_LINES - used) * 60;
            if room > 0 {
                // At a word break, not mid-word.
                let cut: String = line.chars().take(room).collect();
                out.push_str(cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head));
            }
            return out.trim_end().to_string() + "…";
        }
        out.push_str(line);
        out.push('\n');
        used += cost;
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::clamp;

    #[test]
    fn clamp_cuts_at_six_lines_counting_long_ones() {
        assert_eq!(clamp("a\nb"), "a\nb");
        assert_eq!(clamp("1\n2\n3\n4\n5\n6\n7"), "1\n2\n3\n4\n5\n6…");
        assert_eq!(clamp(&format!("1\n2\n3\n4\n5\n{}", "word ".repeat(30))), format!("1\n2\n3\n4\n5\n{}…", "word ".repeat(12).trim_end()));
        let long = "x".repeat(130);
        // 130 chars take 3 lines: 3 more short ones fit, then it's cut.
        assert_eq!(clamp(&format!("{long}\n1\n2\n3\n4")), format!("{long}\n1\n2\n3…"));
    }
}
