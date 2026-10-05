//! Resume after sleep (`agents.resume_after_sleep`).
//!
//! An agent mid-turn when the Mac goes to sleep loses its API connection; Claude ends that turn
//! with `StopFailure` (status `failed`, process still running) once it wakes. On wake (wall
//! clock jumped past the monotonic one, which stops while asleep) every agent that was working
//! is watched for `WATCH_FOR`. When a watched agent stops on an error, its terminal gets
//! `agents.resume_after_sleep_prompt` through the queue, so it goes in only once the agent is
//! ready and never on top of a draft. A failure after that is retried, `MAX_TRIES` in all, the
//! later tries further apart (the network can take a while to come back). An agent that
//! finishes its turn by itself, exits or closes is dropped.
use crate::daemon::Daemon;
use midna_proto::*;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

const TICK: Duration = Duration::from_secs(1);
const WATCH_FOR: Duration = Duration::from_secs(30 * 60);
const MAX_TRIES: usize = 3;
/// How long after each failure the prompt goes in.
const BACKOFF: [i64; MAX_TRIES] = [5, 60, 300];

struct Watch {
    until: Instant,
    tries: usize,
    /// The queued prompt, until it has been typed.
    queued: Option<Id>,
    /// `status.since` of the failure already answered, so it isn't answered twice.
    answered: Option<Timestamp>,
}

#[derive(Default)]
pub struct Runtime {
    watch: Mutex<HashMap<Id, Watch>>,
}

impl Runtime {
    fn watch(&self) -> MutexGuard<'_, HashMap<Id, Watch>> {
        self.watch.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("resume".into()).spawn(move || {
        let mut mono = Instant::now();
        let mut wall = SystemTime::now();
        loop {
            std::thread::sleep(TICK);
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            let woke = SystemTime::now().duration_since(wall).unwrap_or_default() > mono.elapsed() + Duration::from_secs(60);
            if woke {
                let slept_at = wall.duration_since(SystemTime::UNIX_EPOCH).map(|t| t.as_secs() as i64).unwrap_or(0);
                on_wake(&d, slept_at);
            }
            mono = Instant::now();
            wall = SystemTime::now();
            tick(&d);
        }
    });
}

/// Watch the agents that were mid-turn at `slept_at` (unix): working then, or failed since.
pub fn on_wake(d: &Daemon, slept_at: i64) {
    if !d.core().state.setting_bool("agents.resume_after_sleep") {
        return;
    }
    let ids: Vec<Id> = {
        let core = d.core();
        core.state
            .sessions
            .iter()
            .filter(|s| s.agent.is_some() && s.pid.is_some())
            .filter(|s| {
                let in_turn = core.agents.get(&s.id).is_some_and(|a| a.in_turn);
                let failed_since = s.status.state == StatusState::Failed && time::parse_rfc3339(&s.status.since).is_some_and(|t| t >= slept_at);
                s.status.state == StatusState::Working || in_turn || failed_since
            })
            .map(|s| s.id.clone())
            .collect()
    };
    let mut watch = d.resume.watch();
    for id in ids {
        watch.entry(id).or_insert(Watch { until: Instant::now() + WATCH_FOR, tries: 0, queued: None, answered: None });
    }
}

fn actor() -> Actor {
    Actor { kind: ActorKind::System, session: None, name: Some("resume after sleep".into()) }
}

fn tick(d: &Daemon) {
    if d.resume.watch().is_empty() {
        return;
    }
    if !d.core().state.setting_bool("agents.resume_after_sleep") {
        d.resume.watch().clear();
        return;
    }
    let prompt = d.core().state.setting_str("agents.resume_after_sleep_prompt");
    let ids: Vec<Id> = d.resume.watch().keys().cloned().collect();
    for sid in ids {
        let Some((state, since, alive, queued, in_turn)) = ({
            let core = d.core();
            let w = d.resume.watch();
            core.state.session(&sid).zip(w.get(&sid)).map(|(s, w)| {
                let queued = w.queued.as_ref().is_some_and(|q| s.queue.iter().any(|m| &m.id == q));
                (s.status.state, s.status.since.clone(), s.pid.is_some(), queued, core.agents.get(&sid).is_some_and(|a| a.in_turn))
            })
        }) else {
            d.resume.watch().remove(&sid);
            continue;
        };
        let mut watch = d.resume.watch();
        let Some(w) = watch.get_mut(&sid) else { continue };
        if !queued {
            w.queued = None;
        }
        let failed = state == StatusState::Failed && alive && w.answered.as_ref() != Some(&since);
        let finished = matches!(state, StatusState::Done | StatusState::Idle) && !in_turn && w.queued.is_none();
        if !alive || finished || Instant::now() > w.until || (failed && w.tries >= MAX_TRIES) {
            watch.remove(&sid);
            continue;
        }
        if !failed || w.queued.is_some() || prompt.trim().is_empty() {
            continue;
        }
        let at = time::format_unix(time::now_unix() + BACKOFF[w.tries]);
        w.tries += 1;
        w.answered = Some(since);
        drop(watch);
        let added = crate::queue::add(d, &sid, prompt.clone(), true, vec![], SendWhen::At { at }, None, actor(), None);
        let mut watch = d.resume.watch();
        match added {
            Ok(m) => {
                if let Some(w) = watch.get_mut(&sid) {
                    w.queued = Some(m.id);
                }
            }
            Err(_) => {
                watch.remove(&sid);
            }
        }
    }
}
