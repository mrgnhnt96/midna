//! Needs-you inbox: every waiting item in a list (oldest first, filterable by kind), the
//! selected one in full beside it, keyboard driven. Replaces the terminal pane (the sidebar
//! stays). Data is `MainWindow::needs`
//! (`needs_you.list`, refreshed on `needs_you.*` events); every action is `needs_you.resolve`.
use super::border_w;
use crate::app::{MainWindow, Overlay, Screen};
use crate::commands;
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Approved,
    Denied,
    Dismissed,
}

/// The list's kind chips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Approvals,
    Blocked,
    Failed,
    Other,
}

const FILTERS: [(Filter, &str); 5] = [(Filter::All, "All"), (Filter::Approvals, "Approvals"), (Filter::Blocked, "Blocked"), (Filter::Failed, "Failed"), (Filter::Other, "Other")];

fn filter_of(n: &NeedsYou) -> Filter {
    match n.kind {
        NeedsYouKind::Blocked | NeedsYouKind::SecretNeeded => Filter::Blocked,
        NeedsYouKind::Failed => Filter::Failed,
        _ if approves(n) => Filter::Approvals,
        _ => Filter::Other,
    }
}

pub struct Stack {
    pub focus: FocusHandle,
    /// The selected item. When it disappears (resolved here or elsewhere) the item now at
    /// `pos` takes its place.
    cur: Option<String>,
    pos: usize,
    pub menu: bool,
    filter: Filter,
    /// The item a clicked notification opened ("from your notification").
    clicked: Option<String>,
    scroll: ScrollHandle,
    /// Handled during this pass, for the "All clear" summary.
    log: Vec<(String, Outcome)>,
    /// Live screen tails for items without a `screen_excerpt` (None while loading).
    excerpts: HashMap<String, Option<Vec<String>>>,
    /// Resolved here but maybe still in a `needs_you.list` fetched before the resolve
    /// landed; hidden for a few seconds (the daemon's list catches up well within that).
    gone: HashMap<String, std::time::Instant>,
}

impl Stack {
    pub fn new(cx: &mut App) -> Stack {
        Stack {
            focus: cx.focus_handle(),
            cur: None,
            pos: 0,
            menu: false,
            filter: Filter::All,
            clicked: None,
            scroll: ScrollHandle::new(),
            log: vec![],
            excerpts: HashMap::new(),
            gone: HashMap::new(),
        }
    }
}

/// Called when the stack opens: a new pass.
pub fn on_open(m: &mut MainWindow) {
    let s = &mut m.stack;
    s.cur = None;
    s.pos = 0;
    s.menu = false;
    s.filter = Filter::All;
    s.clicked = None;
    s.log.clear();
    s.excerpts.clear();
    s.gone.clear();
    // dev: screenshot the approve dropdown
    s.menu = crate::dev::var("MIDNA_DEBUG_APPROVE_MENU").is_ok();
    if s.menu {
        s.cur = m.needs.iter().find(|n| n.is_approval()).map(|n| n.id.clone());
    }
}

/// Every item still waiting, oldest first.
fn waiting(m: &MainWindow) -> Vec<&NeedsYou> {
    let mut v: Vec<&NeedsYou> = m.needs.iter().filter(|n| !m.stack.gone.contains_key(&n.id)).collect();
    v.sort_by_key(|n| parse_rfc3339(&n.created_at).unwrap_or(i64::MAX));
    v
}

/// The list as shown: `waiting`, through the kind filter.
fn ordered(m: &MainWindow) -> Vec<&NeedsYou> {
    let mut v = waiting(m);
    if m.stack.filter != Filter::All {
        v.retain(|n| filter_of(n) == m.stack.filter);
    }
    v
}

fn current_index(m: &MainWindow, order: &[&NeedsYou]) -> Option<usize> {
    if order.is_empty() {
        return None;
    }
    if let Some(i) = m.stack.cur.as_ref().and_then(|id| order.iter().position(|n| &n.id == id)) {
        return Some(i);
    }
    Some(if m.stack.pos < order.len() { m.stack.pos } else { 0 })
}

fn current(m: &MainWindow) -> Option<NeedsYou> {
    let order = ordered(m);
    current_index(m, &order).map(|i| order[i].clone())
}

struct Meta {
    label: &'static str,
    color: Hsla,
    approve: bool,
}

fn meta(t: &Theme, n: &NeedsYou) -> Meta {
    let (label, color) = match n.kind {
        NeedsYouKind::Approval => ("Policy approval", t.need),
        NeedsYouKind::PermissionPrompt => ("Permission prompt", t.need),
        NeedsYouKind::TriggerWaiting => ("Webhook session", t.accent),
        NeedsYouKind::Blocked => ("Blocked", t.need),
        NeedsYouKind::Note => ("Note", t.work),
        NeedsYouKind::Failed => ("Failed exit", t.err),
        NeedsYouKind::RuleRemoval => ("Rule removal", t.need),
        NeedsYouKind::SecretNeeded => ("Secret needed", t.accent),
        NeedsYouKind::Other => ("Needs you", t.need),
    };
    Meta { label, color, approve: approves(n) }
}

/// Answered with approve / deny.
fn approves(n: &NeedsYou) -> bool {
    matches!(n.kind, NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt | NeedsYouKind::TriggerWaiting) || (n.approval.is_some() && n.kind == NeedsYouKind::Other)
}

/// The two buttons a non-approval card offers: (primary label, resolution or None for
/// "open Triggers"), (secondary label, resolution).
fn other_actions(n: &NeedsYou) -> (Option<(&'static str, Option<Resolution>)>, Option<(&'static str, Resolution)>) {
    match n.kind {
        NeedsYouKind::Blocked => (Some(("I’ve done it", Some(Resolution::Done))), Some(("Dismiss", Resolution::Dismiss))),
        NeedsYouKind::Failed => (Some(("Restart", Some(Resolution::Restart))), Some(("Dismiss", Resolution::Dismiss))),
        NeedsYouKind::Note => (Some(("Got it", Some(Resolution::Dismiss))), None),
        NeedsYouKind::RuleRemoval => (Some(("Remove rule", Some(Resolution::Approve { scope: ApprovalScope::Once }))), Some(("Keep rule", Resolution::Deny))),
        NeedsYouKind::SecretNeeded => (Some(("Open Triggers", None)), Some(("Dismiss", Resolution::Dismiss))),
        _ => (None, Some(("Dismiss", Resolution::Dismiss))),
    }
}

fn session_of<'a>(m: &'a MainWindow, n: &NeedsYou) -> Option<&'a Session> {
    n.session_id.as_ref().and_then(|sid| m.sessions.iter().find(|s| &s.id == sid))
}

fn project_name(m: &MainWindow, pid: Option<&str>) -> String {
    pid.and_then(|p| m.projects.iter().find(|x| x.id == p)).map(|p| p.name.clone()).unwrap_or_else(|| "root".into())
}

/// Its terminal's name (the project is in the list row), else its project.
fn terminal_of(m: &MainWindow, n: &NeedsYou) -> String {
    match session_of(m, n) {
        Some(s) => match crate::windows::name_note(m, s) {
            Some(w) => format!("{} ({w})", s.name),
            None => s.name.clone(),
        },
        None => project_name(m, n.project_id.as_deref()),
    }
}

fn where_of(m: &MainWindow, n: &NeedsYou) -> String {
    match session_of(m, n) {
        Some(s) => match crate::windows::name_note(m, s) {
            Some(w) => format!("{} › {} ({w})", project_name(m, s.project_id.as_deref()), s.name),
            None => format!("{} › {}", project_name(m, s.project_id.as_deref()), s.name),
        },
        None => project_name(m, n.project_id.as_deref()),
    }
}

/// Items that can be approved together with `n`: approvals that are all `bulk_safe` and
/// ask for the same action.
fn same_group<'a>(m: &'a MainWindow, n: &NeedsYou) -> Vec<&'a NeedsYou> {
    let key = |x: &NeedsYou| match &x.approval {
        Some(a) if !a.action.value.is_empty() => format!("{}:{}", a.action.kind, a.action.value),
        _ => format!("{:?}:{}:{}", x.kind, x.title, x.detail),
    };
    if !n.bulk_safe || !n.is_approval() {
        return vec![];
    }
    let k = key(n);
    m.needs.iter().filter(|x| x.bulk_safe && x.is_approval() && key(x) == k).collect()
}

// ------------------------------------------------------------------ actions

fn resolve_cur(m: &mut MainWindow, res: Resolution, cx: &mut Context<MainWindow>) {
    let Some(n) = current(m) else { return };
    let outcome = match res {
        Resolution::Approve { .. } | Resolution::Done | Resolution::Restart => Outcome::Approved,
        Resolution::Deny => Outcome::Denied,
        _ => Outcome::Dismissed,
    };
    let order = ordered(m);
    m.stack.pos = current_index(m, &order).unwrap_or(0);
    m.stack.cur = None;
    m.stack.menu = false;
    m.stack.log.push((n.title.clone(), outcome));
    m.stack.gone.insert(n.id.clone(), std::time::Instant::now());
    m.resolve(n.id, res, cx);
}

fn primary(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(n) = current(m) else { return };
    let t = cx.global::<Theme>().clone();
    if meta(&t, &n).approve {
        return resolve_cur(m, Resolution::Approve { scope: ApprovalScope::Once }, cx);
    }
    match other_actions(&n).0 {
        Some((_, Some(res))) => resolve_cur(m, res, cx),
        Some((_, None)) => {
            m.set_overlay(Overlay::None, window, cx);
            if m.screen != Screen::Triggers {
                m.set_screen(Screen::Triggers, window, cx);
            }
        }
        None => {}
    }
}

fn secondary(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(n) = current(m) else { return };
    let t = cx.global::<Theme>().clone();
    if meta(&t, &n).approve {
        return resolve_cur(m, Resolution::Deny, cx);
    }
    if let Some((_, res)) = other_actions(&n).1 {
        resolve_cur(m, res, cx);
    }
}

fn approve_all(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(n) = current(m) else { return };
    let group: Vec<(String, String)> = same_group(m, &n).into_iter().map(|x| (x.id.clone(), x.title.clone())).collect();
    if group.len() < 2 {
        return;
    }
    let order = ordered(m);
    m.stack.pos = current_index(m, &order).unwrap_or(0);
    m.stack.cur = None;
    m.stack.menu = false;
    for (id, title) in group {
        m.stack.log.push((title, Outcome::Approved));
        m.stack.gone.insert(id.clone(), std::time::Instant::now());
        m.resolve(id, Resolution::Approve { scope: ApprovalScope::Once }, cx);
    }
}

/// ⌘J inside the list: next item (wraps).
pub fn skip(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let len = ordered(m).len();
    if let Some(i) = current_index(m, &ordered(m)) {
        select_at(m, (i + 1) % len, cx);
    }
}

/// ↑ / ↓: the item above or below (stops at the ends).
fn step(m: &mut MainWindow, down: bool, cx: &mut Context<MainWindow>) {
    let len = ordered(m).len();
    if let Some(i) = current_index(m, &ordered(m)) {
        select_at(m, if down { (i + 1).min(len - 1) } else { i.saturating_sub(1) }, cx);
    }
}

fn select_at(m: &mut MainWindow, i: usize, cx: &mut Context<MainWindow>) {
    let Some(id) = ordered(m).get(i).map(|n| n.id.clone()) else { return };
    m.stack.cur = Some(id);
    m.stack.pos = i;
    m.stack.menu = false;
    m.stack.scroll.scroll_to_item(i);
    cx.notify();
}

/// Open the list on item `id` (a clicked notification), keeping it open if it already is.
pub fn show(m: &mut MainWindow, id: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.overlay != Overlay::NeedsYou {
        m.set_overlay(Overlay::NeedsYou, window, cx);
    }
    m.stack.filter = Filter::All;
    m.stack.clicked = Some(id.clone());
    let i = ordered(m).iter().position(|n| n.id == id);
    match i {
        Some(i) => select_at(m, i, cx),
        None => go_to(m, id, cx),
    }
}

/// More than a card shows: its request is longer than one line, or its question takes more
/// screen than the card's last `TAIL` lines. A notification click opens the terminal instead.
pub fn too_long(n: &NeedsYou) -> bool {
    let request = n.approval.as_ref().map(|a| a.action.value.as_str()).unwrap_or("");
    let excerpt = n.screen_excerpt.as_ref().map(|e| e.iter().filter(|l| !l.trim().is_empty()).count()).unwrap_or(0);
    request.contains('\n') || request.chars().count() > 160 || n.detail.chars().count() > 280 || n.detail.lines().count() > 4 || excerpt > TAIL
}

fn go_to(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.stack.cur = Some(id);
    m.stack.menu = false;
    cx.notify();
}

/// ⌘O: leave the list and open the item's terminal.
fn open_terminal(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = current(m).and_then(|n| n.session_id) else {
        return;
    };
    m.stack.cur = None;
    m.set_overlay(Overlay::None, window, cx);
    if m.sessions.iter().any(|s| s.id == sid) {
        m.select(sid, window, cx);
    }
}

fn back(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.overlay == Overlay::NeedsYou {
        m.set_overlay(Overlay::None, window, cx);
    }
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let md = &ks.modifiers;
    if !md.platform && !md.shift && !md.alt && !md.control && matches!(ks.key.as_str(), "up" | "down") {
        if !m.stack.menu {
            step(m, ks.key == "down", cx);
        }
        cx.stop_propagation();
        return;
    }
    if !md.platform {
        return;
    }
    match ks.key.as_str() {
        "o" => open_terminal(m, window, cx),
        "enter" if ks.modifiers.shift => approve_all(m, cx),
        _ => return,
    }
    cx.stop_propagation();
}

/// Fetch the live screen tail for the selected item when the item carries no excerpt.
fn ensure_excerpt(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(n) = current(m) else { return };
    if n.screen_excerpt.as_ref().is_some_and(|e| !e.is_empty()) || m.stack.excerpts.contains_key(&n.id) {
        return;
    }
    let Some(sid) = n.session_id.clone() else {
        return;
    };
    m.stack.excerpts.insert(n.id.clone(), None);
    let backend = m.backend.clone();
    let nid = n.id.clone();
    cx.spawn(async move |this, cx| {
        let res = cx.background_executor().spawn(async move { backend.call("session.read", json!({"id": sid, "lines": 12})) }).await;
        let lines = res.ok().and_then(|v| v.get("text").and_then(|t| t.as_str()).map(tail)).unwrap_or_default();
        let _ = this.update(cx, |m, cx| {
            m.stack.excerpts.insert(nid, Some(lines));
            cx.notify();
        });
    })
    .detach();
}

/// How many lines of screen a card shows.
const TAIL: usize = 8;

/// The last `TAIL` lines, without trailing blanks.
fn tail(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(TAIL);
    lines.split_off(skip)
}

// ------------------------------------------------------------------ view

fn kbd(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().font_family(t.mono_font.clone()).text_size(px(11.)).child(s.into())
}

pub fn render(m: &mut MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    m.stack.gone.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(5));
    // The last item of a kind went: back to all of them.
    if m.stack.filter != Filter::All && ordered(m).is_empty() {
        m.stack.filter = Filter::All;
    }
    ensure_excerpt(m, cx);
    let m = &*m;
    let order = ordered(m);
    let cur_i = current_index(m, &order);
    let back_name = m.selected_session().map(|s| s.name.clone()).unwrap_or_else(|| "terminal".into());
    let approve_key = m.key_label("keys.approve");
    let deny_key = m.key_label("keys.deny");

    let body: AnyElement = match cur_i {
        None => empty_state(m, t, &back_name, cx).into_any_element(),
        Some(i) => {
            let n = order[i].clone();
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .child(list(m, t, &order, i, &approve_key, &deny_key, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(div().id("ny-detail").flex_1().min_h_0().overflow_y_scroll().px(px(32.)).pt(px(26.)).pb(px(16.)).child(card(m, t, &n, cx)))
                        .child(div().flex_none().px(px(32.)).pt(px(12.)).pb(px(24.)).child(actions(m, t, &n, &approve_key, &deny_key, cx))),
                )
                .into_any_element()
        }
    };

    div()
        .id("needs-you")
        .key_context("MidnaOverlay")
        .track_focus(&m.stack.focus)
        .on_key_down(cx.listener(on_key))
        .on_action(cx.listener(|m, _: &crate::actions::ApproveOnce, w, cx| primary(m, w, cx)))
        .on_action(cx.listener(|m, _: &crate::actions::Deny, _w, cx| secondary(m, cx)))
        .on_action(cx.listener(|m, _: &crate::actions::NextNeedsYou, _w, cx| skip(m, cx)))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| {
            if m.stack.menu {
                m.stack.menu = false;
                cx.notify();
            } else {
                back(m, w, cx);
            }
        }))
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .bg(t.bg)
        .child(body)
}

/// The left column: title and count, kind chips, one row per item, key hints.
fn list(m: &MainWindow, t: &Theme, order: &[&NeedsYou], cur: usize, approve_key: &str, deny_key: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let all = waiting(m);
    let mut chips = div().flex().flex_none().flex_wrap().gap(px(6.)).px(px(18.)).pb(px(12.));
    for (f, label) in FILTERS {
        let count = if f == Filter::All { all.len() } else { all.iter().filter(|n| filter_of(n) == f).count() };
        if f != Filter::All && count == 0 {
            continue;
        }
        let on = m.stack.filter == f;
        chips = chips.child(
            div()
                .id(SharedString::from(format!("ny-filter-{label}")))
                .flex()
                .items_center()
                .h(px(26.))
                .px(px(10.))
                .rounded(px(13.))
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |d| d.bg(t.accent_soft))
                .text_color(if on { t.fg } else { t.dim })
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.stack.filter = f;
                    m.stack.cur = None;
                    m.stack.pos = 0;
                    m.stack.menu = false;
                    cx.notify();
                }))
                .child(if f == Filter::All { label.to_string() } else { format!("{label} {count}") }),
        );
    }

    let mut rows = div().id("ny-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&m.stack.scroll).flex().flex_col();
    for (i, n) in order.iter().enumerate() {
        let mt = meta(t, n);
        let on = i == cur;
        let id = n.id.clone();
        rows = rows.child(
            div()
                .id(("ny-row", i))
                .flex()
                .flex_none()
                .items_center()
                .gap(px(12.))
                .px(px(16.))
                .py(px(9.))
                .border_l_2()
                .border_color(if on { t.accent } else { transparent_black() })
                .when(on, |d| d.bg(t.raised))
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, _, _, cx| {
                    let i = ordered(m).iter().position(|n| n.id == id).unwrap_or(0);
                    select_at(m, i, cx);
                }))
                .child(div().size(px(8.)).flex_none().rounded_full().bg(mt.color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).child(n.title.clone()))
                                .child(div().flex_none().font_family(t.mono_font.clone()).text_size(px(11.)).text_color(t.dim).child(since_short(&n.created_at))),
                        )
                        .child(div().truncate().text_size(px(12.)).text_color(t.dim).child(format!("{} · {}", mt.label, where_of(m, n)))),
                ),
        );
    }

    div()
        .w(px(360.))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .border_r_1()
        .border_color(t.line)
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(10.))
                .pl(px(18.))
                .pr(px(10.))
                .pt(px(14.))
                .pb(px(12.))
                .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).child("Needs you"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(20.))
                        .min_w(px(20.))
                        .px(px(6.))
                        .rounded(px(10.))
                        .bg(t.need.opacity(0.16))
                        .text_color(t.need)
                        .font_family(t.mono_font.clone())
                        .text_size(px(11.))
                        .child(all.len().to_string()),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .id("ny-back")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(28.))
                        .px(px(8.))
                        .rounded(px(7.))
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.raised))
                        .on_click(cx.listener(|m, _, w, cx| back(m, w, cx)))
                        .child("Back")
                        .child(kbd(t, "esc")),
                ),
        )
        .child(chips)
        .child(rows)
        .child(
            div()
                .flex()
                .flex_none()
                .flex_wrap()
                .gap_x(px(14.))
                .gap_y(px(4.))
                .px(px(18.))
                .py(px(10.))
                .border_t_1()
                .border_color(t.line)
                .text_size(px(11.5))
                .text_color(t.dim)
                .child(hint(t, "↑↓", "move"))
                .child(hint(t, approve_key, "approve"))
                .child(hint(t, deny_key, "deny"))
                .child(hint(t, "⌘O", "terminal")),
        )
}

fn hint(t: &Theme, key: &str, label: &str) -> Div {
    div().flex().gap(px(5.)).child(kbd(t, key.to_string()).text_color(t.fg)).child(label.to_string())
}

/// The selected item in full: kind, terminal, title, why, the request, its screen.
fn card(m: &MainWindow, t: &Theme, n: &NeedsYou, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mt = meta(t, n);
    let sess = session_of(m, n);
    let who = commands::who(n, sess);
    let where_ = terminal_of(m, n);
    let request = n
        .approval
        .as_ref()
        .map(|a| a.action.value.clone())
        .filter(|v| !v.is_empty())
        .or_else(|| (n.kind == NeedsYouKind::Failed).then(|| sess.map(|s| s.command.join(" ")).unwrap_or_default()).filter(|v| !v.is_empty()));
    let why = if !n.detail.is_empty() {
        n.detail.clone()
    } else if let Some(r) = n.approval.as_ref().and_then(|a| a.matched_rule.clone()) {
        format!("Rule {r} asks for this")
    } else {
        match n.kind {
            NeedsYouKind::PermissionPrompt => "The agent's own permission prompt".into(),
            NeedsYouKind::Approval => "No rule allows this yet".into(),
            NeedsYouKind::Blocked => "Raised with midna attention".into(),
            NeedsYouKind::Note => "Raised with midna attention --note".into(),
            NeedsYouKind::Failed => sess.and_then(|s| s.status.exit_code).map(|c| format!("Exited with code {c}")).unwrap_or_else(|| "Non-zero exit".into()),
            _ => String::new(),
        }
    };
    let mut grid = div().flex().flex_col().gap(px(6.)).text_size(px(13.));
    let grow = |label: &str, value: AnyElement| {
        div().flex().gap(px(12.)).child(div().w(px(74.)).flex_none().text_color(t.dim).child(label.to_string())).child(div().flex_1().min_w_0().child(value))
    };
    grid = grid.child(grow("Asked by", div().child(who.clone()).into_any_element()));
    if !why.is_empty() {
        grid = grid.child(grow("Why", div().child(why).into_any_element()));
    }
    if let Some(r) = request.clone() {
        grid = grid.child(grow("Request", div().truncate().font_family(t.mono_font.clone()).text_size(px(12.5)).child(r).into_any_element()));
    }

    // screen excerpt: the item's own, else the live tail of its terminal
    let excerpt: Option<Vec<String>> =
        n.screen_excerpt.clone().filter(|e| !e.is_empty()).or_else(|| m.stack.excerpts.get(&n.id).cloned().flatten()).filter(|e| e.iter().any(|l| !l.trim().is_empty()));
    let loading = excerpt.is_none() && m.stack.excerpts.get(&n.id).is_some_and(|e| e.is_none());
    let peek = (excerpt.is_some() || loading).then(|| {
        let lines = excerpt.clone().unwrap_or_default();
        let mut body = div().flex().flex_col().px(px(12.)).py(px(8.)).font_family(t.mono_font.clone()).text_size(px(12.)).line_height(px(12. * 1.6)).overflow_hidden();
        if loading {
            body = body.child(div().text_color(t.dim).child("Reading the terminal…"));
        }
        for l in &lines {
            let color = if l.contains("error") || l.contains("panicked") || l.starts_with("exit ") {
                t.err
            } else if l.trim_start().starts_with('⏺') {
                t.accent
            } else {
                t.fg
            };
            body = body.child(div().whitespace_nowrap().overflow_hidden().text_color(color).child(if l.is_empty() { " ".to_string() } else { l.clone() }));
        }
        div()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(t.line)
            .bg(t.term)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(t.line)
                    .text_size(px(11.5))
                    .text_color(t.dim)
                    .child(div().flex_1().truncate().child(if lines.len() == 1 { format!("{where_} · last line") } else { format!("{where_} · last {} lines", lines.len()) }))
            )
            .child(body)
    });

    // approve all N once
    let group = same_group(m, n);
    let bulk = (group.len() >= 2).then(|| {
        let others: Vec<String> = group.iter().filter(|x| x.id != n.id).map(|x| where_of(m, x)).collect();
        border_w(div(), 1.)
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(9.))
            .rounded(px(9.))
            .border_color(t.accent)
            .border_dashed()
            .bg(t.accent_soft)
            .child(div().flex_1().min_w(px(240.)).child(format!("Same request from {}", others.join(", "))))
            .child(
                div()
                    .id("approve-all")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(28.))
                    .px(px(11.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.accent)
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.accent_soft))
                    .on_click(cx.listener(|m, _, _, cx| approve_all(m, cx)))
                    .child(format!("Approve all {} once", group.len()))
                    .child(kbd(t, "⌘⇧↩")),
            )
    });

    let long = too_long(n).then(|| {
        div().flex().items_center().gap(px(8.)).text_size(px(12.)).text_color(t.dim).child(Icon::PopOut.el(12., t.dim)).child("This one's longer than fits here: answer it in its terminal (⌘O).")
    });

    let open = sess.is_some().then(|| {
        div()
            .id("ny-open")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(32.))
            .px(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .cursor_pointer()
            .hover(|s| s.bg(t.line))
            .on_click(cx.listener(|m, _, w, cx| open_terminal(m, w, cx)))
            .child(Icon::PopOut.el(13., t.fg))
            .child("Open terminal")
            .child(kbd(t, "⌘O").text_color(t.dim))
    });
    let clicked = m.stack.clicked.as_deref() == Some(n.id.as_str());
    div()
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(
            div()
                .flex()
                .items_start()
                .gap(px(12.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(10.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .h(px(22.))
                                        .px(px(9.))
                                        .rounded(px(11.))
                                        .bg(mt.color.opacity(0.14))
                                        .text_color(mt.color)
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(mt.label),
                                )
                                .child(div().text_color(t.dim).child(where_.clone())),
                        )
                        .when(clicked, |d| d.child(div().text_size(px(11.)).text_color(t.dim.opacity(0.7)).child("from your notification"))),
                )
                .child(div().flex_1())
                .children(open),
        )
        .child(div().text_size(px(22.)).line_height(px(28.)).font_weight(FontWeight::BOLD).child(n.title.clone()))
        .child(grid)
        .children(long)
        .children(peek)
        .children(bulk)
}

/// The selected item's buttons, along the bottom of the detail column.
fn actions(m: &MainWindow, t: &Theme, n: &NeedsYou, approve_key: &str, deny_key: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mt = meta(t, n);
    let sess = session_of(m, n);
    let mut actions = div().flex().flex_wrap().items_center().gap(px(8.));
    if mt.approve {
        let start = n.kind == NeedsYouKind::TriggerWaiting;
        let session_name = sess.map(|s| s.name.clone()).unwrap_or_else(|| "it".into());
        let scope_hint = if start { "this trigger" } else { "this command" };
        // A folder-trust dialog (midnad's trust.rs) has no scopes: trusting saves the folder.
        let trust = n.kind == NeedsYouKind::PermissionPrompt && n.title.starts_with("Trust this folder?");
        let options: Vec<(&str, String, ApprovalScope)> = vec![
            ("Approve once", approve_key.to_string(), ApprovalScope::Once),
            ("Approve for 15 minutes", scope_hint.into(), ApprovalScope::Minutes { minutes: 15 }),
            ("Approve for 1 hour", scope_hint.into(), ApprovalScope::Minutes { minutes: 60 }),
            ("Approve for this session", format!("until {session_name} exits"), ApprovalScope::Session),
            ("Always approve", "adds a rule".into(), ApprovalScope::Always),
        ];
        let menu = m.stack.menu.then(|| {
            let mut b = super::sidebar::menu_box(t).min_w(px(270.));
            for (i, (label, hint, scope)) in options.into_iter().enumerate() {
                b = b.child(super::sidebar::menu_item(
                    t,
                    &format!("ny-approve-opt-{i}"),
                    label,
                    &hint,
                    cx.listener(move |m, _, _, cx| resolve_cur(m, Resolution::Approve { scope: scope.clone() }, cx)),
                ));
            }
            div().absolute().right_0().bottom(px(44.)).child(deferred(b).with_priority(3))
        });
        actions = actions
            .child(
                div()
                    .relative()
                    .flex()
                    .child(
                        div()
                            .id("ny-approve")
                            .flex()
                            .items_center()
                            .h(px(38.))
                            .px(px(16.))
                            .rounded_l(px(7.))
                            .when(start || trust, |d| d.rounded_r(px(7.)))
                            .bg(t.accent)
                            .text_color(t.accent_fg)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(14.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.92))
                            .on_click(cx.listener(|m, _, _, cx| resolve_cur(m, Resolution::Approve { scope: ApprovalScope::Once }, cx)))
                            .child(format!("{} {approve_key}", if start { "Start" } else if trust { "Trust" } else { "Approve" })),
                    )
                    .when(!start && !trust, |d| {
                        d.child(
                            div()
                                .id("ny-approve-more")
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(32.))
                                .h(px(38.))
                                .rounded_r(px(7.))
                                .bg(t.accent)
                                .border_l_1()
                                .border_color(hsla(0., 0., 0., 0.25))
                                .cursor_pointer()
                                .on_click(cx.listener(|m, _, _, cx| {
                                    m.stack.menu = !m.stack.menu;
                                    cx.notify();
                                }))
                                .child(Icon::Chevron.el(12., t.accent_fg)),
                        )
                    })
                    .when(start, |d| d.child(div().w(px(0.)).rounded_r(px(7.))))
                    .children(menu),
            )
            .child(
                div()
                    .id("ny-deny")
                    .flex()
                    .items_center()
                    .h(px(38.))
                    .px(px(16.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(|m, _, _, cx| secondary(m, cx)))
                    .child(format!("Deny {deny_key}")),
            );
    } else {
        let (p, s) = other_actions(n);
        if let Some((label, _)) = p {
            let danger = n.kind == NeedsYouKind::RuleRemoval;
            actions = actions.child(
                div()
                    .id("ny-primary")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(38.))
                    .px(px(16.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(if danger { t.err } else { t.accent })
                    .bg(if danger { t.err.opacity(0.12) } else { t.accent_soft })
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .on_click(cx.listener(|m, _, w, cx| primary(m, w, cx)))
                    .child(label)
                    .child(kbd(t, approve_key.to_string()).font_weight(FontWeight::NORMAL)),
            );
        }
        if let Some((label, _)) = s {
            actions = actions.child(
                div()
                    .id("ny-secondary")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(38.))
                    .px(px(16.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(14.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(|m, _, _, cx| secondary(m, cx)))
                    .child(label)
                    .child(kbd(t, deny_key.to_string()).text_color(t.dim)),
            );
        }
    }
    actions
}

fn empty_state(m: &MainWindow, t: &Theme, back_name: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let log = &m.stack.log;
    let count = |o: Outcome| log.iter().filter(|(_, x)| *x == o).count();
    let mut parts = vec![];
    for (o, w) in [(Outcome::Approved, "approved"), (Outcome::Denied, "denied"), (Outcome::Dismissed, "dismissed")] {
        if count(o) > 0 {
            parts.push(format!("{} {w}", count(o)));
        }
    }
    let (title, sub) = if log.is_empty() {
        (
            "Nothing needs you",
            "When a rule asks, an agent hits a permission prompt or raises a flag, a terminal exits non-zero, or a webhook waits on your gate, it shows up here.".to_string(),
        )
    } else {
        ("All clear", format!("This pass: {}. Nothing else needs you right now.", parts.join(" · ")))
    };
    div()
        .flex_1()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(12.))
        .p(px(40.))
        .child(div().flex().items_center().justify_center().size(px(64.)).rounded_full().border_1().border_color(t.line).bg(t.panel).child(Icon::Check.el(30., t.ok)))
        .child(div().text_size(px(24.)).font_weight(FontWeight::BOLD).child(title))
        .child(div().max_w(px(480.)).text_color(t.dim).text_center().child(sub))
        .child(
            div()
                .id("ny-empty-back")
                .mt(px(8.))
                .flex()
                .items_center()
                .h(px(34.))
                .px(px(16.))
                .rounded(px(7.))
                .bg(t.accent)
                .text_color(t.accent_fg)
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .on_click(cx.listener(|m, _, w, cx| back(m, w, cx)))
                .child(format!("Back to {back_name}")),
        )
}

#[cfg(test)]
mod tests {
    #[test]
    fn tail_keeps_last_eight_without_trailing_blanks() {
        let text = (1..=12).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n") + "\n\n  \n";
        let t = super::tail(&text);
        assert_eq!(t.len(), 8);
        assert_eq!(t.first().unwrap(), "l5");
        assert_eq!(t.last().unwrap(), "l12");
    }

    #[test]
    fn too_long_when_the_card_cant_show_the_question() {
        use crate::model::NeedsYou;
        assert!(!super::too_long(&NeedsYou { detail: "Run the migration?".into(), screen_excerpt: Some(vec!["a".into(); 8]), ..Default::default() }));
        assert!(super::too_long(&NeedsYou { screen_excerpt: Some(vec!["a".into(); 9]), ..Default::default() }));
        assert!(super::too_long(&NeedsYou { detail: "x".repeat(281), ..Default::default() }));
        assert!(super::too_long(&NeedsYou { detail: "1\n2\n3\n4\n5".into(), ..Default::default() }));
    }
}
