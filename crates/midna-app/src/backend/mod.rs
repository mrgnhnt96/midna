//! The GUI's view of midnad.
//!
//! `Backend` is a thin seam: the real implementation (`daemon`) speaks newline JSON-RPC
//! on `$MIDNA_HOME/midnad.sock` plus the binary `stream.attach` protocol; the fake one
//! (`fake`, `MIDNA_BACKEND=fake`) serves the design's sample data in memory so the UI
//! can be developed and screenshotted without a daemon.
use crate::frame::FrameSink;
use crate::model::Event;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

pub mod daemon;
pub mod fake;
#[cfg(feature = "fake-engine")]
mod fake_engine;

#[derive(Clone, Debug, PartialEq)]
pub enum ConnState {
    Connecting,
    Connected,
    NotRunning { socket: PathBuf, error: String },
}

#[derive(Clone, Debug)]
pub enum BackendEvent {
    Conn(ConnState),
    Event(Event),
    /// `window.command` notification pushed to GUI clients (front, keep_on_top, ...).
    WindowCommand(Value),
    /// Any other daemon notification for GUI clients (e.g. `updates.command`).
    Notification {
        method: String,
        params: Value,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct AttachRequest<'a> {
    pub session: &'a str,
    pub cols: u16,
    pub rows: u16,
    pub cell_w: u32,
    pub cell_h: u32,
}

/// A live `stream.attach` connection. Dropping it (or `close`) detaches.
pub trait TermStream: Send + Sync {
    /// Credit for the next frame (tag 0x01).
    fn want(&self);
    /// Tag 0x02.
    fn resize(&self, cols: u16, rows: u16, cell_w: u32, cell_h: u32);
    /// Tag 0x03.
    fn input(&self, bytes: &[u8]);
    /// Keys, scrolling, mouse, focus and paste (tags 0x04..0x08); the daemon encodes them
    /// for the app's current modes.
    fn send(&self, m: &midna_proto::frame::ClientMsg);
    fn close(&self);
}

pub trait Backend: Send + Sync + 'static {
    fn label(&self) -> &'static str;
    fn socket_path(&self) -> PathBuf;
    /// Blocking JSON-RPC call on the control connection. Call from a background task.
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value>;
    /// Start the event subscription (reconnecting). Pushes `Conn` state changes too.
    fn subscribe(&self, tx: async_channel::Sender<BackendEvent>);
    /// Open a frame stream for a session. Blocking; call from a background task.
    fn attach(&self, req: AttachRequest<'_>, sink: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>>;
}

/// Pick the backend: `MIDNA_BACKEND=fake` for the in-memory fake, otherwise the daemon.
pub fn from_env() -> Arc<dyn Backend> {
    match std::env::var("MIDNA_BACKEND").as_deref() {
        Ok("fake") => Arc::new(fake::FakeBackend::new()),
        _ => Arc::new(daemon::DaemonBackend::new(daemon::socket_path())),
    }
}
