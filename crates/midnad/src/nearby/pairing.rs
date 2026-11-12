//! Pairing two devices (`peers.pair`, `peers.confirm`).
//!
//! One side asks over an authenticated Noise connection ([`super::transport`]), so each side
//! already knows the other's device key for certain. What neither knows yet is whether that key
//! is the device across the room or someone in the middle, so both agree on a 6-digit code that
//! depends on the handshake hash and both keys, and the people compare the codes on the two
//! screens. To stop a man in the middle from trying nonces until two codes collide, the code also
//! depends on a nonce from each side, and the responder commits to its nonce before it sees the
//! initiator's (as Bluetooth's numeric comparison does): `commit = H(h ‖ nb)`, then `na`, then
//! `nb`, then `code = H(h ‖ initiator fp ‖ responder fp ‖ na ‖ nb)` mod 10⁶.
//!
//! Each side's person confirms with `peers.confirm` (human only; each side gets a needs-you
//! approval whose deferred call is that) and picks how to keep the other device. A side stores
//! the device once both people confirmed. Saying no, a different code, the 2 minutes running out
//! or the connection breaking ends it on both sides with nothing stored.
use super::store::Stored;
use super::transport::{self, Channel, Wire, hex, random, unhex};
use super::{Inner, display_name, identity, local, visibility};
use crate::daemon::Daemon;
use crate::rpc::{R, ok};
use midna_proto::nearby::{
    self as nb, PAIRING_SECS, Pairing, PairingRole, PairingState, PeersConfirmParams, PeersForgetParams, PeersForgetResult,
    PeersPairParams, Tier, Visibility,
};
use midna_proto::{
    ActionKind, Actor, ApprovalRequest, NeedsYou, NeedsYouKind, PolicyAction, RpcError, kinds, time,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

/// How long one step of agreeing on the code may take.
const STEP: Duration = transport::HANDSHAKE_TIMEOUT;
/// Pairings this device may be asked into at once.
const MAX_INCOMING: usize = 4;
/// How long `peers.list` still shows a pairing that ended.
const KEEP_ENDED: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(200);
pub const CONFIRM_METHOD: &str = "peers.confirm";

pub struct Slot {
    pub info: Pairing,
    ended: Option<Instant>,
}

/// What both sides agreed on.
pub struct Agreed {
    /// 6 digits.
    pub code: String,
    /// The pair's presence key (paired-only advertisements).
    pub presence: [u8; 32],
    /// The name the other side gave.
    pub name: String,
}

fn sha(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn commitment(hash: &[u8], nb: &[u8; 32]) -> [u8; 32] {
    sha(&[b"midna-pair-commit/1", hash, nb])
}

pub fn code(hash: &[u8], initiator: &str, responder: &str, na: &[u8; 32], nb: &[u8; 32]) -> String {
    let d = sha(&[b"midna-pair-sas/1", hash, initiator.as_bytes(), responder.as_bytes(), na, nb]);
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000;
    format!("{n:06}")
}

pub fn presence(hash: &[u8], na: &[u8; 32], nb: &[u8; 32]) -> [u8; 32] {
    sha(&[b"midna-pair-presence/1", hash, na, nb])
}

fn nonce(s: &str) -> Result<[u8; 32], String> {
    unhex(s).and_then(|v| <[u8; 32]>::try_from(v).ok()).ok_or_else(|| "the other device sent a bad nonce".to_string())
}

/// What a message that should have been the next pairing step says instead.
fn not_a_step(m: Wire) -> String {
    match m {
        Wire::Refused { reason } if reason == transport::NOT_PAIRED => "the other device turned the request away".into(),
        Wire::Refused { reason } => format!("the other device turned the request away: {reason}"),
        Wire::PairDecline { reason } => format!("the other device declined: {reason}"),
        _ => "the other device broke the pairing protocol".into(),
    }
}

/// The asking side, on a fresh connection: request, nonces, code. `me` is this device's
/// fingerprint and `name` the name it gives.
pub fn agree_as_initiator(ch: &mut Channel, me: &str, name: &str) -> Result<Agreed, String> {
    ch.send(&Wire::PairRequest { name: name.into(), protocol: nb::PROTOCOL })?;
    let (commit, peer_name) = match ch.expect(STEP)? {
        Wire::PairCommit { commit, name } => (nonce(&commit)?, name),
        m => return Err(not_a_step(m)),
    };
    let na: [u8; 32] = random();
    ch.send(&Wire::PairNonce { nonce: hex(&na) })?;
    let nb = match ch.expect(STEP)? {
        Wire::PairReveal { nonce: n } => nonce(&n)?,
        m => return Err(not_a_step(m)),
    };
    if commitment(&ch.hash, &nb) != commit {
        let _ = ch.send(&Wire::PairDecline { reason: "the nonce didn't match its commitment".into() });
        return Err("the other device broke the pairing protocol (its nonce didn't match its commitment)".into());
    }
    Ok(Agreed { code: code(&ch.hash, me, &ch.fingerprint, &na, &nb), presence: presence(&ch.hash, &na, &nb), name: peer_name })
}

/// The asked side, once the request (`peer_name`) came in.
pub fn agree_as_responder(ch: &mut Channel, me: &str, name: &str, peer_name: String) -> Result<Agreed, String> {
    let nb: [u8; 32] = random();
    ch.send(&Wire::PairCommit { commit: hex(&commitment(&ch.hash, &nb)), name: name.into() })?;
    let na = match ch.expect(STEP)? {
        Wire::PairNonce { nonce: n } => nonce(&n)?,
        m => return Err(not_a_step(m)),
    };
    ch.send(&Wire::PairReveal { nonce: hex(&nb) })?;
    Ok(Agreed { code: code(&ch.hash, &ch.fingerprint, me, &na, &nb), presence: presence(&ch.hash, &na, &nb), name: peer_name })
}

fn clean_name(n: &str) -> String {
    let n: String = n.chars().filter(|c| !c.is_control()).take(63).collect();
    match n.trim() {
        "" => "Midna".into(),
        t => t.into(),
    }
}

pub(super) fn prune(i: &mut Inner) {
    i.pairings.retain(|_, s| s.ended.is_none_or(|t| t.elapsed() < KEEP_ENDED));
}

/// `peers.pair`: connect to a listed device, agree on the code, and return the pairing once
/// both screens show it.
pub fn pair(d: &Arc<Daemon>, p: PeersPairParams) -> R {
    let me = identity(d).fingerprint.clone();
    if p.fingerprint == me {
        return Err(RpcError::bad_params("that's this Mac"));
    }
    let peer = {
        let mut i = d.nearby.inner();
        prune(&mut i);
        if i.pairings.values().any(|s| s.info.fingerprint == p.fingerprint && !s.info.state.is_over()) {
            return Err(RpcError::conflict("already pairing with that device; confirm or decline that pairing first"));
        }
        i.peers.get(&p.fingerprint).cloned()
    };
    let Some(peer) = peer else {
        return Err(RpcError::not_found(
            "no device with that id in peers.list: it must be on this network and visible to everyone (or already paired)",
        ));
    };
    if peer.protocol != nb::PROTOCOL {
        return Err(RpcError::refused(format!(
            "{} speaks Nearby protocol {} and this Mac {}; update the older one first",
            peer.name,
            peer.protocol,
            nb::PROTOCOL
        )));
    }
    let addrs = addresses(d, peer.host.as_deref(), peer.port)?;
    let mut ch = transport::connect(&addrs, local(d)).map_err(|e| RpcError::refused(format!("couldn't reach {}: {e}", peer.name)))?;
    if ch.fingerprint != p.fingerprint {
        return Err(RpcError::refused(format!(
            "the device that answered at {}'s address has a different key ({}); it isn't the device listed",
            peer.name,
            nb::short_fingerprint(&ch.fingerprint)
        )));
    }
    let agreed = agree_as_initiator(&mut ch, &me, &display_name(d)).map_err(RpcError::refused)?;
    ok(begin(d, ch, PairingRole::Outgoing, agreed))
}

/// Where to connect to a peer seen at `host`:`port`.
pub fn addresses(d: &Daemon, host: Option<&str>, port: Option<u16>) -> Result<Vec<SocketAddr>, RpcError> {
    let port = port.ok_or_else(|| RpcError::refused("that device hasn't said which port it takes connections on"))?;
    if d.cfg.nearby.local_only {
        return Ok(vec![SocketAddr::from(([127, 0, 0, 1], port)), SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port))]);
    }
    let host = host.map(|h| h.trim_end_matches('.')).filter(|h| !h.is_empty()).ok_or_else(|| RpcError::refused("that device has no address yet"))?;
    let addrs: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|e| RpcError::refused(format!("looking up {host}: {e}")))?.collect();
    if addrs.is_empty() {
        return Err(RpcError::refused(format!("{host} has no address")));
    }
    Ok(addrs)
}

/// The other device asked to pair (its first message was `pair_request`).
pub fn incoming(d: &Arc<Daemon>, mut ch: Channel, name: String, protocol: u32) {
    let turn_away = |mut ch: Channel, reason: &str| {
        let _ = ch.send(&Wire::Refused { reason: reason.into() });
        ch.finish();
    };
    if protocol != nb::PROTOCOL {
        return turn_away(ch, &format!("this Mac speaks Nearby protocol {}", nb::PROTOCOL));
    }
    let vis = visibility(d);
    let (known, replaced, busy) = {
        let mut i = d.nearby.inner();
        prune(&mut i);
        let known = i.store(&d.cfg.home).get(&ch.fingerprint).is_some();
        let live = |s: &&Slot| !s.info.state.is_over();
        let replaced: Vec<String> = i.pairings.values().filter(live).filter(|s| s.info.fingerprint == ch.fingerprint).map(|s| s.info.id.clone()).collect();
        let others = i.pairings.values().filter(live).filter(|s| s.info.role == PairingRole::Incoming && s.info.fingerprint != ch.fingerprint).count();
        (known, replaced, others >= MAX_INCOMING)
    };
    if vis != Visibility::Everyone && !known {
        return turn_away(ch, "this Mac isn't taking pairing requests (it isn't visible to everyone)");
    }
    if busy {
        return turn_away(ch, "this Mac has too many pairing requests waiting; try again in a couple of minutes");
    }
    for id in replaced {
        end(d, &id, PairingState::Failed, "the device asked again", Close::Withdraw);
    }
    let me = identity(d).fingerprint.clone();
    match agree_as_responder(&mut ch, &me, &display_name(d), name) {
        Ok(agreed) => {
            begin(d, ch, PairingRole::Incoming, agreed);
        }
        Err(e) => eprintln!("midnad: nearby: a pairing request from {} broke off: {e}", nb::short_fingerprint(&ch.fingerprint)),
    }
}

/// Both sides have the code: keep the pairing, ask this side's person, and wait for both.
fn begin(d: &Arc<Daemon>, ch: Channel, role: PairingRole, agreed: Agreed) -> Pairing {
    let id = format!("pr_{}", crate::state::hex_id(6));
    let name = clean_name(&agreed.name);
    let now = time::now_unix();
    let claims_name_of = d.nearby.inner().store(&d.cfg.home).named(&name, &ch.fingerprint).map(|s| s.fingerprint.clone());
    let mut info = Pairing {
        id: id.clone(),
        role,
        fingerprint: ch.fingerprint.clone(),
        name,
        claims_name_of,
        state: PairingState::Comparing,
        code: Some(agreed.code.clone()),
        confirmed: false,
        peer_confirmed: false,
        tier: None,
        needs_you_id: None,
        started_at: time::format_unix(now),
        expires_at: time::format_unix(now + PAIRING_SECS),
        error: None,
    };
    d.nearby.inner().pairings.insert(id.clone(), Slot { info: info.clone(), ended: None });
    let ny = ask(d, &info);
    info.needs_you_id = Some(ny.clone());
    if let Some(s) = d.nearby.inner().pairings.get_mut(&id) {
        s.info.needs_you_id = Some(ny);
    }
    d.emit(kinds::PEERS_PAIRING, Actor::system(), None, None, serde_json::to_value(&info).unwrap_or_default());
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("nearby-pairing".into()).spawn(move || run(w, ch, id, agreed));
    info
}

/// The needs-you approval that asks this side's person to compare the codes.
fn ask(d: &Daemon, p: &Pairing) -> String {
    let code = nb::spaced_code(p.code.as_deref().unwrap_or_default());
    let mut item = d.new_needs_you(NeedsYouKind::Approval, format!("Pair with {}? Code {code}", p.name), Actor::system(), None);
    let asked = match p.role {
        PairingRole::Outgoing => format!("You asked to pair with {}.", p.name),
        PairingRole::Incoming => format!("{} asks to pair with this Mac.", p.name),
    };
    let clash = match &p.claims_name_of {
        Some(fp) => format!(
            "\nCareful: you already paired with a device called {}, and this isn't it (device {}, not {}).",
            p.name,
            nb::short_fingerprint(&p.fingerprint),
            nb::short_fingerprint(fp)
        ),
        None => String::new(),
    };
    item.detail = format!(
        "{asked} Approve only if {} shows the same code: {code}. Approving keeps it as a contact; to keep it as one of \
         your own Macs instead run: midna peers confirm {} --as my-device\nDevice {}{clash}",
        p.name,
        p.id,
        nb::short_fingerprint(&p.fingerprint)
    );
    item.approval = Some(ApprovalRequest {
        action: PolicyAction { kind: ActionKind::Cli, value: format!("midna peers confirm {}", p.id), session: None, project: None },
        matched_rule: None,
        target_session: None,
    });
    d.core().state.deferred.insert(
        item.id.clone(),
        crate::state::Deferred { method: CONFIRM_METHOD.into(), params: json!({ "pairing": p.id, "accept": true }), guard: None },
    );
    d.raise_needs_you(item).id
}

/// The pairing's connection, until both confirmed or it ended.
fn run(w: Weak<Daemon>, mut ch: Channel, id: String, agreed: Agreed) {
    let deadline = Instant::now() + Duration::from_secs(PAIRING_SECS as u64);
    let mut sent = false;
    loop {
        let Some(d) = w.upgrade() else { return };
        if d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let Some(info) = d.nearby.inner().pairings.get(&id).map(|s| s.info.clone()) else { return };
        if info.state.is_over() {
            // Ended on this side (declined, replaced): tell the other side why.
            let _ = ch.send(&Wire::PairDecline { reason: info.error.unwrap_or_else(|| "declined".into()) });
            return ch.finish();
        }
        if info.confirmed && !sent {
            if let Err(e) = ch.send(&Wire::PairConfirm) {
                end(&d, &id, PairingState::Failed, &format!("lost the connection: {e}"), Close::Withdraw);
                return;
            }
            sent = true;
        }
        if info.confirmed && info.peer_confirmed {
            complete(&d, &ch, &info, &agreed);
            return ch.finish();
        }
        if Instant::now() >= deadline {
            end(&d, &id, PairingState::Failed, "time ran out before both people confirmed", Close::Withdraw);
            continue;
        }
        drop(d);
        let got = ch.recv(POLL);
        let Some(d) = w.upgrade() else { return };
        match got {
            Ok(None) => {}
            Ok(Some(Wire::PairConfirm)) => {
                if let Some(s) = d.nearby.inner().pairings.get_mut(&id) {
                    s.info.peer_confirmed = true;
                }
            }
            Ok(Some(Wire::PairDecline { reason })) => {
                end(&d, &id, PairingState::Declined, &format!("the other device declined ({reason})"), Close::Withdraw);
                return ch.finish();
            }
            Ok(Some(_)) => {
                end(&d, &id, PairingState::Failed, "the other device broke the pairing protocol", Close::Withdraw);
                return ch.finish();
            }
            Err(e) => {
                end(&d, &id, PairingState::Failed, &format!("lost the connection: {e}"), Close::Withdraw);
                return;
            }
        }
    }
}

/// Both people confirmed: keep the device.
fn complete(d: &Daemon, ch: &Channel, info: &Pairing, agreed: &Agreed) {
    let stored = Stored {
        fingerprint: ch.fingerprint.clone(),
        public_key: hex(ch.peer_key.as_bytes()),
        name: info.name.clone(),
        tier: info.tier.unwrap_or_default(),
        paired_at: time::now_rfc3339(),
        presence: hex(&agreed.presence),
    };
    let saved = d.nearby.inner().store(&d.cfg.home).insert(stored.clone());
    if let Err(e) = saved {
        end(d, &info.id, PairingState::Failed, &format!("couldn't save the paired device: {e}"), Close::Withdraw);
        return;
    }
    if end(d, &info.id, PairingState::Paired, "", Close::Withdraw).is_some() {
        let nearby = d.nearby.inner().peers.contains_key(&stored.fingerprint);
        d.emit(kinds::PEERS_PAIRED, Actor::system(), None, None, serde_json::to_value(stored.paired(nearby)).unwrap_or_default());
    }
}

/// What ending a pairing does with its needs-you item.
enum Close {
    Withdraw,
    As(Value, Actor),
    /// It's closing already (that's why the pairing ends).
    Closed,
}

/// End a pairing that hasn't ended: set its state, close its needs-you and say so. None if it
/// had already ended (or doesn't exist).
fn end(d: &Daemon, id: &str, state: PairingState, why: &str, close: Close) -> Option<Pairing> {
    let info = {
        let mut i = d.nearby.inner();
        let s = i.pairings.get_mut(id)?;
        if s.info.state.is_over() {
            return None;
        }
        s.info.state = state;
        s.info.error = (!why.is_empty()).then(|| why.to_string());
        s.ended = Some(Instant::now());
        s.info.clone()
    };
    if let Some(ny) = &info.needs_you_id {
        match close {
            Close::As(resolution, by) => drop(d.close_needs_you(ny, resolution, by)),
            Close::Withdraw => drop(d.withdraw_needs_you(ny, &format!("the pairing ended: {why}"))),
            Close::Closed => {}
        }
    }
    if state != PairingState::Paired {
        d.emit(kinds::PEERS_PAIRING_ENDED, Actor::system(), None, None, serde_json::to_value(&info).unwrap_or_default());
    }
    Some(info)
}

/// `peers.confirm` (human only).
pub fn confirm(d: &Daemon, p: PeersConfirmParams) -> R {
    let info = {
        let mut i = d.nearby.inner();
        prune(&mut i);
        let s = i.pairings.get_mut(&p.pairing).ok_or_else(|| RpcError::not_found(format!("no pairing {}", p.pairing)))?;
        match s.info.state {
            PairingState::Comparing => {}
            st if st.is_over() => {
                let why = s.info.error.clone().map(|e| format!(": {e}")).unwrap_or_default();
                return Err(RpcError::conflict(format!("that pairing already ended ({}{why})", serde_json::to_value(st).unwrap_or_default())));
            }
            _ => return Err(RpcError::conflict("that pairing has no code yet")),
        }
        if p.accept {
            s.info.confirmed = true;
            s.info.tier = Some(p.tier.unwrap_or(Tier::Contact));
        }
        s.info.clone()
    };
    if !p.accept {
        let deny = Close::As(json!({ "kind": "deny" }), Actor::human());
        return ok(end(d, &info.id, PairingState::Declined, "declined on this Mac", deny).unwrap_or(info));
    }
    if let Some(ny) = &info.needs_you_id {
        d.close_needs_you(ny, json!({ "kind": "approve", "scope": { "kind": "once" } }), Actor::human());
    }
    ok(info)
}

/// A needs-you item closed: if it was a pairing's and the person didn't confirm (they denied
/// or dismissed it, or it was withdrawn), the pairing is declined.
pub fn needs_you_closed(d: &Daemon, item: &NeedsYou, resolution: &Value) {
    if item.kind != NeedsYouKind::Approval {
        return;
    }
    let id = d.nearby.inner().pairings.values().find(|s| s.info.needs_you_id.as_deref() == Some(item.id.as_str()) && !s.info.confirmed).map(|s| s.info.id.clone());
    let Some(id) = id else { return };
    let how = resolution.get("kind").and_then(Value::as_str).unwrap_or("closed");
    let why = if how == "withdrawn" { "the request was withdrawn".to_string() } else { "declined on this Mac".to_string() };
    end(d, &id, PairingState::Declined, &why, Close::Closed);
}

/// Approvals for pairings that ended with the daemon that raised them.
pub fn withdraw_stale(d: &Daemon) {
    let stale: Vec<String> = d.core().state.deferred.iter().filter(|(_, def)| def.method == CONFIRM_METHOD).map(|(id, _)| id.clone()).collect();
    for id in stale {
        d.withdraw_needs_you(&id, "midnad restarted, which ended the pairing");
    }
}

/// `peers.set_tier` (human only).
pub fn set_tier(d: &Daemon, p: nb::PeersSetTierParams) -> R {
    let peer = {
        let mut i = d.nearby.inner();
        let s = i.store(&d.cfg.home).set_tier(&p.fingerprint, p.tier).map_err(|e| RpcError::internal(format!("saving the paired devices: {e}")))?;
        s.map(|s| s.paired(i.peers.contains_key(&s.fingerprint)))
    };
    let Some(peer) = peer else { return Err(RpcError::not_found("no paired device with that id")) };
    d.emit(kinds::PEERS_CHANGED, Actor::system(), None, None, serde_json::to_value(&peer).unwrap_or_default());
    ok(peer)
}

/// `peers.forget` (human only).
pub fn forget(d: &Daemon, p: PeersForgetParams) -> R {
    let gone = {
        let mut i = d.nearby.inner();
        let gone = i.store(&d.cfg.home).remove(&p.fingerprint).map_err(|e| RpcError::internal(format!("saving the paired devices: {e}")))?;
        gone.map(|s| s.paired(i.peers.contains_key(&s.fingerprint)))
    };
    let Some(forgotten) = gone else { return Err(RpcError::not_found("no paired device with that id")) };
    d.emit(kinds::PEERS_FORGOTTEN, Actor::system(), None, None, serde_json::to_value(&forgotten).unwrap_or_default());
    ok(PeersForgetResult { forgotten })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_depend_on_everything() {
        let h = [1u8; 32];
        let (na, nb) = ([2u8; 32], [3u8; 32]);
        let c = code(&h, "a", "b", &na, &nb);
        assert_eq!(c.len(), nb::CODE_DIGITS);
        assert!(c.bytes().all(|b| b.is_ascii_digit()));
        assert_eq!(c, code(&h, "a", "b", &na, &nb));
        let others = [code(&[9; 32], "a", "b", &na, &nb), code(&h, "b", "a", &na, &nb), code(&h, "a", "b", &[4; 32], &nb), code(&h, "a", "b", &na, &[4; 32])];
        assert!(others.iter().all(|o| *o != c), "{c} {others:?}");
        assert_ne!(commitment(&h, &nb), commitment(&h, &na));
        assert_ne!(presence(&h, &na, &nb), presence(&h, &nb, &na));
        assert_eq!(clean_name("  \u{7}Bravo\n "), "Bravo");
        assert_eq!(clean_name(""), "Midna");
    }
}
