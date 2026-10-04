//! Needs-you card stack (docs/design/NeedsYou-C.dc.html): one item at a time, keyboard
//! driven. Replaces the terminal pane (the sidebar stays). Data is `MainWindow::needs`
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

pub struct Stack {
    pub focus: FocusHandle,
    /// The card on top. When it disappears (resolved here or elsewhere) the item now at
    /// `pos` takes its place.
    cur: Option<String>,
    pos: usize,
    menu: bool,
    /// Handled during this pass, for the progress bar and the "All clear" summary.
    log: Vec<(String, Outcome)>,
    /// Live screen tails for items without a `screen_excerpt` (None while loading).
    excerpts: HashMap<String, Option<Vec<String>>>,
    /// Resolved here but maybe still in a `needs_you.list` fetched before the resolve
    /// landed; hidden for a few seconds (the daemon's list catches up well within that).
    gone: HashMap<String, std::time::Instant>,
}

impl Stack {
    pub fn new(cx: &mut App) -> Stack {
        Stack { focus: cx.focus_handle(), cur: None, pos: 0, menu: false, log: vec![], excerpts: HashMap::new(), gone: HashMap::new() }
    }
}

/// Called when the stack opens: a new pass.
pub fn on_open(m: &mut MainWindow) {
    let s = &mut m.stack;
    s.cur = None;
    s.pos = 0;
    s.menu = false;
    s.log.clear();
    s.excerpts.clear();
    s.gone.clear();
    // dev: screenshot the approve dropdown
    s.menu = crate::dev::var("MIDNA_DEBUG_APPROVE_MENU").is_ok();
    if s.menu {
        s.cur = m.needs.iter().find(|n| n.is_approval()).map(|n| n.id.clone());
    }
}

/// Oldest first.
fn ordered(m: &MainWindow) -> Vec<&NeedsYou> {
    let mut v: Vec<&NeedsYou> = m.needs.iter().filter(|n| !m.stack.gone.contains_key(&n.id)).collect();
    v.sort_by_key(|n| parse_rfc3339(&n.created_at).unwrap_or(i64::MAX));
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
    let approve =
        matches!(n.kind, NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt | NeedsYouKind::TriggerWaiting) || (n.approval.is_some() && n.kind == NeedsYouKind::Other);
    Meta { label, color, approve }
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

fn where_of(m: &MainWindow, n: &NeedsYou) -> String {
    match session_of(m, n) {
        Some(s) => format!("{} › {}", project_name(m, s.project_id.as_deref()), s.name),
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

/// ⌘J inside the stack: next card (wraps).
pub fn skip(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let order = ordered(m);
    if let Some(i) = current_index(m, &order) {
        let next = (i + 1) % order.len();
        m.stack.cur = Some(order[next].id.clone());
        m.stack.pos = next;
        m.stack.menu = false;
    }
    cx.notify();
}

fn go_to(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    m.stack.cur = Some(id);
    m.stack.menu = false;
    cx.notify();
}

/// ⌘O: leave the stack and open the card's terminal.
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
    if !ks.modifiers.platform {
        return;
    }
    match ks.key.as_str() {
        "o" => open_terminal(m, window, cx),
        "enter" if ks.modifiers.shift => approve_all(m, cx),
        _ => return,
    }
    cx.stop_propagation();
}

/// Fetch the live screen tail for the current card when the item carries no excerpt.
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

/// The last 8 lines, without trailing blanks.
fn tail(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let skip = lines.len().saturating_sub(8);
    lines.split_off(skip)
}

// ------------------------------------------------------------------ view

fn kbd(t: &Theme, s: impl Into<SharedString>) -> Div {
    div().font_family(t.mono_font.clone()).text_size(px(11.)).child(s.into())
}

fn ghost_button(t: &Theme, id: impl Into<ElementId>, label: impl Into<SharedString>, key: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(38.))
        .px(px(12.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .child(label.into())
        .when(!key.is_empty(), |d| d.child(kbd(t, key)))
}

pub fn render(m: &mut MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    m.stack.gone.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(5));
    ensure_excerpt(m, cx);
    let m = &*m;
    let order = ordered(m);
    let cur_i = current_index(m, &order);
    let back_name = m.selected_session().map(|s| s.name.clone()).unwrap_or_else(|| "terminal".into());
    let approve_key = m.key_label("keys.approve");
    let deny_key = m.key_label("keys.deny");
    let next_key = m.key_label("keys.next_needs_you");

    // ---- header: title, N left, progress
    let mut progress = div().flex().gap(px(3.)).w(px(280.));
    for (_, o) in &m.stack.log {
        progress = progress.child(div().flex_1().h(px(5.)).rounded(px(3.)).bg(if *o == Outcome::Denied { t.err } else { t.ok }));
    }
    for (i, _) in order.iter().enumerate() {
        progress = progress.child(div().flex_1().h(px(5.)).rounded(px(3.)).bg(if Some(i) == cur_i { t.accent } else { t.line }));
    }
    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(14.))
        .min_h(px(44.))
        .pl(px(18.))
        .pr(px(12.))
        .border_b_1()
        .border_color(t.line)
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Needs you"))
        .when(!order.is_empty(), |d| d.child(div().text_color(t.dim).child(format!("{} left", order.len()))).child(progress))
        .child(div().flex_1())
        .child(
            div()
                .id("ny-back")
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(10.))
                .rounded(px(7.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(|m, _, w, cx| back(m, w, cx)))
                .child(format!("Back to {back_name}"))
                .child(kbd(t, "esc")),
        );

    let body: AnyElement = match cur_i {
        None => empty_state(m, t, &back_name, cx).into_any_element(),
        Some(i) => {
            let n = order[i].clone();
            let after: Vec<NeedsYou> = (1..order.len()).map(|k| order[(i + k) % order.len()].clone()).collect();
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .id("ny-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .items_center()
                        .px(px(24.))
                        .pt(px(34.))
                        .pb(px(16.))
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .max_w(px(760.))
                                .mt(px(14.))
                                .when(after.len() > 1, |d| d.child(behind(t, 36., -20., 0.45)))
                                .when(!after.is_empty(), |d| d.child(behind(t, 18., -10., 0.75)))
                                .child(card(m, t, &n, &approve_key, &deny_key, &next_key, cx)),
                        )
                        .when(!after.is_empty(), |d| d.child(up_next(m, t, &after, cx))),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .flex_wrap()
                        .justify_center()
                        .gap(px(18.))
                        .px(px(16.))
                        .py(px(10.))
                        .border_t_1()
                        .border_color(t.line)
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .children(key_hints(t, &n, &approve_key, &deny_key))
                        .child(hint(t, &next_key, "skip"))
                        .child(hint(t, "⌘O", "open terminal"))
                        .child(hint(t, "esc", "back")),
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
        .child(
            div()
                .size_full()
                .flex()
                .flex_col()
                .bg(linear_gradient(180., linear_color_stop(t.accent_soft, 0.), linear_color_stop(t.accent_soft.opacity(0.), 0.55)))
                .child(header)
                .child(body),
        )
}

/// Footer hints for the card's own actions ("⌘↩ approve once", "⌘↩ got it", ...).
fn key_hints(t: &Theme, n: &NeedsYou, approve_key: &str, deny_key: &str) -> Vec<Div> {
    if meta(t, n).approve {
        return vec![hint(t, approve_key, "approve once"), hint(t, deny_key, "deny")];
    }
    let (p, s) = other_actions(n);
    let mut out = vec![];
    if let Some((label, _)) = p {
        out.push(hint(t, approve_key, &label.to_lowercase()));
    }
    if let Some((label, _)) = s {
        out.push(hint(t, deny_key, &label.to_lowercase()));
    }
    out
}

fn hint(t: &Theme, key: &str, label: &str) -> Div {
    div().flex().gap(px(5.)).child(kbd(t, key.to_string()).text_color(t.fg)).child(label.to_string())
}

fn behind(t: &Theme, inset: f32, top: f32, opacity: f32) -> Div {
    div().absolute().left(px(inset)).right(px(inset)).top(px(top)).h(px(60.)).rounded(px(14.)).border_1().border_color(t.line).bg(t.panel).opacity(opacity)
}

fn card(m: &MainWindow, t: &Theme, n: &NeedsYou, approve_key: &str, deny_key: &str, next_key: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mt = meta(t, n);
    let sess = session_of(m, n);
    let who = commands::who(n, sess);
    let where_ = where_of(m, n);
    let icon =
        if n.asked_by.kind == "trigger" || n.kind == NeedsYouKind::TriggerWaiting { Icon::Triggers } else { sess.map(|s| Icon::from_glyph(s.glyph())).unwrap_or(Icon::Shell) };
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
                    .when(sess.is_some(), |d| {
                        d.child(
                            div()
                                .id("peek-open")
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(24.))
                                .px(px(8.))
                                .rounded(px(6.))
                                .cursor_pointer()
                                .hover(|s| s.bg(t.raised))
                                .on_click(cx.listener(|m, _, w, cx| open_terminal(m, w, cx)))
                                .child(Icon::PopOut.el(12., t.dim))
                                .child("Open terminal")
                                .child(kbd(t, "⌘O")),
                        )
                    }),
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

    // actions
    let mut actions = div().flex().flex_wrap().items_center().gap(px(8.));
    if mt.approve {
        let start = n.kind == NeedsYouKind::TriggerWaiting;
        let session_name = sess.map(|s| s.name.clone()).unwrap_or_else(|| "it".into());
        let scope_hint = if start { "this trigger" } else { "this command" };
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
                            .bg(t.accent)
                            .text_color(t.accent_fg)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(14.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.92))
                            .on_click(cx.listener(|m, _, _, cx| resolve_cur(m, Resolution::Approve { scope: ApprovalScope::Once }, cx)))
                            .child(format!("{} {approve_key}", if start { "Start" } else { "Approve" })),
                    )
                    .when(!start, |d| {
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
    actions = actions
        .child(div().flex_1())
        .when(sess.is_some(), |d| d.child(ghost_button(t, "ny-open", "Open terminal", "⌘O".into()).text_color(t.dim).on_click(cx.listener(|m, _, w, cx| open_terminal(m, w, cx)))))
        .child(ghost_button(t, "ny-skip", "Skip", next_key.to_string()).border_1().border_color(t.line).on_click(cx.listener(|m, _, _, cx| skip(m, cx))));

    let since = since_short(&n.created_at);
    div()
        .relative()
        .flex()
        .flex_col()
        .gap(px(14.))
        .px(px(22.))
        .py(px(20.))
        .rounded(px(14.))
        .overflow_hidden()
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(18.)), blur_radius: px(50.), spread_radius: px(0.), inset: false }])
        .child(div().absolute().top_0().left_0().right_0().h(px(3.)).bg(mt.color))
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
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(mt.color.opacity(0.14))
                        .text_color(mt.color)
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .child(mt.label.to_uppercase()),
                )
                .when(!since.is_empty(), |d| d.child(div().text_color(t.dim).child(if since == "now" { "just now".to_string() } else { format!("{since} ago") })))
                .child(div().flex_1())
                .child(div().flex().items_center().gap(px(7.)).text_color(t.dim).child(icon.el(14., t.dim)).child(format!("{who} · {where_}"))),
        )
        .child(div().text_size(px(24.)).line_height(px(29.)).font_weight(FontWeight::BOLD).child(n.title.clone()))
        .child(grid)
        .children(peek)
        .children(bulk)
        .child(actions)
}

fn up_next(m: &MainWindow, t: &Theme, after: &[NeedsYou], cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let mut d = div().w_full().max_w(px(760.)).mt(px(22.)).child(div().px(px(4.)).pb(px(6.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child("UP NEXT"));
    for (i, n) in after.iter().take(3).enumerate() {
        let mt = meta(t, n);
        let id = n.id.clone();
        d = d.child(
            div()
                .id(("ny-next", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .w_full()
                .px(px(10.))
                .py(px(7.))
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, _, _, cx| go_to(m, id.clone(), cx)))
                .child(div().size(px(7.)).flex_none().rounded_full().bg(mt.color))
                .child(div().w(px(130.)).flex_none().text_size(px(12.)).text_color(t.dim).child(mt.label))
                .child(div().flex_1().min_w_0().truncate().child(n.title.clone()))
                .child(div().flex_none().text_size(px(12.)).text_color(t.dim).child(where_of(m, n))),
        );
    }
    d
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
            "When a rule asks, an agent hits a permission prompt or raises a flag, a terminal exits non-zero, or a webhook waits on your gate, it shows up here one card at a time.".to_string(),
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
}
