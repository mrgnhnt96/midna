//! midna's entrance: the window opens see-through, with no shadow and no traffic lights, while
//! the setup screen plays Twilight Tiles' opening over the desktop (`setup_screen.rs`, matching
//! the design canvas's timing study). Then the window turns opaque and its chrome comes back.
//!
//! Plays on launch while setup isn't finished (`MIDNA_INTRO=1` forces it;
//! `MIDNA_DEBUG_SCREEN=twilight` replays it; `MIDNA_INTRO_SPEED=0.25` plays it at a quarter speed).
use crate::app::MainWindow;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Off,
    /// The opening is playing; only the setup screen is drawn.
    Intro,
}

/// Only the first main window of a launch plays it.
pub fn wanted(onboarding: &crate::ui::onboarding::Onboarding) -> bool {
    static PLAYED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let want = std::env::var("MIDNA_INTRO").is_ok() || (!onboarding.saved.finished && std::env::var("MIDNA_SNAPSHOT").is_err());
    want && !PLAYED.swap(true, std::sync::atomic::Ordering::SeqCst)
}

/// Playback rate of the opening (dev: `MIDNA_INTRO_SPEED`).
pub fn speed() -> f32 {
    std::env::var("MIDNA_INTRO_SPEED").ok().and_then(|v| v.parse::<f32>().ok()).filter(|v| *v > 0.).unwrap_or(1.)
}

/// How long the opening runs, in real time.
pub fn length() -> Duration {
    Duration::from_secs_f32(crate::ui::setup_screen::INTRO_MS / 1000. / speed())
}

/// Start the entrance. Call from `MainWindow::new` (or to replay).
pub fn start(m_seq: u64, window: &mut Window, cx: &mut Context<MainWindow>) {
    window.set_background_appearance(WindowBackgroundAppearance::Transparent);
    chrome(window, false);
    let length = length();
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(length).await;
        let _ = this.update_in(cx, |m, window, cx| {
            if m.twilight_seq == m_seq {
                m.twilight_phase = Phase::Off;
                // The opening already brought the card in.
                m.onboarding.card_seq_opened = m.onboarding.card_seq;
                window.set_background_appearance(WindowBackgroundAppearance::Opaque);
                chrome(window, true);
                cx.notify();
            }
        });
    })
    .detach();
}

/// Play it again (dev).
pub fn replay(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.twilight_seq += 1;
    m.twilight_phase = Phase::Intro;
    start(m.twilight_seq, window, cx);
    cx.notify();
}

/// Show or hide the native window's shadow and traffic lights.
fn chrome(window: &Window, visible: bool) {
    use objc2_app_kit::{NSView, NSWindowButton};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return };
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let Some(ns) = view.window() else { return };
    ns.setHasShadow(visible);
    for b in [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(button) = ns.standardWindowButton(b) {
            button.setHidden(!visible);
        }
    }
}
