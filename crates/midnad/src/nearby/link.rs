//! Nearby connections in and out. The listener takes connections on the advertised port and,
//! after the Noise handshake, gates every one on its device key: a pairing request goes to
//! [`super::pairing`]; anything else from a device that isn't paired gets `refused`
//! ([`transport::NOT_PAIRED`]) and the connection closes. Trust is checked again on every
//! message, so forgetting a device cuts it off mid-connection. Paired devices may ping, offer
//! shares ([`super::share`]) and fetch the files of shares offered to them
//! ([`super::transfer`]). With visibility `off` nothing is served at all.
use super::transport::{self, Channel, Wire};
use super::{local, pairing, share, transfer, visibility};
use crate::daemon::Daemon;
use midna_proto::RpcError;
use midna_proto::nearby::Visibility;
use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

/// Connections served at once; more are closed straight away.
const MAX_CONNS: usize = 32;
/// A paired device's connection closes after this long without a message.
const IDLE: Duration = Duration::from_secs(30);
const ACCEPT_POLL: Duration = Duration::from_millis(300);

pub fn listen(d: &Arc<Daemon>, listener: TcpListener) {
    let _ = listener.set_nonblocking(true);
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("nearby-listen".into()).spawn(move || {
        loop {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            match listener.accept() {
                Ok((stream, _)) => {
                    if d.nearby.conns.fetch_add(1, Ordering::AcqRel) >= MAX_CONNS {
                        d.nearby.conns.fetch_sub(1, Ordering::AcqRel);
                        continue;
                    }
                    let _ = stream.set_nonblocking(false);
                    let w = w.clone();
                    let spawned = std::thread::Builder::new().name("nearby-conn".into()).spawn(move || {
                        serve(&w, stream);
                        if let Some(d) = w.upgrade() {
                            d.nearby.conns.fetch_sub(1, Ordering::AcqRel);
                        }
                    });
                    if spawned.is_err() {
                        d.nearby.conns.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    drop(d);
                    std::thread::sleep(ACCEPT_POLL);
                }
                Err(e) => {
                    eprintln!("midnad: nearby: accepting a connection: {e}");
                    drop(d);
                    std::thread::sleep(ACCEPT_POLL);
                }
            }
        }
    });
}

fn serve(w: &Weak<Daemon>, stream: std::net::TcpStream) {
    let Some(d) = w.upgrade() else { return };
    if visibility(&d) == Visibility::Off {
        return;
    }
    let Ok(mut ch) = transport::accept(stream, local(&d)) else { return };
    let first = match ch.expect(transport::HANDSHAKE_TIMEOUT) {
        Ok(m) => m,
        Err(_) => return,
    };
    match first {
        Wire::PairRequest { name, protocol } => pairing::incoming(&d, ch, name, protocol),
        m => {
            drop(d);
            paired(w, ch, m);
        }
    }
}

/// Is `fp` a device this one paired with (and is Nearby on)?
pub fn trusted(d: &Daemon, fp: &str) -> bool {
    visibility(d) != Visibility::Off && d.nearby.inner().store(&d.cfg.home).get(fp).is_some()
}

/// Serve a connection whose first message wasn't a pairing request: paired devices only.
fn paired(w: &Weak<Daemon>, mut ch: Channel, first: Wire) {
    let mut next = Some(first);
    loop {
        let msg = match next.take() {
            Some(m) => m,
            None => match ch.recv(IDLE) {
                Ok(Some(m)) => m,
                Ok(None) => return ch.finish(),
                Err(_) => return,
            },
        };
        let Some(d) = w.upgrade() else { return };
        if d.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        if !trusted(&d, &ch.fingerprint) {
            let _ = ch.send(&Wire::Refused { reason: transport::NOT_PAIRED.into() });
            return ch.finish();
        }
        let reply = match msg {
            Wire::Ping => Wire::Pong,
            Wire::ShareOffer { id, payload } => share::incoming(&d, &ch.fingerprint, id, payload),
            Wire::ShareResult { id, state } => share::result(&d, &ch.fingerprint, id, &state),
            Wire::FileGet { id, file, offset } => {
                let from = ch.fingerprint.clone();
                transfer::serve(&d, &mut ch, &from, &id, file, offset)
            }
            Wire::FileDone { id, ok, error } => transfer::finished(&d, &ch.fingerprint, &id, ok, error),
            _ => Wire::Refused { reason: "unsupported".into() },
        };
        drop(d);
        if ch.send(&reply).is_err() {
            return;
        }
    }
}

/// An authenticated connection to a paired device that can be seen now (for sending to it).
pub fn connect_peer(d: &Daemon, fp: &str) -> Result<Channel, RpcError> {
    if !trusted(d, fp) {
        return Err(RpcError::refused("that device isn't paired with this Mac"));
    }
    let peer = d.nearby.inner().peers.get(fp).cloned().ok_or_else(|| RpcError::not_found("that device can't be seen on the network now"))?;
    let addrs = pairing::addresses(d, peer.host.as_deref(), peer.port)?;
    let ch = transport::connect(&addrs, local(d)).map_err(|e| RpcError::refused(format!("couldn't reach {}: {e}", peer.name)))?;
    if ch.fingerprint != fp {
        return Err(RpcError::refused(format!("the device that answered at {}'s address isn't it (another key)", peer.name)));
    }
    Ok(ch)
}
