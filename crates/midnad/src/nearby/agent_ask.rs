//! Asking an agent terminal that is already open on one of my other Macs (`share.send` kind
//! `agent_ask`), and getting its answer back (`share.answer`).
//!
//! The sender first lists that Mac's agent terminals (`peers.sessions`, [`sessions`]): it asks
//! over the paired connection ([`Wire::SessionsGet`]) and the other side answers with names and
//! ids only, never what is on their screens, and only to a device it keeps as one of mine
//! ([`listing`]). The ask names one of them by id or name. The receiver finds it when the offer
//! comes ([`admit`]: a contact's ask, or one naming no agent terminal here, is refused with the
//! reason) and again when its person accepts: then the prompt is queued in that terminal
//! ([`queue`]), and the queue types it once the agent is idle, never into a busy prompt. With
//! `want_reply` the share watches the terminal like an agent start's: the turn that prompt
//! starts ends with the agent's last message, owed back to the sender (`agent_start.rs`). A turn
//! that ends while the prompt still waits in the queue isn't its answer ([`sent`] marks when it
//! went in), and taking the prompt out of the queue owes a note instead ([`unqueued`]). If the
//! terminal is gone or is no longer an agent by then, accepting declines the ask with the reason.
use super::share::{self, Entry};
use super::transport::{self, Wire};
use super::{agent_start, link};
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{PeerSession, PeersSessionsParams, PeersSessionsResult, ShareDirection, ShareItem, SharePayload, ShareState, Tier};
use midna_proto::{RpcError, SendWhen, StatusState};

/// Why a device that isn't one of mine can't see or ask this Mac's terminals.
const ONLY_MINE: &str = "only your own devices can see or ask this Mac's agent terminals";
/// The most terminals one listing carries.
const MAX_LISTED: usize = 200;

/// `peers.sessions`: the agent terminals open on one of my devices.
pub fn sessions(d: &Daemon, p: PeersSessionsParams) -> R {
    let fp = p.peer.trim().to_lowercase();
    let known = d.nearby.inner().store(&d.cfg.home).get(&fp).map(|s| (s.tier, s.name.clone()));
    let (tier, name) = known.ok_or_else(|| RpcError::not_found(format!("no paired device {fp} (peers.list)")))?;
    if tier != Tier::MyDevice {
        return Err(RpcError::refused(format!("{name} is kept as a contact: {ONLY_MINE}")));
    }
    let mut ch = link::connect_peer_waiting(d, &fp)?;
    let got = ch.send(&Wire::SessionsGet).and_then(|_| ch.expect(transport::HANDSHAKE_TIMEOUT));
    match got {
        Ok(Wire::Sessions { sessions }) => {
            ch.finish();
            ok(PeersSessionsResult { peer: fp, peer_name: name, sessions })
        }
        Ok(Wire::Refused { reason }) if reason == "unsupported" => Err(RpcError::refused(format!("{name} runs an older Midna that can't list its terminals"))),
        Ok(Wire::Refused { reason }) if reason == transport::NOT_PAIRED => Err(RpcError::refused(format!("{name} isn't paired with this Mac any more"))),
        Ok(Wire::Refused { reason }) => Err(RpcError::refused(format!("{name} wouldn't list its terminals: {reason}"))),
        Ok(other) => Err(RpcError::internal(format!("{name} answered {other:?}"))),
        Err(e) => Err(RpcError::conflict(format!("asking {name} for its terminals: {e}"))),
    }
}

/// A paired device (`from`, proven by the connection) asks for this Mac's agent terminals.
pub fn listing(d: &Daemon, from: &str) -> Wire {
    if !mine(d, from) {
        return Wire::Refused { reason: ONLY_MINE.into() };
    }
    Wire::Sessions { sessions: agent_terminals(d).into_iter().take(MAX_LISTED).collect() }
}

pub(super) fn mine(d: &Daemon, fp: &str) -> bool {
    d.nearby.inner().store(&d.cfg.home).get(fp).is_some_and(|s| s.tier == Tier::MyDevice)
}

/// The agent terminals running here, as another of my devices may see them.
fn agent_terminals(d: &Daemon) -> Vec<PeerSession> {
    let core = d.core();
    core.state
        .sessions
        .iter()
        .filter(|s| core.rt.contains_key(&s.id))
        .filter_map(|s| {
            let busy = matches!(s.status.state, StatusState::Working | StatusState::NeedsYou) || core.agents.get(&s.id).is_some_and(|a| a.in_turn);
            Some(PeerSession {
                id: s.id.clone(),
                name: s.name.clone(),
                agent: s.agent?,
                project: core.state.project(&s.project_id).map(|p| p.name.clone()),
                state: s.status.state,
                busy,
            })
        })
        .collect()
}

/// The agent terminal here that `query` names: its id, else its name (any case), else the one
/// whose name contains it. Err says why none (or more than one) does, for the sender.
fn resolve(d: &Daemon, query: &str) -> Result<PeerSession, String> {
    let q = query.trim();
    let all = agent_terminals(d);
    if let Some(s) = all.iter().find(|s| s.id == q) {
        return Ok(s.clone());
    }
    let lower = q.to_lowercase();
    let pick = |found: Vec<&PeerSession>, how: &str| -> Result<Option<PeerSession>, String> {
        match found.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some((*one).clone())),
            many => {
                let names: Vec<String> = many.iter().take(8).map(|s| format!("{} ({})", s.name, s.id)).collect();
                Err(format!("{} agent terminals here {how} “{q}”: {}; ask by id", many.len(), names.join(", ")))
            }
        }
    };
    if let Some(s) = pick(all.iter().filter(|s| s.name.to_lowercase() == lower).collect(), "are named")? {
        return Ok(s);
    }
    if let Some(s) = pick(all.iter().filter(|s| s.name.to_lowercase().contains(&lower)).collect(), "have names containing")? {
        return Ok(s);
    }
    // Say what it is, if it is a terminal here at all.
    let core = d.core();
    let named = core.state.sessions.iter().find(|s| s.id == q || s.name.to_lowercase() == lower);
    Err(match named {
        Some(s) if !core.rt.contains_key(&s.id) => format!("“{}” here isn't running", s.name),
        Some(s) => format!("“{}” here isn't an agent terminal", s.name),
        None => format!("no agent terminal here is called “{q}” (peers.sessions lists them)"),
    })
}

/// An incoming agent ask from `from`: only from one of my devices, and only naming an agent
/// terminal that is here. Err is the reason the offer is refused with.
pub(super) fn admit(d: &Daemon, from: &str, payload: &SharePayload) -> Result<(), String> {
    let SharePayload::AgentAsk { session, .. } = payload else { return Ok(()) };
    if !mine(d, from) {
        return Err(ONLY_MINE.into());
    }
    resolve(d, session).map(drop)
}

/// For an agent ask's guard: a one-line summary (the prompt's first line), then the whole prompt
/// and whether its answer comes back.
pub fn describe(payload: &SharePayload, shown: usize) -> String {
    let SharePayload::AgentAsk { session, prompt, want_reply } = payload else { return String::new() };
    let first = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let back = if *want_reply { " Its answer comes back when that turn ends." } else { "" };
    format!(
        "Prompt: {}\n\n{}\n\nQueued in the agent terminal “{session}” there, typed once that agent is idle.{back}",
        share::cut(first, 80),
        share::cut(prompt.trim(), shown)
    )
}

/// For an incoming agent ask's approval: (the terminal it names, as words; what approving does).
pub(super) fn approval(d: &Daemon, it: &ShareItem) -> (String, String) {
    let SharePayload::AgentAsk { session, want_reply, .. } = &it.payload else { return (String::new(), String::new()) };
    let back = if *want_reply { format!(", and {} gets the agent's answer when that turn ends", it.peer_name) } else { String::new() };
    match resolve(d, session) {
        Ok(s) => (
            format!("“{}”", s.name),
            format!(
                "Approving queues this prompt in {} ({}): it is typed once that agent is idle, never into a busy \
                 prompt{back}. Nothing else is typed or run.",
                s.name, s.id
            ),
        ),
        Err(why) => (format!("“{session}”"), format!("That terminal can't take it now ({why}): approving declines it with that reason.")),
    }
}

/// What accepting an agent ask did.
pub(super) enum Queued {
    /// Queued in terminal `sid` as message `id`; `result` says so.
    In { result: String, sid: String, id: String },
    /// The terminal it names isn't an agent terminal here (any more): decline it, with this.
    Gone(String),
}

/// Accept an incoming agent ask: queue its prompt in the terminal it names, typed once that
/// agent is idle.
pub(super) fn queue(d: &Daemon, ctx: &Ctx, it: &ShareItem) -> Result<Queued, RpcError> {
    let SharePayload::AgentAsk { session, prompt, .. } = &it.payload else { return Err(RpcError::bad_params("not an agent ask")) };
    if it.state == ShareState::Accepted {
        // Accepting it again would ask again.
        let did = it.result.clone().unwrap_or_else(|| "queued".into());
        return Err(RpcError::conflict(format!("{} was accepted already ({did}); send it again to ask again", it.id)));
    }
    let s = match resolve(d, session) {
        Ok(s) => s,
        Err(why) => return Ok(Queued::Gone(why)),
    };
    let m = crate::queue::add(d, &s.id, prompt.clone(), true, vec![], SendWhen::Idle, None, ctx.actor(), None)?;
    let result = format!("queued in {} ({}), typed once its agent is idle", s.id, s.name);
    Ok(Queued::In { result, sid: s.id, id: m.id })
}

/// The queue typed message `id` into terminal `sid`: if it is an agent ask's prompt, the next
/// turn that ends there is its answer.
pub fn sent(d: &Daemon, sid: &str, id: &str) {
    let mut i = d.nearby.inner();
    let inbox = i.inbox(&d.cfg.home);
    let Some(e) = inbox.items.iter_mut().find(|e| asks_with(e, sid, id)) else { return };
    e.queued = None;
    let _ = inbox.save();
}

/// Accepting recorded that message `id` waits in terminal `sid`'s queue. If the queue typed it
/// in the meantime (an idle agent takes it at once), [`sent`] came too early to see that: mark it
/// sent now.
pub(super) fn recorded(d: &Daemon, sid: &str, id: &str) {
    let waiting = d.core().state.session(sid).is_some_and(|s| s.queue.iter().any(|m| m.id == id));
    if !waiting {
        sent(d, sid, id);
    }
}

/// Queued messages `ids` were taken out of terminal `sid`'s queue before they were sent: an
/// agent ask among them that wants its answer gets a note instead, so its sender stops waiting.
pub fn unqueued(d: &Daemon, sid: &str, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let name = d.core().state.session(sid).map(|s| s.name.clone()).unwrap_or_else(|| sid.to_string());
    let note = format!("(The prompt was taken out of {name}'s queue before it was sent; the agent didn't see it.)");
    agent_start::owe_where(d, |e| ids.iter().any(|id| asks_with(e, sid, id)), true, &note);
}

/// `e` is an incoming agent ask whose prompt waits in terminal `sid`'s queue as message `id`.
fn asks_with(e: &Entry, sid: &str, id: &str) -> bool {
    e.item.direction == ShareDirection::Incoming
        && matches!(e.item.payload, SharePayload::AgentAsk { .. })
        && e.queued.as_deref() == Some(id)
        && (e.watch.is_none() || e.watch.as_deref() == Some(sid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describing_shows_the_prompt_and_the_terminal() {
        let p = SharePayload::AgentAsk { session: "api tests".into(), prompt: "Did it pass?\nWhich failed?".into(), want_reply: true };
        let text = describe(&p, 2000);
        assert!(text.starts_with("Prompt: Did it pass?\n\nDid it pass?\nWhich failed?\n\nQueued in the agent terminal “api tests”"), "{text}");
        assert!(text.contains("answer comes back"), "{text}");
        let quiet = SharePayload::AgentAsk { session: "x".into(), prompt: "hi".into(), want_reply: false };
        assert!(!describe(&quiet, 2000).contains("answer"));
    }
}
