//! Terminal header: status dot, name, agent icon, script segments (`script.run` slot
//! `header`, default `github`), and the right toolbar.
use super::sidebar::{menu_box, menu_item, segments};
use super::status_dot;
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
    let tool = |id: &'static str, icon: Icon, tip: &'static str| {
        div()
            .id(id)
            .size(px(32.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|st| st.bg(t.raised))
            .tooltip(move |_, cx| cx.new(|_| Tip(tip.into())).into())
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
        .child(status_dot(t, m.effective_state(s), 9.))
        .child(match crate::ui::rename::field(m, &s.id, crate::ui::rename::At::Header, t, 15., cx) {
            Some(f) => f,
            None => {
                let sid = s.id.clone();
                div()
                    .id("header-name")
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .whitespace_nowrap()
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
        .when(s.notify_muted(), |d| {
            d.child(
                div()
                    .id("header-muted")
                    .tooltip(|_, cx| cx.new(|_| Tip("Notifications muted for this terminal".into())).into())
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
                .children(crate::ui::links::button(m, t, cx))
                .child(tool("tb-image", Icon::Image, "Add image").on_click(cx.listener(|m, _, window, cx| crate::annotate::open(m, window, cx))))
                .child(
                    tool("tb-split", Icon::Split, "Split ⌘D")
                        .when(m.split.is_some(), |d| d.bg(t.raised))
                        .on_click(cx.listener(|m, _, window, cx| crate::ui::split::toggle(m, window, cx))),
                )
                .child(tool("tb-popout", Icon::PopOut, "Pop out, keep on top").on_click(cx.listener(|m, _, w, cx| {
                    if let Some(id) = m.selected.clone() {
                        crate::ui::popout::open(m, id, w, cx);
                    }
                })))
                .child(tool("tb-restart", Icon::Restart, "Restart").on_click(cx.listener(|m, _, _, cx| m.restart_selected(cx))))
                .child(
                    div()
                        .relative()
                        .child(tool("tb-more", Icon::Dots, "More").when(more_open, |d| d.bg(t.raised)).on_click(cx.listener(|m, _, _, cx| {
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
                    cx.listener(move |m, _, _, cx| {
                        if let Some(id) = m.selected.clone() {
                            cx.write_to_clipboard(ClipboardItem::new_string(id));
                        }
                        m.menu = Menu::None;
                        cx.notify();
                    }),
                ))
                .child(menu_item(
                    t,
                    "more-mute",
                    if muted { "Unmute notifications" } else { "Mute notifications" },
                    "this terminal",
                    cx.listener(move |m, _, _, cx| {
                        if let Some(id) = m.selected.clone() {
                            // null drops the override: back to the global settings
                            let value = if muted { serde_json::Value::Null } else { serde_json::json!(false) };
                            m.rpc("notify.set", serde_json::json!({ "session": id, "key": "enabled", "value": value }), cx, |m, _, _, cx| {
                                m.request_refresh(crate::app::refresh::SESSIONS, cx)
                            });
                        }
                        m.menu = Menu::None;
                        cx.notify();
                    }),
                ))
                .child(menu_item(t, "more-close", "Close terminal", "", cx.listener(|m, _, _, cx| m.close_selected(cx)))),
        ),
    )
    .with_priority(1)
}

/// Plain tooltip view.
pub struct Tip(pub SharedString);

impl Render for Tip {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>();
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(6.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(11.5))
            .child(self.0.clone())
    }
}
