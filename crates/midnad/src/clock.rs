//! Awake time: how long it has been, not counting the hours the Mac slept.
//!
//! The monotonic clock (`Instant`) stops while the Mac sleeps and the wall clock doesn't, so
//! when the wall clock has moved further than the monotonic one since the last look, the
//! difference was spent asleep. Each such gap is kept as a sleep (`$MIDNA_HOME/sleeps.json`,
//! the last `KEEP_SECS`, so a daemon restart doesn't forget last night) and
//! `awake_secs_since` leaves them out. "Nobody touched it for N minutes" checks (idle
//! triggers, queued messages waiting for quiet, needs-you expiry, a queued restart's grace)
//! use it, so they don't all come due at once on wake. Real clock times (`SendWhen::At`,
//! schedules) and calendar periods (event log retention, insights) stay on the wall clock.
use midna_proto::{ClockSleepsParams, ClockSleepsResult, RpcError, SleepPeriod, time};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_secs(1);
/// A wall clock ahead of the monotonic one by more than this was asleep (smaller gaps are
/// scheduling noise or clock nudges).
const SLACK_SECS: i64 = 60;
/// Sleeps older than this are dropped.
pub const KEEP_SECS: i64 = 14 * 24 * 3600;

/// The Mac slept from `from` to `to` (unix seconds).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sleep {
    pub from: i64,
    pub to: i64,
}

struct Inner {
    /// The last look at both clocks.
    mono: Instant,
    wall: i64,
    sleeps: Vec<Sleep>,
    /// When the Mac went to sleep, for a wake nobody has handled yet (`take_wake`).
    woke: Option<i64>,
}

pub struct Clock {
    inner: Mutex<Inner>,
    path: Option<PathBuf>,
}

impl Clock {
    /// A clock keeping its sleeps in `home` (none = not persisted).
    pub fn open(home: Option<&Path>) -> Clock {
        let path = home.map(|h| h.join("sleeps.json"));
        let now = time::now_unix();
        let mut sleeps: Vec<Sleep> = path.as_ref().and_then(|p| std::fs::read(p).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        sleeps.retain(|s| s.to > now - KEEP_SECS && s.from < s.to);
        sleeps.sort_by_key(|s| s.from);
        Clock { inner: Mutex::new(Inner { mono: Instant::now(), wall: now, sleeps, woke: None }), path }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Compare both clocks with the last look; a gap the monotonic clock didn't see is a sleep.
    fn observe(&self) -> MutexGuard<'_, Inner> {
        let mut i = self.inner();
        let now = time::now_unix();
        let mono = i.mono.elapsed().as_secs() as i64;
        let slept = now - i.wall - mono;
        if slept > SLACK_SECS {
            let s = Sleep { from: i.wall, to: i.wall + slept };
            i.sleeps.retain(|s| s.to > now - KEEP_SECS);
            i.sleeps.push(s);
            i.woke = Some(i.woke.unwrap_or(s.from));
            self.save(&i.sleeps);
        }
        i.mono = Instant::now();
        i.wall = now;
        i
    }

    fn save(&self, sleeps: &[Sleep]) {
        let Some(path) = &self.path else { return };
        let Ok(mut bytes) = serde_json::to_vec(sleeps) else { return };
        bytes.push(b'\n');
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    /// Seconds from `since` (unix) to now that the Mac was awake.
    pub fn awake_secs_since(&self, since: i64) -> i64 {
        let now = time::now_unix();
        awake_between(&self.observe().sleeps, since, now)
    }

    /// The sleeps that ended after `since` (unix), oldest first.
    pub fn sleeps_since(&self, since: i64) -> Vec<Sleep> {
        self.observe().sleeps.iter().filter(|s| s.to > since).copied().collect()
    }

    /// When the Mac went to sleep, if it has woken since the last call.
    pub fn take_wake(&self) -> Option<i64> {
        self.observe().woke.take()
    }

    /// Tests: pretend the Mac just woke from `secs` asleep (the wall clock moved on `secs`
    /// further than the monotonic one since the last look).
    #[doc(hidden)]
    pub fn simulate_sleep(&self, secs: i64) {
        let mut i = self.observe();
        i.wall -= secs;
    }
}

/// Seconds between `from` and `to` (unix) outside `sleeps`.
pub fn awake_between(sleeps: &[Sleep], from: i64, to: i64) -> i64 {
    if to <= from {
        return 0;
    }
    let asleep: i64 = sleeps.iter().map(|s| (s.to.min(to) - s.from.max(from)).max(0)).sum();
    (to - from - asleep).max(0)
}

/// `clock.sleeps`.
pub fn sleeps(d: &crate::daemon::Daemon, p: ClockSleepsParams) -> crate::rpc::R {
    let since = match p.since.as_deref() {
        Some(t) => time::parse_rfc3339(t).ok_or_else(|| RpcError::bad_params(format!("since must be RFC 3339, not {t}")))?,
        None => 0,
    };
    let sleeps = d.clock.sleeps_since(since).into_iter().map(|s| SleepPeriod { from: time::format_unix(s.from), to: time::format_unix(s.to), secs: s.to - s.from }).collect();
    crate::rpc::ok(ClockSleepsResult { sleeps })
}

/// Every second: notice a wake and tell `resume`.
pub fn start(d: &Arc<crate::daemon::Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("clock".into()).spawn(move || {
        loop {
            std::thread::sleep(TICK);
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            if let Some(slept_at) = d.clock.take_wake() {
                crate::resume::on_wake(&d, slept_at);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_time_leaves_out_sleeps() {
        let sleeps = [Sleep { from: 100, to: 200 }, Sleep { from: 300, to: 350 }];
        assert_eq!(awake_between(&sleeps, 0, 400), 250);
        // A sleep partly before `from` counts only from `from`.
        assert_eq!(awake_between(&sleeps, 150, 400), 150);
        // Asleep the whole time.
        assert_eq!(awake_between(&sleeps, 110, 190), 0);
        assert_eq!(awake_between(&[], 10, 70), 60);
        assert_eq!(awake_between(&sleeps, 400, 300), 0);
    }

    #[test]
    fn a_wall_clock_jump_is_a_sleep_and_is_kept() {
        let home = std::env::temp_dir().join(format!("midna-clock-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let c = Clock::open(Some(&home));
        let before = time::now_unix();
        assert_eq!(c.take_wake(), None);
        c.simulate_sleep(8 * 3600);
        assert!(c.awake_secs_since(before - 8 * 3600 - 30) <= 31, "the night asleep doesn't count");
        assert!(c.take_wake().is_some_and(|at| (before - 8 * 3600 - at).abs() <= 2));
        assert_eq!(c.take_wake(), None, "one wake, handled once");
        // A restarted daemon still knows about it.
        let again = Clock::open(Some(&home));
        assert_eq!(again.sleeps_since(0).len(), 1);
        assert!(again.awake_secs_since(before - 9 * 3600) <= 3600 + 31);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_small_gap_is_not_a_sleep() {
        let c = Clock::open(None);
        c.simulate_sleep(30);
        assert_eq!(c.take_wake(), None);
        assert!(c.sleeps_since(0).is_empty());
    }
}
