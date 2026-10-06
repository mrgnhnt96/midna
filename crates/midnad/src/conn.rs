//! One client connection: a reader thread (this one) parses JSON-RPC lines and dispatches;
//! a writer thread serializes responses and notifications so they never interleave.
//!
//! Robustness rules (a misbehaving client must never hurt midnad or other clients):
//! - Lines are capped at [`MAX_LINE`]; a longer line closes the connection.
//! - Invalid UTF-8 / invalid JSON gets a JSON-RPC parse error, and the connection stays usable.
//! - The outbound queue is capped at [`MAX_PENDING`] bytes. A client that stops reading (its
//!   socket buffer full, its queue over the cap) is disconnected instead of growing the
//!   daemon's memory; it can reconnect and replay with `events.subscribe {since_seq}`.
//! - Writes time out after [`WRITE_TIMEOUT`], so a stuck peer can't pin a writer thread.
//! - A panic in a handler is caught and answered as an internal error.
use crate::daemon::Daemon;
use crate::peer;
use crate::rpc::{self, Ctx, Role};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// Longest accepted request line.
pub const MAX_LINE: usize = 32 << 20;
/// Bytes queued for one connection before it is cut off as too slow.
pub const MAX_PENDING: usize = 64 << 20;
/// A single blocked write longer than this ends the connection.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

pub enum Out {
    Line(String),
    /// Flush and stop the writer (the connection is switching to binary stream mode or closing).
    Stop,
}

/// The connection is gone, or was cut off for falling too far behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// Sender side of a connection's outbound queue (responses and notifications).
#[derive(Clone)]
pub struct OutTx {
    tx: Sender<Out>,
    pending: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    sock: Option<Arc<UnixStream>>,
}

impl OutTx {
    /// A queue with a receiver (tests, and `serve`).
    pub fn pair() -> (OutTx, Receiver<Out>) {
        let (tx, rx) = channel();
        (OutTx { tx, pending: Arc::new(AtomicUsize::new(0)), closed: Arc::new(AtomicBool::new(false)), sock: None }, rx)
    }

    /// A queue nobody reads (internal contexts): every send fails.
    pub fn detached() -> OutTx {
        let (tx, _rx) = Self::pair();
        tx
    }

    /// Queue a message. Fails if the connection is gone or has fallen too far behind (in
    /// which case the connection is shut down).
    pub fn send(&self, m: Out) -> Result<(), Closed> {
        if matches!(m, Out::Stop) {
            // Always delivered, so the writer ends even after a cut-off.
            return self.tx.send(m).map_err(|_| Closed);
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(Closed);
        }
        let n = match &m {
            Out::Line(l) => l.len() + 1,
            Out::Stop => 0,
        };
        if self.pending.fetch_add(n, Ordering::AcqRel) + n > MAX_PENDING {
            self.pending.fetch_sub(n, Ordering::AcqRel);
            self.cut_off();
            return Err(Closed);
        }
        self.tx.send(m).map_err(|_| {
            self.pending.fetch_sub(n, Ordering::AcqRel);
            Closed
        })
    }

    fn cut_off(&self) {
        if !self.closed.swap(true, Ordering::AcqRel)
            && let Some(s) = &self.sock {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
    }

    /// The client hung up (or was cut off). Peeks at the socket without consuming anything, so
    /// a handler blocked on the human can notice its caller is gone. A connection with no
    /// socket (internal contexts, tests) never is.
    pub fn peer_gone(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return true;
        }
        let Some(s) = &self.sock else { return false };
        let mut b = [0u8; 1];
        // SAFETY: a one-byte peek into a local buffer on a socket we own.
        let n = unsafe { libc::recv(s.as_raw_fd(), b.as_mut_ptr().cast(), 1, libc::MSG_PEEK | libc::MSG_DONTWAIT) };
        match n {
            0 => true,
            n if n > 0 => false,
            _ => matches!(std::io::Error::last_os_error().raw_os_error(), Some(libc::ECONNRESET | libc::ENOTCONN | libc::EPIPE | libc::EBADF)),
        }
    }

    /// Bytes queued and not yet written (tests).
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
}

fn writer_thread(mut s: UnixStream, rx: Receiver<Out>, pending: Arc<AtomicUsize>, closed: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
    let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
    std::thread::Builder::new()
        .name("conn-writer".into())
        .spawn(move || {
            for m in rx {
                match m {
                    Out::Line(mut l) => {
                        let n = l.len() + 1;
                        l.push('\n');
                        let ok = s.write_all(l.as_bytes()).is_ok();
                        pending.fetch_sub(n, Ordering::AcqRel);
                        if !ok {
                            closed.store(true, Ordering::Release);
                            let _ = s.shutdown(std::net::Shutdown::Both);
                            break;
                        }
                    }
                    Out::Stop => break,
                }
            }
        })
        .expect("spawn writer")
}

pub fn response(id: &Value, r: Result<Value, midna_proto::RpcError>) -> String {
    match r {
        Ok(v) => json!({ "jsonrpc": "2.0", "id": id, "result": v }).to_string(),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }).to_string(),
    }
}

/// Read one `\n`-terminated line of at most `max` bytes (without the newline).
/// Ok(None) = EOF; Err = I/O error or an over-long line.
pub fn read_line_bounded(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Option<()>> {
    buf.clear();
    loop {
        let avail = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if avail.is_empty() {
            return Ok(if buf.is_empty() { None } else { Some(()) });
        }
        let (chunk, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (&avail[..i], Some(i + 1)),
            None => (avail, None),
        };
        if buf.len() + chunk.len() > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "request line too long"));
        }
        buf.extend_from_slice(chunk);
        let used = done.unwrap_or(chunk.len());
        r.consume(used);
        if done.is_some() {
            return Ok(Some(()));
        }
    }
}

/// The connection's role: the GUI is human, unless it runs inside one of this daemon's
/// terminals (then an agent launched it, e.g. `Midna.app/Contents/MacOS/midna-app` typed into
/// a midna shell). Everything else is an agent.
fn base_role(d: &Daemon, fd: i32, pid: Option<i32>) -> Role {
    let (Some(pid), Some(exe)) = (pid, pid.and_then(peer::pid_path)) else { return Role::Agent };
    let gui = peer::GuiIdentity { app_path: d.cfg.app_path.clone(), requirement: d.cfg.gui_requirement.clone() };
    if !gui.matches(&exe, pid, peer::peer_token(fd).as_ref()) {
        return Role::Agent;
    }
    if peer::terminal_of(pid, &d.terminal_pids()).is_some() {
        return Role::Agent;
    }
    Role::Human
}

pub fn serve(d: Arc<Daemon>, s: UnixStream, conn_id: u64) {
    let pid = peer::peer_pid(s.as_raw_fd());
    let role = base_role(&d, s.as_raw_fd(), pid);
    let (Ok(ws), Ok(ctl)) = (s.try_clone(), s.try_clone()) else { return };
    let (tx, rx) = OutTx::pair();
    let tx = OutTx { sock: Some(Arc::new(ctl)), ..tx };
    let writer = writer_thread(ws, rx, tx.pending.clone(), tx.closed.clone());
    let mut reader = BufReader::with_capacity(64 << 10, s);
    let mut line = Vec::new();
    loop {
        match read_line_bounded(&mut reader, &mut line, MAX_LINE) {
            Ok(Some(())) => {}
            Ok(None) => break,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::InvalidData {
                    let err = midna_proto::RpcError::new(midna_proto::error::PARSE_ERROR, format!("request line longer than {} MB; closing", MAX_LINE >> 20));
                    let _ = tx.send(Out::Line(response(&Value::Null, Err(err))));
                }
                break;
            }
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let req: Value = match serde_json::from_slice(&line) {
            Ok(v @ Value::Object(_)) => v,
            Ok(_) => {
                let err = midna_proto::RpcError::new(midna_proto::error::INVALID_REQUEST, "a request must be a JSON object");
                let _ = tx.send(Out::Line(response(&Value::Null, Err(err))));
                continue;
            }
            Err(e) => {
                let err = midna_proto::RpcError::new(midna_proto::error::PARSE_ERROR, format!("parse error: {e}"));
                let _ = tx.send(Out::Line(response(&Value::Null, Err(err))));
                continue;
            }
        };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("").to_string();
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let mut ctx = Ctx::new(role, &params, conn_id, tx.clone(), pid);
        if method == "stream.attach" {
            // Answer, stop the line writer, then hand the socket to the binary stream.
            match crate::stream::prepare(&d, &params) {
                Ok(handle) => {
                    let _ = tx.send(Out::Line(response(id.as_ref().unwrap_or(&Value::Null), Ok(json!({ "ok": true })))));
                    let _ = tx.send(Out::Stop);
                    drop(tx);
                    let _ = writer.join();
                    crate::stream::run(handle, reader, &params, ctx.is_human());
                    return;
                }
                Err(e) => {
                    let _ = tx.send(Out::Line(response(id.as_ref().unwrap_or(&Value::Null), Err(e))));
                    continue;
                }
            }
        }
        let result = match rpc::bind_caller(&d, &mut ctx) {
            Err(e) => Err(e),
            Ok(()) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rpc::call(&d, &ctx, &method, params))).unwrap_or_else(|p| {
                let what = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
                eprintln!("midnad: handler for {method} panicked: {what}");
                Err(midna_proto::RpcError::internal(format!("internal error in {method}")))
            }),
        };
        // Notifications (no id) get no response.
        if let Some(id) = id {
            let _ = tx.send(Out::Line(response(&id, result)));
        }
    }
    let _ = tx.send(Out::Stop);
    drop(tx);
    let _ = writer.join();
    d.forget_conn(conn_id);
}
