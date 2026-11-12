//! Sharing between paired devices (`share.send|accept|decline`, `inbox.list`).
//!
//! The sender offers a share over an authenticated connection ([`Wire::ShareOffer`]); the
//! receiver checks it, keeps it in its inbox (`nearby/inbox.json`, mode 0600, both directions,
//! the latest [`MAX_KEPT`]) and answers [`Wire::ShareAck`]. Who sent it is the connection's
//! device key, never anything the message says. An incoming share is data only: it raises a
//! needs-you approval and waits. Only the human's `share.accept` (or approving that item) acts
//! on it: a message is marked read, a snippet is pasted into the terminal they pick (never
//! followed by Enter), a link is opened or pinned. Accepting or declining owes the sender a
//! [`Wire::ShareResult`], delivered at once if it can be seen and retried while it can't.
use super::transport::{self, Wire};
use super::{identity, link};
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{
    self as nb, InboxListParams, InboxListResult, ShareAcceptParams, ShareDeclineParams, ShareDirection, ShareItem, ShareLinkAction,
    SharePayload, ShareSendParams, ShareState, Tier,
};
use midna_proto::{ActionKind, Actor, ApprovalRequest, LinksAddParams, NeedsYou, NeedsYouKind, PolicyAction, RpcError, kinds, time};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Shares kept in the inbox, both directions; the oldest decided ones go first.
const MAX_KEPT: usize = 500;
/// The most text the inbox keeps in all; the oldest decided shares go first.
const MAX_KEPT_BYTES: u64 = 16 << 20;
/// How often owed results are retried.
const RETRY: Duration = Duration::from_secs(3);
/// `inbox.list` without a limit.
const LIST_DEFAULT: usize = 100;
/// The deferred call an incoming share's approval runs.
pub const ACCEPT_METHOD: &str = "share.accept";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    item: ShareItem,
    /// Incoming and decided: the sender hasn't been told yet.
    #[serde(default)]
    owe: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    items: Vec<Entry>,
}

/// `nearby/inbox.json`, oldest first.
pub struct Inbox {
    path: PathBuf,
    items: Vec<Entry>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join("nearby").join("inbox.json")
}

impl Inbox {
    pub fn load(home: &Path) -> Inbox {
        let path = path(home);
        let file: File = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("midnad: nearby: {} can't be read ({e}); starting an empty inbox", path.display());
                File::default()
            }),
            Err(_) => File::default(),
        };
        Inbox { path, items: file.items }
    }

    fn get(&self, id: &str) -> Option<&Entry> {
        self.items.iter().find(|e| e.item.id == id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.items.iter_mut().find(|e| e.item.id == id)
    }

    fn push(&mut self, e: Entry) {
        self.items.push(e);
        self.prune();
    }

    /// Drop the oldest shares past the limits, decided ones (that owe nothing) before waiting ones.
    fn prune(&mut self) {
        let bytes = |items: &[Entry]| items.iter().map(|e| e.item.size).sum::<u64>();
        while self.items.len() > MAX_KEPT || (self.items.len() > 1 && bytes(&self.items) > MAX_KEPT_BYTES) {
            let done = self.items.iter().position(|e| e.item.state != ShareState::Offered && !e.owe);
            self.items.remove(done.unwrap_or(0));
        }
    }

    fn save(&self) -> Result<(), RpcError> {
        self.write().map_err(|e| RpcError::internal(format!("saving the inbox: {e}")))
    }

    fn write(&self) -> std::io::Result<()> {
        midna_proto::paths::guard_write(&self.path)?;
        let dir = self.path.parent().expect("inbox path has a parent");
        std::fs::create_dir_all(dir)?;
        let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        let text = serde_json::to_string(&File { items: self.items.clone() })?;
        let tmp = self.path.with_extension(format!("tmp-{}", std::process::id()));
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }
}

fn peer_name(d: &Daemon, fp: &str) -> String {
    let mut i = d.nearby.inner();
    let stored = i.store(&d.cfg.home).get(fp).map(|s| s.name.clone());
    stored.or_else(|| i.peers.get(fp).map(|p| p.name.clone())).unwrap_or_else(|| nb::short_fingerprint(fp))
}

fn emit(d: &Daemon, kind: &str, by: Actor, item: &ShareItem) {
    d.emit(kind, by, None, None, serde_json::to_value(item).unwrap_or_default());
}

/// `share.send`: offer a paired device a share and wait until it is in its inbox.
pub fn send(d: &Daemon, ctx: &Ctx, p: ShareSendParams) -> R {
    let payload = p.payload().map_err(RpcError::bad_params)?;
    let fp = p.peer.trim().to_lowercase();
    let mut ch = link::connect_peer(d, &fp)?;
    let name = peer_name(d, &fp);
    let id = format!("sh_{}", crate::state::hex_id(12));
    let sent = ch.send(&Wire::ShareOffer { id: id.clone(), payload: serde_json::to_value(&payload).unwrap_or_default() }).and_then(|_| ch.expect(transport::HANDSHAKE_TIMEOUT));
    let failed = |why: String| RpcError::refused(format!("{name} didn't take it: {why}"));
    match sent {
        Ok(Wire::ShareAck { id: got }) if got == id => ch.finish(),
        Ok(Wire::Refused { reason }) => return Err(failed(refusal_words(&reason))),
        Ok(other) => return Err(failed(format!("it answered {other:?}"))),
        Err(e) => return Err(failed(e)),
    }
    let item = ShareItem {
        id,
        direction: ShareDirection::Outgoing,
        peer: fp,
        peer_name: name,
        from: identity(d).fingerprint.clone(),
        size: payload.size(),
        payload,
        state: ShareState::Offered,
        sent_at: time::now_rfc3339(),
        decided_at: None,
        result: None,
        needs_you_id: None,
    };
    {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.push(Entry { item: item.clone(), owe: false });
        inbox.save()?;
    }
    emit(d, kinds::SHARE_SENT, ctx.actor(), &item);
    ok(item)
}

/// Why the other device refused, for people.
fn refusal_words(reason: &str) -> String {
    match reason {
        transport::NOT_PAIRED => "it isn't paired with this Mac (any more)".into(),
        "unsupported" => "it runs an older Midna that can't take shares".into(),
        r => r.to_string(),
    }
}

/// A paired device (`from`, proven by the connection) offers a share.
pub fn incoming(d: &Daemon, from: &str, id: String, payload: Value) -> Wire {
    let refuse = |reason: &str| Wire::Refused { reason: reason.into() };
    if !nb::valid_share_id(&id) {
        return refuse("that isn't a share id");
    }
    let payload: SharePayload = match serde_json::from_value(payload) {
        Ok(p) => p,
        Err(_) => return refuse("this Midna can't take that kind of share; update it"),
    };
    if let Err(e) = payload.check() {
        return refuse(&e);
    }
    let name = peer_name(d, from);
    let item = ShareItem {
        id: id.clone(),
        direction: ShareDirection::Incoming,
        peer: from.to_string(),
        peer_name: name,
        from: from.to_string(),
        size: payload.size(),
        payload,
        state: ShareState::Offered,
        sent_at: time::now_rfc3339(),
        decided_at: None,
        result: None,
        needs_you_id: None,
    };
    {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        if let Some(e) = inbox.get(&id) {
            // The same offer again (its ack was lost): it's already here.
            let same = e.item.direction == ShareDirection::Incoming && e.item.peer == from;
            return if same { Wire::ShareAck { id } } else { refuse("that share id is taken") };
        }
        let waiting = inbox.items.iter().filter(|e| e.item.direction == ShareDirection::Incoming && e.item.peer == from && e.item.state == ShareState::Offered).count();
        if waiting >= nb::MAX_PENDING_FROM {
            return refuse("inbox_full: too many of your shares are waiting on this Mac");
        }
        inbox.push(Entry { item: item.clone(), owe: false });
        if let Err(e) = inbox.save() {
            inbox.items.retain(|e| e.item.id != id);
            return refuse(&e.message);
        }
    }
    let tier = d.nearby.inner().store(&d.cfg.home).get(from).map(|s| s.tier).unwrap_or_default();
    let ny = ask(d, &item, tier);
    let item = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        match inbox.get_mut(&id) {
            Some(e) => {
                e.item.needs_you_id = Some(ny);
                let item = e.item.clone();
                let _ = inbox.save();
                item
            }
            None => item,
        }
    };
    emit(d, kinds::SHARE_OFFERED, Actor::system(), &item);
    Wire::ShareAck { id }
}

/// The first `n` characters of `s`, with … when cut.
fn cut(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The needs-you approval that asks this Mac's person about an incoming share. Approving it
/// accepts the share (a link is opened); denying it declines it.
fn ask(d: &Daemon, it: &ShareItem, tier: Tier) -> String {
    let name = &it.peer_name;
    let first_line = |t: &str| cut(t.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim(), 80);
    let (title, body, approve) = match &it.payload {
        SharePayload::Message { text, reply_to } => {
            let answers = if reply_to.is_some() { " (a reply)" } else { "" };
            (
                format!("{name}: {}", first_line(text)),
                format!("{}\n\nMessage from {name}{answers}. Approving marks it read. Reply: midna inbox reply {} <text>", cut(text, 2000), it.id),
                json!({ "id": it.id }),
            )
        }
        SharePayload::Snippet { text, title } => {
            let lines = text.lines().count().max(1);
            let named = title.as_deref().map(|t| format!(": {t}")).unwrap_or_default();
            (
                format!("{name} sent a snippet{named} ({lines} line{})", if lines == 1 { "" } else { "s" }),
                format!(
                    "{}\n\nApproving keeps it in your inbox; nothing runs. To paste it into a terminal (never followed by \
                     Enter): midna inbox accept {} --into <terminal>",
                    cut(text, 2000),
                    it.id
                ),
                json!({ "id": it.id }),
            )
        }
        SharePayload::Link { url, title } => (
            format!("{name} sent a link: {}", cut(title.as_deref().unwrap_or(url), 80)),
            format!("{}\n\nApproving opens it in your browser. To pin it to a terminal's links instead: midna inbox accept {} --pin --into <terminal>", cut(url, 2000), it.id),
            json!({ "id": it.id, "action": "open" }),
        ),
    };
    let who = match tier {
        Tier::MyDevice => "one of your Macs",
        Tier::Contact => "a contact",
    };
    let mut item = d.new_needs_you(NeedsYouKind::Approval, title, Actor::system(), None);
    item.detail = format!("{body}\nFrom {name}, {who} (device {}). Deny to decline it.", nb::short_fingerprint(&it.from));
    item.approval = Some(ApprovalRequest {
        action: PolicyAction { kind: ActionKind::Cli, value: format!("midna inbox accept {}", it.id), session: None, project: None },
        matched_rule: None,
        target_session: None,
    });
    d.core().state.deferred.insert(item.id.clone(), crate::state::Deferred { method: ACCEPT_METHOD.into(), params: approve, guard: None });
    d.raise_needs_you(item).id
}

/// A paired device (`from`) says its person accepted or declined a share this Mac sent.
pub fn result(d: &Daemon, from: &str, id: String, state: &str) -> Wire {
    let state = match state {
        "accepted" => ShareState::Accepted,
        "declined" => ShareState::Declined,
        _ => return Wire::Refused { reason: "unknown share state".into() },
    };
    let changed = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let e = inbox.get_mut(&id).filter(|e| e.item.direction == ShareDirection::Outgoing && e.item.peer == from && e.item.state == ShareState::Offered);
        match e {
            Some(e) => {
                e.item.state = state;
                e.item.decided_at = Some(time::now_rfc3339());
                let item = e.item.clone();
                let _ = inbox.save();
                Some(item)
            }
            None => None,
        }
    };
    if let Some(item) = changed {
        let kind = if state == ShareState::Accepted { kinds::SHARE_ACCEPTED } else { kinds::SHARE_DECLINED };
        emit(d, kind, Actor::system(), &item);
    }
    // Acked even when unknown (forgotten, pruned, said twice), so the other side stops retrying.
    Wire::ShareAck { id }
}

/// An incoming share, or not_found.
fn incoming_item(d: &Daemon, id: &str) -> Result<ShareItem, RpcError> {
    let mut i = d.nearby.inner();
    i.inbox(&d.cfg.home)
        .get(id)
        .filter(|e| e.item.direction == ShareDirection::Incoming)
        .map(|e| e.item.clone())
        .ok_or_else(|| RpcError::not_found(format!("no incoming share {id} (inbox.list)")))
}

/// `share.accept` (human only).
pub fn accept(d: &Daemon, ctx: &Ctx, p: ShareAcceptParams) -> R {
    let it = incoming_item(d, &p.id)?;
    if it.state == ShareState::Declined {
        return Err(RpcError::conflict(format!("{} was declined", p.id)));
    }
    let session = |sid: Option<String>| -> Result<String, RpcError> {
        let sid = sid.ok_or_else(|| RpcError::bad_params("no terminal: pass `session` or run inside a midna terminal"))?;
        if d.core().state.session(&sid).is_none() {
            return Err(RpcError::not_found(format!("no terminal {sid}")));
        }
        Ok(sid)
    };
    if p.action.is_some() && !matches!(it.payload, SharePayload::Link { .. }) {
        return Err(RpcError::bad_params("only a link takes an action (open | pin)"));
    }
    let result = match &it.payload {
        SharePayload::Message { .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("a message isn't pasted; send a snippet for that"));
            }
            "read".to_string()
        }
        SharePayload::Snippet { text, .. } => match p.session {
            Some(sid) => {
                let sid = session(Some(sid))?;
                paste(d, &sid, text)?;
                format!("pasted into {sid}")
            }
            None => it.result.clone().unwrap_or_else(|| "kept in the inbox".into()),
        },
        SharePayload::Link { url, title } => match p.action.unwrap_or(ShareLinkAction::Open) {
            ShareLinkAction::Open => {
                if p.session.is_some() {
                    return Err(RpcError::bad_params("`session` is for pinning (action pin)"));
                }
                open_url(url)?;
                "opened".to_string()
            }
            ShareLinkAction::Pin => {
                let sid = session(p.session.or(ctx.session.clone()))?;
                let add = LinksAddParams {
                    session: None,
                    target: url.clone(),
                    title: title.clone(),
                    note: Some(format!("from {}", it.peer_name)),
                    pin: true,
                };
                crate::links::add(d, &sid, &add, ctx.actor())?;
                format!("pinned to {sid}")
            }
        },
    };
    let (item, first, ny) = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let e = inbox.get_mut(&p.id).ok_or_else(|| RpcError::not_found(format!("no incoming share {}", p.id)))?;
        let first = e.item.state == ShareState::Offered;
        if first {
            e.item.state = ShareState::Accepted;
            e.item.decided_at = Some(time::now_rfc3339());
            e.owe = true;
        } else if e.item.state == ShareState::Declined {
            return Err(RpcError::conflict(format!("{} was declined", p.id)));
        }
        e.item.result = Some(result);
        let ny = e.item.needs_you_id.take();
        let item = e.item.clone();
        inbox.save()?;
        (item, first, ny)
    };
    if let Some(ny) = ny {
        d.close_needs_you(&ny, json!({ "kind": "approve", "scope": { "kind": "once" } }), ctx.actor());
    }
    if first {
        emit(d, kinds::SHARE_ACCEPTED, ctx.actor(), &item);
        nudge(d);
    }
    ok(item)
}

/// `share.decline` (human only).
pub fn decline(d: &Daemon, ctx: &Ctx, p: ShareDeclineParams) -> R {
    let it = incoming_item(d, &p.id)?;
    match it.state {
        ShareState::Accepted => Err(RpcError::conflict(format!("{} was already accepted", p.id))),
        ShareState::Declined => ok(it),
        ShareState::Offered => ok(decline_offered(d, &p.id, ctx.actor()).unwrap_or(it)),
    }
}

/// Decline a share that is still offered; None if it isn't.
fn decline_offered(d: &Daemon, id: &str, by: Actor) -> Option<ShareItem> {
    let (item, ny) = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let e = inbox.get_mut(id).filter(|e| e.item.direction == ShareDirection::Incoming && e.item.state == ShareState::Offered)?;
        e.item.state = ShareState::Declined;
        e.item.decided_at = Some(time::now_rfc3339());
        e.owe = true;
        let ny = e.item.needs_you_id.take();
        let item = e.item.clone();
        let _ = inbox.save();
        (item, ny)
    };
    if let Some(ny) = ny {
        d.close_needs_you(&ny, json!({ "kind": "deny" }), by.clone());
    }
    emit(d, kinds::SHARE_DECLINED, by, &item);
    nudge(d);
    Some(item)
}

/// A needs-you item closed. Denying a share's approval declines the share; dismissing it (or
/// it being withdrawn) leaves the share waiting in the inbox.
pub fn needs_you_closed(d: &Daemon, item: &NeedsYou, resolution: &Value) {
    if item.kind != NeedsYouKind::Approval {
        return;
    }
    let id = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let Some(e) = inbox.items.iter_mut().find(|e| e.item.needs_you_id.as_deref() == Some(item.id.as_str())) else { return };
        e.item.needs_you_id = None;
        let id = e.item.id.clone();
        let _ = inbox.save();
        id
    };
    if resolution.get("kind").and_then(Value::as_str) == Some("deny") {
        decline_offered(d, &id, Actor::human());
    }
}

/// A snippet as it may be pasted: control characters (escape sequences, ^C, an end-of-paste
/// marker's ESC, C1 codes) dropped, keeping tabs and line breaks, with no trailing line break.
fn pasteable(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r')).collect();
    kept.trim_end_matches(['\n', '\r']).to_string()
}

/// Paste a snippet into a terminal: never followed by Enter, and several lines only into a
/// terminal that turned on bracketed paste (otherwise each line would run).
fn paste(d: &Daemon, sid: &str, text: &str) -> Result<(), RpcError> {
    let rt = d.rt(sid).ok_or_else(|| RpcError::not_found(format!("terminal {sid} isn't running")))?;
    let text = pasteable(text);
    let lines = text.contains(['\n', '\r']);
    let out = rt
        .with(move |e| (!lines || e.bracketed_paste()).then(|| e.paste(&text)))
        .ok_or_else(|| RpcError::conflict(format!("terminal {sid} isn't responding")))?
        .ok_or_else(|| {
            RpcError::refused(format!(
                "{sid} isn't ready for a paste of several lines (bracketed paste is off), so each line would run; \
                 paste it at a shell or editor prompt, or copy it from the inbox"
            ))
        })?;
    if !out.is_empty() && !rt.write(&out) {
        return Err(RpcError::conflict(format!("terminal {sid} is not accepting input (process gone?)")));
    }
    Ok(())
}

fn open_url(url: &str) -> Result<(), RpcError> {
    std::process::Command::new("/usr/bin/open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| RpcError::internal(format!("opening the link: {e}")))
}

/// `inbox.list`.
pub fn list(d: &Daemon, p: InboxListParams) -> R {
    let mut i = d.nearby.inner();
    let inbox = i.inbox(&d.cfg.home);
    let waiting = inbox.items.iter().filter(|e| e.item.direction == ShareDirection::Incoming && e.item.state == ShareState::Offered).count();
    let peer = p.peer.map(|f| f.trim().to_lowercase());
    let items = inbox
        .items
        .iter()
        .rev()
        .map(|e| &e.item)
        .filter(|it| p.direction.is_none_or(|x| it.direction == x) && p.state.is_none_or(|x| it.state == x) && peer.as_ref().is_none_or(|f| &it.peer == f))
        .take(p.limit.unwrap_or(LIST_DEFAULT))
        .cloned()
        .collect();
    ok(InboxListResult { items, waiting })
}

/// Wake the delivery thread: a result is owed.
fn nudge(d: &Daemon) {
    let (lock, cv) = &*d.nearby.wake;
    *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
    cv.notify_all();
}

/// Deliver owed results to the devices that can be seen now, a few seconds apart.
pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let wake = d.nearby.wake.clone();
    let _ = std::thread::Builder::new().name("nearby-share".into()).spawn(move || {
        loop {
            {
                let Some(d) = w.upgrade() else { return };
                if d.shutting_down.load(Ordering::Relaxed) {
                    return;
                }
                deliver(&d);
            }
            let (lock, cv) = &*wake;
            let mut woken = lock.lock().unwrap_or_else(|e| e.into_inner());
            if !*woken {
                woken = cv.wait_timeout(woken, RETRY).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
            }
            *woken = false;
        }
    });
}

fn deliver(d: &Daemon) {
    let owed: Vec<(String, String, ShareState)> = {
        let mut i = d.nearby.inner();
        let seen: Vec<String> = i.peers.keys().cloned().collect();
        let inbox = i.inbox(&d.cfg.home);
        inbox.items.iter().filter(|e| e.owe && seen.contains(&e.item.peer)).map(|e| (e.item.id.clone(), e.item.peer.clone(), e.item.state)).collect()
    };
    for (id, fp, state) in owed {
        let state_word = if state == ShareState::Accepted { "accepted" } else { "declined" };
        let Ok(mut ch) = link::connect_peer(d, &fp) else { continue };
        let answer = ch.send(&Wire::ShareResult { id: id.clone(), state: state_word.into() }).and_then(|_| ch.expect(transport::HANDSHAKE_TIMEOUT));
        if !matches!(&answer, Ok(Wire::ShareAck { id: got }) if *got == id) {
            continue;
        }
        ch.finish();
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        if let Some(e) = inbox.get_mut(&id) {
            e.owe = false;
            let _ = inbox.save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_carries_no_control_characters_or_final_enter() {
        assert_eq!(pasteable("echo hi\n"), "echo hi");
        assert_eq!(pasteable("a\x1b[201~\x03b\u{9b}c\x7f\r\n"), "a[201~bc");
        assert_eq!(pasteable("one\n\ttwo\r\n\n"), "one\n\ttwo");
        assert_eq!(pasteable("x\x04\r"), "x");
    }

    fn entry(id: &str, state: ShareState, size: u64) -> Entry {
        Entry {
            item: ShareItem {
                id: id.into(),
                direction: ShareDirection::Incoming,
                peer: "p".into(),
                peer_name: "P".into(),
                from: "p".into(),
                size,
                payload: SharePayload::Message { text: "x".into(), reply_to: None },
                state,
                sent_at: time::now_rfc3339(),
                decided_at: None,
                result: None,
                needs_you_id: None,
            },
            owe: false,
        }
    }

    #[test]
    fn prune_drops_the_oldest_decided_first() {
        let mut inbox = Inbox { path: PathBuf::from("/nonexistent"), items: vec![] };
        inbox.push(entry("sh_wait", ShareState::Offered, 1));
        for n in 0..MAX_KEPT {
            inbox.push(entry(&format!("sh_{n:04}"), ShareState::Accepted, 1));
        }
        assert_eq!(inbox.items.len(), MAX_KEPT);
        assert!(inbox.get("sh_wait").is_some(), "a waiting share outlives decided ones");
        assert!(inbox.get("sh_0000").is_none());
        inbox.push(entry("sh_big", ShareState::Accepted, MAX_KEPT_BYTES));
        assert!(inbox.items.iter().map(|e| e.item.size).sum::<u64>() <= MAX_KEPT_BYTES || inbox.items.len() == 1);
    }
}
