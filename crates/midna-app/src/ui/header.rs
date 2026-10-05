//! Terminal header: status dot, name, agent icon, script segments (`script.run` slot
//! `header`, default `github`), and the right toolbar.
use super::sidebar::{menu_box, menu_item, segments};
use super::{custom_status_label, session_dot};
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub fn render(m: &MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let Some(s) = m.selected_session() else {
        return div().h(px(44.)).flex_none().border_b_1().border_color(t.line).into_any_element();
    };
    let segs = m.header_segments.get(&s.id).cloned().unwrap_or_default();
    let tool = |id: &'static str, icon: Icon, text: &'static str, setting: &'static str| {
        div()
            .id(id)
            .size(px(32.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|st| st.bg(t.raised))
            .tooltip(tip_keys(text, setting))
            .child(icon.el(16., t.dim))
    };
    let more_open = m.menu == Menu::More;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(14.))
        .min_h(px(44.))
        .pl(px(18.))
        .pr(px(10.))
        .border_b_1()
        .border_color(t.line)
        .child(session_dot(t, m.effective_state(s), s.custom_status.as_ref(), 9.))
        .child(match crate::ui::rename::field(m, &s.id, crate::ui::rename::At::Header, t, 15., cx) {
            Some(f) => f,
            None => {
                let sid = s.id.clone();
                div()
                    .id("header-name")
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .whitespace_nowrap()
                    .tooltip(tip_keys("Double-click to rename", "keys.rename"))
                    .on_click(cx.listener(move |m, ev: &ClickEvent, w, cx| {
                        if ev.click_count() == 2 {
                            crate::ui::rename::start(m, &sid, crate::ui::rename::At::Header, w, cx);
                        }
                    }))
                    .child(crate::ui::rename::shown_name(m, &s.id, &s.name, cx))
                    .into_any_element()
            }
        })
        .child(Icon::from_glyph(s.glyph()).el(14., t.dim))
        .when_some(s.custom_status.as_ref(), |d, c| {
            let tip = c.detail.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| "Set by a trigger".into());
            d.child(div().id("header-custom-status").tooltip(tip_fixed(tip, "")).child(custom_status_label(t, c, 11.5)))
        })
        .when(s.notify_muted(), |d| {
            d.child(
                div()
                    .id("header-muted")
                    .tooltip(tip_keys("Notifications muted for this terminal", "keys.mute"))
                    .child(Icon::BellOff.el(13., t.dim)),
            )
        })
        .when(!segs.is_empty(), |d| d.child(div().w(px(1.)).h(px(18.)).flex_none().bg(t.line)).child(div().min_w_0().overflow_hidden().child(segments(&segs, t, 12., cx))))
        .child(
            // empty header space drags the window like a titlebar
            div().id("header-drag").flex_1().h(px(44.)).on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, window, _| {
                if ev.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            }),
        )
        .child(
            div()
                .flex()
                .gap(px(2.))
                .children(crate::ui::subagents::button(m, t, cx))
                .children(crate::ui::links::button(m, t, cx))
                .child(tool("tb-image", Icon::Image, "Add image", "keys.add_image").on_click(cx.listener(|m, _, window, cx| crate::annotate::open(m, window, cx))))
                .child(
                    tool("tb-split", Icon::Split, if m.split.is_some() { "Close split" } else { "Split" }, "keys.split")
                        .when(m.split.is_some(), |d| d.bg(t.raised))
                        .on_click(cx.listener(|m, _, window, cx| crate::ui::split::toggle(m, window, cx))),
                )
                .child(tool("tb-popout", Icon::PopOut, "Pop out", "keys.pop_out").on_click(cx.listener(|m, _, w, cx| {
                    if let Some(id) = m.selected.clone() {
                        crate::ui::popout::open(m, id, w, cx);
                    }
                })))
                .child(tool("tb-restart", Icon::Restart, "Restart", "keys.restart").on_click(cx.listener(|m, _, _, cx| m.restart_selected(cx))))
                .child(
                    div()
                        .relative()
                        .child(tool("tb-more", Icon::Dots, "More", "keys.terminal_menu").when(more_open, |d| d.bg(t.raised)).on_click(cx.listener(|m, _, _, cx| {
                            m.menu = if m.menu == Menu::More { Menu::None } else { Menu::More };
                            cx.notify();
                        })))
                        .when(more_open, |d| d.child(more_menu(m, t, cx))),
                ),
        )
        .into_any_element()
}

fn more_menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let id = m.selected.clone().unwrap_or_default();
    let keys = |setting: &str| {
        let k = m.key_label(setting);
        (!k.is_empty()).then(|| key_chip(t, k.into()))
    };
    let muted = m.selected_session().is_some_and(|s| s.notify_muted());
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            menu_box(t)
                .mt(px(36.))
                .child(menu_item(
                    t,
                    "more-copy",
                    "Copy session id",
                    &id,
                    cx.listener(|m, _, _, cx| m.copy_session_id(cx)),
                )
                .children(keys("keys.copy_session_id")))
                .child(menu_item(
                    t,
                    "more-mute",
                    if muted { "Unmute notifications" } else { "Mute notifications" },
                    "this terminal",
                    cx.listener(|m, _, _, cx| m.toggle_mute(cx)),
                )
                .children(keys("keys.mute")))
                .child(menu_item(t, "more-close", "Close terminal", "", cx.listener(|m, _, _, cx| m.close_selected(cx))).children(keys("keys.close"))),
        ),
    )
    .with_priority(1)
}

/// Tooltip view: the text, and the shortcut keys when the thing has one.
pub struct Tip {
    pub text: SharedString,
    /// Pretty keys ("⇧⌘T"), or empty.
    pub keys: SharedString,
}

/// Tooltip with no shortcut.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip { text: text.clone(), keys: SharedString::default() }).into()
}

/// Tooltip with the keys bound to a `keys.*` setting, looked up when shown (so a rebind shows).
pub fn tip_keys(text: impl Into<SharedString>, setting: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| {
        let keys = crate::actions::label(cx, setting).into();
        cx.new(|_| Tip { text: text.clone(), keys }).into()
    }
}

/// Tooltip with fixed keys (a screen's own keys, e.g. "⇧↩").
pub fn tip_fixed(text: impl Into<SharedString>, keys: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |_, cx| cx.new(|_| Tip { text: text.clone(), keys: keys.into() }).into()
}

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>();
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(11.5))
            .child(self.text.clone())
            .when(!self.keys.is_empty(), |d| d.child(key_chip(t, self.keys.clone())))
    }
}

/// Keys drawn as a small key cap ("⇧⌘T").
pub fn key_chip(t: &Theme, keys: SharedString) -> Div {
    div().flex_none().px(px(5.)).rounded(px(4.)).border_1().border_color(t.line).bg(t.panel).text_color(t.dim).text_size(px(11.)).whitespace_nowrap().child(keys)
}
