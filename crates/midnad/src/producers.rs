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
//! Never orphaned: each producer's pid (and its start time, against pid reuse) is in
//! `streams/<name>.pid` while it runs. The daemon stops every group when it stops
//! ([`stop_all`]), and on start ([`start`]) stops any a previous daemon left behind (a crash, an
//! upgrade's exec) before starting every enabled stream. After the Mac wakes every stream
//! starts again ([`on_wake`]).
use crate::daemon::Daemon;
use midna_proto::secrets::Scrubber;
use midna_proto::streams::{EVENTS_KEPT, MAX_PAYLOAD_BYTES, StreamSpec, parse_producer_line};
use midna_proto::*;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Epochs are global, so a stream removed and added again never matches an old thread.
static EPOCH: AtomicU64 = AtomicU64::new(0);

fn next_epoch() -> u64 {
    EPOCH.fetch_add(1, Ordering::Relaxed) + 1
}

/// The producers' book-keeping. Never held together with `core` or `streams`.
#[derive(Default)]
pub struct Producers {
    slots: Mutex<HashMap<String, Slot>>,
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
        if let Some(pid) = s.pid {
            signal_group(pid, libc::SIGTERM);
        }
    }
    let (w, n) = (Arc::downgrade(d), name.to_string());
    let _ = std::thread::Builder::new().name(format!("stream-{name}")).spawn(move || supervise(w, n, epoch));
}

/// Stop stream `name`'s producer and keep it stopped (paused).
pub fn pause(d: &Daemon, name: &str) {
    let mut g = d.producers.lock();
    let s = g.entry(name.to_string()).or_default();
    s.epoch = next_epoch();
    s.state = StreamState::Paused;
    s.retry_at = None;
    if let Some(pid) = s.pid {
        signal_group(pid, libc::SIGTERM);
    }
}

/// Stop stream `name`'s producer and forget it (removed).
pub fn forget(d: &Daemon, name: &str) {
    if let Some(pid) = d.producers.lock().remove(name).and_then(|s| s.pid) {
        signal_group(pid, libc::SIGTERM);
    }
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

/// Stream `name` as `streams.list` shows it.
pub fn status(d: &Daemon, name: &str) -> Option<StreamStatus> {
    let spec = d.core().state.streams.iter().find(|s| s.name == name).cloned()?;
    let last_event = d.streams.events(name, EVENTS_KEPT).0.into_iter().find(|r| r.verdict == StreamVerdict::Accepted);
    let approval = spec.needs_approval.then(|| crate::rpc::streams::approval_of(d, name)).flatten();
    let g = d.producers.lock();
    let s = g.get(name);
    let state = match () {
        _ if spec.needs_approval => StreamState::AwaitingApproval,
        _ if !spec.enabled => StreamState::Paused,
        _ => s.map_or(StreamState::Starting, |s| s.state),
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
        retry_at: s.and_then(|s| s.retry_at.clone()).filter(|_| state == StreamState::Backoff),
        error: s.and_then(|s| s.error.clone()),
        last_event,
    })
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
        let ran = match spawn(&d, &spec, &log) {
            Ok(child) => {
                let pid = child.id() as i32;
                let began = Instant::now();
                let start = crate::procs::table_start(pid).unwrap_or(0);
                let _ = std::fs::write(pid_path(&d, &name), format!("{pid} {start}\n"));
                d.producers.update(&name, epoch, |s| {
                    s.state = StreamState::Running;
                    s.pid = Some(pid);
                    s.started_at = Some(time::now_rfc3339());
                    s.error = None;
                    s.retry_at = None;
                });
                log.line(&format!("started: pid {pid}"));
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
                let mut g = d.producers.lock();
                if let Some(s) = g.get_mut(&name) {
                    if s.pid == Some(pid) {
                        s.pid = None;
                    }
                    if s.epoch == epoch {
                        s.last_exit = Some(exit);
                    }
                }
                ran
            }
            Err(e) => {
                log.line(&format!("couldn't start: {e}"));
                d.producers.update(&name, epoch, |s| s.error = Some(e));
                Duration::ZERO
            }
        };
        let wait_for = next_backoff(delay, ran);
        delay = Some(wait_for);
        let at = time::format_unix(time::now_unix() + wait_for.as_secs().max(1) as i64);
        let scheduled = w.upgrade().is_some_and(|d| {
            !d.shutting_down.load(Ordering::Relaxed)
                && d.producers.update(&name, epoch, |s| {
                    s.state = StreamState::Backoff;
                    s.retry_at = Some(at);
                    s.restarts += 1;
                })
        });
        if !scheduled {
            return;
        }
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
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return Some(s),
            Ok(None) => {}
            Err(_) => return None,
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

fn spawn(d: &Arc<Daemon>, spec: &StreamSpec, log: &Arc<Log>) -> Result<Child, String> {
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
    cmd.env_remove("MIDNA_SESSION").env_remove("MIDNA_TRIGGER").env_remove("MIDNA_DELIVERY").env("MIDNA_STREAM", &spec.name);
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
    let masks: Vec<(String, Vec<u8>)> = secrets.into_iter().map(|(k, v)| (k, v.into_bytes())).collect();
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
