//! Transports: A = in-process (PTY reader thread -> engine thread -> frame mailbox -> UI);
//! B-raw = daemon owns PTY+engine and streams raw bytes, GUI runs its own (mirror) engine;
//! B-snap = daemon owns PTY+engine and streams dirty-row frames on GUI credit.
use crate::engine::*;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex};

pub enum Msg {
    Bytes(Vec<u8>, u64),
    Resize(u16, u16, u32, u32),
    Want,
}

pub struct Mailbox {
    pub frame: Mutex<Option<Frame>>,
    pub wake: async_channel::Sender<()>,
}
impl Mailbox {
    pub fn put(&self, f: Frame) {
        let mut g = self.frame.lock().unwrap();
        match g.as_mut() {
            Some(old) => old.merge(f),
            None => *g = Some(f),
        }
        drop(g);
        let _ = self.wake.try_send(());
    }
}

/// Engine thread: owns the !Send Terminal for its whole life.
fn engine_thread(rx: Receiver<Msg>, mb: Arc<Mailbox>, pty_fd: Option<RawFd>) {
    std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let mut eng = Engine::new(80, 24, pty_fd);
            let mut want = true;
            let handle = |eng: &mut Engine, want: &mut bool, m: Msg| match m {
                Msg::Bytes(b, t) => eng.feed(&b, t),
                Msg::Resize(c, r, cw, ch) => {
                    eng.resize(c, r, cw, ch);
                    if let Some(fd) = pty_fd {
                        pty_resize(fd, c, r);
                    }
                }
                Msg::Want => *want = true,
            };
            while let Ok(m) = rx.recv() {
                handle(&mut eng, &mut want, m);
                // drain whatever is queued (bounded so frames keep flowing)
                let mut n = 0;
                while n < 256 {
                    match rx.try_recv() {
                        Ok(m) => handle(&mut eng, &mut want, m),
                        Err(_) => break,
                    }
                    n += 1;
                }
                if std::env::var("MIDNA_DEBUG").is_ok() { eprintln!("engine: want={want} bytes={}", eng.bytes); }
                if want {
                    if let Some(f) = eng.frame() {
                        want = false;
                        mb.put(f);
                    }
                }
            }
        })
        .unwrap();
}

fn pty_reader(fd: RawFd, tx: SyncSender<Msg>) {
    std::thread::Builder::new()
        .name("pty-reader".into())
        .spawn(move || loop {
            let mut buf = vec![0u8; 65536];
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
            buf.truncate(n as usize);
            if tx.send(Msg::Bytes(buf, now_ns())).is_err() {
                break;
            }
        })
        .unwrap();
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    A,
    BRaw,
    BSnap,
}

pub struct Transport {
    pub kind: Kind,
    pty_fd: RawFd,
    tx: Option<SyncSender<Msg>>,
    sock: Option<Mutex<UnixStream>>,
    pub child_pid: i32,
    pub daemon_pid: i32,
}

fn send_msg(s: &mut UnixStream, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut h = [0u8; 5];
    h[0] = kind;
    h[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    s.write_all(&h)?;
    s.write_all(payload)
}
fn recv_msg(s: &mut impl Read, buf: &mut Vec<u8>) -> std::io::Result<u8> {
    let mut h = [0u8; 5];
    s.read_exact(&mut h)?;
    let len = u32::from_le_bytes(h[1..].try_into().unwrap()) as usize;
    buf.resize(len, 0);
    s.read_exact(buf)?;
    Ok(h[0])
}

impl Transport {
    pub fn start(kind: Kind, cmd: &str, mb: Arc<Mailbox>) -> Transport {
        match kind {
            Kind::A => {
                let (fd, pid) = spawn_pty(cmd, 80, 24);
                let (tx, rx) = sync_channel(64);
                engine_thread(rx, mb, Some(fd));
                pty_reader(fd, tx.clone());
                Transport { kind, pty_fd: fd, tx: Some(tx), sock: None, child_pid: pid, daemon_pid: 0 }
            }
            Kind::BRaw | Kind::BSnap => {
                let sock = format!("/tmp/midna-spike-{}.sock", std::process::id());
                let _ = std::fs::remove_file(&sock);
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["daemon", &sock, if kind == Kind::BSnap { "snap" } else { "raw" }, cmd])
                    .spawn()
                    .expect("spawn daemon");
                let daemon_pid = child.id() as i32;
                let mut s = loop {
                    match UnixStream::connect(&sock) {
                        Ok(s) => break s,
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                };
                let mut buf = vec![];
                assert_eq!(recv_msg(&mut s, &mut buf).unwrap(), b'P');
                let child_pid = i32::from_le_bytes(buf[..4].try_into().unwrap());
                let mut rd = s.try_clone().unwrap();
                let tx = if kind == Kind::BRaw {
                    let (tx, rx) = sync_channel(64);
                    engine_thread(rx, mb.clone(), None);
                    Some(tx)
                } else {
                    None
                };
                let txr = tx.clone();
                std::thread::Builder::new()
                    .name("sock-reader".into())
                    .spawn(move || {
                        let mut buf = Vec::new();
                        while let Ok(k) = recv_msg(&mut rd, &mut buf) {
                            match k {
                                b'O' => {
                                    let t = u64::from_le_bytes(buf[..8].try_into().unwrap());
                                    if let Some(tx) = &txr {
                                        if tx.send(Msg::Bytes(buf[8..].to_vec(), t)).is_err() {
                                            break;
                                        }
                                    }
                                }
                                b'F' => mb.put(Frame::decode(&buf)),
                                _ => {}
                            }
                        }
                    })
                    .unwrap();
                Transport { kind, pty_fd: -1, tx, sock: Some(Mutex::new(s)), child_pid, daemon_pid }
            }
        }
    }

    pub fn input(&self, b: &[u8]) {
        match &self.sock {
            None => write_all(self.pty_fd, b),
            Some(s) => {
                let _ = send_msg(&mut s.lock().unwrap(), b'I', b);
            }
        }
    }
    pub fn resize(&self, c: u16, r: u16, cw: u32, ch: u32) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Msg::Resize(c, r, cw, ch));
        }
        if let Some(s) = &self.sock {
            let mut p = vec![];
            p.extend_from_slice(&c.to_le_bytes());
            p.extend_from_slice(&r.to_le_bytes());
            p.extend_from_slice(&cw.to_le_bytes());
            p.extend_from_slice(&ch.to_le_bytes());
            let _ = send_msg(&mut s.lock().unwrap(), b'R', &p);
        }
    }
    pub fn want(&self) {
        match (self.kind, &self.tx, &self.sock) {
            (Kind::BSnap, _, Some(s)) => {
                let _ = send_msg(&mut s.lock().unwrap(), b'W', &[]);
            }
            (_, Some(tx), _) => {
                let _ = tx.send(Msg::Want);
            }
            _ => {}
        }
    }
    pub fn shutdown(&self) {
        unsafe {
            if self.child_pid > 0 {
                libc::kill(self.child_pid, libc::SIGHUP);
            }
            if self.daemon_pid > 0 {
                libc::kill(self.daemon_pid, libc::SIGTERM);
            }
        }
    }
}

// ------------------------------------------------------------------ daemon process

pub fn daemon_main(sock: &str, mode: &str, cmd: &str) {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    let snap = mode == "snap";
    let l = UnixListener::bind(sock).unwrap();
    let (mut s, _) = l.accept().unwrap();
    let _ = std::fs::remove_file(sock);
    let (fd, pid) = spawn_pty(cmd, 80, 24);
    send_msg(&mut s, b'P', &pid.to_le_bytes()).unwrap();
    unsafe {
        let f = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, f | libc::O_NONBLOCK);
    }
    let mut eng = Engine::new(80, 24, Some(fd));
    let mut credit = true;
    let mut inbuf: Vec<u8> = vec![];
    let mut out = Vec::with_capacity(1 << 20);
    let mut rbuf = vec![0u8; 65536];
    let sfd = s.as_raw_fd();
    loop {
        let mut pf = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: sfd, events: libc::POLLIN, revents: 0 },
        ];
        let r = unsafe { libc::poll(pf.as_mut_ptr(), 2, 1000) };
        if r < 0 {
            continue;
        }
        if pf[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            // read a bounded batch
            let mut got = 0usize;
            loop {
                let n = unsafe { libc::read(fd, rbuf.as_mut_ptr() as *mut _, rbuf.len()) };
                if n <= 0 {
                    if n == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                        if got == 0 && pf[0].revents & libc::POLLHUP != 0 {
                            return; // child gone
                        }
                    }
                    break;
                }
                let t = now_ns();
                let d = &rbuf[..n as usize];
                eng.feed(d, t);
                if !snap {
                    out.clear();
                    out.extend_from_slice(&t.to_le_bytes());
                    out.extend_from_slice(d);
                    if send_msg(&mut s, b'O', &out).is_err() {
                        return;
                    }
                }
                got += n as usize;
                if got > (1 << 20) {
                    break;
                }
            }
        }
        if pf[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let mut b = [0u8; 65536];
            match unsafe { libc::read(sfd, b.as_mut_ptr() as *mut _, b.len()) } {
                n if n <= 0 => {
                    unsafe { libc::kill(pid, libc::SIGHUP) };
                    return;
                }
                n => inbuf.extend_from_slice(&b[..n as usize]),
            }
            while inbuf.len() >= 5 {
                let len = u32::from_le_bytes(inbuf[1..5].try_into().unwrap()) as usize;
                if inbuf.len() < 5 + len {
                    break;
                }
                let k = inbuf[0];
                let p: Vec<u8> = inbuf[5..5 + len].to_vec();
                inbuf.drain(..5 + len);
                match k {
                    b'I' => write_all(fd, &p),
                    b'R' => {
                        let c = u16::from_le_bytes([p[0], p[1]]);
                        let r = u16::from_le_bytes([p[2], p[3]]);
                        let cw = u32::from_le_bytes(p[4..8].try_into().unwrap());
                        let ch = u32::from_le_bytes(p[8..12].try_into().unwrap());
                        eng.resize(c, r, cw, ch);
                        pty_resize(fd, c, r);
                    }
                    b'W' => credit = true,
                    _ => {}
                }
            }
        }
        if snap && credit {
            if let Some(f) = eng.frame() {
                out.clear();
                f.encode(&mut out);
                if send_msg(&mut s, b'F', &out).is_err() {
                    return;
                }
                credit = false;
            }
        }
    }
}
