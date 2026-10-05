//! Queued restarts and agent update detection.
//!
//! A queued restart (`session.restart` with `when: idle`, an installed agent update under
//! `agents.restart_on_update = when_idle`, or "When idle" on an update prompt under `ask`) runs once nothing would be lost: the agent is not in
//! a turn or waiting on the human, its last `Stop` reported no background work and no scheduled
//! wakeups, no subagent is running, its input box is empty, and the terminal has been quiet for
//! `agents.restart_idle_secs`. Until then `AgentInfo.restart.waiting_for` says why it waits.
use crate::agent_work::{claude_input_text, claude_update_notice, newer, parse_version};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// Update `sid`'s AgentInfo; emits `session.agent` when the closure says it changed.
fn edit(d: &Daemon, sid: &str, f: impl FnOnce(&mut AgentInfo) -> bool) {
    let changed = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return };
        let info = s.agent_info.get_or_insert_with(Default::default);
        f(info).then(|| (s.project_id.clone(), info.clone()))
    };
    if let Some((project, info)) = changed {
        d.mark_dirty();
        d.emit(kinds::SESSION_AGENT, Actor::system(), Some(project), Some(sid.into()), serde_json::to_value(&info).unwrap_or_default());
    }
}

pub fn queue(d: &Daemon, sid: &str, reason: &str, by: Actor) {
    let waiting_for = blockers(d, sid);
    edit(d, sid, |i| {
        if i.restart.as_ref().is_some_and(|r| r.reason == reason) {
            return false;
        }
        i.restart = Some(QueuedRestart { reason: reason.into(), queued_at: time::now_rfc3339(), by, waiting_for });
        true
    });
}

pub fn cancel(d: &Daemon, sid: &str, by: Actor) {
    let mut had = false;
    edit(d, sid, |i| {
        had = i.restart.take().is_some();
        had
    });
    if had {
        let project = d.core().state.session(sid).map(|s| s.project_id.clone());
        d.emit(kinds::AUDIT, by, project, Some(sid.into()), json!({ "action": "restart_cancelled" }));
    }
}

/// "Not now" to the terminal's update prompt: hidden until a newer update.
pub fn decline_update(d: &Daemon, sid: &str) {
    edit(d, sid, |i| {
        let changed = i.update_available.is_some() && i.update_declined != i.update_available;
        i.update_declined = i.update_available.clone();
        changed
    });
}

/// Why `sid` can't be restarted without losing anything right now (empty = go).
pub fn blockers(d: &Daemon, sid: &str) -> Vec<String> {
    let (s, in_turn, open_items, idle_secs) = {
        let core = d.core();
        let Some(s) = core.state.session(sid).cloned() else { return vec!["no such terminal".into()] };
        let in_turn = core.agents.get(sid).is_some_and(|a| a.in_turn);
        let open_items = core
            .state
            .needs_you
            .iter()
            .any(|n| n.session_id.as_deref() == Some(sid) && matches!(n.kind, NeedsYouKind::Approval | NeedsYouKind::PermissionPrompt | NeedsYouKind::Blocked));
        let idle_secs = core.state.setting("agents.restart_idle_secs").as_i64().unwrap_or(60).max(0);
        (s, in_turn, open_items, idle_secs)
    };
    let mut w = vec![];
    match s.status.state {
        StatusState::Working => w.push("the agent is working".to_string()),
        StatusState::NeedsYou => w.push("the agent is waiting on you".to_string()),
        _ if in_turn => w.push("a turn is in progress".to_string()),
        _ => {}
    }
    if open_items {
        w.push("an approval or prompt is open".into());
    }
    if let Some(info) = &s.agent_info {
        w.extend(info.in_flight());
    }
    let Some(rt) = d.rt(sid) else { return w };
    let last = rt.activity.load(Ordering::Relaxed).max(time::parse_rfc3339(&s.status.since).unwrap_or(0));
    if time::now_unix() - last < idle_secs {
        w.push(format!("less than {idle_secs}s since the terminal was last active"));
    }
    if s.agent == Some(AgentKind::Claude) {
        // Styled read: a dim placeholder or suggestion in the box is not typed text.
        match rt.with(|e| e.screen_undimmed()).map(|rows| claude_input_text(&rows)) {
            Some(Some(t)) if t.is_empty() => {}
            Some(Some(_)) => w.push("text is typed in the input box".into()),
            _ => w.push("the input box isn't on screen (a dialog or menu is open)".into()),
        }
    }
    w
}

/// Every couple of seconds: refresh what queued restarts wait for, and run the ones that can go.
pub fn tick(d: &Arc<Daemon>) {
    let queued: Vec<Id> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter(|s| s.pid.is_some() && s.agent_info.as_ref().is_some_and(|i| i.restart.is_some()))
            .filter(|s| !core.agents.get(&s.id).is_some_and(|a| a.restarting))
            .map(|s| s.id.clone())
            .collect()
    };
    for sid in queued {
        let w = blockers(d, &sid);
        let go = w.is_empty();
        edit(d, &sid, |i| match i.restart.as_mut() {
            Some(r) if r.waiting_for != w => {
                r.waiting_for = w;
                true
            }
            _ => false,
        });
        if !go {
            continue;
        }
        let Some(plan) = d.core().state.session(&sid).and_then(|s| s.agent_info.as_ref()?.restart.clone()) else { continue };
        if let Some(a) = d.core().agents.get_mut(&sid) {
            a.restarting = true;
        }
        let (d2, sid2) = (d.clone(), sid.clone());
        let spawned = std::thread::Builder::new().name("restart".into()).spawn(move || {
            let resume = d2.core().state.session(&sid2).is_some_and(|s| s.agent_info.as_ref().is_some_and(|i| i.conversation_id.is_some()));
            if let Err(e) = crate::rpc::session::restart_now(&d2, &sid2, resume, &plan.reason, plan.by.clone()) {
                if let Some(a) = d2.core().agents.get_mut(&sid2) {
                    a.restarting = false;
                }
                edit(&d2, &sid2, |i| {
                    i.restart = None;
                    true
                });
                let mut item = d2.new_needs_you(NeedsYouKind::Failed, format!("Queued restart failed: {}", e.message), Actor::system(), Some(sid2.clone()));
                item.detail = plan.reason.clone();
                d2.raise_needs_you(item);
            }
        });
        if spawned.is_err()
            && let Some(a) = d.core().agents.get_mut(&sid)
        {
            a.restarting = false;
        }
    }
}

/// Installed version of an agent CLI, resolved the way a terminal launches it (login shell).
fn installed_version(d: &Daemon, agent: AgentKind) -> Option<String> {
    let bin = d.cfg.agent_bin.clone().unwrap_or_else(|| agent.as_str().to_string());
    let argv = crate::rpc::session::exec_argv(&[bin, "--version".into()]);
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < Duration::from_secs(20) => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    parse_version(&out)
}

/// For each live Claude terminal: is a newer Claude installed than the one it runs? Either the
/// status line's version is older than `claude --version`, or Claude shows its own "Restart to
/// update" notice (the only signal when the terminal's status line isn't midna's, so no version
/// is reported). Either sets `update_available`; `agents.restart_on_update = when_idle` also
/// queues the restart, `ask` leaves it to the prompt the GUI shows on the terminal.
pub fn check_updates(d: &Daemon) {
    let running: Vec<(Id, Option<String>)> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter(|s| s.pid.is_some() && s.agent == Some(AgentKind::Claude))
            .map(|s| (s.id.clone(), s.agent_info.as_ref().and_then(|i| i.version.clone())))
            .collect()
    };
    if running.is_empty() {
        return;
    }
    let installed = installed_version(d, AgentKind::Claude);
    let mode = d.core().state.setting_str("agents.restart_on_update");
    for (sid, version) in running {
        let by_version = matches!((&installed, &version), (Some(i), Some(v)) if newer(i, v));
        let notice = || d.rt(&sid).and_then(|rt| rt.read(true)).is_some_and(|(screen, _, _)| claude_update_notice(&screen));
        if !by_version && !notice() {
            continue;
        }
        // The notice without a known installed version still names the update, so it is noted once.
        let target = installed.clone().unwrap_or_else(|| "update".into());
        let mut fresh = false;
        edit(d, &sid, |i| {
            fresh = i.update_available.as_deref() != Some(target.as_str());
            i.update_available = Some(target.clone());
            fresh
        });
        if !fresh {
            continue;
        }
        let change = match (&version, &installed) {
            (Some(v), Some(i)) if newer(i, v) => format!("{v} → {i}"),
            (_, Some(i)) => format!("{i} installed"),
            _ => "update installed".into(),
        };
        // ask: `update_available` is the prompt, shown when the terminal is opened.
        if mode == "when_idle" {
            queue(d, &sid, &format!("Claude {change}"), Actor::system());
        }
    }
}

/// Background: queued restarts every 2s, update checks every minute.
pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("restart-queue".into()).spawn(move || {
        let mut n = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            tick(&d);
            crate::adopt::reap(&d);
            n += 1;
            // First check ~10s after start, then every minute. Off the tick so a slow login
            // shell never delays a queued restart.
            if n % 30 == 5 {
                let d2 = d.clone();
                let _ = std::thread::Builder::new().name("agent-updates".into()).spawn(move || check_updates(&d2));
            }
        }
    });
}
