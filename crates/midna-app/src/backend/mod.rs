//! The GUI's view of midnad.
//!
//! `Backend` is a thin seam: the real implementation (`daemon`) speaks newline JSON-RPC
//! on `$MIDNA_HOME/midnad.sock` plus the binary `stream.attach` protocol; the fake one
//! (`fake`, `MIDNA_BACKEND=fake`) serves the design's sample data in memory so the UI
//! can be developed and screenshotted without a daemon.
use crate::frame::FrameSink;
use crate::model::Event;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

pub mod daemon;
pub mod fake;
#[cfg(feature = "fake-engine")]
mod fake_engine;

#[derive(Clone, Debug, PartialEq)]
pub enum ConnState {
    Connecting,
    Connected,
    NotRunning { socket: PathBuf, error: String },
    /// Another Mac's window whose drive link isn't up yet, or is down: how far it got, from
    /// this Mac's midnad (`drive.list`).
    Link(LinkProgress),
}

/// A drive link that isn't carrying calls yet (`ConnState::Link`).
#[derive(Clone, Debug, PartialEq)]
pub struct LinkProgress {
    /// Connected means the link is up but this window's own connection through it isn't yet.
    pub state: midna_proto::nearby::DriveState,
    pub stage: Option<midna_proto::nearby::DriveStage>,
    pub reason: Option<String>,
}

impl LinkProgress {
    /// The window has started the link and heard nothing back yet.
    pub fn starting() -> Self {
        LinkProgress { state: midna_proto::nearby::DriveState::Connecting, stage: None, reason: None }
    }

    /// "reconnecting to Bravo: the network dropped".
    pub fn describe(&self, name: &str) -> String {
        use midna_proto::nearby::DriveState;
        let state = match self.state {
            DriveState::Connecting => format!("connecting to {name}"),
            DriveState::Connected => format!("connected to {name}"),
            DriveState::Reconnecting => format!("reconnecting to {name}"),
            DriveState::Offline => format!("{name} is offline"),
            DriveState::Closed => format!("the link to {name} closed"),
        };
        match self.reason.as_deref() {
            Some(r) if !r.is_empty() => format!("{state}: {r}"),
            _ => state,
        }
    }
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

/// Another of my Macs, driven over a drive link (`drive.open`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Mac {
    /// Its fingerprint: what a window is keyed by.
    pub peer: String,
    /// Its name, for the window title and messages.
    pub name: String,
}

pub trait Backend: Send + Sync + 'static {
    fn label(&self) -> &'static str;
    /// This Mac's midnad socket, also for another Mac's backend: app-state.json, themes, sounds
    /// and commands stay on this Mac.
    fn socket_path(&self) -> PathBuf;
    /// The Mac this backend reaches when it isn't this one.
    fn mac(&self) -> Option<&Mac> {
        None
    }
    /// This Mac's backend, for another Mac's (None: this is it). See [`local`].
    fn this_mac(&self) -> Option<Arc<dyn Backend>> {
        None
    }
    /// A backend for another of my Macs, reached over a drive link this one opens. Use
    /// [`for_mac`], which keeps one per Mac.
    fn open_mac(self: Arc<Self>, mac: Mac) -> Arc<dyn Backend>;
    /// Blocking JSON-RPC call on the control connection. Call from a background task.
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value>;
    /// Start the event subscription (reconnecting). Pushes `Conn` state changes too.
    fn subscribe(&self, tx: async_channel::Sender<BackendEvent>);
    /// Open a frame stream for a session. Blocking; call from a background task.
    fn attach(&self, req: AttachRequest<'_>, sink: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>>;
}

/// The fingerprint of the Mac a backend reaches; None: this one.
pub fn mac_id(b: &dyn Backend) -> Option<String> {
    b.mac().map(|m| m.peer.clone())
}

fn remotes() -> &'static Mutex<HashMap<String, Arc<dyn Backend>>> {
    static R: OnceLock<Mutex<HashMap<String, Arc<dyn Backend>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// This Mac's backend: `b` itself, or the one another Mac's backend was opened through. For
/// what stays on this Mac whichever Mac's window asks (Settings, this Mac's notifications).
pub fn local(b: &Arc<dyn Backend>) -> Arc<dyn Backend> {
    b.this_mac().unwrap_or_else(|| b.clone())
}

/// The backend for another of my Macs: one per Mac, shared by its windows, opened through
/// `local` (this Mac's backend) the first time.
pub fn for_mac(local: &Arc<dyn Backend>, mac: Mac) -> Arc<dyn Backend> {
    let mut r = remotes().lock().unwrap();
    r.entry(mac.peer.clone()).or_insert_with(|| local.clone().open_mac(mac)).clone()
}

/// Pick the backend: `MIDNA_BACKEND=fake` for the in-memory fake, otherwise the daemon.
pub fn from_env() -> Arc<dyn Backend> {
    match std::env::var("MIDNA_BACKEND").as_deref() {
        Ok("fake") => Arc::new(fake::FakeBackend::new()),
        _ => Arc::new(daemon::DaemonBackend::new(daemon::socket_path())),
    }
}
