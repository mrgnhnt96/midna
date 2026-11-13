//! Starting an agent on a paired device with a prompt (`share.send` kind `agent_start`), and
//! getting its answer back (`share.answer`).
//!
//! The sender reads where the new agent should work from a terminal ([`build`]): its project
//! (as a tab's) and directory. Nothing runs on the other Mac until its person accepts (or set
//! this device to auto-accept agent starts): then its midnad opens a new agent terminal in its
//! checkout of that project (where a tab would open) with the prompt as the agent's first
//! ([`open`]). With `want_reply` it watches that terminal: when the agent's first turn ends it
//! keeps the agent's last message, cut to a message's limit, as an answer owed to the sender
//! ([`turn_ended`], or [`ended`] if the terminal ends first), and the delivery thread offers it
//! back as a message whose `reply_to` is the agent start ([`deliver`]). On the sender that
//! message is read at once, fires `share.answered`, and `share.answer` returns it ([`answered`],
//! [`answer`]).
//!
//! With `project` the sender names the receiver's project instead, and needs no terminal: the
//! agent starts in that project's folder wherever the sender is (see `tab.rs`).
use super::share::{self, Entry, Reply};
use super::tab::{self, Place};
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok};
use midna_proto::nearby::{self as nb, ShareAnswerParams, ShareAnswerResult, ShareDirection, ShareItem, SharePayload, ShareSendParams, ShareState};
use midna_proto::{AgentKind, RpcError, SessionKind, SessionOpenParams, time};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a title made from the prompt may be.
const TITLE_FROM_PROMPT: usize = 48;
/// How often `share.answer` looks again while it waits.
const POLL: Duration = Duration::from_millis(200);

/// `share.send` kind `agent_start`: the payload, with the project and directory of terminal
/// `session` (default the caller's), or the receiver's project named `project`.
pub fn build(d: &Daemon, ctx: &Ctx, p: &ShareSendParams) -> Result<SharePayload, RpcError> {
    if p.text.is_some() || p.url.is_some() || p.reply_to.is_some() || p.path.is_some() || p.object.is_some() || !p.ids.is_empty() {
        return Err(RpcError::bad_params(
            "an agent start takes a `prompt` (and `agent`, `session` or `project`, `title`, `want_reply`), not text, a url, a path or an object",
        ));
    }
    let prompt = p.prompt.clone().filter(|t| !t.trim().is_empty()).ok_or_else(|| RpcError::bad_params("an agent start needs a `prompt`"))?;
    let named = tab::named_project(p)?;
    if named.is_some() && p.session.is_some() {
        return Err(RpcError::bad_params("an agent start opens in a terminal's project (`session`) or in a named one (`project`), not both"));
    }
    let first = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    let title = tab::title_of(p, &share::cut(first, TITLE_FROM_PROMPT));
    let payload = match named {
        Some(name) => {
            let mine = ctx.session.as_deref().and_then(|sid| d.core().state.session(sid).and_then(|s| s.agent));
            let agent = p.agent.or(mine).unwrap_or(AgentKind::Claude);
            SharePayload::AgentStart { title, agent, project: None, cwd: String::new(), project_name: Some(name), prompt, want_reply: p.want_reply }
        }
        None => {
            let sid = p
                .session
                .clone()
                .or_else(|| ctx.session.clone())
                .ok_or_else(|| RpcError::bad_params("which project? pass `project` (its name on the other Mac) or `session` (a terminal in it here)"))?;
            let (s, pid) = {
                let core = d.core();
                let s = core.state.session(&sid).cloned().ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
                (s, core.rt.get(&sid).map(|rt| rt.pid))
            };
            let cwd = crate::git::work_cwd(&s, pid);
            let agent = p.agent.or(s.agent).unwrap_or(AgentKind::Claude);
            SharePayload::AgentStart { title, agent, project: tab::project_of(&cwd), cwd, project_name: None, prompt, want_reply: p.want_reply }
        }
    };
    payload.check().map_err(RpcError::bad_params)?;
    Ok(payload)
}

/// For an agent start's guard and approval: a one-line summary (the prompt's first line), then
/// the whole prompt and where it would work and whether its answer comes back.
pub fn describe(payload: &SharePayload, shown: usize) -> String {
    let SharePayload::AgentStart { agent, project, cwd, project_name, prompt, want_reply, .. } = payload else { return String::new() };
    let place = match project {
        _ if project_name.is_some() => format!("project {}", project_name.as_deref().unwrap_or_default()),
        Some(p) if p.path.is_empty() => format!("{} (in {cwd})", p.remote),
        Some(p) => format!("{}/{} (in {cwd})", p.remote, p.path),
        None => cwd.clone(),
    };
    let first = prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let back = if *want_reply { " Its answer comes back when its first turn ends." } else { "" };
    format!(
        "Prompt: {}\n\n{}\n\n{} in {place}.{back}",
        share::cut(first, 80),
        share::cut(prompt.trim(), shown),
        super::session::agent_name(*agent)
    )
}

/// Accept an incoming agent start: open a new agent terminal where it maps to (or `chosen`)
/// with its prompt. Returns what it did, for the share's `result`, and the new terminal.
pub fn open(d: &Arc<Daemon>, ctx: &Ctx, it: &ShareItem, chosen: Option<&str>) -> Result<(String, String), RpcError> {
    let SharePayload::AgentStart { title, agent, prompt, want_reply, .. } = &it.payload else { return Err(RpcError::bad_params("not an agent start")) };
    if it.state == ShareState::Accepted {
        // Accepting it again would run the prompt again.
        let did = it.result.clone().unwrap_or_else(|| "started".into());
        return Err(RpcError::conflict(format!("{} was accepted already ({did}); send it again to ask again", it.id)));
    }
    let place = match chosen {
        Some(c) => Place::chosen(c)?,
        None => tab::place(d, &it.payload),
    };
    place.usable()?;
    let here = place.here();
    let params = SessionOpenParams {
        project_id: Some(place.project_id(d)),
        kind: SessionKind::Agent,
        agent: Some(*agent),
        name: Some(title.clone()),
        cwd: place.dir().map(str::to_string),
        command: None,
        prompt: Some(prompt.clone()),
        agent_args: vec![],
        resume: None,
        cols: None,
        rows: None,
        background: false,
        close_on_exit: false,
        on_default_branch: false,
        switch_to_default_branch: false,
    };
    let back = if *want_reply { " · its first answer goes back to them" } else { "" };
    let preface = format!(
        "\x1b[1m── {} · {} started for {} {here}{back} ──\x1b[0m\r\n",
        tab::plain(title),
        super::session::agent_name(*agent),
        tab::plain(&it.peer_name)
    );
    let s = crate::rpc::session::open_session_with(d, ctx, params, Some(preface.into_bytes()))?;
    Ok((format!("opened as {} {here}", s.id), s.id))
}

/// `text` as a message may carry it: at most [`nb::MAX_MESSAGE_TEXT`] bytes, with … when cut.
fn message_text(text: &str) -> String {
    let text = text.trim();
    if text.len() <= nb::MAX_MESSAGE_TEXT {
        return text.to_string();
    }
    format!("{}…", tab::cut_bytes(text, nb::MAX_MESSAGE_TEXT - '…'.len_utf8()))
}

/// Keep `text` as the answer owed for the first incoming share `pick` chooses (every one with
/// `all`) that still waits for one: an agent start, or an agent ask (it stops waiting in its
/// queue too). The delivery thread sends it.
pub(super) fn owe_where(d: &Daemon, pick: impl Fn(&Entry) -> bool, all: bool, text: &str) {
    let owed = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        let mut owed = false;
        for e in inbox.items.iter_mut().filter(|e| e.item.direction == ShareDirection::Incoming && pick(e)) {
            e.queued = None;
            if e.watch.take().is_some() {
                e.reply = Some(Reply { id: format!("sh_{}", crate::state::hex_id(12)), text: message_text(text) });
                owed = true;
            }
            if !all {
                break;
            }
        }
        if owed {
            let _ = inbox.save();
        }
        owed
    };
    if owed {
        share::nudge(d);
    }
}

/// An agent's turn ended (its Stop hook, or Codex's turn-complete notify) with `message`, its
/// last one; `failed` says why when the turn failed. The first turn of an agent start that wants
/// a reply sends that message back, and so does the turn an agent ask's prompt started (not one
/// that ended while it still waited in the queue).
pub fn turn_ended(d: &Daemon, sid: &str, message: Option<&str>, failed: Option<&str>) {
    let message = message.map(str::trim).filter(|m| !m.is_empty());
    let text = match (failed, message) {
        (Some(why), Some(m)) => format!("(The agent's turn failed: {why})\n\n{m}"),
        (Some(why), None) => format!("(The agent's turn failed: {why})"),
        (None, Some(m)) => m.to_string(),
        (None, None) => "(The agent finished its turn without a message.)".to_string(),
    };
    owe_where(d, |e| e.watch.as_deref() == Some(sid) && e.queued.is_none(), false, &text);
}

/// Terminal `sid`'s process ended. An agent start or ask still waiting for its answer gets a
/// note instead, so its sender stops waiting.
pub fn ended(d: &Daemon, sid: &str) {
    owe_where(d, |e| e.watch.as_deref() == Some(sid), true, "(The agent's terminal ended before it answered.)");
}

/// Offer the answers owed to devices that can be seen now. Each keeps its share id, so an offer
/// whose ack was lost is taken once.
pub fn deliver(d: &Daemon, seen: &[String]) {
    let owed: Vec<(String, String, Reply)> = {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        inbox.items.iter().filter(|e| seen.contains(&e.item.peer)).filter_map(|e| Some((e.item.id.clone(), e.item.peer.clone(), e.reply.clone()?))).collect()
    };
    for (to, fp, reply) in owed {
        {
            // A try that got as far as recording it, but not its ack.
            let mut i = d.nearby.inner();
            let inbox = i.inbox(&d.cfg.home);
            inbox.items.retain(|e| !(e.item.id == reply.id && e.item.direction == ShareDirection::Outgoing));
        }
        let payload = SharePayload::Message { text: reply.text.clone(), reply_to: Some(to.clone()) };
        if share::offer(d, &fp, reply.id.clone(), payload, None, midna_proto::Actor::system(), false).is_err() {
            continue;
        }
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        if let Some(e) = inbox.get_mut(&to) {
            e.reply = None;
            let _ = inbox.save();
        }
    }
}

/// An incoming message from `from` (proven by the connection): if it answers an agent start or
/// ask this Mac sent it with `want_reply` and no answer came yet, mark it read and the share
/// answered. True if it did; the caller then fires `share.answered`.
pub(super) fn answered(inbox: &mut share::Inbox, from: &str, msg: &mut Entry) -> bool {
    let SharePayload::Message { reply_to: Some(to), .. } = &msg.item.payload else { return false };
    let Some(start) = inbox.get_mut(to) else { return false };
    let wanted = start.item.payload.wants_reply();
    if start.item.direction != ShareDirection::Outgoing || start.item.peer != from || !wanted || start.answered_by.is_some() {
        return false;
    }
    start.answered_by = Some(msg.item.id.clone());
    msg.item.state = ShareState::Accepted;
    msg.item.decided_at = Some(time::now_rfc3339());
    msg.item.result = Some(format!("read: the answer to {to}"));
    msg.item.auto = true;
    msg.owe = true;
    true
}

/// `share.answer`: an agent start or ask this Mac sent, and its answer once it came, waiting up to
/// `wait_secs` for it.
pub fn answer(d: &Daemon, p: ShareAnswerParams) -> R {
    let deadline = Instant::now() + Duration::from_secs(p.wait_secs.unwrap_or(0).min(nb::MAX_ANSWER_WAIT_SECS));
    loop {
        let (share, answer) = {
            let mut i = d.nearby.inner();
            let inbox = i.inbox(&d.cfg.home);
            let e = inbox
                .get(&p.id)
                .filter(|e| e.item.direction == ShareDirection::Outgoing)
                .ok_or_else(|| RpcError::not_found(format!("no share {} sent from this Mac (inbox.list)", p.id)))?;
            if !e.item.payload.wants_reply() {
                return Err(RpcError::bad_params(format!("{} isn't an agent start or ask sent with want_reply; nothing answers it", p.id)));
            }
            let answer = e.answered_by.as_deref().and_then(|a| inbox.get(a)).map(|a| a.item.clone());
            (e.item.clone(), answer)
        };
        let over = answer.is_some() || share.state == ShareState::Declined || d.shutting_down.load(std::sync::atomic::Ordering::Relaxed);
        if over || Instant::now() >= deadline {
            return ok(ShareAnswerResult { share, answer });
        }
        std::thread::sleep(POLL.min(deadline.saturating_duration_since(Instant::now())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_fits_in_a_message() {
        assert_eq!(message_text("  done \n"), "done");
        let long = "é".repeat(nb::MAX_MESSAGE_TEXT);
        let cut = message_text(&long);
        assert!(cut.len() <= nb::MAX_MESSAGE_TEXT && cut.ends_with('…'), "{}", cut.len());
        assert!(SharePayload::Message { text: cut, reply_to: Some("sh_abcd".into()) }.check().is_ok());
    }

    #[test]
    fn describing_shows_the_prompt_and_where() {
        let p = SharePayload::AgentStart {
            title: "ask".into(),
            agent: AgentKind::Claude,
            project: Some(nb::TabProject { remote: "github.com/a/b".into(), path: "src".into() }),
            cwd: "/w/b/src".into(),
            project_name: None,
            prompt: "Why?".into(),
            want_reply: true,
        };
        let text = describe(&p, 2000);
        assert!(text.starts_with("Prompt: Why?\n\nWhy?\n\nClaude Code in github.com/a/b/src (in /w/b/src).") && text.contains("answer comes back"), "{text}");
    }
}
