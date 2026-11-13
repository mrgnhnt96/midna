//! Sharing between paired devices (`share.send|accept|decline`, `inbox.list`).
//!
//! The sender offers a share over an authenticated connection ([`Wire::ShareOffer`]); the
//! receiver checks it, keeps it in its inbox (`nearby/inbox.json`, mode 0600, both directions,
//! the latest [`MAX_KEPT`]) and answers [`Wire::ShareAck`]. Who sent it is the connection's
//! device key, never anything the message says. An incoming share is data only: it raises a
//! needs-you approval and waits. Only the human's `share.accept` (or approving that item) acts
//! on it: a message is marked read, a snippet is pasted into the terminal they pick (never
//! followed by Enter), a link is opened or pinned, a tab opens as a new shell (`tab.rs`), an
//! agent start opens a new agent terminal with its prompt (`agent_start.rs`), an agent ask
//! queues its prompt in the agent terminal it names (`agent_ask.rs`). Accepting or
//! declining owes the sender a [`Wire::ShareResult`], delivered at once if it can be seen and
//! retried while it can't. Accepting a file share starts fetching its files ([`super::transfer`]).
use super::transport::{self, Wire};
use super::{agent_ask, agent_start, identity, link, notes, objects, session, tab, transfer};
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{
    self as nb, InboxListParams, InboxListResult, ShareAcceptParams, ShareDeclineParams, ShareDirection, ShareItem, ShareKind,
    ShareLinkAction, SharePayload, ShareSendParams, ShareState, Tier, TransferState,
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
pub const DECLINE_METHOD: &str = "share.decline";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Entry {
    pub(super) item: ShareItem,
    /// Incoming and decided: the sender hasn't been told yet.
    #[serde(default)]
    pub(super) owe: bool,
    /// An outgoing file share: the file or folder on this Mac it is served from. An outgoing
    /// agent session: its transcript snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) source: Option<PathBuf>,
    /// An incoming agent session accepted into a folder its person chose (`--cwd`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) open_in: Option<String>,
    /// An incoming agent start or ask accepted with `want_reply`: the agent terminal whose turn
    /// answers it, until it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) watch: Option<String>,
    /// An incoming agent ask: its prompt's message in that terminal's queue, until it is typed.
    /// A turn that ends before then isn't its answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) queued: Option<String>,
    /// An incoming agent start or ask: its answer, owed to the sender until it took it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) reply: Option<Reply>,
    /// An outgoing agent start or ask: the incoming message that answered it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) answered_by: Option<String>,
}

impl Entry {
    pub(super) fn new(item: ShareItem, source: Option<PathBuf>) -> Entry {
        Entry { item, owe: false, source, open_in: None, watch: None, queued: None, reply: None, answered_by: None }
    }
}

/// An agent's answer owed to the device that asked: offered as a message under share id `id`,
/// the same on every try, so a lost ack doesn't deliver it twice.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Reply {
    pub(super) id: String,
    pub(super) text: String,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    items: Vec<Entry>,
}

/// `nearby/inbox.json`, oldest first.
pub struct Inbox {
    path: PathBuf,
    pub(super) items: Vec<Entry>,
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

    pub(super) fn get(&self, id: &str) -> Option<&Entry> {
        self.items.iter().find(|e| e.item.id == id)
    }

    pub(super) fn get_mut(&mut self, id: &str) -> Option<&mut Entry> {
        self.items.iter_mut().find(|e| e.item.id == id)
    }

    fn push(&mut self, e: Entry) {
        self.items.push(e);
        self.prune();
    }

    /// Drop the oldest shares past the limits, decided ones (that owe nothing) before waiting
    /// ones. A file share whose files are still moving is never dropped.
    fn prune(&mut self) {
        let bytes = |items: &[Entry]| items.iter().map(|e| kept_bytes(&e.item)).sum::<u64>();
        while self.items.len() > MAX_KEPT || (self.items.len() > 1 && bytes(&self.items) > MAX_KEPT_BYTES) {
            let moving = |e: &Entry| e.item.transfer.as_ref().is_some_and(|t| !t.is_over());
            let owes = |e: &Entry| e.owe || e.watch.is_some() || e.reply.is_some();
            let done = self.items.iter().position(|e| e.item.state != ShareState::Offered && !owes(e) && !moving(e));
            let Some(at) = done.or_else(|| self.items.iter().position(|e| !moving(e))) else { return };
            let gone = self.items.remove(at);
            // An agent session's transcript snapshot, or an agent start's notes, go with it.
            if gone.item.direction == ShareDirection::Outgoing
                && matches!(gone.item.payload, SharePayload::Agent { .. } | SharePayload::AgentStart { .. })
                && let Some(home) = self.path.parent().and_then(Path::parent)
            {
                super::session::forget(home, &gone.item.id);
            }
        }
    }

    pub(super) fn save(&self) -> Result<(), RpcError> {
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

/// What a share takes up in the inbox: a file share keeps only its list of files.
fn kept_bytes(it: &ShareItem) -> u64 {
    match &it.payload {
        SharePayload::File { files, dirs, .. } => files.iter().map(|f| 80 + f.path.len() as u64).chain(dirs.iter().map(|d| d.len() as u64)).sum(),
        // The transcript isn't kept here, only what names it.
        SharePayload::Agent { .. } => 1024,
        // Nor are an agent start's notes, only their first lines.
        SharePayload::AgentStart { prompt, notes: Some(n), .. } => (prompt.len() + n.head.len()) as u64 + 1024,
        _ => it.size,
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
    let id = format!("sh_{}", crate::state::hex_id(12));
    let (payload, source) = match p.kind {
        ShareKind::Object => {
            if p.text.is_some() || p.url.is_some() || p.title.is_some() || p.reply_to.is_some() || p.path.is_some() || p.session.is_some() {
                return Err(RpcError::bad_params("an object share takes only `object` and `ids`"));
            }
            let kind = p.object.ok_or_else(|| RpcError::bad_params("an object share needs `object`"))?;
            (SharePayload::Object { object: objects::build(d, kind, &p.ids)? }, None)
        }
        ShareKind::File => {
            if p.text.is_some() || p.url.is_some() || p.title.is_some() || p.reply_to.is_some() {
                return Err(RpcError::bad_params("a file takes only `path`"));
            }
            let path = p.path.as_deref().ok_or_else(|| RpcError::bad_params("a file needs `path`"))?;
            let (payload, source) = transfer::prepare(path)?;
            (payload, Some(source))
        }
        ShareKind::Tab => {
            if p.object.is_some() || !p.ids.is_empty() {
                return Err(RpcError::bad_params("only an object share takes `object` and `ids`"));
            }
            (tab::snapshot(d, ctx, &p)?, None)
        }
        ShareKind::Agent => {
            if p.object.is_some() || !p.ids.is_empty() {
                return Err(RpcError::bad_params("only an object share takes `object` and `ids`"));
            }
            let (payload, copy) = session::snapshot(d, ctx, &p, &id)?;
            (payload, Some(copy))
        }
        ShareKind::AgentStart => agent_start::build(d, ctx, &p, &id)?,
        _ => (p.payload().map_err(RpcError::bad_params)?, None),
    };
    let fp = p.peer.trim().to_lowercase();
    // An agent session's transcript snapshot, or an agent start's notes, go if the offer does.
    let dropped = |e: RpcError| {
        session::forget(&d.cfg.home, &id);
        e
    };
    if payload.project_name().is_some() {
        let tier = d.nearby.inner().store(&d.cfg.home).get(&fp).map(|s| (s.tier, s.name.clone()));
        if let Some((Tier::Contact, name)) = tier {
            return Err(dropped(RpcError::refused(format!("{name} is kept as a contact: only your own devices open things in a project by name"))));
        }
    }
    notes::may_send(d, &fp, p.notes.is_some()).map_err(dropped)?;
    super::guard::send(d, ctx, &payload, &fp).map_err(dropped)?;
    ok(offer(d, &fp, id.clone(), payload, source, ctx.actor(), true).map_err(dropped)?)
}

/// Offer device `fp` a share that may go (the guard let it, or it answers something it asked)
/// and wait until it is in its inbox; `source` is what serves it from this Mac. `patient` (a
/// person waits on it) tries again while macOS asks about Local Network access.
pub(super) fn offer(d: &Daemon, fp: &str, id: String, payload: SharePayload, source: Option<PathBuf>, by: Actor, patient: bool) -> Result<ShareItem, RpcError> {
    let fp = fp.to_string();
    let name = peer_name(d, &fp);
    let named = payload.project_name().is_some();
    let mut ch = if patient { link::connect_peer_waiting(d, &fp)? } else { link::connect_peer(d, &fp)? };
    let offer = Wire::ShareOffer { id: id.clone(), payload: serde_json::to_value(&payload).unwrap_or_default() };
    let item = ShareItem {
        id: id.clone(),
        direction: ShareDirection::Outgoing,
        peer: fp,
        peer_name: name.clone(),
        from: identity(d).fingerprint.clone(),
        size: payload.size(),
        payload,
        state: ShareState::Offered,
        sent_at: time::now_rfc3339(),
        decided_at: None,
        result: None,
        needs_you_id: None,
        transfer: None,
        auto: false,
    };
    // Kept before it is offered: a device that auto-accepts answers (and fetches a file) at once.
    {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.push(Entry::new(item.clone(), source));
        inbox.save()?;
    }
    let sent = ch.send(&offer).and_then(|_| ch.expect(transport::HANDSHAKE_TIMEOUT));
    let failed = |why: String| {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.items.retain(|e| e.item.id != id);
        let _ = inbox.save();
        RpcError::refused(format!("{name} didn't take it: {why}"))
    };
    match sent {
        Ok(Wire::ShareAck { id: got }) if got == id => ch.finish(),
        // An older Midna doesn't know `project_name`, and refuses the empty directory sent with it.
        Ok(Wire::Refused { reason }) if named && reason.ends_with("directory must be an absolute path") => {
            return Err(failed("it runs an older Midna that can't open things in a project by name; update it".into()));
        }
        Ok(Wire::Refused { reason }) => return Err(failed(refusal_words(&reason))),
        Ok(other) => return Err(failed(format!("it answered {other:?}"))),
        Err(e) => return Err(failed(e)),
    }
    let item = d.nearby.inner().inbox(&d.cfg.home).get(&id).map(|e| e.item.clone()).unwrap_or(item);
    emit(d, kinds::SHARE_SENT, by, &item);
    Ok(item)
}

/// Why the other device refused, for people.
fn refusal_words(reason: &str) -> String {
    match reason {
        transport::NOT_PAIRED => "it isn't paired with this Mac (any more)".into(),
        "unsupported" => "it runs an older Midna that can't take shares".into(),
        r => r.to_string(),
    }
}

/// Shares like `payload` from `from` are accepted without asking: it is one of my devices, its
/// person turned that kind on, and (for Midna's own things) it is a summary or a theme that
/// changes nothing only the human may change. Rules, triggers, settings, queued prompts and
/// agent sessions always wait for review; an agent start may be auto-accepted (it runs
/// supervised, like an agent's own `session.open` with a prompt), and so may an agent ask (its
/// prompt is queued, like an agent's own `queue.add`).
fn auto_accepts(d: &Daemon, from: &str, payload: &SharePayload) -> bool {
    let on = d.nearby.inner().store(&d.cfg.home).get(from).is_some_and(|s| s.auto_accepts(payload.kind()));
    on && match payload {
        SharePayload::Object { object } => {
            matches!(object, nb::MidnaObject::Summary { .. } | nb::MidnaObject::Theme { .. }) && !objects::needs_human(d, object)
        }
        SharePayload::Agent { .. } => false,
        _ => true,
    }
}

/// A paired device (`from`, proven by the connection) offers a share.
pub fn incoming(d: &Arc<Daemon>, from: &str, id: String, payload: Value) -> Wire {
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
    if let Err(e) = agent_ask::admit(d, from, &payload) {
        return refuse(&e);
    }
    if let Err(e) = tab::admit(d, from, &payload) {
        return refuse(&e);
    }
    if let Err(e) = notes::admit(d, from, &payload) {
        return refuse(&e);
    }
    let name = peer_name(d, from);
    let mut item = ShareItem {
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
        transfer: None,
        auto: false,
    };
    {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        if let Some(e) = inbox.get(&id) {
            // The same offer again (its ack was lost): it's already here.
            let same = e.item.direction == ShareDirection::Incoming && e.item.peer == from;
            return if same { Wire::ShareAck { id } } else { refuse("that share id is taken") };
        }
        let mut e = Entry::new(item.clone(), None);
        let answer = agent_start::answered(inbox, from, &mut e);
        let waiting = inbox.items.iter().filter(|e| e.item.direction == ShareDirection::Incoming && e.item.peer == from && e.item.state == ShareState::Offered).count();
        if !answer && waiting >= nb::MAX_PENDING_FROM {
            return refuse("inbox_full: too many of your shares are waiting on this Mac");
        }
        item = e.item.clone();
        inbox.push(e);
        if let Err(e) = inbox.save() {
            inbox.items.retain(|e| e.item.id != id);
            inbox.items.iter_mut().filter(|e| e.answered_by.as_deref() == Some(id.as_str())).for_each(|e| e.answered_by = None);
            return refuse(&e.message);
        }
        if answer {
            // The answer to an agent start this Mac sent: read at once, nothing to approve.
            drop(i);
            emit(d, kinds::SHARE_OFFERED, Actor::system(), &item);
            emit(d, kinds::SHARE_ANSWERED, Actor::system(), &item);
            nudge(d);
            return Wire::ShareAck { id };
        }
    }
    if auto_accepts(d, from, &item.payload) {
        // Accepted once the sender has its ack, so it hears the answer (and serves a file) then.
        emit(d, kinds::SHARE_OFFERED, Actor::system(), &item);
        let (d, share) = (d.clone(), id.clone());
        let _ = std::thread::Builder::new().name("nearby-auto-accept".into()).spawn(move || auto_accept(&d, &share));
        return Wire::ShareAck { id };
    }
    let on = d.nearby.inner().store(&d.cfg.home).get(from).is_some_and(|s| s.auto_accepts(item.payload.kind()));
    let note = on.then_some("Rules, triggers, settings and queued prompts always ask, even from a device set to auto-accept.");
    let item = ask_about(d, &id, note).unwrap_or(item);
    emit(d, kinds::SHARE_OFFERED, Actor::system(), &item);
    Wire::ShareAck { id }
}

/// Raise the approval for incoming share `id` if it still waits and nothing asks yet (with
/// `note` saying why it wasn't auto-accepted). Returns the share as it is now.
fn ask_about(d: &Daemon, id: &str, note: Option<&str>) -> Option<ShareItem> {
    let (it, tier) = {
        let mut i = d.nearby.inner();
        let it = i.inbox(&d.cfg.home).get(id).map(|e| e.item.clone())?;
        let tier = i.store(&d.cfg.home).get(&it.from).map(|s| s.tier).unwrap_or_default();
        (it, tier)
    };
    if it.state != ShareState::Offered || it.needs_you_id.is_some() {
        return Some(it);
    }
    let ny = ask(d, &it, tier, note);
    let mut i = d.nearby.inner();
    let inbox = i.inbox(&d.cfg.home);
    let e = inbox.get_mut(id)?;
    e.item.needs_you_id = Some(ny);
    let it = e.item.clone();
    let _ = inbox.save();
    Some(it)
}

/// Accept incoming share `id` without asking, at an agent's level: whatever only the human may
/// accept is refused there too. If it can't be, it waits for its person like any other.
fn auto_accept(d: &Arc<Daemon>, id: &str) {
    let Ok(it) = incoming_item(d, id) else { return };
    if it.state != ShareState::Offered || it.needs_you_id.is_some() {
        return;
    }
    if !auto_accepts(d, &it.from, &it.payload) {
        ask_about(d, id, None);
        return;
    }
    let ctx = Ctx::internal_trigger(Actor::system());
    let p = ShareAcceptParams { id: id.to_string(), session: None, action: None, cwd: None };
    if let Err(e) = accept_as(d, &ctx, p, true) {
        eprintln!("midnad: nearby: auto-accepting {id} failed ({}); asking instead", e.message);
        let note = format!("It is set to be accepted automatically, but that failed: {}", e.message);
        if let Some(it) = ask_about(d, id, Some(&note)).filter(|it| it.state == ShareState::Offered) {
            emit(d, kinds::SHARE_OFFERED, Actor::system(), &it);
        }
    }
}

/// The first `n` characters of `s`, with … when cut.
pub(super) fn cut(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// The needs-you approval that asks this Mac's person about an incoming share. Approving it
/// accepts the share (a link is opened); denying it declines it.
fn ask(d: &Daemon, it: &ShareItem, tier: Tier, note: Option<&str>) -> String {
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
        SharePayload::File { name, folder, files, .. } => {
            let what = if *folder { "folder" } else { "file" };
            let count = if *folder { format!(", {} file{}", files.len(), if files.len() == 1 { "" } else { "s" }) } else { String::new() };
            let to = transfer::downloads(d).map(|p| p.display().to_string()).unwrap_or_else(|_| "your downloads folder".into());
            (
                format!("{name} sent a {what}: {} ({}{count})", cut(&nb::safe_name(name), 80), nb::size_label(it.size)),
                format!(
                    "{}{count}, {}.\n\nApproving saves it into {to}, marked as downloaded; nothing is opened or run.",
                    nb::safe_name(name),
                    nb::size_label(it.size)
                ),
                json!({ "id": it.id }),
            )
        }
        SharePayload::Tab { title, scrollback, project_name: Some(_), .. } if scrollback.is_empty() => (
            format!("{name} opens a new tab here: {}", cut(title, 80)),
            format!(
                "{}\n\n{} Approving opens a new shell there; nothing runs in it. To open it in another folder: midna inbox accept {} --cwd <dir>",
                tab::describe(&it.payload, 2000),
                tab::place(d, &it.payload).words(&it.payload),
                it.id
            ),
            json!({ "id": it.id }),
        ),
        SharePayload::Tab { title, .. } => (
            format!("{name} sent a terminal tab: {}", cut(title, 80)),
            format!(
                "{}\n\n{} Approving opens a new tab with their scrollback shown above a fresh shell; nothing they ran \
                 runs here, and no process or environment comes with it. To open it in another folder: midna inbox accept {} --cwd <dir>",
                tab::describe(&it.payload, 2000),
                tab::place(d, &it.payload).words(&it.payload),
                it.id
            ),
            json!({ "id": it.id }),
        ),
        SharePayload::Object { object } => {
            let then = match object {
                nb::MidnaObject::Queue { .. } => format!(
                    "Approving keeps them in your inbox. To queue them in a terminal (each waits until its agent is idle): \
                     midna inbox accept {} --into <terminal>",
                    it.id
                ),
                _ => "Approving makes the changes marked +; nothing else changes.".into(),
            };
            (format!("{name} sent from Midna: {}", cut(&object.label(), 80)), format!("{}\n\n{then}", objects::review(d, object)), json!({ "id": it.id }))
        }
        SharePayload::Agent { title, agent, .. } => (
            format!("{name} sent an agent session: {}", cut(title, 80)),
            format!(
                "{}\n\n{} Approving fetches the transcript, puts it where {} keeps its conversations, and opens a tab \
                 resuming it, idle until you prompt it; nothing runs before. To open it in another folder: midna inbox accept {} --cwd <dir>",
                session::describe(&it.payload),
                tab::place(d, &it.payload).words(&it.payload),
                session::agent_name(*agent),
                it.id
            ),
            json!({ "id": it.id }),
        ),
        SharePayload::AgentStart { agent, want_reply, .. } => {
            let agent = session::agent_name(*agent);
            let back = if *want_reply { format!(", and sends {name} its answer when its first turn ends") } else { String::new() };
            let SharePayload::AgentStart { prompt, .. } = &it.payload else { unreachable!() };
            let (notes, fetch) = match notes::words(&it.payload) {
                Some(w) => (format!("\n\n{w}"), format!("fetches the notes into {}, then ", notes::saved_path(&d.cfg.home, &it.id).display())),
                None => (String::new(), String::new()),
            };
            let pointed = if fetch.is_empty() { "" } else { " (pointing it at the notes)" };
            (
                format!("{name} asks {agent} here: {}", first_line(prompt)),
                format!(
                    "{}{notes}\n\n{} Approving {fetch}opens a new {agent} terminal there and gives it this prompt{pointed}{back}; \
                     nothing runs before. To open it in another folder: midna inbox accept {} --cwd <dir>",
                    cut(prompt, 2000),
                    tab::place(d, &it.payload).words(&it.payload),
                    it.id
                ),
                json!({ "id": it.id }),
            )
        }
        SharePayload::AgentAsk { prompt, .. } => {
            let (tab, then) = agent_ask::approval(d, it);
            (format!("{name} asks {tab} here: {}", first_line(prompt)), format!("{}\n\n{then}", cut(prompt, 2000)), json!({ "id": it.id }))
        }
    };
    let who = match tier {
        Tier::MyDevice => "one of your Macs",
        Tier::Contact => "a contact",
    };
    let mut item = d.new_needs_you(NeedsYouKind::Approval, title, Actor::system(), None);
    item.detail = format!("{body}\nFrom {name}, {who} (device {}). Deny to decline it.", nb::short_fingerprint(&it.from));
    if let Some(note) = note {
        item.detail.push_str(&format!("\n{note}"));
    }
    item.approval = Some(ApprovalRequest {
        action: PolicyAction { kind: ActionKind::Cli, value: format!("midna inbox accept {}", it.id), session: None, project: None },
        matched_rule: None,
        target_session: None,
    });
    d.core().state.deferred.insert(item.id.clone(), crate::state::Deferred { method: ACCEPT_METHOD.into(), params: approve, guard: None });
    d.raise_needs_you(item).id
}

/// A paired device (`from`) says its person accepted or declined a share this Mac sent;
/// `reason` is why it was declined, when it says.
pub fn result(d: &Daemon, from: &str, id: String, state: &str, reason: Option<String>) -> Wire {
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
                if state == ShareState::Declined
                    && let Some(why) = reason.as_deref().map(str::trim).filter(|r| !r.is_empty())
                {
                    e.item.result = Some(cut(why, 500));
                }
                let item = e.item.clone();
                let _ = inbox.save();
                Some(item)
            }
            None => None,
        }
    };
    if state == ShareState::Declined && changed.is_some() {
        session::forget(&d.cfg.home, &id);
    }
    if let Some(item) = changed {
        let kind = if state == ShareState::Accepted { kinds::SHARE_ACCEPTED } else { kinds::SHARE_DECLINED };
        emit(d, kind, Actor::system(), &item);
    }
    // Acked even when unknown (forgotten, pruned, said twice), so the other side stops retrying.
    Wire::ShareAck { id }
}

/// The needs-you approval incoming share `id` waits on, if it still waits.
pub fn waiting_on(d: &Daemon, id: &str) -> Option<String> {
    let mut i = d.nearby.inner();
    let e = i.inbox(&d.cfg.home).get(id)?;
    (e.item.direction == ShareDirection::Incoming && e.item.state == ShareState::Offered).then(|| e.item.needs_you_id.clone())?
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
pub fn accept(d: &Arc<Daemon>, ctx: &Ctx, p: ShareAcceptParams) -> R {
    accept_as(d, ctx, p, false)
}

/// Accept an incoming share; `auto` when no one was asked (it came from one of my devices set
/// to auto-accept its kind).
fn accept_as(d: &Arc<Daemon>, ctx: &Ctx, p: ShareAcceptParams, auto: bool) -> R {
    let it = incoming_item(d, &p.id)?;
    if auto && (it.state != ShareState::Offered || it.needs_you_id.is_some()) {
        return Err(RpcError::conflict(format!("{} was decided or waits for its person", p.id)));
    }
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
    let file = matches!(it.payload, SharePayload::File { .. });
    let agent = matches!(it.payload, SharePayload::Agent { .. });
    // An agent start with a notes file starts once they are here.
    let with_notes = matches!(it.payload, SharePayload::AgentStart { notes: Some(_), .. });
    if p.cwd.is_some() && !matches!(it.payload, SharePayload::Tab { .. } | SharePayload::Agent { .. } | SharePayload::AgentStart { .. }) {
        return Err(RpcError::bad_params("only a tab, an agent session or an agent start takes a `cwd`"));
    }
    // An agent start or ask that wants its answer back: the terminal that will give it.
    let mut watch = None;
    // An agent ask: its prompt in that terminal's queue, and that terminal.
    let mut queued = None;
    let mut asked_in = None;
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
        SharePayload::File { .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("a file isn't pasted; it is saved into nearby.downloads"));
            }
            transfer::accepting(d, &it)?
        }
        SharePayload::Tab { .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("a tab opens as a new terminal; `cwd` picks its folder"));
            }
            tab::open(d, ctx, &it.peer_name, &it.payload, p.cwd.as_deref())?
        }
        SharePayload::Object { object } => {
            if p.cwd.is_some() {
                return Err(RpcError::bad_params("`cwd` is for a tab"));
            }
            let sid = match (object, p.session) {
                (nb::MidnaObject::Queue { .. }, Some(sid)) => Some(session(Some(sid))?),
                (_, sid) => sid,
            };
            objects::apply(d, ctx, object, sid.as_deref(), &it.peer_name)?
        }
        SharePayload::Agent { .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("an agent session opens as a new terminal; `cwd` picks its folder"));
            }
            session::accepting(d, &it, p.cwd.as_deref())?
        }
        SharePayload::AgentStart { want_reply, .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("an agent start opens as a new terminal; `cwd` picks its folder"));
            }
            if with_notes {
                notes::accepting(d, &it, p.cwd.as_deref())?
            } else {
                let (result, sid) = agent_start::open(d, ctx, &it, p.cwd.as_deref())?;
                watch = want_reply.then_some(sid);
                result
            }
        }
        SharePayload::AgentAsk { want_reply, .. } => {
            if p.session.is_some() {
                return Err(RpcError::bad_params("an agent ask goes to the terminal it names"));
            }
            match agent_ask::queue(d, ctx, &it)? {
                agent_ask::Queued::In { result, sid, id } => {
                    watch = want_reply.then(|| sid.clone());
                    queued = Some(id);
                    asked_in = Some(sid);
                    result
                }
                agent_ask::Queued::Gone(why) => {
                    // Nothing to ask any more: the sender hears why.
                    let item = decline_offered(d, &p.id, ctx.actor(), Some(&why));
                    return ok(item.unwrap_or(it));
                }
            }
        }
    };
    let (item, first, ny) = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let e = inbox.get_mut(&p.id).ok_or_else(|| RpcError::not_found(format!("no incoming share {}", p.id)))?;
        let first = e.item.state == ShareState::Offered;
        if first {
            if auto && e.item.needs_you_id.is_some() {
                return Err(RpcError::conflict(format!("{} waits for its person now", p.id)));
            }
            e.item.state = ShareState::Accepted;
            e.item.decided_at = Some(time::now_rfc3339());
            e.item.auto = auto;
            e.owe = true;
            e.watch = watch;
            e.queued = queued.clone();
        } else if auto {
            return Err(RpcError::conflict(format!("{} was already decided", p.id)));
        } else if e.item.state == ShareState::Declined {
            return Err(RpcError::conflict(format!("{} was declined", p.id)));
        }
        if (agent || with_notes) && e.item.transfer.as_ref().is_none_or(|t| t.state == TransferState::Failed) {
            e.open_in = p.cwd.clone();
        }
        e.item.result = Some(result);
        let ny = e.item.needs_you_id.take();
        let item = e.item.clone();
        inbox.save()?;
        (item, first, ny)
    };
    if let (Some(sid), Some(id)) = (&asked_in, &queued) {
        // The queue may have typed it before the entry above said it waits there.
        agent_ask::recorded(d, sid, id);
    }
    if let Some(ny) = ny {
        d.close_needs_you(&ny, json!({ "kind": "approve", "scope": { "kind": "once" } }), ctx.actor());
    }
    if first {
        emit(d, kinds::SHARE_ACCEPTED, ctx.actor(), &item);
        nudge(d);
    }
    if file || agent || with_notes {
        transfer::begin(d, &item.id);
        return ok(incoming_item(d, &item.id).unwrap_or(item));
    }
    ok(item)
}

/// `share.decline` (human only).
pub fn decline(d: &Daemon, ctx: &Ctx, p: ShareDeclineParams) -> R {
    let it = incoming_item(d, &p.id)?;
    match it.state {
        ShareState::Accepted => Err(RpcError::conflict(format!("{} was already accepted", p.id))),
        ShareState::Declined => ok(it),
        ShareState::Offered => ok(decline_offered(d, &p.id, ctx.actor(), None).unwrap_or(it)),
    }
}

/// Decline a share that is still offered, with `reason` (kept as its result and sent back)
/// when there is one; None if it isn't offered.
fn decline_offered(d: &Daemon, id: &str, by: Actor, reason: Option<&str>) -> Option<ShareItem> {
    let (item, ny) = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let e = inbox.get_mut(id).filter(|e| e.item.direction == ShareDirection::Incoming && e.item.state == ShareState::Offered)?;
        e.item.state = ShareState::Declined;
        e.item.decided_at = Some(time::now_rfc3339());
        if let Some(why) = reason {
            e.item.result = Some(why.to_string());
        }
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
        decline_offered(d, &id, Actor::human(), None);
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

/// Wake the delivery thread: a result (or an agent's answer) is owed.
pub(super) fn nudge(d: &Daemon) {
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
    let seen: Vec<String> = d.nearby.inner().peers.keys().cloned().collect();
    let owed: Vec<(String, String, ShareState, Option<String>)> = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox
            .items
            .iter()
            .filter(|e| e.owe && seen.contains(&e.item.peer))
            .map(|e| {
                // Why it was declined, when midnad declined it (an agent ask whose terminal went).
                let reason = e.item.result.clone().filter(|_| e.item.state == ShareState::Declined);
                (e.item.id.clone(), e.item.peer.clone(), e.item.state, reason)
            })
            .collect()
    };
    for (id, fp, state, reason) in owed {
        let state_word = if state == ShareState::Accepted { "accepted" } else { "declined" };
        let Ok(mut ch) = link::connect_peer(d, &fp) else { continue };
        let answer = ch.send(&Wire::ShareResult { id: id.clone(), state: state_word.into(), reason }).and_then(|_| ch.expect(transport::HANDSHAKE_TIMEOUT));
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
    // After the results, so an agent start is accepted on its sender before it is answered.
    agent_start::deliver(d, &seen);
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
        Entry::new(
            ShareItem {
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
                transfer: None,
                auto: false,
            },
            None,
        )
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
