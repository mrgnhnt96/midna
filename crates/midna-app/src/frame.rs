//! Terminal frames come from `midna_proto::frame` (the B-snap dirty-rows codec); this module
//! adds the hand-off point between a stream thread and the UI.
pub use midna_proto::frame::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Where a stream drops frames for the UI. Newer frames merge into an unread one.
pub struct FrameSink {
    pub frame: Mutex<Option<Frame>>,
    pub wake: async_channel::Sender<()>,
    /// Set by the stream when the connection ends (session closed / daemon gone).
    pub ended: AtomicBool,
}

impl FrameSink {
    pub fn new(wake: async_channel::Sender<()>) -> Self {
        FrameSink { frame: Mutex::new(None), wake, ended: AtomicBool::new(false) }
    }
    pub fn put(&self, f: Frame) {
        let mut g = self.frame.lock().unwrap();
        match g.as_mut() {
            Some(old) => old.merge(f),
            None => *g = Some(f),
        }
        drop(g);
        let _ = self.wake.try_send(());
    }
    pub fn take(&self) -> Option<Frame> {
        self.frame.lock().unwrap().take()
    }
    pub fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
    }
    /// Reuse the sink for a new stream (re-attach after the old one ended).
    pub fn reset(&self) {
        self.ended.store(false, Ordering::SeqCst);
        self.frame.lock().unwrap().take();
    }
    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}
