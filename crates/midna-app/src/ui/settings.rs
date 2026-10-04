//! Settings is its own window (Settings-C); see `crate::settings_window`.
use crate::backend::Backend;
use gpui_kit::App;
use std::sync::Arc;

/// Open (or bring forward) the Settings window.
pub fn open(backend: Arc<dyn Backend>, cx: &mut App) {
    crate::settings_window::open(backend, cx);
}
