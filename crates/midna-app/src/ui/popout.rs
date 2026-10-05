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
    /// This window's queued-messages panel (its pill's click and ⌘U open it here).
    queue: Entity<crate::ui::queue::QueueView>,
    on_top: bool,
    session: String,
    main: WeakEntity<MainWindow>,
    _release: Subscription,
}

impl PopOut {
    fn set_on_top(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.on_top = on;
        set_level(window, on);
        cx.notify();
    }

    fn toggle_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let st = cx.global::<crate::ui::queue::QueueStore>();
        let sid = st.target.borrow_mut().take().unwrap_or_else(|| self.session.clone());
        self.queue.update(cx, |v, cx| v.toggle(sid, window, cx));
    }

    /// Back into its main window (the one that has it now, see `windows`), selected there.
    fn dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        forget(&self.session, cx);
        window.remove_window();
        let id = self.session.clone();
        cx.defer(move |cx| crate::windows::reveal(id, cx));
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
            .tooltip(crate::ui::header::tip_keys(if on { "Stop keeping this window on top" } else { "Keep this window above others" }, "keys.keep_on_top"))
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
            .tooltip(crate::ui::header::tip_keys("Back to main window", "keys.pop_out"))
            .child(Icon::DockIn.el(13., t.dim))
            .on_click(cx.listener(|p, _, w, cx| p.dock(w, cx)));
        div()
            .key_context(CTX_MAIN)
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            // the pop-out key toggles: here it docks back into the main window
            .on_action(cx.listener(|p, _: &crate::actions::PopOut, w, cx| p.dock(w, cx)))
            .on_action(cx.listener(move |p, _: &crate::actions::ToggleKeepOnTop, w, cx| p.set_on_top(!on, w, cx)))
            .on_action(cx.listener(|p, _: &crate::ui::queue::ToggleQueue, w, cx| p.toggle_queue(w, cx)))
            .relative()
            .size_full()
            .bg(t.term)
            .pt(px(28.))
            .child(self.term.clone())
            .child(div().absolute().top(px(4.)).right(px(8.)).flex().items_center().gap(px(6.)).child(dock).child(pill))
            .when(self.queue.read(cx).is_open(), |d| d.child(self.queue.clone()))
    }
}

pub fn is_popped(session: &str, cx: &App) -> bool {
    cx.try_global::<Popped>().is_some_and(|p| p.0.contains_key(session))
}

/// Bring `session`'s pop-out forward. False when it isn't popped out.
pub fn activate(session: &str, cx: &mut App) -> bool {
    cx.try_global::<Popped>().and_then(|p| p.0.get(session).copied()).is_some_and(|h| h.update(cx, |_, w, _| w.activate_window()).is_ok())
}

/// Whether `session`'s pop-out is the window you're using.
pub fn is_active(session: &str, cx: &App) -> bool {
    let h = cx.try_global::<Popped>().and_then(|p| p.0.get(session).copied());
    h.is_some_and(|h| cx.active_window() == Some(h.into()))
}

/// Close `session`'s pop-out, if it has one (its terminal is being closed).
pub fn close(session: &str, cx: &mut App) {
    let h = cx.try_global::<Popped>().and_then(|p| p.0.get(session).copied());
    forget(session, cx);
    if let Some(h) = h {
        let _ = h.update(cx, |_, w, _| w.remove_window());
    }
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
    let (main, backend) = (cx.entity().downgrade(), m.backend.clone());
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
            let queue = cx.new(|cx| crate::ui::queue::QueueView::new(backend.clone(), cx));
            let term = cx.new(|cx| TerminalView::new(id.clone(), backend, window, cx));
            let fh = term.read(cx).focus_handle().clone();
            fh.focus(window, cx);
            cx.subscribe_in(&queue, window, |p: &mut PopOut, _, ev: &crate::ui::queue::QueueEvent, window, cx| match ev {
                crate::ui::queue::QueueEvent::Closed => {
                    let fh = p.term.read(cx).focus_handle().clone();
                    fh.focus(window, cx);
                }
                crate::ui::queue::QueueEvent::Error(e) => eprintln!("midna-app: pop-out {}: {e}", p.session),
            })
            .detach();
            // Closed any way (⌘W, the traffic light, dock): the main window may show it again.
            let _release = cx.on_release(|p: &mut PopOut, cx| {
                forget(&p.session, cx);
                let _ = p.main.update(cx, |_, cx| cx.notify());
            });
            PopOut { term, queue, on_top: true, session: id, main, _release }
        })
    });
    if let Ok(h) = opened {
        cx.default_global::<Popped>().0.insert(session, h);
        cx.notify();
    }
}

/// Dev only (`MIDNA_DEBUG_SCREEN=popout-queue`): pop the terminal out and open its queue there.
pub fn debug_queue(m: &mut MainWindow, session: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    open(m, session.clone(), window, cx);
    cx.spawn(async move |_, cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        cx.update(|cx| {
            if let Some(h) = cx.try_global::<Popped>().and_then(|p| p.0.get(&session).copied()) {
                let _ = h.update(cx, |p, w, cx| p.toggle_queue(w, cx));
            }
        });
    })
    .detach();
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
