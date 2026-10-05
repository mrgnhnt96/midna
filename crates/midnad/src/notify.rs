//! The notifier: turns daemon events into `notify.posted` events (see `midna_proto::notify`).
//!
//! A thread reads the event log (`EventLog::listen`), maps each signal to a category, and
//! posts when the category is on globally (`notify.<key>`) and for that terminal
//! (`Session::notify`). The app shows `via: app` notifications (and skips the terminal you're
//! looking at); with no app connected midnad shows them itself (`via: system`). Each carries
//! its category's sound file, volume and image (`style`, see `crate::notify_media`), and that
//! sound rendered at its volume for macOS to play with the banner (`notify_media::rendered`).
//!
//! Lock order: `core`, then `Daemon::notify`. Never emit while holding `notify`.
use crate::daemon::Daemon;
use midna_proto::notify::{self, Posted};
use midna_proto::*;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The same kind of notification for the same terminal within this window is one notification
/// (e.g. a permission prompt and the approval raised for it).
const DEDUPE: Duration = Duration::from_secs(3);
/// An agent may send this many notifications per minute (`notify.send`).
const AGENT_PER_MINUTE: usize = 6;
const BODY_MAX: usize = 180;

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
    category: &'static str,
    session: Option<Id>,
    project: Option<Id>,
    body: String,
    needs_you_id: Option<String>,
    /// `notify.send`'s `sound` (None: play the category's sound).
    sound: Option<bool>,
    key: Option<String>,
    /// `notify.test`: shown whatever the switches say, never deduped.
    test: bool,
}

impl Draft {
    fn new(category: &'static str, e: &Event, body: impl Into<String>) -> Draft {
        Draft { category, session: e.session_id.clone(), project: e.project_id.clone(), body: body.into(), needs_you_id: None, sound: None, key: None, test: false }
    }
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
            if item.kind == NeedsYouKind::Approval
                && let Some(a) = &item.approval
            {
                body = format!("Approve? {}", a.action.value);
            }
            let mut draft = Draft::new(category, e, body);
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
            Some(Draft::new("turn_done", e, if reply.is_empty() { took } else { format!("{took}: {reply}") }))
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
            Some(Draft::new("background", e, what))
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
            let mut draft = Draft::new("pr_checks", e, body);
            // Several terminals on one branch see the same PR.
            draft.key = Some(format!("pr_checks:{}:{:?}", pr.url, pr.checks));
            Some(draft)
        }
        kinds::TRIGGER_FIRED => {
            let id = s(&e.data, "trigger_id");
            let name = d.core().state.triggers.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
            let outcome = s(&e.data, "outcome");
            let mut draft = Draft::new("triggers", e, if outcome.is_empty() { format!("Trigger “{name}” fired") } else { format!("Trigger “{name}”: {outcome}") });
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

/// Post an agent's own notification (`notify.send`).
pub fn send(d: &Daemon, session: Option<Id>, title: &str, body: &str, sound: bool, from_agent: bool) -> NotifySendResult {
    send_as(d, "agent", session, title, body, sound, from_agent)
}

/// A notification in `category` ("agent" for `notify.send`, "from_trigger" for a trigger's
/// `notify` action). `rate_limit`: at most AGENT_PER_MINUTE a minute per terminal.
pub fn send_as(d: &Daemon, category: &'static str, session: Option<Id>, title: &str, body: &str, sound: bool, rate_limit: bool) -> NotifySendResult {
    let project = session.as_deref().and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()));
    if rate_limit && let Some(sid) = &session {
        let prefix = format!("{category}:{sid}:");
        let st = d.notify();
        let minute = Instant::now().checked_sub(Duration::from_secs(60));
        let sent = st.recent.iter().filter(|(t, k)| k.starts_with(&prefix) && minute.is_none_or(|m| *t > m)).count();
        if sent >= AGENT_PER_MINUTE {
            return NotifySendResult { posted: false, reason: Some("rate_limited".into()) };
        }
    }
    let text = if body.trim().is_empty() { title.trim().to_string() } else { format!("{}\n{}", title.trim(), body.trim()) };
    let draft = Draft {
        category,
        key: Some(format!("{category}:{}:{}", session.as_deref().unwrap_or(""), text)),
        session,
        project,
        body: text,
        needs_you_id: None,
        sound: Some(sound),
        test: false,
    };
    match post(d, draft) {
        Ok(_) => NotifySendResult { posted: true, reason: None },
        Err(r) => NotifySendResult { posted: false, reason: Some(r.into()) },
    }
}

/// A test notification with a category's sound, volume and image (`notify.test`).
pub fn test(d: &Daemon, session: Option<Id>, category: &str) -> NotifySendResult {
    let Some(c) = notify::category(category) else {
        return NotifySendResult { posted: false, reason: Some("unknown_category".into()) };
    };
    let project = session.as_deref().and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()));
    let draft = Draft { category: c.key, session, project, body: format!("Test: {}", c.label), needs_you_id: None, sound: None, key: None, test: true };
    match post(d, draft) {
        Ok(_) => NotifySendResult { posted: true, reason: None },
        Err(r) => NotifySendResult { posted: false, reason: Some(r.into()) },
    }
}

/// A category's sound file and volume (1–100; None: silent) and image, from the settings.
pub fn style(home: &std::path::Path, setting: &dyn Fn(&str) -> Value, category: &str) -> (Option<(String, u8)>, Option<String>) {
    let str_of = |k: &str| setting(k).as_str().unwrap_or("").to_string();
    let int_of = |k: &str| setting(k).as_i64().unwrap_or(100).clamp(0, 100);
    let volume = int_of("notify.volume") * int_of(&notify::volume_key(category)) / 100;
    let sound = crate::notify_media::sound_path(home, &str_of(&notify::sound_key(category)))
        .filter(|_| volume > 0)
        .map(|p| (p.to_string_lossy().into_owned(), volume.max(1) as u8));
    let image = match str_of(&notify::image_key(category)).as_str() {
        "" => crate::notify_media::image_path(home, &str_of("notify.image")),
        v => crate::notify_media::image_path(home, v),
    };
    (sound, image.map(|p| p.to_string_lossy().into_owned()))
}

/// Check the settings, dedupe, and emit `notify.posted` (showing it from midnad when no app
/// is connected).
fn post(d: &Daemon, draft: Draft) -> Result<Posted, &'static str> {
    let (title, (sound, image), when_app_closed) = {
        let core = d.core();
        let st = &core.state;
        let session = draft.session.as_deref().and_then(|s| st.session(s));
        if !draft.test
            && let Some(r) = blocked(&|k| st.setting_bool(k), session.map(|s| &s.notify), draft.category)
        {
            return Err(r);
        }
        let project = draft.project.as_deref().or(session.map(|s| s.project_id.as_str())).and_then(|p| st.project(p)).map(|p| p.name.clone());
        let title = match (session, project) {
            (Some(s), Some(p)) if p != s.name => format!("{} · {p}", s.name),
            (Some(s), _) => s.name.clone(),
            (None, Some(p)) => p,
            (None, None) => "midna".into(),
        };
        (title, style(&d.cfg.home, &|k| st.setting(k), draft.category), st.setting_bool("notify.when_app_closed") || draft.test)
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
    } else if when_app_closed && system_allowed(d) {
        "system"
    } else {
        "none"
    };
    let posted = Posted {
        category: draft.category.into(),
        title,
        body: truncate(&draft.body, BODY_MAX),
        sound: sound.is_some(),
        notification_sound: sound.as_ref().and_then(|(f, v)| crate::notify_media::rendered(&d.cfg.home, f, *v)).map(|p| p.to_string_lossy().into_owned()),
        volume: sound.as_ref().map(|s| s.1),
        sound_file: sound.map(|s| s.0),
        image,
        test: draft.test,
        via: via.into(),
        needs_you_id: draft.needs_you_id,
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
}
