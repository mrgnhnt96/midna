//! Real midnad backend, built on `midna_proto::Client`: a control connection for calls, a
//! second connection for `events.subscribe` (reconnecting), and one `stream.attach`
//! connection per visible terminal.
use super::{AttachRequest, Backend, BackendEvent, ConnState, LinkProgress, Mac, TermStream};
use crate::frame::FrameSink;
use crate::model::Event;
use anyhow::anyhow;
use anyhow::bail;
use midna_proto::client::{Client, ClientError, Notification};
use midna_proto::frame::{Frame, TAG_WANT, encode_input, encode_resize};
use midna_proto::nearby::{DriveLink, DriveListResult, DriveState};
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
    target: Target,
    /// Idle control connections. A call takes one (or opens a new one) and puts it back when
    /// done, so a slow call never holds up the others behind a shared connection.
    idle: Mutex<Vec<Client>>,
}

/// How long a call may take before the app gives up on it.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Calls midnad may hold open longer: pairing and sending wait up to a minute while macOS asks
/// about Local Network access, and a timed-out call is retried, which would start them twice.
const SLOW_CALLS: &[&str] = &["peers.pair", "share.send"];
const SLOW_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Idle control connections kept open; more are opened while calls overlap.
const MAX_IDLE: usize = 4;

/// Frames a stream over a drive link keeps in flight (midnad caps credit at `term::MAX_CREDIT`,
/// 4). One would make every frame wait a Wi-Fi round trip; a few is enough to hide it.
const LINK_FRAME_WINDOW: u32 = 3;

/// What a backend connects to.
#[derive(Clone)]
enum Target {
    /// This Mac's midnad.
    Local(PathBuf),
    /// Another of my Macs, through the drive link this Mac's midnad keeps to it.
    Link(Arc<Link>),
}

/// A drive link's socket speaks midnad.sock's protocol, carried to that Mac. It's asked of this
/// Mac's midnad (`drive.open`) the first time, and again once its socket is gone (the link
/// closed, or this Mac's midnad restarted).
struct Link {
    local: Arc<dyn Backend>,
    mac: Mac,
    socket: Mutex<Option<PathBuf>>,
    /// Why `drive.open` refused, or said the link had closed (it's gone from `drive.list` then).
    refused: Mutex<Option<String>>,
}

impl Link {
    fn socket(&self) -> anyhow::Result<PathBuf> {
        if let Some(s) = self.socket.lock().unwrap().clone().filter(|s| s.exists()) {
            return Ok(s);
        }
        let refuse = |why: String| {
            *self.refused.lock().unwrap() = Some(why.clone());
            anyhow!(why)
        };
        let v = self.local.call("drive.open", serde_json::json!({ "peer": self.mac.peer })).map_err(|e| {
            refuse(e.downcast_ref::<midna_proto::error::RpcError>().map(|r| r.message.clone()).unwrap_or_else(|| e.to_string()))
        })?;
        let link: DriveLink = serde_json::from_value(v)?;
        if link.state == DriveState::Closed {
            return Err(refuse(link.reason.filter(|r| !r.is_empty()).unwrap_or_else(|| format!("the link to {} closed", self.mac.name))));
        }
        *self.refused.lock().unwrap() = None;
        let s = PathBuf::from(link.socket);
        *self.socket.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    /// Where the link is, from this Mac's midnad (`drive.list`); None once it's gone.
    fn progress(&self) -> Option<LinkProgress> {
        let links = self.local.call("drive.list", Value::Null).ok().and_then(|v| serde_json::from_value::<DriveListResult>(v).ok())?;
        let l = links.links.into_iter().find(|l| l.peer == self.mac.peer)?;
        Some(LinkProgress { state: l.state, stage: l.stage, reason: l.reason })
    }

    /// Why a connection through the link failed: a link that isn't connected answers each one
    /// with a refusal and closes it, so the state comes from this Mac's midnad instead.
    fn why(&self) -> String {
        match self.progress() {
            None => self.refused.lock().unwrap().clone().unwrap_or_else(|| format!("no link to {}", self.mac.name)),
            Some(p) => p.describe(&self.mac.name),
        }
    }

    /// The window's state after a connection through the link failed: how far the link got
    /// while it's on its way (or back), else why it can't be reached.
    fn state(&self) -> ConnState {
        match self.progress() {
            Some(p) if p.state != DriveState::Closed => ConnState::Link(p),
            _ => ConnState::NotRunning { socket: self.local.socket_path(), error: self.why() },
        }
    }
}

impl Target {
    /// The socket to connect to.
    fn socket(&self) -> std::io::Result<PathBuf> {
        match self {
            Target::Local(s) => Ok(s.clone()),
            Target::Link(l) => l.socket().map_err(std::io::Error::other),
        }
    }

    /// The socket to name in a "not running" state, and why connecting failed.
    fn failed(&self, e: impl std::fmt::Display) -> ConnState {
        match self {
            Target::Local(s) => ConnState::NotRunning { socket: s.clone(), error: e.to_string() },
            Target::Link(l) => l.state(),
        }
    }
}

impl DaemonBackend {
    pub fn new(socket: PathBuf) -> Self {
        DaemonBackend { target: Target::Local(socket), idle: Mutex::new(Vec::new()) }
    }

    /// Another of my Macs, through a drive link `local` (this Mac's backend) opens.
    pub fn link(local: Arc<dyn Backend>, mac: Mac) -> Self {
        DaemonBackend { target: Target::Link(Arc::new(Link { local, mac, socket: Mutex::new(None), refused: Mutex::new(None) })), idle: Mutex::new(Vec::new()) }
    }

    /// Frames a stream may have in flight: one locally, as midnad has always given, a few over
    /// a drive link. Older midnads keep one credit at most, so the extra asks do no harm there.
    fn frame_window(&self) -> u32 {
        match self.target {
            Target::Local(_) => 1,
            Target::Link(_) => LINK_FRAME_WINDOW,
        }
    }

    fn socket(&self) -> std::io::Result<PathBuf> {
        self.target.socket()
    }

    fn connect(&self) -> std::io::Result<Client> {
        let mut c = Client::connect(&self.socket()?)?;
        // The GUI is human by peer-executable check; never forward an inherited MIDNA_SESSION.
        c.set_caller(None);
        // If the daemon ever takes the GUI for an agent (launched from a midna terminal), a call
        // needing approval answers at once (PENDING) instead of freezing it until the human does.
        let c = c.no_wait();
        c.set_read_timeout(Some(CALL_TIMEOUT))?;
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
        match &self.target {
            Target::Local(s) => s.clone(),
            Target::Link(l) => l.local.socket_path(),
        }
    }

    fn mac(&self) -> Option<&Mac> {
        match &self.target {
            Target::Local(_) => None,
            Target::Link(l) => Some(&l.mac),
        }
    }

    fn this_mac(&self) -> Option<Arc<dyn Backend>> {
        match &self.target {
            Target::Local(_) => None,
            Target::Link(l) => Some(l.local.clone()),
        }
    }

    fn open_mac(self: Arc<Self>, mac: Mac) -> Arc<dyn Backend> {
        Arc::new(DaemonBackend::link(self, mac))
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
                Err(e) => {
                    if let Target::Link(l) = &self.target {
                        return Err(anyhow!("{method}: {}", l.why()));
                    }
                    return Err(e.into());
                }
            };
            let slow = SLOW_CALLS.contains(&method);
            if slow {
                let _ = c.set_read_timeout(Some(SLOW_CALL_TIMEOUT));
            }
            let r = c.call_value(method, params.clone());
            if slow {
                let _ = c.set_read_timeout(Some(CALL_TIMEOUT));
            }
            match r {
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
                    // Same text as before; callers can downcast to RpcError for the code.
                    return Err(anyhow::Error::new(e));
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
                    if let Target::Link(l) = &self.target {
                        return Err(anyhow!("{method}: {}", l.why()));
                    }
                    return Err(anyhow!("{e}"));
                }
            }
        }
    }

    fn subscribe(&self, tx: async_channel::Sender<BackendEvent>) {
        let target = self.target.clone();
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
                    // Nobody's listening any more: don't connect again (through a drive link that
                    // would open it again, see `Link::socket`, after it was closed on purpose).
                    if tx.is_closed() {
                        return;
                    }
                    let sub = target.socket().and_then(|s| Client::connect(&s)).map_err(ClientError::Io).and_then(|mut c| {
                        c.set_caller(None);
                        c.subscribe(since)
                    });
                    let mut sub = match sub {
                        Ok(s) => s,
                        Err(e) => {
                            if !send_state(target.failed(e), &mut last) {
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
                    if !send_state(target.failed("connection closed"), &mut last) {
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
        let mut s = match self.socket().and_then(UnixStream::connect) {
            Ok(s) => s,
            Err(e) => match &self.target {
                Target::Link(l) => bail!("{}", l.why()),
                Target::Local(_) => return Err(e.into()),
            },
        };
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
                if let Target::Link(l) = &self.target {
                    bail!("{}", l.why());
                }
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
        // Attaching grants one frame. Over a drive link, ask for a few in flight so scrolling
        // isn't held to one frame per Wi-Fi round trip; a local stream keeps the one.
        for _ in 1..self.frame_window() {
            st.want();
        }
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

    /// A stand-in midnad answering each call with `answer(method, params)` (`{"rpc_error": m}`
    /// answers with that error); it counts the calls.
    fn daemon_with(answer: impl Fn(&str, &Value) -> Value + Send + Sync + 'static) -> (PathBuf, Arc<Mutex<Vec<String>>>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("mdl-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("s");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let (calls, seen) = (Arc::new(Mutex::new(vec![])), Arc::new(answer));
        let log = calls.clone();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let (log, answer) = (log.clone(), seen.clone());
                std::thread::spawn(move || {
                    let mut w = s.try_clone().unwrap();
                    for line in BufReader::new(s).lines().map_while(Result::ok) {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let method = req["method"].as_str().unwrap_or_default().to_string();
                        log.lock().unwrap().push(method.clone());
                        let a = answer(&method, &req["params"]);
                        let out = match a.get("rpc_error") {
                            Some(m) => serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "error": { "code": 3, "message": m } }),
                            None => serde_json::json!({ "jsonrpc": "2.0", "id": req["id"], "result": a }),
                        };
                        let _ = writeln!(w, "{out}");
                    }
                });
            }
        });
        (sock, calls)
    }

    fn mini() -> Mac {
        Mac { peer: "f".repeat(64), name: "Morgan's Mac mini".into() }
    }

    fn drive_link(socket: &std::path::Path, state: &str, reason: Option<&str>) -> Value {
        serde_json::json!({ "link": "l_1", "peer": "f".repeat(64), "name": "Morgan's Mac mini", "socket": socket, "state": state, "reason": reason })
    }

    #[test]
    fn a_connecting_link_tells_the_window_how_far_it_got() {
        let gone = std::env::temp_dir().join(format!("mdl-connecting-{}", std::process::id()));
        let g = gone.clone();
        let (local, _) = daemon_with(move |m, _| match m {
            "drive.open" => drive_link(&g, "connecting", None),
            "drive.list" => {
                let mut l = drive_link(&g, "connecting", None);
                l["stage"] = "proving".into();
                serde_json::json!({ "links": [l] })
            }
            _ => Value::Null,
        });
        let here: Arc<dyn Backend> = Arc::new(DaemonBackend::new(local));
        let mini = here.open_mac(mini());
        let (tx, rx) = async_channel::unbounded();
        mini.subscribe(tx);
        let first = rx.recv_blocking().unwrap();
        let BackendEvent::Conn(ConnState::Link(p)) = first else { panic!("{first:?}") };
        assert_eq!(p, LinkProgress { state: DriveState::Connecting, stage: Some(midna_proto::nearby::DriveStage::Proving), reason: None });
    }

    #[test]
    fn another_macs_calls_go_through_its_drive_link() {
        let (remote, remote_calls) = daemon_with(|m, _| serde_json::json!({ "from": "mini", "method": m }));
        let r = remote.clone();
        let (local, local_calls) = daemon_with(move |m, p| match m {
            "drive.open" => {
                assert_eq!(p["peer"], "f".repeat(64));
                drive_link(&r, "connected", None)
            }
            _ => serde_json::json!({ "from": "here" }),
        });
        let here: Arc<dyn Backend> = Arc::new(DaemonBackend::new(local.clone()));
        let mini = here.clone().open_mac(mini());
        assert_eq!(mini.mac(), Some(&self::mini()));
        assert_eq!(mini.socket_path(), local, "app-state and other files stay this Mac's");
        assert_eq!(mini.call("session.list", Value::Null).unwrap()["from"], "mini");
        assert_eq!(mini.call("needs_you.list", Value::Null).unwrap()["from"], "mini");
        assert_eq!(here.call("session.list", Value::Null).unwrap()["from"], "here");
        assert!(Arc::ptr_eq(&crate::backend::local(&mini), &here), "the other Mac's backend knows this Mac's");
        assert_eq!(*local_calls.lock().unwrap(), ["drive.open", "session.list"], "the link opens once, then its socket is reused");
        assert_eq!(*remote_calls.lock().unwrap(), ["session.list", "needs_you.list"]);
    }

    #[test]
    fn a_link_that_is_down_says_why_with_the_macs_name() {
        let gone = std::env::temp_dir().join(format!("mdl-gone-{}", std::process::id()));
        let g = gone.clone();
        let (local, _) = daemon_with(move |m, _| match m {
            "drive.open" => drive_link(&g, "offline", None),
            "drive.list" => serde_json::json!({ "links": [drive_link(&g, "reconnecting", Some("the network dropped"))] }),
            _ => Value::Null,
        });
        let here: Arc<dyn Backend> = Arc::new(DaemonBackend::new(local));
        let mini = here.open_mac(mini());
        let e = mini.call("session.list", Value::Null).unwrap_err().to_string();
        assert_eq!(e, "session.list: reconnecting to Morgan's Mac mini: the network dropped");
    }

    #[test]
    fn a_closed_link_is_not_dialled() {
        let (local, _) = daemon_with(|m, _| match m {
            "drive.open" => drive_link(std::path::Path::new("/nowhere"), "closed", Some("Morgan's Mac mini stopped letting this Mac drive it")),
            "drive.list" => serde_json::json!({ "links": [] }),
            _ => Value::Null,
        });
        let here: Arc<dyn Backend> = Arc::new(DaemonBackend::new(local));
        let mini = here.open_mac(mini());
        let (tx, rx) = async_channel::unbounded();
        mini.subscribe(tx);
        let first = rx.recv_blocking().unwrap();
        let BackendEvent::Conn(ConnState::NotRunning { error, .. }) = first else { panic!("{first:?}") };
        assert_eq!(error, "Morgan's Mac mini stopped letting this Mac drive it");
    }

    #[test]
    fn a_refused_drive_open_says_why() {
        let (local, _) = daemon_with(|m, _| match m {
            "drive.open" => serde_json::json!({ "rpc_error": "Morgan's Mac mini isn't paired with this Mac" }),
            "drive.list" => serde_json::json!({ "links": [] }),
            _ => Value::Null,
        });
        let here: Arc<dyn Backend> = Arc::new(DaemonBackend::new(local));
        let e = here.open_mac(mini()).call("session.list", Value::Null).unwrap_err().to_string();
        assert_eq!(e, "session.list: Morgan's Mac mini isn't paired with this Mac");
    }
}
