//! Nearby: Midnas on the same network find each other over Bonjour (`peers.list`, events
//! `peers.appeared` / `peers.left`, settings `nearby.*`).
//!
//! Each home has a persistent Ed25519 device key (`identity.rs`); its fingerprint is the device's
//! id. A `nearby` thread owns every DNS-SD ref (`dnssd.rs`) and, about three times a second,
//! makes them match `nearby.visibility`: `off` neither browses nor advertises, `paired` browses
//! and, once a device is paired, advertises a nameless instance only paired devices recognise
//! (a random salt and one tag per paired device, HMAC(the pair's presence key, salt ‖ this
//! device's fingerprint)), `everyone` browses and advertises the display name, fingerprint and
//! protocol version in the TXT record. `everyone` goes back to
//! `paired` on its own after `EVERYONE_SECS`; the deadline is in state.json so a restart doesn't
//! extend it. Bonjour errors are kept for `peers.list` and retried every few seconds.
//!
//! The advertised port takes Noise connections bound to the device keys (`transport.rs`,
//! `link.rs`): devices that aren't paired may only ask to pair (`pairing.rs`), and paired ones
//! are kept in `nearby/peers.json` (`store.rs`). An advertisement is only a hint: whatever it
//! claims, a connection proves the key, so a device that copies a paired one's name or tags
//! still can't pass for it.
//!
//! The service type keeps installs apart: only the installed Midna on its own home uses
//! `_midna._tcp`; Midna Dev and every other home use `_midna-dev._tcp`, `MIDNA_NEARBY_SERVICE`
//! picks another (or `off`), and in-process daemons (tests) have Nearby off unless the test turns
//! it on with a service of its own.
pub mod dnssd;
pub mod guard;
pub mod identity;
pub mod link;
pub mod objects;
pub mod pairing;
pub mod session;
pub mod share;
pub mod tab;
pub mod store;
pub mod transfer;
pub mod transport;

use crate::daemon::Daemon;
use crate::rpc::{R, ok};
use midna_proto::nearby::{self as nb, NearbyMe, Peer, PeerLeft, PeersListResult, Visibility};
use midna_proto::{Actor, kinds, paths, time};
use serde_json::json;
use std::collections::{BTreeMap, HashMap};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// How long the thread waits on DNS-SD before checking the settings again.
const TICK: Duration = Duration::from_millis(300);
/// After a Bonjour error, how long until it tries again.
const RETRY: Duration = Duration::from_secs(5);
const BROWSE_TAG: usize = 1;
const REGISTER_TAG: usize = 2;

/// Which Bonjour service type a daemon uses (`Config::nearby`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Mode {
    /// Nearby off: no Bonjour at all (in-process daemons, `MIDNA_NEARBY_SERVICE=off`).
    #[default]
    Off,
    /// `_midna._tcp` for the installed Midna on its own home, `_midna-dev._tcp` otherwise.
    Auto,
    Service(String),
}

#[derive(Clone, Debug)]
pub struct Setup {
    pub mode: Mode,
    /// Advertise and browse on this Mac only (tests), not the network.
    pub local_only: bool,
    /// How long `everyone` lasts before going back to `paired` (tests shorten it).
    pub everyone_secs: i64,
    /// A pause after each file chunk this Mac sends (tests slow transfers down to cut them).
    pub serve_delay: Duration,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup { mode: Mode::Off, local_only: false, everyone_secs: nb::EVERYONE_SECS, serve_delay: Duration::ZERO }
    }
}

impl Setup {
    /// A real midnad: `Auto`, unless `MIDNA_NEARBY_SERVICE` names a service type or says `off`.
    pub fn from_env() -> Setup {
        let mode = match std::env::var("MIDNA_NEARBY_SERVICE").unwrap_or_default().trim() {
            "" => Mode::Auto,
            "off" => Mode::Off,
            s if nb::valid_service(s) => Mode::Service(s.to_string()),
            s => {
                eprintln!("midnad: MIDNA_NEARBY_SERVICE={s:?} isn't a service type like _name._tcp; using the default");
                Mode::Auto
            }
        };
        Setup { mode, ..Setup::default() }
    }

    /// The service type for a daemon on `home`; None = Nearby is off.
    pub fn service(&self, home: &Path) -> Option<String> {
        match &self.mode {
            Mode::Off => None,
            Mode::Service(s) => Some(s.clone()),
            Mode::Auto if !paths::is_dev() && home == paths::production_home() => Some(nb::SERVICE.into()),
            Mode::Auto => Some(nb::DEV_SERVICE.into()),
        }
    }
}

#[derive(Default)]
struct Inner {
    service: Option<String>,
    advertising: bool,
    browsing: bool,
    error: Option<String>,
    /// By fingerprint.
    peers: BTreeMap<String, Peer>,
    /// The paired devices (loaded the first time they're needed).
    store: Option<store::Store>,
    /// By id.
    pairings: BTreeMap<String, pairing::Slot>,
    /// The port Nearby connections come in on.
    port: Option<u16>,
    /// Shares to and from paired devices (loaded the first time they're needed).
    inbox: Option<share::Inbox>,
}

impl Inner {
    fn store(&mut self, home: &Path) -> &mut store::Store {
        self.store.get_or_insert_with(|| store::Store::load(home))
    }

    fn inbox(&mut self, home: &Path) -> &mut share::Inbox {
        self.inbox.get_or_insert_with(|| share::Inbox::load(home))
    }
}

#[derive(Default)]
pub struct Runtime {
    identity: OnceLock<identity::Identity>,
    local: OnceLock<transport::Local>,
    computer_name: OnceLock<String>,
    inner: Mutex<Inner>,
    /// Connections being served.
    conns: AtomicUsize,
    /// Set and notified when a share result is owed (`share.rs`'s delivery thread waits on it).
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Set and notified when a file share is accepted (`transfer.rs`'s thread waits on it).
    fetch: Arc<(Mutex<bool>, Condvar)>,
}

impl Runtime {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// This home's device identity (made the first time it's asked for).
pub fn identity(d: &Daemon) -> &identity::Identity {
    d.nearby.identity.get_or_init(|| identity::load_or_create(&d.cfg.home))
}

/// This process's Noise key, signed by the device key.
pub fn local(d: &Daemon) -> &transport::Local {
    d.nearby.local.get_or_init(|| transport::Local::new(identity(d)))
}

pub fn visibility(d: &Daemon) -> Visibility {
    d.setting("nearby.visibility").as_str().and_then(Visibility::parse).unwrap_or_default()
}

/// The name other Midnas see: `nearby.name`, else the Mac's computer name.
pub fn display_name(d: &Daemon) -> String {
    let set = d.setting("nearby.name");
    match set.as_str().map(str::trim) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => d.nearby.computer_name.get_or_init(computer_name).clone(),
    }
}

/// `peers.list`.
pub fn list(d: &Daemon) -> R {
    let fingerprint = identity(d).fingerprint.clone();
    let visibility = visibility(d);
    let everyone_until = match visibility {
        Visibility::Everyone => d.core().state.nearby_everyone_until.map(time::format_unix),
        _ => None,
    };
    let name = display_name(d);
    let mut i = d.nearby.inner();
    pairing::prune(&mut i);
    let store = i.store(&d.cfg.home).clone();
    let mut peers: Vec<Peer> = i.peers.values().cloned().map(|p| annotate(p, &store)).collect();
    peers.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
    let paired = store.peers.values().map(|s| s.paired(i.peers.contains_key(&s.fingerprint))).collect();
    let mut pairings: Vec<_> = i.pairings.values().map(|s| s.info.clone()).collect();
    pairings.sort_by(|a, b| a.started_at.cmp(&b.started_at));
    ok(PeersListResult {
        me: NearbyMe {
            fingerprint,
            name,
            visibility,
            everyone_until,
            service: i.service.clone(),
            advertising: i.advertising,
            browsing: i.browsing,
            port: i.port,
            protocol: nb::PROTOCOL,
            error: i.error.clone(),
        },
        peers,
        paired,
        pairings,
    })
}

/// A seen peer as this device knows it: its tier if paired, and flagged if it isn't but uses a
/// paired device's name.
fn annotate(mut p: Peer, store: &store::Store) -> Peer {
    p.tier = store.get(&p.fingerprint).map(|s| s.tier);
    p.claims_name_of = match p.tier {
        Some(_) => None,
        None => store.named(&p.name, &p.fingerprint).map(|s| s.fingerprint.clone()),
    };
    p
}

/// `nearby.visibility` was set (to the same value too: picking `everyone` again restarts its
/// time). Called by settings after the value is stored.
pub fn visibility_set(d: &Daemon, value: &serde_json::Value) {
    let until = (value.as_str() == Some("everyone")).then(|| time::now_unix() + d.cfg.nearby.everyone_secs);
    let mut core = d.core();
    if core.state.nearby_everyone_until != until {
        core.state.nearby_everyone_until = until;
        drop(core);
        d.mark_dirty();
    }
}

/// `everyone` past its deadline goes back to `paired`.
fn expire(d: &Daemon) {
    let everyone = visibility(d) == Visibility::Everyone;
    let now = time::now_unix();
    let revert = {
        let mut core = d.core();
        match (everyone, core.state.nearby_everyone_until) {
            (true, Some(t)) => now >= t,
            (true, None) => {
                core.state.nearby_everyone_until = Some(now + d.cfg.nearby.everyone_secs);
                drop(core);
                d.mark_dirty();
                false
            }
            (false, Some(_)) => {
                core.state.nearby_everyone_until = None;
                drop(core);
                d.mark_dirty();
                false
            }
            (false, None) => false,
        }
    };
    if revert {
        crate::rpc::settings::set_as(d, Actor::system(), "nearby.visibility", json!("paired"));
    }
}

pub fn start(d: &Arc<Daemon>) {
    let service = d.cfg.nearby.service(&d.cfg.home);
    let iface = if d.cfg.nearby.local_only { dnssd::LOCAL_ONLY } else { dnssd::ANY };
    d.nearby.inner().service = service.clone();
    pairing::withdraw_stale(d);
    let port = match service {
        Some(_) => match open_listener() {
            Ok((listener, port)) => {
                d.nearby.inner().port = Some(port);
                link::listen(d, listener);
                share::start(d);
                transfer::start(d);
                Ok(port)
            }
            Err(e) => Err(e),
        },
        None => Err("Nearby is off".into()),
    };
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("nearby".into()).spawn(move || {
        // Dropped when the thread ends, which withdraws the advertisement.
        let mut worker = service.map(|s| Worker::new(s, iface, port));
        loop {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            expire(&d);
            match worker.as_mut() {
                Some(wk) => wk.step(&d),
                None => {
                    drop(d);
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    });
}

type InstKey = (String, u32, String);

/// One advertised instance the browse found.
struct Inst {
    tag: usize,
    resolve: Option<dnssd::Ref>,
    found: Option<Found>,
}

#[derive(Clone)]
struct Found {
    fingerprint: String,
    name: String,
    protocol: u32,
    host: String,
    port: u16,
    /// It advertised its own fingerprint and name (visible to everyone), not paired-only tags.
    named: bool,
}

struct Reg {
    r: dnssd::Ref,
    name: String,
    txt: Vec<u8>,
    up: bool,
}

struct Worker {
    service: String,
    iface: u32,
    /// The advertised port (the listener's).
    port: Result<u16, String>,
    /// The salt of this daemon's paired-only advertisement.
    salt: [u8; 16],
    browse: Option<dnssd::Ref>,
    reg: Option<Reg>,
    instances: HashMap<InstKey, Inst>,
    next_tag: usize,
    retry_at: Option<Instant>,
    error: Option<String>,
}

impl Worker {
    fn new(service: String, iface: u32, port: Result<u16, String>) -> Worker {
        Worker {
            service,
            iface,
            port,
            salt: transport::random(),
            browse: None,
            reg: None,
            instances: HashMap::new(),
            next_tag: REGISTER_TAG + 1,
            retry_at: None,
            error: None,
        }
    }

    fn fail(&mut self, what: &str, err: String) {
        let e = format!("{what}: {err}");
        if self.error.as_deref() != Some(&e) {
            eprintln!("midnad: nearby: {e}");
        }
        self.error = Some(e);
        self.retry_at = Some(Instant::now() + RETRY);
    }

    fn step(&mut self, d: &Daemon) {
        self.reconcile(d);
        self.publish(d);
        let mut fds: Vec<libc::pollfd> = vec![];
        let mut whose: Vec<Option<usize>> = vec![];
        if let Some(b) = &self.browse {
            fds.push(pollfd(b.fd()));
            whose.push(Some(BROWSE_TAG));
        }
        if let Some(r) = &self.reg {
            fds.push(pollfd(r.r.fd()));
            whose.push(Some(REGISTER_TAG));
        }
        for i in self.instances.values() {
            if let Some(r) = &i.resolve {
                fds.push(pollfd(r.fd()));
                whose.push(Some(i.tag));
            }
        }
        if fds.is_empty() {
            std::thread::sleep(TICK);
            return;
        }
        // SAFETY: `fds` is a valid array of `fds.len()` pollfds.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, TICK.as_millis() as i32) };
        if n <= 0 {
            return;
        }
        for (p, tag) in fds.iter().zip(whose) {
            if p.revents == 0 {
                continue;
            }
            let Some(tag) = tag else { continue };
            self.process(d, tag);
        }
        for ev in dnssd::take_events() {
            self.handle(d, ev);
        }
        self.publish(d);
    }

    fn process(&mut self, d: &Daemon, tag: usize) {
        match tag {
            BROWSE_TAG => {
                if let Some(Err(e)) = self.browse.as_ref().map(dnssd::Ref::process) {
                    self.stop_browse(d);
                    self.fail("looking for other Midnas", dnssd::describe(e));
                }
            }
            REGISTER_TAG => {
                if let Some(Err(e)) = self.reg.as_ref().map(|r| r.r.process()) {
                    self.reg = None;
                    self.fail("showing this Mac", dnssd::describe(e));
                }
            }
            tag => {
                if let Some(i) = self.instances.values_mut().find(|i| i.tag == tag)
                    && let Some(Err(_)) = i.resolve.as_ref().map(dnssd::Ref::process)
                {
                    i.resolve = None;
                }
            }
        }
    }

    /// Make the refs match the settings.
    fn reconcile(&mut self, d: &Daemon) {
        let vis = visibility(d);
        let may_retry = self.retry_at.is_none_or(|t| Instant::now() >= t);
        if vis == Visibility::Off {
            if self.browse.is_some() {
                self.stop_browse(d);
            }
        } else if self.browse.is_none() && may_retry {
            match dnssd::browse(BROWSE_TAG, self.iface, &self.service) {
                Ok(r) => self.browse = Some(r),
                Err(e) => self.fail("looking for other Midnas", dnssd::describe(e)),
            }
        }
        let me = identity(d).fingerprint.clone();
        let protocol = nb::PROTOCOL.to_string();
        let (instance, txt) = match vis {
            Visibility::Off => {
                self.reg = None;
                return;
            }
            Visibility::Everyone => {
                let name = display_name(d);
                let txt = nb::txt_record(&[(nb::TXT_PROTOCOL, &protocol), (nb::TXT_FINGERPRINT, &me), (nb::TXT_NAME, &name)]);
                (instance_name(&name), txt)
            }
            Visibility::Paired => {
                let mut salted = self.salt.to_vec();
                salted.extend_from_slice(me.as_bytes());
                let tags: Vec<String> = d.nearby.inner().store(&d.cfg.home).peers.values().filter_map(|p| p.tag(&salted)).collect();
                if tags.is_empty() {
                    self.reg = None;
                    return;
                }
                let salt = transport::hex(&self.salt);
                let mut pairs: Vec<(String, String)> = vec![(nb::TXT_PROTOCOL.into(), protocol.clone()), (nb::TXT_SALT.into(), salt.clone())];
                for (n, chunk) in tags.chunks(TAGS_PER_STRING).enumerate() {
                    let key = if n == 0 { nb::TXT_TAGS.to_string() } else { format!("{}{}", nb::TXT_TAGS, n + 1) };
                    pairs.push((key, chunk.concat()));
                }
                let pairs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                (format!("Midna {}", &salt[..4]), nb::txt_record(&pairs))
            }
        };
        if self.reg.as_ref().is_some_and(|r| r.name == instance && r.txt == txt) || !may_retry {
            return;
        }
        self.reg = None;
        let port = match &self.port {
            Ok(port) => *port,
            Err(e) => {
                let e = e.clone();
                self.fail("showing this Mac", e);
                return;
            }
        };
        match dnssd::register(REGISTER_TAG, self.iface, &instance, &self.service, port, &txt) {
            Ok(r) => self.reg = Some(Reg { r, name: instance, txt, up: false }),
            Err(e) => self.fail("showing this Mac", dnssd::describe(e)),
        }
    }

    fn handle(&mut self, d: &Daemon, ev: dnssd::Ev) {
        match ev {
            dnssd::Ev::Registered { err, .. } => {
                if err == 0 {
                    if let Some(r) = self.reg.as_mut() {
                        r.up = true;
                    }
                } else {
                    self.reg = None;
                    self.fail("showing this Mac", dnssd::describe(err));
                }
            }
            dnssd::Ev::Browsed { err, .. } if err != 0 => {
                self.stop_browse(d);
                self.fail("looking for other Midnas", dnssd::describe(err));
            }
            dnssd::Ev::Browsed { add: true, iface, name, regtype, domain, .. } => {
                let key = (name, iface, domain);
                if self.instances.contains_key(&key) {
                    return;
                }
                let tag = self.next_tag;
                self.next_tag += 1;
                let resolve = dnssd::resolve(tag, iface, &key.0, &regtype, &key.2).ok();
                self.instances.insert(key, Inst { tag, resolve, found: None });
            }
            dnssd::Ev::Browsed { add: false, iface, name, domain, .. } => {
                if self.instances.remove(&(name, iface, domain)).is_some_and(|i| i.found.is_some()) {
                    self.sync_peers(d);
                }
            }
            dnssd::Ev::Resolved { tag, err, host, port, txt } => {
                let Some(i) = self.instances.values_mut().find(|i| i.tag == tag) else { return };
                // One answer is enough; a peer that changes its record re-advertises.
                i.resolve = None;
                if err != 0 {
                    return;
                }
                let fields: HashMap<String, String> = nb::parse_txt(&txt).into_iter().collect();
                let protocol = fields.get(nb::TXT_PROTOCOL).and_then(|v| v.parse().ok()).unwrap_or(0);
                let (fingerprint, name, named) = match fields.get(nb::TXT_FINGERPRINT) {
                    Some(fp) if is_fingerprint(fp) => (fp.clone(), fields.get(nb::TXT_NAME).cloned().unwrap_or_default(), true),
                    Some(_) => return,
                    None => match paired_tag(d, &fields) {
                        Some((fp, name)) => (fp, name, false),
                        None => return,
                    },
                };
                if fingerprint == identity(d).fingerprint {
                    return;
                }
                i.found = Some(Found { fingerprint, name, protocol, host, port, named });
                self.sync_peers(d);
            }
        }
    }

    fn stop_browse(&mut self, d: &Daemon) {
        self.browse = None;
        self.instances.clear();
        self.sync_peers(d);
    }

    /// Peers are devices, not instances: one device seen on two interfaces is one peer, and it
    /// leaves when its last instance goes.
    fn sync_peers(&mut self, d: &Daemon) {
        let mut seen: BTreeMap<String, Found> = BTreeMap::new();
        for f in self.instances.values().filter_map(|i| i.found.as_ref()) {
            seen.entry(f.fingerprint.clone()).or_insert_with(|| f.clone());
        }
        let (mut appeared, mut left) = (vec![], vec![]);
        {
            let mut inner = d.nearby.inner();
            let store = inner.store(&d.cfg.home);
            // A paired device that shows itself under a new name: keep the new one.
            let renamed: Vec<(String, String)> = seen
                .values()
                .filter(|f| f.named && !f.name.trim().is_empty())
                .filter(|f| store.get(&f.fingerprint).is_some_and(|s| s.name != f.name.trim()))
                .map(|f| (f.fingerprint.clone(), f.name.trim().to_string()))
                .collect();
            if !renamed.is_empty() {
                for (fp, name) in renamed {
                    if let Some(s) = store.peers.get_mut(&fp) {
                        s.name = name;
                    }
                }
                if let Err(e) = store.save() {
                    eprintln!("midnad: nearby: saving the paired devices: {e}");
                }
            }
            let store = store.clone();
            inner.peers.retain(|fp, p| {
                let keep = seen.contains_key(fp);
                if !keep {
                    left.push(PeerLeft { fingerprint: fp.clone(), name: p.name.clone() });
                }
                keep
            });
            for (fp, f) in seen {
                let new = !inner.peers.contains_key(&fp);
                let peer = inner.peers.entry(fp.clone()).or_insert_with(|| Peer {
                    fingerprint: fp,
                    name: String::new(),
                    protocol: 0,
                    host: None,
                    port: None,
                    seen_since: time::now_rfc3339(),
                    tier: None,
                    claims_name_of: None,
                });
                peer.name = if f.name.is_empty() { "Midna".into() } else { f.name };
                peer.protocol = f.protocol;
                peer.host = Some(f.host).filter(|h| !h.is_empty());
                peer.port = Some(f.port).filter(|p| *p != 0);
                if new {
                    appeared.push(annotate(peer.clone(), &store));
                }
            }
        }
        for p in left {
            d.emit(kinds::PEERS_LEFT, Actor::system(), None, None, serde_json::to_value(&p).unwrap_or_default());
        }
        for p in appeared {
            d.emit(kinds::PEERS_APPEARED, Actor::system(), None, None, serde_json::to_value(&p).unwrap_or_default());
        }
    }

    fn publish(&mut self, d: &Daemon) {
        let vis = visibility(d);
        let healthy = (vis == Visibility::Off || self.browse.is_some()) && (vis != Visibility::Everyone || self.reg.as_ref().is_some_and(|r| r.up));
        if healthy {
            self.error = None;
            self.retry_at = None;
        }
        let mut i = d.nearby.inner();
        i.browsing = self.browse.is_some();
        i.advertising = self.reg.as_ref().is_some_and(|r| r.up);
        i.error = self.error.clone();
    }
}

fn pollfd(fd: i32) -> libc::pollfd {
    libc::pollfd { fd, events: libc::POLLIN, revents: 0 }
}

fn is_fingerprint(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Tags in one TXT string (each [`nb::TAG_HEX`] digits; a string holds 255 bytes).
const TAGS_PER_STRING: usize = 30;

/// The paired device a paired-only advertisement is from, if it's one of this device's:
/// (fingerprint, name as stored).
fn paired_tag(d: &Daemon, fields: &HashMap<String, String>) -> Option<(String, String)> {
    let salt = transport::unhex(fields.get(nb::TXT_SALT)?)?;
    let mut tags = std::collections::HashSet::new();
    for (k, v) in fields {
        let Some(rest) = k.strip_prefix(nb::TXT_TAGS) else { continue };
        if !rest.is_empty() && !rest.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let mut s = v.as_str();
        while s.len() >= nb::TAG_HEX && s.is_char_boundary(nb::TAG_HEX) {
            tags.insert(s[..nb::TAG_HEX].to_string());
            s = &s[nb::TAG_HEX..];
        }
    }
    let mut i = d.nearby.inner();
    let store = i.store(&d.cfg.home);
    store.peers.values().find_map(|p| {
        let mut salted = salt.clone();
        salted.extend_from_slice(p.fingerprint.as_bytes());
        p.tag(&salted).filter(|t| tags.contains(t)).map(|_| (p.fingerprint.clone(), p.name.clone()))
    })
}

/// A DNS-SD instance name is at most 63 bytes.
fn instance_name(name: &str) -> String {
    let mut end = name.len().min(63);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    match name[..end].trim() {
        "" => "Midna".into(),
        n => n.into(),
    }
}

/// The listener for Nearby connections, on every address (IPv6 and IPv4).
fn open_listener() -> Result<(std::net::TcpListener, u16), String> {
    let (fd, port) = reserve_port()?;
    // SAFETY: listen on a socket this function owns.
    if unsafe { libc::listen(std::os::fd::AsRawFd::as_raw_fd(&fd), 16) } != 0 {
        return Err(format!("can't take Nearby connections: {}", std::io::Error::last_os_error()));
    }
    Ok((std::net::TcpListener::from(fd), port))
}

/// A TCP port for Nearby connections: bound on every address, not listening yet.
fn reserve_port() -> Result<(OwnedFd, u16), String> {
    // SAFETY: plain socket calls on a descriptor this function owns; `addr` and `len` are valid.
    unsafe {
        let fd = libc::socket(libc::AF_INET6, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(format!("no socket for Nearby: {}", std::io::Error::last_os_error()));
        }
        let owned = OwnedFd::from_raw_fd(fd);
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        let off: libc::c_int = 0;
        libc::setsockopt(fd, libc::IPPROTO_IPV6, libc::IPV6_V6ONLY, (&off as *const libc::c_int).cast(), size_of::<libc::c_int>() as libc::socklen_t);
        let mut addr: libc::sockaddr_in6 = std::mem::zeroed();
        addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
        addr.sin6_len = size_of::<libc::sockaddr_in6>() as u8;
        if libc::bind(fd, (&addr as *const libc::sockaddr_in6).cast(), size_of::<libc::sockaddr_in6>() as libc::socklen_t) != 0 {
            return Err(format!("no port for Nearby: {}", std::io::Error::last_os_error()));
        }
        let mut len = size_of::<libc::sockaddr_in6>() as libc::socklen_t;
        if libc::getsockname(fd, (&mut addr as *mut libc::sockaddr_in6).cast(), &mut len) != 0 {
            return Err(format!("no port for Nearby: {}", std::io::Error::last_os_error()));
        }
        Ok((owned, u16::from_be(addr.sin6_port)))
    }
}

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCDynamicStoreCopyComputerName(store: *const std::ffi::c_void, encoding: *mut u32) -> core_foundation_sys::string::CFStringRef;
}

/// The Mac's computer name (System Settings › General › Sharing), else its host name.
fn computer_name() -> String {
    // SAFETY: a null store asks the system's; the result is ours to release (create rule).
    let name = unsafe {
        let r = SCDynamicStoreCopyComputerName(std::ptr::null(), std::ptr::null_mut());
        (!r.is_null()).then(|| core_foundation::base::TCFType::wrap_under_create_rule(r)).map(|s: core_foundation::string::CFString| s.to_string())
    };
    if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
        return n.trim().to_string();
    }
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes.
    let host = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) == 0 }
        .then(|| String::from_utf8_lossy(&buf[..buf.iter().position(|b| *b == 0).unwrap_or(0)]).into_owned())
        .unwrap_or_default();
    let host = host.strip_suffix(".local").unwrap_or(&host).to_string();
    if host.is_empty() { "Mac".into() } else { host }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_by_home() {
        let auto = Setup { mode: Mode::Auto, ..Setup::default() };
        let expected = if paths::is_dev() { nb::DEV_SERVICE } else { nb::SERVICE };
        assert_eq!(auto.service(&paths::production_home()).as_deref(), Some(expected));
        assert_eq!(auto.service(Path::new("/tmp/midna-x")).as_deref(), Some(nb::DEV_SERVICE), "temp homes never see production");
        assert_eq!(Setup::default().service(Path::new("/tmp/midna-x")), None);
        let own = Setup { mode: Mode::Service("_midna-t1._tcp".into()), ..Setup::default() };
        assert_eq!(own.service(&paths::production_home()).as_deref(), Some("_midna-t1._tcp"));
    }

    #[test]
    fn instance_names_fit() {
        assert_eq!(instance_name("  "), "Midna");
        assert_eq!(instance_name("Studio Mac"), "Studio Mac");
        let long = "ü".repeat(40);
        let n = instance_name(&long);
        assert!(n.len() <= 63 && long.starts_with(&n));
    }

    #[test]
    fn a_port_is_held() {
        let (_fd, port) = reserve_port().unwrap();
        assert_ne!(port, 0);
        assert!(!computer_name().is_empty());
    }
}
