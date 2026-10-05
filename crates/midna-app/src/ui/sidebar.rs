//! Left sidebar (264px): traffic-light strip, "N need you", project groups with terminal
//! rows (click a heading to fold it; remembered in app-state.json), and the footer (Today card + Triggers / Rules / Settings).
//! Collapsed (⌘B, also in app-state.json) it's a 76px rail: one status dot per terminal.
use super::caps_label;
use crate::actions::*;
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub const WIDTH: f32 = 264.;
/// Collapsed: just wide enough for the traffic lights.
pub const RAIL_WIDTH: f32 = 76.;
/// Group fold/unfold duration (skipped under the system's Reduce motion).
const FOLD_MS: f32 = 180.;

/// A terminal row being dragged to a new place in its group.
struct DraggedRow {
    id: String,
    group: String,
}

/// Nothing follows the cursor: the row itself moves as the drag crosses its neighbours.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// The sidebar, which also tracks a row dragged out of the window (to move it to another
/// main window, or a new one) and lights up while another window's row is over it.
pub fn render(m: &MainWindow, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let inner = if m.sidebar_collapsed { rail(m, t, cx).into_any_element() } else { full(m, t, window, cx) };
    let hint = m.windows.borrow().drop_hint == Some(m.id);
    div()
        .id("sidebar")
        .relative()
        .flex()
        .flex_none()
        .h_full()
        .on_drag_move(cx.listener(drag_out_move))
        .on_mouse_up(MouseButton::Left, cx.listener(|m, ev: &MouseUpEvent, w, cx| drag_out_end(m, ev.position, w, cx)))
        .on_mouse_up_out(MouseButton::Left, cx.listener(|m, ev: &MouseUpEvent, w, cx| drag_out_end(m, ev.position, w, cx)))
        .child(inner)
        .when(hint, |d| {
            d.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.accent_soft)
                    .border_2()
                    .border_color(t.accent)
                    .text_color(t.accent)
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .child("Move here"),
            )
        })
        .into_any_element()
}

/// The rows a drag moves: the dragged one, or its group's selected rows when it's selected.
fn drag_ids(m: &MainWindow, dragged: &str) -> Vec<String> {
    if m.marked.len() > 1 && m.is_marked(dragged) { m.marked_ids() } else { vec![dragged.to_string()] }
}

fn drag_out_move(m: &mut MainWindow, ev: &DragMoveEvent<DraggedRow>, window: &mut Window, cx: &mut Context<MainWindow>) {
    let dragged = ev.drag(cx).id.clone();
    let pos = ev.event.position;
    m.drag_out = Some((drag_ids(m, &dragged), pos));
    let bounds = window.bounds();
    let over = crate::windows::window_at(bounds.origin + pos, m.id, bounds, cx).filter(|w| *w != m.id);
    crate::windows::set_drop_hint(over, cx);
}

/// Released: over another main window, the rows move there; outside every window, they get
/// a new one under the cursor. Inside this window the drag was a reorder (already done).
fn drag_out_end(m: &mut MainWindow, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some((ids, _)) = m.drag_out.take() else { return };
    crate::windows::set_drop_hint(None, cx);
    let bounds = window.bounds();
    let screen = bounds.origin + pos;
    if bounds.contains(&screen) {
        return;
    }
    let target = crate::windows::window_at(screen, m.id, bounds, cx);
    if target == Some(m.id) {
        return;
    }
    m.hand_off(&ids, window, cx);
    let backend = m.backend.clone();
    cx.defer(move |cx| {
        let target = target.or_else(|| {
            let at = Bounds::new(screen - point(px(80.), px(20.)), bounds.size);
            crate::windows::open(backend, Some(at), cx).and_then(|h| crate::windows::id_of(h, cx))
        });
        if let Some(t) = target {
            crate::windows::give(ids, t, cx);
        }
    });
}

fn full(m: &MainWindow, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let compact = m.compact();
    let need_n = m.needs.len();
    let jump_key = m.key_label("keys.next_needs_you");

    let top = titlebar_strip().flex().items_center().justify_end().pr(px(8.)).child(collapse_button(t, false));

    let need_btn = {
        let has = need_n > 0;
        let (fg, bg, border) = if has { (t.need, t.need_soft, t.need) } else { (t.dim, t.raised.opacity(0.0), t.line) };
        div()
            .id("need-you")
            .flex()
            .items_center()
            .gap(px(8.))
            .mx(px(10.))
            .mb(px(6.))
            .h(px(34.))
            .px(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .text_color(fg)
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .tooltip(super::header::tip_keys("Open the needs-you cards", "keys.needs_you"))
            .on_click(|_, w, cx| w.dispatch_action(Box::new(OpenNeedsYou), cx))
            .child(div().size(px(8.)).rounded_full().flex_none().when(has, |d| d.bg(t.need)).when(!has, |d| super::border_w(d, 1.5).border_color(t.dim)))
            .child(div().flex_1().child(if has { format!("{need_n} need you") } else { "Nothing needs you".to_string() }))
            .child(div().font_family(t.mono_font.clone()).text_size(px(11.)).font_weight(FontWeight::NORMAL).child(jump_key))
    };

    let mut list = div().id("sessions").flex().flex_col().flex_1().min_h_0().py(px(4.)).overflow_y_scroll();
    for (gi, g) in m.groups().into_iter().enumerate() {
        let pid = g.project.map(|p| p.id.clone());
        let n = m.needs_in_project(pid.as_deref());
        let menu_key = pid.clone().unwrap_or_else(|| "root".into());
        let menu_open = m.menu == Menu::Project(menu_key.clone());
        let collapsed = pid.as_ref().is_some_and(|p| m.collapsed.contains(p));
        // 0 = open, 1 = folded; eased between the two for FOLD_MS after a heading click.
        let fold = match pid.as_ref().and_then(|p| m.fold_anim.get(p)) {
            Some(at) if at.elapsed().as_secs_f32() * 1000. < FOLD_MS => {
                window.request_animation_frame();
                let e = 1. - (1. - at.elapsed().as_secs_f32() * 1000. / FOLD_MS).powi(3);
                if collapsed { e } else { 1. - e }
            }
            _ => if collapsed { 1. } else { 0. },
        };
        let chevron = Icon::Chevron.el(10., t.dim).when(fold > 0., |s| s.with_transformation(Transformation::rotate(radians(-std::f32::consts::FRAC_PI_2 * fold))));
        let toggle_key = pid.clone();
        let header = div()
            .id(SharedString::from(format!("group-{gi}")))
            .flex()
            .items_center()
            .gap(px(6.))
            .pt(px(8.))
            .pr(px(8.))
            .pb(px(2.))
            .pl(px(10.))
            .cursor_pointer()
            .tooltip(super::header::tip_keys(if collapsed { "Show this project's terminals" } else { "Fold this project" }, "keys.fold_project"))
            .on_click(cx.listener(move |m, _, _, cx| {
                if let Some(p) = toggle_key.clone() {
                    toggle_fold(m, p, cx);
                }
            }))
            .child(chevron)
            .child(caps_label(t, &g.name).flex_1().truncate())
            .when(n > 0, |d| {
                d.child(
                    div()
                        .min_w(px(18.))
                        .h(px(18.))
                        .px(px(5.))
                        .rounded(px(9.))
                        .bg(t.need)
                        .text_color(t.badge_fg)
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(n.to_string()),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("group-menu-{gi}")))
                    .relative()
                    .size(px(26.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(t.raised))
                    .when(menu_open, |d| d.bg(t.raised))
                    .tooltip(super::header::tip_keys("Project actions", "keys.project_menu"))
                    .on_click(cx.listener(move |m, _, _, cx| {
                        cx.stop_propagation();
                        let k = Menu::Project(menu_key.clone());
                        m.menu = if m.menu == k { Menu::None } else { k };
                        cx.notify();
                    }))
                    .child(Icon::Dots.el(14., t.dim))
                    .when(menu_open, |d| {
                        // ⌘T opens in the current project, so its keys belong on this menu only there
                        let keys = (m.current_project_id() == pid).then(|| m.key_label("keys.new_terminal")).filter(|k| !k.is_empty());
                        d.child(project_menu(t, pid.clone(), keys, cx))
                    }),
            );
        // Root terminals belong to no project: no heading, just rows.
        let mut group = div().flex().flex_col().mb(px(if compact { 2. } else { 8. })).when(g.project.is_some(), |d| d.child(header)).when(g.project.is_none(), |d| d.pt(px(4.)));
        // A folded group keeps only the selected terminal, so the open one never disappears:
        // the rows around it fold as separate runs.
        let key = pid.clone().unwrap_or_else(|| "root".into());
        let (mut run, mut ri) = (vec![], 0);
        for s in g.sessions {
            if m.selected.as_deref() == Some(&s.id) {
                group = group.children(fold_run(std::mem::take(&mut run), format!("{key}/{ri}"), fold, m)).child(row(m, s, &key, t, compact, cx));
                ri += 1;
            } else if fold < 1. {
                run.push(row(m, s, &key, t, compact, cx).into_any_element());
            }
        }
        group = group.children(fold_run(run, format!("{key}/{ri}"), fold, m));
        list = list.child(group);
    }
    if m.sessions.is_empty() && m.conn == crate::backend::ConnState::Connected {
        list = list
            .child(div().px(px(10.)).pt(px(8.)).child(open_project_button(m, t, "sidebar-open-project", cx)))
            .child(div().px(px(14.)).py(px(10.)).text_size(px(12.)).text_color(t.dim).child(format!("Or {} for a terminal at root.", m.key_label("keys.new_terminal"))));
    }

    let background = background_group(m, t, compact, window, cx);
    let footer = footer(m, t, cx);

    div()
        .w(px(WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(t.panel)
        .border_r_1()
        .border_color(t.line)
        .child(top)
        .when(need_n > 0, |d| d.child(need_btn))
        .child(list)
        .children(background)
        .child(footer)
        .into_any_element()
}

/// Key of the Background group in `fold_anim` / `fold_heights` (project ids start with `p_`).
const BACKGROUND: &str = "@background";
/// Unfolded Background rows scroll past this height, so the project list keeps the room.
const BACKGROUND_MAX_H: f32 = 220.;

/// Background terminals (`session.open` with `background`, `midna open --background`): a
/// dimmed group pinned between the project list and the footer (outside the list's scroll, so
/// it never falls below the fold), folded by default. Unfolded, its rows scroll on their own
/// past `BACKGROUND_MAX_H`. A folded group keeps only the selected terminal; clicking a row
/// previews it in the main pane like any other.
fn background_group(m: &MainWindow, t: &Theme, compact: bool, window: &mut Window, cx: &mut Context<MainWindow>) -> Option<Div> {
    let sessions = m.background_sessions();
    if sessions.is_empty() || m.background_hidden {
        return None;
    }
    let menu_open = m.menu == Menu::Background;
    let n = sessions.len();
    let need = sessions.iter().filter(|s| matches!(m.effective_state(s), StatusState::NeedsYou | StatusState::Failed)).count();
    let fold = match m.fold_anim.get(BACKGROUND) {
        Some(at) if at.elapsed().as_secs_f32() * 1000. < FOLD_MS => {
            window.request_animation_frame();
            let e = 1. - (1. - at.elapsed().as_secs_f32() * 1000. / FOLD_MS).powi(3);
            if m.background_open { 1. - e } else { e }
        }
        _ => if m.background_open { 0. } else { 1. },
    };
    let chevron = Icon::Chevron.el(10., t.dim).when(fold > 0., |s| s.with_transformation(Transformation::rotate(radians(-std::f32::consts::FRAC_PI_2 * fold))));
    let header = div()
        .id("group-background")
        .flex()
        .items_center()
        .gap(px(6.))
        .pt(px(8.))
        .pr(px(14.))
        .pb(px(2.))
        .pl(px(10.))
        .opacity(0.7)
        .cursor_pointer()
        .hover(|s| s.opacity(1.))
        .relative()
        .when(menu_open, |d| d.opacity(1.))
        .tooltip(super::header::tip(if m.background_open { "Fold background terminals (right-click for more)" } else { "Show background terminals (right-click for more)" }))
        .on_click(cx.listener(|m, _, _, cx| toggle_background(m, cx)))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|m, _, _, cx| {
                cx.stop_propagation();
                m.menu = if m.menu == Menu::Background { Menu::None } else { Menu::Background };
                cx.notify();
            }),
        )
        .child(chevron)
        .child(caps_label(t, "Background").flex_1())
        .when(need > 0, |d| d.child(div().size(px(7.)).rounded_full().bg(t.need)))
        .child(div().text_size(px(11.)).text_color(t.dim).child(n.to_string()))
        .when(menu_open, |d| d.child(background_menu(t, cx)));
    let mut group = div().id("background-rows").flex().flex_col().max_h(px(BACKGROUND_MAX_H)).overflow_y_scroll();
    let (mut run, mut ri) = (vec![], 0);
    let dim = |el: AnyElement, selected: bool| div().when(!selected, |d| d.opacity(0.55)).child(el).into_any_element();
    for s in sessions {
        if m.selected.as_deref() == Some(&s.id) {
            group = group.children(fold_run(std::mem::take(&mut run), format!("{BACKGROUND}/{ri}"), fold, m)).child(dim(row(m, s, BACKGROUND, t, compact, cx).into_any_element(), true));
            ri += 1;
        } else if fold < 1. {
            run.push(dim(row(m, s, BACKGROUND, t, compact, cx).into_any_element(), false));
        }
    }
    group = group.children(fold_run(run, format!("{BACKGROUND}/{ri}"), fold, m));
    Some(div().flex_none().flex().flex_col().pb(px(if compact { 2. } else { 6. })).border_t_1().border_color(t.line).child(header).child(group))
}

/// The Background heading's right-click menu. Opens upward: the heading sits above the footer.
fn background_menu(t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    deferred(
        anchored().anchor(Anchor::BottomLeft).snap_to_window_with_margin(px(8.)).child(
            menu_box(t).mb(px(4.)).child(menu_item(
                t,
                "background-hide",
                "Hide from sidebar",
                "show again from ⌘K",
                cx.listener(|m, _, _, cx| set_background_hidden(m, true, cx)),
            )),
        ),
    )
    .with_priority(1)
}

/// Leave the Background group out of the sidebar and rail, or bring it back (remembered in
/// app-state.json). The terminals keep running and stay in ⌘K and the needs-you count.
pub fn set_background_hidden(m: &mut MainWindow, hidden: bool, cx: &mut Context<MainWindow>) {
    m.background_hidden = hidden;
    m.menu = Menu::None;
    crate::ui::statusbar::save_state(m);
    if hidden {
        m.toast("Background terminals hidden from the sidebar. ⌘K \"Show background terminals\" brings them back.", cx);
    }
    cx.notify();
}

/// Fold or unfold the Background group (remembered in app-state.json).
pub fn toggle_background(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.background_open = !m.background_open;
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        m.fold_anim.remove(BACKGROUND);
    } else {
        m.fold_anim.insert(BACKGROUND.into(), std::time::Instant::now());
    }
    crate::ui::statusbar::save_state(m);
    cx.notify();
}

/// Traffic lights are drawn by AppKit (transparent titlebar); this strip is the drag area.
fn titlebar_strip() -> Stateful<Div> {
    div().id("titlebar-drag").h(px(44.)).flex_none().on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, window, _| {
        if ev.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    })
}

fn collapse_button(t: &Theme, collapsed: bool) -> Stateful<Div> {
    div()
        .id("sidebar-toggle")
        .size(px(28.))
        .flex_none()
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .tooltip(super::header::tip_keys(if collapsed { "Expand the sidebar" } else { "Collapse the sidebar" }, "keys.sidebar"))
        // don't start a window drag from the strip under it
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(|_, w, cx| w.dispatch_action(Box::new(ToggleSidebar), cx))
        .child(Icon::Sidebar.el(16., t.dim))
}

/// Collapse the sidebar to its rail, or expand it again (remembered in app-state.json).
pub fn toggle_collapsed(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.sidebar_collapsed = !m.sidebar_collapsed;
    crate::ui::statusbar::save_state(m);
    cx.notify();
}

/// The collapsed sidebar: expand button, a needs-you count, one status dot per terminal (groups
/// split by a rule; folded groups keep only the selected terminal), and Triggers / Rules / Settings.
fn rail(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let need_n = m.needs.len();
    let mut list = div().id("rail-sessions").flex().flex_col().items_center().gap(px(2.)).flex_1().min_h_0().py(px(4.)).overflow_y_scroll();
    let mut first = true;
    for g in m.groups() {
        let folded = g.project.is_some_and(|p| m.collapsed.contains(&p.id));
        let sessions: Vec<_> = g.sessions.into_iter().filter(|s| !folded || m.selected.as_deref() == Some(&s.id)).collect();
        if sessions.is_empty() {
            continue;
        }
        if !std::mem::take(&mut first) {
            list = list.child(div().w(px(28.)).h(px(1.)).my(px(5.)).flex_none().bg(t.line));
        }
        let project = if g.project.is_some() { g.name.as_str() } else { "" };
        for s in sessions {
            list = list.child(rail_row(m, s, project, t, cx));
        }
    }
    // Background terminals: dimmed after a rule, only while their group is unfolded (the
    // selected one always), and never while it is hidden.
    let bg: Vec<_> = m.background_sessions().into_iter().filter(|s| !m.background_hidden && (m.background_open || m.selected.as_deref() == Some(&s.id))).collect();
    if !bg.is_empty() {
        if !first {
            list = list.child(div().w(px(28.)).h(px(1.)).my(px(5.)).flex_none().bg(t.line));
        }
        for s in bg {
            let selected = m.selected.as_deref() == Some(&s.id);
            list = list.child(div().when(!selected, |d| d.opacity(0.55)).child(rail_row(m, s, "background", t, cx)));
        }
    }
    let btn = |id: &'static str, icon: Icon, label: &'static str, setting: &'static str, active: bool| {
        div()
            .id(id)
            .size(px(36.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .when(active, |d| d.bg(t.raised))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .tooltip(super::header::tip_keys(label, setting))
            .child(icon.el(16., if active { t.fg } else { t.dim }))
    };
    div()
        .w(px(RAIL_WIDTH))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .bg(t.panel)
        .border_r_1()
        .border_color(t.line)
        .child(titlebar_strip())
        .child(div().flex().justify_center().pb(px(6.)).child(collapse_button(t, true)))
        .when(need_n > 0, |d| {
            d.child(
                div().flex().justify_center().pb(px(6.)).child(
                    div()
                        .id("rail-need-you")
                        .min_w(px(28.))
                        .h(px(24.))
                        .px(px(7.))
                        .rounded(px(12.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .bg(t.need)
                        .text_color(t.badge_fg)
                        .text_size(px(12.))
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .tooltip(super::header::tip_keys(format!("{need_n} need you"), "keys.needs_you"))
                        .on_click(|_, w, cx| w.dispatch_action(Box::new(OpenNeedsYou), cx))
                        .child(need_n.to_string()),
                ),
            )
        })
        .child(list)
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(2.))
                .py(px(8.))
                .border_t_1()
                .border_color(t.line)
                .child(btn("rail-insights", Icon::Screen, "Insights", "keys.insights", m.screen == Screen::Insights).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Insights, w, cx))))
                .child(btn("rail-triggers", Icon::Triggers, "Triggers", "keys.triggers", m.screen == Screen::Triggers).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Triggers, w, cx))))
                .child(btn("rail-rules", Icon::Rules, "Rules", "keys.rules", m.screen == Screen::Rules).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Rules, w, cx))))
                .child(btn("rail-settings", Icon::Settings, "Settings", "keys.settings", false).on_click(cx.listener(|m, _, _, cx| crate::ui::settings::open(m.backend.clone(), cx)))),
        )
}

/// A terminal on the rail: its agent icon with the status dot in the corner; the name (and
/// project) in the tooltip.
fn rail_row(m: &MainWindow, s: &Session, project: &str, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let selected = m.selected.as_deref() == Some(&s.id);
    let marked = !selected && m.marked.len() > 1 && m.is_marked(&s.id);
    let id = s.id.clone();
    let name = crate::ui::rename::shown_name(m, &s.id, &s.name, cx);
    let tip = if project.is_empty() { name.to_string() } else { format!("{name} · {project}") };
    div()
        .id(SharedString::from(format!("rail-{}", s.id)))
        .relative()
        .size(px(40.))
        .flex_none()
        .rounded(px(8.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(selected, |d| d.bg(t.raised))
        .when(marked, |d| d.bg(t.accent_soft))
        .when(!selected && !marked, |d| d.hover(|st| st.bg(t.raised.opacity(0.5))))
        .map(|d| crate::annotate::row_drop_target(d, m, &s.id, t, cx))
        .tooltip(super::header::tip(tip))
        .on_click(cx.listener(move |m, ev: &ClickEvent, w, cx| {
            if !click_marks(m, &id, ev, w, cx) {
                m.select_only(id.clone(), w, cx);
            }
        }))
        .when(selected, |d| d.child(div().absolute().left(px(-18.)).top(px(8.)).bottom(px(8.)).w(px(2.)).bg(t.accent)))
        .child(super::terminal_icon(m, t, s, 16., if selected { t.fg } else { t.dim }))
        .child(div().absolute().top(px(5.)).right(px(5.)).child(super::terminal_dot(m, t, s, 8.)))
}

/// ⌘-click toggles `id` in the selection, ⌘⇧-click selects the range to it. False for a
/// click without ⌘.
fn click_marks(m: &mut MainWindow, id: &str, ev: &ClickEvent, w: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let md = ev.modifiers();
    match (md.platform, md.shift) {
        (true, true) => m.mark_range(id.to_string(), w, cx),
        (true, false) => m.toggle_mark(id.to_string(), w, cx),
        _ => return false,
    }
    true
}

/// A run of rows under a group heading, clipped to `1 - fold` of its natural height (measured
/// every prepaint, so a fold starts from the real height). Gone once fully folded.
fn fold_run(rows: Vec<AnyElement>, key: String, fold: f32, m: &MainWindow) -> Option<Div> {
    if rows.is_empty() || fold >= 1. {
        return None;
    }
    let full = m.fold_heights.borrow().get(&key).copied().unwrap_or(0.);
    let heights = m.fold_heights.clone();
    Some(
        div()
            .flex()
            .flex_col()
            .overflow_hidden()
            .when(fold > 0., |d| d.h(px(full * (1. - fold))))
            .on_children_prepainted(move |b, _, _| {
                if let Some(b) = b.first() {
                    heights.borrow_mut().insert(key.clone(), f32::from(b.size.height));
                }
            })
            .child(div().flex_none().flex().flex_col().children(rows)),
    )
}

/// Moves the dragged terminal to `target`'s place once the cursor passes `target`'s middle
/// (from above or below), so rows of different heights don't swap back and forth. Dragging
/// a selected row (⌘-click / ⌘⇧-click) moves its group's selected rows with it, as one block.
fn drag_over(m: &mut MainWindow, dragged: &str, target: &str, y: Pixels, bounds: Bounds<Pixels>, cx: &mut Context<MainWindow>) {
    let ids: Vec<String> = m.ordered_sessions().iter().map(|s| s.id.clone()).collect();
    let moving: Vec<String> = if m.marked.len() > 1 && m.is_marked(dragged) {
        let group = m.groups().into_iter().find(|g| g.sessions.iter().any(|s| s.id == dragged)).map(|g| g.sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>()).unwrap_or_default();
        group.into_iter().filter(|id| m.is_marked(id)).collect()
    } else {
        vec![dragged.to_string()]
    };
    if moving.iter().any(|i| i == target) {
        return;
    }
    let (Some(from), Some(to)) = (ids.iter().position(|i| i == dragged), ids.iter().position(|i| i == target)) else { return };
    let mid = bounds.origin.y + bounds.size.height / 2.;
    if (from < to && y < mid) || (from > to && y > mid) {
        return;
    }
    let mut ids: Vec<String> = ids.into_iter().filter(|i| !moving.contains(i)).collect();
    let Some(at) = ids.iter().position(|i| i == target) else { return };
    let at = if from < to { at + 1 } else { at };
    ids.splice(at..at, moving);
    m.order = ids;
    crate::ui::statusbar::save_state(m);
    cx.notify();
}

fn row(m: &MainWindow, s: &Session, group: &str, t: &Theme, compact: bool, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let selected = m.selected.as_deref() == Some(&s.id);
    // Part of a multi-selection but not the terminal on screen.
    let marked = !selected && m.marked.len() > 1 && m.is_marked(&s.id);
    let id = s.id.clone();
    let (drag, group, target) = (DraggedRow { id: s.id.clone(), group: group.to_string() }, group.to_string(), s.id.clone());
    let state = m.effective_state(s);
    let attention = matches!(state, StatusState::NeedsYou | StatusState::Failed);
    let custom = super::status_label(m, s);
    // A trigger's custom status (or a ui.status.looks label) replaces the built-in second
    // line: its label in its color.
    let custom_line = custom.as_ref().filter(|_| !compact).map(|c| {
        let since = c.since.as_deref().map(since_short).unwrap_or_default();
        let text = if since.is_empty() { c.label.clone() } else { format!("{} · {since}", c.label) };
        (text, t.status_color(&c.color))
    });
    let custom_icon = custom.as_ref().and_then(|c| c.icon.as_deref()).and_then(Icon::from_name);
    let line2 = custom_line.or_else(|| (!compact && attention).then(|| {
        let need = m.need_for_session(&s.id);
        let text = s
            .status
            .reason
            .clone()
            .filter(|r| !r.is_empty())
            .or_else(|| need.map(|n| n.title.clone()).filter(|_| state == StatusState::NeedsYou))
            .or_else(|| s.status.exit_code.map(|c| format!("exit {c}")))
            .unwrap_or_else(|| if s.status.state == StatusState::Failed { "failed".into() } else { "needs you".into() });
        let since = need.filter(|_| state == StatusState::NeedsYou).map(|n| since_short(&n.created_at)).or_else(|| s.status.since.as_deref().map(since_short)).unwrap_or_default();
        let text = if since.is_empty() { text } else { format!("{text} · {since}") };
        (text, if state == StatusState::Failed { t.err } else { t.need })
    }));
    let segs = m.row_segments.get(&s.id).cloned().unwrap_or_default();
    let pad_y = if compact { 5. } else { 8. };

    div()
        .id(SharedString::from(format!("row-{}", s.id)))
        .relative()
        .flex()
        .flex_col()
        .gap(px(2.))
        .w_full()
        .px(px(14.))
        .py(px(pad_y))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.raised))
        .when(marked, |d| d.bg(t.accent_soft))
        .when(!selected && !marked, |d| d.hover(|st| st.bg(t.raised.opacity(0.5))))
        .map(|d| crate::annotate::row_drop_target(d, m, &s.id, t, cx))
        .on_drag(drag, |_, _, _, cx| cx.new(|_| NoGhost))
        .on_drag_move(cx.listener(move |m, ev: &DragMoveEvent<DraggedRow>, _, cx| {
            let d = ev.drag(cx);
            let y = ev.event.position.y;
            if d.group == group && d.id != target && ev.bounds.top() <= y && y < ev.bounds.bottom() {
                let dragged = d.id.clone();
                drag_over(m, &dragged, &target, y, ev.bounds, cx);
            }
        }))
        .on_click(cx.listener(move |m, ev: &ClickEvent, w, cx| {
            if click_marks(m, &id, ev, w, cx) {
            } else if ev.click_count() == 2 {
                crate::ui::rename::start(m, &id, crate::ui::rename::At::Sidebar, w, cx);
            } else {
                m.select_only(id.clone(), w, cx);
            }
        }))
        .when(selected, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .child(div().w(px(12.)).flex_none().flex().items_center().child(super::terminal_dot(m, t, s, 8.)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .flex_1()
                        .min_w_0()
                        .child(match crate::ui::rename::field(m, &s.id, crate::ui::rename::At::Sidebar, t, 13., cx) {
                            Some(f) => f,
                            None => div().font_weight(FontWeight::MEDIUM).truncate().child(crate::ui::rename::shown_name(m, &s.id, &s.name, cx)).into_any_element(),
                        })
                        .when(!segs.is_empty(), |d| d.child(div().ml_auto().flex_none().child(segments(&segs, t, 10.5, cx)))),
                )
                .when(crate::ui::popout::is_popped(&s.id, cx), |d| d.child(Icon::PopOut.el(12., t.dim)))
                .child(super::terminal_icon(m, t, s, 14., t.dim)),
        )
        .when_some(line2, |d, (text, color)| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .pl(px(21.))
                    .text_size(px(12.))
                    .text_color(color)
                    .children(custom_icon.filter(|_| custom.is_some()).map(|i| i.el(12., color)))
                    .child(div().min_w_0().truncate().child(text)),
            )
        })
}

/// Script segments (`script.run`): gap between segments, a single space when `join`.
/// `icon` is `dot` or any `Icon::from_name` name (branch, worktree, pr, check, cross, …).
pub fn segments(segs: &[Segment], t: &Theme, mono_size: f32, _cx: &mut Context<MainWindow>) -> Div {
    let mut out = div().flex().items_center().gap(px(14.)).whitespace_nowrap();
    let mut cur: Option<Div> = None;
    for (i, seg) in segs.iter().enumerate() {
        let color = t.tone(seg.tone);
        let mut el = div().id(("segment", i)).flex().items_center().gap(px(6.)).text_color(color);
        if seg.mono {
            el = el.font_family(t.mono_font.clone()).text_size(px(mono_size));
        }
        match seg.icon.as_deref().map(|n| (n, Icon::from_name(n))) {
            Some(("dot", _)) => el = el.child(div().size(px(7.)).rounded_full().bg(color)),
            Some((_, Some(icon))) => el = el.child(icon.el(13., color)),
            _ => {}
        }
        el = el.when(!seg.text.is_empty(), |d| d.child(seg.text.clone()));
        if let Some(url) = seg.link.clone() {
            el = el.cursor_pointer().on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&url));
        }
        if let Some(tip) = seg.tooltip.clone().filter(|t| !t.is_empty()) {
            el = el.tooltip(super::header::tip(tip));
        }
        cur = Some(match cur.take() {
            Some(group) if seg.join => group.child(el),
            Some(group) => {
                out = out.child(group);
                div().flex().items_center().gap(px(4.)).child(el)
            }
            None => div().flex().items_center().gap(px(4.)).child(el),
        });
    }
    if let Some(g) = cur {
        out = out.child(g);
    }
    out
}

/// "Open project…": folder picker → `project.add` → a terminal there. Shown while there are
/// no projects, in the sidebar and the empty pane.
pub fn open_project_button(m: &MainWindow, t: &Theme, id: &'static str, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(34.))
        .px(px(12.))
        .rounded(px(8.))
        .bg(t.accent)
        .text_color(t.accent_fg)
        .font_weight(FontWeight::BOLD)
        .cursor_pointer()
        .hover(|s| s.opacity(0.9))
        .on_click(cx.listener(|m, _, _, cx| m.pick_project(cx)))
        .child(Icon::Project.el(14., t.accent_fg))
        .child(div().flex_1().child("Open project…"))
        .child(div().font_family(t.mono_font.clone()).text_size(px(11.)).font_weight(FontWeight::NORMAL).child(m.key_label("keys.open_project")))
}

fn project_menu(t: &Theme, pid: Option<String>, terminal_keys: Option<String>, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let p1 = pid.clone();
    let p2 = pid.clone();
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            menu_box(t)
                .mt(px(30.))
                .child(menu_item(
                    t,
                    "new-term",
                    "New terminal here",
                    "",
                    cx.listener(move |m, _, _, cx| {
                        m.menu = Menu::None;
                        m.rpc("session.open", serde_json::json!({"kind": "shell", "project_id": p1}), cx, |m, _, _, cx| m.request_refresh(crate::app::refresh::SESSIONS, cx));
                    }),
                )
                .children(terminal_keys.map(|k| super::header::key_chip(t, k.into()))))
                .child(menu_item(
                    t,
                    "new-agent",
                    "New agent here",
                    "",
                    cx.listener(move |m, _, _, cx| {
                        m.menu = Menu::None;
                        m.rpc("session.open", serde_json::json!({"kind": "agent", "agent": "claude", "project_id": p2}), cx, |m, _, _, cx| {
                            m.request_refresh(crate::app::refresh::SESSIONS, cx)
                        });
                    }),
                )),
        ),
    )
    .with_priority(1)
}

/// Fold or unfold a project's group (animated unless Reduce Motion is on).
pub fn toggle_fold(m: &mut MainWindow, project: String, cx: &mut Context<MainWindow>) {
    if !m.collapsed.remove(&project) {
        m.collapsed.insert(project.clone());
    }
    // Re-read each time: macOS posts no change we can follow, and the read is cheap.
    gpui_kit::base::apply_system_reduce_motion(cx);
    if cx.reduce_motion() {
        m.fold_anim.remove(&project);
    } else {
        m.fold_anim.insert(project, std::time::Instant::now());
    }
    crate::ui::statusbar::save_state(m);
    cx.notify();
}

pub fn menu_box(t: &Theme) -> Div {
    div()
        .min_w(px(220.))
        .p(px(6.))
        .rounded(px(10.))
        .border_1()
        .border_color(t.line)
        .bg(t.raised)
        .text_color(t.fg)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
        .flex()
        .flex_col()
}

pub fn menu_item(t: &Theme, id: &str, label: &str, hint: &str, on: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Stateful<Div> {
    div()
        .id(SharedString::from(id.to_string()))
        .flex()
        .items_center()
        .gap(px(10.))
        .w_full()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|s| s.bg(t.accent_soft))
        .on_click(on)
        .child(div().flex_1().child(label.to_string()))
        .when(!hint.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(t.dim).child(hint.to_string())))
}

fn footer(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let stat = |n: String, label: &str| {
        div()
            .flex()
            .flex_col()
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(n))
            .child(div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child(label.to_string()))
    };
    let today = &m.today;
    let card = div()
        .id("today")
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(12.))
        .py(px(10.))
        .rounded(px(9.))
        .border_1()
        .border_color(if m.screen == Screen::Insights { t.accent } else { t.line })
        .bg(t.raised)
        .cursor_pointer()
        .tooltip(super::header::tip_keys("Insights", "keys.insights"))
        .on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Insights, w, cx)))
        .child(div().flex().items_baseline().gap(px(8.)).child(caps_label(t, "Today").flex_1()).child(div().text_size(px(11.)).text_color(t.dim).child("Insights ›")))
        .child(
            div()
                .flex()
                .justify_between()
                .gap(px(8.))
                .child(stat(today.turns.to_string(), "agent turns"))
                .child(stat(today.messages.to_string(), "messages sent"))
                .child(stat(format!("${:.2}", today.spend_usd), "spent")),
        );
    let btn = |id: &'static str, icon: Icon, label: &'static str, setting: &'static str, active: bool| {
        div()
            .id(id)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(3.))
            .flex_1()
            .pt(px(7.))
            .pb(px(5.))
            .rounded(px(7.))
            .text_size(px(10.5))
            .text_color(if active { t.fg } else { t.dim })
            .when(active, |d| d.bg(t.raised))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .tooltip(super::header::tip_keys(label, setting))
            .child(icon.el(16., if active { t.fg } else { t.dim }))
            .child(label)
    };
    div().flex().flex_col().gap(px(8.)).p(px(10.)).border_t_1().border_color(t.line).children(super::onboarding::checklist(m, t, cx)).child(card).child(
        div()
            .flex()
            .gap(px(4.))
            .child(btn("btn-triggers", Icon::Triggers, "Triggers", "keys.triggers", m.screen == Screen::Triggers).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Triggers, w, cx))))
            .child(btn("btn-rules", Icon::Rules, "Rules", "keys.rules", m.screen == Screen::Rules).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Rules, w, cx))))
            .child(btn("btn-settings", Icon::Settings, "Settings", "keys.settings", false).on_click(cx.listener(|m, _, _, cx| crate::ui::settings::open(m.backend.clone(), cx)))),
    )
}
