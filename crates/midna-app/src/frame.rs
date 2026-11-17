//! Terminal frames come from `midna_proto::frame` (the B-snap dirty-rows codec); this module
//! adds the hand-off point between a stream thread and the UI.
pub use midna_proto::frame::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Where a stream drops frames for the UI. Newer frames merge into an unread one.
pub struct FrameSink {
    pub frame: Mutex<Option<Frame>>,
    /// Frames folded into `frame` (changed only while holding its lock): each one used a credit
    /// that the UI gives back when it applies them.
    count: AtomicU32,
    pub wake: async_channel::Sender<()>,
    /// Set by the stream when the connection ends (session closed / daemon gone).
    pub ended: AtomicBool,
}

impl FrameSink {
    pub fn new(wake: async_channel::Sender<()>) -> Self {
        FrameSink { frame: Mutex::new(None), count: AtomicU32::new(0), wake, ended: AtomicBool::new(false) }
    }
    pub fn put(&self, f: Frame) {
        let mut g = self.frame.lock().unwrap();
        match g.as_mut() {
            Some(old) => old.merge(f),
            None => *g = Some(f),
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        drop(g);
        let _ = self.wake.try_send(());
    }
    /// The unread frame and how many stream frames went into it (the credits to give back).
    pub fn take(&self) -> Option<(Frame, u32)> {
        let mut g = self.frame.lock().unwrap();
        let f = g.take()?;
        Some((f, self.count.swap(0, Ordering::SeqCst)))
    }
    pub fn end(&self) {
        self.ended.store(true, Ordering::SeqCst);
        let _ = self.wake.try_send(());
    }
    /// Reuse the sink for a new stream (re-attach after the old one ended).
    pub fn reset(&self) {
        self.ended.store(false, Ordering::SeqCst);
        let mut g = self.frame.lock().unwrap();
        g.take();
        self.count.store(0, Ordering::SeqCst);
    }
    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn take_counts_the_frames_merged_into_it() {
        let (tx, _rx) = async_channel::bounded(1);
        let sink = FrameSink::new(tx);
        assert!(sink.take().is_none());
        sink.put(Frame::default());
        sink.put(Frame::default());
        sink.put(Frame::default());
        assert_eq!(sink.take().map(|(_, n)| n), Some(3));
        sink.put(Frame::default());
        assert_eq!(sink.take().map(|(_, n)| n), Some(1));
        sink.put(Frame::default());
        sink.reset();
        assert!(sink.take().is_none());
        sink.put(Frame::default());
        assert_eq!(sink.take().map(|(_, n)| n), Some(1));
    }
}
