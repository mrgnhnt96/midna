//! The sidebar's Summaries section (the Summaries shelf canvas,
//! https://claude.ai/artifact/1iUQRLfUEtSGkfLSXW5Cuf): every unpinned project with no terminals
//! that keeps its Summary (`ui::summary`) gets one row here, at the end of the list, rather than
//! its own heading. The newest close comes first; past `CAP` rows the rest wait behind
//! "Show N more".
//!
//! A row shows the project's name and when its last terminal closed. Under the pointer the age
//! gives way to ⋯ (Open Summary, New agent, New terminal, Pin to sidebar, Clear Summary) and ✕
//! (Clear Summary), as a terminal row's ✕ does; a right-click opens the same menu. The heading
//! folds the section (remembered in app-state.json), and its ⋯ or a right-click offers Clear all
//! summaries. Clearing says so in a toast with Undo.
//!
//! While the heading is scrolled below the list's bottom edge it sticks there; clicking it
//! scrolls the section into view.
//!
//! ⌥⌘↑ / ⌥⌘↓ (`step`) go on from the last terminal to these rows and back round.
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::theme::Theme;
use crate::ui::close_anim::{self, Highlight};
use crate::ui::summary;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

/// Rows shown before "Show N more".
const CAP: usize = 5;
/// How long a Clear's toast offers Undo.
const UNDO_FOR: Duration = Duration::from_secs(8);
/// The heading's fold in `MainWindow::fold_anim` (not a project id).
const FOLD_KEY: &str = "shelf:fold";
/// The heading in `CloseAnim::rows`, for sticking to the list's bottom.
const HEAD_KEY: &str = "shelf:head";
/// The heading folding or unfolding.
const FOLD_MS: f32 = 180.;

/// A Clear the toast can take back: each project's dismissal before it.
pub struct Undo {
    pub msg: String,
    restore: Vec<(String, Option<String>)>,
    at: Instant,
}

/// Whether `pid` has terminals with rows in the sidebar (as `MainWindow::groups` counts them).
pub fn has_rows(m: &MainWindow, pid: &str) -> bool {
    m.sessions.iter().any(|s| s.project_id.as_deref() == Some(pid) && !s.background && m.shows(&s.id) && !crate::ui::split::is_split(m, &s.id))
}

/// Whether `pid`'s Summary row belongs in this section rather than under its own heading.
pub fn in_shelf(m: &MainWindow, pid: &str) -> bool {
    m.projects.iter().any(|p| p.id == pid && !p.pinned) && !has_rows(m, pid)
}

/// The section's rows, newest close first: projects that keep their Summary, and the one whose
/// Summary is on screen.
pub fn pids(m: &MainWindow) -> Vec<String> {
    let shown = summary::shown(m);
    let secs = |p: &str| m.summary.newest(p).and_then(midna_proto::time::parse_rfc3339).unwrap_or(0);
    let v: Vec<(i64, &str, &str)> = m
        .projects
        .iter()
        .filter(|p| !p.pinned && !has_rows(m, &p.id) && (shown == Some(p.id.as_str()) || m.summary.keeps(&p.id)))
        .map(|p| (secs(&p.id), p.name.as_str(), p.id.as_str()))
        .collect();
    newest_first(v)
}

fn name(m: &MainWindow, pid: &str) -> String {
    m.projects.iter().find(|p| p.id == pid).map(|p| p.name.clone()).unwrap_or_default()
}

/// The rows ⌥⌘↑ / ⌥⌘↓ reach: the unfolded section's (only the one on screen when folded).
fn reachable(m: &MainWindow) -> Vec<String> {
    let all = pids(m);
    if m.sidebar_collapsed {
        return vec![];
    }
    if m.summary.shelf_folded {
        return all.into_iter().filter(|p| summary::shown(m) == Some(p.as_str())).collect();
    }
    all
}

/// ⌥⌘↑ / ⌥⌘↓ over `terms` (the terminals in sidebar order) and then this section's rows,
/// wrapping round. False when there are none here and nothing of this section is on screen:
/// the terminals alone step as before.
pub fn step(m: &mut MainWindow, terms: &[String], by: isize, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let shelf = reachable(m);
    let here = summary::shown(m).filter(|p| shelf.iter().any(|x| x == p)).map(str::to_string);
    if shelf.is_empty() {
        return false;
    }
    let stops: Vec<(bool, &String)> = terms.iter().map(|t| (false, t)).chain(shelf.iter().map(|p| (true, p))).collect();
    let at = match &here {
        Some(p) => stops.iter().position(|(s, x)| *s && *x == p),
        None => m.selected.as_ref().and_then(|id| stops.iter().position(|(s, x)| !*s && *x == id)),
    };
    let next = next(at, stops.len(), by);
    let (is_shelf, id) = (stops[next].0, stops[next].1.clone());
    if Some(next) == at {
        return true;
    }
    crate::sounds::play("switched");
    if is_shelf {
        if !m.summary.shelf_more && shelf.iter().position(|p| *p == id).is_some_and(|i| i >= CAP) {
            m.summary.shelf_more = true;
        }
        summary::open(m, id, window, cx);
    } else {
        if here.is_none() {
            close_anim::switch(m, &id, cx);
        }
        m.select(id, window, cx);
    }
    true
}

/// The stop `by` away from `at` among `n`, wrapping round; from none, the first going down and
/// the last going up.
fn next(at: Option<usize>, n: usize, by: isize) -> usize {
    match at {
        Some(i) => (i as isize + by).rem_euclid(n as isize) as usize,
        None if by > 0 => 0,
        None => n - 1,
    }
}

/// Newest close first; names break ties.
fn newest_first(mut v: Vec<(i64, &str, &str)>) -> Vec<String> {
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    v.into_iter().map(|(_, _, id)| id.to_string()).collect()
}

/// Clear every row here, in one go (Undo brings them all back). The section folds away.
pub fn clear_all(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    let all = pids(m);
    if all.is_empty() {
        return;
    }
    let restore: Vec<(String, Option<String>)> = all.iter().map(|p| (p.clone(), m.summary.dismissed.get(p).cloned())).collect();
    for p in &all {
        if let Some(newest) = m.summary.newest(p).map(str::to_string) {
            m.summary.dismissed.insert(p.clone(), newest);
        }
    }
    crate::ui::statusbar::save_state(m);
    let msg = match all.as_slice() {
        [one] => format!("{}’s Summary cleared", name(m, one)),
        _ => format!("{} summaries cleared", all.len()),
    };
    offer_undo(m, msg, restore, cx);
    let shown = summary::shown(m).is_some_and(|p| all.iter().any(|x| x == p));
    close_anim::shelf_leave(m, all, cx);
    if shown {
        m.set_screen(Screen::Terminal, window, cx);
    }
    cx.notify();
}

/// A toast saying `msg`, with Undo putting `restore` back, for `UNDO_FOR`.
pub fn offer_undo(m: &mut MainWindow, msg: String, restore: Vec<(String, Option<String>)>, cx: &mut Context<MainWindow>) {
    m.toast = None;
    m.summary.undo = Some(Undo { msg, restore, at: Instant::now() });
    cx.spawn(async move |this, cx| {
        cx.background_executor().timer(UNDO_FOR + Duration::from_millis(100)).await;
        let _ = this.update(cx, |m, cx| {
            if m.summary.undo.as_ref().is_some_and(|u| u.at.elapsed() >= UNDO_FOR) {
                m.summary.undo = None;
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

fn undo(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(u) = m.summary.undo.take() else { return };
    let back: Vec<String> = u.restore.iter().map(|(p, _)| p.clone()).collect();
    for (p, before) in u.restore {
        match before {
            Some(b) => m.summary.dismissed.insert(p, b),
            None => m.summary.dismissed.remove(&p),
        };
    }
    crate::ui::statusbar::save_state(m);
    close_anim::shelf_back(m, &back, cx);
    cx.notify();
}

/// The Clear toast, bottom right, with its Undo.
pub fn undo_toast(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let u = m.summary.undo.as_ref().filter(|u| u.at.elapsed() < UNDO_FOR)?;
    Some(
        div()
            .id("shelf-undo")
            .absolute()
            .right(px(16.))
            .bottom(px(40.))
            .max_w(px(420.))
            .flex()
            .items_center()
            .gap(px(12.))
            .pl(px(12.))
            .pr(px(6.))
            .py(px(5.))
            .rounded(px(8.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .text_size(px(12.))
            .occlude()
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .child(div().min_w_0().truncate().child(u.msg.clone()))
            .child(super::screen_kit::btn(t, "shelf-undo-btn", "Undo").on_click(cx.listener(|m, _, _, cx| undo(m, cx))))
            .into_any_element(),
    )
}

fn toggle_fold(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.summary.shelf_folded = !m.summary.shelf_folded;
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        m.fold_anim.remove(FOLD_KEY);
    } else {
        m.fold_anim.insert(FOLD_KEY.into(), Instant::now());
    }
    crate::ui::statusbar::update_state(&m.backend, "summaries_folded", serde_json::json!(m.summary.shelf_folded));
    cx.notify();
}

/// How far the section is folded, 0 (open) to 1.
fn fold(m: &MainWindow, window: &mut Window) -> f32 {
    let folded = m.summary.shelf_folded;
    match m.fold_anim.get(FOLD_KEY) {
        Some(at) if at.elapsed().as_secs_f32() * 1000. < FOLD_MS => {
            window.request_animation_frame();
            let e = 1. - (1. - at.elapsed().as_secs_f32() * 1000. / FOLD_MS).powi(3);
            if folded { e } else { 1. - e }
        }
        _ => if folded { 1. } else { 0. },
    }
}

/// A small icon button in a row or the heading, shown under the pointer.
fn icon_btn(t: &Theme, id: String, icon: Icon, size: f32, tip: &'static str) -> Stateful<Div> {
    div()
        .id(SharedString::from(id))
        .relative()
        .size(px(20.))
        .rounded(px(4.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(t.line))
        .tooltip(super::header::tip(tip))
        .child(icon.el(size, t.dim))
}

/// The heading: the fold chevron, "SUMMARIES", the count, and ⋯ under the pointer.
fn heading(m: &MainWindow, t: &Theme, n: usize, turn: f32, stuck: bool, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let menu_open = m.menu == Menu::Shelf && !stuck;
    let group = SharedString::from(if stuck { "shelf-head-stuck" } else { "shelf-head" });
    let chevron = Icon::Chevron.el(10., t.dim).when(turn > 0., |s| s.with_transformation(Transformation::rotate(radians(-std::f32::consts::FRAC_PI_2 * turn))));
    let open_menu = |m: &mut MainWindow, cx: &mut Context<MainWindow>| {
        m.menu = if m.menu == Menu::Shelf { Menu::None } else { Menu::Shelf };
        cx.notify();
    };
    div()
        .id(group.clone())
        .group(group.clone())
        .flex()
        .items_center()
        .gap(px(6.))
        .pt(px(8.))
        .pr(px(8.))
        .pb(px(2.))
        .pl(px(10.))
        .cursor_pointer()
        .tooltip(super::header::tip(if stuck { "Go to Summaries" } else if m.summary.shelf_folded { "Show Summaries (right-click for more)" } else { "Fold Summaries (right-click for more)" }))
        .on_click(cx.listener(move |m, _, _, cx| {
            if stuck {
                let s = &m.sidebar_scroll;
                s.scroll_to_top_of_item(s.children_count().saturating_sub(1));
                cx.notify();
            } else {
                toggle_fold(m, cx);
            }
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |m, _, _, cx| {
                cx.stop_propagation();
                if !stuck {
                    open_menu(m, cx);
                }
            }),
        )
        .child(chevron)
        .child(super::caps_label(t, "Summaries"))
        .child(div().text_size(px(11.)).text_color(t.dim).child(n.to_string()))
        .child(div().flex_1())
        .when(!stuck, |d| {
            d.child(
                icon_btn(t, "shelf-head-more".into(), Icon::Dots, 14., "Summaries actions")
                    .size(px(26.))
                    .rounded(px(6.))
                    .when(menu_open, |d| d.bg(t.raised))
                    .when(!menu_open, |d| d.opacity(0.).group_hover(group, |s| s.opacity(1.)))
                    .on_click(cx.listener(move |m, _, _, cx| {
                        cx.stop_propagation();
                        open_menu(m, cx);
                    }))
                    .when(menu_open, |d| d.child(shelf_menu(t, cx))),
            )
        })
}

fn shelf_menu(t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            super::sidebar::menu_box(t)
                .occlude()
                .mt(px(30.))
                .child(super::sidebar::menu_item(t, "shelf-clear-all", "Clear all summaries", "", cx.listener(|m, _, w, cx| clear_all(m, w, cx)))),
        ),
    )
    .with_priority(1)
}

fn row_menu(t: &Theme, pid: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let (p1, p2, p3, p4, p5) = (pid.to_string(), pid.to_string(), pid.to_string(), pid.to_string(), pid.to_string());
    let rule = || div().h(px(1.)).mx(px(6.)).my(px(4.)).bg(t.line);
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            super::sidebar::menu_box(t)
                .occlude()
                .mt(px(24.))
                .child(super::sidebar::menu_item(t, "shelf-open", "Open Summary", "", cx.listener(move |m, _, w, cx| {
                    m.menu = Menu::None;
                    summary::open(m, p1.clone(), w, cx);
                })))
                .child(super::sidebar::menu_item(t, "shelf-agent", "New agent", "", cx.listener(move |m, _, _, cx| {
                    m.menu = Menu::None;
                    let a = m.last_agent(Some(&p2));
                    m.open_session(Some(p2.clone()), Some(a), cx);
                    cx.notify();
                })))
                .child(super::sidebar::menu_item(t, "shelf-term", "New terminal", "", cx.listener(move |m, _, _, cx| {
                    m.menu = Menu::None;
                    m.open_session(Some(p3.clone()), None, cx);
                    cx.notify();
                })))
                .child(rule())
                .child(
                    super::sidebar::menu_item(t, "shelf-pin", "Pin to sidebar", "", cx.listener(move |m, _, _, cx| {
                        m.menu = Menu::None;
                        m.rpc("project.update", serde_json::json!({"id": p4, "pinned": true}), cx, |m, _, _, cx| m.request_refresh(crate::app::refresh::PROJECTS, cx));
                        cx.notify();
                    })),
                )
                .child(rule())
                .child(super::sidebar::menu_item(t, "shelf-clear", "Clear Summary", "", cx.listener(move |m, _, w, cx| {
                    m.menu = Menu::None;
                    summary::close_row(m, &p5, w, cx);
                }))),
        ),
    )
    .with_priority(1)
}

/// A project's row: its Summary icon, its name, and when its last terminal closed, which gives
/// way to ⋯ and ✕ under the pointer.
fn row(m: &MainWindow, t: &Theme, pid: &str, compact: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let on = summary::shown(m) == Some(pid);
    let hl = m.close_anim.highlight(&close_anim::summary_key(pid), on);
    let own = on && hl == Highlight::Own;
    let moving = match hl {
        Highlight::At { top, h } if m.close_anim.summary_leaving(pid).is_none() => Some((top, h)),
        _ => None,
    };
    let menu_open = m.menu == Menu::ShelfRow(pid.to_string());
    let last = m.summary.newest(pid).map(|ts| super::screen_kit::ago(Some(ts)));
    let group = SharedString::from(format!("shelf-row-{pid}"));
    let (open_id, menu_id, more_id, x_id) = (pid.to_string(), pid.to_string(), pid.to_string(), pid.to_string());
    let label = name(m, pid);
    let acts = div()
        .absolute()
        .top_0()
        .right_0()
        .h_full()
        .flex()
        .items_center()
        .gap(px(2.))
        .when(!menu_open, |d| d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.)))
        .child(
            icon_btn(t, format!("shelf-more-{pid}"), Icon::Dots, 12., "Actions")
                .when(menu_open, |d| d.bg(t.line))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |m, _, _, cx| {
                    cx.stop_propagation();
                    let k = Menu::ShelfRow(more_id.clone());
                    m.menu = if m.menu == k { Menu::None } else { k };
                    cx.notify();
                }))
                .when(menu_open, |d| d.child(row_menu(t, pid, cx))),
        )
        .child(
            icon_btn(t, format!("shelf-x-{pid}"), Icon::Cross, 10., "Clear Summary")
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |m, _, w, cx| {
                    cx.stop_propagation();
                    summary::close_row(m, &x_id, w, cx);
                })),
        );
    div()
        .id(group.clone())
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .gap(px(9.))
        .w_full()
        .px(px(14.))
        .py(px(if compact { 5. } else { 8. }))
        .cursor_pointer()
        .when_some(moving, |d, (top, h)| d.child(super::sidebar::highlight(t).top(px(top)).h(px(h))))
        .when(own, |d| d.bg(t.raised).child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .when(!on, |d| d.hover(|s| s.bg(t.raised.opacity(0.5))))
        .tooltip(super::header::tip(format!("{label}’s closed terminals")))
        .on_click(cx.listener(move |m, _, w, cx| summary::open(m, open_id.clone(), w, cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |m, _, _, cx| {
                cx.stop_propagation();
                let k = Menu::ShelfRow(menu_id.clone());
                m.menu = if m.menu == k { Menu::None } else { k };
                cx.notify();
            }),
        )
        .child(div().w(px(12.)).flex_none().flex().items_center().child(Icon::Summary.el(12., if on { t.accent } else { t.dim })))
        .child(div().flex_1().min_w_0().font_weight(FontWeight::MEDIUM).truncate().child(label))
        .child(
            div()
                .relative()
                .flex_none()
                .min_w(px(44.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_end()
                .children(last.map(|a| div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().when(menu_open, |d| d.opacity(0.)).group_hover(group.clone(), |s| s.opacity(0.)).child(a)))
                .child(acts),
        )
        .into_any_element()
}

/// The section, at the end of the sidebar's list. None with no rows.
pub fn section(m: &MainWindow, t: &Theme, compact: bool, window: &mut Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    // Clear all: the section as it was folds away whole.
    if let Some(l) = m.close_anim.shelf_leaving() {
        let mut col = div().flex().flex_col().child(heading(m, t, l.pids.len(), 0., false, cx));
        for p in l.pids.iter().take(if m.summary.shelf_more { usize::MAX } else { CAP }) {
            col = col.child(row(m, t, p, compact, cx));
        }
        let (fold, lift) = l.progress();
        let measure = l.measure();
        let content = div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .when_some(l.height(), |d, h| d.h(px(h * (1. - fold))))
            .on_children_prepainted(move |b, _, _| {
                if let Some(b) = b.first() {
                    measure.set(f32::from(b.size.height));
                }
            })
            .child(div().flex_none().flex().flex_col().relative().top(px(-close_anim::LIFT * lift)).opacity(1. - lift).child(col));
        return Some(div().mt(px(4.)).border_t_1().border_color(t.line).relative().child(content).child(div().absolute().inset_0().occlude()).into_any_element());
    }
    let mut list = pids(m);
    // A row just cleared folds away where it was.
    let leaving = m.projects.iter().find(|p| m.close_anim.shelf_row_leaving(&p.id).is_some() && !list.contains(&p.id)).map(|p| p.id.clone());
    if let Some(gone) = &leaving {
        let secs = |p: &str| m.summary.newest(p).and_then(midna_proto::time::parse_rfc3339).unwrap_or(0);
        let at = list.iter().position(|p| secs(p) < secs(gone)).unwrap_or(list.len());
        list.insert(at, gone.clone());
    }
    if list.is_empty() {
        return None;
    }
    let count = list.len() - usize::from(leaving.is_some());
    let fold = fold(m, window);
    let more = m.summary.shelf_more;
    let mut col = div().flex().flex_col();
    let (mut run, mut ri, mut hidden) = (vec![], 0, 0);
    for (i, p) in list.iter().enumerate() {
        let on = summary::shown(m) == Some(p.as_str());
        if i >= CAP && !more && !on {
            hidden += 1;
            continue;
        }
        let el = super::sidebar::measured(m, &close_anim::summary_key(p), row(m, t, p, compact, cx));
        let el = match (leaving.as_deref() == Some(p.as_str()), m.close_anim.summary_leaving(p), m.close_anim.arrival(&close_anim::summary_key(p))) {
            (true, Some(l), _) => super::sidebar::fold_away(m, l, t, el),
            (_, _, Some(a)) => super::sidebar::arrive(a, el),
            _ => el,
        };
        if on {
            col = col.children(super::sidebar::fold_run(std::mem::take(&mut run), format!("shelf/{ri}"), fold, m)).child(el);
            ri += 1;
        } else if fold < 1. {
            run.push(el);
        }
    }
    if fold < 1. {
        let toggle = |m: &mut MainWindow, cx: &mut Context<MainWindow>| {
            m.summary.shelf_more = !m.summary.shelf_more;
            cx.notify();
        };
        let label = if hidden > 0 { Some(format!("Show {hidden} more")) } else if more && list.len() > CAP { Some("Show fewer".to_string()) } else { None };
        if let Some(label) = label {
            run.push(
                div()
                    .id("shelf-show-more")
                    .pl(px(35.))
                    .pr(px(14.))
                    .py(px(5.))
                    .text_size(px(12.))
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.fg))
                    .on_click(cx.listener(move |m, _, _, cx| toggle(m, cx)))
                    .child(label)
                    .into_any_element(),
            );
        }
    }
    col = col.children(super::sidebar::fold_run(run, format!("shelf/{ri}"), fold, m));
    let head = super::sidebar::measured(m, HEAD_KEY, heading(m, t, count, fold, false, cx).into_any_element());
    Some(div().flex().flex_col().mt(px(4.)).pb(px(4.)).border_t_1().border_color(t.line).child(head).child(col).into_any_element())
}

/// The heading, stuck to the list's bottom edge while the section is scrolled below it.
pub fn stuck(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if m.close_anim.shelf_leaving().is_some() {
        return None;
    }
    let n = pids(m).len();
    if n == 0 {
        return None;
    }
    let (top, h) = *m.close_anim.rows.borrow().get(HEAD_KEY)?;
    let bottom = f32::from(m.sidebar_scroll.bounds().bottom());
    if top + h <= bottom + 0.5 {
        return None;
    }
    Some(div().absolute().left_0().right_0().bottom_0().pb(px(4.)).bg(t.panel).border_t_1().border_color(t.line).child(heading(m, t, n, 0., true, cx)).into_any_element())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn stepping_wraps_round_both_ways() {
        assert_eq!(next(Some(2), 3, 1), 0);
        assert_eq!(next(Some(0), 3, -1), 2);
        assert_eq!(next(Some(1), 3, 1), 2);
        assert_eq!(next(None, 3, 1), 0);
        assert_eq!(next(None, 3, -1), 2);
    }

    #[test]
    fn rows_go_newest_close_first_then_by_name() {
        let v = vec![(10, "b", "p_b"), (30, "c", "p_c"), (10, "a", "p_a")];
        assert_eq!(newest_first(v), vec!["p_c", "p_a", "p_b"]);
    }
}
