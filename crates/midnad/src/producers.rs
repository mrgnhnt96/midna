//! Streams midnad runs (`streams.add`): each enabled stream's producer is a command midnad
//! starts in its own session and process group, with the terminal environment plus
//! `MIDNA_STREAM` and the stream's secrets (looked up at each start, `rpc::secret::stream_env`).
//! Its stdout is one JSON object per line (`{"kind":"…","id":"…","data":{…}}`), each handed to
//! the same intake as `streams.publish` (`rpc::streams::admit`), so dedupe and limits apply.
//! Lines that aren't events, publishes the intake drops and its stderr go to
//! `$MIDNA_HOME/streams/<name>.log` (capped, one older file kept), with stored secret values
//! masked, as are stdout lines before they're read.
//!
//! One thread per stream starts the producer, waits for it and, when it exits, starts it again
//! after a backoff (1s, doubling to 5 minutes, back to 1s once a run lasted 10 minutes).
//! Pausing, removing or restarting a stream bumps its epoch: the thread that started the
//! old producer stops its group (SIGTERM, SIGKILL after a grace) and ends. When the leader exits
//! on its own, what's left of its group is stopped too, so nothing it started outlives it.
//!
//! Each start gives the producer a new random token in `MIDNA_STREAM_TOKEN`, kept only here in
//! memory (masked in its log like a secret): once a stream is declared, `streams.publish` to it
//! takes only calls that carry it (the CLI forwards it as `caller.stream_token`) or the human's,
//! so nobody else on the socket can fake its events ([`token_ok`]). A restart, pause or exit
//! drops the old one.
//!
//! Never orphaned: each producer's pid (and its start time, against pid reuse) is in
//! `streams/<name>.pid` while it runs. The daemon stops every group when it stops
//! ([`stop_all`]), and on start ([`start`]) stops any a previous daemon left behind (a crash, an
//! upgrade's exec) before starting every enabled stream. After the Mac wakes every stream
//! starts again ([`on_wake`]).
//!
//! Health: a stream whose producer exits (or can't start) 5 times within 10 minutes is failing.
//! midnad keeps starting it again, raises one `stream_failing` needs-you item (never a second:
//! an open one is found by its `stream`, also after the daemon restarts) and resolves it once a
//! run lasts 10 minutes, or when the stream is paused or removed. Each change of a stream's
//! state is published as a `stream.health` event.
use crate::daemon::Daemon;
use midna_proto::secrets::Scrubber;
use midna_proto::streams::{EVENTS_KEPT, FAILING_EXITS, FAILING_WINDOW_SECS, HEALTHY_AFTER_SECS, LOG_TAIL_LINES, MAX_PAYLOAD_BYTES, StreamSpec, parse_producer_line};
use midna_proto::*;
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

pub const FIRST_BACKOFF: Duration = Duration::from_secs(1);
pub const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// A run this long puts the backoff back at [`FIRST_BACKOFF`].
pub const STEADY: Duration = Duration::from_secs(600);
/// A stream's log is moved to `<name>.log.1` when it would pass this.
pub const LOG_MAX: u64 = 1024 * 1024;
/// SIGTERM, then SIGKILL this much later.
const GRACE: Duration = Duration::from_millis(1500);
const POLL: Duration = Duration::from_millis(100);
/// The longest stdout line read as an event; longer ones are dropped.
const MAX_LINE: usize = MAX_PAYLOAD_BYTES + 4096;
/// How much of a line goes in the log.
const LOG_CLIP: usize = 2000;
const FAILING_WINDOW: Duration = Duration::from_secs(FAILING_WINDOW_SECS);
const HEALTHY_AFTER: Duration = Duration::from_secs(HEALTHY_AFTER_SECS);
/// How much of a log's end is read for its last lines.
const TAIL_BYTES: u64 = 16 * 1024;
/// How much of the reason a `stream_failing` item's title shows.
const WHY_CLIP: usize = 160;

/// Epochs are global, so a stream removed and added again never matches an old thread.
static EPOCH: AtomicU64 = AtomicU64::new(0);

fn next_epoch() -> u64 {
    EPOCH.fetch_add(1, Ordering::Relaxed) + 1
}

/// The producers' book-keeping. Never held together with `core` or `streams`.
#[derive(Default)]
pub struct Producers {
    slots: Mutex<HashMap<String, Slot>>,
    /// Tests: backoffs are a twentieth as long ([`quick_backoff`]).
    quick: AtomicBool,
}

#[derive(Default)]
struct Slot {
    /// The thread allowed to run this stream's producer.
    epoch: u64,
    state: StreamState,
    pid: Option<i32>,
    started_at: Option<String>,
    restarts: u32,
    last_exit: Option<StreamExit>,
    retry_at: Option<String>,
    error: Option<String>,
    /// The running producer's `MIDNA_STREAM_TOKEN`. Never stored, logged or shown.
    token: Option<String>,
    /// When its producer exited (or couldn't start) within the last [`FAILING_WINDOW`].
    exits: VecDeque<Instant>,
    /// Since when its producer has been running.
    up_since: Option<Instant>,
    /// Failing ([`FAILING_EXITS`] exits within [`FAILING_WINDOW`]): since when.
    failing_since: Option<String>,
    /// Its open `stream_failing` item.
    needs_you: Option<Id>,
    /// The state last published as `stream.health`.
    shown: Option<StreamState>,
}

impl Slot {
    /// Its state as shown: failing sticks through its restarts until it runs
    /// [`HEALTHY_AFTER`], unless it is paused.
    fn health(&self) -> StreamState {
        match self.state {
            StreamState::Paused => StreamState::Paused,
            _ if self.failing_since.is_some() => StreamState::Failing,
            s => s,
        }
    }

    /// An exit at `now`: drop the ones older than [`FAILING_WINDOW`]. True when this one makes
    /// the stream failing.
    fn exited(&mut self, now: Instant) -> bool {
        self.exits.push_back(now);
        while self.exits.front().is_some_and(|t| now.duration_since(*t) > FAILING_WINDOW) {
            self.exits.pop_front();
        }
        if self.failing_since.is_none() && self.exits.len() >= FAILING_EXITS {
            self.failing_since = Some(time::now_rfc3339());
            return true;
        }
        false
    }

    fn exits_recent(&self) -> u32 {
        self.exits.iter().filter(|t| t.elapsed() <= FAILING_WINDOW).count() as u32
    }

    /// No longer failing: forget its exits. True if it was failing.
    fn clear_failing(&mut self) -> bool {
        self.exits.clear();
        self.needs_you = None;
        self.failing_since.take().is_some()
    }
}

impl Producers {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Slot>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn current(&self, name: &str, epoch: u64) -> bool {
        self.lock().get(name).is_some_and(|s| s.epoch == epoch)
    }

    /// Change the slot if `epoch` still owns it.
    fn update(&self, name: &str, epoch: u64, f: impl FnOnce(&mut Slot)) -> bool {
        match self.lock().get_mut(name) {
            Some(s) if s.epoch == epoch => {
                f(s);
                true
            }
            _ => false,
        }
    }
}

/// The wait before starting a producer again: `prev` doubled (to [`MAX_BACKOFF`]), or
/// [`FIRST_BACKOFF`] for the first exit and after a run of [`STEADY`] or more.
pub fn next_backoff(prev: Option<Duration>, ran: Duration) -> Duration {
    match prev {
        Some(p) if ran < STEADY => (p * 2).min(MAX_BACKOFF),
        _ => FIRST_BACKOFF,
    }
}

pub fn dir(d: &Daemon) -> PathBuf {
    d.cfg.home.join("streams")
}

pub fn log_path(d: &Daemon, name: &str) -> PathBuf {
    dir(d).join(format!("{name}.log"))
}

fn pid_path(d: &Daemon, name: &str) -> PathBuf {
    dir(d).join(format!("{name}.pid"))
}

/// Should midnad run this stream: on, and not waiting for the human's approval.
fn runs(s: &StreamSpec) -> bool {
    s.enabled && !s.needs_approval
}

/// On daemon start: stop what an earlier daemon left running, then start every enabled stream.
pub fn start(d: &Arc<Daemon>) {
    reap_leftovers(&dir(d));
    adopt_failing(d);
    let names: Vec<String> = d.core().state.streams.iter().filter(|s| runs(s)).map(|s| s.name.clone()).collect();
    for n in names {
        start_one(d, &n);
    }
}

/// After a wake: start every enabled stream again (its connections may be long dead).
pub fn on_wake(d: &Arc<Daemon>) {
    let names: Vec<String> = d.core().state.streams.iter().filter(|s| runs(s)).map(|s| s.name.clone()).collect();
    for n in names {
        start_one(d, &n);
    }
}

/// Start stream `name`'s producer now; a running one is stopped first.
pub fn start_one(d: &Arc<Daemon>, name: &str) {
    let epoch = next_epoch();
    {
        let mut g = d.producers.lock();
        let s = g.entry(name.to_string()).or_default();
        s.epoch = epoch;
        s.state = StreamState::Starting;
        s.retry_at = None;
        s.token = None;
        s.up_since = None;
        if let Some(pid) = s.pid {
            signal_group(pid, libc::SIGTERM);
        }
    }
    publish_health(d, name);
    let (w, n) = (Arc::downgrade(d), name.to_string());
    let _ = std::thread::Builder::new().name(format!("stream-{name}")).spawn(move || supervise(w, n, epoch));
}

/// Stop stream `name`'s producer and keep it stopped (paused). A failing stream's item
/// resolves: the human chose to stop it.
pub fn pause(d: &Daemon, name: &str) {
    {
        let mut g = d.producers.lock();
        let s = g.entry(name.to_string()).or_default();
        s.epoch = next_epoch();
        s.state = StreamState::Paused;
        s.retry_at = None;
        s.up_since = None;
        s.token = None;
        s.clear_failing();
        if let Some(pid) = s.pid {
            signal_group(pid, libc::SIGTERM);
        }
    }
    resolve_failing(d, name, "paused");
    publish_health(d, name);
}

/// Stop stream `name`'s producer and forget it (removed).
pub fn forget(d: &Daemon, name: &str) {
    if let Some(pid) = d.producers.lock().remove(name).and_then(|s| s.pid) {
        signal_group(pid, libc::SIGTERM);
    }
    resolve_failing(d, name, "removed");
}

/// The daemon is stopping: stop every producer's group and wait for them (SIGKILL after a grace).
pub fn stop_all(d: &Daemon) {
    let pids: Vec<i32> = {
        let mut g = d.producers.lock();
        for s in g.values_mut() {
            s.epoch = next_epoch();
        }
        g.values().filter_map(|s| s.pid).collect()
    };
    stop_groups(&pids, false);
    let names: Vec<String> = d.producers.lock().keys().cloned().collect();
    for n in names {
        let _ = std::fs::remove_file(pid_path(d, &n));
    }
}

/// Whether `token` is stream `name`'s running producer's (`MIDNA_STREAM_TOKEN`).
pub fn token_ok(d: &Daemon, name: &str, token: Option<&str>) -> bool {
    let Some(token) = token else { return false };
    d.producers.lock().get(name).and_then(|s| s.token.as_deref()).is_some_and(|t| same(t.as_bytes(), token.as_bytes()))
}

/// Compare in time that doesn't depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Stream `name` as `streams.list` shows it.
pub fn status(d: &Daemon, name: &str) -> Option<StreamStatus> {
    let spec = d.core().state.streams.iter().find(|s| s.name == name).cloned()?;
    let (records, counts) = d.streams.events(name, EVENTS_KEPT);
    let last_event = records.into_iter().find(|r| r.verdict == StreamVerdict::Accepted);
    let approval = spec.needs_approval.then(|| crate::rpc::streams::approval_of(d, name)).flatten();
    let log_tail = log_tail(&log_path(d, name), LOG_TAIL_LINES);
    let g = d.producers.lock();
    let s = g.get(name);
    let state = shown_state(&spec, s);
    let raw = s.map_or(StreamState::Starting, |s| s.state);
    let backoff = runs(&spec) && raw == StreamState::Backoff;
    let why = match state {
        _ if state.healthy() => None,
        StreamState::AwaitingApproval => Some(AWAITING_WHY.into()),
        _ => why_of(s.and_then(|s| s.error.as_deref()), &log_tail, s.and_then(|s| s.last_exit.as_ref())),
    };
    Some(StreamStatus {
        log: log_path(d, name).to_string_lossy().into_owned(),
        name: spec.name,
        command: spec.command,
        cwd: spec.cwd,
        project_id: spec.project_id,
        secrets: spec.secrets,
        enabled: spec.enabled,
        state,
        approval,
        pid: s.and_then(|s| s.pid).map(|p| p as u32),
        started_at: s.and_then(|s| s.pid.and(s.started_at.clone())),
        restarts: s.map_or(0, |s| s.restarts),
        last_exit: s.and_then(|s| s.last_exit.clone()),
        retry_at: s.and_then(|s| s.retry_at.clone()).filter(|_| backoff),
        error: s.and_then(|s| s.error.clone()),
        last_event,
        counts,
        exits_recent: s.map_or(0, Slot::exits_recent),
        failing_since: s.and_then(|s| s.failing_since.clone()).filter(|_| state == StreamState::Failing),
        why,
        needs_you: s.and_then(|s| s.needs_you.clone()).filter(|_| state == StreamState::Failing),
        log_tail,
    })
}

/// How every stream is doing, in short (`daemon.info`).
pub fn summary(d: &Daemon) -> StreamsHealth {
    let names: Vec<String> = d.core().state.streams.iter().map(|s| s.name.clone()).collect();
    let mut out = StreamsHealth { total: names.len() as u32, ..Default::default() };
    for st in names.iter().filter_map(|n| status(d, n)) {
        if st.state.healthy() {
            out.healthy += 1;
        } else {
            out.unhealthy.push(StreamBrief { name: st.name, state: st.state, why: st.why, retry_at: st.retry_at });
        }
    }
    out
}

const AWAITING_WHY: &str = "an agent added or changed it: it waits for the human's approval";

/// A stream's state as shown: awaiting approval, paused, else its producer's (failing sticks).
fn shown_state(spec: &StreamSpec, s: Option<&Slot>) -> StreamState {
    match s {
        _ if spec.needs_approval => StreamState::AwaitingApproval,
        _ if !spec.enabled => StreamState::Paused,
        Some(s) => s.health(),
        None => StreamState::Starting,
    }
}

/// Publish `stream.health` if stream `name`'s state changed since it was last published.
pub fn publish_health(d: &Daemon, name: &str) {
    let Some(spec) = d.core().state.streams.iter().find(|s| s.name == name).cloned() else { return };
    let (mut health, error, last_exit) = {
        let mut g = d.producers.lock();
        let s = g.entry(name.to_string()).or_default();
        let state = shown_state(&spec, Some(&*s));
        if s.shown == Some(state) {
            return;
        }
        let from = s.shown.replace(state);
        let health = StreamHealth {
            stream: name.to_string(),
            state,
            from,
            why: None,
            retry_at: s.retry_at.clone().filter(|_| s.state == StreamState::Backoff),
            restarts: s.restarts,
            exits_recent: s.exits_recent(),
            needs_you: s.needs_you.clone().filter(|_| state == StreamState::Failing),
        };
        (health, s.error.clone(), s.last_exit.clone())
    };
    if health.state == StreamState::AwaitingApproval {
        health.why = Some(AWAITING_WHY.into());
    } else if !health.state.healthy() {
        health.why = why_of(error.as_deref(), &log_tail(&log_path(d, name), LOG_TAIL_LINES), last_exit.as_ref());
    }
    d.emit(kinds::STREAM_HEALTH, stream_actor(name), spec.project_id, None, serde_json::to_value(&health).unwrap_or_default());
}

fn stream_actor(name: &str) -> Actor {
    Actor { name: Some(format!("stream {name}")), ..Actor::system() }
}

/// Stream `name` just became failing (`epoch`'s run): raise its `stream_failing` item, or
/// take the one already open.
fn raise_failing(d: &Daemon, name: &str, epoch: u64) {
    let open = d.core().state.needs_you.iter().find(|n| n.kind == NeedsYouKind::StreamFailing && n.stream.as_deref() == Some(name)).map(|n| n.id.clone());
    let id = match open {
        Some(id) => id,
        None => {
            let Some(st) = status(d, name) else { return };
            let why = st.why.clone().unwrap_or_else(|| "its producer keeps exiting".into());
            let why = if why.chars().count() > WHY_CLIP { format!("{}…", why.chars().take(WHY_CLIP).collect::<String>()) } else { why };
            let mut item = d.new_needs_you(NeedsYouKind::StreamFailing, format!("Stream {name} keeps failing: {why}"), stream_actor(name), None);
            item.project_id = st.project_id.clone();
            item.stream = Some(name.to_string());
            item.detail = format!(
                "Its producer exited {} times in 10 minutes ({} restarts in all). midna keeps starting it again, waiting up to 5 \
                 minutes between tries. Its log: {}. Fix the cause, then `midna stream restart {name}`, or stop it with \
                 `midna stream pause {name}`. This clears itself once it runs 10 minutes.",
                st.exits_recent, st.restarts, st.log
            );
            d.raise_needs_you(item).id
        }
    };
    if !d.producers.update(name, epoch, |s| s.needs_you = Some(id.clone())) {
        // Paused, removed or restarted meanwhile: whatever did that settles it.
        resolve_failing(d, name, "stopped");
    }
}

/// Close stream `name`'s open `stream_failing` items (it is healthy, paused or removed).
fn resolve_failing(d: &Daemon, name: &str, how: &str) {
    let ids: Vec<Id> = d.core().state.needs_you.iter().filter(|n| n.kind == NeedsYouKind::StreamFailing && n.stream.as_deref() == Some(name)).map(|n| n.id.clone()).collect();
    for id in ids {
        d.close_needs_you(&id, json!({ "kind": "done", "auto": true, "stream": how }), stream_actor(name));
    }
}

/// `epoch`'s producer has run [`HEALTHY_AFTER`]: a failing stream is healthy again.
fn recovered(d: &Daemon, name: &str, epoch: u64) {
    let mut was = false;
    d.producers.update(name, epoch, |s| was = s.clear_failing());
    if was {
        resolve_failing(d, name, "healthy");
        publish_health(d, name);
    }
}

/// On daemon start: open `stream_failing` items (from before a restart or an upgrade) belong
/// to their streams again, still failing until a run lasts [`HEALTHY_AFTER`]. One for a stream
/// that's gone or paused resolves.
fn adopt_failing(d: &Daemon) {
    let items: Vec<(Id, Option<String>, String)> =
        d.core().state.needs_you.iter().filter(|n| n.kind == NeedsYouKind::StreamFailing).map(|n| (n.id.clone(), n.stream.clone(), n.created_at.clone())).collect();
    for (id, stream, at) in items {
        let runs = stream.as_deref().is_some_and(|n| d.core().state.streams.iter().any(|s| s.name == n && runs(s)));
        let adopted = runs && {
            let mut g = d.producers.lock();
            let s = g.entry(stream.clone().unwrap_or_default()).or_default();
            match &s.needs_you {
                Some(_) => false,
                None => {
                    s.needs_you = Some(id.clone());
                    s.failing_since = Some(at);
                    true
                }
            }
        };
        if !adopted {
            d.close_needs_you(&id, json!({ "kind": "done", "auto": true, "stream": if runs { "duplicate" } else { "gone" } }), Actor::system());
        }
    }
}

/// Tests: backoffs a twentieth as long, so a stream fails within a second or two.
#[doc(hidden)]
pub fn quick_backoff(d: &Daemon) {
    d.producers.quick.store(true, Ordering::Relaxed);
}

/// Tests: stream `name`'s producer started `secs` earlier than it did.
#[doc(hidden)]
pub fn backdate_up(d: &Daemon, name: &str, secs: u64) {
    if let Some(s) = d.producers.lock().get_mut(name) {
        s.up_since = s.up_since.and_then(|t| t.checked_sub(Duration::from_secs(secs)));
    }
}

/// The last `n` lines of a stream's log, oldest first.
fn log_tail(path: &Path, n: usize) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(path) else { return vec![] };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(TAIL_BYTES);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return vec![];
    }
    let mut buf = vec![];
    let _ = f.take(TAIL_BYTES).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    if from > 0 && !lines.is_empty() {
        // Read from the middle of a line.
        lines.remove(0);
    }
    lines[lines.len().saturating_sub(n)..].iter().map(|l| l.to_string()).collect()
}

/// Why a stream isn't running, in a line: why it couldn't start, else its last run's last
/// stderr line (from its log's `tail`), else how it last exited.
pub fn why_of(error: Option<&str>, tail: &[String], last_exit: Option<&StreamExit>) -> Option<String> {
    if let Some(e) = error {
        return Some(format!("couldn't start: {e}"));
    }
    for line in tail.iter().rev() {
        // `<time> <text>`
        let text = line.split_once(' ').map_or(line.as_str(), |(_, t)| t);
        if let Some(err) = text.strip_prefix("stderr: ") {
            return Some(err.to_string());
        }
        if text.starts_with("started: ") {
            break;
        }
    }
    last_exit.map(describe)
}

/// One stream's thread: start the producer, wait for it, and start it again after a backoff,
/// for as long as `epoch` owns the stream.
fn supervise(w: Weak<Daemon>, name: String, epoch: u64) {
    let mut delay: Option<Duration> = None;
    loop {
        // A producer the stream had before is still stopping: wait for it (its thread clears `pid`).
        let until = Instant::now() + GRACE + Duration::from_secs(1);
        loop {
            let Some(d) = w.upgrade() else { return };
            let busy = match d.producers.lock().get(&name) {
                Some(s) if s.epoch == epoch => s.pid.is_some(),
                _ => return,
            };
            if !busy || Instant::now() >= until {
                break;
            }
            drop(d);
            std::thread::sleep(POLL);
        }
        let Some(d) = w.upgrade() else { return };
        if d.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let Some(spec) = d.core().state.streams.iter().find(|s| s.name == name && runs(s)).cloned() else { return };
        let log = Log::open(log_path(&d, &name));
        // Set before it starts, so its first publish already has it.
        let token = crate::state::hex_id(64);
        if !d.producers.update(&name, epoch, |s| s.token = Some(token.clone())) {
            return;
        }
        let drop_token = |d: &Daemon| {
            if let Some(s) = d.producers.lock().get_mut(&name)
                && s.token.as_deref() == Some(token.as_str())
            {
                s.token = None;
            }
        };
        let ran = match spawn(&d, &spec, &log, &token) {
            Ok(child) => {
                let pid = child.id() as i32;
                let began = Instant::now();
                let start = crate::procs::table_start(pid).unwrap_or(0);
                let _ = std::fs::write(pid_path(&d, &name), format!("{pid} {start}\n"));
                d.producers.update(&name, epoch, |s| {
                    s.state = StreamState::Running;
                    s.pid = Some(pid);
                    s.started_at = Some(time::now_rfc3339());
                    s.up_since = Some(began);
                    s.error = None;
                    s.retry_at = None;
                });
                log.line(&format!("started: pid {pid}"));
                publish_health(&d, &name);
                drop(d);
                let status = wait(&w, &name, epoch, child, pid);
                // Whatever it started goes with it.
                stop_groups(&[pid], false);
                let ran = began.elapsed();
                let exit = StreamExit {
                    at: time::now_rfc3339(),
                    code: status.and_then(|s| s.code()),
                    signal: status.and_then(|s| s.signal()),
                    ran_secs: ran.as_secs(),
                };
                log.line(&format!("exited: {} after {}", describe(&exit), human(ran)));
                let Some(d) = w.upgrade() else { return };
                remove_pidfile(&pid_path(&d, &name), pid);
                drop_token(&d);
                let mut g = d.producers.lock();
                if let Some(s) = g.get_mut(&name) {
                    if s.pid == Some(pid) {
                        s.pid = None;
                    }
                    if s.epoch == epoch {
                        s.last_exit = Some(exit);
                        s.up_since = None;
                    }
                }
                ran
            }
            Err(e) => {
                log.line(&format!("couldn't start: {e}"));
                drop_token(&d);
                d.producers.update(&name, epoch, |s| s.error = Some(e));
                Duration::ZERO
            }
        };
        let backoff = next_backoff(delay, ran);
        delay = Some(backoff);
        let Some(d) = w.upgrade() else { return };
        let wait_for = if d.producers.quick.load(Ordering::Relaxed) { backoff / 20 } else { backoff };
        let at = time::format_unix(time::now_unix() + wait_for.as_secs().max(1) as i64);
        let mut failing = false;
        let scheduled = !d.shutting_down.load(Ordering::Relaxed)
            && d.producers.update(&name, epoch, |s| {
                s.state = StreamState::Backoff;
                s.retry_at = Some(at);
                s.restarts += 1;
                failing = s.exited(Instant::now());
            });
        if !scheduled {
            return;
        }
        if failing {
            log.line(&format!("failing: {FAILING_EXITS} exits within {}", human(FAILING_WINDOW)));
            raise_failing(&d, &name, epoch);
        }
        publish_health(&d, &name);
        drop(d);
        log.line(&format!("starting again in {}", human(wait_for)));
        let until = Instant::now() + wait_for;
        while Instant::now() < until {
            std::thread::sleep(POLL.min(until.saturating_duration_since(Instant::now())));
            if !w.upgrade().is_some_and(|d| d.producers.current(&name, epoch)) {
                return;
            }
        }
    }
}

/// Wait for the producer to exit; once `epoch` no longer owns the stream (or the daemon is
/// stopping), stop its group: SIGTERM, then SIGKILL after [`GRACE`].
fn wait(w: &Weak<Daemon>, name: &str, epoch: u64, mut child: Child, pid: i32) -> Option<ExitStatus> {
    let mut term_at: Option<Instant> = None;
    let mut steady = false;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => {}
            Err(_) => return None,
        }
        // A run of HEALTHY_AFTER makes a failing stream healthy again.
        if !steady && let Some(d) = w.upgrade() {
            let up = d.producers.lock().get(name).is_some_and(|s| s.epoch == epoch && s.up_since.is_some_and(|t| t.elapsed() >= HEALTHY_AFTER));
            if up {
                steady = true;
                recovered(&d, name, epoch);
            }
        }
        let stale = w.upgrade().is_none_or(|d| d.shutting_down.load(Ordering::Relaxed) || !d.producers.current(name, epoch));
        match term_at {
            None if stale => {
                signal_group(pid, libc::SIGTERM);
                term_at = Some(Instant::now());
            }
            Some(t) if t.elapsed() >= GRACE => signal_group(pid, libc::SIGKILL),
            _ => {}
        }
        std::thread::sleep(POLL);
    }
}

fn spawn(d: &Arc<Daemon>, spec: &StreamSpec, log: &Arc<Log>, token: &str) -> Result<Child, String> {
    if !Path::new(&spec.cwd).is_dir() {
        return Err(format!("its folder {} isn't there", spec.cwd));
    }
    let secrets = crate::rpc::secret::stream_env(d, &spec.name, &spec.secrets, spec.project_id.as_ref()).map_err(|e| e.message)?;
    let argv = crate::rpc::session::exec_argv(&spec.command);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(&spec.cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for k in crate::pty::ENV_SCRUB {
        cmd.env_remove(k);
    }
    for (k, v) in crate::rpc::session::session_env(d, "", spec.project_id.as_deref().unwrap_or("")) {
        match k.as_str() {
            "MIDNA_SESSION" | "COLORTERM" => {}
            "TERM" => {
                cmd.env(&k, "dumb");
            }
            _ => {
                cmd.env(&k, v);
            }
        }
    }
    // Not a terminal's, and no trigger caused it: what it publishes fires triggers.
    cmd.env_remove("MIDNA_SESSION").env_remove("MIDNA_TRIGGER").env_remove("MIDNA_DELIVERY").env("MIDNA_STREAM", &spec.name).env("MIDNA_STREAM_TOKEN", token);
    for (k, v) in &secrets {
        cmd.env(k, v);
    }
    unsafe {
        cmd.pre_exec(|| {
            // Its own session and process group, so stopping it stops everything it started.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            Ok(())
        });
    }
    let mut child = {
        let _spawns = crate::pty::spawn_lock();
        cmd.spawn().map_err(|e| format!("{}: {e}", spec.command.join(" ")))?
    };
    let mut masks: Vec<(String, Vec<u8>)> = secrets.into_iter().map(|(k, v)| (k, v.into_bytes())).collect();
    masks.push(("MIDNA_STREAM_TOKEN".into(), token.as_bytes().to_vec()));
    let mask = || Scrubber::new(masks.iter().map(|(k, v)| (k.as_str(), v.as_slice())));
    if let Some(out) = child.stdout.take() {
        let (log, mut scrub, name) = (log.clone(), mask(), spec.name.clone());
        let w = Arc::downgrade(d);
        let _ = std::thread::Builder::new().name(format!("stream-{}-out", spec.name)).spawn(move || read_events(w, &name, out, &mut scrub, &log));
    }
    if let Some(err) = child.stderr.take() {
        let (log, mut scrub) = (log.clone(), mask());
        let _ = std::thread::Builder::new().name(format!("stream-{}-err", spec.name)).spawn(move || {
            let mut r = BufReader::new(err);
            let mut buf = vec![];
            while let Some(long) = read_line(&mut r, &mut buf, MAX_LINE) {
                let line = clean(&mut scrub, &buf);
                if !line.trim().is_empty() {
                    log.line(&format!("stderr: {}{}", clip(&line), if long { " …(cut)" } else { "" }));
                }
            }
        });
    }
    Ok(child)
}

/// A producer's stdout: each line an event for the intake; anything else goes in the log.
fn read_events(w: Weak<Daemon>, name: &str, out: impl std::io::Read, scrub: &mut Scrubber, log: &Log) {
    let actor = Actor { name: Some(format!("stream {name}")), ..Actor::system() };
    let mut r = BufReader::new(out);
    let mut buf = vec![];
    while let Some(long) = read_line(&mut r, &mut buf, MAX_LINE) {
        let line = clean(scrub, &buf);
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if long {
            log.line(&format!("stdout: dropped a line over {MAX_LINE} bytes: {}", clip(line)));
            continue;
        }
        let Some(d) = w.upgrade() else { return };
        match parse_producer_line(name, line.as_bytes()) {
            Ok(p) => {
                if let Err(e) = crate::rpc::streams::admit(&d, p, None, actor.clone()) {
                    log.line(&format!("dropped: {}: {}", e.message, clip(line)));
                }
            }
            Err(why) => log.line(&format!("stdout ({why}): {}", clip(line))),
        }
    }
}

/// Read a line into `buf` (without its newline), at most `max` bytes of it; the rest of a
/// longer line is skipped. `Some(cut)`, or None at the end of the stream.
fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>, max: usize) -> Option<bool> {
    buf.clear();
    let mut cut = false;
    loop {
        let avail = match r.fill_buf() {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        };
        if avail.is_empty() {
            return (!buf.is_empty() || cut).then_some(cut);
        }
        let (chunk, used, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (&avail[..i], i + 1, true),
            None => (avail, avail.len(), false),
        };
        let room = max.saturating_sub(buf.len());
        if chunk.len() > room {
            cut = true;
        }
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        r.consume(used);
        if done {
            return Some(cut);
        }
    }
}

/// `line` with stored secret values masked, as text.
fn clean(scrub: &mut Scrubber, line: &[u8]) -> String {
    let mut out = scrub.feed(line);
    out.extend(scrub.finish());
    String::from_utf8_lossy(&out).into_owned()
}

fn clip(s: &str) -> String {
    if s.chars().count() <= LOG_CLIP { s.to_string() } else { format!("{}…", s.chars().take(LOG_CLIP).collect::<String>()) }
}

fn describe(e: &StreamExit) -> String {
    match (e.code, e.signal) {
        (Some(c), _) => format!("exit code {c}"),
        (None, Some(s)) => format!("signal {s}"),
        _ => "lost track of it".into(),
    }
}

fn human(t: Duration) -> String {
    match t.as_secs() {
        s if s < 1 => format!("{}ms", t.as_millis()),
        s if s < 120 => format!("{s}s"),
        s => format!("{}m{}s", s / 60, s % 60),
    }
}

/// Remove a pidfile if it is still `pid`'s (a newer run of the same name may have written it).
fn remove_pidfile(path: &Path, pid: i32) {
    let ours = std::fs::read_to_string(path).is_ok_and(|t| t.split_whitespace().next() == Some(pid.to_string().as_str()));
    if ours {
        let _ = std::fs::remove_file(path);
    }
}

fn signal_group(pid: i32, sig: i32) {
    if pid > 1 {
        unsafe { libc::kill(-pid, sig) };
    }
}

fn group_alive(pid: i32) -> bool {
    pid > 1 && unsafe { libc::kill(-pid, 0) } == 0
}

/// SIGTERM the groups led by `pids`, wait up to [`GRACE`] for them to go, SIGKILL the rest.
/// With `reap`, leaders that are our children but no `Child` waits for (ones a daemon started
/// before an upgrade's exec) are reaped as they exit, so a zombie doesn't keep its group alive.
fn stop_groups(pids: &[i32], reap: bool) {
    let reap_all = |ps: &[i32]| {
        if reap {
            for &p in ps {
                let mut st = 0;
                unsafe { libc::waitpid(p, &mut st, libc::WNOHANG) };
            }
        }
    };
    let alive: Vec<i32> = pids.iter().copied().filter(|&p| group_alive(p)).collect();
    for &p in &alive {
        signal_group(p, libc::SIGTERM);
    }
    let until = Instant::now() + GRACE;
    loop {
        reap_all(&alive);
        if Instant::now() >= until || !alive.iter().any(|&p| group_alive(p)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for &p in &alive {
        if group_alive(p) {
            signal_group(p, libc::SIGKILL);
        }
    }
    reap_all(&alive);
}

/// Stop producers an earlier daemon left running (its pidfiles), checking each pid still is
/// the process it started (start time), and remove the pidfiles.
pub fn reap_leftovers(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut pids = vec![];
    for e in rd.flatten() {
        let path = e.path();
        if path.extension().is_none_or(|x| x != "pid") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut it = text.split_whitespace().map(|s| s.parse::<i64>().ok());
        if let (Some(Some(pid)), Some(Some(start))) = (it.next(), it.next())
            && pid > 1
            && start != 0
            && crate::procs::table_start(pid as i32) == Some(start)
        {
            pids.push(pid as i32);
        }
        let _ = std::fs::remove_file(&path);
    }
    stop_groups(&pids, true);
}

/// A stream's log file: one timestamped line per entry; moved to `<name>.log.1` (replacing
/// the one before) when it would pass [`LOG_MAX`].
pub struct Log {
    path: PathBuf,
    file: Mutex<Option<(std::fs::File, u64)>>,
}

impl Log {
    pub fn open(path: PathBuf) -> Arc<Log> {
        Arc::new(Log { path, file: Mutex::new(None) })
    }

    pub fn line(&self, text: &str) {
        let entry = format!("{} {text}\n", time::now_rfc3339());
        let mut g = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            if let Some(dir) = self.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) else { return };
            let size = f.metadata().map(|m| m.len()).unwrap_or(0);
            *g = Some((f, size));
        }
        if g.as_ref().is_some_and(|(_, size)| size + entry.len() as u64 > LOG_MAX) {
            *g = None;
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
            let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) else { return };
            *g = Some((f, 0));
        }
        if let Some((f, size)) = g.as_mut()
            && f.write_all(entry.as_bytes()).is_ok()
        {
            *size += entry.len() as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_exits_within_ten_minutes_make_a_stream_failing_once() {
        let mut s = Slot::default();
        let t0 = Instant::now();
        // Spread out: never 5 inside the window.
        for i in 0..8u64 {
            assert!(!s.exited(t0 + Duration::from_secs(i * 200)));
        }
        assert_eq!(s.exits.len(), 4);
        assert!(s.failing_since.is_none());
        let t1 = t0 + Duration::from_secs(5000);
        let fails: Vec<bool> = (0..7u64).map(|i| s.exited(t1 + Duration::from_secs(i))).collect();
        assert_eq!(fails.iter().filter(|f| **f).count(), 1, "{fails:?}");
        assert_eq!(s.health(), StreamState::Failing);
        s.state = StreamState::Running;
        assert_eq!(s.health(), StreamState::Failing, "failing sticks through a restart");
        s.state = StreamState::Paused;
        assert_eq!(s.health(), StreamState::Paused);
        assert!(s.clear_failing());
        assert!(s.exits.is_empty() && !s.clear_failing());
    }

    #[test]
    fn why_takes_the_start_error_then_the_last_runs_stderr_then_the_exit() {
        let exit = StreamExit { at: "t".into(), code: Some(3), signal: None, ran_secs: 1 };
        let tail = |l: &[&str]| l.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(why_of(Some("no such file"), &[], Some(&exit)).unwrap(), "couldn't start: no such file");
        let t = tail(&["t0 stderr: old", "t1 started: pid 4", "t2 stderr: invalid_auth", "t3 exited: exit code 3 after 1s"]);
        assert_eq!(why_of(None, &t, Some(&exit)).unwrap(), "invalid_auth");
        let t = tail(&["t0 stderr: old", "t1 started: pid 4", "t3 exited: exit code 3 after 1s"]);
        assert_eq!(why_of(None, &t, Some(&exit)), Some(describe(&exit)));
        assert_eq!(why_of(None, &[], None), None);
    }

    #[test]
    fn log_tail_reads_the_last_lines_of_a_big_log() {
        let p = std::env::temp_dir().join(format!("midna-tail-{}.log", std::process::id()));
        let body: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, body).unwrap();
        assert_eq!(log_tail(&p, 3), ["line 4997", "line 4998", "line 4999"]);
        std::fs::write(&p, "a\nb\n").unwrap();
        assert_eq!(log_tail(&p, 10), ["a", "b"]);
        std::fs::remove_file(&p).ok();
        assert!(log_tail(&p, 3).is_empty());
    }

    #[test]
    fn backoff_doubles_to_five_minutes_and_resets_after_a_steady_run() {
        let quick = Duration::from_secs(3);
        let mut d = next_backoff(None, quick);
        let mut seen = vec![d.as_secs()];
        for _ in 0..10 {
            d = next_backoff(Some(d), quick);
            seen.push(d.as_secs());
        }
        assert_eq!(seen, [1, 2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        assert_eq!(next_backoff(Some(MAX_BACKOFF), STEADY), FIRST_BACKOFF);
        assert_eq!(next_backoff(Some(MAX_BACKOFF), STEADY - Duration::from_secs(1)), MAX_BACKOFF);
    }

    #[test]
    fn log_is_capped_and_keeps_one_older_file() {
        let dir = std::env::temp_dir().join(format!("midna-stream-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = Log::open(dir.join("s.log"));
        let line = "x".repeat(1000);
        for _ in 0..(LOG_MAX as usize / 1000 + 50) {
            log.line(&line);
        }
        let size = |p: &str| std::fs::metadata(dir.join(p)).map(|m| m.len()).unwrap_or(0);
        assert!(size("s.log") <= LOG_MAX && size("s.log") > 0, "{}", size("s.log"));
        assert!(size("s.log.1") <= LOG_MAX && size("s.log.1") > LOG_MAX / 2, "{}", size("s.log.1"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_lines_are_cut_and_the_rest_skipped() {
        let mut r = std::io::Cursor::new(b"short\nabcdefghij\n\nlast".to_vec());
        let mut buf = vec![];
        assert_eq!(read_line(&mut r, &mut buf, 5), Some(false));
        assert_eq!(buf, b"short");
        assert_eq!(read_line(&mut r, &mut buf, 5), Some(true));
        assert_eq!(buf, b"abcde");
        assert_eq!(read_line(&mut r, &mut buf, 5), Some(false));
        assert_eq!(buf, b"");
        assert_eq!(read_line(&mut r, &mut buf, 5), Some(false));
        assert_eq!(buf, b"last");
        assert_eq!(read_line(&mut r, &mut buf, 5), None);
    }

    #[test]
    fn masks_secret_values_in_lines() {
        let mut s = Scrubber::new([("TOK", b"s3cr3t-value".as_slice())]);
        assert_eq!(clean(&mut s, br#"{"kind":"k","data":{"t":"s3cr3t-value"}}"#), r#"{"kind":"k","data":{"t":"‹TOK›"}}"#);
        assert_eq!(clean(&mut s, b"tail s3cr3t"), "tail s3cr3t", "a held-back prefix comes out at the end of the line");
    }
}
