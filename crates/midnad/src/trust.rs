//! An agent's folder-trust dialog as a needs-you item.
//!
//! In a folder it hasn't seen, Claude Code asks "do you trust the files in this folder?" before
//! it starts (Codex has the same for a directory). No hook fires while that is up, so without
//! this the terminal just sits at idle/started. Once a second, every agent terminal that hasn't
//! reported a conversation yet (or has a trust item open) is read: a trust dialog on screen
//! raises a `permission_prompt` item titled [`TITLE`] and sets the status to needs_you; the
//! dialog gone (answered here, in the terminal, or the agent exited) closes it again.
//! Approve/deny pick "Yes" / "No" in the dialog (`needs_you::resolve` → [`answer`]).
use crate::agent_state::{screen_shows_trust, trust_keys};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
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
            (true, None) if !answered().contains_key(&sid) => raise(d, &sid, &cwd, screen),
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

/// Answer the dialog for an approve/deny of its item, only if it is still on screen.
pub fn answer(d: &Daemon, item: &NeedsYou, approve: bool) {
    let Some(sid) = &item.session_id else { return };
    let Some(rt) = d.rt(sid) else { return };
    let Some((screen, _, _)) = rt.read(true) else { return };
    let Some(dialog) = screen_shows_trust(&screen) else { return };
    answered().insert(sid.clone(), Instant::now());
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
}

/// Terminals whose dialog was just answered here: the agent takes a moment to redraw, and the
/// old dialog must not be raised again meanwhile.
const ANSWER_GRACE: Duration = Duration::from_secs(3);

fn answered() -> MutexGuard<'static, HashMap<Id, Instant>> {
    static M: LazyLock<Mutex<HashMap<Id, Instant>>> = LazyLock::new(Default::default);
    let mut m = M.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|_, t| t.elapsed() < ANSWER_GRACE);
    m
}
