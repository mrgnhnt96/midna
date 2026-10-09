//! The notifier: turns daemon events into `notify.posted` events (see `midna_proto::notify`).
//!
//! A thread reads the event log (`EventLog::listen`), maps each signal to a category, and
//! posts when the category is on globally (`notify.<key>`) and for that terminal
//! (`Session::notify`). The app shows `via: app` notifications (and skips the terminal you're
//! looking at); with no app connected midnad shows them itself (`via: system`). Each carries
//! its category's sound file, volume and image (`style`, see `crate::notify_media`), and that
//! sound rendered at its volume, in `~/Library/Sounds`, for macOS to play with the banner
//! (`notify_media::banner_sound`).
//! Its title and text are midna's own unless `notify.title.<key>` / `notify.body.<key>` hold
//! a template (`texts`).
//!
//! Lock order: `core`, then `Daemon::notify`. Never emit while holding `notify`.
use crate::daemon::Daemon;
use midna_proto::notify::{self, Posted};
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The same kind of notification for the same terminal within this window is one notification
/// (e.g. a permission prompt and the approval raised for it).
const DEDUPE: Duration = Duration::from_secs(3);
/// An agent may send this many notifications per minute (`notify.send`).
const AGENT_PER_MINUTE: usize = 6;
const BODY_MAX: usize = 180;
const TITLE_MAX: usize = 80;

/// Book-keeping shared by the notifier thread and `notify.send`.
#[derive(Default)]
pub struct State {
    /// (when, dedupe key) of recent posts, oldest first.
    recent: VecDeque<(Instant, String)>,
    /// When each terminal's open turn started (unix secs, from `agent.turn_started`).
    turn_start: HashMap<Id, i64>,
    /// Each terminal's PR number and checks state as last seen.
    checks: HashMap<Id, (u64, ChecksState)>,
    /// Each agent terminal's running background task ids.
    background: HashMap<Id, HashSet<String>>,
}

/// A notification `notify.send` posted, and what the human did with it. Kept in `state.json`
/// (`notify_sent`, oldest first, at most SENT_MAX), so clicks, buttons and their `on` actions
/// still work after midnad restarts.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sent {
    pub id: String,
    pub category: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Id>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    /// What to run for each response (`notify.send`'s `on`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub on: BTreeMap<String, TriggerAction>,
    /// Who sent it (the `on` actions run on their say-so).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_by: Option<Actor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<notify::Response>,
    /// What the `on` action for `response` did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback: Option<notify::CallbackRun>,
}

/// How many `notify.send` notifications midnad remembers responses for.
pub const SENT_MAX: usize = 500;

/// What `notify.send` adds to a notification beyond its text.
#[derive(Clone, Debug, Default)]
pub struct Extras {
    /// The caller's id (None: midna makes one up).
    pub id: Option<String>,
    pub open: Option<String>,
    pub actions: Vec<String>,
    /// What to run for each response, checked (`rpc::notify::callbacks`).
    pub on: BTreeMap<String, TriggerAction>,
    /// Who sent it.
    pub sent_by: Option<Actor>,
}

/// Why a notification is not posted (None = post it).
pub fn blocked(settings: &dyn Fn(&str) -> bool, overrides: Option<&BTreeMap<String, bool>>, category: &str) -> Option<&'static str> {
    if !settings("notify.enabled") {
        return Some("disabled");
    }
    if overrides.and_then(|o| o.get("enabled")) == Some(&false) {
        return Some("muted");
    }
    let on = overrides.and_then(|o| o.get(category)).copied().unwrap_or_else(|| settings(&notify::setting_key(category)));
    (!on).then_some("category_off")
}

pub fn start(d: &Arc<Daemon>) {
    let rx = d.log.listen();
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("notify".into()).spawn(move || {
        // Ends when the log (and its sender) is dropped with the daemon.
        for e in rx {
            let Some(d) = w.upgrade() else { return };
            if !d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                on_event(&d, &e);
            }
        }
    });
}

/// A notification to consider: category, terminal, text, and an optional dedupe key.
struct Draft {
    category: String,
    session: Option<Id>,
    project: Option<Id>,
    /// midna's own text (`{{text}}`).
    body: String,
    /// The event kind behind it (`{{event}}`; empty for `notify.send` / `notify.test`).
    event: String,
    /// What a `notify.body.<category>` template can use: the event's data plus the
    /// category's own variables (`NotifyCategory::vars`).
    vars: Value,
    needs_you_id: Option<String>,
    /// `notify.send`'s `sound` (None: play the category's sound).
    sound: Option<bool>,
    key: Option<String>,
    /// `notify.test`: shown whatever the switches say, never deduped.
    test: bool,
    /// `notify.send`'s id, URL and buttons.
    extras: Extras,
}

impl Draft {
    fn new(category: &str, e: &Event, body: impl Into<String>) -> Draft {
        let vars = if e.data.is_object() { e.data.clone() } else { json!({}) };
        Draft {
            category: category.into(),
            session: e.session_id.clone(),
            project: e.project_id.clone(),
            body: body.into(),
            event: e.kind.clone(),
            vars,
            needs_you_id: None,
            sound: None,
            key: None,
            test: false,
            extras: Extras::default(),
        }
    }

    /// Add a template variable (`{{k}}`).
    fn var(mut self, k: &str, v: impl Into<Value>) -> Draft {
        if let Value::Object(m) = &mut self.vars {
            m.insert(k.into(), v.into());
        }
        self
    }
}

/// The title and text a notification shows: `notify.title.<category>` /
/// `notify.body.<category>` rendered when set, else midna's own (`heading`, `Draft::body`).
/// The text may come out empty (`none`, or a template with nothing in it); the title never
/// does.
fn texts(templates: (&str, &str), draft: &Draft, session: Option<&Session>, project: Option<&str>, heading: String) -> (String, String) {
    let (title_tpl, body_tpl) = (templates.0.trim(), templates.1.trim());
    if title_tpl.is_empty() && body_tpl.is_empty() {
        return (heading, draft.body.clone());
    }
    let mut vars = draft.vars.clone();
    if let Value::Object(m) = &mut vars {
        m.insert("heading".into(), json!(heading));
        m.insert("text".into(), json!(draft.body));
        m.insert("category".into(), json!(draft.category));
        m.insert("project".into(), json!(project.unwrap_or("")));
    }
    let facts = session.map(|s| crate::local::SessionFacts { id: s.id.clone(), name: s.name.clone(), project_id: s.project_id.clone(), agent: s.agent, status: s.status.state });
    let render = |tpl: &str| crate::local::render(tpl, &draft.event, facts.as_ref(), &vars, None, false).trim().to_string();
    let title = Some(title_tpl).filter(|t| !t.is_empty()).map(render).filter(|t| !t.is_empty()).unwrap_or_else(|| heading.clone());
    let body = match body_tpl {
        "" => draft.body.clone(),
        notify::NO_BODY => String::new(),
        tpl => render(tpl),
    };
    (title, body)
}

fn on_event(d: &Daemon, e: &Event) {
    if let Some(draft) = draft_for(d, e) {
        let _ = post(d, draft);
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Map one event to a notification draft (and keep the notifier's book-keeping).
fn draft_for(d: &Daemon, e: &Event) -> Option<Draft> {
    let sid = e.session_id.clone();
    match e.kind.as_str() {
        kinds::NEEDS_YOU_RAISED => {
            let item: NeedsYou = serde_json::from_value(e.data.clone()).ok()?;
            let category = match item.kind {
                NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt => "approval",
                NeedsYouKind::Blocked | NeedsYouKind::Note => "attention",
                NeedsYouKind::Failed => "failed",
                NeedsYouKind::TriggerWaiting | NeedsYouKind::SecretNeeded | NeedsYouKind::RuleRemoval => "requests",
            };
            let mut body = capitalize(&item.title);
            let action = item.approval.as_ref().map(|a| a.action.value.clone()).unwrap_or_default();
            if item.kind == NeedsYouKind::Approval && item.approval.is_some() {
                body = format!("Approve? {action}");
            }
            let kind = serde_json::to_value(item.kind).unwrap_or_default();
            let mut draft = Draft::new(category, e, body).var("title", capitalize(&item.title)).var("kind", kind).var("action", action);
            draft.needs_you_id = Some(item.id);
            Some(draft)
        }
        kinds::AGENT_TURN_STARTED => {
            if let Some(sid) = sid {
                d.notify().turn_start.insert(sid, time::parse_rfc3339(&e.at).unwrap_or_else(time::now_unix));
            }
            None
        }
        kinds::AGENT_TURN_ENDED => {
            let sid = sid?;
            let started = d.notify().turn_start.remove(&sid);
            // Only a turn the agent finished: not an interrupt, a dismissed prompt or an exit.
            if !matches!(s(&e.data, "reason"), "Stop" | "agent-turn-complete") {
                return None;
            }
            let secs = started.map(|t| time::parse_rfc3339(&e.at).unwrap_or_else(time::now_unix) - t).unwrap_or(0).max(0);
            let min = d.core().state.setting_i64("notify.turn_done_min_secs");
            if secs < min {
                return None;
            }
            let reply = first_line(s(&e.data, "message"));
            let took = if secs > 0 { format!("Finished in {}", duration(secs)) } else { "Finished".into() };
            let text = if reply.is_empty() { took } else { format!("{took}: {reply}") };
            Some(Draft::new("turn_done", e, text).var("elapsed", duration(secs)).var("secs", secs).var("reply", reply))
        }
        kinds::SESSION_STATUS => {
            let state = s(&e.data, "state");
            let reason = s(&e.data, "reason");
            // A turn that ended in an error (hooks report it; a failed process raises a
            // needs-you item instead).
            if state == "failed" && e.actor.kind == ActorKind::Agent {
                return Some(Draft::new("failed", e, if reason.is_empty() { "The agent's turn failed".to_string() } else { format!("Turn failed: {reason}") }));
            }
            if state == "exited" && reason == "exited" && e.actor.kind == ActorKind::System {
                return Some(Draft::new("exited", e, "Exited"));
            }
            None
        }
        kinds::SESSION_AGENT => {
            let sid = sid?;
            let info: AgentInfo = serde_json::from_value(e.data.clone()).ok()?;
            let now: HashSet<String> = info.background.iter().filter(|b| b.status.is_empty() || b.status == "running").map(|b| b.id.clone()).collect();
            let before = d.notify().background.insert(sid.clone(), now.clone()).unwrap_or_default();
            let finished: Vec<&String> = before.difference(&now).collect();
            // An exit or restart drops the list without the tasks finishing.
            let live = d.core().state.session(&sid).is_some_and(|s| s.pid.is_some() && !s.status.state.is_terminal());
            if finished.is_empty() || !live {
                return None;
            }
            // The description came with an earlier snapshot; keep it short either way.
            let what = if finished.len() == 1 { "A background task finished".to_string() } else { format!("{} background tasks finished", finished.len()) };
            Some(Draft::new("background", e, what).var("count", finished.len()))
        }
        kinds::SESSION_GIT => {
            let sid = sid?;
            let pr = e.data.get("git").and_then(|g| g.get("pr")).and_then(|p| serde_json::from_value::<PrInfo>(p.clone()).ok());
            let Some(pr) = pr else {
                d.notify().checks.remove(&sid);
                return None;
            };
            let before = d.notify().checks.insert(sid, (pr.number, pr.checks));
            let finished = before == Some((pr.number, ChecksState::Pending)) && matches!(pr.checks, ChecksState::Passing | ChecksState::Failing);
            if !finished {
                return None;
            }
            let body = match pr.checks {
                ChecksState::Failing => format!("PR #{}: {} check{} failing", pr.number, pr.failing_count.max(1), if pr.failing_count == 1 { "" } else { "s" }),
                _ => format!("PR #{}: checks passing", pr.number),
            };
            let checks = if pr.checks == ChecksState::Failing { "failing" } else { "passing" };
            let failing = if pr.checks == ChecksState::Failing { pr.failing_count.max(1) } else { 0 };
            let mut draft = Draft::new("pr_checks", e, body).var("number", pr.number).var("checks", checks).var("failing", failing);
            // Several terminals on one branch see the same PR.
            draft.key = Some(format!("pr_checks:{}:{:?}", pr.url, pr.checks));
            Some(draft)
        }
        kinds::CLEANUP_FINISHED => {
            let run: CleanupRun = serde_json::from_value(e.data.get("run")?.clone()).ok()?;
            let text = crate::cleanup::notification_text(&run)?;
            let mut draft = Draft::new("cleanup", e, text).var("terminal", run.session_name.clone()).var("removed", run.removed.join(", ")).var("kept", run.kept.join(", "));
            draft.key = Some(format!("cleanup:{}", run.id));
            Some(draft)
        }
        kinds::TRIGGER_FIRED => {
            let id = s(&e.data, "trigger_id");
            let name = d.core().state.triggers.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
            let outcome = s(&e.data, "outcome");
            let text = if outcome.is_empty() { format!("Trigger “{name}” fired") } else { format!("Trigger “{name}”: {outcome}") };
            let mut draft = Draft::new("triggers", e, text).var("name", name);
            draft.key = Some(format!("triggers:{}", s(&e.data, "delivery_id")));
            Some(draft)
        }
        kinds::SESSION_RESTARTED => {
            // A restart the human asked for needs no notification.
            if e.actor.kind == ActorKind::Human {
                return None;
            }
            let reason = s(&e.data, "reason");
            Some(Draft::new("restarted", e, if reason.is_empty() { "Restarted into the same conversation".to_string() } else { format!("Restarted: {reason}") }))
        }
        _ => None,
    }
}

/// A notification in `category` ("agent" for `notify.send`, "from_trigger" for a trigger's
/// `notify` action). `rate_limit`: at most AGENT_PER_MINUTE a minute per terminal. Remembered
/// by its id (`Sent`) so clicks and buttons can be reported back.
pub fn send_as(d: &Daemon, category: &str, session: Option<Id>, title: &str, body: &str, sound: bool, rate_limit: bool, extras: Extras) -> NotifySendResult {
    let project = session.as_deref().and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()));
    let id = extras.id.clone().unwrap_or_else(|| format!("n-{}", crate::state::hex_id(6)));
    let not = |reason: &str| NotifySendResult { posted: false, reason: Some(reason.into()), id: Some(id.clone()), ..Default::default() };
    if rate_limit && let Some(sid) = &session {
        let prefix = format!("{category}:{sid}:");
        let st = d.notify();
        let minute = Instant::now().checked_sub(Duration::from_secs(60));
        let sent = st.recent.iter().filter(|(t, k)| k.starts_with(&prefix) && minute.is_none_or(|m| *t > m)).count();
        if sent >= AGENT_PER_MINUTE {
            return not("rate_limited");
        }
    }
    let text = if body.trim().is_empty() { title.trim().to_string() } else { format!("{}\n{}", title.trim(), body.trim()) };
    let draft = Draft {
        category: category.into(),
        key: Some(format!("{category}:{}:{}:{}", session.as_deref().unwrap_or(""), extras.id.as_deref().unwrap_or(""), text)),
        session: session.clone(),
        project: project.clone(),
        body: text,
        event: String::new(),
        vars: json!({ "title": title.trim(), "body": body.trim() }),
        needs_you_id: None,
        sound: Some(sound),
        test: false,
        extras: Extras { id: Some(id.clone()), ..extras },
    };
    let (actions, on, sent_by) = (draft.extras.actions.clone(), draft.extras.on.clone(), draft.extras.sent_by.clone());
    // Remembered before it's shown, so a response can't arrive for a notification midnad
    // doesn't know yet; taken back if it isn't shown.
    let sent = Sent { id: id.clone(), category: category.into(), title: title.trim().into(), session, project, actions, on, sent_by, response: None, callback: None };
    let replaced = remember(d, sent);
    match post(d, draft) {
        Ok(p) => {
            d.mark_dirty();
            NotifySendResult { posted: true, reason: None, id: Some(id), via: Some(p.via), ..Default::default() }
        }
        Err(r) => {
            {
                let mut core = d.core();
                core.state.notify_sent.retain(|s| s.id != id);
                core.state.notify_sent.extend(replaced);
            }
            d.mark_dirty();
            not(r)
        }
    }
}

/// Keep `s` (replacing one with its id, dropping the oldest past SENT_MAX). Returns the one
/// it replaced.
fn remember(d: &Daemon, s: Sent) -> Option<Sent> {
    let mut core = d.core();
    let list = &mut core.state.notify_sent;
    let replaced = list.iter().position(|x| x.id == s.id).and_then(|i| list.remove(i));
    while list.len() >= SENT_MAX {
        list.pop_front();
    }
    list.push_back(s);
    replaced
}

/// The `notify.send` notification `id`, while midnad remembers it.
pub fn sent(d: &Daemon, id: &str) -> Option<Sent> {
    d.core().state.notify_sent.iter().find(|s| s.id == id).cloned()
}

/// Record what the human did with notification `id` (the first response counts), run its
/// `on` action for that response, and emit `notify.responded`. Err: no such notification, or
/// a button it doesn't have.
pub fn respond(d: &Arc<Daemon>, id: &str, r: notify::Response) -> Result<Sent, String> {
    let s = {
        let mut core = d.core();
        let Some(s) = core.state.notify_sent.iter_mut().find(|s| s.id == id) else { return Err(format!("no notification `{id}` (unknown, or midnad forgot it)")) };
        if r.kind == notify::ResponseKind::Action && !r.action.as_ref().is_some_and(|a| s.actions.contains(a)) {
            return Err(format!("notification `{id}` has no button {:?}; it has {:?}", r.action.as_deref().unwrap_or(""), s.actions));
        }
        if s.response.is_some() {
            return Ok(s.clone());
        }
        s.response = Some(r.clone());
        s.clone()
    };
    d.mark_dirty();
    let mut data = json!({ "id": s.id, "kind": r.kind, "action": r.action, "category": s.category, "title": s.title });
    let callback = s.on.get(r.on_key()).map(|action| run_callback(d, &s, &r, action, &data));
    let s = match callback {
        Some(cb) => {
            data["callback"] = json!(cb);
            let mut core = d.core();
            let stored = core.state.notify_sent.iter_mut().find(|x| x.id == s.id);
            stored.map(|x| {
                x.callback = Some(cb.clone());
                x.clone()
            })
            .unwrap_or(Sent { callback: Some(cb), ..s })
        }
        None => s,
    };
    d.mark_dirty();
    d.emit(kinds::NOTIFY_RESPONDED, Actor::human(), s.project.clone(), s.session.clone(), data);
    Ok(s)
}

/// Run notification `s`'s `on` action for response `r`, as a one-off local trigger named
/// after it (`notify:<id>`), so it runs, is recorded and shows in deliveries the way a local
/// trigger on `notify.responded` would.
fn run_callback(d: &Arc<Daemon>, s: &Sent, r: &notify::Response, action: &TriggerAction, data: &Value) -> notify::CallbackRun {
    let on = r.on_key().to_string();
    let t = callback_trigger(&s.id, &s.title, &on, action.clone(), s.sent_by.clone());
    let (delivery_id, outcome) = crate::local::fire_once(d, &t, kinds::NOTIFY_RESPONDED, s.session.as_deref(), data);
    notify::CallbackRun { on, delivery_id, outcome }
}

/// The one-off trigger notification `id`'s `on` action runs as (never stored with the
/// triggers).
pub fn callback_trigger(id: &str, title: &str, on: &str, action: TriggerAction, sent_by: Option<Actor>) -> Trigger {
    let title: String = title.trim().chars().take(40).collect();
    Trigger {
        id: format!("notify:{id}"),
        name: format!("Notification “{title}” › {on}"),
        source: TriggerSource::Local,
        event: kinds::NOTIFY_RESPONDED.into(),
        filter: TriggerFilter::default(),
        action,
        enabled: true,
        state: TriggerState::Active,
        secret_set: false,
        created_by: sent_by.unwrap_or_else(Actor::system),
        created_at: time::now_rfc3339(),
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

/// Wait up to `secs` for a response to notification `id` (None: none by then, or midnad
/// forgot it). With it, what its `on` action did.
pub fn wait_response(d: &Daemon, id: &str, secs: u64) -> (Option<notify::Response>, Option<notify::CallbackRun>) {
    let deadline = Instant::now() + Duration::from_secs(secs.min(600));
    loop {
        let s = sent(d, id);
        // The response is recorded before its `on` action runs: wait for both.
        let done = s.as_ref().and_then(|s| s.response.as_ref().map(|r| s.callback.is_some() || !s.on.contains_key(r.on_key())));
        if done == Some(true) || Instant::now() >= deadline {
            return s.map(|s| (s.response, s.callback)).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A test notification with a category's sound, volume and image (`notify.test`).
pub fn test(d: &Daemon, session: Option<Id>, category: &str) -> NotifySendResult {
    let label = match (notify::category(category), d.core().state.notify_kind(category)) {
        (Some(c), _) => c.label.to_string(),
        (None, Some(k)) => k.label.clone(),
        (None, None) => return NotifySendResult { posted: false, reason: Some("unknown_category".into()), ..Default::default() },
    };
    let project = session.as_deref().and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()));
    let draft = Draft {
        category: category.into(),
        session,
        project,
        body: format!("Test: {label}"),
        event: String::new(),
        vars: sample_vars(category),
        needs_you_id: None,
        sound: None,
        key: None,
        test: true,
        extras: Extras::default(),
    };
    match post(d, draft) {
        Ok(p) => NotifySendResult { posted: true, reason: None, id: None, via: Some(p.via), ..Default::default() },
        Err(r) => NotifySendResult { posted: false, reason: Some(r.into()), ..Default::default() },
    }
}

/// Made-up values for a test notification, so `notify.test` shows what a category's
/// `notify.body.<key>` template looks like filled in.
fn sample_vars(category: &str) -> Value {
    match category {
        "approval" => json!({ "title": "Run cargo test", "detail": "", "kind": "approval", "action": "cargo test --workspace" }),
        "attention" | "requests" => json!({ "title": "Which branch should I use?", "detail": "", "kind": "blocked" }),
        "failed" => json!({ "title": "Exited with status 101", "detail": "", "kind": "failed", "reason": "API error" }),
        "turn_done" => json!({ "elapsed": "2m 5s", "secs": 125, "reply": "All tests pass.", "message": "All tests pass.\nI also tidied the README." }),
        "agent" | "from_trigger" => json!({ "title": "Build is green", "body": "Ready to ship" }),
        "background" => json!({ "count": 1 }),
        "pr_checks" => json!({ "number": 42, "checks": "failing", "failing": 2 }),
        "triggers" => json!({ "name": "Review PRs", "outcome": "started an agent" }),
        "restarted" => json!({ "reason": "Claude Code updated" }),
        "cleanup" => json!({ "terminal": "Fix login", "removed": "worktree fix-login, branch fix/login, origin/fix/login", "kept": "" }),
        // A kind you added gets what was sent, like `agent`.
        _ => json!({ "title": "Deploy finished", "body": "staging is on f568837" }),
    }
}

/// A category's sound file and volume (1–100; None: silent) and image, from the settings.
pub fn style(home: &std::path::Path, setting: &dyn Fn(&str) -> Value, category: &str) -> (Option<(String, u8)>, Option<String>) {
    let str_of = |k: &str| setting(k).as_str().unwrap_or("").to_string();
    let int_of = |k: &str| setting(k).as_i64().unwrap_or(100).clamp(0, 100);
    let volume = int_of("notify.volume") * int_of(&notify::volume_key(category)) / 100;
    let sound = crate::notify_media::sound_path(home, &str_of(&notify::sound_key(category)))
        .filter(|_| volume > 0 && setting("notify.sounds") != Value::Bool(false))
        .map(|p| (p.to_string_lossy().into_owned(), volume.max(1) as u8));
    let image = match str_of(&notify::image_key(category)).as_str() {
        "" => crate::notify_media::image_path(home, &str_of("notify.image")),
        v => crate::notify_media::image_path(home, v),
    };
    (sound, image.map(|p| p.to_string_lossy().into_owned()))
}

/// Play a sound now, with no notification (`notify.play`): `what` is a kind (a notification
/// category or a sound effect: its sound at its volume) or a sound's name (at `volume`, else
/// full). Scaled by `notify.volume`; nothing plays with `notify.sounds` off. The app plays it;
/// with no app, midnad does (`afplay`) when `notify.when_app_closed` allows.
pub fn play(d: &Daemon, session: Option<Id>, what: &str, volume: Option<u8>, rate_limit: bool) -> Result<NotifyPlayResult, String> {
    let (file, volume, kind, enabled, when_app_closed) = {
        let core = d.core();
        let st = &core.state;
        let int_of = |k: &str| st.setting(k).as_i64().unwrap_or(100).clamp(0, 100);
        let (name, kind_volume, kind) = if notify::has_sound(what) {
            (st.setting_str(&notify::sound_key(what)), int_of(&notify::volume_key(what)), Some(what.to_string()))
        } else {
            (what.to_string(), 100, None)
        };
        let Some(file) = crate::notify_media::sound_path(&d.cfg.home, &name) else {
            if kind.is_some() {
                return Ok(NotifyPlayResult { played: false, sound: name, reason: Some("no_sound".into()) });
            }
            return Err(format!(
                "no sound or kind `{what}`: a kind (approval, approved, …), a Twilight sound ({}), a macOS sound ({}) or a sound from `midna notify media`",
                notify::TWILIGHT.join(", "),
                notify::SYSTEM_SOUNDS.join(", ")
            ));
        };
        let volume = int_of("notify.volume") * volume.map_or(kind_volume, i64::from).clamp(0, 100) / 100;
        ((name, file.to_string_lossy().into_owned()), volume, kind, st.setting_bool("notify.sounds"), st.setting_bool("notify.when_app_closed"))
    };
    let ((name, file), mut st) = (file, d.notify());
    let quiet = |reason: &str| Ok(NotifyPlayResult { played: false, sound: name.clone(), reason: Some(reason.into()) });
    if !enabled {
        return quiet("sounds_off");
    }
    if volume == 0 {
        return quiet("silent");
    }
    if rate_limit {
        let prefix = format!("play:{}", session.as_deref().unwrap_or(""));
        let now = Instant::now();
        while st.recent.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(60)) {
            st.recent.pop_front();
        }
        let minute = now.checked_sub(Duration::from_secs(60));
        if st.recent.iter().filter(|(t, k)| *k == prefix && minute.is_none_or(|m| *t > m)).count() >= AGENT_PER_MINUTE {
            return quiet("rate_limited");
        }
        st.recent.push_back((now, prefix));
    }
    drop(st);
    let via = if d.gui_connected() {
        "app"
    } else if when_app_closed && system_allowed(d) {
        "system"
    } else {
        "none"
    };
    let played = notify::Played { sound: name.clone(), file: file.clone(), volume: volume as u8, kind, via: via.into() };
    let project = session.as_deref().and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()));
    d.emit(kinds::NOTIFY_SOUND, Actor::system(), project, session, serde_json::to_value(&played).unwrap_or_default());
    if via == "system" {
        let v = format!("{:.2}", volume as f32 / 100.);
        let _ = std::thread::Builder::new().name("notify-afplay".into()).spawn(move || {
            let _ = std::process::Command::new("/usr/bin/afplay").args(["-v", &v, &file]).stdin(std::process::Stdio::null()).output();
        });
    }
    Ok(NotifyPlayResult { played: via != "none", sound: name, reason: (via == "none").then(|| "no_app".into()) })
}

/// Check the settings, dedupe, and emit `notify.posted` (showing it from midnad when no app
/// is connected).
fn post(d: &Daemon, draft: Draft) -> Result<Posted, &'static str> {
    let (title, text, (sound, image), (push, push_focused), when_app_closed, look) = {
        let core = d.core();
        let st = &core.state;
        let session = draft.session.as_deref().and_then(|s| st.session(s));
        if !draft.test
            && let Some(r) = blocked(&|k| st.setting_bool(k), session.map(|s| &s.notify), &draft.category)
        {
            return Err(r);
        }
        let project = draft.project.as_deref().or(session.map(|s| s.project_id.as_str())).and_then(|p| st.project(p)).map(|p| p.name.clone());
        let heading = match (session, project.as_deref()) {
            (Some(s), Some(p)) if p != s.name => format!("{} · {p}", s.name),
            (Some(s), _) => s.name.clone(),
            (None, Some(p)) => p.to_string(),
            (None, None) => "midna".into(),
        };
        let templates = (st.setting_str(&notify::title_key(&draft.category)), st.setting_str(&notify::body_key(&draft.category)));
        let (title, text) = texts((&templates.0, &templates.1), &draft, session, project.as_deref(), heading);
        // A test shows everywhere; otherwise only kinds set to push become banners.
        let push = draft.test || st.setting_bool(&notify::push_key(&draft.category));
        let push_focused = draft.test || st.setting_bool(&notify::push_focused_key(&draft.category));
        // How long it stays on screen, its color and (a kind you added) its label.
        let look = (
            st.setting(&notify::stay_key(&draft.category)).as_u64().map(|s| s.min(3600) as u32),
            Some(st.setting_str(&notify::color_key(&draft.category))).filter(|c| !c.is_empty()),
            st.notify_kind(&draft.category).map(|k| k.label.clone()),
        );
        let style = style(&d.cfg.home, &|k| st.setting(k), &draft.category);
        (title, text, style, (push, push_focused), st.setting_bool("notify.when_app_closed") || draft.test, look)
    };
    if !draft.test {
        let mut st = d.notify();
        let now = Instant::now();
        while st.recent.front().is_some_and(|(t, _)| now.duration_since(*t) > Duration::from_secs(60)) {
            st.recent.pop_front();
        }
        let key = draft.key.clone().unwrap_or_else(|| format!("{}:{}", draft.category, draft.session.as_deref().unwrap_or("")));
        // Approvals and permission prompts for the same terminal are one "approval" key.
        if st.recent.iter().any(|(t, k)| *k == key && now.duration_since(*t) < DEDUPE) {
            return Err("duplicate");
        }
        st.recent.push_back((now, key));
    }
    // An agent's notification plays its sound only when the agent asked for one.
    let sound = sound.filter(|_| draft.sound != Some(false));
    let via = if d.gui_connected() {
        "app"
    } else if push && when_app_closed && system_allowed(d) {
        "system"
    } else {
        "none"
    };
    let posted = Posted {
        category: draft.category.clone(),
        title: truncate(&title, TITLE_MAX),
        body: truncate(&text, BODY_MAX),
        sound: sound.is_some(),
        notification_sound: sound.as_ref().and_then(|(f, v)| {
            let dir = crate::notify_media::banner_dir(&d.cfg.home, system_allowed(d));
            crate::notify_media::banner_sound(&d.cfg.home, &dir, f, *v)
        }),
        volume: sound.as_ref().map(|s| s.1),
        sound_file: sound.map(|s| s.0),
        image,
        push,
        push_focused,
        test: draft.test,
        via: via.into(),
        needs_you_id: draft.needs_you_id,
        stay_secs: look.0,
        color: look.1,
        label: look.2,
        id: draft.extras.id,
        open: draft.extras.open,
        actions: draft.extras.actions,
    };
    d.emit(kinds::NOTIFY_POSTED, Actor::system(), draft.project, draft.session, serde_json::to_value(&posted).unwrap_or_default());
    if via == "system" {
        show_system(&posted);
    }
    Ok(posted)
}

/// midnad shows notifications itself only as the real daemon process, and never when
/// `MIDNA_NOTIFY_SYSTEM=0` (tests that run the binary set it).
fn system_allowed(d: &Daemon) -> bool {
    d.cfg.owns_process && std::env::var("MIDNA_NOTIFY_SYSTEM").as_deref() != Ok("0")
}

/// No app to show it: post through `osascript` (shown as a Script Editor notification, no
/// image) and play the sound with `afplay` at its volume.
fn show_system(p: &Posted) {
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!("display notification \"{}\" with title \"{}\"", q(&p.body), q(&p.title));
    let sound = p.sound_file.clone().map(|f| (f, p.volume.unwrap_or(100)));
    let _ = std::thread::Builder::new().name("notify-osascript".into()).spawn(move || {
        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).stdin(std::process::Stdio::null()).output();
        if let Some((file, volume)) = sound {
            let v = format!("{:.2}", volume as f32 / 100.);
            let _ = std::process::Command::new("/usr/bin/afplay").args(["-v", &v, &file]).stdin(std::process::Stdio::null()).output();
        }
    });
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { s.chars().take(max - 1).collect::<String>() + "…" }
}

fn duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, s % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(off: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |k: &str| {
            if off.contains(&k) {
                return false;
            }
            k.strip_prefix("notify.").and_then(notify::category).map(|c| c.default).unwrap_or(true)
        }
    }

    #[test]
    fn global_settings_then_terminal_overrides() {
        let defaults = settings(&[]);
        assert_eq!(blocked(&defaults, None, "approval"), None);
        assert_eq!(blocked(&defaults, None, "pr_checks"), Some("category_off"));
        let mut o = BTreeMap::new();
        o.insert("pr_checks".to_string(), true);
        o.insert("turn_done".to_string(), false);
        assert_eq!(blocked(&defaults, Some(&o), "pr_checks"), None);
        assert_eq!(blocked(&defaults, Some(&o), "turn_done"), Some("category_off"));
        o.insert("enabled".to_string(), false);
        assert_eq!(blocked(&defaults, Some(&o), "approval"), Some("muted"));
        assert_eq!(blocked(&settings(&["notify.enabled"]), None, "approval"), Some("disabled"));
    }

    #[test]
    fn text_helpers() {
        assert_eq!(first_line("\n  Done.  \nmore"), "Done.");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(duration(125), "2m 5s");
        assert_eq!(capitalize("permission Bash"), "Permission Bash");
    }

    fn turn_done() -> Draft {
        let data = json!({ "reason": "Stop", "message": "All done.\nmore" });
        let e = Event { seq: 1, at: "2026-10-06T12:00:00Z".into(), kind: "agent.turn_ended".into(), actor: Actor::system(), project_id: None, session_id: None, data };
        Draft::new("turn_done", &e, "Finished in 2m 5s: All done.").var("elapsed", "2m 5s").var("reply", "All done.")
    }

    fn texts_of(title: &str, body: &str, d: &Draft) -> (String, String) {
        texts((title, body), d, None, Some("midna"), "api · midna".into())
    }

    #[test]
    fn title_and_body_templates() {
        let d = turn_done();
        let own = ("api · midna".to_string(), "Finished in 2m 5s: All done.".to_string());
        assert_eq!(texts_of("", "", &d), own);
        assert_eq!(texts_of("  ", "  ", &d), own);
        assert_eq!(texts_of("", "{{reply}} ({{elapsed}}) in {{project}}", &d).1, "All done. (2m 5s) in midna");
        // The event's data, the built-in text, the category and the event kind.
        assert_eq!(texts_of("", "{{data.reason}}|{{message}}|{{category}}|{{event}}", &d).1, "Stop|All done.\nmore|turn_done|agent.turn_ended");
        assert_eq!(texts_of("", "✓ {{text}}", &d).1, "✓ Finished in 2m 5s: All done.");
        // The text may be left out: `none`, or a template that renders empty.
        assert_eq!(texts_of("", "none", &d), (own.0.clone(), String::new()));
        assert_eq!(texts_of("", "{{missing}}", &d).1, "");
        // The title never is.
        assert_eq!(texts_of("Done in {{elapsed}} ({{heading}})", "", &d), ("Done in 2m 5s (api · midna)".into(), own.1.clone()));
        assert_eq!(texts_of("{{missing}}", "none", &d), (own.0.clone(), String::new()));
    }

    #[test]
    fn every_category_has_title_and_body_settings_and_test_values() {
        for c in notify::CATEGORIES {
            assert!(midna_proto::settings::setting(&notify::body_key(c.key)).is_some(), "notify.body.{}", c.key);
            assert!(midna_proto::settings::setting(&notify::title_key(c.key)).is_some(), "notify.title.{}", c.key);
            let sample = sample_vars(c.key);
            for v in c.vars {
                assert!(sample.get(*v).is_some(), "sample_vars({}) lacks {v}", c.key);
            }
        }
    }
}
