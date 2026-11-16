//! The Mac switcher: the sidebar's name plate is a button (⌃⌘M) whose menu lists this Mac and
//! my paired Macs with how each stands (`macs`). Choosing one opens its window or brings it
//! forward; one that can't be driven is greyed, its line says why. Pair another Mac… opens
//! Settings › Nearby. ↑↓ (Home, End) move among the ones that can be chosen, ↩ chooses, esc
//! closes.
use crate::actions::{Kind, overlay, overlay_context};
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::macs::Entry;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub struct MacMenu {
    pub focus: FocusHandle,
    /// The highlighted row: a Mac's index, or the Macs' count for Pair another Mac….
    sel: usize,
}

impl MacMenu {
    pub fn new(cx: &mut App) -> MacMenu {
        MacMenu { focus: cx.focus_handle(), sel: 0 }
    }
}

/// The plate shows (`sidebar::name_plate`): the sidebar is open and the window shows a name.
/// Otherwise the menu opens at the window's top left.
pub fn plate_shown(m: &MainWindow) -> bool {
    !m.sidebar_collapsed && m.shown_name().is_some()
}

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.menu == Menu::Macs {
        close(m, window, cx);
    } else {
        open(m, None, window, cx);
    }
}

/// Open the menu with `sel` highlighted (None: this window's Mac).
pub fn open(m: &mut MainWindow, sel: Option<usize>, window: &mut Window, cx: &mut Context<MainWindow>) {
    crate::macs::refresh(crate::backend::local(&m.backend), cx);
    let list = crate::macs::list(cx);
    m.menu = Menu::Macs;
    m.mac_menu.sel = sel.or_else(|| crate::macs::index_of(&list, &m.mac_id())).unwrap_or(0);
    m.mac_menu.focus.focus(window, cx);
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.focus_terminal(window, cx);
    cx.notify();
}

/// The rows that can be chosen now (the last, `list.len()`, is Pair another Mac…).
fn choosable(list: &[Entry], cx: &mut App) -> Vec<usize> {
    let (open, _) = crate::windows::mac_windows(cx);
    list.iter().enumerate().filter(|(_, e)| e.enabled(open.contains(&e.id()))).map(|(i, _)| i).chain([list.len()]).collect()
}

fn pick(m: &mut MainWindow, i: usize, window: &mut Window, cx: &mut Context<MainWindow>) {
    let list = crate::macs::list(cx);
    if !choosable(&list, cx).contains(&i) {
        return;
    }
    close(m, window, cx);
    let local = crate::backend::local(&m.backend);
    if i == list.len() {
        crate::settings_window::open_nearby(local, cx);
        return;
    }
    // After this window's update: it may bring another window forward.
    cx.defer(move |cx| crate::macs::switch(&local, i, cx));
}

/// The row `step` choosable rows from `cur` (wrapping); `step` 0 is the first, `isize::MAX` the last.
fn step(rows: &[usize], cur: usize, step: isize) -> usize {
    let n = rows.len() as isize;
    match step {
        0 => rows[0],
        isize::MAX => rows[rows.len() - 1],
        _ => {
            let at = rows.iter().position(|r| *r >= cur).unwrap_or(0) as isize;
            let at = if rows.contains(&cur) || step < 0 { at } else { at - 1 };
            rows[(at + step).rem_euclid(n) as usize]
        }
    }
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let list = crate::macs::list(cx);
    let rows = choosable(&list, cx);
    let k = ev.keystroke.key.as_str();
    match k {
        "up" | "down" | "home" | "end" => {
            let by = match k {
                "up" => -1,
                "down" => 1,
                "home" => 0,
                _ => isize::MAX,
            };
            m.mac_menu.sel = step(&rows, m.mac_menu.sel, by);
        }
        "enter" => {
            let sel = m.mac_menu.sel;
            pick(m, sel, window, cx);
        }
        "escape" => close(m, window, cx),
        "m" if ev.keystroke.modifiers.control && ev.keystroke.modifiers.platform => close(m, window, cx),
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

/// The name plate: this window's Mac, and a chevron; it opens the menu.
pub fn plate(m: &MainWindow, name: String, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let open = m.menu == Menu::Macs;
    div()
        .id("mac-plate")
        .relative()
        .flex()
        .items_center()
        .gap(px(6.))
        .min_w_0()
        .h(px(26.))
        .px(px(8.))
        .rounded(px(6.))
        .border_1()
        .border_color(t.line)
        .text_size(px(12.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(if open { t.fg } else { t.dim })
        .when(open, |d| d.bg(t.raised))
        .hover(|s| s.bg(t.raised).text_color(t.fg))
        .whitespace_nowrap()
        .cursor_pointer()
        .tooltip(super::header::tip_fixed("Switch Mac", "⌃⌘M"))
        .on_click(cx.listener(|m, _, w, cx| {
            cx.stop_propagation();
            toggle(m, w, cx);
        }))
        .child(Icon::Mac.el(13., t.dim))
        .child(div().min_w_0().truncate().child(name))
        .child(Icon::Chevron.el(9., t.dim))
        .when(open, |d| d.child(menu(m, t, Anchor::TopLeft, px(30.), cx)))
}

/// The menu where the plate isn't (sidebar collapsed, or no name shown): the window's top left.
pub fn loose(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    (m.menu == Menu::Macs && !plate_shown(m)).then(|| div().absolute().top(px(8.)).left(px(78.)).child(menu(m, t, Anchor::TopLeft, px(28.), cx)).into_any_element())
}

fn menu(m: &MainWindow, t: &Theme, corner: Anchor, below: Pixels, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let list = crate::macs::list(cx);
    let (open, _) = crate::windows::mac_windows(cx);
    let here = crate::macs::index_of(&list, &m.mac_id());
    let sel = m.mac_menu.sel;
    let mut rows = div().flex().flex_col();
    for (i, e) in list.iter().enumerate() {
        let enabled = e.enabled(open.contains(&e.id()));
        rows = rows.child(
            row(t, ("mac-row", i), i == sel && enabled, enabled, cx, move |m, w, cx| pick(m, i, w, cx))
                .child(div().w(px(12.)).flex_none().when(here == Some(i), |d| d.child(Icon::Check.el(11., t.accent))))
                .child(Icon::Mac.el(14., if e.mac.is_some() && enabled { t.fg } else { t.dim }))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .child(div().truncate().child(e.name.clone()))
                        .child(div().text_size(px(11.)).font_weight(FontWeight::NORMAL).text_color(t.dim).whitespace_normal().child(e.detail())),
                )
                .when(i < 9 && enabled, |d| d.child(div().flex_none().pl(px(16.)).text_size(px(11.)).font_family(t.mono_font.clone()).text_color(t.dim).child(format!("⌃⌘{}", i + 1)))),
        );
    }
    let pair = list.len();
    deferred(
        anchored().anchor(corner).snap_to_window_with_margin(px(8.)).child(
            div()
                .id("mac-menu")
                .key_context(overlay_context(Kind::Popover, overlay::MACS))
                .track_focus(&m.mac_menu.focus)
                .on_key_down(cx.listener(on_key))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .occlude()
                .mt(below)
                .w(px(260.))
                .p(px(4.))
                .flex()
                .flex_col()
                .rounded(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_color(t.fg)
                .text_size(px(13.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(10.)), blur_radius: px(30.), spread_radius: px(0.), inset: false }])
                .child(rows)
                .child(div().h(px(1.)).my(px(4.)).bg(t.line))
                .child(row(t, "mac-pair", sel == pair, true, cx, move |m, w, cx| pick(m, pair, w, cx)).child(div().w(px(12.)).flex_none()).child("Pair another Mac…")),
        ),
    )
    .with_priority(1)
}

fn row(
    t: &Theme,
    id: impl Into<ElementId>,
    selected: bool,
    enabled: bool,
    cx: &mut Context<MainWindow>,
    on: impl Fn(&mut MainWindow, &mut Window, &mut Context<MainWindow>) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .min_h(px(26.))
        .px(px(8.))
        .py(px(3.))
        .rounded(px(5.))
        .when(selected, |d| d.bg(t.accent_soft))
        .when(!enabled, |d| d.text_color(t.dim).opacity(0.7))
        .when(enabled, |d| d.cursor_pointer().hover(|s| s.bg(t.accent_soft)).on_click(cx.listener(move |m, _, w, cx| on(m, w, cx))))
}

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn arrows_move_among_the_rows_that_can_be_chosen() {
        // Rows 0, 1 and 3 can be chosen (2 can't); 4 is Pair another Mac….
        let rows = [0, 1, 3, 4];
        assert_eq!(step(&rows, 1, 1), 3, "skips the greyed one");
        assert_eq!(step(&rows, 3, -1), 1);
        assert_eq!(step(&rows, 4, 1), 0, "wraps");
        assert_eq!(step(&rows, 0, -1), 4, "wraps back");
        assert_eq!(step(&rows, 2, 1), 3, "from a greyed row (opened on one to say why)");
        assert_eq!(step(&rows, 2, -1), 1);
        assert_eq!(step(&rows, 3, 0), 0, "home");
        assert_eq!(step(&rows, 0, isize::MAX), 4, "end");
    }
}
