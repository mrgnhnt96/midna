//! Hidden runs for agents (Slate, https://github.com/mrgnhnt96/slate): `slate shoot` and
//! `slate drive` launch the app with `SLATE=1`, and the first main window opens below the
//! desktop, never focused, while the app stays an accessory app that is never frontmost.
//! `scripts/slate.sh` runs them against this checkout's build; every other build links a
//! stand-in that never hides (`crates/slate-stub`), since slate is private.
//!
//! Only builds with dev drivers honour it (`dev.rs`): a drive presses keys and clicks in the
//! app, whose connection is the human's. A release build ignores `SLATE=1`.
//!
//! Under Slate the app also leaves out what would show on screen or take focus outside that
//! window: the launch entrance, the needs-you badge, macOS notifications, the windows saved
//! from last time (one main window opens, at `--size`) and activation.

use gpui_kit::{App, Application, Window, WindowOptions};

/// Whether this run is hidden.
pub fn enabled() -> bool {
    crate::dev::ENABLED && slate::enabled()
}

/// The app as an accessory app under Slate.
pub fn application(app: Application) -> Application {
    if crate::dev::ENABLED { slate::hidden(app) } else { app }
}

/// A main window's options: no focus and `SLATE_SIZE` under Slate.
pub fn window_options(options: WindowOptions, cx: &App) -> WindowOptions {
    if enabled() { slate::window_options(options, cx) } else { options }
}

/// Hides the first main window and has Slate shoot or drive it. Later windows are left alone.
pub fn attach(window: &mut Window, cx: &mut App) {
    static ATTACHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if enabled() && !ATTACHED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        slate::attach(window, cx);
    }
}

/// Brings midna to the front at launch, except under Slate or `MIDNA_NO_ACTIVATE`.
pub fn activate(cx: &mut App) {
    if !enabled() && std::env::var("MIDNA_NO_ACTIVATE").is_err() {
        cx.activate(true);
    }
}
