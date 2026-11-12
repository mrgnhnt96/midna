//! Local Network access, as far as midnad can tell. macOS (15 and later) asks once whether Midna
//! may use the local network; until the person answers, and for good if they say no, Bonjour
//! fails with `kDNSServiceErr_PolicyDenied` and connections to other Macs fail with
//! `EHOSTUNREACH` or `EPERM`. There's no API that says which: so for [`WAIT`] after something
//! was first blocked it's `waiting` (the prompt is likely up), and `denied` after that. Anything
//! that gets through (Bonjour running cleanly, a connection to another Mac) makes it `ok` again.
//!
//! Pairing and sending ([`dial`]) keep trying a blocked connection while it's `waiting`, so
//! answering the prompt lets them go through instead of failing.
use super::transport::{self, Channel};
use crate::daemon::Daemon;
use midna_proto::nearby::LocalNetwork;
use midna_proto::{Actor, RpcError, kinds};
use serde_json::json;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// How long after the first block it counts as `waiting`.
pub const WAIT: Duration = Duration::from_secs(60);
/// While waiting, how often a blocked connection is tried again.
const REDIAL: Duration = Duration::from_secs(2);

/// What's blocked now, and since when the first block of this daemon's life.
#[derive(Debug, Default)]
pub struct Access {
    /// Bonjour said the policy denies it.
    bonjour: bool,
    /// The last connection to another Mac was blocked.
    connect: bool,
    first: Option<Instant>,
    /// The state last announced (`peers.local_network`).
    told: Option<LocalNetwork>,
}

impl Access {
    pub fn bonjour(&mut self, blocked: bool, now: Instant) {
        self.bonjour = blocked;
        self.note(now);
    }

    pub fn connect(&mut self, blocked: bool, now: Instant) {
        self.connect = blocked;
        self.note(now);
    }

    fn note(&mut self, now: Instant) {
        if self.bonjour || self.connect {
            self.first.get_or_insert(now);
        }
    }

    pub fn state(&self, now: Instant) -> LocalNetwork {
        match self.first {
            _ if !self.bonjour && !self.connect => LocalNetwork::Ok,
            Some(t) if now < t + WAIT => LocalNetwork::Waiting,
            _ => LocalNetwork::Denied,
        }
    }

    /// While `waiting`: when it turns into `denied`.
    fn waiting_until(&self, now: Instant) -> Option<Instant> {
        (self.state(now) == LocalNetwork::Waiting).then(|| self.first.map(|t| t + WAIT)).flatten()
    }

    /// The state, if it changed since the last time this said so.
    fn changed(&mut self, now: Instant) -> Option<LocalNetwork> {
        let s = self.state(now);
        let before = self.told.replace(s).unwrap_or(LocalNetwork::Ok);
        (before != s).then_some(s)
    }
}

/// This daemon's Local Network access now; None when Nearby is off for it.
pub fn state(d: &Daemon) -> Option<LocalNetwork> {
    d.nearby.inner().service.as_ref()?;
    Some(lock(d).state(Instant::now()))
}

fn lock(d: &Daemon) -> std::sync::MutexGuard<'_, Access> {
    d.nearby.access.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record what Bonjour says and announce a change.
pub fn bonjour(d: &Daemon, blocked: bool) {
    lock(d).bonjour(blocked, Instant::now());
    announce(d);
}

fn connected(d: &Daemon, blocked: bool) {
    lock(d).connect(blocked, Instant::now());
    announce(d);
}

/// Emit `peers.local_network` if the state moved (also from `waiting` to `denied` with time;
/// the nearby thread calls this every pass).
pub fn announce(d: &Daemon) {
    let changed = lock(d).changed(Instant::now());
    if let Some(s) = changed {
        d.emit(kinds::PEERS_LOCAL_NETWORK, Actor::system(), None, None, json!({ "local_network": s.as_str() }));
    }
}

/// Connect to `name` at `addrs` and run the handshake. A connection macOS blocks counts toward
/// the Local Network state; with `patient`, one blocked while that's `waiting` is tried again
/// until it goes through or the wait is over (pairing and sending, which a person is watching).
pub fn dial(d: &Daemon, addrs: &[SocketAddr], name: &str, patient: bool) -> Result<Channel, RpcError> {
    let loopback = addrs.iter().all(|a| a.ip().is_loopback());
    let mut gave_up_waiting = false;
    loop {
        let e = match transport::connect(addrs, super::local(d)) {
            Ok(ch) => {
                if !loopback {
                    connected(d, false);
                }
                return Ok(ch);
            }
            Err(e) => e,
        };
        if !e.blocked || loopback {
            return Err(RpcError::refused(format!("couldn't reach {name}: {}", e.message)));
        }
        connected(d, true);
        let now = Instant::now();
        let until = lock(d).waiting_until(now);
        match until {
            Some(t) if patient && !gave_up_waiting => {
                std::thread::sleep(REDIAL.min(t.saturating_duration_since(now)).max(Duration::from_millis(100)));
                gave_up_waiting = Instant::now() >= t;
            }
            Some(_) => return Err(RpcError::refused(waiting_words(name))),
            None => return Err(RpcError::refused(blocked_words(name))),
        }
    }
}

fn waiting_words(name: &str) -> String {
    format!("Allow Local Network access in the macOS prompt to connect to {name}, then try again.")
}

/// A blocked connection once the wait is over.
fn blocked_words(name: &str) -> String {
    format!(
        "Midna can't reach {name}: macOS is blocking it. Turn on Local Network access for Midna in System Settings › \
         Privacy & Security › Local Network, then try again (and check both Macs are on the same network)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_then_denied_then_ok() {
        let t0 = Instant::now();
        let mut a = Access::default();
        assert_eq!(a.state(t0), LocalNetwork::Ok);
        a.bonjour(true, t0);
        assert_eq!(a.state(t0 + Duration::from_secs(5)), LocalNetwork::Waiting);
        assert_eq!(a.state(t0 + WAIT), LocalNetwork::Denied);
        a.bonjour(false, t0 + WAIT);
        assert_eq!(a.state(t0 + WAIT), LocalNetwork::Ok);
        // The wait is once per daemon: blocked again later is denied straight away.
        a.connect(true, t0 + WAIT * 2);
        assert_eq!(a.state(t0 + WAIT * 2), LocalNetwork::Denied);
    }

    #[test]
    fn either_block_counts_until_both_clear() {
        let t0 = Instant::now();
        let mut a = Access::default();
        a.connect(true, t0);
        a.bonjour(false, t0);
        assert_eq!(a.state(t0), LocalNetwork::Waiting, "Bonjour working doesn't clear a blocked connection");
        a.connect(false, t0);
        assert_eq!(a.state(t0), LocalNetwork::Ok);
    }

    #[test]
    fn changes_are_told_once() {
        let t0 = Instant::now();
        let mut a = Access::default();
        assert_eq!(a.changed(t0), None, "the first look isn't a change");
        a.bonjour(true, t0);
        assert_eq!(a.changed(t0), Some(LocalNetwork::Waiting));
        assert_eq!(a.changed(t0), None);
        assert_eq!(a.changed(t0 + WAIT), Some(LocalNetwork::Denied));
        assert_eq!(a.waiting_until(t0 + WAIT), None);
    }
}
