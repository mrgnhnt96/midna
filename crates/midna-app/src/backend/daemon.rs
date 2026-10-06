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
    /// Idle control connections. A call takes one (or opens a new one) and puts it back when
    /// done, so a slow call never holds up the others behind a shared connection.
    idle: Mutex<Vec<Client>>,
}

/// Idle control connections kept open; more are opened while calls overlap.
const MAX_IDLE: usize = 4;

impl DaemonBackend {
    pub fn new(socket: PathBuf) -> Self {
        DaemonBackend { socket, idle: Mutex::new(Vec::new()) }
    }

    fn connect(&self) -> std::io::Result<Client> {
        let mut c = Client::connect(&self.socket)?;
        // The GUI is human by peer-executable check; never forward an inherited MIDNA_SESSION.
        c.set_caller(None);
        // If the daemon ever takes the GUI for an agent (launched from a midna terminal), a call
        // needing approval answers at once (PENDING) instead of freezing it until the human does.
        let c = c.no_wait();
        c.set_read_timeout(Some(Duration::from_secs(30)))?;
        Ok(c)
    }

    fn take(&self) -> std::io::Result<Client> {
        let idle = self.idle.lock().unwrap().pop();
        match idle {
            Some(c) => Ok(c),
            None => self.connect(),
        }
    }

    fn give_back(&self, c: Client) {
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_IDLE {
            idle.push(c);
        }
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
        // One retry after a transport error: the call in flight when midnad re-execs itself
        // (upgrade/restart) loses its connection; the new image is listening ~1s later.
        // Calls refused because a handoff is in progress ("upgrading; retry") are retried too.
        let mut io_retry = true;
        let mut busy_retries = 6;
        loop {
            let mut c = match self.take() {
                Ok(c) => c,
                Err(_) if io_retry => {
                    io_retry = false;
                    std::thread::sleep(Duration::from_millis(700));
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            match c.call_value(method, params.clone()) {
                Ok(v) => {
                    self.give_back(c);
                    return Ok(v);
                }
                Err(ClientError::Rpc(e)) if busy_retries > 0 && e.code == midna_proto::error::CONFLICT && e.message.contains("upgrading") => {
                    self.give_back(c);
                    busy_retries -= 1;
                    std::thread::sleep(Duration::from_millis(400));
                }
                Err(ClientError::Rpc(e)) if e.code == midna_proto::error::PENDING => {
                    self.give_back(c);
                    let id = e.data.as_ref().and_then(|d| d["needs_you_id"].as_str()).unwrap_or("?").to_string();
                    return Err(anyhow!("waiting for your approval (needs-you {id}); it goes ahead once you approve"));
                }
                Err(ClientError::Rpc(e)) => {
                    self.give_back(c);
                    return Err(anyhow!("{e}"));
                }
                // The transport broke: the idle connections went with the same daemon image.
                Err(ClientError::Io(e)) if io_retry => {
                    self.idle.lock().unwrap().clear();
                    io_retry = false;
                    eprintln!("midna-app: {method}: {e}; reconnecting and retrying once");
                    std::thread::sleep(Duration::from_millis(300));
                }
                Err(e) => {
                    self.idle.lock().unwrap().clear();
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// A stand-in midnad: `slow` answers after 1.5 s, `pending` answers like a call waiting on
    /// approval, anything else answers at once with the params it got.
    fn fake_daemon() -> (PathBuf, Arc<Mutex<usize>>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("mdb-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let conns = Arc::new(Mutex::new(0));
        let counted = conns.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                *counted.lock().unwrap() += 1;
                std::thread::spawn(move || {
                    let mut w = s.try_clone().unwrap();
                    for line in BufReader::new(s).lines().map_while(Result::ok) {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let res = match req["method"].as_str() {
                            Some("slow") => {
                                std::thread::sleep(Duration::from_millis(1500));
                                serde_json::json!({ "result": "slow" })
                            }
                            Some("pending") => serde_json::json!({ "error": { "code": midna_proto::error::PENDING, "message": "waiting", "data": { "needs_you_id": "n_abc" } } }),
                            _ => serde_json::json!({ "result": req["params"] }),
                        };
                        let mut out = serde_json::json!({ "jsonrpc": "2.0", "id": req["id"] });
                        out.as_object_mut().unwrap().extend(res.as_object().unwrap().clone());
                        let _ = writeln!(w, "{out}");
                    }
                });
            }
        });
        (sock, conns)
    }

    #[test]
    fn a_slow_call_does_not_hold_up_the_others() {
        let (sock, _) = fake_daemon();
        let b = Arc::new(DaemonBackend::new(sock));
        let slow = {
            let b = b.clone();
            std::thread::spawn(move || b.call("slow", Value::Null).unwrap())
        };
        std::thread::sleep(Duration::from_millis(100));
        let t0 = Instant::now();
        b.call("fast", serde_json::json!({})).unwrap();
        assert!(t0.elapsed() < Duration::from_millis(500), "fast call waited {:?} behind the slow one", t0.elapsed());
        assert_eq!(slow.join().unwrap(), "slow");
    }

    #[test]
    fn connections_are_reused_and_calls_never_wait_on_the_human() {
        let (sock, conns) = fake_daemon();
        let b = DaemonBackend::new(sock);
        for _ in 0..5 {
            let echoed = b.call("echo", serde_json::json!({})).unwrap();
            assert_eq!(echoed["caller"], serde_json::json!({ "no_wait": true }), "no session forwarded, and no_wait set");
        }
        assert_eq!(*conns.lock().unwrap(), 1, "sequential calls share one connection");
    }

    #[test]
    fn a_pending_approval_comes_back_at_once_as_a_readable_error() {
        let (sock, _) = fake_daemon();
        let b = DaemonBackend::new(sock);
        let e = b.call("pending", Value::Null).unwrap_err().to_string();
        assert!(e.contains("waiting for your approval") && e.contains("n_abc"), "{e}");
        assert!(b.call("echo", serde_json::json!({})).is_ok(), "the connection is still usable");
    }
}
