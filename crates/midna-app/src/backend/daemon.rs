//! Real midnad backend, built on `midna_proto::Client`: a control connection for calls, a
//! second connection for `events.subscribe` (reconnecting), and one `stream.attach`
//! connection per visible terminal.
use super::{AttachRequest, Backend, BackendEvent, ConnState, TermStream};
use crate::frame::FrameSink;
use crate::model::Event;
use anyhow::anyhow;
use anyhow::bail;
use midna_proto::client::{Client, ClientError, Notification};
use midna_proto::frame::{Frame, TAG_WANT, encode_input, encode_resize};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// `MIDNA_SOCKET` (set inside midna terminals; honoured for dev setups), else under the home.
/// Midna Dev always uses its own home's.
pub fn socket_path() -> PathBuf {
    midna_proto::paths::socket_path()
}

pub struct DaemonBackend {
    socket: PathBuf,
    control: Mutex<Option<Client>>,
}

impl DaemonBackend {
    pub fn new(socket: PathBuf) -> Self {
        DaemonBackend { socket, control: Mutex::new(None) }
    }

    fn connect(&self) -> std::io::Result<Client> {
        let mut c = Client::connect(&self.socket)?;
        // The GUI is human by peer-executable check; never forward an inherited MIDNA_SESSION.
        c.set_caller(None);
        Ok(c)
    }
}

/// `{ seq, kind, ... }` decoded through proto's strict type, then re-read leniently.
fn to_event(e: midna_proto::Event) -> Option<Event> {
    serde_json::to_value(e).ok().and_then(|v| serde_json::from_value(v).ok())
}

impl Backend for DaemonBackend {
    fn label(&self) -> &'static str {
        "midnad"
    }

    fn socket_path(&self) -> PathBuf {
        self.socket.clone()
    }

    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let mut guard = self.control.lock().unwrap();
        // One retry after a transport error: the call in flight when midnad re-execs itself
        // (upgrade/restart) loses its connection; the new image is listening ~1s later.
        // Calls refused because a handoff is in progress ("upgrading; retry") are retried too.
        let mut io_retry = true;
        let mut busy_retries = 6;
        loop {
            if guard.is_none() {
                let c = match self.connect() {
                    Ok(c) => c,
                    Err(e) if io_retry => {
                        io_retry = false;
                        drop(guard);
                        std::thread::sleep(Duration::from_millis(700));
                        guard = self.control.lock().unwrap();
                        let _ = e;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                };
                c.set_read_timeout(Some(Duration::from_secs(30)))?;
                *guard = Some(c);
            }
            match guard.as_mut().unwrap().call_value(method, params.clone()) {
                Ok(v) => return Ok(v),
                Err(ClientError::Rpc(e)) if busy_retries > 0 && e.code == midna_proto::error::CONFLICT && e.message.contains("upgrading") => {
                    busy_retries -= 1;
                    drop(guard);
                    std::thread::sleep(Duration::from_millis(400));
                    guard = self.control.lock().unwrap();
                }
                Err(ClientError::Rpc(e)) => return Err(anyhow!("{e}")),
                Err(ClientError::Io(e)) if io_retry => {
                    *guard = None;
                    io_retry = false;
                    eprintln!("midna-app: {method}: {e}; reconnecting and retrying once");
                    drop(guard);
                    std::thread::sleep(Duration::from_millis(300));
                    guard = self.control.lock().unwrap();
                }
                Err(e) => {
                    *guard = None; // transport broke; reconnect next time
                    return Err(anyhow!("{e}"));
                }
            }
        }
    }

    fn subscribe(&self, tx: async_channel::Sender<BackendEvent>) {
        let socket = self.socket.clone();
        std::thread::Builder::new()
            .name("midnad-events".into())
            .spawn(move || {
                let mut since: Option<u64> = None;
                let mut last: Option<ConnState> = None;
                let send_state = |st: ConnState, last: &mut Option<ConnState>| -> bool {
                    if last.as_ref() != Some(&st) {
                        *last = Some(st.clone());
                        return tx.send_blocking(BackendEvent::Conn(st)).is_ok();
                    }
                    !tx.is_closed()
                };
                loop {
                    let sub = Client::connect(&socket).map_err(ClientError::Io).and_then(|mut c| {
                        c.set_caller(None);
                        c.subscribe(since)
                    });
                    let mut sub = match sub {
                        Ok(s) => s,
                        Err(e) => {
                            if !send_state(ConnState::NotRunning { socket: socket.clone(), error: e.to_string() }, &mut last) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(1000));
                            continue;
                        }
                    };
                    // Replay only what we missed; on first connect start from now.
                    if since.is_none() {
                        since = Some(sub.seq_at_subscribe);
                    }
                    if !send_state(ConnState::Connected, &mut last) {
                        return;
                    }
                    loop {
                        let msg = match sub.next_notification() {
                            Ok(Notification::Event(e)) => {
                                if since.is_some_and(|s| e.seq <= s) {
                                    continue; // already seen (replay)
                                }
                                since = Some(e.seq);
                                match to_event(e) {
                                    Some(ev) => BackendEvent::Event(ev),
                                    None => continue,
                                }
                            }
                            Ok(Notification::WindowCommand(v)) => BackendEvent::WindowCommand(v),
                            Ok(Notification::Other { method, params }) => BackendEvent::Notification { method, params },
                            Err(ClientError::Decode(_)) => continue,
                            Err(_) => break,
                        };
                        if tx.send_blocking(msg).is_err() {
                            return;
                        }
                    }
                    if !send_state(ConnState::NotRunning { socket: socket.clone(), error: "connection closed".into() }, &mut last) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(750));
                }
            })
            .expect("spawn events thread");
    }

    fn attach(&self, req: AttachRequest<'_>, sink: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>> {
        // Raw socket rather than `Client::attach_stream` so detaching can shut the socket down
        // (proto's StreamWriter can't), which ends the reader thread immediately.
        let mut s = UnixStream::connect(&self.socket)?;
        let msg = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "stream.attach", "params": {
            "session": req.session, "cols": req.cols, "rows": req.rows, "cell_w": req.cell_w, "cell_h": req.cell_h,
        }});
        let mut line = serde_json::to_vec(&msg)?;
        line.push(b'\n');
        s.write_all(&line)?;
        // Read the one-line reply byte by byte so no binary frame data is over-read.
        s.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut reply = Vec::new();
        let mut b = [0u8; 1];
        loop {
            if s.read(&mut b)? == 0 {
                bail!("midnad closed the stream before replying");
            }
            if b[0] == b'\n' {
                break;
            }
            reply.push(b[0]);
            if reply.len() > 1 << 16 {
                bail!("stream.attach reply too long");
            }
        }
        s.set_read_timeout(None)?;
        let v: Value = serde_json::from_slice(&reply)?;
        if let Some(e) = v.get("error") {
            let msg = e.get("message").and_then(|m| m.as_str()).unwrap_or("error");
            bail!("stream.attach: {msg}");
        }
        let writer = s.try_clone()?;
        std::thread::Builder::new().name("midnad-stream".into()).spawn(move || {
            let mut hdr = [0u8; 4];
            let mut buf = Vec::new();
            loop {
                if s.read_exact(&mut hdr).is_err() {
                    break;
                }
                buf.resize(u32::from_le_bytes(hdr) as usize, 0);
                if s.read_exact(&mut buf).is_err() {
                    break;
                }
                match Frame::decode(&buf) {
                    Ok(f) => sink.put(f),
                    Err(e) => eprintln!("midna-app: {e}"),
                }
            }
            sink.end();
        })?;
        let st = Arc::new(DaemonStream { w: Mutex::new(writer), closed: AtomicBool::new(false) });
        st.want();
        Ok(st)
    }
}

struct DaemonStream {
    w: Mutex<UnixStream>,
    closed: AtomicBool,
}

impl DaemonStream {
    fn write_raw(&self, bytes: &[u8]) {
        if !self.closed.load(Ordering::Relaxed) {
            let _ = self.w.lock().unwrap().write_all(bytes);
        }
    }
}

impl TermStream for DaemonStream {
    fn want(&self) {
        self.write_raw(&[TAG_WANT]);
    }
    fn resize(&self, cols: u16, rows: u16, cw: u32, ch: u32) {
        self.write_raw(&encode_resize(cols, rows, cw, ch));
    }
    fn input(&self, bytes: &[u8]) {
        self.write_raw(&encode_input(bytes));
    }
    fn send(&self, m: &midna_proto::frame::ClientMsg) {
        self.write_raw(&m.encode());
    }
    fn close(&self) {
        if !self.closed.swap(true, Ordering::Relaxed) {
            let _ = self.w.lock().unwrap().shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Drop for DaemonStream {
    fn drop(&mut self) {
        self.close();
    }
}
