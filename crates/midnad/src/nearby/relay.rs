//! Drive links: a window on this Mac driving another of my Macs (`docs/DECISIONS.md`, "Nearby:
//! driving another of my Macs").
//!
//! **This Mac** ([`open`], `drive.open`, only the verified app): makes `<home>/drive/<link>.sock`
//! (0600, directory 0700) and accepts on it only the app process that opened the link, checked on
//! every accept. One Noise connection to that Mac carries every connection the app makes to the
//! socket, each as its own stream, so the app's Backend works on it as on `midnad.sock`. The link
//! reports its stages and drops (event `drive.state`), and is tried again by itself after a drop:
//! soon at first, every 30 s once it has failed for a minute. While it is down a new connection
//! to the socket gets an error line and closes; nothing is queued for later.
//!
//! **That Mac** ([`serve`], when a paired connection sends `drive_open`): registers the link with
//! [`drive::open`] (refused unless that device may drive this Mac), then serves each stream as an
//! ordinary client connection ([`crate::conn::serve_peer`]: agent role, actor `via: peer`). The
//! permission is checked again on every message, and [`drive::changed`] cuts the link through
//! its closer.
//!
//! After `drive_ok` each side sends raw Noise messages ([`transport::RAW_MAX`] at most): a tag byte,
//! then for [`OPEN`], [`DATA`] and [`CLOSE`] a big-endian `u32` stream id. Bytes are cut into
//! messages as they are read, so terminal frames go out as soon as midnad writes them and nothing
//! comes near the 1 MiB `Wire` cap. Either side pings every [`PING_EVERY`] and drops the link after
//! [`SILENCE`] without a message, which also keeps it clear of the listener's 30 s idle close.
//! A stream whose reader stops reading is cut off once [`MAX_PENDING`] bytes wait for it, as
//! `conn.rs` cuts off a slow client, so one stuck window never holds up the link.
use super::link::{self, Reach};
use super::transport::{self, Channel, RawReader, RawWriter, Wire};
use super::drive;
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{DriveCloseParams, DriveCloseResult, DriveLink, DriveOpenParams, DriveStage, DriveState, Tier};
use midna_proto::types::Actor;
use midna_proto::{RpcError, kinds};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// The driving side opened stream `sid`.
pub const OPEN: u8 = 1;
/// Bytes on stream `sid`.
pub const DATA: u8 = 2;
/// Stream `sid` ended (either side).
pub const CLOSE: u8 = 3;
pub const PING: u8 = 4;
pub const PONG: u8 = 5;
/// The link is closing; the rest is why (UTF-8).
pub const BYE: u8 = 6;

/// How often each side pings.
pub const PING_EVERY: Duration = Duration::from_secs(5);
/// A link with nothing from the other side for this long is dropped.
pub const SILENCE: Duration = Duration::from_secs(20);
/// A write to the other Mac blocked this long drops the link.
const WRITE_TIMEOUT: Duration = Duration::from_secs(20);
/// A write to a local stream blocked this long ends that stream.
const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Bytes waiting for one stream's reader before it is cut off.
pub const MAX_PENDING: usize = 64 << 20;
/// Streams open on one link at once (a window's control connections and one per terminal it shows).
pub const MAX_STREAMS: usize = 256;
/// The bytes one `DATA` message carries.
const DATA_MAX: usize = transport::RAW_MAX - 5;
/// How long the read loop waits before it looks at the time (pings, checks).
const TICK: Duration = Duration::from_secs(1);
/// After this long failing to connect, a link is `offline` and tried every [`OFFLINE_RETRY`].
const OFFLINE_AFTER: Duration = Duration::from_secs(60);
const OFFLINE_RETRY: Duration = Duration::from_secs(30);
/// The longest wait between tries before a link is offline.
const MAX_BACKOFF: Duration = Duration::from_secs(8);

/// That Mac's refusal of `drive_open` when it doesn't let this one drive it, and its `bye` when
/// that permission goes away. The driving side words it with that Mac's name.
pub const NOT_ALLOWED: &str = "drive_not_allowed";
/// That Mac's `bye` when its midnad stops.
const STOPPING: &str = "drive_stopping";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn message(tag: u8, sid: u32, data: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(5 + data.len());
    m.push(tag);
    m.extend_from_slice(&sid.to_be_bytes());
    m.extend_from_slice(data);
    m
}

/// What one local stream's writer thread is given.
enum Item {
    Data(Vec<u8>),
    /// The other side ended the stream: write what came before, then close.
    End,
}

struct Stream {
    tx: Sender<Item>,
    pending: Arc<AtomicUsize>,
    sock: UnixStream,
}

/// One drive link's connection to the other Mac, with the streams it carries. Both sides use it.
pub struct Mux {
    writer: Mutex<RawWriter>,
    tcp: TcpStream,
    streams: Mutex<HashMap<u32, Stream>>,
    done: AtomicBool,
    why: Mutex<Option<String>>,
}

impl Mux {
    fn new(writer: RawWriter) -> Result<Arc<Mux>, String> {
        let tcp = writer.stream().map_err(|e| e.to_string())?;
        Ok(Arc::new(Mux { writer: Mutex::new(writer), tcp, streams: Mutex::new(HashMap::new()), done: AtomicBool::new(false), why: Mutex::new(None) }))
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// Why it closed.
    fn why(&self) -> String {
        lock(&self.why).clone().unwrap_or_else(|| "the connection closed".into())
    }

    fn send(&self, tag: u8, sid: u32, data: &[u8]) -> bool {
        self.send_raw(&message(tag, sid, data))
    }

    fn send_raw(&self, m: &[u8]) -> bool {
        if self.is_done() {
            return false;
        }
        let sent = lock(&self.writer).send(m);
        if let Err(e) = &sent {
            self.close(&format!("the connection to that Mac broke ({e})"), false);
        }
        sent.is_ok()
    }

    /// Close the link and every stream on it; `tell` sends the other side why first. Once.
    pub fn close(&self, why: &str, tell: bool) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        *lock(&self.why) = Some(why.to_string());
        // Don't wait behind a write that's stuck: then the other side learns it from the close.
        if tell && let Ok(mut w) = self.writer.try_lock() {
            let _ = self.tcp.set_write_timeout(Some(Duration::from_secs(1)));
            let mut m = vec![BYE];
            m.extend_from_slice(&why.as_bytes()[..why.len().min(1024)]);
            let _ = w.send(&m);
        }
        let _ = self.tcp.shutdown(Shutdown::Both);
        for (_, s) in lock(&self.streams).drain() {
            let _ = s.sock.shutdown(Shutdown::Both);
        }
    }

    /// Carry local stream `sock` as `sid`: what it says goes to the other side, what comes for
    /// `sid` is written to it. `announce`: tell the other side it opened. False if the link is
    /// closed or carries too many streams already.
    fn attach(self: &Arc<Self>, sid: u32, sock: UnixStream, announce: bool) -> bool {
        let (Ok(mut rd), Ok(mut wr), Ok(ctl)) = (sock.try_clone(), sock.try_clone(), sock.try_clone()) else { return false };
        let (tx, rx) = channel();
        let pending = Arc::new(AtomicUsize::new(0));
        {
            let mut streams = lock(&self.streams);
            if self.is_done() || streams.len() >= MAX_STREAMS || streams.contains_key(&sid) {
                return false;
            }
            streams.insert(sid, Stream { tx, pending: pending.clone(), sock: ctl });
        }
        if announce && !self.send(OPEN, sid, &[]) {
            return false;
        }
        let _ = wr.set_write_timeout(Some(STREAM_WRITE_TIMEOUT));
        let m = self.clone();
        let out = std::thread::Builder::new().name("drive-out".into()).spawn(move || {
            for item in rx {
                match item {
                    Item::Data(b) => {
                        let ok = wr.write_all(&b).is_ok();
                        pending.fetch_sub(b.len(), Ordering::AcqRel);
                        if !ok {
                            m.end(sid, true);
                            return;
                        }
                    }
                    Item::End => break,
                }
            }
            let _ = wr.shutdown(Shutdown::Both);
        });
        let m = self.clone();
        let inp = std::thread::Builder::new().name("drive-in".into()).spawn(move || {
            let mut buf = vec![0u8; 5 + DATA_MAX];
            buf[0] = DATA;
            buf[1..5].copy_from_slice(&sid.to_be_bytes());
            loop {
                match rd.read(&mut buf[5..]) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !m.send_raw(&buf[..5 + n]) {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            m.end(sid, true);
        });
        if out.is_err() || inp.is_err() {
            self.end(sid, true);
            return false;
        }
        true
    }

    /// Stream `sid` ended here: close it and, with `tell`, say so to the other side (once).
    fn end(&self, sid: u32, tell: bool) {
        let gone = lock(&self.streams).remove(&sid);
        if let Some(s) = gone {
            let _ = s.sock.shutdown(Shutdown::Both);
            if tell {
                self.send(CLOSE, sid, &[]);
            }
        }
    }

    /// Bytes for `sid` came in.
    fn deliver(&self, sid: u32, data: &[u8]) {
        let over = {
            let streams = lock(&self.streams);
            let Some(s) = streams.get(&sid) else { return };
            let n = data.len();
            s.pending.fetch_add(n, Ordering::AcqRel) + n > MAX_PENDING || s.tx.send(Item::Data(data.to_vec())).is_err()
        };
        if over {
            self.end(sid, true);
        }
    }

    /// The other side ended `sid`: write what it sent, then close it here.
    fn ended(&self, sid: u32) {
        if let Some(s) = lock(&self.streams).remove(&sid) {
            let _ = s.tx.send(Item::End);
        }
    }

    /// Streams open now.
    pub fn streams(&self) -> usize {
        lock(&self.streams).len()
    }

    /// Read the other side until the link closes. `check` runs on every message and every
    /// [`TICK`] and ends the link with its error; `on` handles OPEN, DATA and CLOSE. Returns why
    /// the link ended.
    fn run(self: &Arc<Self>, mut rd: RawReader, check: impl Fn() -> Result<(), String>, mut on: impl FnMut(&Arc<Mux>, u8, u32, &[u8])) -> String {
        let mut heard = Instant::now();
        let mut pinged = Instant::now();
        while !self.is_done() {
            match rd.recv(TICK) {
                Ok(Some(m)) => {
                    heard = Instant::now();
                    if let Err(why) = check() {
                        self.close(&why, true);
                        break;
                    }
                    let Some((&tag, rest)) = m.split_first() else { continue };
                    match tag {
                        PING => {
                            self.send(PONG, 0, &[]);
                        }
                        PONG => {}
                        BYE => {
                            self.close(&String::from_utf8_lossy(rest), false);
                            break;
                        }
                        OPEN | DATA | CLOSE if rest.len() >= 4 => {
                            let sid = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
                            on(self, tag, sid, &rest[4..]);
                        }
                        _ => {}
                    }
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    self.close(&format!("the connection to that Mac closed ({e})"), false);
                    break;
                }
            }
            if let Err(why) = check() {
                self.close(&why, true);
                break;
            }
            if heard.elapsed() >= SILENCE {
                self.close("that Mac stopped answering", true);
                break;
            }
            if pinged.elapsed() >= PING_EVERY {
                pinged = Instant::now();
                self.send(PING, 0, &[]);
            }
        }
        self.why()
    }
}

// ------------------------------------------------------------------ that Mac: serving a link

/// A paired device sent `drive_open` on `ch`: serve the link while it may drive this Mac.
pub fn serve(w: &Weak<Daemon>, mut ch: Channel) {
    let Some(d) = w.upgrade() else { return };
    let fp = ch.fingerprint.clone();
    let Some(handle) = drive::open(&d, &fp) else {
        let _ = ch.send(&Wire::Refused { reason: NOT_ALLOWED.into() });
        return ch.finish();
    };
    if ch.send(&Wire::DriveOk).is_err() {
        return;
    }
    let Ok((rd, wr)) = ch.split(WRITE_TIMEOUT) else { return };
    let Ok(mux) = Mux::new(wr) else { return };
    let m = mux.clone();
    // The cut's own words are for the person here; the other side says it with this Mac's name.
    handle.on_cut(move |_| m.close(NOT_ALLOWED, true));
    drop(d);
    let check = || {
        let d = w.upgrade().ok_or(STOPPING)?;
        if d.shutting_down.load(Ordering::Relaxed) {
            return Err(STOPPING.to_string());
        }
        if handle.is_cut() || !link::trusted(&d, &fp) || !drive::allowed(&d, &fp) {
            return Err(NOT_ALLOWED.to_string());
        }
        Ok(())
    };
    mux.run(rd, check, |mux, tag, sid, data| match tag {
        OPEN => {
            let Some(d) = w.upgrade() else { return };
            let Ok((ours, theirs)) = UnixStream::pair() else {
                mux.send(CLOSE, sid, &[]);
                return;
            };
            if !mux.attach(sid, ours, false) {
                mux.send(CLOSE, sid, &[]);
                return;
            }
            let fp = fp.clone();
            let conn = d.conn_id();
            let spawned = std::thread::Builder::new().name("drive-conn".into()).spawn(move || crate::conn::serve_peer(d, theirs, conn, &fp));
            if spawned.is_err() {
                mux.end(sid, true);
            }
        }
        DATA => mux.deliver(sid, data),
        _ => mux.ended(sid),
    });
    drop(handle);
}

// ------------------------------------------------------------------ this Mac: driving another

/// The drive links this Mac has open to others, by link id.
#[derive(Default)]
pub struct Outgoing {
    links: Mutex<HashMap<String, Arc<Out>>>,
}

/// The app process that opened a link: only it may use the link, and the link closes when it
/// quits.
struct Opener {
    pid: i32,
    exe: String,
}

impl Opener {
    fn alive(&self) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        let exists = unsafe { libc::kill(self.pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        exists && crate::peer::pid_path(self.pid).as_deref() == Some(self.exe.as_str())
    }
}

/// One link this Mac opened.
struct Out {
    status: Mutex<DriveLink>,
    opener: Opener,
    socket: PathBuf,
    mux: Mutex<Option<Arc<Mux>>>,
    closed: AtomicBool,
    /// Set and notified to try again now (or to notice it closed).
    wake: (Mutex<bool>, Condvar),
    next_sid: AtomicU32,
    /// When the first of the current run of failed tries began.
    failing_since: Mutex<Option<Instant>>,
}

impl Out {
    fn id(&self) -> String {
        lock(&self.status).link.clone()
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn poke(&self) {
        *lock(&self.wake.0) = true;
        self.wake.1.notify_all();
    }

    /// Wait up to `t`, or until poked.
    fn sleep(&self, t: Duration) {
        let mut g = lock(&self.wake.0);
        let deadline = Instant::now() + t;
        while !*g && !self.is_closed() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self.wake.1.wait_timeout(g, left).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        *g = false;
    }

    /// Change the link's status; say so (event `drive.state`) when its state, stage or reason
    /// changed.
    fn set(&self, d: &Daemon, f: impl FnOnce(&mut DriveLink)) {
        let now = {
            let mut s = lock(&self.status);
            let before = (s.state, s.stage, s.reason.clone());
            f(&mut s);
            if s.state != DriveState::Connecting {
                s.stage = None;
            }
            (before != (s.state, s.stage, s.reason.clone())).then(|| s.clone())
        };
        if let Some(s) = now {
            d.emit(kinds::DRIVE_STATE, Actor::system(), None, None, serde_json::to_value(s).unwrap_or_default());
        }
    }
}

fn outgoing(d: &Daemon) -> MutexGuard<'_, HashMap<String, Arc<Out>>> {
    lock(&d.nearby.relay.links)
}

/// The directory drive link sockets live in.
fn socket_dir(d: &Daemon) -> PathBuf {
    d.cfg.home.join("drive")
}

/// Startup: remove the sockets a previous run left.
pub fn clear_stale(d: &Daemon) {
    if let Ok(rd) = std::fs::read_dir(socket_dir(d)) {
        for e in rd.flatten() {
            if e.path().extension().is_some_and(|x| x == "sock") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// `drive.list`.
pub fn list(d: &Daemon) -> Vec<DriveLink> {
    let mut v: Vec<DriveLink> = outgoing(d).values().map(|o| lock(&o.status).clone()).collect();
    v.sort_by(|a, b| a.link.cmp(&b.link));
    v
}

/// `drive.open` from the verified app (the caller checked), process `pid`: a link to `peer`.
pub fn open(d: &Arc<Daemon>, peer: &str, pid: i32) -> Result<DriveLink, RpcError> {
    let exe = crate::peer::pid_path(pid).ok_or_else(|| RpcError::refused("can't tell which app is asking"))?;
    let name = {
        let mut i = d.nearby.inner();
        let p = i.store(&d.cfg.home).get(peer).cloned().ok_or_else(|| RpcError::not_found("that device isn't paired with this Mac"))?;
        if p.tier != Tier::MyDevice {
            return Err(RpcError::refused(format!("{} is kept as a contact here: only my devices can be driven (keep it as my device first)", p.name)));
        }
        p.name
    };
    if !link::trusted(d, peer) {
        return Err(RpcError::refused("Nearby is off on this Mac (visibility off): turn it on to drive another Mac"));
    }
    if let Some(o) = outgoing(d).values().find(|o| lock(&o.status).peer == peer && o.opener.pid == pid && !o.is_closed()).cloned() {
        o.poke();
        return Ok(lock(&o.status).clone());
    }
    let dir = socket_dir(d);
    crate::server::secure_dir(&dir).map_err(|e| RpcError::internal(format!("making {}: {e}", dir.display())))?;
    let id = format!("dl_{}", transport::hex(&transport::random::<4>()));
    let socket = dir.join(format!("{id}.sock"));
    let _ = std::fs::remove_file(&socket);
    let listener = bind(&socket).map_err(|e| RpcError::internal(format!("making the link's socket {}: {e}", socket.display())))?;
    let status = DriveLink {
        link: id.clone(),
        peer: peer.to_string(),
        name,
        socket: socket.display().to_string(),
        state: DriveState::Connecting,
        stage: Some(DriveStage::Finding),
        reason: None,
        last_seen: None,
        drops: 0,
    };
    let out = Arc::new(Out {
        status: Mutex::new(status.clone()),
        opener: Opener { pid, exe },
        socket,
        mux: Mutex::new(None),
        closed: AtomicBool::new(false),
        wake: (Mutex::new(false), Condvar::new()),
        next_sid: AtomicU32::new(1),
        failing_since: Mutex::new(None),
    });
    outgoing(d).insert(id, out.clone());
    d.emit(kinds::DRIVE_STATE, Actor::system(), None, None, serde_json::to_value(&status).unwrap_or_default());
    let w = Arc::downgrade(d);
    let o = out.clone();
    let accepting = std::thread::Builder::new().name("drive-accept".into()).spawn(move || accept(&w, &o, listener));
    let w = Arc::downgrade(d);
    let o = out.clone();
    let running = std::thread::Builder::new().name("drive-link".into()).spawn(move || keep_up(&w, &o));
    if accepting.is_err() || running.is_err() {
        close_out(d, &out, "midnad couldn't start the link");
        return Err(RpcError::internal("couldn't start the link"));
    }
    Ok(status)
}

/// `drive.open`: only the verified app on this Mac.
pub fn open_rpc(d: &Arc<Daemon>, ctx: &Ctx, p: DriveOpenParams) -> R {
    let pid = ctx.pid.filter(|_| ctx.app).ok_or_else(app_only)?;
    ok(open(d, p.peer.trim(), pid)?)
}

/// `drive.close`.
pub fn close_rpc(d: &Daemon, ctx: &Ctx, p: DriveCloseParams) -> R {
    if !ctx.app {
        return Err(app_only());
    }
    ok(DriveCloseResult { closed: close(d, &p.link) })
}

fn app_only() -> RpcError {
    RpcError::human_only("only the Midna app can open or close a link to drive another Mac")
}

/// `drive.close`: false if there's no such link.
pub fn close(d: &Daemon, link: &str) -> bool {
    let Some(o) = outgoing(d).get(link).cloned() else { return false };
    close_out(d, &o, "closed");
    true
}

fn close_out(d: &Daemon, o: &Arc<Out>, why: &str) {
    if o.closed.swap(true, Ordering::SeqCst) {
        return;
    }
    outgoing(d).remove(&o.id());
    if let Some(m) = lock(&o.mux).take() {
        m.close(why, true);
    }
    o.poke();
    // Wake the accept loop so it sees the link closed.
    let _ = UnixStream::connect(&o.socket);
    let _ = std::fs::remove_file(&o.socket);
    o.set(d, |s| {
        s.state = DriveState::Closed;
        s.reason = Some(why.to_string());
    });
}

/// Bind with mode 0600 (the umask tightened around bind, as `server.rs` does).
fn bind(socket: &Path) -> std::io::Result<UnixListener> {
    // SAFETY: umask only changes this process's file-creation mask.
    let old = unsafe { libc::umask(0o077) };
    let l = UnixListener::bind(socket);
    unsafe { libc::umask(old) };
    let l = l?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Accept the app's connections on the link's socket and carry each over the link.
fn accept(w: &Weak<Daemon>, o: &Arc<Out>, listener: UnixListener) {
    for conn in listener.incoming() {
        let Some(d) = w.upgrade() else { return };
        if o.is_closed() || d.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut conn) = conn else { continue };
        let fd = conn.as_raw_fd();
        let pid = crate::peer::peer_pid(fd);
        if pid != Some(o.opener.pid) || crate::conn::base_role(&d, fd, pid) != crate::rpc::Role::Human {
            refuse(&mut conn, "only the Midna app that opened this drive link may use it");
            continue;
        }
        let mux = lock(&o.mux).clone();
        let Some(mux) = mux.filter(|m| !m.is_done()) else {
            let name = lock(&o.status).name.clone();
            refuse(&mut conn, &format!("not connected to {name} now; the link is being tried again"));
            continue;
        };
        let sid = o.next_sid.fetch_add(1, Ordering::SeqCst);
        if !mux.attach(sid, conn, true) {
            continue;
        }
    }
}

/// Answer a connection that may not use the link with one JSON-RPC error line, then close it.
fn refuse(conn: &mut UnixStream, why: &str) {
    let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
    let line = crate::conn::response(&serde_json::Value::Null, Err(RpcError::refused(why)));
    let _ = conn.write_all(format!("{line}\n").as_bytes());
    let _ = conn.shutdown(Shutdown::Both);
}

/// Keep the link up until it's closed: connect, serve until it drops, try again.
fn keep_up(w: &Weak<Daemon>, o: &Arc<Out>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        let Some(d) = w.upgrade() else { return };
        if o.is_closed() {
            return;
        }
        if d.shutting_down.load(Ordering::Relaxed) {
            return close_out(&d, o, "midnad stopped");
        }
        if !o.opener.alive() {
            return close_out(&d, o, "the app that opened it quit");
        }
        let peer = lock(&o.status).peer.clone();
        let offline = lock(&o.failing_since).is_some_and(|t| t.elapsed() >= OFFLINE_AFTER);
        let staged = |stage: DriveStage| {
            o.set(&d, |s| {
                if s.state == DriveState::Connecting {
                    s.stage = Some(stage);
                }
            })
        };
        let up = connect(&d, &peer, &staged).and_then(|ch| {
            let (rd, wr) = ch.split(WRITE_TIMEOUT)?;
            Ok((rd, Mux::new(wr)?))
        });
        match up {
            Ok((rd, mux)) => {
                backoff = Duration::from_secs(1);
                *lock(&o.failing_since) = None;
                *lock(&o.mux) = Some(mux.clone());
                if o.is_closed() {
                    mux.close("closed", true);
                    return;
                }
                o.set(&d, |s| {
                    s.state = DriveState::Connected;
                    s.reason = None;
                    s.last_seen = Some(midna_proto::time::now_rfc3339());
                });
                drop(d);
                let why = serve_out(w, o, &mux, rd);
                lock(&o.mux).take();
                let Some(d) = w.upgrade() else { return };
                let name = lock(&o.status).name.clone();
                let why = match why.as_str() {
                    NOT_ALLOWED => format!("{name} stopped letting this Mac drive it"),
                    STOPPING => format!("Midna on {name} stopped"),
                    _ => why,
                };
                if o.is_closed() {
                    return;
                }
                *lock(&o.failing_since) = Some(Instant::now());
                o.set(&d, |s| {
                    s.state = DriveState::Reconnecting;
                    s.reason = Some(why);
                    s.last_seen = Some(midna_proto::time::now_rfc3339());
                    s.drops += 1;
                });
                // Try again at once: a blip shouldn't cost a second.
                continue;
            }
            Err(why) => {
                let since = *lock(&o.failing_since).get_or_insert_with(Instant::now);
                let gone = since.elapsed() >= OFFLINE_AFTER;
                o.set(&d, |s| {
                    if gone {
                        s.state = DriveState::Offline;
                    }
                    s.reason = Some(why);
                });
                drop(d);
                let wait = if gone || offline { OFFLINE_RETRY } else { backoff };
                backoff = (backoff * 2).min(MAX_BACKOFF);
                o.sleep(wait);
            }
        }
    }
}

/// Connect to `peer` and ask to drive it, reporting each stage.
fn connect(d: &Daemon, peer: &str, stage: &dyn Fn(DriveStage)) -> Result<Channel, String> {
    let reach = Reach { finding: &|| stage(DriveStage::Finding), proving: &|| stage(DriveStage::Proving), patient: false };
    let mut ch = link::reach_staged(d, peer, &reach).map_err(|e| e.message)?;
    ch.send(&Wire::DriveOpen)?;
    match ch.expect(transport::HANDSHAKE_TIMEOUT)? {
        Wire::DriveOk => Ok(ch),
        Wire::Refused { reason } if reason == NOT_ALLOWED => {
            let name = lock_name(d, peer);
            Err(format!("{name} doesn't let this Mac drive it: turn it on at {name} in Settings › Nearby"))
        }
        Wire::Refused { reason } if reason == transport::NOT_PAIRED => Err(format!("{} doesn't know this Mac any more (pair again)", lock_name(d, peer))),
        Wire::Refused { reason } => Err(format!("{} refused: {reason}", lock_name(d, peer))),
        Wire::Unknown => Err(format!("{}'s Midna is too old to be driven", lock_name(d, peer))),
        _ => Err("an unexpected answer".into()),
    }
}

fn lock_name(d: &Daemon, peer: &str) -> String {
    let stored = d.nearby.inner().store(&d.cfg.home).get(peer).map(|s| s.name.clone());
    stored.unwrap_or_else(|| midna_proto::nearby::short_fingerprint(peer))
}

/// Relay the app's streams over a connected link until it drops; why it dropped.
fn serve_out(w: &Weak<Daemon>, o: &Arc<Out>, mux: &Arc<Mux>, rd: RawReader) -> String {
    let check = || {
        let d = w.upgrade().ok_or("midnad stopped")?;
        if o.is_closed() {
            return Err("closed".to_string());
        }
        if d.shutting_down.load(Ordering::Relaxed) {
            return Err("midnad stopped".to_string());
        }
        Ok(())
    };
    let looked = std::cell::Cell::new(Instant::now());
    let why = mux.run(
        rd,
        || {
            check()?;
            // Looked at now and then (not on every message), so a quit app's link closes
            // without waiting for a drop.
            if looked.get().elapsed() >= PING_EVERY {
                looked.set(Instant::now());
                if !o.opener.alive() {
                    return Err("the app that opened it quit".to_string());
                }
            }
            Ok(())
        },
        |mux, tag, sid, data| match tag {
            DATA => mux.deliver(sid, data),
            CLOSE => mux.ended(sid),
            _ => {}
        },
    );
    if !o.opener.alive()
        && let Some(d) = w.upgrade()
    {
        close_out(&d, o, "the app that opened it quit");
    }
    why
}
