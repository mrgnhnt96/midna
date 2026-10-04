//! Small blocking client: JSON-RPC calls, event subscriptions, and binary frame streams.
use crate::error::RpcError;
use crate::frame::{self, Frame};
use crate::methods::Caller;
use crate::types::Event;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub enum ClientError {
    /// Could not reach the daemon, or the connection broke.
    Io(std::io::Error),
    /// The daemon answered with a JSON-RPC error.
    Rpc(RpcError),
    /// The answer didn't parse.
    Decode(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Io(e) => write!(f, "daemon connection: {e}"),
            ClientError::Rpc(e) => write!(f, "{e}"),
            ClientError::Decode(e) => write!(f, "bad response: {e}"),
        }
    }
}
impl std::error::Error for ClientError {}
impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

/// A server-pushed notification.
#[derive(Clone, Debug)]
pub enum Notification {
    Event(Event),
    /// `window.command` params, sent to human (GUI) connections.
    WindowCommand(Value),
    Other { method: String, params: Value },
}

pub struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    caller: Option<Caller>,
    queued: VecDeque<Notification>,
}

impl Client {
    /// Connect to the daemon socket. The caller hint is taken from `MIDNA_SESSION` if set.
    pub fn connect(socket: impl AsRef<Path>) -> std::io::Result<Client> {
        let s = UnixStream::connect(socket)?;
        let caller = std::env::var("MIDNA_SESSION")
            .ok()
            .filter(|s| !s.is_empty())
            .map(|session| Caller { session: Some(session), role: None });
        Ok(Client { reader: BufReader::new(s.try_clone()?), writer: s, next_id: 1, caller, queued: VecDeque::new() })
    }

    /// Connect using `paths::socket_path()`.
    pub fn connect_default() -> std::io::Result<Client> {
        Client::connect(crate::paths::socket_path())
    }

    /// Override the identity hint sent with every call (None = send nothing).
    pub fn set_caller(&mut self, caller: Option<Caller>) {
        self.caller = caller;
    }

    /// Voluntarily act as an agent (optionally for a session), even from a human binary.
    pub fn as_agent(mut self, session: Option<String>) -> Client {
        self.caller = Some(Caller { session, role: Some("agent".into()) });
        self
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.writer.set_read_timeout(d)
    }

    pub fn call<T: DeserializeOwned>(&mut self, method: &str, params: impl Serialize) -> Result<T, ClientError> {
        let params = serde_json::to_value(params).map_err(|e| ClientError::Decode(e.to_string()))?;
        let v = self.call_value(method, params)?;
        serde_json::from_value(v).map_err(|e| ClientError::Decode(format!("{method}: {e}")))
    }

    pub fn call_value(&mut self, method: &str, params: Value) -> Result<Value, ClientError> {
        let id = self.send_request(method, params)?;
        loop {
            let msg = self.read_json()?;
            if msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    let e: RpcError = serde_json::from_value(err.clone()).map_err(|e| ClientError::Decode(e.to_string()))?;
                    return Err(ClientError::Rpc(e));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
            if let Some(n) = parse_notification(msg) {
                self.queued.push_back(n);
            }
        }
    }

    fn send_request(&mut self, method: &str, mut params: Value) -> Result<u64, ClientError> {
        if let Some(c) = &self.caller {
            if params.is_null() {
                params = json!({});
            }
            if let Some(o) = params.as_object_mut() {
                o.insert("caller".into(), serde_json::to_value(c).unwrap());
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        Ok(id)
    }

    fn read_json(&mut self) -> Result<Value, ClientError> {
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(ClientError::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "daemon closed the connection")));
            }
            if !line.trim().is_empty() {
                return serde_json::from_str(&line).map_err(|e| ClientError::Decode(e.to_string()));
            }
        }
    }

    /// Block for the next server notification (events, window commands).
    pub fn next_notification(&mut self) -> Result<Notification, ClientError> {
        if let Some(n) = self.queued.pop_front() {
            return Ok(n);
        }
        loop {
            let msg = self.read_json()?;
            if let Some(n) = parse_notification(msg) {
                return Ok(n);
            }
        }
    }

    /// Subscribe to events (replaying everything after `since_seq`) and iterate them.
    pub fn subscribe(mut self, since_seq: Option<u64>) -> Result<Subscription, ClientError> {
        let r: crate::methods::SubscribeResult = self.call("events.subscribe", json!({ "since_seq": since_seq }))?;
        Ok(Subscription { client: self, seq_at_subscribe: r.seq })
    }

    /// Open a dedicated frame-stream connection for a session.
    pub fn attach_stream(
        socket: impl AsRef<Path>,
        session: &str,
        cols: u16,
        rows: u16,
        cell_w: u32,
        cell_h: u32,
    ) -> Result<AttachStream, ClientError> {
        let mut c = Client::connect(socket)?;
        let params = json!({ "session": session, "cols": cols, "rows": rows, "cell_w": cell_w, "cell_h": cell_h });
        c.call_value("stream.attach", params)?;
        Ok(AttachStream { reader: c.reader, writer: c.writer, buf: Vec::new() })
    }
}

fn parse_notification(msg: Value) -> Option<Notification> {
    let method = msg.get("method")?.as_str()?.to_string();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method.as_str() {
        "event" => match serde_json::from_value(params.clone()) {
            Ok(e) => Notification::Event(e),
            Err(_) => Notification::Other { method, params },
        },
        "window.command" => Notification::WindowCommand(params),
        _ => Notification::Other { method, params },
    })
}

/// Iterator over events from `events.subscribe`. Non-event notifications are skipped;
/// use [`Subscription::next_notification`] to see them too.
pub struct Subscription {
    client: Client,
    /// Latest seq when the subscription started (replayed events have seq <= this).
    pub seq_at_subscribe: u64,
}

impl Subscription {
    pub fn next_notification(&mut self) -> Result<Notification, ClientError> {
        self.client.next_notification()
    }
    pub fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.client.set_read_timeout(d)
    }
}

impl Iterator for Subscription {
    type Item = Result<Event, ClientError>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.client.next_notification() {
                Ok(Notification::Event(e)) => return Some(Ok(e)),
                Ok(_) => continue,
                Err(ClientError::Io(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

/// A binary frame stream for one session (see ARCHITECTURE.md "Frame stream connection").
/// Call `want()` to grant credit for one frame, then `next_frame()` to receive it.
pub struct AttachStream {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    buf: Vec<u8>,
}

impl AttachStream {
    pub fn want(&mut self) -> std::io::Result<()> {
        self.writer.write_all(&[frame::TAG_WANT])
    }
    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> std::io::Result<()> {
        self.writer.write_all(&frame::encode_resize(cols, rows, cell_w, cell_h))
    }
    pub fn input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.write_all(&frame::encode_input(bytes))
    }
    /// Any client message (key, scroll, mouse, focus, paste, ...).
    pub fn send(&mut self, m: &frame::ClientMsg) -> std::io::Result<()> {
        self.writer.write_all(&m.encode())
    }
    /// Block until the next frame arrives.
    pub fn next_frame(&mut self) -> std::io::Result<Frame> {
        let mut len = [0u8; 4];
        self.reader.read_exact(&mut len)?;
        self.buf.resize(u32::from_le_bytes(len) as usize, 0);
        self.reader.read_exact(&mut self.buf)?;
        Frame::decode(&self.buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
    pub fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.writer.set_read_timeout(d)
    }
    /// A second handle for writing (want/resize/input) from another thread.
    pub fn writer(&self) -> std::io::Result<StreamWriter> {
        Ok(StreamWriter { s: self.writer.try_clone()? })
    }
}

/// Write half of an [`AttachStream`], usable from another thread.
pub struct StreamWriter {
    s: UnixStream,
}

impl StreamWriter {
    pub fn want(&mut self) -> std::io::Result<()> {
        self.s.write_all(&[frame::TAG_WANT])
    }
    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> std::io::Result<()> {
        self.s.write_all(&frame::encode_resize(cols, rows, cell_w, cell_h))
    }
    pub fn input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.s.write_all(&frame::encode_input(bytes))
    }
    pub fn send(&mut self, m: &frame::ClientMsg) -> std::io::Result<()> {
        self.s.write_all(&m.encode())
    }
}
