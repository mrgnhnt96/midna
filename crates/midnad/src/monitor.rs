//! Monitor grants drive the monitoring agent by themselves.
//!
//! A grant (`session.monitor_grant`, from the row's "Monitor other tabs" menu or ⌘K) lets a
//! terminal's agent answer other terminals' prompts and approvals. On its own that agent would
//! never know: the human would have to tell it, and it would build its own polling loop. So
//! midnad does the rest, typing into the terminal through its queue (`queue.rs`) once the
//! agent is ready:
//!
//! - granted: a briefing (keep the other tabs working, what it may answer, until when, how).
//! - every `monitor.check_secs` (and at once on a needs-you item or a status change): what the
//!   other tabs need. A permission prompt or approval in the grant's scope, or an agent tab
//!   that has sat idle, done, exited or failed for `IDLE_FOR` (finished, to close; or stopped
//!   short, to nudge). Each thing goes in one note at most once: an idle tab again only after
//!   it worked in between. Lines gather in a single note waiting in the queue, so a burst wakes
//!   the agent once, and a line goes again when its item is answered or its agent starts
//!   working before the note is typed.
//! - revoked or expired: a note that it ended, and the waiting note is dropped.
//!
//! Only agent terminals get these; a shell with a grant gets nothing typed into it.
use crate::daemon::Daemon;
use midna_proto::*;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// First line of the note that gathers what other terminals need.
pub const NOTE_HEAD: &str = "[midna monitor] Other tabs need a look:";
const NOTE_FOOT: &str = "Handle each the way your monitor briefing says, then end your turn: midna sends the next note.";
/// How long an agent tab sits idle before it is a line (not the pause between two turns).
const IDLE_FOR: i64 = 30;

/// What midnad remembers per monitoring terminal (in memory: a restart notes things once more).
#[derive(Default)]
struct Watch {
    /// Keys already put in a note that went out (typed, or removed by the human).
    told: HashSet<String>,
    /// The waiting note's queue id and its keys, as last written.
    note: Option<(Id, Vec<String>)>,
}

#[derive(Default)]
pub struct Runtime {
    watch: Mutex<HashMap<Id, Watch>>,
    last_check: Mutex<Option<Instant>>,
}

pub fn start(d: &Arc<Daemon>) {
    let rx = d.log.listen();
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("monitor-events".into()).spawn(move || {
        for e in rx {
            let Some(d) = w.upgrade() else { return };
            if !d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                on_event(&d, &e);
            }
        }
    });
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("monitor-check".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }
            let every = d.core().state.setting_i64("monitor.check_secs");
            let due = {
                let mut last = d.monitor.last_check.lock().unwrap_or_else(|e| e.into_inner());
                let due = every > 0 && last.is_none_or(|t| t.elapsed() >= Duration::from_secs(every as u64));
                if due {
                    *last = Some(Instant::now());
                }
                due
            };
            if due {
                check(&d);
            }
        }
    });
}

pub fn on_event(d: &Daemon, e: &Event) {
    match e.kind.as_str() {
        kinds::SESSION_MONITOR_GRANTED => {
            if let Some(m) = e.session_id.as_deref() {
                granted(d, m);
            }
        }
        kinds::SESSION_MONITOR_REVOKED => ended(d, e.session_id.as_deref(), "the human revoked it"),
        kinds::SESSION_MONITOR_EXPIRED => ended(d, e.session_id.as_deref(), "its time ran out"),
        kinds::NEEDS_YOU_RAISED | kinds::NEEDS_YOU_RESOLVED | kinds::SESSION_STATUS | kinds::SESSION_CLOSED => check(d),
        _ => {}
    }
}

/// Bring every monitoring terminal's waiting note up to date.
pub fn check(d: &Daemon) {
    let now = time::now_unix();
    let monitors: Vec<(Id, MonitorScope)> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter(|s| s.agent.is_some() && !s.status.state.is_terminal())
            .filter_map(|s| s.monitor_grant.as_ref().filter(|g| g.live_at(now)).map(|g| (s.id.clone(), g.scope)))
            .collect()
    };
    {
        let live: HashSet<&Id> = monitors.iter().map(|(m, _)| m).collect();
        d.monitor.watch.lock().unwrap_or_else(|e| e.into_inner()).retain(|m, _| live.contains(m));
    }
    for (m, scope) in monitors {
        let want = wanted(d, &m, scope, now);
        sync_note(d, &m, want);
    }
}

/// What terminal `m` should hear about now, as (key, line).
fn wanted(d: &Daemon, m: &str, scope: MonitorScope, now: i64) -> Vec<(String, String)> {
    let core = d.core();
    let name = |sid: &str| core.state.session(sid).map(|s| format!("{} ({sid})", s.name)).unwrap_or_else(|| sid.to_string());
    let mut out: Vec<(String, String)> = core
        .state
        .needs_you
        .iter()
        .filter(|n| n.session_id.as_deref() != Some(m) && covers(scope, n))
        .map(|n| (n.id.clone(), item_line(n, &n.session_id.as_deref().map(name).unwrap_or_else(|| "no terminal".into()))))
        .collect();
    for s in core.state.sessions.iter().filter(|s| s.id != m && s.agent.is_some() && s.queue.is_empty()) {
        let stopped = match s.status.state {
            StatusState::Idle => "went idle",
            StatusState::Done => "finished its turn",
            StatusState::Exited => "exited",
            StatusState::Failed => "failed",
            StatusState::Working | StatusState::NeedsYou => continue,
        };
        let Some(since) = time::parse_rfc3339(&s.status.since) else { continue };
        if now - since < IDLE_FOR {
            continue;
        }
        let line = format!("- {} {stopped} at {}: done with its task (close it with `midna close {}`), or stopped short (nudge it)?", name(&s.id), clock(since), s.id);
        out.push((format!("{}@{since}", s.id), line));
    }
    out
}

/// Whether a grant of `scope` may answer `item` (never a question or a folder-trust prompt).
fn covers(scope: MonitorScope, item: &NeedsYou) -> bool {
    match item.kind {
        NeedsYouKind::PermissionPrompt => scope.covers_prompts() && item.question.is_none() && !crate::trust::is_trust_item(item),
        NeedsYouKind::Approval => scope.covers_approvals(),
        _ => false,
    }
}

fn item_line(item: &NeedsYou, tab: &str) -> String {
    let what = if item.kind == NeedsYouKind::Approval { "approval" } else { "permission prompt" };
    format!("- `{}` {what} in {tab}: {}", item.id, item.title.lines().next().unwrap_or(""))
}

pub fn note(lines: &[&str]) -> String {
    format!("{NOTE_HEAD}\n{}\n{NOTE_FOOT}", lines.join("\n"))
}

/// Rewrite `m`'s waiting note to hold what it wants and wasn't told yet.
fn sync_note(d: &Daemon, m: &str, want: Vec<(String, String)>) {
    let mut watches = d.monitor.watch.lock().unwrap_or_else(|e| e.into_inner());
    let w = watches.entry(m.to_string()).or_default();
    // The last note went out (or the human removed it): its keys were told.
    if let Some((id, keys)) = w.note.take() {
        if waiting(d, m, &id) {
            w.note = Some((id, keys));
        } else {
            w.told.extend(keys);
        }
    }
    // Forget what no longer applies, so it can come back (an idle tab after it worked again).
    let current: HashSet<&String> = want.iter().map(|(k, _)| k).collect();
    w.told.retain(|k| current.contains(k));
    let fresh: Vec<&(String, String)> = want.iter().filter(|(k, _)| !w.told.contains(k)).collect();
    let keys: Vec<String> = fresh.iter().map(|(k, _)| k.clone()).collect();
    let lines: Vec<&str> = fresh.iter().map(|(_, l)| l.as_str()).collect();
    match w.note.take() {
        Some((id, old)) if old == keys => w.note = Some((id, old)),
        Some((id, _)) if keys.is_empty() => {
            let _ = crate::queue::remove(d, m, &id, Actor::system());
        }
        Some((id, old)) => {
            let p = QueueUpdateParams { session: Some(m.into()), id: id.clone(), text: Some(note(&lines)), enter: None, when: None, retry: false };
            match crate::queue::update(d, m, p, Actor::system()) {
                Ok(_) => w.note = Some((id, keys)),
                // It started typing meanwhile: those were told; the rest go in a new note.
                Err(_) => {
                    let rest: Vec<usize> = (0..keys.len()).filter(|&i| !old.contains(&keys[i])).collect();
                    w.told.extend(old);
                    if !rest.is_empty() {
                        let lines: Vec<&str> = rest.iter().map(|&i| lines[i]).collect();
                        if let Ok(q) = queue(d, m, note(&lines)) {
                            w.note = Some((q.id, rest.iter().map(|&i| keys[i].clone()).collect()));
                        }
                    }
                }
            }
        }
        None if keys.is_empty() => {}
        None => {
            if let Ok(q) = queue(d, m, note(&lines)) {
                w.note = Some((q.id, keys));
            }
        }
    }
}

/// Message `id` is still waiting in `m`'s queue (not typed, not removed).
fn waiting(d: &Daemon, m: &str, id: &str) -> bool {
    d.core().state.session(m).is_some_and(|s| s.queue.iter().any(|q| q.id == id && q.state != QueueState::Sending))
}

fn queue(d: &Daemon, m: &str, text: String) -> Result<QueuedMessage, RpcError> {
    crate::queue::add(d, m, text, true, vec![], SendWhen::Idle, None, Actor::system(), None)
}

fn drop_note(d: &Daemon, m: &str) {
    let note = d.monitor.watch.lock().unwrap_or_else(|e| e.into_inner()).remove(m).and_then(|w| w.note);
    if let Some((id, _)) = note {
        let _ = crate::queue::remove(d, m, &id, Actor::system());
    }
}

fn granted(d: &Daemon, m: &str) {
    let Some((grant, is_agent)) = d.core().state.session(m).and_then(|s| Some((s.monitor_grant.clone()?, s.agent.is_some()))) else { return };
    if !is_agent {
        return;
    }
    // A new or changed grant starts over: the briefing goes first, then a fresh note.
    drop_note(d, m);
    let every = d.core().state.setting_i64("monitor.check_secs");
    let _ = queue(d, m, briefing(&grant, every));
    check(d);
}

/// What the agent is told when the human gives its terminal a grant.
pub fn briefing(g: &MonitorGrant, check_secs: i64) -> String {
    let until = match g.until.as_deref().and_then(time::parse_rfc3339) {
        Some(t) => format!("until {}", clock(t)),
        None => "until the human revokes it".into(),
    };
    let what = match g.scope {
        MonitorScope::All => "permission prompts and approval requests",
        MonitorScope::Prompts => "permission prompts",
        MonitorScope::Approvals => "approval requests",
    };
    let when = if check_secs > 0 { format!("checks the other tabs every {check_secs}s and ") } else { String::new() };
    format!(
        "[midna monitor] The human made this terminal the monitor for the other tabs, {until}. Your one job: keep the other agents working, and tidy up the tabs that are finished.\n\
         midna {when}types a note here whenever another tab needs you, so don't start a watch loop, Monitor or timer of your own; handle the note, then end your turn.\n\
         - A prompt or approval: read it (`midna read <id> --screen`, `midna needs get <n_id>`). You may answer their {what}: approve once with `midna approve <n_id>` when it is safe and within what that agent was asked to do, or deny with `--deny`. When unsure, leave it to the human, with `midna attention \"<one line>\"` if it blocks work.\n\
         - An idle, done, exited or failed agent: read its screen. Stopped short of its task: tell it how to go on with `midna queue add --session <id> \"<text>\"`. Finished (its task submitted, its PR merged, nothing left to do): close it with `midna close <id>`. Asking the human something, or waiting on a time or another tab: leave it.\n\
         - Never yours, grant or not: questions an agent asks the human, folder-trust prompts, human-only actions, approving for longer than once, and closing a tab that is working. More: `midna skill monitor`.\n\
         - When the grant ends you'll get a note; stop then."
    )
}

fn ended(d: &Daemon, m: Option<&str>, why: &str) {
    let Some(m) = m else { return };
    drop_note(d, m);
    if d.core().state.session(m).is_some_and(|s| s.agent.is_some() && !s.status.state.is_terminal()) {
        let _ = queue(d, m, format!("[midna monitor] Your monitor grant ended ({why}). Stop answering and closing other tabs, and stop any watch you started; what is still waiting goes to the human."));
    }
}

/// "2:00 AM" local, with the weekday when it isn't today.
fn clock(t: i64) -> String {
    let (_, _, day, h, mi, wd) = time::local_parts(t);
    let today = time::local_parts(time::now_unix()).2;
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    let wd = if day != today { format!("{} ", ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][wd as usize]) } else { String::new() };
    format!("{wd}{h12}:{mi:02} {half}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(scope: MonitorScope, until: Option<String>) -> MonitorGrant {
        MonitorGrant { scope, granted_at: time::now_rfc3339(), until }
    }

    #[test]
    fn the_briefing_says_the_job_until_when_and_how_often() {
        let b = briefing(&grant(MonitorScope::Prompts, None), 30);
        assert!(b.contains("until the human revokes it") && b.contains("their permission prompts"), "{b}");
        assert!(b.contains("every 30s") && b.contains("don't start a watch loop") && b.contains("midna close <id>"), "{b}");
        let until = time::format_unix(time::now_unix() + 3600);
        let b = briefing(&grant(MonitorScope::All, Some(until)), 0);
        assert!(b.contains("prompts and approval requests") && !b.contains("every"), "{b}");
        assert!(b.contains(" AM. ") || b.contains(" PM. "), "{b}");
    }

    #[test]
    fn a_note_has_its_head_lines_and_foot() {
        let n = note(&["- `n_1` a", "- b"]);
        assert_eq!(n, format!("{NOTE_HEAD}\n- `n_1` a\n- b\n{NOTE_FOOT}"));
    }
}
