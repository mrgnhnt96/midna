//! Driving this Mac from one of my other Macs: whether a paired device may (`peers.set_drive`,
//! `peers.confirm` `allow_drive`; kept in `nearby/peers.json`), and the live drive links it has.
//!
//! The relay (`relay.rs`) opens a drive link through [`open`] when a paired device asks to drive
//! this Mac, and drops the returned [`Handle`] when the link closes. Whatever takes the permission away cuts that device's links at once,
//! before the call returns: `peers.set_drive` off, keeping it as a contact (`peers.set_tier`),
//! `peers.forget`, and pairing again without it. A cut link runs the relay's closer once
//! (`Link::on_cut`) and reads as cut from then on (`Link::is_cut`), so a link that was being opened
//! while the permission went away is cut as soon as it registers its closer.
use crate::daemon::Daemon;
use midna_proto::kinds;
use midna_proto::nearby::{PairedPeer, PeerDrive};
use midna_proto::types::Actor;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

type Closer = Box<dyn FnOnce(&str) + Send>;

/// One drive link from a paired device.
pub struct Link {
    peer: String,
    id: u64,
    cut: AtomicBool,
    closer: Mutex<Option<Closer>>,
    links: Arc<Mutex<HashMap<String, Vec<(u64, Arc<Link>)>>>>,
}

impl Link {
    pub fn peer(&self) -> &str {
        &self.peer
    }

    /// The permission went away: stop relaying.
    pub fn is_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }

    /// What closes the link (its connection) when it's cut, called once with why. Runs at once
    /// if the link was cut already.
    pub fn on_cut(&self, close: impl FnOnce(&str) + Send + 'static) {
        if self.is_cut() {
            close("this Mac stopped letting that device drive it");
            return;
        }
        *lock(&self.closer) = Some(Box::new(close));
        if self.is_cut()
            && let Some(c) = lock(&self.closer).take()
        {
            c("this Mac stopped letting that device drive it");
        }
    }

    fn cut(&self, why: &str) {
        self.cut.store(true, Ordering::SeqCst);
        if let Some(c) = lock(&self.closer).take() {
            c(why);
        }
    }
}

/// A link closed by the relay leaves the registry.
pub struct Handle(Arc<Link>);

impl std::ops::Deref for Handle {
    type Target = Link;
    fn deref(&self) -> &Link {
        &self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let mut all = lock(&self.0.links);
        if let Some(v) = all.get_mut(&self.0.peer) {
            v.retain(|(id, _)| *id != self.0.id);
            if v.is_empty() {
                all.remove(&self.0.peer);
            }
        }
    }
}

/// The drive links open now, by the driving device's fingerprint.
#[derive(Default)]
pub struct Links {
    next: AtomicU64,
    by_peer: Arc<Mutex<HashMap<String, Vec<(u64, Arc<Link>)>>>>,
}

impl Links {
    /// How many links `peer` has open.
    pub fn count(&self, peer: &str) -> usize {
        lock(&self.by_peer).get(peer).map_or(0, Vec::len)
    }

    fn register(&self, peer: &str) -> Handle {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let link = Arc::new(Link { peer: peer.to_string(), id, cut: AtomicBool::new(false), closer: Mutex::new(None), links: self.by_peer.clone() });
        lock(&self.by_peer).entry(peer.to_string()).or_default().push((id, link.clone()));
        Handle(link)
    }

    /// Cut every link `peer` has; how many there were.
    fn cut(&self, peer: &str, why: &str) -> usize {
        let gone = lock(&self.by_peer).remove(peer).unwrap_or_default();
        for (_, l) in &gone {
            l.cut(why);
        }
        gone.len()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// `peer` may drive this Mac now: it is paired, kept as my_device, and allowed.
pub fn allowed(d: &Daemon, peer: &str) -> bool {
    d.nearby.inner().store(&d.cfg.home).get(peer).is_some_and(|s| s.drives())
}

/// Open a drive link from `peer`, or None if it may not drive this Mac. The relay keeps the
/// handle while the link is open and closes it when it's cut.
pub fn open(d: &Daemon, peer: &str) -> Option<Handle> {
    let mut i = d.nearby.inner();
    let may = i.store(&d.cfg.home).get(peer).is_some_and(|s| s.drives());
    // Registered under the same lock that turning it off takes, so no link slips past a cut.
    may.then(|| d.nearby.drive.register(peer))
}

/// `peer`'s permission went away: cut its links. Call with the store's lock released.
pub fn cut(d: &Daemon, peer: &str, why: &str) -> usize {
    d.nearby.drive.cut(peer, why)
}

/// Whether `before` (None: not paired) could drive this Mac and `after` can't any more, or the
/// other way round: say so (event `peers.drive`) and, when it was turned off, cut its links.
pub fn changed(d: &Daemon, peer: &str, name: &str, before: bool, after: bool, why: &str) {
    if before == after {
        return;
    }
    if !after {
        cut(d, peer, why);
    }
    let ev = PeerDrive { peer: peer.to_string(), name: name.to_string(), on: after };
    d.emit(kinds::PEERS_DRIVE, Actor::system(), None, None, serde_json::to_value(ev).unwrap_or_default());
}

/// [`changed`] for a device whose entry is `after` now.
pub fn changed_to(d: &Daemon, before: bool, after: &PairedPeer, why: &str) {
    changed(d, &after.fingerprint, &after.name, before, after.drive, why);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn cutting_a_peer_closes_each_of_its_links_once_and_only_its_own() {
        let links = Links::default();
        let (tx, rx) = mpsc::channel();
        let a1 = links.register("a");
        let a2 = links.register("a");
        let b = links.register("b");
        for (n, l) in [(1, &a1), (2, &a2), (3, &b)] {
            let tx = tx.clone();
            l.on_cut(move |why| tx.send((n, why.to_string())).unwrap());
        }
        assert_eq!((links.count("a"), links.count("b")), (2, 1));

        assert_eq!(links.cut("a", "off"), 2);
        let mut got: Vec<_> = rx.try_iter().collect();
        got.sort();
        assert_eq!(got, vec![(1, "off".to_string()), (2, "off".to_string())]);
        assert!(a1.is_cut() && a2.is_cut() && !b.is_cut());
        assert_eq!((links.count("a"), links.count("b")), (0, 1));
        assert_eq!(links.cut("a", "again"), 0, "nothing left to cut");
        assert!(rx.try_iter().next().is_none());

        // A closer set after the cut runs at once.
        let (tx2, rx2) = mpsc::channel();
        a1.on_cut(move |_| tx2.send(()).unwrap());
        assert!(rx2.try_recv().is_ok());

        // A link the relay closed itself leaves the registry.
        drop(b);
        assert_eq!(links.count("b"), 0);
        drop((a1, a2));
        assert!(lock(&links.by_peer).is_empty());
    }
}
