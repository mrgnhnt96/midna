//! Main window rendering, laid out after docs/design/Main.dc.html.
pub mod annotate;
pub mod ax_prompt;
pub mod badge;
pub mod banner;
pub mod update_banner;
pub mod charts;
pub mod close_anim;
pub mod close_window;
pub mod command_bar;
pub mod footer;
pub mod header;
pub mod hooks;
pub mod insights;
pub mod onboarding;
pub mod links;
pub mod queue;
pub mod need_anim;
pub mod needs_you;
pub mod notifications;
pub mod toast;
pub mod popout;
pub mod quit_hold;
pub mod rename;
pub mod rules;
pub mod screen_kit;
pub mod screens;
pub mod setup_screen;
pub mod settings;
pub mod sidebar;
pub mod sidebar_anim;
pub mod split;
pub mod statusbar;
pub mod subagent_window;
pub mod subagents;
pub mod text_input;
pub mod triggers;
pub mod twilight;

use crate::actions::CTX_MAIN;
use crate::app::{MainWindow, Menu, Overlay, Screen};
use crate::backend::ConnState;
use crate::model::{CustomStatus, Glyph, Session, StatusState};
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

/// A terminal's status look from `ui.status.looks` (empty when nothing restyles its status).
pub fn look(m: &MainWindow, s: &Session) -> midna_proto::settings::StatusLook {
    let rules: Vec<String> = m.settings.get("ui.status.looks").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    if rules.is_empty() {
        return Default::default();
    }
    let who = match s.glyph() {
        Glyph::Claude => "claude",
        Glyph::Codex => "codex",
        Glyph::Monitor => "monitor",
        Glyph::Shell => "shell",
    };
    let state = match m.effective_state(s) {
        StatusState::Working => "working",
        StatusState::NeedsYou => "needs_you",
        StatusState::Done => "done",
        StatusState::Failed => "failed",
        StatusState::Exited => "exited",
        _ => "idle",
    };
    midna_proto::settings::status_look(&rules, who, state)
}

/// The built-in color of a status, as a `status_color` name (for a restyled label without a color).
fn state_color(state: StatusState) -> &'static str {
    match state {
        StatusState::NeedsYou => "amber",
        StatusState::Working => "blue",
        StatusState::Done => "green",
        StatusState::Failed => "red",
        _ => "gray",
    }
}

/// A terminal's dot: a trigger's custom status first, then a `ui.status.looks` color, else
/// the built-in dot.
pub fn terminal_dot(m: &MainWindow, t: &Theme, s: &Session, size: f32) -> Div {
    let state = m.effective_state(s);
    if s.custom_status.is_some() {
        return session_dot(t, state, s.custom_status.as_ref(), size);
    }
    match look(m, s).color {
        Some(c) => {
            let color = t.status_color(&c);
            let d = div().size(px(size)).flex_none().rounded_full().bg(color);
            if state == StatusState::NeedsYou {
                d.shadow(vec![BoxShadow { color: color.opacity(0.22), offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }])
            } else {
                d
            }
        }
        None => status_dot(t, state, size),
    }
}

/// The agent icon spot: a `ui.status.looks` icon (in the look's color) while that status
/// holds, else the terminal's agent/kind icon in `color`.
pub fn terminal_icon(m: &MainWindow, t: &Theme, s: &Session, size: f32, color: Hsla) -> Svg {
    let l = look(m, s);
    match l.icon.as_deref().and_then(crate::icons::Icon::from_name) {
        Some(i) => i.el(size, l.color.as_deref().map_or(color, |c| t.status_color(c))),
        None => crate::icons::Icon::from_glyph(s.glyph()).el(size, color),
    }
}

/// The label to show for a terminal's status: a trigger's custom status, else a
/// `ui.status.looks` label (as a custom status in the look's color, or the status's own).
pub fn status_label(m: &MainWindow, s: &Session) -> Option<CustomStatus> {
    if let Some(c) = &s.custom_status {
        return Some(c.clone());
    }
    let l = look(m, s);
    let state = m.effective_state(s);
    l.label.map(|label| CustomStatus { label, color: l.color.unwrap_or_else(|| state_color(state).into()), base: state, ..Default::default() })
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
        // The launch opening over setup: only the setup screen, over the desktop (the window is
        // see-through). Over the main window, the app draws as usual, masked (the tile window keeps the mask).
        twilight::first_frame(self, window, cx);
        if twilight::over_setup(self) {
            let root = div()
                .id("midna-main")
                .key_context(CTX_MAIN)
                .track_focus(&self.focus)
                .size_full()
                .capture_key_down(cx.listener(|m, ev: &KeyDownEvent, w, cx| {
                    if setup_screen::on_key(m, ev, w, cx) {
                        cx.stop_propagation();
                    }
                }))
                .children(setup_screen::render(self, &t, window, cx));
            return crate::composer::register(MainWindow::register_actions(root, cx), cx);
        }
        twilight::sync_lights(self, window);
        crate::composer::sync(self);
        self.close_anim.prune();
        let sidebar_anim = sidebar_anim::frame(self, window);
        let sidebar = sidebar::render(self, &t, sidebar_anim, window, cx);
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
                                let el = split::render(self, &term, &t, window, cx);
                                close_anim::pane(self, el, window)
                            }
                            None => screens::empty_terminal(self, &t, cx).into_any_element(),
                        })
                        .children(banner::render(self, &t, cx))
                        .children(update_banner::render(self, &t, cx))
                        .children(self.selected.clone().and_then(|id| annotate::tray(&id, &t, cx, |m, id, w, cx| _ = crate::annotate::open(m, Some(id), w, cx), |m, w, cx| m.focus_terminal(w, cx))))
                        .children(crate::composer::render(self, &t, cx)),
                )
                .into_any_element(),
            (_, Screen::Rules) => rules::render(self, window, cx),
            (_, Screen::Triggers) => triggers::render(self, window, cx),
            (_, Screen::Insights) => insights::render(self, window, cx),
            (_, Screen::Notifications) => notifications::render(self, &t, window, cx),
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
            .child(
                div().flex().flex_1().min_h_0().child(sidebar).child(
                    // the sidebar collapsing or expanding: the pane slides along with its edge
                    div().relative().left(px(sidebar_anim.map_or(0., |f| f.pane_offset(sidebar::RAIL_WIDTH)))).flex().flex_1().min_w_0().h_full().child(main),
                ),
            )
            .child(statusbar::render(self, &t, cx))
            .when(self.overlay == Overlay::CommandBar, |d| d.child(command_bar::render(self, &t, window, cx)))
            .when(self.overlay == Overlay::Annotate, |d| d.child(self.annot.clone()))
            .when(self.queue.read(cx).is_open(), |d| d.child(self.queue.clone()))
            // The composer's field takes its arrows before anything else sees them.
            // Setup's theme step browses with ← and → wherever focus is.
            .capture_key_down(cx.listener(|m, ev: &KeyDownEvent, w, cx| {
                if crate::composer::forward_nav_key(m, ev) || setup_screen::on_key(m, ev, w, cx) {
                    cx.stop_propagation();
                }
            }))
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
            .children(hooks::render(self, &t, cx))
            .child(
                // Cards over the terminal pane's top-right corner: Kass's Accessibility ask, then the setup step.
                div().absolute().top(px(56.)).right(px(12.)).flex().flex_col().items_end().gap(px(10.)).children(ax_prompt::render(self, &t, cx)).children(toast::render(self, &t, cx)),
            )
            .when_some(self.toast.clone(), |d, (msg, _)| d.child(toast(&t, msg).bottom(px(40.))))
            // Setup's Twilight Tiles screen covers the whole window while setup is open.
            .children(setup_screen::render(self, &t, window, cx))
            .when(self.menu != Menu::None, |d| {
                // click-away layer under any open popover menu
                d.child(
                    deferred(div().id("menu-dismiss").absolute().top_0().left_0().size_full().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|m, _, _, cx| {
                            links::unhover(m, cx);
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

/// A short message in the bottom-right corner (place it with `.bottom(…)`).
pub fn toast(t: &Theme, msg: String) -> Div {
    div()
        .absolute()
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
        .child(msg)
}
