//! Drive links: a window on this Mac driving another of my Macs (`docs/DECISIONS.md`, "Nearby:
//! driving another of my Macs").
//!
//! **This Mac** ([`open`], `drive.open`, only the verified app): makes `<home>/drive/<link>.sock`
//! (0600, directory 0700) and accepts on it only the app process that opened the link, checked on
//! every accept. One Noise connection to that Mac carries every connection the app makes to the
//! socket, each as its own stream, so the app's Backend works on it as on `midnad.sock`. The link
//! reports its stages and drops (event `drive.state`), and is tried again by itself after a drop:
//! soon at first, every 30 s once it has failed for a minute. Then a link to a Mac Bonjour can't
//! see is parked (`offline`, its reason saying it's paused): nothing is dialed until that Mac is
//! seen again ([`seen_again`]) or this Mac's network changes ([`network_changed`]). A link that
//! Mac refuses ([`NOT_ALLOWED`]) is parked at once, until that Mac lets this one drive it (it
//! says so, `drive_allowed`: [`allowed`]). Opening it again (`drive.open`, Try now) tries any
//! parked link now. While it is down a new connection to the socket gets an error line and
//! closes; nothing is queued for later.
//!
//! **That Mac** ([`serve`], when a paired connection sends `drive_open`): registers the link with
//! [`drive::open`] (refused unless that device may drive this Mac), then serves each stream as an
//! ordinary client connection ([`crate::conn::serve_peer`]: the human here, actor `via: peer`, minus
//! [`super::local_only`]). The
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
//!
//! When both sides offer [`LZ4`] (`drive_open` / `drive_ok`), bytes worth packing go as [`ZDATA`]
//! instead: a full-screen frame (~100 KB at 180x50) becomes a few KB. Short messages such as
//! keystrokes stay plain [`DATA`], and a Mac on an older build gets plain DATA only.
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
/// Bytes on stream `sid`, packed (only on a link that agreed on [`LZ4`]): after the stream id,
/// the unpacked length (big-endian `u32`, at most [`ZDATA_MAX`]), then one lz4 block.
pub const ZDATA: u8 = 7;
/// The compression a link can agree on (`drive_open`'s `compress`, `drive_ok`'s answer).
pub const LZ4: &str = "lz4";

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
/// The most one [`ZDATA`] message unpacks to, and the most read from a local stream at once on a
/// link that packs. A peer can't make one message cost more memory than this.
const ZDATA_MAX: usize = 256 << 10;
/// Bytes shorter than this go plain: packing a keystroke saves nothing and costs time.
const PACK_FROM: usize = 512;
/// How long the read loop waits before it looks at the time (pings, checks).
const TICK: Duration = Duration::from_secs(1);
/// After this long failing to connect, a link is `offline`: tried every [`OFFLINE_RETRY`] while
/// that Mac can be seen, parked while it can't.
const OFFLINE_AFTER: Duration = Duration::from_secs(60);
/// How often an offline link that Mac can be seen is tried, and a parked one looked at (nothing
/// dialed: has the app quit, is that Mac seen now).
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

/// `data` (at most [`ZDATA_MAX`] bytes) as one [`ZDATA`] message for `sid` in `out`, when packing
/// makes it smaller and the result fits in one raw message.
fn pack(sid: u32, data: &[u8], out: &mut Vec<u8>) -> bool {
    debug_assert!(data.len() <= ZDATA_MAX);
    out.clear();
    out.push(ZDATA);
    out.extend_from_slice(&sid.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let head = out.len();
    out.resize(head + lz4_flex::block::get_maximum_output_size(data.len()), 0);
    match lz4_flex::block::compress_into(data, &mut out[head..]) {
        Ok(n) if n < data.len() && head + n <= transport::RAW_MAX => {
            out.truncate(head + n);
            true
        }
        _ => false,
    }
}

/// The bytes a [`ZDATA`] message (what follows its stream id) carries; None if it's malformed or
/// claims more than [`ZDATA_MAX`].
fn unpack(z: &[u8]) -> Option<Vec<u8>> {
    let (len, block) = z.split_first_chunk::<4>()?;
    let len = u32::from_be_bytes(*len) as usize;
    if len > ZDATA_MAX {
        return None;
    }
    let mut out = vec![0u8; len];
    let n = lz4_flex::block::decompress_into(block, &mut out).ok()?;
    (n == len).then_some(out)
}

/// Let a local stream hold a whole frame or two, so one read on a packing link takes most of a
/// frame (packs better, fewer messages) instead of the system's 8 KiB at a time.
fn widen(sock: &UnixStream) {
    let size = ZDATA_MAX as libc::c_int;
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY: setsockopt reads one c_int from a valid pointer on a socket this owns.
        unsafe { libc::setsockopt(sock.as_raw_fd(), libc::SOL_SOCKET, opt, (&size as *const libc::c_int).cast(), size_of::<libc::c_int>() as libc::socklen_t) };
    }
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
    /// Both sides agreed on [`LZ4`]: send [`ZDATA`] where it pays, and take it.
    lz4: bool,
}

impl Mux {
    fn new(writer: RawWriter, lz4: bool) -> Result<Arc<Mux>, String> {
        let tcp = writer.stream().map_err(|e| e.to_string())?;
        Ok(Arc::new(Mux { writer: Mutex::new(writer), tcp, streams: Mutex::new(HashMap::new()), done: AtomicBool::new(false), why: Mutex::new(None), lz4 }))
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

    /// Send bytes read from local stream `sid`: packed where the link agreed to it and it pays,
    /// else as plain [`DATA`] messages of at most [`DATA_MAX`] bytes. `out` is scratch space.
    fn send_data(&self, sid: u32, data: &[u8], out: &mut Vec<u8>) -> bool {
        if self.lz4 && data.len() >= PACK_FROM && pack(sid, data, out) {
            return self.send_raw(out);
        }
        if data.len() > DATA_MAX {
            return data.chunks(DATA_MAX).all(|piece| self.send_data(sid, piece, out));
        }
        out.clear();
        out.push(DATA);
        out.extend_from_slice(&sid.to_be_bytes());
        out.extend_from_slice(data);
        self.send_raw(out)
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
            let mut buf = vec![0u8; if m.lz4 { ZDATA_MAX } else { DATA_MAX }];
            let mut out = Vec::with_capacity(transport::RAW_MAX);
            loop {
                match rd.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !m.send_data(sid, &buf[..n], &mut out) {
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
                        ZDATA if rest.len() >= 4 => {
                            let sid = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
                            // Unpacked here, so `on` sees plain DATA and MAX_PENDING counts what
                            // the stream will really hold.
                            let Some(plain) = self.lz4.then(|| unpack(&rest[4..])).flatten() else {
                                self.close("that Mac sent data that doesn't unpack", true);
                                break;
                            };
                            on(self, DATA, sid, &plain);
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

/// A paired device sent `drive_open` on `ch`, offering `compress`: serve the link while it may
/// drive this Mac.
pub fn serve(w: &Weak<Daemon>, mut ch: Channel, compress: &[String]) {
    let Some(d) = w.upgrade() else { return };
    let fp = ch.fingerprint.clone();
    let Some(handle) = drive::open(&d, &fp) else {
        let _ = ch.send(&Wire::Refused { reason: NOT_ALLOWED.into() });
        return ch.finish();
    };
    let lz4 = compress.iter().any(|c| c == LZ4);
    if ch.send(&Wire::DriveOk { compress: lz4.then(|| LZ4.to_string()) }).is_err() {
        return;
    }
    let Ok((rd, wr)) = ch.split(WRITE_TIMEOUT) else { return };
    let Ok(mux) = Mux::new(wr, lz4) else { return };
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
            if mux.lz4 {
                widen(&ours);
                widen(&theirs);
            }
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
    /// Devices this Mac just let drive it, to be told so ([`tell_owed`]).
    owed: Mutex<Vec<String>>,
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
    /// Why it's parked, while it is: nothing is dialed until it's woken.
    parked: Mutex<Option<Fail>>,
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

    /// Wait up to `t`, or until poked; whether it was poked.
    fn sleep(&self, t: Duration) -> bool {
        let mut g = lock(&self.wake.0);
        let deadline = Instant::now() + t;
        while !*g && !self.is_closed() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            g = self.wake.1.wait_timeout(g, left).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        std::mem::replace(&mut *g, false)
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
        parked: Mutex::new(None),
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
            let (name, reason) = { let s = lock(&o.status); (s.name.clone(), s.reason.clone().unwrap_or_default()) };
            let why = match lock(&o.parked).is_some() {
                true => format!("not connected to {name} now ({reason})"),
                false => format!("not connected to {name} now; the link is being tried again"),
            };
            refuse(&mut conn, &why);
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
        let staged = |stage: DriveStage| {
            o.set(&d, |s| {
                if s.state == DriveState::Connecting {
                    s.stage = Some(stage);
                }
            })
        };
        let up = connect(&d, &peer, &staged).and_then(|(ch, lz4)| {
            let (rd, wr) = ch.split(WRITE_TIMEOUT).map_err(Failed::from)?;
            Ok((rd, Mux::new(wr, lz4)?))
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
            Err(Failed { kind, why }) => {
                let failing = lock(&o.failing_since).get_or_insert_with(Instant::now).elapsed();
                let next = after(kind, failing, backoff);
                let name = lock(&o.status).name.clone();
                if next == After::Park {
                    *lock(&o.parked) = Some(kind);
                }
                o.set(&d, |s| {
                    if failing >= OFFLINE_AFTER || next == After::Park {
                        s.state = DriveState::Offline;
                    }
                    s.reason = Some(if next == After::Park { paused(kind, &why, &name) } else { why });
                });
                drop(d);
                match next {
                    After::Retry(wait) => {
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        o.sleep(wait);
                    }
                    After::Park => {
                        park(w, o, kind, &peer);
                        // Woken: that Mac is back, or something changed. Tried as a fresh run.
                        *lock(&o.parked) = None;
                        *lock(&o.failing_since) = None;
                        backoff = Duration::from_secs(1);
                    }
                }
            }
        }
    }
}

/// Why a try failed, as far as what the link does next goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fail {
    /// Bonjour doesn't see that Mac: there's nothing to dial.
    Unseen,
    /// That Mac doesn't let this one drive it.
    NotAllowed,
    /// Anything else: seen but not answering, refused for another reason, a broken handshake.
    Other,
}

/// A failed try: its kind, and why in words.
struct Failed {
    kind: Fail,
    why: String,
}

impl From<String> for Failed {
    fn from(why: String) -> Self {
        Failed { kind: Fail::Other, why }
    }
}

/// What a link does after a try that failed.
#[derive(Debug, PartialEq, Eq)]
enum After {
    /// Try again after this long.
    Retry(Duration),
    /// Dial nothing until it's woken ([`park`]).
    Park,
}

/// After a try that failed with `kind`, the link having failed for `failing`, its backoff now
/// `backoff`: a refusal parks at once; a Mac that can't be seen, once the link is offline;
/// anything else is tried again, every [`OFFLINE_RETRY`] once offline.
fn after(kind: Fail, failing: Duration, backoff: Duration) -> After {
    match kind {
        Fail::NotAllowed => After::Park,
        _ if failing < OFFLINE_AFTER => After::Retry(backoff),
        Fail::Unseen => After::Park,
        Fail::Other => After::Retry(OFFLINE_RETRY),
    }
}

/// A parked link's reason: why its last try failed, and what wakes it. A refusal keeps "this Mac
/// drive it", which the app reads as that Mac's refusal.
fn paused(kind: Fail, why: &str, name: &str) -> String {
    let until = match kind {
        Fail::NotAllowed => format!("{name} lets this Mac drive it"),
        _ => format!("{name} is seen on the network again or this Mac's network changes"),
    };
    format!("{}; paused until {until}", why.trim_end_matches('.'))
}

/// A parked link waits, dialing nothing, until it's poked ([`seen_again`], [`network_changed`],
/// [`allowed`], `drive.open` again) or closed. Every [`OFFLINE_RETRY`] it looks, without the
/// network, whether the app quit (the caller's loop closes it) or Bonjour sees that Mac now.
fn park(w: &Weak<Daemon>, o: &Out, kind: Fail, peer: &str) {
    while !o.sleep(OFFLINE_RETRY) {
        let Some(d) = w.upgrade() else { return };
        if o.is_closed() || d.shutting_down.load(Ordering::Relaxed) || !o.opener.alive() {
            return;
        }
        if kind == Fail::Unseen && seen(&d, peer) {
            return;
        }
    }
}

/// Whether Bonjour sees `peer` now.
fn seen(d: &Daemon, peer: &str) -> bool {
    d.nearby.inner().peers.contains_key(peer)
}

/// What woke links up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wake {
    /// Bonjour sees that Mac again.
    Seen,
    /// This Mac's network changed.
    Network,
    /// That Mac lets this one drive it now.
    Allowed,
}

/// Whether `wake` tries a link parked for `parked` (None: not parked, waiting out a backoff,
/// which a try now only shortens). Only that Mac's own word, or the person here, tries a refused
/// one again.
fn wakes(parked: Option<Fail>, wake: Wake) -> bool {
    wake == Wake::Allowed || parked != Some(Fail::NotAllowed)
}

/// Poke this Mac's links (to `peer` only, if given) that `wake` tries.
fn wake_links(d: &Daemon, peer: Option<&str>, wake: Wake) {
    for o in outgoing(d).values() {
        let to = peer.is_none_or(|p| lock(&o.status).peer == p);
        if to && wakes(*lock(&o.parked), wake) {
            o.poke();
        }
    }
}

/// Bonjour sees `peers` now (they just appeared): try their links now.
pub fn seen_again(d: &Daemon, peers: &[&str]) {
    for p in peers {
        wake_links(d, Some(p), Wake::Seen);
    }
}

/// This Mac's network changed (`netwatch`): try the links waiting for a Mac to be seen now.
pub fn network_changed(d: &Daemon) {
    wake_links(d, None, Wake::Network);
}

/// `peer` lets this Mac drive it now (its `drive_allowed`): try its links now.
pub fn allowed(d: &Daemon, peer: &str) {
    wake_links(d, Some(peer), Wake::Allowed);
}

/// This Mac now lets `peer` drive it ([`drive::changed`]): tell it, so a link it parked on the
/// refusal comes up now. Sent from the nearby thread ([`tell_owed`]), never waited on here.
pub fn tell_allowed(d: &Daemon, peer: &str) {
    let mut owed = lock(&d.nearby.relay.owed);
    if !owed.iter().any(|p| p == peer) {
        owed.push(peer.to_string());
    }
}

/// Send the `drive_allowed` notices owed, each on a thread of its own: best effort (a Mac that
/// can't be reached now, or an older build that doesn't know it, gets nothing; its person can
/// try the link again).
pub fn tell_owed(d: &Arc<Daemon>) {
    let owed = std::mem::take(&mut *lock(&d.nearby.relay.owed));
    for peer in owed {
        let d = d.clone();
        let _ = std::thread::Builder::new().name("drive-allowed".into()).spawn(move || {
            let Ok(mut ch) = link::connect_peer(&d, &peer) else { return };
            drop(d);
            if ch.send(&Wire::DriveAllowed).is_ok() {
                let _ = ch.expect(transport::HANDSHAKE_TIMEOUT);
            }
            ch.finish();
        });
    }
}

/// Connect to `peer` and ask to drive it, reporting each stage; the channel, and whether that Mac
/// agreed to [`LZ4`] (an older one doesn't answer it: plain DATA).
fn connect(d: &Daemon, peer: &str, stage: &dyn Fn(DriveStage)) -> Result<(Channel, bool), Failed> {
    let reach = Reach { finding: &|| stage(DriveStage::Finding), proving: &|| stage(DriveStage::Proving), patient: false };
    let mut ch = link::reach_staged(d, peer, &reach).map_err(|e| Failed { kind: if seen(d, peer) { Fail::Other } else { Fail::Unseen }, why: e.message })?;
    ch.send(&Wire::DriveOpen { compress: vec![LZ4.into()] })?;
    match ch.expect(transport::HANDSHAKE_TIMEOUT)? {
        Wire::DriveOk { compress } => Ok((ch, compress.as_deref() == Some(LZ4))),
        Wire::Refused { reason } if reason == NOT_ALLOWED => {
            let name = lock_name(d, peer);
            Err(Failed { kind: Fail::NotAllowed, why: format!("{name} doesn't let this Mac drive it: turn it on at {name} in Settings › Nearby") })
        }
        Wire::Refused { reason } if reason == transport::NOT_PAIRED => Err(format!("{} doesn't know this Mac any more (pair again)", lock_name(d, peer)).into()),
        Wire::Refused { reason } => Err(format!("{} refused: {reason}", lock_name(d, peer)).into()),
        Wire::Unknown => Err(format!("{}'s Midna is too old to be driven", lock_name(d, peer)).into()),
        _ => Err(String::from("an unexpected answer").into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use super::transport::tests::{ident, pair_up};
    use midna_proto::frame::{Cell, Frame, RowData};
    use serde::Deserialize;

    /// A full-screen frame of this file's text at `top`, coloured like code, as midnad encodes it.
    fn screen(top: usize, cols: usize, rows: usize) -> Vec<u8> {
        let lines: Vec<&str> = include_str!("relay.rs").lines().collect();
        let palette = [[0xd8, 0xde, 0xe9], [0x81, 0xa1, 0xc1], [0xa3, 0xbe, 0x8c], [0xeb, 0xcb, 0x8b], [0xbf, 0x61, 0x6a]];
        let changed = (0..rows)
            .map(|y| {
                let chars: Vec<char> = lines.get(top + y).copied().unwrap_or("").chars().collect();
                let mut fg = palette[0];
                let cells = (0..cols)
                    .map(|x| {
                        let ch = chars.get(x).copied().unwrap_or(' ');
                        if !ch.is_alphanumeric() && ch != '_' {
                            fg = palette[ch as usize % palette.len()];
                        }
                        Cell { ch, fg, bg: [0x1e, 0x1e, 0x2e], flags: 0 }
                    })
                    .collect();
                (y as u16, RowData { cells, ..Default::default() })
            })
            .collect();
        let f = Frame { cols: cols as u16, rows: rows as u16, full: true, changed, cursor: Some((3, 7)), built_ns: 123_456_789, ..Default::default() };
        let mut out = Vec::new();
        f.encode(&mut out);
        out
    }

    /// Two ends of a split Noise connection.
    fn link(tag: &str) -> ((RawReader, RawWriter), (RawReader, RawWriter)) {
        let (a, b) = (ident(&format!("{tag}-a")), ident(&format!("{tag}-b")));
        let (ca, cb) = pair_up(&a, &b);
        (ca.unwrap().split(Duration::from_secs(5)).unwrap(), cb.unwrap().split(Duration::from_secs(5)).unwrap())
    }

    /// Carry `sends` from a local stream over a link (`lz4` or not) and read the raw messages
    /// that arrive: (tag, bytes on the wire, the stream bytes they carry).
    fn carry(tag: &str, lz4: bool, sends: &[&[u8]]) -> Vec<(u8, usize, Vec<u8>)> {
        let ((_ra, wa), (mut rb, _wb)) = link(tag);
        let mux = Mux::new(wa, lz4).unwrap();
        let (ours, mut app) = UnixStream::pair().unwrap();
        widen(&ours);
        widen(&app);
        assert!(mux.attach(9, ours, false));
        let want: usize = sends.iter().map(|s| s.len()).sum();
        let mut got = Vec::new();
        let mut carried = 0;
        for s in sends {
            app.write_all(s).unwrap();
            let target = carried + s.len();
            while carried < target {
                let m = rb.recv(Duration::from_secs(5)).unwrap().expect("a message").to_vec();
                assert_eq!(&m[1..5], &9u32.to_be_bytes());
                let bytes = match m[0] {
                    DATA => m[5..].to_vec(),
                    ZDATA => unpack(&m[5..]).expect("unpacks"),
                    t => panic!("tag {t}"),
                };
                carried += bytes.len();
                got.push((m[0], m.len(), bytes));
            }
        }
        assert_eq!(carried, want);
        mux.close("done", false);
        got
    }

    #[test]
    fn a_packing_link_squeezes_frames_and_leaves_keystrokes_plain() {
        let frame = screen(40, 180, 50);
        let keys = b"\x03\x00\x00\x00\x01a";
        let got = carry("pk", true, &[&frame, keys]);
        let (frames, key) = got.split_at(got.len() - 1);
        let wire: usize = frames.iter().map(|(_, n, _)| n).sum();
        let back: Vec<u8> = frames.iter().flat_map(|(_, _, b)| b.clone()).collect();
        assert_eq!(back, frame, "the frame arrives whole");
        assert!(frames.iter().all(|(t, _, _)| *t == ZDATA), "{:?}", frames.iter().map(|f| (f.0, f.1)).collect::<Vec<_>>());
        assert!(wire * 10 < frame.len(), "{} bytes became {wire} on the wire", frame.len());
        assert_eq!((key[0].0, key[0].2.as_slice()), (DATA, &keys[..]), "a keystroke goes plain");
        eprintln!("180x50 frame: {} bytes, {wire} on the wire in {} message(s)", frame.len(), frames.len());
    }

    #[test]
    fn a_plain_link_sends_what_it_read_as_it_did() {
        let frame = screen(0, 180, 50);
        let got = carry("pl", false, &[&frame]);
        assert!(got.iter().all(|(t, n, _)| *t == DATA && *n <= transport::RAW_MAX));
        assert_eq!(got.iter().flat_map(|(_, _, b)| b.clone()).collect::<Vec<_>>(), frame);
        assert_eq!(got.iter().map(|(_, n, _)| n).sum::<usize>(), frame.len() + 5 * got.len(), "no packing");
    }

    #[test]
    fn bytes_that_dont_pack_go_plain() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let noise: Vec<u8> = (0..3 * DATA_MAX)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 32) as u8
            })
            .collect();
        let mut out = Vec::new();
        assert!(!pack(1, &noise[..DATA_MAX], &mut out), "noise doesn't shrink");
        let got = carry("nz", true, &[&noise]);
        assert!(got.iter().all(|(t, n, _)| *t == DATA && *n <= transport::RAW_MAX));
        assert_eq!(got.iter().flat_map(|(_, _, b)| b.clone()).collect::<Vec<_>>(), noise);
    }

    #[test]
    fn unpacking_is_bounded() {
        let mut out = Vec::new();
        let frame = screen(0, 120, 40);
        assert!(pack(1, &frame, &mut out));
        assert_eq!(unpack(&out[5..]).unwrap(), frame);
        // Claims more than ZDATA_MAX: refused before anything is allocated for it.
        let mut big = out[5..].to_vec();
        big[..4].copy_from_slice(&(ZDATA_MAX as u32 + 1).to_be_bytes());
        assert_eq!(unpack(&big), None);
        // A block that unpacks to more than it claims (a bomb), less, or is cut short.
        let zeros = lz4_flex::block::compress(&vec![0u8; 10 << 20]);
        let mut bomb = 1024u32.to_be_bytes().to_vec();
        bomb.extend_from_slice(&zeros);
        assert_eq!(unpack(&bomb), None);
        let mut short = out[5..].to_vec();
        short[..4].copy_from_slice(&(frame.len() as u32 + 1).to_be_bytes());
        assert_eq!(unpack(&short), None);
        assert_eq!(unpack(&out[5..out.len() - 3]), None);
        assert_eq!(unpack(&[0, 0]), None);
    }

    /// Run a mux (`lz4` or not) reading `rd`, delivering what comes for stream 9 to a local socket;
    /// the socket's other end and the thread that returns why the link ended.
    fn receiver(rd: RawReader, wr: RawWriter, lz4: bool) -> (UnixStream, std::thread::JoinHandle<String>) {
        let mux = Mux::new(wr, lz4).unwrap();
        let (ours, app) = UnixStream::pair().unwrap();
        assert!(mux.attach(9, ours, false));
        let t = std::thread::spawn(move || {
            mux.run(rd, || Ok(()), |mux, tag, sid, data| match tag {
                DATA => mux.deliver(sid, data),
                _ => mux.ended(sid),
            })
        });
        (app, t)
    }

    #[test]
    fn a_packing_link_delivers_zdata_and_drops_a_peer_that_sends_garbage() {
        let ((ra, wa), (rb, mut wb)) = link("rx");
        let (mut app, t) = receiver(ra, wa, true);
        let frame = screen(10, 180, 50);
        let mut m = Vec::new();
        assert!(pack(9, &frame[..DATA_MAX], &mut m));
        wb.send(&m).unwrap();
        let mut back = vec![0u8; DATA_MAX];
        app.read_exact(&mut back).unwrap();
        assert_eq!(back, frame[..DATA_MAX]);
        let mut bomb = vec![ZDATA, 0, 0, 0, 9];
        bomb.extend_from_slice(&u32::MAX.to_be_bytes());
        bomb.extend_from_slice(&[0; 32]);
        wb.send(&bomb).unwrap();
        assert_eq!(t.join().unwrap(), "that Mac sent data that doesn't unpack");
        drop(rb);
    }

    #[test]
    fn a_plain_link_refuses_zdata() {
        let ((ra, wa), (_rb, mut wb)) = link("pz");
        let (_app, t) = receiver(ra, wa, false);
        let mut m = Vec::new();
        assert!(pack(9, &screen(0, 80, 24), &mut m));
        wb.send(&m).unwrap();
        assert_eq!(t.join().unwrap(), "that Mac sent data that doesn't unpack");
    }

    /// `drive_open` / `drive_ok` as builds before compression knew them.
    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    enum Before {
        DriveOpen,
        DriveOk,
        #[serde(other)]
        Unknown,
    }

    #[test]
    fn older_builds_and_this_one_still_open_links() {
        let json = |w: &Wire| serde_json::to_string(w).unwrap();
        // This build to an older one: the offer and a plain answer read as before.
        let offer = Wire::DriveOpen { compress: vec![LZ4.into()] };
        assert_eq!(serde_json::from_str::<Before>(&json(&offer)).unwrap(), Before::DriveOpen);
        assert_eq!(json(&Wire::DriveOk { compress: None }), r#"{"t":"drive_ok"}"#);
        assert_eq!(serde_json::from_str::<Before>(&json(&Wire::DriveOk { compress: Some(LZ4.into()) })).unwrap(), Before::DriveOk);
        // An older build to this one: no offer, no compression.
        assert_eq!(serde_json::from_str::<Wire>(r#"{"t":"drive_open"}"#).unwrap(), Wire::DriveOpen { compress: vec![] });
        assert_eq!(serde_json::from_str::<Wire>(r#"{"t":"drive_ok"}"#).unwrap(), Wire::DriveOk { compress: None });
        assert_eq!(json(&Wire::DriveOpen { compress: vec![] }), r#"{"t":"drive_open"}"#);
        // The notice that a Mac lets this one drive it: an older build reads it as one it
        // doesn't know (and refuses it), not as a broken message.
        assert_eq!(json(&Wire::DriveAllowed), r#"{"t":"drive_allowed"}"#);
        assert_eq!(serde_json::from_str::<Before>(&json(&Wire::DriveAllowed)).unwrap(), Before::Unknown);
    }

    #[test]
    fn a_link_is_parked_once_its_mac_cant_be_seen_and_at_once_when_refused() {
        let (secs, b) = (Duration::from_secs, Duration::from_secs(4));
        // Failing for less than a minute: tried again soon, seen or not (a blip, or Bonjour
        // still looking right after the link opened).
        for kind in [Fail::Unseen, Fail::Other] {
            assert_eq!(after(kind, secs(0), b), After::Retry(b), "{kind:?}");
            assert_eq!(after(kind, secs(59), b), After::Retry(b), "{kind:?}");
        }
        // Offline: a Mac Bonjour can't see is parked, dialing nothing; one it can see but that
        // doesn't answer is still tried every 30 s.
        assert_eq!(after(Fail::Unseen, OFFLINE_AFTER, b), After::Park);
        assert_eq!(after(Fail::Unseen, secs(3600), b), After::Park);
        assert_eq!(after(Fail::Other, OFFLINE_AFTER, b), After::Retry(OFFLINE_RETRY));
        // Refused: never tried again by itself, from the first refusal on.
        assert_eq!(after(Fail::NotAllowed, secs(0), b), After::Park);
        assert_eq!(after(Fail::NotAllowed, secs(3600), b), After::Park);
    }

    #[test]
    fn a_parked_links_reason_says_it_is_paused_and_until_what() {
        let unseen = paused(Fail::Unseen, "that device can't be seen on the network now", "Riley's MacBook Air");
        assert_eq!(unseen, "that device can't be seen on the network now; paused until Riley's MacBook Air is seen on the network again or this Mac's network changes");
        let off = paused(Fail::Unseen, "Midna can't see other Macs: Local Network access is off. Turn it on, then try again.", "Bravo");
        assert!(off.starts_with("Midna can't see other Macs") && off.contains("try again; paused until Bravo"), "{off}");
        let refused = paused(Fail::NotAllowed, "Bravo doesn't let this Mac drive it: turn it on at Bravo in Settings › Nearby", "Bravo");
        assert_eq!(refused, "Bravo doesn't let this Mac drive it: turn it on at Bravo in Settings › Nearby; paused until Bravo lets this Mac drive it");
        assert!(refused.contains("this Mac drive it"), "the app still reads it as that Mac's refusal");
    }

    /// A link of this test process's own, never opened.
    fn out() -> Arc<Out> {
        let status = DriveLink {
            link: "dl_test".into(),
            peer: "fp".into(),
            name: "Bravo".into(),
            socket: String::new(),
            state: DriveState::Offline,
            stage: None,
            reason: None,
            last_seen: None,
            drops: 0,
        };
        Arc::new(Out {
            status: Mutex::new(status),
            opener: Opener { pid: std::process::id() as i32, exe: String::new() },
            socket: PathBuf::new(),
            mux: Mutex::new(None),
            closed: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            next_sid: AtomicU32::new(1),
            failing_since: Mutex::new(None),
            parked: Mutex::new(Some(Fail::Unseen)),
        })
    }

    #[test]
    fn a_parked_link_waits_until_its_poked() {
        let o = out();
        assert!(!o.sleep(Duration::from_millis(20)), "nothing poked it: it waited it out");
        o.poke();
        assert!(o.sleep(Duration::from_secs(30)), "a poke that came before it slept still counts");
        assert!(!o.sleep(Duration::from_millis(20)), "once");
        // Parked: back as soon as it's poked, long before its next look.
        let (poker, started) = (o.clone(), Instant::now());
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            poker.poke();
        });
        park(&Weak::new(), &o, Fail::Unseen, "fp");
        t.join().unwrap();
        assert!(started.elapsed() < OFFLINE_RETRY / 2, "{:?}", started.elapsed());
    }

    #[test]
    fn only_that_macs_word_or_the_person_here_wakes_a_refused_link() {
        // A link waiting for its Mac to be seen: woken by Bonjour seeing it, a network change,
        // or that Mac allowing it. So is one between tries (it's just tried sooner).
        for parked in [Some(Fail::Unseen), None] {
            for wake in [Wake::Seen, Wake::Network, Wake::Allowed] {
                assert!(wakes(parked, wake), "{parked:?} {wake:?}");
            }
        }
        // A refused one: only that Mac's "drive_allowed" (and `drive.open`, which pokes it
        // directly). Seeing it again or moving networks doesn't change its answer.
        assert!(wakes(Some(Fail::NotAllowed), Wake::Allowed));
        assert!(!wakes(Some(Fail::NotAllowed), Wake::Seen));
        assert!(!wakes(Some(Fail::NotAllowed), Wake::Network));
    }
}
