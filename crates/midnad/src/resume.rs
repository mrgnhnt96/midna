//! Resume after sleep (`agents.resume_after_sleep`) and after a lost network
//! (`agents.resume_after_network`).
//!
//! An agent mid-turn when the Mac goes to sleep or the network drops loses its API connection;
//! Claude ends that turn with `StopFailure` (status `failed`, process still running). On wake
//! (`clock.rs` noticed the wall clock jump past the monotonic one) every agent that was
//! working is watched for `WATCH_FOR`; a `StopFailure` whose error reads like a connection error
//! puts that agent on a watch for `NETWORK_WATCH_FOR`. When a watched agent stops on an error,
//! midna waits until the API host is reachable again, then its terminal gets
//! `agents.resume_after_sleep_prompt` through the queue, so it goes in only once the agent is
//! ready and never on top of a draft. A failure after that is retried, `MAX_TRIES` in all, the
//! later tries further apart (the network can take a while to settle). An agent that finishes
//! its turn by itself, exits or closes is dropped.
use crate::daemon::Daemon;
use midna_proto::*;
use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
const WATCH_FOR: Duration = Duration::from_secs(30 * 60);
/// An outage can last a while: a turn lost to it is resumed if the network is back within this.
const NETWORK_WATCH_FOR: Duration = Duration::from_secs(6 * 60 * 60);
const MAX_TRIES: usize = 3;
/// How long after each failure (once online) the prompt goes in.
const BACKOFF: [i64; MAX_TRIES] = [5, 60, 300];
/// How often reachability is checked while a failed agent waits for the network.
const PROBE_EVERY: Duration = Duration::from_secs(5);
const PROBE_HOST: &str = "api.anthropic.com:443";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cause {
    Sleep,
    Network,
}

impl Cause {
    fn setting(self) -> &'static str {
        match self {
            Cause::Sleep => "agents.resume_after_sleep",
            Cause::Network => "agents.resume_after_network",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Cause::Sleep => "resume after sleep",
            Cause::Network => "resume after network loss",
        }
    }
}

struct Watch {
    cause: Cause,
    until: Instant,
    tries: usize,
    /// The queued prompt, until it has been typed.
    queued: Option<Id>,
    /// `status.since` of the failure already answered, so it isn't answered twice.
    answered: Option<Timestamp>,
}

impl Watch {
    fn new(cause: Cause, for_: Duration) -> Watch {
        Watch { cause, until: Instant::now() + for_, tries: 0, queued: None, answered: None }
    }
}

type Probe = Box<dyn Fn() -> bool + Send + Sync>;

#[derive(Default)]
pub struct Runtime {
    watch: Mutex<HashMap<Id, Watch>>,
    /// Replaces the reachability check (tests).
    probe: Mutex<Option<Probe>>,
    /// The last reachability check: when, and whether it got through.
    online: Mutex<Option<(Instant, bool)>>,
}

impl Runtime {
    fn watch(&self) -> MutexGuard<'_, HashMap<Id, Watch>> {
        self.watch.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the API host answers, checked at most every `PROBE_EVERY`.
    fn online(&self) -> bool {
        if let Some((at, up)) = *self.online.lock().unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < PROBE_EVERY
        {
            return up;
        }
        let up = match &*self.probe.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(f) => f(),
            None => reachable(PROBE_HOST),
        };
        *self.online.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), up));
        up
    }
}

/// Replace the reachability check (tests).
pub fn set_probe(d: &Daemon, f: impl Fn() -> bool + Send + Sync + 'static) {
    *d.resume.probe.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(f));
    *d.resume.online.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn reachable(host: &str) -> bool {
    let Ok(addrs) = host.to_socket_addrs() else { return false };
    addrs.into_iter().any(|a| TcpStream::connect_timeout(&a, Duration::from_secs(3)).is_ok())
}

/// Whether a `StopFailure`'s error reads like a lost connection rather than, say, a rate limit.
pub fn is_network_error(error: &str, details: &str) -> bool {
    let text = format!("{error} {details}").to_lowercase();
    ["connection", "network", "offline", "socket", "fetch failed", "timed out", "timeout", "econnreset", "econnrefused", "enotfound", "etimedout", "eai_again", "enetunreach", "ehostunreach"]
        .iter()
        .any(|w| text.contains(w))
}

/// An agent's turn died (`StopFailure`): if it was a lost connection, watch it so it is
/// resumed once the network is back.
pub fn on_failure(d: &Daemon, sid: &Id, error: &str, details: &str) {
    if !is_network_error(error, details) || !d.core().state.setting_bool(Cause::Network.setting()) {
        return;
    }
    d.resume.watch().entry(sid.clone()).or_insert_with(|| Watch::new(Cause::Network, NETWORK_WATCH_FOR));
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("resume".into()).spawn(move || {
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

/// Watch the agents that were mid-turn at `slept_at` (unix): working then, or failed since.
/// `clock.rs` calls it on wake.
pub fn on_wake(d: &Daemon, slept_at: i64) {
    if !d.core().state.setting_bool(Cause::Sleep.setting()) {
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
        watch.entry(id).or_insert_with(|| Watch::new(Cause::Sleep, WATCH_FOR));
    }
}

fn actor(cause: Cause) -> Actor {
    Actor { kind: ActorKind::System, session: None, name: Some(cause.name().into()) }
}

fn tick(d: &Daemon) {
    if d.resume.watch().is_empty() {
        return;
    }
    let (sleep_on, network_on) = {
        let core = d.core();
        (core.state.setting_bool(Cause::Sleep.setting()), core.state.setting_bool(Cause::Network.setting()))
    };
    d.resume.watch().retain(|_, w| match w.cause {
        Cause::Sleep => sleep_on,
        Cause::Network => network_on,
    });
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
        let cause = w.cause;
        drop(watch);
        // Typing `continue` while still offline only fails the turn again: wait for the network.
        if !d.resume.online() {
            continue;
        }
        let mut watch = d.resume.watch();
        let Some(w) = watch.get_mut(&sid) else { continue };
        let at = time::format_unix(time::now_unix() + BACKOFF[w.tries]);
        w.tries += 1;
        w.answered = Some(since);
        drop(watch);
        let added = crate::queue::add(d, &sid, prompt.clone(), true, vec![], SendWhen::At { at }, None, actor(cause), None);
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
