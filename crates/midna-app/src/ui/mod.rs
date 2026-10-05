//! Main window rendering, laid out after docs/design/Main.dc.html.
pub mod annotate;
pub mod banner;
pub mod charts;
pub mod close_window;
pub mod command_bar;
pub mod header;
pub mod insights;
pub mod links;
pub mod queue;
pub mod needs_you;
pub mod popout;
pub mod quit_hold;
pub mod rename;
pub mod rules;
pub mod screen_kit;
pub mod screens;
pub mod settings;
pub mod sidebar;
pub mod split;
pub mod statusbar;
pub mod subagent_window;
pub mod subagents;
pub mod text_input;
pub mod triggers;

use crate::actions::CTX_MAIN;
use crate::app::{MainWindow, Menu, Overlay, Screen};
use crate::backend::ConnState;
use crate::model::{CustomStatus, StatusState};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// Status dot. `size` 8 in rows, 9 in the header.
pub fn status_dot(t: &Theme, state: StatusState, size: f32) -> Div {
    let d = div().size(px(size)).flex_none().rounded_full();
    match state {
        StatusState::NeedsYou => {
            d.bg(t.need).shadow(vec![BoxShadow { color: t.need_ring, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }])
        }
        StatusState::Working => d.bg(t.work),
        StatusState::Done => d.bg(t.ok),
        StatusState::Failed => d.bg(t.err),
        _ => border_w(d, 1.5).border_color(t.dim),
    }
}

/// A terminal's status dot: a trigger's custom status in its color when set (ringed like
/// needs-you when its base is needs-you), else the built-in `status_dot`.
pub fn session_dot(t: &Theme, state: StatusState, custom: Option<&CustomStatus>, size: f32) -> Div {
    let Some(c) = custom else {
        return status_dot(t, state, size);
    };
    let color = t.status_color(&c.color);
    let d = div().size(px(size)).flex_none().rounded_full().bg(color);
    if state == StatusState::NeedsYou || c.base == StatusState::NeedsYou {
        d.shadow(vec![BoxShadow { color: color.opacity(0.22), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }])
    } else {
        d
    }
}

/// A custom status as a small colored label (icon when the icon set has it), e.g. in the
/// terminal header.
pub fn custom_status_label(t: &Theme, c: &CustomStatus, size: f32) -> Div {
    let color = t.status_color(&c.color);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(5.))
        .h(px(size + 9.))
        .px(px(8.))
        .rounded(px((size + 9.) / 2.))
        .bg(color.opacity(0.12))
        .text_size(px(size))
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .whitespace_nowrap()
        .children(c.icon.as_deref().and_then(crate::icons::Icon::from_name).map(|i| i.el(size, color)))
        .child(c.label.clone())
}

/// Border with a fractional width (GPUI's helpers are whole pixels).
pub fn border_w<E: Styled>(mut e: E, w: f32) -> E {
    let s = e.style();
    s.border_widths.top = Some(px(w).into());
    s.border_widths.right = Some(px(w).into());
    s.border_widths.bottom = Some(px(w).into());
    s.border_widths.left = Some(px(w).into());
    e
}

/// The uppercase 11px section label used for project names and the Today card.
pub fn caps_label(t: &Theme, text: &str) -> Div {
    div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(text.to_uppercase())
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        crate::composer::sync(self);
        let sidebar = sidebar::render(self, &t, window, cx);
        let main: AnyElement = match (&self.conn, self.screen) {
            (ConnState::NotRunning { .. }, _) | (ConnState::Connecting, _) => screens::not_running(self, &t, cx).into_any_element(),
            // NeedsYou-C replaces the pane right of the sidebar, whatever screen is under it.
            _ if self.overlay == Overlay::NeedsYou => needs_you::render(self, &t, window, cx).into_any_element(),
            (_, Screen::Terminal) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(header::render(self, &t, window, cx))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .bg(t.term)
                        .child(match &self.terminal {
                            Some(term) => {
                                let term = term.clone();
                                split::render(self, &term, &t, window, cx)
                            }
                            None => screens::empty_terminal(self, &t, cx).into_any_element(),
                        })
                        .children(banner::render(self, &t, cx))
                        .children(annotate::tray(self, &t, cx))
                        .children(crate::composer::render(self, &t, cx)),
                )
                .into_any_element(),
            (_, Screen::Rules) => rules::render(self, window, cx),
            (_, Screen::Triggers) => triggers::render(self, window, cx),
            (_, Screen::Insights) => insights::render(self, window, cx),
        };

        let root = div()
            .id("midna-main")
            .key_context(CTX_MAIN)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(13.))
            .line_height(px(13. * 1.45))
            .child(div().flex().flex_1().min_h_0().child(sidebar).child(main))
            .child(statusbar::render(self, &t, cx))
            .when(self.overlay == Overlay::CommandBar, |d| d.child(command_bar::render(self, &t, window, cx)))
            .when(self.overlay == Overlay::Annotate, |d| d.child(annotate::render(self, &t, window, cx)))
            .when(self.queue.read(cx).is_open(), |d| d.child(self.queue.clone()))
            // ⌘Q hold: releasing ⌘ (or Q, when macOS reports it) cancels
            .on_modifiers_changed(cx.listener(|m, ev: &ModifiersChangedEvent, _, cx| {
                if !ev.modifiers.platform {
                    quit_hold::cancel(m, "⌘ released", cx);
                }
            }))
            .capture_key_up(cx.listener(|m, ev: &KeyUpEvent, _, cx| {
                if ev.keystroke.key == "q" || !ev.keystroke.modifiers.platform {
                    quit_hold::cancel(m, &format!("key up {}", ev.keystroke.unparse()), cx);
                }
            }))
            .children(quit_hold::render(self, &t))
            .children(close_window::render(self, &t, cx))
            .when_some(self.toast.clone(), |d, (msg, _)| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(40.))
                        .right(px(16.))
                        .max_w(px(420.))
                        .px(px(12.))
                        .py(px(8.))
                        .rounded(px(8.))
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.line)
                        .text_size(px(12.))
                        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
                        .child(msg),
                )
            })
            .when(self.menu != Menu::None, |d| {
                // click-away layer under any open popover menu
                d.child(
                    deferred(div().id("menu-dismiss").absolute().top_0().left_0().size_full().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|m, _, _, cx| {
                            m.menu = Menu::None;
                            cx.notify();
                        }),
                    ))
                    .with_priority(0),
                )
            });
        crate::composer::register(MainWindow::register_actions(root, cx), cx)
    }
}
