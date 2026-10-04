//! Pop-out: a separate window showing one terminal, kept above other apps' windows
//! (`NSFloatingWindowLevel`, like picture-in-picture) until its "Keep on top" pill is turned
//! off. A normal window otherwise, so it can be sent back and closed with ⌘W. The session
//! keeps running in midnad either way.
use crate::actions::CTX_MAIN;
use crate::backend::Backend;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::sync::Arc;

pub struct PopOut {
    term: Entity<TerminalView>,
    on_top: bool,
}

impl PopOut {
    fn set_on_top(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.on_top = on;
        set_level(window, on);
        cx.notify();
    }
}

impl Render for PopOut {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>();
        let on = self.on_top;
        let pill = div()
            .id("keep-on-top")
            .absolute()
            .top(px(5.))
            .right(px(8.))
            .px(px(8.))
            .h(px(18.))
            .flex()
            .items_center()
            .rounded(px(9.))
            .text_size(px(11.))
            .cursor_pointer()
            .when(on, |d| d.bg(t.accent_soft).text_color(t.accent))
            .when(!on, |d| d.text_color(t.dim).hover(|s| s.bg(t.raised)))
            .child(if on { "Keep on top ✓" } else { "Keep on top" })
            .on_click(cx.listener(move |p, _, w, cx| p.set_on_top(!on, w, cx)));
        div()
            .key_context(CTX_MAIN)
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .relative()
            .size_full()
            .bg(t.term)
            .pt(px(28.))
            .child(self.term.clone())
            .child(pill)
    }
}

pub fn open(session: String, backend: Arc<dyn Backend>, cx: &mut App) {
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(440.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some(format!("midna · {session}").into()), appears_transparent: true, traffic_light_position: Some(point(px(10.), px(9.))) }),
        kind: WindowKind::Normal,
        app_id: Some("com.mrgnhnt.midna".into()),
        ..Default::default()
    };
    let _ = cx.open_window(opts, |window, cx| {
        set_level(window, true);
        cx.new(|cx| {
            let term = cx.new(|cx| TerminalView::new(session, backend, window, cx));
            let fh = term.read(cx).focus_handle().clone();
            fh.focus(window, cx);
            PopOut { term, on_top: true }
        })
    });
}

/// Floating (above other apps' normal windows) or normal level for the native window.
fn set_level(window: &Window, on_top: bool) {
    use objc2_app_kit::{NSFloatingWindowLevel, NSNormalWindowLevel, NSView};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    let view = unsafe { &*h.ns_view.as_ptr().cast::<NSView>() };
    if let Some(w) = view.window() {
        w.setLevel(if on_top { NSFloatingWindowLevel } else { NSNormalWindowLevel });
    }
}
