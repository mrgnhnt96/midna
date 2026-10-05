//! Left sidebar (264px): traffic-light strip, "N need you", project groups with terminal
//! rows (click a heading to fold it; remembered in app-state.json), and the footer (Today card + Triggers / Rules / Settings).
use super::{caps_label, session_dot};
use crate::actions::*;
use crate::app::{MainWindow, Menu, Screen};
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub const WIDTH: f32 = 264.;
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

pub fn render(m: &MainWindow, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let compact = m.compact();
    let need_n = m.needs.len();
    let jump_key = m.key_label("keys.next_needs_you");

    // Traffic lights are drawn by AppKit (transparent titlebar); this strip is the drag area.
    let top = div().id("titlebar-drag").h(px(44.)).flex_none().on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, window, _| {
        if ev.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            window.start_window_move();
        }
    });

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
        .child(footer)
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
/// (from above or below), so rows of different heights don't swap back and forth.
fn drag_over(m: &mut MainWindow, dragged: &str, target: &str, y: Pixels, bounds: Bounds<Pixels>, cx: &mut Context<MainWindow>) {
    let mut ids: Vec<String> = m.ordered_sessions().iter().map(|s| s.id.clone()).collect();
    let (Some(from), Some(to)) = (ids.iter().position(|i| i == dragged), ids.iter().position(|i| i == target)) else { return };
    let mid = bounds.origin.y + bounds.size.height / 2.;
    if (from < to && y < mid) || (from > to && y > mid) {
        return;
    }
    let id = ids.remove(from);
    ids.insert(to, id);
    m.order = ids;
    crate::ui::statusbar::save_state(m);
    cx.notify();
}

fn row(m: &MainWindow, s: &Session, group: &str, t: &Theme, compact: bool, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let selected = m.selected.as_deref() == Some(&s.id);
    let id = s.id.clone();
    let (drag, group, target) = (DraggedRow { id: s.id.clone(), group: group.to_string() }, group.to_string(), s.id.clone());
    let state = m.effective_state(s);
    let attention = matches!(state, StatusState::NeedsYou | StatusState::Failed);
    let custom = s.custom_status.as_ref();
    // A trigger's custom status replaces the built-in second line: its label in its color.
    let custom_line = custom.filter(|_| !compact).map(|c| {
        let since = c.since.as_deref().map(since_short).unwrap_or_default();
        let text = if since.is_empty() { c.label.clone() } else { format!("{} · {since}", c.label) };
        (text, t.status_color(&c.color))
    });
    let custom_icon = custom.and_then(|c| c.icon.as_deref()).and_then(Icon::from_name);
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
        .when(!selected, |d| d.hover(|st| st.bg(t.raised.opacity(0.5))))
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
            if ev.click_count() == 2 {
                crate::ui::rename::start(m, &id, crate::ui::rename::At::Sidebar, w, cx);
            } else {
                m.select(id.clone(), w, cx);
            }
        }))
        .when(selected, |d| d.child(div().absolute().left_0().top_0().bottom_0().w(px(2.)).bg(t.accent)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(9.))
                .child(div().w(px(12.)).flex_none().flex().items_center().child(session_dot(t, state, custom, 8.)))
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
                .child(Icon::from_glyph(s.glyph()).el(14., t.dim)),
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
pub fn segments(segs: &[Segment], t: &Theme, mono_size: f32, _cx: &mut Context<MainWindow>) -> Div {
    let mut out = div().flex().items_center().gap(px(14.)).whitespace_nowrap();
    let mut cur: Option<Div> = None;
    for seg in segs {
        let color = t.tone(seg.tone);
        let mut el = div().flex().items_center().gap(px(6.)).text_color(color);
        if seg.mono {
            el = el.font_family(t.mono_font.clone()).text_size(px(mono_size));
        }
        match seg.icon.as_deref() {
            Some("branch") => el = el.child(Icon::Branch.el(13., color)),
            Some("pr") => el = el.child(Icon::Pr.el(13., color)),
            Some("dot") => el = el.child(div().size(px(7.)).rounded_full().bg(color)),
            _ => {}
        }
        el = el.child(seg.text.clone());
        if let Some(url) = seg.link.clone() {
            el = el.cursor_pointer().on_mouse_down(MouseButton::Left, move |_, _, cx| cx.open_url(&url));
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
    div().flex().flex_col().gap(px(8.)).p(px(10.)).border_t_1().border_color(t.line).child(card).child(
        div()
            .flex()
            .gap(px(4.))
            .child(btn("btn-triggers", Icon::Triggers, "Triggers", "keys.triggers", m.screen == Screen::Triggers).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Triggers, w, cx))))
            .child(btn("btn-rules", Icon::Rules, "Rules", "keys.rules", m.screen == Screen::Rules).on_click(cx.listener(|m, _, w, cx| m.set_screen(Screen::Rules, w, cx))))
            .child(btn("btn-settings", Icon::Settings, "Settings", "keys.settings", false).on_click(cx.listener(|m, _, _, cx| crate::ui::settings::open(m.backend.clone(), cx)))),
    )
}
