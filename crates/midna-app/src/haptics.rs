//! Trackpad haptics (setting `ui.haptics`, on by default): a light tap on a Force Touch trackpad
//! when you click something clickable anywhere in the app.
//!
//! One AppKit local monitor on left-mouse-down covers every window, rather than a call in each
//! of the app's click handlers. "Clickable" is whatever shows the pointing-hand cursor: every
//! button, row and link sets `cursor_pointer()`, and GPUI hands that to `NSCursor`, so a click
//! over a pointing hand is a click on something that acts. Text, terminals and empty space
//! (I-beam / arrow) stay quiet. A mouse, or a trackpad without Force Touch, ignores the request.
//! Main thread only (NSEvent / NSCursor).
use std::cell::Cell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2_app_kit::{NSCursor, NSEvent, NSEventMask, NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime, NSHapticFeedbackPerformer};

thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(true) };
}

/// Install the click monitor (once, at launch). It lives as long as the app.
pub fn start() {
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        if ENABLED.get() && NSCursor::currentCursor() == NSCursor::pointingHandCursor() {
            NSHapticFeedbackManager::defaultPerformer().performFeedbackPattern_performanceTime(NSHapticFeedbackPattern::Generic, NSHapticFeedbackPerformanceTime::Now);
        }
        event.as_ptr()
    });
    // SAFETY: main thread (called from `app.run`); the handler returns the event it was given.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::LeftMouseDown, &handler) };
    std::mem::forget(monitor);
}

/// `ui.haptics` changed (or the app connected).
pub fn sync(enabled: bool) {
    ENABLED.set(enabled);
}
