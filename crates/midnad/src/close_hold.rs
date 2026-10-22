//! Closes nobody at the keyboard asked for (an agent's `session.close`, `close_on_exit`) ask the
//! app first, since the human may be reading that terminal. The app gets `session.closing` and
//! answers with `session.close_hold`: `close` when they aren't looking at it, `hold` while a
//! countdown shows (then `close`, or `keep` when they stop it). No app, or no answer within
//! [`ASK`], and it closes as before. A held close goes ahead when the last app disconnects.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the app has to answer before the close goes ahead.
const ASK: Duration = Duration::from_millis(1500);

struct Waiting {
    /// Names this close, so a late timer leaves a newer one alone.
    token: u64,
    force: bool,
    cleanup: Option<bool>,
    how: &'static str,
    by: Actor,
    /// The app said hold: no timeout.
    held: bool,
}

/// The closes waiting on the app, by terminal. Not persisted: a restarted daemon has no app
/// counting down.
#[derive(Default)]
pub struct Holds {
    map: Mutex<HashMap<Id, Waiting>>,
    next: AtomicU64,
}

impl Holds {
    fn take(&self, sid: &str) -> Option<Waiting> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).remove(sid)
    }
}

/// Close `sid` for `by`, asking the app first when one is connected.
pub fn close(d: &Arc<Daemon>, sid: &str, force: bool, cleanup: Option<bool>, how: &'static str, by: Actor) {
    if !d.gui_connected() {
        finish(d, sid, Waiting { token: 0, force, cleanup, how, by, held: false });
        return;
    }
    let token = d.holds.next.fetch_add(1, Ordering::Relaxed);
    {
        let mut map = d.holds.map.lock().unwrap_or_else(|e| e.into_inner());
        // Asked twice: the first ask stands.
        if map.contains_key(sid) {
            return;
        }
        map.insert(sid.to_string(), Waiting { token, force, cleanup, how, by: by.clone(), held: false });
    }
    if d.gui_send("session.closing", json!({ "id": sid, "by": by, "how": how })) == 0 {
        if let Some(w) = d.holds.take(sid) {
            finish(d, sid, w);
        }
        return;
    }
    let (d, sid) = (d.clone(), sid.to_string());
    let _ = std::thread::Builder::new().name("close-ask".into()).spawn(move || {
        std::thread::sleep(ASK);
        let unanswered = {
            let mut map = d.holds.map.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&sid) {
                Some(w) if w.token == token && !w.held => map.remove(&sid),
                _ => None,
            }
        };
        if let Some(w) = unanswered {
            finish(&d, &sid, w);
        }
    });
}

/// `session.close_hold`: the app's answer. False when no close was waiting.
pub fn answer(d: &Arc<Daemon>, sid: &str, action: CloseHold) -> bool {
    match action {
        CloseHold::Hold => {
            let by = {
                let mut map = d.holds.map.lock().unwrap_or_else(|e| e.into_inner());
                let Some(w) = map.get_mut(sid) else { return false };
                w.held = true;
                (w.by.clone(), w.how)
            };
            let project = d.core().state.session(sid).map(|s| s.project_id.clone());
            d.emit(kinds::SESSION_CLOSE_HELD, Actor::human(), project, Some(sid.into()), json!({ "by": by.0, "how": by.1 }));
            true
        }
        CloseHold::Close => {
            let Some(w) = d.holds.take(sid) else { return false };
            finish(d, sid, w);
            true
        }
        CloseHold::Keep => {
            let Some(w) = d.holds.take(sid) else { return false };
            let project = d.core().state.session(sid).map(|s| s.project_id.clone());
            d.emit(kinds::SESSION_CLOSE_KEPT, Actor::human(), project, Some(sid.into()), json!({ "by": w.by, "how": w.how }));
            true
        }
    }
}

/// A connection went away: with no app left to count down, the held closes go ahead.
pub fn on_disconnect(d: &Arc<Daemon>) {
    if d.gui_connected() {
        return;
    }
    let all: Vec<(Id, Waiting)> = d.holds.map.lock().unwrap_or_else(|e| e.into_inner()).drain().collect();
    for (sid, w) in all {
        finish(d, &sid, w);
    }
}

/// Close it, unless it changed meanwhile: it's gone, its process came back (a restart after
/// `close_on_exit`), or it got busy again and the close wasn't forced.
fn finish(d: &Arc<Daemon>, sid: &str, w: Waiting) {
    let Some(state) = d.core().state.session(sid).map(|s| s.status.state) else { return };
    if w.how == "exited" && !state.is_terminal() {
        return;
    }
    if !w.force && matches!(state, StatusState::Working | StatusState::NeedsYou) {
        return;
    }
    let closing = crate::cleanup::capture(d, sid, w.cleanup);
    let ctx = Ctx { as_actor: Some(w.by), ..Ctx::internal_system() };
    crate::rpc::session::close_inner(d, &ctx, sid, w.force, w.how);
    if let Some(c) = closing {
        crate::cleanup::after_close(d, c);
    }
}
