//! A stand-in for Slate with the calls midna-app makes (`src/hidden.rs`). It never hides a run:
//! `enabled` is false and the rest pass through. `scripts/slate.sh` builds with the real crate.

use gpui_kit::{App, Application, Window, WindowOptions};

pub fn enabled() -> bool {
    false
}

pub fn hidden(app: Application) -> Application {
    app
}

pub fn window_options(options: WindowOptions, _cx: &App) -> WindowOptions {
    options
}

pub fn attach(_window: &mut Window, _cx: &mut App) {}
