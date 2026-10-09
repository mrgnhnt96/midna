//! Why a terminal's agent ended. A terminal midna closes is gone before the agent's own
//! `SessionEnd` hook arrives as the agent dies, and a restart replaces the process under it.
//! This remembers each such end for a minute, so the late hook is still accepted and
//! `agent.session_ended` can say who ended the agent and how. Not persisted.
use midna_proto::{Actor, Id};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an end waits for the agent's hooks. Claude sends `SessionEnd` within a second or
/// two of its SIGHUP.
const KEEP: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct End {
    /// close | force_close | replace | exited (`close_on_exit`) | reset | project_removed | restart
    pub how: &'static str,
    pub by: Actor,
    pub project: Id,
    /// The terminal is gone (everything but a restart).
    pub closed: bool,
    at: Instant,
}

#[derive(Default)]
pub struct Ends(Mutex<HashMap<Id, End>>);

impl Ends {
    pub fn record(&self, sid: &str, how: &'static str, by: Actor, project: Id, closed: bool) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, e| e.at.elapsed() < KEEP);
        m.insert(sid.to_string(), End { how, by, project, closed, at: Instant::now() });
    }

    /// The end recorded for `sid` in the last minute, if any.
    pub fn get(&self, sid: &str) -> Option<End> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(sid).filter(|e| e.at.elapsed() < KEEP).cloned()
    }

    /// Like `get`, and forgets it: a restarted agent's next `SessionEnd` is its own.
    pub fn take(&self, sid: &str) -> Option<End> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.remove(sid).filter(|e| e.at.elapsed() < KEEP)
    }

    /// A terminal closed in the last minute (its late hooks are accepted).
    pub fn closed(&self, sid: &str) -> Option<End> {
        self.get(sid).filter(|e| e.closed)
    }
}
