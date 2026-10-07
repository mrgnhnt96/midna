//! Terminal header: status dot, name, agent icon, script segments (`script.run` slot
//! `header`, default `github`), and the right toolbar.
use super::sidebar::{menu_box, menu_item, segments};
use super::{custom_status_label, status_label, terminal_dot, terminal_icon};
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
        .child(terminal_dot(m, t, s, 9.))
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
        .child(terminal_icon(m, t, s, 14., t.dim))
        .when_some(status_label(m, s), |d, c| {
            let by = if s.custom_status.is_some() { "Set by a trigger" } else { "Set in ui.status.looks" };
            let tip = c.detail.clone().filter(|x| !x.is_empty()).unwrap_or_else(|| by.into());
            d.child(div().id("header-custom-status").tooltip(tip_fixed(tip, "")).child(custom_status_label(t, &c, 11.5)))
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
        .child(toolbar(m, t, cx))
        .into_any_element()
}

/// What each built-in toolbar button is called in menus.
fn button_label(b: &str) -> &str {
    match b {
        "subagents" => "Subagents",
        "links" => "Session links",
        "ide" => "Open in IDE",
        "image" => "Add image",
        "split" => "Split",
        "popout" => "Pop out",
        "restart" => "Restart",
        p => p.rsplit('/').next().unwrap_or(p),
    }
}

fn list_setting(m: &MainWindow, key: &str) -> Vec<String> {
    m.settings.get(key).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
}

/// `ui.header.buttons`, in order: built-in names and the human's own script paths. Falls back
/// to the default while settings haven't loaded.
pub fn buttons(m: &MainWindow) -> Vec<String> {
    if m.settings.contains_key("ui.header.buttons") {
        list_setting(m, "ui.header.buttons")
    } else {
        midna_proto::settings::DEFAULT_HEADER_BUTTONS.iter().map(|s| s.to_string()).collect()
    }
}

/// The script paths in `ui.header.buttons` (custom buttons).
pub fn custom_buttons(m: &MainWindow) -> Vec<String> {
    buttons(m).into_iter().filter(|b| b.starts_with('/')).collect()
}

/// Built-in buttons left out of `ui.header.buttons`: their actions live in the More menu.
pub fn hidden(m: &MainWindow) -> Vec<String> {
    let shown = buttons(m);
    midna_proto::settings::HEADER_BUTTONS.iter().filter(|b| !shown.iter().any(|s| s == *b)).map(|s| s.to_string()).collect()
}

/// Add image shows only while the selected terminal, or the split beside it, runs an agent.
fn image_target(m: &MainWindow, cx: &App) -> bool {
    m.selected.iter().cloned().chain(m.split.as_ref().map(|s| s.session_id(cx))).any(|id| crate::annotate::is_agent(m, &id))
}

fn tool(t: &Theme, id: &'static str, icon: Icon, text: &'static str, setting: &'static str) -> Stateful<Div> {
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
}

/// The right toolbar: `ui.header.buttons` in order, then More. A hidden built-in's popover (links, subagents, IDE list) opens from More. Right-click shows or
/// hides buttons.
fn toolbar(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let hidden = hidden(m);
    let shown = |b: &str| !hidden.iter().any(|h| h == b);
    let more_open = m.menu == Menu::More;
    let popover_on_more: Option<AnyElement> = match &m.menu {
        Menu::Links if !shown("links") => Some(crate::ui::links::popover(m, t, cx).into_any_element()),
        Menu::Subagents if !shown("subagents") => Some(crate::ui::subagents::popover(m, t, cx).into_any_element()),
        Menu::Ide if !shown("ide") => Some(crate::ide::menu(m, t, cx).into_any_element()),
        _ => None,
    };
    div()
        .id("header-toolbar")
        .flex()
        .gap(px(2.))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(|m, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                m.status_menu_at = ev.position;
                m.menu = if m.menu == Menu::HeaderButtons { Menu::None } else { Menu::HeaderButtons };
                cx.notify();
            }),
        )
        .children(buttons(m).into_iter().filter_map(|b| toolbar_button(m, b, t, cx)))
        .child(
            div()
                .relative()
                .child(tool(t, "tb-more", Icon::Dots, "More", "keys.terminal_menu").when(more_open, |d| d.bg(t.raised)).on_click(cx.listener(|m, _, _, cx| {
                    m.menu = if m.menu == Menu::More { Menu::None } else { Menu::More };
                    cx.notify();
                })))
                .when(more_open, |d| d.child(more_menu(m, &hidden, t, cx)))
                .children(popover_on_more),
        )
        .when(m.menu == Menu::HeaderButtons, |d| d.child(buttons_menu(m, t, cx)))
}

/// One `ui.header.buttons` entry: a built-in button, or a custom one (a script path).
fn toolbar_button(m: &MainWindow, b: String, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    Some(match b.as_str() {
        "subagents" => crate::ui::subagents::button(m, t, cx)?,
        "links" => crate::ui::links::button(m, t, cx)?,
        "ide" => crate::ide::button(m, t, cx)?,
        "image" if !image_target(m, cx) => return None,
        "image" => tool(t, "tb-image", Icon::Image, "Add image", "keys.add_image")
            .on_click(cx.listener(|m, _, window, cx| {
                crate::annotate::open(m, None, window, cx);
            }))
            .into_any_element(),
        "split" => tool(t, "tb-split", Icon::Split, if m.split.is_some() { "Close split" } else { "Split" }, "keys.split")
            .when(m.split.is_some(), |d| d.bg(t.raised))
            .on_click(cx.listener(|m, _, window, cx| crate::ui::split::toggle(m, window, cx)))
            .into_any_element(),
        "popout" => tool(t, "tb-popout", Icon::PopOut, "Pop out", "keys.pop_out")
            .on_click(cx.listener(|m, _, w, cx| {
                if let Some(id) = m.selected.clone() {
                    crate::ui::popout::open(m, id, w, cx);
                }
            }))
            .into_any_element(),
        "restart" => tool(t, "tb-restart", Icon::Restart, "Restart", "keys.restart").on_click(cx.listener(|m, _, _, cx| m.restart_selected(cx))).into_any_element(),
        p if p.starts_with('/') => custom_button(m, b, t, cx)?,
        _ => return None,
    })
}

/// A custom button: its script's segments (an empty list hides it); the first tooltip, else the
/// script's name, on hover. A click runs the script with MIDNA_CLICK=1 (`script.click`).
fn custom_button(m: &MainWindow, path: String, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let sid = m.selected.clone()?;
    let segs = m.header_buttons.get(&sid).and_then(|b| b.get(&path)).cloned().unwrap_or_default();
    if segs.is_empty() {
        return None;
    }
    let tip_text = segs.iter().find_map(|s| s.tooltip.clone()).unwrap_or_else(|| button_label(&path).to_string());
    // the tooltip is the button's; segments without one keep hover quiet
    let plain: Vec<_> = segs.into_iter().map(|s| crate::model::Segment { tooltip: None, link: None, ..s }).collect();
    Some(
        div()
            .id(SharedString::from(format!("tb-custom-{path}")))
            .h(px(32.))
            .px(px(8.))
            .rounded(px(7.))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(px(12.5))
            .hover(|st| st.bg(t.raised))
            .tooltip(tip(tip_text))
            .on_click(cx.listener(move |m, _, _, cx| click_custom(m, sid.clone(), path.clone(), cx)))
            .child(segments(&plain, t, 12., cx).gap(px(6.)))
            .into_any_element(),
    )
}

fn click_custom(m: &mut MainWindow, sid: String, path: String, cx: &mut Context<MainWindow>) {
    let params = serde_json::json!({"session_id": sid, "script": path});
    m.rpc("script.click", params, cx, move |m, v, _, cx| {
        let segs: Vec<crate::model::Segment> = crate::model::parse_list(&v);
        if !segs.is_empty() {
            m.header_buttons.entry(sid).or_default().insert(path, segs);
        }
        m.request_refresh(crate::app::refresh::HEADER, cx);
        cx.notify();
    });
}

/// Right-click on the toolbar: a check row per built-in button and per custom one, saved to
/// `ui.header.buttons` (a re-shown built-in goes back to its default place; unchecking a custom
/// one removes it). Order is changed in settings (or by asking an agent). It stays open, so several can be
/// toggled; it occludes so a click doesn't reach the click-away layer or start a window drag underneath.
fn buttons_menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let current = buttons(m);
    let mut list = menu_box(t).occlude().text_size(px(12.5));
    let all = midna_proto::settings::HEADER_BUTTONS.iter().map(|s| s.to_string()).chain(current.iter().filter(|b| b.starts_with('/')).cloned());
    for b in all {
        let on = current.iter().any(|c| *c == b);
        let next = crate::ui::statusbar::toggled(&current, &b, "ui.header.buttons");
        let hint = if b.starts_with('/') { "unchecking removes it" } else { "" };
        list = list.child(
            menu_item(t, &format!("hb-{b}"), button_label(&b), hint, cx.listener(move |m, _, _, cx| save_list(m, "ui.header.buttons", next.clone(), cx)))
                .child(div().size(px(14.)).flex_none().when(on, |d| d.child(Icon::Check.el(13., t.accent)))),
        );
    }
    deferred(anchored().position(m.status_menu_at).anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(list.mt(px(4.)))).with_priority(1)
}

fn save_list(m: &mut MainWindow, key: &'static str, value: Vec<String>, cx: &mut Context<MainWindow>) {
    m.settings.insert(key.into(), serde_json::json!(value));
    m.rpc("settings.set", serde_json::json!({"key": key, "value": value}), cx, |_, _, _, _| {});
    cx.notify();
}

fn more_menu(m: &MainWindow, hidden: &[String], t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let id = m.selected.clone().unwrap_or_default();
    let keys = |setting: &str| {
        let k = m.key_label(setting);
        (!k.is_empty()).then(|| key_chip(t, k.into()))
    };
    let muted = m.selected_session().is_some_and(|s| s.notify_muted());
    // hidden toolbar buttons, in toolbar order, above the terminal's own actions
    let mut moved = div().flex().flex_col();
    let mut any_moved = false;
    for b in midna_proto::settings::HEADER_BUTTONS.iter().filter(|b| hidden.iter().any(|h| h == *b)) {
        let id = format!("more-{b}");
        let item = match *b {
            "subagents" => {
                let n = crate::ui::subagents::lists(m).0.len();
                menu_item(t, &id, "Subagents", &format!("{n} running"), cx.listener(|m, _, w, cx| crate::ui::subagents::toggle(m, w, cx))).children(keys("keys.subagents"))
            }
            "links" => {
                let n = m.selected.as_ref().and_then(|s| m.links.by_session.get(s)).map_or(0, Vec::len);
                menu_item(t, &id, "Session links", &n.to_string(), cx.listener(|m, _, w, cx| crate::ui::links::toggle(m, w, cx))).children(keys("keys.links"))
            }
            "ide" => {
                let name = crate::ide::current(m).map(|(i, _)| i.name).unwrap_or_else(|| "IDE".into());
                moved = moved.child(
                    menu_item(t, "more-ide-open", &format!("Open in {name}"), "", cx.listener(|m, _, _, cx| {
                        m.menu = Menu::None;
                        crate::ide::open_default(m, cx);
                    }))
                    .children(keys("keys.open_ide")),
                );
                menu_item(t, &id, "Open in…", "", cx.listener(|m, _, w, cx| crate::ide::toggle(m, w, cx))).children(keys("keys.choose_ide"))
            }
            "image" if !image_target(m, cx) => continue,
            "image" => menu_item(t, &id, "Add image", "", cx.listener(|m, _, w, cx| {
                m.menu = Menu::None;
                crate::annotate::open(m, None, w, cx);
            }))
            .children(keys("keys.add_image")),
            "split" => menu_item(t, &id, if m.split.is_some() { "Close split" } else { "Split" }, "", cx.listener(|m, _, w, cx| {
                m.menu = Menu::None;
                crate::ui::split::toggle(m, w, cx);
            }))
            .children(keys("keys.split")),
            "popout" => menu_item(t, &id, "Pop out", "", cx.listener(|m, _, w, cx| {
                m.menu = Menu::None;
                if let Some(id) = m.selected.clone() {
                    crate::ui::popout::open(m, id, w, cx);
                }
            }))
            .children(keys("keys.pop_out")),
            "restart" => menu_item(t, &id, "Restart", "", cx.listener(|m, _, _, cx| {
                m.menu = Menu::None;
                m.restart_selected(cx);
            }))
            .children(keys("keys.restart")),
            _ => continue,
        };
        moved = moved.child(item);
        any_moved = true;
    }
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            menu_box(t)
                .mt(px(36.))
                .when(any_moved, |d| d.child(moved).child(div().h(px(1.)).my(px(4.)).bg(t.line)))
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
