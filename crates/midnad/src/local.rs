//! Local triggers: triggers that fire on what midnad sees on this Mac (an agent hook
//! `hook.<Event>`, any midna event such as `agent.prompt_blocked` or `session.status`, a
//! terminal going `idle`, or the clock: `schedule` with a cron expression), and the custom
//! statuses they put on terminals (`set_status`).
//!
//! One worker thread takes events (the event log's listener) and hooks (`agent.hook`) in order,
//! clears custom statuses whose condition passed, and fires matching triggers. Events a trigger
//! caused never fire triggers (no chains, no loops), and a trigger waits `cooldown_secs`
//! (default 60) before firing again for the same terminal. `send_to_session` adds its steps to
//! the terminal's message queue (`queue.rs`), which types each once the agent is ready.
//!
//! It also notices prompts a UserPromptSubmit hook refused: Claude erases the prompt, writes a
//! warning to its transcript and never starts a turn, so nothing would end the turn midna opened
//! for it. midna ends it and emits `agent.prompt_blocked` (`{hook, message, prompt}`).
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use crate::state::hex_id;
use crate::webhooks::payload::{glob_ci, shell_quote};
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const DEFAULT_COOLDOWN_SECS: u64 = 60;
/// The built-in trigger that labels a terminal whose prompt a hook refused.
pub const BUILTIN_PROMPT_BLOCKED: &str = "prompt_blocked_status";
const IDLE_TICK: Duration = Duration::from_secs(15);
/// A transcript warning older than this is history (a first read of an old transcript).
const BLOCKED_FRESH_SECS: i64 = 120;
/// If nothing follows a prompt for this long and the agent's title says it's stopped, the
/// prompt went nowhere (a refusal the transcript didn't show).
const SILENT_PROMPT: Duration = Duration::from_secs(15);
/// After a sleep (or a stalled worker) missed schedule minutes this recent still fire, once.
const SCHEDULE_CATCH_UP_SECS: i64 = 10 * 60;
const BLOCKED_PREFIX: &str = "UserPromptSubmit operation blocked by hook";

pub enum Input {
    Event(Event),
    Hook { session: Id, agent: AgentKind, event: String, payload: Value },
}

#[derive(Default)]
struct Book {
    /// Unix time of each terminal's last prompt or turn start/end (the idle clock).
    activity: HashMap<Id, i64>,
    /// (trigger, terminal) -> when it last fired.
    fired: HashMap<(Id, Id), Instant>,
    /// (trigger, terminal) -> the activity time an idle trigger already fired for.
    idle_fired: HashMap<(Id, Id), i64>,
    /// Hooks seen per terminal (the silent-prompt check).
    hooks: HashMap<Id, u64>,
    /// Event seq when each terminal's custom status was set; older events don't clear it.
    status_seq: HashMap<Id, u64>,
    /// The last minute (unix seconds) schedule triggers were checked for.
    schedule_checked: Option<i64>,
}

#[derive(Default)]
pub struct Runtime {
    tx: Mutex<Option<Sender<Input>>>,
    book: Mutex<Book>,
}

impl Runtime {
    fn book(&self) -> MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, i: Input) {
        if let Some(tx) = self.tx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = tx.send(i);
        }
    }
}

pub fn start(d: &Arc<Daemon>) {
    seed_builtins(d);
    seed_activity(d);
    let (tx, rx) = channel();
    *d.local.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
    let events = d.log.listen();
    let _ = std::thread::Builder::new().name("local-events".into()).spawn(move || {
        for e in events {
            if tx.send(Input::Event(e)).is_err() {
                return;
            }
        }
    });
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("local-triggers".into()).spawn(move || {
        let mut last_idle = Instant::now();
        loop {
            let got = rx.recv_timeout(IDLE_TICK);
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            match got {
                Ok(Input::Event(e)) => on_event(&d, &e),
                Ok(Input::Hook { session, agent, event, payload }) => on_hook(&d, &session, agent, &event, &payload),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if last_idle.elapsed() >= IDLE_TICK {
                last_idle = Instant::now();
                idle_tick(&d);
                schedule_tick(&d);
            }
        }
    });
}

/// `agent.hook` saw `event` for `sid` (called on the hook's connection thread).
pub fn hook(d: &Arc<Daemon>, sid: &str, agent: AgentKind, event: &str, payload: &Value) {
    let seen = {
        let mut b = d.local.book();
        let n = b.hooks.entry(sid.to_string()).or_default();
        *n += 1;
        *n
    };
    if agent == AgentKind::Claude && event == "UserPromptSubmit" {
        watch_silent_prompt(d, sid, seen);
    }
    d.local.send(Input::Hook { session: sid.to_string(), agent, event: format!("hook.{event}"), payload: payload.clone() });
}

// ------------------------------------------------------------------ built-ins

/// Add the built-in triggers that were never added (or were reset).
pub fn seed_builtins(d: &Daemon) {
    let mut core = d.core();
    if core.state.seeded.iter().any(|s| s == BUILTIN_PROMPT_BLOCKED) {
        return;
    }
    core.state.seeded.push(BUILTIN_PROMPT_BLOCKED.into());
    let now = time::now_rfc3339();
    core.state.triggers.push(Trigger {
        id: format!("t_{}", hex_id(6)),
        name: "Prompt blocked by a hook".into(),
        source: TriggerSource::Local,
        event: kinds::AGENT_PROMPT_BLOCKED.into(),
        filter: TriggerFilter::default(),
        action: TriggerAction::SetStatus { label: "Prompt blocked".into(), color: "amber".into(), icon: None, base: StatusState::NeedsYou, clear_on: StatusClear::Prompt },
        enabled: true,
        state: TriggerState::Active,
        secret_set: false,
        created_by: Actor::system(),
        created_at: now.clone(),
        last_fired_at: None,
        fired: 0,
        last_fired_summary: None,
        enabled_at: Some(now),
        secret_set_at: None,
        secret_store: None,
        github_hook_id: None,
        session_name_template: None,
        cooldown_secs: Some(0),
        builtin: Some(BUILTIN_PROMPT_BLOCKED.into()),
    });
    drop(core);
    d.mark_dirty();
}

/// The idle clock survives a daemon restart: start it from the last turn events in the log.
fn seed_activity(d: &Daemon) {
    let found: HashMap<Id, i64> = d.log.with_events(|events| {
        let mut m = HashMap::new();
        for e in events.iter().filter(|e| is_activity(&e.kind)) {
            if let (Some(sid), Some(at)) = (&e.session_id, time::parse_rfc3339(&e.at)) {
                m.insert(sid.clone(), at);
            }
        }
        m
    });
    d.local.book().activity.extend(found);
}

fn is_activity(kind: &str) -> bool {
    matches!(kind, kinds::AGENT_PROMPT_SUBMITTED | kinds::AGENT_TURN_STARTED | kinds::AGENT_TURN_ENDED)
}

// ------------------------------------------------------------------ inputs

fn on_event(d: &Arc<Daemon>, e: &Event) {
    if let Some(sid) = &e.session_id {
        if is_activity(&e.kind)
            && let Some(at) = time::parse_rfc3339(&e.at)
        {
            d.local.book().activity.insert(sid.clone(), at);
        }
        clear_if_due(d, sid, e);
        if matches!(e.kind.as_str(), kinds::SESSION_CLOSED | kinds::SESSION_EXITED) {
            let mut b = d.local.book();
            b.activity.remove(sid);
            b.status_seq.remove(sid);
        }
    }
    // What a trigger did (and trigger bookkeeping) never fires triggers.
    if e.actor.kind == ActorKind::Trigger || e.kind.starts_with("trigger.") {
        return;
    }
    fire_matching(d, &e.kind, e.session_id.as_deref(), None, &e.data);
}

fn on_hook(d: &Arc<Daemon>, sid: &str, agent: AgentKind, event: &str, payload: &Value) {
    fire_matching(d, event, Some(sid), Some(agent), payload);
}

/// Tests: move a terminal's idle clock back `secs`, then check idle triggers now.
#[doc(hidden)]
pub fn backdate_activity(d: &Arc<Daemon>, sid: &str, secs: i64) {
    d.local.book().activity.insert(sid.to_string(), time::now_unix() - secs);
    idle_tick(d);
}

fn idle_tick(d: &Arc<Daemon>) {
    let triggers: Vec<Trigger> = d.core().state.triggers.iter().filter(|t| is_live(t) && t.event.trim().eq_ignore_ascii_case("idle")).cloned().collect();
    if triggers.is_empty() {
        return;
    }
    let now = time::now_unix();
    let sessions: Vec<(SessionFacts, bool)> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter(|s| core.rt.contains_key(&s.id))
            .map(|s| {
                let busy = matches!(s.status.state, StatusState::Working | StatusState::NeedsYou) || core.agents.get(&s.id).is_some_and(|a| a.in_turn);
                (SessionFacts::of(s), busy)
            })
            .collect()
    };
    for t in &triggers {
        let Some(mins) = t.filter.idle_minutes.filter(|m| *m > 0) else { continue };
        for (s, busy) in &sessions {
            let last = d.local.book().activity.get(&s.id).copied();
            let sending = crate::queue::pending(d, &s.id);
            let Some(last) = last else { continue };
            let idle = now - last;
            if *busy || sending || idle < i64::from(mins) * 60 {
                continue;
            }
            let key = (t.id.clone(), s.id.clone());
            if d.local.book().idle_fired.get(&key) == Some(&last) {
                continue;
            }
            let data = json!({ "idle_minutes": idle / 60, "idle_secs": idle, "last_activity_at": time::format_unix(last) });
            let ev = evaluate(t, "idle", Some(s), &data);
            if !ev.fires() {
                continue;
            }
            d.local.book().idle_fired.insert(key, last);
            fire(d, t, "idle", Some(s), &data, ev.line);
        }
    }
}

fn schedule_tick(d: &Arc<Daemon>) {
    let minute = time::now_unix().div_euclid(60) * 60;
    // The first check after a start covers the current minute only.
    let prev = d.local.book().schedule_checked.replace(minute).unwrap_or(minute - 60);
    if prev >= minute {
        return;
    }
    let triggers: Vec<Trigger> = d.core().state.triggers.iter().filter(|t| is_live(t) && t.event.trim().eq_ignore_ascii_case("schedule")).cloned().collect();
    for (t, at) in due(&triggers, prev, minute) {
        fire_schedule(d, &t, at);
    }
}

/// Schedule triggers due after minute `prev` up to and including `minute`, each once, at its
/// latest matching minute (looking back at most `SCHEDULE_CATCH_UP_SECS`). The window, start,
/// end and run limit count as well as the cron.
pub fn due(triggers: &[Trigger], prev: i64, minute: i64) -> Vec<(Trigger, i64)> {
    let from = (prev + 60).max(minute - SCHEDULE_CATCH_UP_SECS);
    let mut out = vec![];
    for t in triggers {
        let Ok(c) = cron::Schedule::of(&t.filter, t.fired) else { continue };
        let mut at = minute;
        while at >= from {
            if c.matches(at) {
                out.push((t.clone(), at));
                break;
            }
            at -= 60;
        }
    }
    out
}

/// Tests: fire schedule trigger `id` as if its minute `at` came round.
#[doc(hidden)]
pub fn fire_schedule_now(d: &Arc<Daemon>, id: &str, at: i64) {
    let t = d.core().state.triggers.iter().find(|t| t.id == id).cloned();
    if let Some(t) = t {
        fire_schedule(d, &t, at);
    }
}

pub fn schedule_data(cron: &str, at: i64) -> Value {
    let (_, _, _, h, mi, _) = time::local_parts(at);
    json!({ "cron": cron, "scheduled_for": time::format_unix(at), "local_time": format!("{h:02}:{mi:02}") })
}

/// A schedule with a session, project or agent filter acts on every running terminal that
/// matches; without one it fires once, about no terminal.
fn fire_schedule(d: &Arc<Daemon>, t: &Trigger, at: i64) {
    let data = schedule_data(t.filter.cron.as_deref().unwrap_or_default(), at);
    let f = &t.filter;
    if f.session.is_none() && f.project.is_none() && f.agent.is_none() {
        let ev = evaluate(t, "schedule", None, &data);
        if ev.fires() {
            fire(d, t, "schedule", None, &data, ev.line);
        }
        return;
    }
    let sessions: Vec<SessionFacts> = {
        let core = d.core();
        core.state.sessions.iter().filter(|s| core.rt.contains_key(&s.id)).map(SessionFacts::of).collect()
    };
    for s in &sessions {
        let ev = evaluate(t, "schedule", Some(s), &data);
        if ev.fires() {
            fire(d, t, "schedule", Some(s), &data, ev.line);
        }
    }
}

// ------------------------------------------------------------------ matching

/// The terminal an event is about, as filters and templates see it.
#[derive(Clone, Debug)]
pub struct SessionFacts {
    pub id: Id,
    pub name: String,
    pub project_id: Id,
    pub agent: Option<AgentKind>,
    pub status: StatusState,
}

impl SessionFacts {
    fn of(s: &Session) -> SessionFacts {
        SessionFacts { id: s.id.clone(), name: s.name.clone(), project_id: s.project_id.clone(), agent: s.agent, status: s.status.state }
    }

    fn lookup(d: &Daemon, sid: &str) -> Option<SessionFacts> {
        d.core().state.session(sid).map(SessionFacts::of)
    }
}

fn is_live(t: &Trigger) -> bool {
    t.source == TriggerSource::Local && t.enabled && t.state == TriggerState::Active
}

/// How one local trigger evaluated against one event.
#[derive(Clone, Debug, PartialEq)]
pub struct Eval {
    pub event_ok: bool,
    pub filters_ok: bool,
    /// e.g. `Auto-compact: event agent.prompt_blocked ✓ · message "Compact first…" ✓`.
    pub line: String,
}

impl Eval {
    pub fn fires(&self) -> bool {
        self.event_ok && self.filters_ok
    }
}

/// Does `t` match `event` about terminal `s` with `data` (hook payload or event data)?
pub fn evaluate(t: &Trigger, event: &str, s: Option<&SessionFacts>, data: &Value) -> Eval {
    let mut parts = vec![];
    let event_ok = glob_ci(t.event.trim(), event);
    parts.push(if event_ok { format!("event {event} ✓") } else { format!("event {event} ≠ {}", t.event) });
    let mut ok = true;
    let f = &t.filter;
    let mut check = |name: &str, want: &Option<String>, got: Option<&str>| {
        if let Some(w) = want {
            let pass = got.is_some_and(|g| glob_ci(w, g));
            ok &= pass;
            parts.push(match got {
                Some(g) if pass => format!("{name} {g} ✓"),
                Some(g) => format!("{name} {g} ≠ {w}"),
                None => format!("{name} missing (wants {w})"),
            });
        }
    };
    check("session", &f.session, s.map(|s| s.id.as_str()));
    check("project", &f.project, s.map(|s| s.project_id.as_str()));
    check("agent", &f.agent.map(|a| a.as_str().to_string()), s.and_then(|s| s.agent).map(|a| a.as_str()));
    for (path, want) in &f.fields {
        let got = lookup(data, path);
        let pass = got.as_deref().is_some_and(|g| glob_ci(want, g));
        ok &= pass;
        let shown = got.as_deref().map(|g| clip(g, 60));
        parts.push(match shown {
            Some(g) if pass => format!("{path} \"{g}\" ✓"),
            Some(g) => format!("{path} \"{g}\" ≠ \"{want}\""),
            None => format!("{path} missing (wants \"{want}\")"),
        });
    }
    Eval { event_ok, filters_ok: ok, line: format!("{}: {}", t.name, parts.join(" · ")) }
}

/// A dotted path into JSON (`tool_input.command`, `items.0.name`) as text.
pub fn lookup(v: &Value, path: &str) -> Option<String> {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = match cur {
            Value::Object(m) => m.get(seg)?,
            Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    match cur {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => Some(other.to_string()),
    }
}

fn clip(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n { one } else { format!("{}…", one.chars().take(n).collect::<String>()) }
}

/// `{{last_prompt}}`, `{{event}}`, `{{session.id|name|project_id|agent|status}}`,
/// `{{data.<path>}}` or a bare `{{<path>}}` into the data. `quote` shell-quotes every value.
pub fn render(tpl: &str, event: &str, s: Option<&SessionFacts>, data: &Value, last_prompt: Option<&str>, quote: bool) -> String {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else {
            out.push_str(&rest[i..]);
            return out;
        };
        let key = after[..j].trim();
        let val = match key {
            "last_prompt" => last_prompt.map(str::to_string),
            "event" => Some(event.to_string()),
            "session.id" => s.map(|s| s.id.clone()),
            "session.name" => s.map(|s| s.name.clone()),
            "session.project_id" => s.map(|s| s.project_id.clone()),
            "session.agent" => s.and_then(|s| s.agent).map(|a| a.as_str().to_string()),
            "session.status" => s.map(|s| s.status.as_str().to_string()),
            k => lookup(data, k.strip_prefix("data.").unwrap_or(k)),
        }
        .unwrap_or_default();
        out.push_str(&if quote { shell_quote(&val) } else { val });
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    out
}

fn uses_last_prompt(t: &Trigger) -> bool {
    match &t.action {
        TriggerAction::SendToSession { steps } => steps.iter().any(|s| s.text.contains("last_prompt")),
        TriggerAction::StartAgent { prompt_template, .. } => prompt_template.contains("last_prompt"),
        _ => false,
    }
}

/// The terminal's most recent prompt, in full.
pub fn last_prompt(d: &Daemon, sid: &str) -> Option<String> {
    let filter = EventFilter { kinds: Some(vec![kinds::AGENT_PROMPT_SUBMITTED.into()]), session_id: Some(sid.into()), project_id: None };
    d.log.list(0, 1, &filter).pop().and_then(|e| e.data.get("prompt").and_then(Value::as_str).map(str::to_string)).filter(|p| !p.trim().is_empty())
}

/// `agent`: who sent the hook (an agent run by hand in a shell terminal has no session agent).
fn fire_matching(d: &Arc<Daemon>, event: &str, sid: Option<&str>, agent: Option<AgentKind>, data: &Value) {
    let candidates: Vec<Trigger> = d.core().state.triggers.iter().filter(|t| is_live(t) && glob_ci(t.event.trim(), event)).cloned().collect();
    if candidates.is_empty() {
        return;
    }
    let s = sid.and_then(|sid| SessionFacts::lookup(d, sid)).map(|mut s| {
        s.agent = s.agent.or(agent);
        s
    });
    for t in &candidates {
        let ev = evaluate(t, event, s.as_ref(), data);
        if !ev.fires() {
            continue;
        }
        if let Some(s) = &s {
            let cooldown = Duration::from_secs(t.cooldown_secs.unwrap_or(DEFAULT_COOLDOWN_SECS));
            let key = (t.id.clone(), s.id.clone());
            let mut b = d.local.book();
            if b.fired.get(&key).is_some_and(|at| at.elapsed() < cooldown) {
                continue;
            }
            b.fired.insert(key, Instant::now());
        }
        fire(d, t, event, s.as_ref(), data, ev.line);
    }
}

// ------------------------------------------------------------------ firing

fn actor_of(t: &Trigger) -> Actor {
    crate::webhooks::process::trigger_actor(t)
}

/// Where a `run_command` runs, for dry runs: "" (a tab), " in Background", " with no terminal".
pub fn run_where(background: bool, headless: bool) -> &'static str {
    match (background, headless) {
        (_, true) => " with no terminal",
        (true, _) => " in Background",
        _ => "",
    }
}

pub fn action_name(a: &TriggerAction) -> &'static str {
    match a {
        TriggerAction::StartAgent { .. } => "start_agent",
        TriggerAction::RunCommand { .. } => "run_command",
        TriggerAction::Attention { .. } => "attention",
        TriggerAction::SendToSession { .. } => "send_to_session",
        TriggerAction::SetStatus { .. } => "set_status",
        TriggerAction::ClearStatus {} => "clear_status",
        TriggerAction::Notify { .. } => "notify",
    }
}

/// Run `t` once, now, as if it matched `event` about terminal `sid` (no filters, no cooldown):
/// a `notify.send` `on` action. Returns (delivery id, one-line outcome).
pub fn fire_once(d: &Arc<Daemon>, t: &Trigger, event: &str, sid: Option<&str>, data: &Value) -> (Id, String) {
    let s = sid.and_then(|sid| SessionFacts::lookup(d, sid));
    let by = &t.created_by;
    let by = match (by.kind, by.session.as_deref(), by.name.as_deref()) {
        (ActorKind::Human, ..) => "the human".to_string(),
        (_, Some(sid), _) => format!("{} in terminal {sid}", serde_json::to_value(by.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()),
        (_, None, Some(n)) => n.to_string(),
        _ => "a process outside midna".to_string(),
    };
    let line = format!("{}: response ✓ · sent by {by}", t.name);
    fire(d, t, event, s.as_ref(), data, line)
}

fn fire(d: &Arc<Daemon>, t: &Trigger, event: &str, s: Option<&SessionFacts>, data: &Value, line: String) -> (Id, String) {
    let id = format!("d_{}", hex_id(6));
    let mut runs = vec![];
    let (started, outcome) = run(d, t, event, s, data, &id, &mut runs);
    let summary = s.map(|s| s.name.clone());
    {
        let mut core = d.core();
        if let Some(tt) = core.state.triggers.iter_mut().find(|x| x.id == t.id) {
            tt.fired += 1;
            tt.last_fired_at = Some(time::now_rfc3339());
            tt.last_fired_summary = summary.clone();
        }
    }
    d.mark_dirty();
    let del = Delivery {
        id,
        source: TriggerSource::Local,
        event: event.to_string(),
        delivery_guid: format!("local-{}", hex_id(8)),
        received_at: time::now_rfc3339(),
        verdict: Verdict::Verified,
        trigger_id: Some(t.id.clone()),
        session_started: started.clone(),
        summary: outcome.clone(),
        action: None,
        repo: None,
        subject: summary,
        http_status: None,
        eval: vec![line],
        triggers_fired: vec![t.id.clone()],
        sessions_started: started.clone().into_iter().collect(),
        recovered: false,
        replay_of: None,
        body_sha256: None,
        command_runs: runs,
    };
    let project = t.action.project_id().cloned().or_else(|| s.map(|s| s.project_id.clone()));
    d.emit(
        kinds::TRIGGER_FIRED,
        actor_of(t),
        project,
        s.map(|s| s.id.clone()).or(started.clone()),
        json!({ "trigger_id": t.id, "delivery_id": del.id, "session_id": started, "outcome": outcome, "event": event, "local": true }),
    );
    crate::webhooks::process::record(d, &del, None);
    (del.id, outcome)
}

/// Run one local trigger's action for delivery `del_id`. Returns (terminal started, one-line
/// outcome); a headless command's run goes in `runs`.
fn run(d: &Arc<Daemon>, t: &Trigger, event: &str, s: Option<&SessionFacts>, data: &Value, del_id: &str, runs: &mut Vec<CommandRun>) -> (Option<Id>, String) {
    let actor = actor_of(t);
    let prompt = if uses_last_prompt(t) { s.and_then(|s| last_prompt(d, &s.id)) } else { None };
    let r = |tpl: &str, quote: bool| render(tpl, event, s, data, prompt.as_deref(), quote);
    let target = || s.ok_or_else(|| format!("{}: event {event} isn't about a terminal", action_name(&t.action)));
    let open = |params: Value| -> Result<Id, String> {
        let p: SessionOpenParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
        let v = crate::rpc::session::open(d, &Ctx::internal_trigger(actor.clone()), p).map_err(|e| e.message)?;
        Ok(v["id"].as_str().unwrap_or_default().to_string())
    };
    let name = || {
        let n = r(t.session_name_template.as_deref().unwrap_or(&t.name), false);
        if n.trim().is_empty() { t.name.clone() } else { n }
    };
    let result: Result<(Option<Id>, String), String> = match &t.action {
        TriggerAction::StartAgent { project_id, agent, prompt_template } => {
            let n = name();
            open(json!({ "project_id": project_id, "kind": "agent", "agent": agent, "name": n, "prompt": r(prompt_template, false) })).map(|id| (Some(id), format!("Started {} › {n}", agent.as_str())))
        }
        TriggerAction::RunCommand { project_id, command, background, headless, timeout_secs } => {
            let n = name();
            if *headless {
                let run = crate::headless::start(d, t, del_id, project_id, &r(command, true), *timeout_secs);
                let out = match &run.error {
                    Some(e) => Err(e.clone()),
                    None => Ok((None, format!("Running with no terminal › {n}"))),
                };
                runs.push(run);
                out
            } else {
                open(json!({ "project_id": project_id, "kind": "monitor", "name": n, "command": [r(command, true)], "background": background }))
                    .map(|id| (Some(id), format!("Ran command{} › {n}", if *background { " in Background" } else { "" })))
            }
        }
        TriggerAction::Attention { message } => {
            let msg = r(message, false);
            let mut item = d.new_needs_you(NeedsYouKind::Note, if msg.trim().is_empty() { t.name.clone() } else { msg }, actor.clone(), s.map(|s| s.id.clone()));
            item.bulk_safe = true;
            item.trigger_id = Some(t.id.clone());
            let item = d.raise_needs_you(item);
            Ok((None, format!("Raised attention ({})", item.id)))
        }
        TriggerAction::SendToSession { steps } => target().and_then(|s| {
            if steps.iter().any(|x| x.text.contains("last_prompt")) && prompt.is_none() {
                return Err(format!("{} has no prompt to resend yet", s.name));
            }
            let rendered: Vec<(String, bool)> = steps.iter().map(|x| (r(&x.text, false), x.enter)).collect();
            let n = crate::queue::add_steps(d, &s.id, t, actor_of(t), rendered)?;
            Ok((None, format!("Queued {n} step{} for {}", if n == 1 { "" } else { "s" }, s.name)))
        }),
        TriggerAction::SetStatus { label, color, icon, base, clear_on } => target().map(|s| {
            let label = r(label, false);
            let detail = lookup(data, "message").map(|m| clip(&m, 300));
            let cs = CustomStatus {
                label: label.clone(),
                color: color.clone(),
                icon: icon.clone(),
                base: *base,
                clear_on: *clear_on,
                trigger_id: Some(t.id.clone()),
                detail,
                needs_you_id: None,
                since: time::now_rfc3339(),
            };
            set_custom(d, &s.id, cs, actor.clone());
            (None, format!("Status “{label}” on {}", s.name))
        }),
        TriggerAction::Notify { title, body, sound, category, open, id } => {
            let extras = crate::notify::Extras { id: id.as_deref().map(|i| r(i, false)), open: open.as_deref().map(|u| r(u, false)), actions: vec![], ..Default::default() };
            Ok((None, notify(d, t, s.map(|s| s.id.clone()), &r(title, false), &r(body, false), *sound, category.as_deref(), extras)))
        }
        TriggerAction::ClearStatus {} => target().map(|s| {
            let had = clear_custom(d, &s.id, actor.clone(), "clear_status", true);
            (None, if had { format!("Cleared the status on {}", s.name) } else { format!("{} had no custom status", s.name) })
        }),
    };
    result.unwrap_or_else(|e| (None, format!("Could not {}: {e}", action_name(&t.action).replace('_', " "))))
}

/// A trigger's `notify` action (local and webhook): one line saying what happened.
pub fn notify(d: &Daemon, t: &Trigger, session: Option<Id>, title: &str, body: &str, sound: bool, category: Option<&str>, extras: crate::notify::Extras) -> String {
    let title = if title.trim().is_empty() { t.name.as_str() } else { title };
    // A kind the human added, while it still exists; else `from_trigger`.
    let category = category.filter(|k| d.core().state.notify_kind(k).is_some()).unwrap_or("from_trigger");
    // A rendered id or URL that doesn't check out is left off rather than losing the notification.
    let extras = crate::notify::Extras {
        id: extras.id.map(|i| i.trim().to_string()).filter(|i| midna_proto::notify::valid_id(i)),
        open: extras.open.map(|u| u.trim().to_string()).filter(|u| midna_proto::notify::valid_open_url(u)),
        actions: vec![],
        ..Default::default()
    };
    let r = crate::notify::send_as(d, category, session, title, body, sound, false, extras);
    match r.reason {
        None => format!("Notified “{}”", clip(title, 60)),
        Some(why) => format!("Notification not shown ({why})"),
    }
}

// ------------------------------------------------------------------ custom statuses

/// Put `cs` on a terminal: a needs-you item for a `needs_you` base, then the base status.
pub fn set_custom(d: &Daemon, sid: &str, mut cs: CustomStatus, actor: Actor) {
    clear_custom(d, sid, actor.clone(), "replaced", false);
    if cs.base == StatusState::NeedsYou {
        let mut item = d.new_needs_you(NeedsYouKind::Blocked, cs.label.clone(), actor.clone(), Some(sid.to_string()));
        item.detail = cs.detail.clone().unwrap_or_default();
        item.bulk_safe = true;
        item.trigger_id = cs.trigger_id.clone();
        cs.needs_you_id = Some(d.raise_needs_you(item).id);
    }
    let project = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return };
        s.custom_status = Some(cs.clone());
        s.project_id.clone()
    };
    d.mark_dirty();
    let e = d.emit(kinds::SESSION_CUSTOM_STATUS, actor.clone(), Some(project), Some(sid.to_string()), json!({ "custom_status": cs }));
    d.local.book().status_seq.insert(sid.to_string(), e.seq);
    d.set_status(sid, cs.base, Some(cs.label.clone()), None, actor);
}

/// Take a terminal's custom status off. `restore`: put a status the custom one still holds
/// back to idle (explicit clears; a prompt or turn moves the status on by itself).
pub fn clear_custom(d: &Daemon, sid: &str, actor: Actor, why: &str, restore: bool) -> bool {
    let (cs, project, holds) = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return false };
        let Some(cs) = s.custom_status.take() else { return false };
        let holds = s.status.state == cs.base && s.status.reason.as_deref() == Some(cs.label.as_str());
        (cs, s.project_id.clone(), holds)
    };
    d.mark_dirty();
    d.local.book().status_seq.remove(sid);
    if let Some(n) = &cs.needs_you_id {
        d.close_needs_you(n, json!({ "kind": "done", "auto": true, "reason": why }), actor.clone());
    }
    d.emit(kinds::SESSION_CUSTOM_STATUS, actor.clone(), Some(project), Some(sid.to_string()), json!({ "custom_status": null, "cleared": why, "label": cs.label }));
    if restore && holds && cs.base != StatusState::Idle {
        d.set_status(sid, StatusState::Idle, Some(format!("cleared “{}”", cs.label)), None, actor);
    }
    true
}

fn clear_if_due(d: &Daemon, sid: &str, e: &Event) {
    let Some(cs) = d.core().state.session(sid).and_then(|s| s.custom_status.clone()) else { return };
    if e.seq <= d.local.book().status_seq.get(sid).copied().unwrap_or(0) {
        return; // happened before the status was set
    }
    let resolved_ours = e.kind == kinds::NEEDS_YOU_RESOLVED && cs.needs_you_id.is_some() && e.data.get("id").and_then(Value::as_str) == cs.needs_you_id.as_deref();
    let why = match e.kind.as_str() {
        kinds::SESSION_EXITED | kinds::SESSION_CLOSED => Some("exited"),
        _ if resolved_ours && e.actor.kind != ActorKind::Trigger => Some("dismissed"),
        kinds::AGENT_PROMPT_SUBMITTED if cs.clear_on == StatusClear::Prompt => Some("prompt"),
        kinds::AGENT_TURN_STARTED if matches!(cs.clear_on, StatusClear::Prompt | StatusClear::Turn) => Some("turn"),
        kinds::SESSION_STATUS if cs.clear_on == StatusClear::Status && e.data.get("state").and_then(|s| serde_json::from_value::<StatusState>(s.clone()).ok()) != Some(cs.base) => {
            Some("status")
        }
        _ => None,
    };
    if let Some(why) = why {
        clear_custom(d, sid, Actor::system(), why, why == "dismissed");
    }
}

// ------------------------------------------------------------------ refused prompts

/// A Claude transcript entry saying a UserPromptSubmit hook refused the prompt: its content.
pub fn blocked_notice(v: &Value) -> Option<String> {
    if v.get("type").and_then(Value::as_str) != Some("system") {
        return None;
    }
    let content = v.get("content").and_then(Value::as_str)?;
    if !content.starts_with(BLOCKED_PREFIX) {
        return None;
    }
    // A first read of an old transcript replays history; only fresh warnings count.
    let fresh = v.get("timestamp").and_then(Value::as_str).and_then(time::parse_rfc3339).is_none_or(|at| time::now_unix() - at <= BLOCKED_FRESH_SECS);
    fresh.then(|| content.to_string())
}

/// What a refusal warning says.
#[derive(Debug, PartialEq)]
pub struct Blocked {
    pub hook: Option<String>,
    pub message: String,
    /// Interactive sessions append the refused prompt (`claude -p` doesn't).
    pub prompt: Option<String>,
}

/// `UserPromptSubmit operation blocked by hook:\n[<hook>]: <stderr>[\n\n\nOriginal prompt: <prompt>]`
/// (Claude Code 2.1.289; see tests/fixtures/claude-2.1.289-prompt-blocked.jsonl).
pub fn parse_blocked(content: &str) -> Blocked {
    let mut rest = content.split_once('\n').map(|(_, r)| r).unwrap_or("");
    let mut prompt = None;
    if let Some(i) = rest.rfind("\n\nOriginal prompt: ") {
        prompt = Some(rest[i + "\n\nOriginal prompt: ".len()..].to_string()).filter(|p| !p.trim().is_empty());
        rest = &rest[..i];
    }
    let rest = rest.trim();
    if let Some(inner) = rest.strip_prefix('[')
        && let Some((hook, msg)) = inner.split_once("]: ")
    {
        return Blocked { hook: Some(hook.to_string()), message: msg.trim().to_string(), prompt };
    }
    Blocked { hook: None, message: rest.to_string(), prompt }
}

/// The transcript says a hook refused the terminal's last prompt.
pub fn prompt_blocked(d: &Arc<Daemon>, sid: &str, content: &str) {
    let Blocked { hook, message, prompt } = parse_blocked(content);
    let Some((project, agent)) = d.core().state.session(sid).map(|s| (s.project_id.clone(), s.agent)) else { return };
    crate::rpc::agent::end_turn_if_open(d, sid, "prompt blocked");
    d.set_status(sid, StatusState::Idle, Some("prompt blocked by a hook".into()), None, Actor::system());
    let actor = Actor { kind: ActorKind::Agent, session: Some(sid.to_string()), name: agent.map(|a| a.as_str().to_string()) };
    let prompt = prompt.or_else(|| last_prompt(d, sid));
    d.emit(kinds::AGENT_PROMPT_BLOCKED, actor, Some(project), Some(sid.to_string()), json!({ "hook": hook, "message": message, "prompt": prompt }));
}

/// No transcript warning, but nothing at all followed the prompt and the title says the agent
/// is stopped: end the turn so the terminal doesn't read "working" forever. No
/// `agent.prompt_blocked` here, since a slow hook looks the same and nothing should be resent.
fn watch_silent_prompt(d: &Arc<Daemon>, sid: &str, seen: u64) {
    let (d, sid) = (d.clone(), sid.to_string());
    let _ = std::thread::Builder::new().name("silent-prompt".into()).spawn(move || {
        std::thread::sleep(SILENT_PROMPT);
        crate::links::read_transcript(&d, &sid);
        if d.local.book().hooks.get(&sid).copied() != Some(seen) {
            return;
        }
        let stuck = {
            let core = d.core();
            let Some(s) = core.state.session(&sid) else { return };
            let stopped = s.agent.is_some_and(|a| crate::agent_state::agent_title_hint(a, &s.title) == crate::agent_state::TitleHint::Stopped);
            stopped && s.status.state == StatusState::Working && core.agents.get(&sid).is_some_and(|a| a.in_turn)
        };
        if stuck {
            crate::rpc::agent::end_turn_if_open(&d, &sid, "no turn after the prompt");
            d.set_status(&sid, StatusState::Idle, Some("no turn started after the prompt".into()), None, Actor::system());
        }
    });
}

// ------------------------------------------------------------------ dry run

/// `trigger.test` for a local trigger: how it evaluates, and what it would do. Nothing runs.
pub fn dry_run(d: &Daemon, t: &Trigger, event: &str, sid: Option<&str>, data: &Value) -> Delivery {
    let s = sid.and_then(|sid| SessionFacts::lookup(d, sid));
    // A schedule: test against its next run, and say when the next few are.
    let mut next = String::new();
    let mut sched = None;
    if let Some(c) = t.filter.cron.as_deref().filter(|_| event.eq_ignore_ascii_case("schedule")) {
        match cron::Schedule::of(&t.filter, t.fired) {
            Ok(sc) => {
                let runs = sc.upcoming(time::now_unix(), 3);
                if let Some(first) = runs.first().filter(|_| data.get("scheduled_for").is_none()) {
                    sched = Some(schedule_data(c, *first));
                }
                let shown: Vec<String> = runs.iter().map(|r| cron::local_label(*r)).collect();
                next = if shown.is_empty() { " · never runs again".into() } else { format!(" · next: {}", shown.join(", ")) };
            }
            Err(e) => next = format!(" · {e}"),
        }
    }
    let data = sched.as_ref().unwrap_or(data);
    let ev = evaluate(t, event, s.as_ref(), data);
    // Without a terminal (or before its first prompt) show where the prompt would go.
    let prompt = s.as_ref().and_then(|s| last_prompt(d, &s.id)).unwrap_or_else(|| "‹last prompt›".into());
    let r = |tpl: &str, quote: bool| render(tpl, event, s.as_ref(), data, Some(&prompt), quote);
    let on = s.as_ref().map(|s| format!(" on {}", s.name)).unwrap_or_default();
    let would = match &t.action {
        TriggerAction::StartAgent { agent, prompt_template, .. } => format!("would start {} with prompt: {}", agent.as_str(), r(prompt_template, false)),
        TriggerAction::RunCommand { command, background, headless, .. } => format!("would run{}: {}", run_where(*background, *headless), r(command, true)),
        TriggerAction::Attention { message } => format!("would raise attention: {}", r(message, false)),
        TriggerAction::SendToSession { steps } => {
            let shown: Vec<String> = steps.iter().map(|x| format!("“{}”", clip(&r(&x.text, false), 80))).collect();
            format!("would send{on}: {}", shown.join(" → "))
        }
        TriggerAction::SetStatus { label, color, base, clear_on, .. } => format!("would set status{on}: “{}” ({color}, {}, clears on {clear_on:?})", r(label, false), base.as_str()),
        TriggerAction::ClearStatus {} => format!("would clear the status{on}"),
        TriggerAction::Notify { title, body, .. } => format!("would notify{on}: {}", [r(title, false), r(body, false)].iter().filter(|x| !x.trim().is_empty()).cloned().collect::<Vec<_>>().join(" — ")),
    };
    let fires = ev.fires();
    let enabled = is_live(t);
    Delivery {
        id: "d_test".into(),
        source: TriggerSource::Local,
        event: event.to_string(),
        delivery_guid: "test".into(),
        received_at: time::now_rfc3339(),
        verdict: if fires { Verdict::Verified } else { Verdict::Filtered },
        trigger_id: Some(t.id.clone()),
        session_started: None,
        summary: match (fires, enabled) {
            (true, true) => format!("Matches · {would}{next}"),
            (true, false) => format!("Matches, but the trigger isn't enabled · {would}{next}"),
            _ => format!("Doesn't match · {}", ev.line.split_once(": ").map(|(_, w)| w).unwrap_or(&ev.line)),
        },
        action: None,
        repo: None,
        subject: s.map(|s| s.name),
        http_status: None,
        eval: vec![ev.line],
        triggers_fired: vec![],
        sessions_started: vec![],
        recovered: false,
        replay_of: None,
        body_sha256: None,
        command_runs: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trig(event: &str, filter: TriggerFilter) -> Trigger {
        Trigger {
            id: "t_1".into(),
            name: "Auto-compact".into(),
            source: TriggerSource::Local,
            event: event.into(),
            filter,
            action: TriggerAction::SendToSession { steps: vec![SendStep { text: "/compact".into(), enter: true }, SendStep { text: "{{last_prompt}}".into(), enter: true }] },
            enabled: true,
            state: TriggerState::Active,
            secret_set: false,
            created_by: Actor::human(),
            created_at: String::new(),
            last_fired_at: None,
            fired: 0,
            last_fired_summary: None,
            enabled_at: None,
            secret_set_at: None,
            secret_store: None,
            github_hook_id: None,
            session_name_template: None,
            cooldown_secs: None,
            builtin: None,
        }
    }

    fn sess() -> SessionFacts {
        SessionFacts { id: "abcd1234".into(), name: "api".into(), project_id: "p_1".into(), agent: Some(AgentKind::Claude), status: StatusState::Idle }
    }

    #[test]
    fn matches_event_session_and_fields() {
        let mut f = TriggerFilter { agent: Some(AgentKind::Claude), ..Default::default() };
        f.fields.insert("message".into(), "*compact first*".into());
        let t = trig("agent.prompt_blocked", f);
        let data = json!({ "hook": "/x/guard", "message": "Compact first: prompt is 80k tokens and the cache is cold" });
        let e = evaluate(&t, "agent.prompt_blocked", Some(&sess()), &data);
        assert!(e.fires(), "{}", e.line);
        assert!(!evaluate(&t, "agent.turn_ended", Some(&sess()), &data).fires());
        let e = evaluate(&t, "agent.prompt_blocked", Some(&sess()), &json!({ "message": "too long" }));
        assert!(!e.fires());
        assert!(e.line.contains("≠"), "{}", e.line);
        assert!(!evaluate(&t, "agent.prompt_blocked", None, &data).fires(), "agent filter needs a terminal");
    }

    #[test]
    fn hook_globs_and_nested_paths() {
        let mut f = TriggerFilter::default();
        f.fields.insert("tool_input.command".into(), "git push*".into());
        let t = trig("hook.*ToolUse", f);
        let data = json!({ "tool_name": "Bash", "tool_input": { "command": "git push origin main" } });
        assert!(evaluate(&t, "hook.PreToolUse", Some(&sess()), &data).fires());
        assert!(!evaluate(&t, "hook.Stop", Some(&sess()), &data).fires());
        assert_eq!(lookup(&json!({ "a": [{ "b": 3 }] }), "a.0.b").as_deref(), Some("3"));
        assert_eq!(lookup(&json!({ "a": null }), "a"), None);
    }

    #[test]
    fn renders_templates() {
        let data = json!({ "message": "Compact first", "nested": { "n": 2 } });
        let out = render("{{session.name}} {{event}} {{message}} {{data.nested.n}} {{missing}}|{{last_prompt}}", "agent.prompt_blocked", Some(&sess()), &data, Some("fix the bug"), false);
        assert_eq!(out, "api agent.prompt_blocked Compact first 2 |fix the bug");
        assert_eq!(render("echo {{message}}", "e", None, &json!({ "message": "a'b; rm" }), None, true), r"echo 'a'\''b; rm'");
        assert_eq!(render("open {{ unterminated", "e", None, &data, None, false), "open {{ unterminated");
    }

    #[test]
    fn schedules_come_due_once_with_bounded_catch_up() {
        let mut t = trig("schedule", TriggerFilter { cron: Some("*/5 * * * *".into()), ..Default::default() });
        // A local minute that's a multiple of 5 (whole-hour offsets keep minutes aligned).
        let m = time::parse_rfc3339("2026-10-05T12:00:00Z").unwrap();
        let m = m + i64::from(60 - time::local_parts(m).4) * 60 % 3600;
        assert_eq!(due(&[t.clone()], m - 60, m), vec![(t.clone(), m)]);
        assert!(due(&[t.clone()], m, m + 4 * 60).is_empty(), "nothing between runs");
        // Asleep for 12 minutes: fires once, for the latest missed minute.
        assert_eq!(due(&[t.clone()], m - 60, m + 12 * 60), vec![(t.clone(), m + 10 * 60)]);
        // Asleep for an hour: only the last 10 minutes count.
        t.filter.cron = Some("0 * * * *".into());
        let hour = m - i64::from(time::local_parts(m).4) * 60;
        assert!(due(&[t.clone()], hour - 60, hour + 30 * 60).is_empty());
        assert_eq!(due(&[t.clone()], hour - 60, hour + 9 * 60), vec![(t.clone(), hour)]);
        // A run limit that's used up, or a window the minute is outside, holds it back.
        t.filter.max_runs = Some(1);
        t.fired = 1;
        assert!(due(&[t.clone()], hour - 60, hour).is_empty());
        t.filter.max_runs = None;
        let (_, _, _, h, ..) = time::local_parts(hour);
        t.filter.window = Some(TimeWindow { from: format!("{:02}:00", (h + 1) % 24), until: format!("{:02}:00", (h + 2) % 24) });
        assert!(due(&[t.clone()], hour - 60, hour).is_empty());
        t.filter.window = Some(TimeWindow { from: format!("{h:02}:00"), until: format!("{:02}:00", (h + 1) % 24) });
        assert_eq!(due(&[t.clone()], hour - 60, hour), vec![(t.clone(), hour)]);
        // `@every 55m` from its start: due at +0, +55 and +110, not at :00 of the next hour.
        t.filter = TriggerFilter { cron: Some("@every 55m".into()), starts_at: Some(time::format_unix(hour)), ..Default::default() };
        assert_eq!(due(&[t.clone()], hour - 60, hour), vec![(t.clone(), hour)]);
        assert!(due(&[t.clone()], hour, hour + 54 * 60).is_empty() && due(&[t.clone()], hour + 55 * 60, hour + 60 * 60).is_empty());
        assert_eq!(due(&[t.clone()], hour + 54 * 60, hour + 55 * 60), vec![(t.clone(), hour + 55 * 60)]);
        assert_eq!(due(&[t.clone()], hour + 109 * 60, hour + 110 * 60), vec![(t.clone(), hour + 110 * 60)]);
    }

    #[test]
    fn parses_blocked_notices() {
        let content = "UserPromptSubmit operation blocked by hook:\n[/Users/me/.local/bin/claude-usage-guard]: Compact first: 92k tokens, cache cold";
        let guard = Some("/Users/me/.local/bin/claude-usage-guard".to_string());
        assert_eq!(parse_blocked(content), Blocked { hook: guard.clone(), message: "Compact first: 92k tokens, cache cold".into(), prompt: None });
        // An interactive session's entry, as Claude Code 2.1.289 wrote it.
        let real: Value = serde_json::from_str(include_str!("../tests/fixtures/claude-2.1.289-prompt-blocked.jsonl")).unwrap();
        let mut real_now = real.clone();
        real_now["timestamp"] = json!(time::now_rfc3339());
        let b = parse_blocked(&blocked_notice(&real_now).expect("recognised"));
        assert_eq!(b, Blocked { hook: guard, message: "Compact first: test refusal from midna".into(), prompt: Some("BLOCKME please summarize".into()) });
        let mut stale = real.clone();
        stale["timestamp"] = json!("2026-01-01T00:00:00Z");
        assert_eq!(blocked_notice(&stale), None, "history isn't a new refusal");
        let now = time::now_rfc3339();
        let v = json!({ "type": "system", "subtype": "informational", "level": "warning", "content": content, "timestamp": now });
        assert_eq!(blocked_notice(&v).as_deref(), Some(content));
        let old = json!({ "type": "system", "content": content, "timestamp": "2020-01-01T00:00:00Z" });
        assert_eq!(blocked_notice(&old), None);
        assert_eq!(blocked_notice(&json!({ "type": "user", "content": content })), None);
    }
}
