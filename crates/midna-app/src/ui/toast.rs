//! In-app banner card (A on the toast + history canvas): while midna is in front, a notification
//! that would be a macOS banner about a terminal you aren't looking at shows here instead, top
//! right, with Go to terminal (⌘J), and Approve / Deny for an approval. It stays its kind's
//! `notify.stay.<kind>` (0: until it's answered, opened or dismissed), fading out as the link
//! preview card does (holding near full strength, then falling away), and stays while the
//! pointer is on it, easing back in if it had started to fade. A long question is cut at a few
//! lines with "Show all". Clicking its header folds it to that one row, see-through until the
//! pointer is on it (it stays, unfaded, until you click it open again); folded, its age and ✕
//! show only under the pointer.
//!
//! More than one waiting (design A, "Peeking stack", on the stacked notifications canvas): the
//! older cards peek out under the top one as up to two edges. Clicking an edge, "+N more" or a
//! folded card's "+N" fans the pile out into a list of folded rows (the top card folds first);
//! clicking a row slides the list back into the pile with that one on top, open. It goes when its needs-you item is answered or you open its terminal.
//! Going any way but running out its time (opened, answered, Go, ✕), the top card fades and sinks
//! away as it last looked, over whichever card is now on top.
//! A `notify.send` card may open a URL instead (Open), show its buttons, be replaced by a later
//! one with the same id, or be withdrawn; what you do with it is reported (`notify.respond`).
//! Not in front: the floating badge (`ui/badge.rs`) or macOS banners (`app.rs` `on_notification`).
use crate::app::MainWindow;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::notify::{Posted, Response, ResponseKind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

const STAY: Duration = Duration::from_secs(8);
/// How long the card takes to come back to full when the pointer reaches it mid-fade.
const RECOVER: Duration = Duration::from_millis(160);
/// How far the card sinks as it fades.
const SINK: f32 = 6.;
/// How long the card takes to fold to its header, or open back up.
const FOLD: Duration = Duration::from_millis(220);
/// The room under a folded card's header.
const FOLDED_PAD: f32 = 10.;
/// How strong a folded card shows while the pointer isn't on it.
const FOLDED_OPACITY: f32 = 0.6;
/// How long the pile takes to fan out into the list, or gather back.
const LIST: Duration = Duration::from_millis(320);
/// Each row of the list starts this far (of the way through) after the one above it.
const STAGGER: f32 = 0.12;
/// A folded row's height: its header (26), the padding around it and the border.
const ROW_H: f32 = 48.;
/// The gap between the list's rows.
const ROW_GAP: f32 = 6.;
/// How far each older card peeks out under the one in front, and how much narrower it is a side.
const PEEK: f32 = 6.;
const PEEK_INSET: f32 = 10.;
/// The height under a card's header before it has painted open once (what it opens to).
const REST_GUESS: f32 = 360.;
/// How long a card takes to fade and sink away as it goes.
const LEAVE: Duration = Duration::from_millis(180);
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
    /// Its needs-you item as last seen open: when it's gone, so is the card (which goes saying
    /// what it asked).
    need: Option<NeedsYou>,
    /// Folded to its header (clicked there): it stays until it's opened, answered or dismissed.
    collapsed: bool,
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
    /// The top card is folding or opening: its seq, a count naming the animation, and when.
    fold: Option<(u64, u64, Instant)>,
    folds: u64,
    /// The height under each card's header (by seq), measured as it paints open (what it folds
    /// from).
    rest_h: Rc<RefCell<HashMap<u64, f32>>>,
    /// The pile is fanned out into the list.
    listing: bool,
    /// The list is fanning out (`listing`) or gathering back: a count naming the animation, and
    /// when it starts (fanning out waits for the top card to fold).
    list_anim: Option<(u64, Instant)>,
    /// The pointer has left the card since it folded: only then does pointing at it bring a
    /// folded card back to full (so it's seen dimming as it folds under the pointer).
    armed: bool,
    /// How strong the card and its age and ✕ showed when it last started to fold or open.
    fold_from: (f32, f32),
    /// The folded card came up to full (true) or went back down, a count naming it, and when.
    lift: Option<(bool, u64, Instant)>,
    lifts: u64,
    /// Cards on their way out, newest last.
    leaving: Vec<Leaving>,
}

/// A card on its way out, drawn as it last looked, fading and sinking away from how strong it
/// showed.
struct Leaving {
    card: Card,
    look: Look,
    expanded: bool,
    full: bool,
    from: f32,
    at: Instant,
}

/// What a card says and offers, from its needs-you item.
#[derive(Clone)]
struct Look {
    headline: String,
    detail: Option<String>,
    command: Option<String>,
    question: Option<NeedsYouQuestion>,
    /// Its open approval: Approve / Deny show.
    approval: Option<String>,
    go_label: &'static str,
}

/// The card's opacity `x` of the way through its stay: the link preview's fade
/// (`cubic-bezier(.7, 0, .84, 0)`), still 97% at the halfway mark and mostly gone in the last
/// quarter.
fn fade(x: f32) -> f32 {
    1. - super::setup_screen::bezier(0.7, 0., 0.84, 0., x.clamp(0., 1.))
}

/// How long a notification shows (`notify.stay.<kind>`, 0 = until handled or dismissed; a
/// daemon from before it: `STAY`). One with buttons stays until handled.
pub fn stay_of(p: &Posted) -> Option<Duration> {
    if !p.actions.is_empty() {
        return None;
    }
    match p.stay_secs {
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s.into())),
        None => Some(STAY),
    }
}

/// Show `p` as a card (newest on top).
pub fn push(m: &mut MainWindow, seq: u64, session: Option<String>, p: Posted, cx: &mut Context<MainWindow>) {
    let now = Instant::now();
    match p.id.as_deref() {
        // A `notify.send` id replaces only its own card.
        Some(id) => m.cards.list.retain(|c| c.posted.id.as_deref() != Some(id)),
        None => m.cards.list.retain(|c| c.session != session || c.posted.category != p.category),
    }
    let stay = stay_of(&p);
    m.cards.list.insert(0, Card { seq, session, posted: p, at: now, stay, until: stay.map(|s| now + s), need: None, collapsed: false });
    m.cards.expanded = false;
    m.cards.generation += 1;
    m.cards.listing = false;
    m.cards.list_anim = None;
    tick(m, cx);
    cx.notify();
}

/// The top card's terminal, if it has one that's still open: ⌘J goes there while one shows.
pub fn top_session(m: &MainWindow) -> Option<String> {
    m.cards.list.first().and_then(|c| c.session.clone()).filter(|s| m.sessions.iter().any(|x| x.id == *s))
}

/// The top card's open needs-you item, when it has no terminal to go to: "Go" opens its card.
fn top_need(m: &MainWindow) -> Option<String> {
    let c = m.cards.list.first().filter(|_| top_session(m).is_none())?;
    c.posted.needs_you_id.clone().filter(|id| m.needs.iter().any(|n| n.id == *id))
}

/// A card shows: ⌘J is its button (Go to terminal, Open, or Dismiss when it has neither).
pub fn can_go(m: &MainWindow) -> bool {
    !m.cards.list.is_empty()
}

/// The top card's URL to open (`notify.send` `open`).
fn top_open(m: &MainWindow) -> Option<String> {
    m.cards.list.first().and_then(|c| c.posted.open.clone())
}

/// Report what you did with card `seq`, if it came from `notify.send`.
fn respond(m: &MainWindow, seq: u64, kind: ResponseKind, action: Option<String>) {
    if let Some(id) = m.cards.list.iter().find(|c| c.seq == seq).and_then(|c| c.posted.id.clone()) {
        crate::notify::respond(&m.backend, &id, Response { kind, action });
    }
}

/// Take away the card for `notify.send` id `id` (`notify.withdraw`).
pub fn withdraw(m: &mut MainWindow, id: &str, cx: &mut Context<MainWindow>) {
    while let Some(seq) = m.cards.list.iter().find(|c| c.posted.id.as_deref() == Some(id)).map(|c| c.seq) {
        dismiss(m, seq, cx);
    }
}

pub fn dismiss_top(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if let Some(seq) = m.cards.list.first().map(|c| c.seq) {
        dismiss(m, seq, cx);
    }
}

/// Drop one card (its ✕).
fn dismiss(m: &mut MainWindow, seq: u64, cx: &mut Context<MainWindow>) {
    let Some(i) = m.cards.list.iter().position(|c| c.seq == seq) else { return };
    remove(m, i, true, cx);
    if i == 0 {
        m.cards.expanded = false;
        m.cards.generation += 1;
    }
    end_list_if_alone(m);
    cx.notify();
}

/// Drop card `i`. On top and `leave`, it fades and sinks away as it last looked (not in the
/// list, where rows just go).
fn remove(m: &mut MainWindow, i: usize, leave: bool, cx: &mut Context<MainWindow>) {
    let leave = leave && i == 0 && !m.cards.listing && !super::queue::reduce_motion();
    let last = leave.then(|| (look(m, &m.cards.list[0], true), shown(m, &m.cards.list[0])));
    let card = m.cards.list.remove(i);
    if let Some((look, from)) = last {
        let now = Instant::now();
        m.cards.leaving.retain(|l| now < l.at + LEAVE);
        m.cards.leaving.push(Leaving { card, look, expanded: m.cards.expanded, full: folded_full(m), from, at: now });
        notify_at(now + LEAVE, cx);
    }
}

/// How strong the top card `c` shows right now.
fn shown(m: &MainWindow, c: &Card) -> f32 {
    if c.collapsed {
        return if folded_full(m) { 1. } else { FOLDED_OPACITY };
    }
    match (c.until, c.stay) {
        (Some(until), Some(stay)) if !m.cards.hovered => fade(1. - until.saturating_duration_since(Instant::now()).as_secs_f32() / stay.as_secs_f32()),
        _ => 1.,
    }
}

/// One card left (or none): there's no list to show.
fn end_list_if_alone(m: &mut MainWindow) {
    if m.cards.list.len() < 2 {
        m.cards.listing = false;
        m.cards.list_anim = None;
    }
}

/// Render again at `at` (an animation's end, where what's drawn changes).
fn notify_at(at: Instant, cx: &mut Context<MainWindow>) {
    let wait = at.saturating_duration_since(Instant::now()) + Duration::from_millis(16);
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(wait).await;
        this.update(cx, |_, cx| cx.notify()).ok();
    })
    .detach();
}

/// Fan the pile out into the list of folded rows, folding the top card first if it's open.
fn open_list(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if m.cards.list.len() < 2 || m.cards.listing {
        return;
    }
    let reduce = super::queue::reduce_motion();
    let now = Instant::now();
    let mut start = now;
    if let Some(c) = m.cards.list.first_mut()
        && !c.collapsed
    {
        c.collapsed = true;
        m.cards.fold_from = (1., 1.);
        m.cards.armed = false;
        if !reduce {
            m.cards.folds += 1;
            m.cards.fold = Some((c.seq, m.cards.folds, now));
            start = now + FOLD;
        }
    }
    m.cards.listing = true;
    m.cards.folds += 1;
    m.cards.list_anim = (!reduce).then_some((m.cards.folds, start));
    m.cards.expanded = false;
    notify_at(start, cx);
    cx.notify();
}

/// Gather the list back into the pile with `seq` on top, opening as the pile closes.
fn pick(m: &mut MainWindow, seq: u64, cx: &mut Context<MainWindow>) {
    let Some(i) = m.cards.list.iter().position(|c| c.seq == seq) else { return };
    let reduce = super::queue::reduce_motion();
    let now = Instant::now();
    let opens = if reduce { now } else { now + LIST };
    let mut c = m.cards.list.remove(i);
    c.collapsed = false;
    c.until = c.stay.map(|s| opens + s);
    m.cards.list.insert(0, c);
    m.cards.listing = false;
    m.cards.list_anim = None;
    m.cards.fold = None;
    m.cards.fold_from = (1., 1.);
    if !reduce {
        m.cards.folds += 1;
        m.cards.list_anim = Some((m.cards.folds, now));
        m.cards.folds += 1;
        m.cards.fold = Some((seq, m.cards.folds, opens));
    }
    m.cards.expanded = false;
    m.cards.recover = None;
    m.cards.generation += 1;
    notify_at(opens, cx);
    cx.notify();
}

/// Fold the top card to its header, or open it back up (its time starting over).
fn toggle_collapsed(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let full = folded_full(m);
    let Some(c) = m.cards.list.first_mut() else { return };
    m.cards.fold_from = match () {
        _ if !c.collapsed => (1., 1.),
        _ if full => (1., 1.),
        _ => (FOLDED_OPACITY, 0.),
    };
    m.cards.armed = false;
    m.cards.lift = None;
    c.collapsed = !c.collapsed;
    m.cards.folds += 1;
    m.cards.fold = Some((c.seq, m.cards.folds, Instant::now()));
    if !c.collapsed {
        c.until = c.stay.map(|s| Instant::now() + s);
    }
    m.cards.expanded = false;
    m.cards.recover = None;
    m.cards.generation += 1;
    cx.notify();
}

/// Open the top card's terminal, or its needs-you card when it has none, and drop the card. With
/// neither (sent from outside a terminal, or its terminal closed), it only drops it: otherwise a
/// kind that stays until handled could never go but by its ✕.
pub fn go(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let (url, need, sid) = (top_open(m), top_need(m), top_session(m));
    if let Some(seq) = m.cards.list.first().map(|c| c.seq) {
        let goes = url.is_some() || need.is_some() || sid.is_some();
        respond(m, seq, if goes { ResponseKind::Clicked } else { ResponseKind::Dismissed }, None);
    }
    dismiss_top(m, cx);
    if let Some(url) = url {
        return crate::notify::open_url(&url);
    }
    if let Some(id) = need {
        return super::needs_you::show(m, id, window, cx);
    }
    let Some(sid) = sid else { return };
    if m.screen != crate::app::Screen::Terminal {
        m.set_screen(crate::app::Screen::Terminal, window, cx);
    }
    cx.defer(move |cx| crate::windows::reveal(sid, cx));
}

/// Drop the cards that should go now (after a refresh, or a terminal opened), not on the next
/// tick.
pub fn sync(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    if prune(m, cx) {
        cx.notify();
    }
}

/// Drop cards whose time is up, whose item was answered, or whose terminal you opened.
fn prune(m: &mut MainWindow, cx: &mut Context<MainWindow>) -> bool {
    let now = Instant::now();
    let before = m.cards.list.len();
    let top = m.cards.list.first().map(|c| c.seq);
    for c in m.cards.list.iter_mut() {
        if let Some(n) = c.posted.needs_you_id.as_ref().and_then(|id| m.needs.iter().find(|n| n.id == *id)) {
            c.need = Some(n.clone());
        }
    }
    let mut i = 0;
    while i < m.cards.list.len() {
        match gone(m, i, now) {
            Some(leave) => remove(m, i, leave, cx),
            None => i += 1,
        }
    }
    end_list_if_alone(m);
    if m.cards.list.first().map(|c| c.seq) != top {
        m.cards.expanded = false;
        m.cards.generation += 1;
    }
    m.cards.list.len() != before
}

/// Card `i` should go: Some(true) answered or opened (it fades away), Some(false) its time is up
/// (it has faded already).
fn gone(m: &MainWindow, i: usize, now: Instant) -> Option<bool> {
    let c = &m.cards.list[i];
    let answered = c.need.is_some() && !c.posted.needs_you_id.as_ref().is_some_and(|id| m.needs.iter().any(|n| n.id == *id));
    let opened = c.session.is_some() && c.session == m.selected;
    let expired = c.until.is_some_and(|u| now >= u) && !c.collapsed && !m.cards.listing && !(m.cards.hovered && i == 0);
    match () {
        _ if answered || opened => Some(true),
        _ if expired => Some(false),
        _ => None,
    }
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
                    if prune(m, cx) {
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

/// What the top card `c` says and offers. `last`: from its needs-you item as last seen when it's
/// no longer open (it's leaving, answered).
fn look(m: &MainWindow, c: &Card, last: bool) -> Look {
    let open = c.posted.needs_you_id.as_deref().and_then(|id| m.needs.iter().find(|n| n.id == id));
    let need = open.or(c.need.as_ref().filter(|_| last));
    let (headline, detail, command) = texts(c, need);
    let approval = need.filter(|n| n.approval.is_some()).map(|n| n.id.clone());
    let go_label = match () {
        _ if top_open(m).is_some() || top_need(m).is_some() => "Open",
        _ if top_session(m).is_none() => "Dismiss",
        _ if c.posted.category == "approval" && approval.is_none() => "Answer in the terminal",
        _ => "Go to terminal",
    };
    Look { headline, detail, command, question: need.and_then(|n| n.question.clone()), approval, go_label }
}

/// What the card says: a headline and the text under it (a question's whole text, a command).
fn texts(c: &Card, need: Option<&NeedsYou>) -> (String, Option<String>, Option<String>) {
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

/// What a header row shows at its end.
enum Meta {
    Show,
    /// Kept in place but unseen (a folded card the pointer isn't on).
    Hide,
    /// Seen while the pointer is on its list row.
    RowHover,
    /// Easing from one strength to another (named, over a time).
    Ease(SharedString, Duration, f32, f32),
}

/// A card's header row: its kind and terminal, "+N" (`count`, opening the list) and its age and ✕.
fn head(m: &MainWindow, t: &Theme, c: &Card, meta: Meta, count: Option<usize>, cx: &mut Context<MainWindow>) -> Div {
    let (label, color) = super::notifications::kind(t, &c.posted);
    let needs = matches!(c.posted.category.as_str(), "approval" | "attention");
    let kind_label = if needs { "Needs you".to_string() } else { label.to_string() };
    let name = super::notifications::source(m, c.session.as_deref(), None);
    let seq = c.seq;
    let end = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(div().text_size(px(11.5)).text_color(t.dim).child(age(c.at.elapsed())))
        .child(
            div()
                .id(("toast-dismiss", seq as usize))
                .flex()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(px(6.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.bg(t.panel))
                .child("✕")
                .on_click(cx.listener(move |m, _, _, cx| {
                    cx.stop_propagation();
                    respond(m, seq, ResponseKind::Dismissed, None);
                    dismiss(m, seq, cx);
                })),
        );
    let end = match meta {
        Meta::Show => end.into_any_element(),
        Meta::Hide => end.opacity(0.).into_any_element(),
        Meta::RowHover => end.opacity(0.).group_hover("toast-row", |s| s.opacity(1.)).into_any_element(),
        Meta::Ease(name, dur, from, to) => {
            end.with_animation(name, Animation::new(dur).with_easing(ease_out), move |el, d| el.opacity(from + (to - from) * d)).into_any_element()
        }
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .w_full()
        .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
        .child(div().flex_none().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(kind_label.to_uppercase()))
        .child(div().min_w_0().font_weight(FontWeight::BOLD).truncate().child(name))
        .child(div().flex_1())
        .children(count.map(|n| {
            div()
                .id(("toast-count", seq as usize))
                .flex_none()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(6.))
                .text_size(px(11.5))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.bg(t.panel).text_color(t.fg))
                .child(format!("+{n}"))
                .on_click(cx.listener(|m, _, _, cx| {
                    cx.stop_propagation();
                    open_list(m, cx);
                }))
        }))
        .child(end)
}

/// A folded card shows at full (and its age and ✕): the pointer came back onto it after it folded.
fn folded_full(m: &MainWindow) -> bool {
    m.cards.hovered && m.cards.armed
}

fn ease_out(x: f32) -> f32 {
    1. - (1. - x).powi(3)
}

/// The pointer reached or left the card (or the list).
fn hover(m: &mut MainWindow, on: bool, cx: &mut Context<MainWindow>) {
    let was = folded_full(m);
    m.cards.hovered = on;
    if !on {
        m.cards.armed = true;
    }
    if folded_full(m) != was {
        m.cards.lifts += 1;
        m.cards.lift = Some((!was, m.cards.lifts, Instant::now()));
    }
    m.cards.recover = None;
    if on {
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
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let now = Instant::now();
    let leaving: Vec<&Leaving> = m.cards.leaving.iter().filter(|l| now < l.at + LEAVE).collect();
    if m.cards.list.is_empty() && leaving.is_empty() {
        return None;
    }
    let reduce = super::queue::reduce_motion();
    let front = (!m.cards.list.is_empty()).then(|| match m.cards.list_anim {
        // Fanning out (once the top card has folded), or gathering back.
        Some((n, at)) if m.cards.listing && now >= at => list(m, t, Some((n, true)), cx),
        Some((n, at)) if !m.cards.listing && now < at + LIST => list(m, t, Some((n, false)), cx),
        None if m.cards.listing && reduce => list(m, t, None, cx),
        _ => stack(m, t, cx),
    });
    // Cards on their way out, over the one now on top.
    let leaving = leaving.into_iter().map(|l| div().absolute().top_0().left_0().child(card_el(m, t, &l.card, &l.look, Some(l), cx)));
    Some(div().relative().w(px(420.)).children(front).children(leaving).into_any_element())
}

/// Where list row `i` (of `n`) sits `k` of the way from the pile to the list: its top, how far
/// in its sides are, and how strong it shows. In the pile, the two behind the top peek out under
/// it and the rest hide behind them.
fn place(i: usize, k: f32, n: usize) -> (f32, f32, f32) {
    let span = 1. + STAGGER * (n.min(6) - 1) as f32;
    let x = (k * span - STAGGER * i.min(5) as f32).clamp(0., 1.);
    let e = 1. - (1. - x).powi(3);
    let back = i.min(2) as f32;
    let top = PEEK * back * (1. - e) + i as f32 * (ROW_H + ROW_GAP) * e;
    let from = if i > 2 { 0. } else { 1. };
    (top, PEEK_INSET * back * (1. - e), from + (FOLDED_OPACITY - from) * e)
}

/// The list's height `k` of the way out: down to its lowest row.
fn list_height(k: f32, n: usize) -> f32 {
    (0..n).map(|i| place(i, k, n).0 + ROW_H).fold(ROW_H, f32::max)
}

/// The pile fanned out into folded rows, newest on top: clicking one gathers it back with that
/// one open. `anim`: (animation name, fanning out), or None to show it out, still.
fn list(m: &MainWindow, t: &Theme, anim: Option<(u64, bool)>, cx: &mut Context<MainWindow>) -> AnyElement {
    let n = m.cards.list.len();
    let k_of = move |d: f32| match anim {
        Some((_, true)) => d,
        Some((_, false)) => 1. - d,
        None => 1.,
    };
    let mut col = div().id("toast-list").relative().occlude().w(px(420.)).on_hover(cx.listener(|m, on: &bool, _, cx| hover(m, *on, cx)));
    // Painted bottom up, so the newest is in front while they're piled.
    for (i, c) in m.cards.list.iter().enumerate().rev() {
        let seq = c.seq;
        let row = div()
            .id(("toast-row", seq as usize))
            .group("toast-row")
            .absolute()
            .h(px(ROW_H))
            .flex()
            .items_center()
            .pl(px(14.))
            .pr(px(8.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .rounded(px(12.))
            .shadow_lg()
            .cursor_pointer()
            .hover(|s| s.opacity(1.))
            .child(head(m, t, c, Meta::RowHover, None, cx))
            .on_click(cx.listener(move |m, _, _, cx| pick(m, seq, cx)));
        let at = move |el: Stateful<Div>, k: f32| {
            let (top, inset, o) = place(i, k, n);
            el.top(px(top)).left(px(inset)).right(px(inset)).opacity(o)
        };
        col = col.child(match anim {
            Some((name, _)) => row
                .with_animation(SharedString::from(format!("toast-row-{name}-{i}")), Animation::new(LIST), move |el, d| at(el, k_of(d)))
                .into_any_element(),
            None => at(row, 1.).into_any_element(),
        });
    }
    match anim {
        Some((name, _)) => col
            .with_animation(SharedString::from(format!("toast-list-{name}")), Animation::new(LIST), move |el, d| el.h(px(list_height(k_of(d), n))))
            .into_any_element(),
        None => col.h(px(list_height(1., n))).into_any_element(),
    }
}

/// The top card, the older ones peeking out under it.
fn stack(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> AnyElement {
    let c = &m.cards.list[0];
    card_el(m, t, c, &look(m, c, false), None, cx)
}

/// Card `c` in front, the older ones peeking out under it; or, `ghost`, a card on its way out,
/// drawn as it last looked (deaf to the pointer), fading and sinking away.
fn card_el(m: &MainWindow, t: &Theme, c: &Card, look: &Look, ghost: Option<&Leaving>, cx: &mut Context<MainWindow>) -> AnyElement {
    let Look { headline, detail, command, question, approval: need, go_label } = look.clone();
    let live = ghost.is_none();
    let long = detail.as_ref().is_some_and(|d| d.lines().count() > CLAMP_LINES || d.chars().count() > CLAMP_LINES * 60);
    let expanded = ghost.map_or(m.cards.expanded, |g| g.expanded);
    let more = if live { m.cards.list.len() - 1 } else { 0 };
    let go_key = m.key_label("keys.next_needs_you");
    let generation = m.cards.generation;
    let stay = c.stay;
    let collapsed = c.collapsed;
    let reduce = super::queue::reduce_motion();
    let peeks = more.min(2);
    let full = ghost.map_or(folded_full(m), |g| g.full);
    // A folded card's strength and its age and ✕: easing as it folds or opens, or as the pointer
    // comes and goes; otherwise still.
    let lift = m.cards.lift.filter(|(_, _, at)| live && collapsed && at.elapsed() < RECOVER && !reduce);
    let (from, meta_from) = m.cards.fold_from;
    // Folding or opening right now: (animation name, folding).
    let folding = m.cards.fold.filter(|(seq, _, at)| live && *seq == c.seq && at.elapsed() < FOLD && !reduce).map(|(_, n, _)| (n, collapsed));
    let hovered = live && m.cards.hovered;
    let recover = m.cards.recover.filter(|_| live);
    // Leaving: from how strong it showed (and how far it had sunk) to gone.
    let leave = ghost.map(|g| (g.from, if collapsed { 0. } else { SINK * (1. - g.from) }));
    let seq = c.seq;
    let out = move |pile: Stateful<Div>, (from, sunk): (f32, f32)| -> AnyElement {
        pile.with_animation(SharedString::from(format!("toast-leave-{seq}")), Animation::new(LEAVE).with_easing(ease_out), move |el, d| {
            el.opacity(from * (1. - d)).top(px(sunk + SINK * d))
        })
        .into_any_element()
    };

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
                        .when(live, |d| d.on_click(cx.listener(|m, _, _, cx| {
                            m.cards.expanded = !m.cards.expanded;
                            cx.stop_propagation();
                            cx.notify();
                        }))),
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
            .when(live, |d| d.on_click(cx.listener(|m, _, w, cx| go(m, w, cx)))),
    );
    // A `notify.send` card's own buttons, a row of their own (they report back, then it goes).
    let actions = (!c.posted.actions.is_empty()).then(|| {
        let mut row = div().flex().flex_none().flex_wrap().gap(px(8.)).px(px(14.)).pt(px(12.));
        for (i, label) in c.posted.actions.iter().enumerate() {
            let (picked, seq) = (label.clone(), c.seq);
            row = row.child(super::screen_kit::btn(t, ("toast-action", i), label.clone()).h(px(34.)).when(live, |b| {
                b.on_click(cx.listener(move |m, _, _, cx| {
                    respond(m, seq, ResponseKind::Action, Some(picked.clone()));
                    dismiss(m, seq, cx);
                }))
            }));
        }
        row
    });
    if let Some(id) = need {
        let (a, d) = (id.clone(), id);
        buttons = buttons
            .child(super::screen_kit::btn(t, "toast-approve", "Approve").h(px(34.)).when(live, |b| {
                b.on_click(cx.listener(move |m, _, _, cx| {
                    dismiss_top(m, cx);
                    m.resolve(a.clone(), Resolution::Approve { scope: ApprovalScope::Once }, cx);
                }))
            }))
            .child(super::screen_kit::btn(t, "toast-deny", "Deny").h(px(34.)).when(live, |b| {
                b.on_click(cx.listener(move |m, _, _, cx| {
                    dismiss_top(m, cx);
                    m.resolve(d.clone(), Resolution::Deny, cx);
                }))
            }));
    }

    let meta = match (folding, lift) {
        (Some((n, true)), _) => Meta::Ease(format!("toast-meta-fold-{n}").into(), FOLD, meta_from, 0.),
        (Some((n, false)), _) => Meta::Ease(format!("toast-meta-fold-{n}").into(), FOLD, meta_from, 1.),
        (None, Some((up, n, _))) => Meta::Ease(format!("toast-meta-lift-{n}").into(), RECOVER, if up { 0. } else { 1. }, if up { 1. } else { 0. }),
        (None, None) if collapsed && !full => Meta::Hide,
        (None, None) => Meta::Show,
    };
    let dim = move |pile: Stateful<Div>| -> AnyElement {
        match (folding, lift) {
            (Some((n, folds)), _) => {
                let to = if folds { FOLDED_OPACITY } else { 1. };
                pile.with_animation(SharedString::from(format!("toast-dim-{n}")), Animation::new(FOLD).with_easing(ease_out), move |el, d| el.opacity(from + (to - from) * d))
                    .into_any_element()
            }
            (None, Some((up, n, _))) => {
                let (a, b) = if up { (FOLDED_OPACITY, 1.) } else { (1., FOLDED_OPACITY) };
                pile.with_animation(SharedString::from(format!("toast-lift-{n}")), Animation::new(RECOVER).with_easing(ease_out), move |el, d| el.opacity(a + (b - a) * d))
                    .into_any_element()
            }
            (None, None) => pile.opacity(if full { 1. } else { FOLDED_OPACITY }).into_any_element(),
        }
    };
    let card = div()
        .id("toast-card")
        .relative()
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
        .child(
            head(m, t, c, meta, (collapsed && more > 0).then_some(more), cx)
                .id("toast-head")
                .flex_none()
                .pl(px(14.))
                .pr(px(8.))
                .pt(px(10.))
                .when(collapsed && folding.is_none(), |d| d.pb(px(FOLDED_PAD)))
                .cursor_pointer()
                .when(live, |d| d.on_click(cx.listener(|m, _, _, cx| toggle_collapsed(m, cx)))),
        );
    // The older cards' edges under it (open the list), then the card in front.
    let mut pile = div()
        .id("toast-stack")
        .relative()
        .w(px(420.))
        .pb(px(PEEK * peeks as f32))
        .when(live, |d| d.occlude().on_hover(cx.listener(|m, on: &bool, _, cx| hover(m, *on, cx))));
    for j in (1..=peeks).rev() {
        let inset = PEEK_INSET * j as f32;
        pile = pile.child(
            div()
                .id(("toast-edge", j))
                .absolute()
                .left(px(inset))
                .right(px(inset))
                .bottom(px(PEEK * (peeks - j) as f32))
                .h(px(ROW_H))
                .bg(t.raised)
                .border_1()
                .border_color(t.line)
                .rounded(px(12.))
                .shadow_lg()
                .cursor_pointer()
                .on_click(cx.listener(|m, _, _, cx| open_list(m, cx))),
        );
    }
    if collapsed && folding.is_none() {
        return match leave {
            Some(l) => out(pile.child(card), l),
            None => dim(pile.child(card)),
        };
    }
    let rest = div()
        .id("toast-rest")
        .relative()
        .flex()
        .flex_col()
        .min_h_0()
        .flex_shrink(1.)
        .overflow_hidden()
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
                .child(match c.posted.image.clone() {
                    // Its kind's image, beside the headline.
                    Some(path) => div()
                        .flex()
                        .items_start()
                        .gap(px(12.))
                        .child(div().flex_1().min_w_0().text_size(px(14.)).font_weight(FontWeight::BOLD).child(headline))
                        .child(super::badge::thumb(path.into(), 48., 9.)),
                    None => div().text_size(px(14.)).font_weight(FontWeight::BOLD).child(headline),
                })
                .children(command.map(|cmd| {
                    div().px(px(10.)).py(px(8.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.)).truncate().child(cmd)
                }))
                .children(text)
                .children(question.filter(|q| !q.options.is_empty()).map(|q| options(t, &q, c.session.clone().filter(|_| live), cx))),
        )
        .children(actions)
        .child(buttons)
        .when(more > 0, |d| {
            d.child(
                div().flex_none().px(px(14.)).pb(px(8.)).child(
                    div()
                        .id("toast-others")
                        .text_size(px(11.5))
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover(|s| s.text_color(t.fg))
                        .child(format!("+{more} more"))
                        .on_click(cx.listener(|m, _, _, cx| open_list(m, cx))),
                ),
            )
        });
    // Under the header: measured while it's open, its height and strength easing while it folds.
    let card = match folding {
        Some((n, folding)) => {
            let full = m.cards.rest_h.borrow().get(&c.seq).copied().unwrap_or(REST_GUESS);
            let ease = |x: f32| 1. - (1. - x).powi(3);
            let rest = rest.with_animation(SharedString::from(format!("toast-fold-{n}")), Animation::new(FOLD).with_easing(ease), move |el, d| {
                let k = if folding { 1. - d } else { d };
                // It ends as the folded header's padding.
                el.flex_none().max_h(px(FOLDED_PAD + (full - FOLDED_PAD).max(0.) * k)).opacity(k)
            });
            card.child(rest)
        }
        None => {
            let heights = m.cards.rest_h.clone();
            let seq = c.seq;
            let measure = move |b: Bounds<Pixels>, _: &mut Window, _: &mut App| {
                heights.borrow_mut().insert(seq, f32::from(b.size.height));
            };
            card.child(rest.child(canvas(measure, |_, _, _, _| {}).absolute().top_0().left_0().size_full()))
        }
    };
    let pile = pile.child(card);
    if let Some(l) = leave {
        return out(pile, l);
    }
    if collapsed || folding.is_some() {
        return dim(pile);
    }
    // Fading out over its stay, or easing back in from wherever the pointer caught it.
    let at = |el: Stateful<Div>, o: f32| el.opacity(o).top(px(SINK * (1. - o)));
    let Some(stay) = stay else { return pile.into_any_element() };
    match recover {
        _ if reduce => pile.into_any_element(),
        Some((seq, from)) if hovered => {
            let ease = |x: f32| 1. - (1. - x).powi(3);
            pile.with_animation(SharedString::from(format!("toast-back-{seq}")), Animation::new(RECOVER).with_easing(ease), move |el, d| at(el, from + (1. - from) * d))
                .into_any_element()
        }
        _ if hovered => pile.into_any_element(),
        _ => pile.with_animation(SharedString::from(format!("toast-fade-{generation}")), Animation::new(stay), move |el, d| at(el, fade(d))).into_any_element(),
    }
}

/// A question's options, numbered as in the terminal. With its terminal (`session`), clicking
/// one types its number there, as picking it in the terminal does.
fn options(t: &Theme, q: &NeedsYouQuestion, session: Option<String>, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mut col = div().flex().flex_col().gap(px(6.)).pt(px(2.));
    let multi = q.multi_select;
    if multi {
        let hint = if session.is_some() { "Pick any number of these, then submit in the terminal" } else { "Pick any number of these in the terminal" };
        col = col.child(div().text_size(px(11.5)).text_color(t.dim).child(hint));
    }
    for (i, o) in q.options.iter().enumerate() {
        let row = div()
            .id(("toast-option", i))
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
            );
        col = col.child(match session.clone() {
            Some(sid) => row.cursor_pointer().hover(|s| s.border_color(t.accent)).on_click(cx.listener(move |m, _, _, cx| {
                cx.stop_propagation();
                choose(m, &sid, i + 1, multi, cx);
            })),
            None => row,
        });
    }
    col
}

/// Pick option `n` of a question in terminal `sid` by typing its number. One pick answers it, so
/// the card goes; with any number to pick, it stays for the rest.
fn choose(m: &mut MainWindow, sid: &str, n: usize, multi: bool, cx: &mut Context<MainWindow>) {
    m.rpc("session.input", serde_json::json!({ "id": sid, "text": n.to_string(), "enter": false }), cx, |_, _, _, _| {});
    if !multi {
        dismiss_top(m, cx);
    }
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
