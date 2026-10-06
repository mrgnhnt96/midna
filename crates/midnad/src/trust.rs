//! An agent's folder-trust dialog as a needs-you item.
//!
//! In a folder it hasn't seen, Claude Code asks "do you trust the files in this folder?" before
//! it starts (Codex has the same for a directory). No hook fires while that is up, so without
//! this the terminal just sits at idle/started. Once a second, every agent terminal that hasn't
//! reported a conversation yet (or has a trust item open) is read: a trust dialog on screen
//! raises a `permission_prompt` item titled [`TITLE`] and sets the status to needs_you; the
//! dialog gone (answered here, in the terminal, or the agent exited) closes it again.
//! Approve/deny pick "Yes" / "No" in the dialog (`needs_you::resolve` → [`answer`]).
//!
//! A cwd covered by `agents.trust_folders` ([`covered`]) gets "Yes" picked right away instead,
//! with an audit event and no item. Every other "Yes" adds its cwd to that setting: approving
//! the item, and "Yes" picked in the terminal itself: a trust item that closes on its own (the dialog
//! went away without midna answering it, see [`closed`]) has its cwd saved if the agent is
//! still running a moment later with no dialog on screen ("No" exits the agent).
use crate::agent_state::{screen_shows_trust, trust_keys};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
/// Title prefix of a trust item (`Trust this folder? <cwd>`).
pub const TITLE: &str = "Trust this folder?";
/// The status reason while the dialog is up.
const REASON: &str = "folder trust";
/// Gap between the keys that move to a choice and the Enter that picks it.
const KEY_GAP: Duration = Duration::from_millis(80);

pub fn is_trust_item(n: &NeedsYou) -> bool {
    n.kind == NeedsYouKind::PermissionPrompt && n.title.starts_with(TITLE)
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("trust".into()).spawn(move || {
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

/// One pass over the agent terminals that could be showing the dialog.
pub fn tick(d: &Daemon) {
    confirm(d);
    let watch: Vec<(Id, crate::term::RtHandle, Option<Id>, StatusState, String)> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter_map(|s| {
                s.agent?;
                let open = core.state.needs_you.iter().find(|n| n.session_id.as_deref() == Some(&s.id) && is_trust_item(n)).map(|n| n.id.clone());
                let fresh = s.agent_info.as_ref().is_none_or(|i| i.conversation_id.is_none());
                let quiet = matches!(s.status.state, StatusState::Idle | StatusState::NeedsYou);
                if open.is_none() && !(fresh && quiet && s.pid.is_some()) {
                    return None;
                }
                Some((s.id.clone(), core.rt.get(&s.id)?.clone(), open, s.status.state, s.cwd.clone()))
            })
            .collect()
    };
    for (sid, rt, open, state, cwd) in watch {
        let Some((screen, _, _)) = rt.read(true) else { continue };
        let shows = screen_shows_trust(&screen).is_some();
        match (shows, open) {
            (true, None) if just_answered(d, &sid) => {}
            (true, None) if covered(&trust_folders(d), &crate::rpc::session::home_dir(), &cwd) => auto_trust(d, &sid, &cwd),
            (true, None) => raise(d, &sid, &cwd, screen),
            (false, Some(id)) => {
                d.close_needs_you(&id, json!({ "kind": "done", "auto": true }), Actor::system());
                let still = d.core().state.session(&sid).is_some_and(|s| s.status.state == StatusState::NeedsYou && s.status.reason.as_deref() == Some(REASON));
                if state == StatusState::NeedsYou && still {
                    d.set_status(&sid, StatusState::Idle, Some("folder trust answered".into()), None, Actor::system());
                }
            }
            _ => {}
        }
    }
}

fn raise(d: &Daemon, sid: &str, cwd: &str, screen: Vec<String>) {
    let agent = d.core().state.session(sid).and_then(|s| s.agent).map(|a| a.as_str()).unwrap_or("the agent");
    let actor = Actor { kind: ActorKind::Agent, session: Some(sid.to_string()), name: Some(agent.to_string()) };
    let mut item = d.new_needs_you(NeedsYouKind::PermissionPrompt, format!("{TITLE} {cwd}"), actor, Some(sid.to_string()));
    item.detail = format!(
        "{agent} asks whether to trust the files in {cwd} before it starts (it may read, edit and run them). \
         Approve picks \"Yes\"; deny picks \"No\", and the agent exits."
    );
    item.screen_excerpt = Some(crate::rpc::session::tail_nonempty(screen, 12));
    d.raise_needs_you(item);
    d.set_status(sid, StatusState::NeedsYou, Some(REASON.into()), None, Actor::system());
}

/// Pick "Yes" for a cwd `agents.trust_folders` covers, leaving a trace in the event log.
fn auto_trust(d: &Daemon, sid: &str, cwd: &str) {
    if !pick(d, sid, true) {
        return;
    }
    let project = d.core().state.session(sid).map(|s| s.project_id.clone());
    d.emit(kinds::AUDIT, Actor::system(), project, Some(sid.to_string()), json!({ "action": "folder_trusted", "cwd": cwd, "by": "agents.trust_folders" }));
}

/// Answer the dialog for an approve/deny of its item, only if it is still on screen. "Yes"
/// also adds the item's cwd to `agents.trust_folders`: the agent remembers the folder as
/// trusted anyway, so midna does too.
pub fn answer(d: &Daemon, item: &NeedsYou, approve: bool) {
    let Some(sid) = &item.session_id else { return };
    if pick(d, sid, approve) && approve {
        let cwd = d.core().state.session(sid).map(|s| s.cwd.clone());
        if let Some(cwd) = cwd {
            remember(d, &cwd);
        }
    }
}

/// How long an agent must outlive a dialog answered in its terminal for that to count as "Yes".
const CONFIRM_AFTER: Duration = Duration::from_millis(1500);

/// One daemon's trust bookkeeping (`Daemon::trust`). Not persisted.
#[derive(Default)]
pub struct Memory {
    /// Terminals whose dialog was just answered here (see [`ANSWER_GRACE`]).
    answered: HashMap<Id, Instant>,
    /// Dialogs answered in the terminal, waiting for [`CONFIRM_AFTER`]: session → (cwd, when).
    pending: HashMap<Id, (String, Instant)>,
}

fn memory(d: &Daemon) -> MutexGuard<'_, Memory> {
    d.trust.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called by `close_needs_you` for every item that closes. A trust item closed as done by
/// midna itself, not by an answer given here, was answered in the terminal (or the agent exited).
pub fn closed(d: &Daemon, item: &NeedsYou, resolution: &serde_json::Value) {
    let auto = resolution.get("kind").and_then(serde_json::Value::as_str) == Some("done") && resolution.get("auto") == Some(&json!(true));
    let Some(sid) = item.session_id.as_deref().filter(|_| auto && is_trust_item(item)) else { return };
    if just_answered(d, sid) {
        return;
    }
    let Some(cwd) = d.core().state.session(sid).map(|s| s.cwd.clone()) else { return };
    memory(d).pending.insert(sid.to_string(), (cwd, Instant::now()));
}

/// Save the cwd of each terminal-answered dialog whose agent is still running.
fn confirm(d: &Daemon) {
    let due: Vec<(Id, String)> = {
        let p = &mut memory(d).pending;
        let due = p.iter().filter(|(_, (_, t))| t.elapsed() >= CONFIRM_AFTER).map(|(sid, (cwd, _))| (sid.clone(), cwd.clone())).collect::<Vec<_>>();
        p.retain(|_, (_, t)| t.elapsed() < CONFIRM_AFTER);
        due
    };
    for (sid, cwd) in due {
        let alive = d.core().state.session(&sid).is_some_and(|s| s.pid.is_some() && !s.status.state.is_terminal());
        let dialog = d.rt(&sid).and_then(|rt| rt.read(true)).is_none_or(|(screen, _, _)| screen_shows_trust(&screen).is_some());
        if alive && !dialog {
            remember(d, &cwd);
        }
    }
}

/// Add `cwd` (as `~/…` under home) to `agents.trust_folders`, unless it is covered already.
fn remember(d: &Daemon, cwd: &str) {
    let home = crate::rpc::session::home_dir();
    let mut list = trust_folders(d);
    if covered(&list, &home, cwd) {
        return;
    }
    let entry = match cwd.strip_prefix(&home) {
        Some(rest) if home != "/" && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => cwd.to_string(),
    };
    list.push(entry);
    let human = Actor { kind: ActorKind::Human, session: None, name: None };
    crate::rpc::settings::set_as(d, human, "agents.trust_folders", json!(list));
}

fn trust_folders(d: &Daemon) -> Vec<String> {
    d.core().state.setting("agents.trust_folders").as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect()
}

/// Does one of `patterns` (`agents.trust_folders`) cover the folder `cwd`? A pattern covers its
/// folder and everything under it; `~` is `home`; `*` matches within one folder name and `**`
/// any number of folders. A cwd with `.` or `..` in it is never covered.
pub fn covered(patterns: &[String], home: &str, cwd: &str) -> bool {
    let path: Vec<&str> = cwd.split('/').filter(|p| !p.is_empty()).collect();
    if !cwd.starts_with('/') || path.iter().any(|p| *p == "." || *p == "..") {
        return false;
    }
    patterns.iter().any(|pat| {
        let abs = match pat.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => pat.clone(),
        };
        let pat: Vec<&str> = abs.split('/').filter(|p| !p.is_empty()).collect();
        prefix_match(&pat, &path)
    })
}

/// Do the folders of `pat` match the first folders of `path` (or all of them)?
fn prefix_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => true,
        Some((&"**", rest)) => (0..=path.len()).any(|i| prefix_match(rest, &path[i..])),
        Some((p, rest)) => path.split_first().is_some_and(|(h, tail)| name_match(p, h) && prefix_match(rest, tail)),
    }
}

/// One folder name against a pattern where `*` is any run of characters.
fn name_match(pat: &str, name: &str) -> bool {
    let Some((first, rest)) = pat.split_once('*') else { return pat == name };
    let Some(mut left) = name.strip_prefix(first) else { return false };
    let mut parts: Vec<&str> = rest.split('*').collect();
    let last = parts.pop().unwrap_or("");
    for part in parts {
        match left.find(part) {
            Some(i) => left = &left[i + part.len()..],
            None => return false,
        }
    }
    left.len() >= last.len() && left.ends_with(last)
}

/// Type the keys that pick Yes (`approve`) or No, if the dialog is still on screen.
fn pick(d: &Daemon, sid: &str, approve: bool) -> bool {
    let Some(rt) = d.rt(sid) else { return false };
    let Some((screen, _, _)) = rt.read(true) else { return false };
    let Some(dialog) = screen_shows_trust(&screen) else { return false };
    memory(d).answered.insert(sid.to_string(), Instant::now());
    let keys = trust_keys(&dialog, approve);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(KEY_GAP);
        }
        rt.key(*k);
    }
    let waiting = d.core().state.session(sid).is_some_and(|s| s.status.state == StatusState::NeedsYou && s.status.reason.as_deref() == Some(REASON));
    if waiting {
        d.set_status(sid, StatusState::Idle, Some("folder trust answered".into()), None, Actor::system());
    }
    true
}

/// Terminals whose dialog was just answered here: the agent takes a moment to redraw, and the
/// old dialog must not be raised again meanwhile.
const ANSWER_GRACE: Duration = Duration::from_secs(3);

fn just_answered(d: &Daemon, sid: &str) -> bool {
    let mut m = memory(d);
    m.answered.retain(|_, t| t.elapsed() < ANSWER_GRACE);
    m.answered.contains_key(sid)
}

#[cfg(test)]
mod tests {
    use super::covered;

    fn cov(pats: &[&str], cwd: &str) -> bool {
        covered(&pats.iter().map(|p| p.to_string()).collect::<Vec<_>>(), "/Users/me", cwd)
    }

    #[test]
    fn a_folder_covers_itself_and_everything_under_it() {
        assert!(cov(&["~/Development"], "/Users/me/Development"));
        assert!(cov(&["~/Development"], "/Users/me/Development/midna/crates"));
        assert!(cov(&["/opt/work/"], "/opt/work/x"));
        assert!(!cov(&["~/Development"], "/Users/me/Dev"));
        assert!(!cov(&["~/Development"], "/Users/me/Development2"), "a prefix of a name isn't its folder");
        assert!(!cov(&["~/Development"], "/Users/me"));
        assert!(!cov(&[], "/Users/me/Development"));
    }

    #[test]
    fn stars_match_within_a_name_and_double_stars_any_depth() {
        assert!(cov(&["~/work/client-*"], "/Users/me/work/client-acme/api"));
        assert!(!cov(&["~/work/client-*"], "/Users/me/work/internal"));
        assert!(cov(&["~/work/*/api"], "/Users/me/work/acme/api"));
        assert!(!cov(&["~/work/*/api"], "/Users/me/work/acme/web"));
        assert!(cov(&["~/src/**/sandbox"], "/Users/me/src/sandbox"));
        assert!(cov(&["~/src/**/sandbox"], "/Users/me/src/a/b/sandbox/x"));
        assert!(!cov(&["~/src/**/sandbox"], "/Users/me/src/a/b"));
        assert!(cov(&["~/*-tmp*"], "/Users/me/x-tmp"));
        assert!(cov(&["~/a*b*c"], "/Users/me/abxc"));
        assert!(!cov(&["~/a*a"], "/Users/me/a"));
    }

    #[test]
    fn dot_dot_never_escapes_into_a_trusted_folder() {
        assert!(!cov(&["~/Development"], "/Users/me/Development/../.ssh"));
        assert!(!cov(&["~/Development"], "relative/Development"));
    }
}
