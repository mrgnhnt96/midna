//! Pop-out: moves one terminal out of the main window into its own window, kept above other
//! apps' windows (`NSFloatingWindowLevel`, like picture-in-picture) until its "Keep on top"
//! pill is turned off. While popped out the main window doesn't show it: the sidebar row
//! carries a pop-out mark and clicking it brings the window forward. The dock button (or
//! closing the window) puts it back. The session keeps running in midnad either way.
use crate::actions::CTX_MAIN;
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::collections::HashMap;

/// Popped-out terminals by session id.
#[derive(Default)]
struct Popped(HashMap<String, WindowHandle<PopOut>>);
impl Global for Popped {}

pub struct PopOut {
    term: Entity<TerminalView>,
    on_top: bool,
    session: String,
    main: WeakEntity<MainWindow>,
    main_window: AnyWindowHandle,
    _release: Subscription,
}

impl PopOut {
    fn set_on_top(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.on_top = on;
        set_level(window, on);
        cx.notify();
    }

    /// Back into the main window, selected there.
    fn dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        forget(&self.session, cx);
        window.remove_window();
        let (main, id) = (self.main.clone(), self.session.clone());
        let _ = self.main_window.update(cx, |_, w, cx| {
            w.activate_window();
            let _ = main.update(cx, |m, cx| m.select(id, w, cx));
        });
    }
}

impl Render for PopOut {
    fn render(&mut self, _w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>();
        let on = self.on_top;
        let pill = div()
            .id("keep-on-top")
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
        let dock = div()
            .id("dock")
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(5.))
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .tooltip(|_, cx| cx.new(|_| crate::ui::header::Tip("Back to main window".into())).into())
            .child(Icon::DockIn.el(13., t.dim))
            .on_click(cx.listener(|p, _, w, cx| p.dock(w, cx)));
        div()
            .key_context(CTX_MAIN)
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .relative()
            .size_full()
            .bg(t.term)
            .pt(px(28.))
            .child(self.term.clone())
            .child(div().absolute().top(px(4.)).right(px(8.)).flex().items_center().gap(px(6.)).child(dock).child(pill))
    }
}

pub fn is_popped(session: &str, cx: &App) -> bool {
    cx.try_global::<Popped>().is_some_and(|p| p.0.contains_key(session))
}

fn forget(session: &str, cx: &mut App) {
    cx.default_global::<Popped>().0.remove(session);
}

/// Move `session` into its own window, or bring its window forward if it already has one.
pub fn open(m: &mut MainWindow, session: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(h) = cx.try_global::<Popped>().and_then(|p| p.0.get(&session).copied())
        && h.update(cx, |_, w, _| w.activate_window()).is_ok()
    {
        return;
    }
    m.release(&session, window, cx);
    let (main, main_window, backend) = (cx.entity().downgrade(), window.window_handle(), m.backend.clone());
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(440.)), cx))),
        titlebar: Some(TitlebarOptions { title: Some(format!("midna · {session}").into()), appears_transparent: true, traffic_light_position: Some(point(px(10.), px(9.))) }),
        kind: WindowKind::Normal,
        app_id: Some("com.mrgnhnt.midna".into()),
        ..Default::default()
    };
    let id = session.clone();
    let opened = cx.open_window(opts, |window, cx| {
        set_level(window, true);
        cx.new(|cx| {
            let term = cx.new(|cx| TerminalView::new(id.clone(), backend, window, cx));
            let fh = term.read(cx).focus_handle().clone();
            fh.focus(window, cx);
            // Closed any way (⌘W, the traffic light, dock): the main window may show it again.
            let _release = cx.on_release(|p: &mut PopOut, cx| {
                forget(&p.session, cx);
                let _ = p.main.update(cx, |_, cx| cx.notify());
            });
            PopOut { term, on_top: true, session: id, main, main_window, _release }
        })
    });
    if let Ok(h) = opened {
        cx.default_global::<Popped>().0.insert(session, h);
        cx.notify();
    }
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
