//! Queued messages: text waiting to be typed into a terminal (`Session.queue`, `queue.*`).
//!
//! Every half second one worker looks at the first message of each terminal's queue. It goes
//! in once its `when` holds and the agent is ready for input: not working or waiting on the
//! human, no permission prompt on screen, nothing typed in Claude's input box, and steady like
//! that for `READY_FOR`. After a submitted message the agent first has to look busy (or
//! `BUSY_GRACE` passes), so the next one never lands before the last one started. Typing runs
//! on its own thread; one message per terminal at a time. A message that can't be typed stays
//! first as `failed` and holds the queue until it is retried, sent now or removed.
//!
//! Local triggers' `send_to_session` adds its steps here, so triggers, agents and the human
//! share one ordered queue per terminal.
use crate::daemon::Daemon;
use crate::rpc::{Ctx, Role};
use crate::state::hex_id;
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(500);
/// After a submitted message the agent gets this long to look busy before "ready" counts.
const BUSY_GRACE: Duration = Duration::from_secs(5);
/// How long the agent must look ready before a message is typed.
const READY_FOR: Duration = Duration::from_millis(1500);
const PASTE_GAP: Duration = Duration::from_millis(250);
const ENTER_DELAY: Duration = Duration::from_millis(300);
pub const MAX_ITEMS: usize = 50;

/// What the worker remembers about a terminal between ticks.
#[derive(Default)]
struct Watch {
    ready_since: Option<Instant>,
    /// When the last message went in, and whether the agent has looked busy since.
    sent_at: Option<Instant>,
    seen_busy: bool,
    /// The last message was typed without Enter: the next one goes on after it.
    unsubmitted: bool,
}

#[derive(Default)]
struct Book {
    watch: HashMap<Id, Watch>,
    /// Terminals a message is being typed into.
    sending: HashSet<Id>,
}

#[derive(Default)]
pub struct Runtime {
    book: Mutex<Book>,
}

impl Runtime {
    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("queue".into()).spawn(move || {
        loop {
            std::thread::sleep(TICK);
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            tick(&d);
        }
    });
}

/// The terminal has messages waiting (idle triggers leave it alone).
pub fn pending(d: &Daemon, sid: &str) -> bool {
    d.core().state.session(sid).is_some_and(|s| !s.queue.is_empty())
}

// ------------------------------------------------------------------ the worker

fn tick(d: &Arc<Daemon>) {
    let heads: Vec<(Id, QueuedMessage)> = {
        let core = d.core();
        core.state.sessions.iter().filter(|s| !s.queue_paused).filter_map(|s| Some((s.id.clone(), s.queue.first()?.clone()))).collect()
    };
    let live: HashSet<&Id> = heads.iter().map(|(s, _)| s).collect();
    d.queue.book().watch.retain(|s, _| live.contains(s));
    for (sid, head) in heads {
        if head.state != QueueState::Waiting || d.queue.book().sending.contains(&sid) {
            continue;
        }
        let w = waiting(d, &sid, &head);
        if w != head.waiting_for {
            edit(d, &sid, |q| {
                if let Some(m) = q.iter_mut().find(|m| m.id == head.id) {
                    m.waiting_for = w.clone();
                }
            });
        }
        if !w.is_empty() {
            continue;
        }
        if !d.queue.book().sending.insert(sid.clone()) {
            continue;
        }
        edit(d, &sid, |q| {
            if let Some(m) = q.iter_mut().find(|m| m.id == head.id) {
                m.state = QueueState::Sending;
            }
        });
        let (d2, sid2) = (d.clone(), sid.clone());
        let spawned = std::thread::Builder::new().name("queue-send".into()).spawn(move || {
            let res = type_message(&d2, &sid2, &head);
            finish(&d2, &sid2, &head, res);
        });
        if spawned.is_err() {
            d.queue.book().sending.remove(&sid);
        }
    }
}

/// What the first message of `sid`'s queue waits for, in plain words (empty = go).
fn waiting(d: &Daemon, sid: &str, m: &QueuedMessage) -> Vec<String> {
    let mut w = vec![];
    let now = time::now_unix();
    match &m.when {
        SendWhen::Idle => {}
        SendWhen::IdleFor { minutes } => {
            // Awake time: the Mac asleep isn't quiet the human chose to leave.
            let quiet = last_activity(d, sid).map_or(0, |last| d.clock.awake_secs_since(last));
            let need = i64::from(*minutes) * 60;
            if quiet < need {
                w.push(format!("{minutes} min of quiet ({} min so far)", quiet / 60));
            }
        }
        SendWhen::At { at } => {
            if time::parse_rfc3339(at).is_some_and(|t| now < t) {
                w.push(format!("{at} to come"));
            }
        }
        SendWhen::After { session } => {
            if let Some(why) = still_running(d, session) {
                w.push(why);
            }
        }
    }
    let unsubmitted = d.queue.book().watch.get(sid).is_some_and(|x| x.unsubmitted);
    let ready = readiness(d, sid, !unsubmitted);
    let mut book = d.queue.book();
    let watch = book.watch.entry(sid.to_string()).or_default();
    match ready {
        Err(why) | Ok(Some(why)) => {
            watch.ready_since = None;
            if ready_is_busy(&why) {
                watch.seen_busy = true;
            }
            w.push(why);
        }
        Ok(None) => {
            let fresh = watch.sent_at.is_some_and(|t| t.elapsed() < BUSY_GRACE);
            if fresh && !watch.seen_busy && !watch.unsubmitted {
                w.push("the last message to start".into());
            } else if watch.ready_since.get_or_insert_with(Instant::now).elapsed() < READY_FOR {
                w.push("the agent to settle".into());
            }
        }
    }
    w
}

const WORKING: &str = "the agent to finish working";

fn ready_is_busy(why: &str) -> bool {
    why == WORKING || why == "the turn in progress to end"
}

/// Can `sid` take input now? `Ok(None)` = yes, `Ok(Some(why))` = not yet, `Err` = it never
/// will as things are (the terminal closed or its process ended).
pub fn readiness(d: &Daemon, sid: &str, check_input_box: bool) -> Result<Option<String>, String> {
    let (rt, agent, title, state, labelled, in_turn, alive) = {
        let core = d.core();
        let s = core.state.session(sid).ok_or("the terminal closed")?;
        let rt = core.rt.get(sid).cloned().ok_or("the terminal's process ended")?;
        // A state a custom status holds (e.g. "Prompt blocked" as needs_you) is only a label.
        let labelled = s.custom_status.as_ref().is_some_and(|c| c.base == s.status.state && s.status.reason.as_deref() == Some(c.label.as_str()));
        (rt, s.agent, s.title.clone(), s.status.state, labelled, core.agents.get(sid).is_some_and(|a| a.in_turn), s.pid.is_some())
    };
    // An agent whose turn died on an error (`StopFailure`) is failed but still running.
    if state.is_terminal() && !alive {
        return Err("the terminal's process ended".into());
    }
    match state {
        StatusState::Working if !labelled => return Ok(Some(WORKING.into())),
        StatusState::NeedsYou if !labelled => return Ok(Some("you to answer the agent".into())),
        _ if in_turn => return Ok(Some("the turn in progress to end".into())),
        _ => {}
    }
    if let Some(agent) = agent
        && crate::agent_state::agent_title_hint(agent, &title) == crate::agent_state::TitleHint::Working
    {
        return Ok(Some(WORKING.into()));
    }
    if rt.read(true).is_some_and(|(screen, _, _)| crate::agent_state::screen_waits_on_human(&screen)) {
        return Ok(Some("a prompt on screen to be answered".into()));
    }
    // Never type on top of a draft. (No input box found = can't tell, e.g. a shell: go.)
    if check_input_box
        && agent == Some(AgentKind::Claude)
        && let Some(Some(t)) = rt.with(|e| e.screen_undimmed()).map(|rows| crate::agent_work::claude_input_text(&rows))
        && !t.trim().is_empty()
    {
        return Ok(Some("the text typed in the input box to be sent or cleared".into()));
    }
    Ok(None)
}

/// Unix time of the terminal's last output, prompt or status change.
fn last_activity(d: &Daemon, sid: &str) -> Option<i64> {
    let since = d.core().state.session(sid).and_then(|s| time::parse_rfc3339(&s.status.since));
    let out = d.rt(sid).map(|rt| rt.activity.load(Ordering::Relaxed));
    since.max(out)
}

/// Why `other` hasn't finished yet (None = it has: idle with an empty queue, ended or gone).
fn still_running(d: &Daemon, other: &str) -> Option<String> {
    let core = d.core();
    let s = core.state.session(other)?;
    if s.status.state.is_terminal() || !core.rt.contains_key(other) {
        return None;
    }
    let busy = matches!(s.status.state, StatusState::Working | StatusState::NeedsYou) || core.agents.get(other).is_some_and(|a| a.in_turn);
    (busy || !s.queue.is_empty()).then(|| format!("{} to finish", s.name))
}

/// Type one message the way `session.input` would for whoever queued it.
fn type_message(d: &Arc<Daemon>, sid: &str, m: &QueuedMessage) -> Result<(), String> {
    let ctx = ctx_for(&m.by);
    let input = |text: &str, enter: bool, images: Vec<String>| {
        let p = SessionInputParams { id: sid.to_string(), text: text.to_string(), enter, images };
        crate::rpc::session::input(d, &ctx, p).map(|_| ()).map_err(|e| e.message)
    };
    if !m.text.contains('\n') {
        return input(&m.text, m.enter, m.images.clone());
    }
    // Several lines go in as one paste, so a newline isn't a submit.
    if !m.images.is_empty() {
        input("", false, m.images.clone())?;
        std::thread::sleep(PASTE_GAP);
    }
    let rt = d.rt(sid).ok_or("the terminal's process ended")?;
    let gone = || "the terminal stopped taking input".to_string();
    if !rt.client(midna_proto::frame::ClientMsg::Paste(m.text.clone())) {
        return Err(gone());
    }
    if m.enter {
        std::thread::sleep(ENTER_DELAY);
        if !rt.key(crate::term::Key::Enter) {
            return Err(gone());
        }
    }
    Ok(())
}

fn ctx_for(by: &Actor) -> Ctx {
    match by.kind {
        ActorKind::Human => Ctx::internal_human(),
        ActorKind::Agent => {
            let mut c = Ctx::internal_trigger(by.clone());
            c.role = Role::Agent;
            c.session = by.session.clone();
            c
        }
        ActorKind::Trigger | ActorKind::System => Ctx::internal_trigger(by.clone()),
    }
}

/// After typing: a sent message leaves the queue, a failed one stays first and holds it.
fn finish(d: &Daemon, sid: &str, m: &QueuedMessage, res: Result<(), String>) {
    d.queue.book().sending.remove(sid);
    match res {
        Ok(()) => {
            {
                let mut book = d.queue.book();
                let w = book.watch.entry(sid.to_string()).or_default();
                *w = Watch { sent_at: Some(Instant::now()), unsubmitted: !m.enter, ..Default::default() };
            }
            edit(d, sid, |q| q.retain(|x| x.id != m.id));
            changed(d, sid, m.by.clone(), "sent", json!({ "id": m.id, "text": clip(&m.text, 200), "trigger_id": m.trigger_id }));
        }
        Err(why) => {
            edit(d, sid, |q| {
                if let Some(x) = q.iter_mut().find(|x| x.id == m.id) {
                    x.state = QueueState::Failed;
                    x.error = Some(why.clone());
                    x.waiting_for.clear();
                }
            });
            changed(d, sid, m.by.clone(), "failed", json!({ "id": m.id, "text": clip(&m.text, 200), "error": why }));
            let name = d.core().state.session(sid).map(|s| s.name.clone()).unwrap_or_default();
            let mut item = d.new_needs_you(NeedsYouKind::Note, format!("Couldn't send a queued message to {name}: {why}"), m.by.clone(), Some(sid.to_string()));
            item.bulk_safe = true;
            item.trigger_id = m.trigger_id.clone();
            d.raise_needs_you(item);
        }
    }
}

// ------------------------------------------------------------------ changes

/// Edit a terminal's queue in place (no event).
fn edit<T>(d: &Daemon, sid: &str, f: impl FnOnce(&mut Vec<QueuedMessage>) -> T) -> Option<T> {
    let out = {
        let mut core = d.core();
        let s = core.state.session_mut(sid)?;
        f(&mut s.queue)
    };
    d.mark_dirty();
    Some(out)
}

/// Emit `session.queue` with what happened plus `left` and `paused`.
fn changed(d: &Daemon, sid: &str, actor: Actor, action: &str, extra: Value) {
    let Some((project, left, paused)) = d.core().state.session(sid).map(|s| (s.project_id.clone(), s.queue.len(), s.queue_paused)) else { return };
    let mut data = json!({ "action": action, "left": left, "paused": paused });
    if let (Some(o), Value::Object(x)) = (data.as_object_mut(), extra) {
        o.extend(x.into_iter().filter(|(_, v)| !v.is_null()));
    }
    d.emit(kinds::SESSION_QUEUE, actor, Some(project), Some(sid.to_string()), data);
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}

pub fn list(d: &Daemon, sid: &str) -> Result<QueueListResult, RpcError> {
    let core = d.core();
    let s = core.state.session(sid).ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
    Ok(QueueListResult { session: sid.to_string(), paused: s.queue_paused, items: s.queue.clone() })
}

fn check_when(d: &Daemon, sid: &str, w: &SendWhen) -> Result<(), RpcError> {
    match w {
        SendWhen::Idle => Ok(()),
        SendWhen::IdleFor { minutes } if *minutes == 0 || *minutes > 7 * 24 * 60 => Err(RpcError::bad_params("idle_for minutes must be 1..=10080")),
        SendWhen::IdleFor { .. } => Ok(()),
        SendWhen::At { at } if time::parse_rfc3339(at).is_none() => Err(RpcError::bad_params(format!("`at` is not an RFC 3339 time: {at}"))),
        SendWhen::At { .. } => Ok(()),
        SendWhen::After { session } if session == sid => Err(RpcError::bad_params("a message can't wait for its own terminal")),
        SendWhen::After { session } if d.core().state.session(session).is_none() => Err(RpcError::not_found(format!("no terminal {session}"))),
        SendWhen::After { .. } => Ok(()),
    }
}

/// Queue a message. The first slot is taken while a message is being typed.
#[allow(clippy::too_many_arguments)]
pub fn add(d: &Daemon, sid: &str, text: String, enter: bool, images: Vec<String>, when: SendWhen, position: Option<usize>, by: Actor, trigger_id: Option<Id>) -> Result<QueuedMessage, RpcError> {
    if text.is_empty() && images.is_empty() {
        return Err(RpcError::bad_params("nothing to queue: give `text` or `images`"));
    }
    if let Some(bad) = images.iter().find(|p| !p.starts_with('/')) {
        return Err(RpcError::bad_params(format!("image paths must be absolute: {bad}")));
    }
    check_when(d, sid, &when)?;
    let m = QueuedMessage {
        id: format!("q_{}", hex_id(6)),
        text,
        enter,
        images,
        when,
        by: by.clone(),
        queued_at: time::now_rfc3339(),
        state: QueueState::Waiting,
        error: None,
        trigger_id,
        waiting_for: vec![],
    };
    let res = edit(d, sid, |q| {
        if q.len() >= MAX_ITEMS {
            return Err(RpcError::conflict(format!("the queue is full ({MAX_ITEMS} messages)")));
        }
        let first = usize::from(q.first().is_some_and(|x| x.state == QueueState::Sending));
        let at = position.unwrap_or(q.len()).clamp(first, q.len());
        q.insert(at, m.clone());
        Ok(())
    });
    res.ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))??;
    changed(d, sid, by, "added", json!({ "id": m.id, "text": clip(&m.text, 200) }));
    Ok(m)
}

/// Run `f` on message `id`, refusing one that is being typed.
fn with_item<T>(d: &Daemon, sid: &str, id: &str, f: impl FnOnce(&mut Vec<QueuedMessage>, usize) -> T) -> Result<T, RpcError> {
    edit(d, sid, |q| {
        let i = q.iter().position(|x| x.id == id).ok_or_else(|| RpcError::not_found(format!("no queued message {id} in {sid}")))?;
        if q[i].state == QueueState::Sending {
            return Err(RpcError::conflict(format!("{id} is being typed right now")));
        }
        Ok(f(q, i))
    })
    .ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?
}

pub fn update(d: &Daemon, sid: &str, p: QueueUpdateParams, by: Actor) -> Result<QueuedMessage, RpcError> {
    if let Some(w) = &p.when {
        check_when(d, sid, w)?;
    }
    if p.text.as_deref().is_some_and(str::is_empty) {
        return Err(RpcError::bad_params("empty text: remove the message instead"));
    }
    let m = with_item(d, sid, &p.id, |q, i| {
        let m = &mut q[i];
        if let Some(t) = p.text {
            m.text = t;
        }
        if let Some(e) = p.enter {
            m.enter = e;
        }
        if let Some(w) = p.when {
            m.when = w;
        }
        if p.retry || m.state == QueueState::Failed {
            m.state = QueueState::Waiting;
            m.error = None;
        }
        m.waiting_for.clear();
        m.clone()
    })?;
    changed(d, sid, by, "updated", json!({ "id": m.id, "text": clip(&m.text, 200) }));
    Ok(m)
}

pub fn remove(d: &Daemon, sid: &str, id: &str, by: Actor) -> Result<(), RpcError> {
    let m = with_item(d, sid, id, |q, i| q.remove(i))?;
    changed(d, sid, by, "removed", json!({ "id": m.id, "text": clip(&m.text, 200) }));
    Ok(())
}

pub fn clear(d: &Daemon, sid: &str, by: Actor) -> Result<QueueListResult, RpcError> {
    let n = edit(d, sid, |q| {
        let before = q.len();
        q.retain(|x| x.state == QueueState::Sending);
        before - q.len()
    })
    .ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
    if n > 0 {
        changed(d, sid, by, "cleared", json!({ "removed": n }));
    }
    list(d, sid)
}

pub fn move_to(d: &Daemon, sid: &str, id: &str, to: usize, by: Actor) -> Result<QueueListResult, RpcError> {
    with_item(d, sid, id, |q, i| {
        let m = q.remove(i);
        let first = usize::from(q.first().is_some_and(|x| x.state == QueueState::Sending));
        q.insert(to.clamp(first, q.len()), m);
    })?;
    changed(d, sid, by, "moved", json!({ "id": id, "to": to }));
    list(d, sid)
}

pub fn pause(d: &Daemon, sid: &str, paused: bool, by: Actor) -> Result<QueueListResult, RpcError> {
    let was = {
        let mut core = d.core();
        let s = core.state.session_mut(sid).ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
        std::mem::replace(&mut s.queue_paused, paused)
    };
    if was != paused {
        d.mark_dirty();
        changed(d, sid, by, if paused { "paused" } else { "resumed" }, json!({}));
    }
    list(d, sid)
}

/// Type message `id` now, whatever its `when` and however busy the agent is.
pub fn send_now(d: &Arc<Daemon>, sid: &str, id: &str) -> Result<(), RpcError> {
    if !d.queue.book().sending.insert(sid.to_string()) {
        return Err(RpcError::conflict("another queued message is being typed into this terminal"));
    }
    let m = with_item(d, sid, id, |q, i| {
        q[i].state = QueueState::Sending;
        q[i].clone()
    });
    let m = match m {
        Ok(m) => m,
        Err(e) => {
            d.queue.book().sending.remove(sid);
            return Err(e);
        }
    };
    let res = type_message(d, sid, &m);
    finish(d, sid, &m, res.clone());
    res.map_err(RpcError::conflict)
}

/// A local trigger's `send_to_session`: its steps, in order, at the end of the queue.
pub fn add_steps(d: &Daemon, sid: &str, t: &Trigger, by: Actor, steps: Vec<(String, bool)>) -> Result<usize, String> {
    if d.core().state.session(sid).is_some_and(|s| s.queue.iter().any(|m| m.trigger_id.as_deref() == Some(t.id.as_str()))) {
        return Err("still queued from an earlier firing".into());
    }
    let n = steps.len();
    for (text, enter) in steps {
        add(d, sid, text, enter, vec![], SendWhen::Idle, None, by.clone(), Some(t.id.clone())).map_err(|e| e.message)?;
    }
    Ok(n)
}
