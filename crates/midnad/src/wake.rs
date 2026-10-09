//! Wake the Mac for scheduled work (`keep_awake.wake`, `keep_awake.wake_setup`).
//!
//! Keep-awake only stops idle sleep, so work scheduled for 7 AM never ran on a Mac that went
//! to sleep at midnight. With `keep_awake.wake` on, midnad keeps one macOS wake scheduled
//! `LEAD` before the next work due inside the keep-awake hours (a schedule trigger, a message
//! queued for a time, an agent's cron wakeup); keep-awake then holds the Mac from that wake
//! until the work has run (`soon`).
//!
//! Scheduling a wake needs root (`pmset schedule wake`). `keep_awake.wake_setup` (human only)
//! asks for an administrator's password once and installs a root-owned helper,
//! `/Library/PrivilegedHelperTools/<home name>.wake`, that takes only `wake|cancel
//! 'MM/DD/YY HH:MM:SS'` and schedules or cancels a wake owned by this midna, plus a sudoers.d
//! entry that lets the user run that helper (and nothing else) without a password. midnad
//! calls it with `sudo -n`. The wakes carry the home's name as their owner, so `pmset -g
//! sched` (readable without root) tells midna's apart from everyone else's and Midna Dev's
//! from Midna's.
//!
//! Only an app's home (`com.mrgnhnt.midna`, `com.mrgnhnt.midna.dev`) gets this; a temp or
//! test home has no helper. Tests use `set_fake`, which records the helper calls instead.
use crate::daemon::Daemon;
use midna_proto::keep_awake::{self as ka, Plan, Today};
use midna_proto::*;
use std::process::Command;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long before the work the Mac wakes.
pub const LEAD: i64 = 120;
/// Work further out than this isn't looked for.
const HORIZON: i64 = 8 * 86_400;
/// How often `pmset -g sched` is read again (a reboot or `pmset schedule cancelall` may have
/// dropped ours).
const RESYNC: Duration = Duration::from_secs(600);
/// How often the grant is checked again.
const READY_EVERY: Duration = Duration::from_secs(60);
/// After a failed helper call, wait this long before trying again.
const RETRY: Duration = Duration::from_secs(60);
const HELPERS: &str = "/Library/PrivilegedHelperTools";
const SUDOERS_D: &str = "/etc/sudoers.d";

#[derive(Default)]
struct Inner {
    /// The wakes macOS has for us (unix, whole minutes): read from `pmset -g sched`, then kept
    /// up to date as we schedule and cancel.
    ours: Vec<i64>,
    synced_at: Option<Instant>,
    ready: bool,
    ready_at: Option<Instant>,
    error: Option<String>,
    failed_at: Option<Instant>,
    /// Tests: helper calls are recorded here instead of run, and the grant counts as installed
    /// once `keep_awake.wake_setup` ran.
    fake: Option<Fake>,
    /// `keep_awake.wake_setup` is waiting on macOS's password dialog.
    asking: Option<Asking>,
}

/// The password dialog runs on its own thread (it waits for the human, longer than any RPC
/// timeout); the next tick picks up how it went.
struct Asking {
    remove: bool,
    by: Actor,
    done: std::thread::JoinHandle<Result<(), String>>,
}

#[derive(Default, Clone)]
struct Fake {
    installed: bool,
    calls: Vec<String>,
}

#[derive(Default)]
pub struct Runtime {
    inner: Mutex<Inner>,
}

impl Runtime {
    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Tests: record helper calls instead of running sudo, and let `keep_awake.wake_setup` install
/// nothing but a flag.
#[doc(hidden)]
pub fn set_fake(d: &Daemon) {
    let mut i = d.wake.inner();
    i.fake = Some(Fake::default());
    i.ready_at = None;
}

/// Tests: the helper calls so far (`wake 10/09/26 06:58:00`, `cancel …`, `setup`, `remove`).
#[doc(hidden)]
pub fn fake_calls(d: &Daemon) -> Vec<String> {
    d.wake.inner().fake.as_ref().map(|f| f.calls.clone()).unwrap_or_default()
}

// ------------------------------------------------------------------ what to wake for

/// The next work due inside the hours, within `HORIZON`: when (unix) and what, in words.
pub fn next_due(d: &Daemon, plan: &Plan, today: Option<&Today>, now: i64) -> Option<(i64, String)> {
    let open = |t: i64| ka::open_at(plan, today, t).is_ok();
    let reopen = |t: i64| ka::next_change(plan, today, t);
    let until = now + HORIZON;
    let mut best: Option<(i64, String)> = None;
    let mut consider = |t: Option<i64>, what: &dyn Fn() -> String| {
        if let Some(t) = t
            && best.as_ref().is_none_or(|(b, _)| t < *b)
        {
            best = Some((t, what()));
        }
    };
    let core = d.core();
    for t in core.state.triggers.iter().filter(|t| crate::local::is_live(t) && t.event.trim().eq_ignore_ascii_case("schedule")) {
        let Ok(s) = cron::Schedule::of(&t.filter, t.fired) else { continue };
        let at = first_open(|at| s.upcoming(at, 1).first().copied(), open, reopen, now, until);
        consider(at, &|| t.name.clone());
    }
    for s in core.state.sessions.iter().filter(|s| s.pid.is_some()) {
        if !s.queue_paused {
            // Only the head goes in; the rest wait for it.
            let at = s.queue.first().filter(|m| m.state != QueueState::Failed).and_then(|m| match &m.when {
                SendWhen::At { at } => time::parse_rfc3339(at),
                _ => None,
            });
            consider(at.filter(|t| *t > now && *t < until && open(*t)), &|| format!("a message queued for {}", s.name));
        }
        for c in s.agent_info.iter().flat_map(|i| i.crons.iter()) {
            let Ok(cr) = cron::Cron::parse(&c.schedule) else { continue };
            let at = first_open(|at| cr.next_after(at), open, reopen, now, until);
            consider(at, &|| format!("{}'s wakeup", s.name));
        }
    }
    best
}

/// The first run (from `next`, which gives the run after a time) that falls inside the hours,
/// before `until`. A run outside them skips ahead to when they open again.
fn first_open(mut next: impl FnMut(i64) -> Option<i64>, open: impl Fn(i64) -> bool, reopen: impl Fn(i64) -> Option<i64>, now: i64, until: i64) -> Option<i64> {
    let mut at = now;
    for _ in 0..64 {
        let t = next(at).filter(|t| *t < until)?;
        if open(t) {
            return Some(t);
        }
        // `next` is strictly after, so one second before the opening finds a run right at it.
        at = reopen(t).map_or(t, |o| (o - 1).max(t));
    }
    None
}

// ------------------------------------------------------------------ keeping the wake scheduled

/// Who this daemon is to macOS, from its home's name: `com.mrgnhnt.midna` or
/// `com.mrgnhnt.midna.dev`. None for any other home (temp, tests).
fn ident(d: &Daemon) -> Option<String> {
    if d.wake.inner().fake.is_some() {
        return Some("com.mrgnhnt.midna.test".into());
    }
    let name = d.cfg.home.file_name()?.to_str()?;
    let app = name == "com.mrgnhnt.midna" || name.strip_prefix("com.mrgnhnt.midna.").is_some_and(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_lowercase()));
    (app && d.cfg.owns_process).then(|| name.to_string())
}

fn helper_path(id: &str) -> String {
    format!("{HELPERS}/{id}.wake")
}

/// sudo skips sudoers.d files with a dot in the name.
fn sudoers_path(id: &str) -> String {
    format!("{SUDOERS_D}/{}-wake", id.replace('.', "-"))
}

/// Make macOS's wakes for us match `want` (the work's time; the wake is `LEAD` before it).
/// Returns the wake status for `keep_awake.status`.
pub fn sync(d: &Daemon, on: bool, want: Option<&(i64, String)>, now: i64) -> KeepAwakeWake {
    answered(d);
    let id = ident(d);
    let mut i = d.wake.inner();
    if i.asking.is_some() {
        return KeepAwakeWake { ready: i.ready, asking: true, line: "Asking for an administrator's password…".into(), ..Default::default() };
    }
    let fake = i.fake.is_some();
    if i.ready_at.is_none_or(|at| at.elapsed() >= READY_EVERY) {
        i.ready = match (&i.fake, &id) {
            (Some(f), _) => f.installed,
            (None, Some(id)) => granted(id),
            (None, None) => false,
        };
        i.ready_at = Some(Instant::now());
    }
    let ready = i.ready;
    let Some(id) = id.clone().filter(|_| ready) else {
        let line = match (on, &id) {
            (_, None) => "Scheduled wake needs Midna's own daemon".to_string(),
            (false, _) => "Off (midna keep-awake wake on)".to_string(),
            (true, _) => "Needs a one-time admin grant: midna keep-awake wake setup".to_string(),
        };
        return KeepAwakeWake { ready, line, error: i.error.clone(), ..Default::default() };
    };
    if !fake && i.synced_at.is_none_or(|at| at.elapsed() >= RESYNC) {
        if let Some(out) = run("/usr/bin/pmset", &["-g", "sched"]) {
            i.ours = parse_sched(&out, &id);
        }
        i.synced_at = Some(Instant::now());
    }
    i.ours.retain(|w| *w > now);
    // Within the lead it's too late to wake for it: we're awake, and keep-awake holds.
    // pmset takes whole minutes.
    let wake_at = if on { want.map(|(t, _)| (t - LEAD).div_euclid(60) * 60).filter(|w| *w > now + 30) } else { None };
    let retry_ok = i.failed_at.is_none_or(|at| at.elapsed() >= RETRY);
    let stale: Vec<i64> = i.ours.iter().copied().filter(|w| Some(*w) != wake_at).collect();
    let missing = wake_at.filter(|w| !i.ours.contains(w));
    if retry_ok && (!stale.is_empty() || missing.is_some()) {
        let mut error = None;
        for w in stale {
            match helper(&mut i, &id, "cancel", w) {
                Ok(()) => i.ours.retain(|o| *o != w),
                Err(e) => error = Some(e),
            }
        }
        if let Some(w) = missing {
            match helper(&mut i, &id, "wake", w) {
                Ok(()) => i.ours.push(w),
                Err(e) => error = Some(e),
            }
        }
        i.failed_at = error.as_ref().map(|_| Instant::now());
        i.error = error;
    }
    let next = i.ours.iter().copied().min();
    let error = i.error.clone();
    drop(i);
    let at = |t: i64| crate::keep_awake::when(t, now);
    let reason = want.map(|(t, what)| format!("{what} at {}", at(*t)));
    let line = match (on, next, &reason) {
        (false, ..) => "Off (midna keep-awake wake on)".to_string(),
        (true, Some(w), Some(r)) => format!("Waking the Mac {} for {r}", at(w)),
        (true, Some(w), None) => format!("Waking the Mac {}", at(w)),
        (true, None, Some(r)) if want.is_some_and(|(t, _)| t - LEAD <= now + 30) => format!("Awake for {r}"),
        (true, None, _) if error.is_some() => format!("Couldn't schedule a wake: {}", error.clone().unwrap_or_default()),
        (true, None, _) => "Nothing scheduled inside the hours to wake for".to_string(),
    };
    KeepAwakeWake { ready, asking: false, next: next.map(time::format_unix), reason, line, error }
}

/// The password dialog closed: on success turn `keep_awake.wake` on (off for a removal) and
/// check the grant again; otherwise keep the error for the status.
fn answered(d: &Daemon) {
    let a = {
        let mut i = d.wake.inner();
        if !i.asking.as_ref().is_some_and(|a| a.done.is_finished()) {
            return;
        }
        i.asking.take()
    };
    let Some(a) = a else { return };
    let r = a.done.join().unwrap_or_else(|_| Err("the password dialog crashed".into()));
    {
        let mut i = d.wake.inner();
        i.ready_at = None;
        i.synced_at = None;
        i.failed_at = None;
        i.error = r.as_ref().err().cloned();
    }
    if r.is_ok() {
        crate::rpc::settings::set_as(d, a.by, "keep_awake.wake", serde_json::Value::Bool(!a.remove));
    }
}

/// Run the helper (`wake` or `cancel` at `w`).
fn helper(i: &mut Inner, id: &str, verb: &str, w: i64) -> Result<(), String> {
    let date = pmset_date(w);
    if let Some(f) = i.fake.as_mut() {
        f.calls.push(format!("{verb} {date}"));
        return Ok(());
    }
    let out = Command::new("/usr/bin/sudo").args(["-n", &helper_path(id), verb, &date]).output().map_err(|e| format!("sudo: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if err.contains("password is required") {
        // The grant is gone: ask again, and say so.
        i.ready_at = None;
        return Err("the admin grant is gone (midna keep-awake wake setup)".into());
    }
    Err(if err.is_empty() { format!("{verb} failed ({})", out.status) } else { err })
}

/// Cancel every wake of ours (before the grant is removed).
fn cancel_all(d: &Daemon, id: &str) {
    let mut i = d.wake.inner();
    if i.fake.is_none()
        && let Some(out) = run("/usr/bin/pmset", &["-g", "sched"])
    {
        i.ours = parse_sched(&out, id);
    }
    for w in std::mem::take(&mut i.ours) {
        let _ = helper(&mut i, id, "cancel", w);
    }
}

/// The grant is installed: the helper is there and sudo lets us run it without a password.
fn granted(id: &str) -> bool {
    let helper = helper_path(id);
    std::path::Path::new(&helper).exists() && Command::new("/usr/bin/sudo").args(["-n", "-l", &helper, "wake"]).output().is_ok_and(|o| o.status.success())
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `10/09/26 06:58:00`: how pmset takes a time (local).
fn pmset_date(t: i64) -> String {
    let (y, m, d, h, mi, _) = time::local_parts(t);
    format!("{m:02}/{d:02}/{:02} {h:02}:{mi:02}:00", y.rem_euclid(100))
}

/// Our wakes in `pmset -g sched`: ` [1]  wake at 10/09/2026 06:58:00 by 'com.mrgnhnt.midna'`.
fn parse_sched(out: &str, owner: &str) -> Vec<i64> {
    let tag = format!(" by '{owner}'");
    out.lines()
        .filter_map(|l| {
            let rest = l.trim_end().strip_suffix(&tag)?;
            let at = rest.split_once("] ").map_or(rest, |(_, r)| r).trim_start().strip_prefix("wake at ")?;
            let (date, clock) = at.trim().split_once(' ')?;
            let mut d = date.split('/').map(|p| p.parse::<i64>().ok());
            let (m, day, y) = (d.next()??, d.next()??, d.next()??);
            let mut c = clock.split(':').map(|p| p.parse::<u32>().ok());
            let (h, mi) = (c.next()??, c.next()??);
            let y = if y < 100 { 2000 + y } else { y };
            Some(time::local_unix(y, m as u32, day as u32, h, mi))
        })
        .collect()
}

// ------------------------------------------------------------------ the grant

/// The helper: schedules or cancels one wake owned by `id`, and nothing else.
fn helper_script(id: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Installed by midna (keep_awake.wake, `midna keep-awake wake setup`). Schedules or cancels\n\
         # one of {id}'s own wakes: wake|cancel 'MM/DD/YY HH:MM:SS'. Remove with\n\
         # `midna keep-awake wake remove`.\n\
         PATH=/usr/bin:/bin\n\
         [ \"$#\" -eq 2 ] || exit 64\n\
         case \"$2\" in\n  \
           [0-9][0-9]/[0-9][0-9]/[0-9][0-9]' '[0-9][0-9]:[0-9][0-9]:[0-9][0-9]) ;;\n  \
           *) exit 64 ;;\n\
         esac\n\
         case \"$1\" in\n  \
           wake) exec /usr/bin/pmset schedule wake \"$2\" '{id}' ;;\n  \
           cancel) exec /usr/bin/pmset schedule cancel wake \"$2\" '{id}' ;;\n  \
           *) exit 64 ;;\n\
         esac\n"
    )
}

fn sudoers(user: &str, id: &str) -> String {
    format!("# Installed by midna (keep_awake.wake): lets {user} schedule {id}'s wakes.\n{user} ALL=(root) NOPASSWD: {} *\n", helper_path(id))
}

/// The admin shell script that installs (or removes) the helper and the sudoers entry. The
/// files are written from heredocs inside it, so nothing the user owns is copied as root.
fn grant_script(user: &str, id: &str, remove: bool) -> String {
    let (helper, sudo) = (helper_path(id), sudoers_path(id));
    if remove {
        return format!("/bin/rm -f '{sudo}' '{helper}'");
    }
    format!(
        "set -e\numask 077\nT=$(/usr/bin/mktemp -d)\ntrap '/bin/rm -rf \"$T\"' EXIT\n\
         /bin/cat > \"$T/helper\" <<'MIDNA_EOF'\n{}MIDNA_EOF\n\
         /bin/cat > \"$T/sudoers\" <<'MIDNA_EOF'\n{}MIDNA_EOF\n\
         /usr/sbin/visudo -cqf \"$T/sudoers\"\n\
         /bin/mkdir -p {HELPERS}\n\
         /usr/bin/install -o root -g wheel -m 755 \"$T/helper\" '{helper}'\n\
         /usr/bin/install -o root -g wheel -m 440 \"$T/sudoers\" '{sudo}'\n",
        helper_script(id),
        sudoers(user, id)
    )
}

/// This user's login name (sudoers needs it), checked to be a plain name.
fn user_name() -> Option<String> {
    let pw = unsafe { libc::getpwuid(libc::getuid()) };
    if pw.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }.to_str().ok()?.to_string();
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))).then_some(name)
}

/// `keep_awake.wake_setup`: install or remove the grant. macOS asks for an administrator's
/// password; the dialog runs in the background and `keep_awake.wake` follows once it's
/// answered (`wake.asking` until then).
pub fn setup(d: &Daemon, ctx: &crate::rpc::Ctx, p: KeepAwakeWakeSetupParams) -> crate::rpc::R {
    let id = ident(d).ok_or_else(|| RpcError::bad_params("scheduled wake needs Midna's or Midna Dev's own daemon; this one runs from another home"))?;
    if d.wake.inner().asking.is_some() {
        return Err(RpcError::bad_params("already asking for an administrator's password"));
    }
    if p.remove {
        cancel_all(d, &id);
    }
    let fake = {
        let mut i = d.wake.inner();
        i.fake.as_mut().map(|f| {
            f.installed = !p.remove;
            f.calls.push(if p.remove { "remove" } else { "setup" }.into());
        })
    };
    let done = if fake.is_some() {
        std::thread::spawn(|| Ok(()))
    } else {
        let user = user_name().ok_or_else(|| RpcError::internal("couldn't read this user's login name"))?;
        let prompt = if p.remove { "Midna wants to stop waking your Mac for scheduled work." } else { "Midna wants to wake your Mac for scheduled work." };
        let script = grant_script(&user, &id, p.remove);
        std::thread::Builder::new()
            .name("wake-grant".into())
            .spawn(move || {
                let out = Command::new("/usr/bin/osascript")
                    .args(["-e", "on run argv", "-e", "do shell script (item 1 of argv) with administrator privileges with prompt (item 2 of argv)", "-e", "end run", &script, prompt])
                    .output()
                    .map_err(|e| format!("osascript: {e}"))?;
                let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                match out.status.success() {
                    true => Ok(()),
                    false if err.contains("-128") => Err("cancelled: no administrator password given".into()),
                    false => Err(format!("installing the grant failed: {err}")),
                }
            })
            .map_err(|e| RpcError::internal(format!("thread: {e}")))?
    };
    d.wake.inner().asking = Some(Asking { remove: p.remove, by: ctx.actor(), done });
    if fake.is_some() {
        // Tests: answered at once.
        while d.wake.inner().asking.as_ref().is_some_and(|a| !a.done.is_finished()) {
            std::thread::yield_now();
        }
        answered(d);
    }
    crate::keep_awake::status(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_open_skips_to_the_hours() {
        // Every hour on the hour; open 7:00-18:00 (minutes since a midnight at 0).
        let h = 3600;
        let next = |t: i64| Some((t / h + 1) * h);
        let open = |t: i64| (7 * h..18 * h).contains(&t.rem_euclid(24 * h));
        let reopen = |t: i64| {
            let day = t.div_euclid(24 * h) * 24 * h;
            Some(if t.rem_euclid(24 * h) < 7 * h { day + 7 * h } else { day + 31 * h })
        };
        assert_eq!(first_open(next, open, reopen, 19 * h, 100 * h), Some(31 * h), "the 7:00 run tomorrow, not 20:00");
        assert_eq!(first_open(next, open, reopen, 8 * h + 5, 100 * h), Some(9 * h));
        assert_eq!(first_open(next, open, reopen, 19 * h, 30 * h), None, "past the horizon");
        assert_eq!(first_open(|_| None, open, reopen, 0, 100 * h), None);
    }

    #[test]
    fn reads_back_its_own_wakes_only() {
        let t = time::local_unix(2026, 10, 9, 6, 58);
        assert_eq!(pmset_date(t), "10/09/26 06:58:00");
        let out = "Scheduled power events:\n \
                   [0]  wake at 10/09/2026 00:09:27 by 'com.apple.alarm.user-invisible-com.apple.calaccessd'\n \
                   [1]  wake at 10/09/2026 06:58:00 by 'com.mrgnhnt.midna'\n \
                   [2]  wake at 10/09/2026 06:58:00 by 'com.mrgnhnt.midna.dev'\n";
        assert_eq!(parse_sched(out, "com.mrgnhnt.midna"), vec![t]);
        assert_eq!(parse_sched(out, "com.mrgnhnt.midna.dev"), vec![t]);
        assert!(parse_sched(out, "midna").is_empty());
    }

    #[test]
    fn the_grant_names_only_the_helper() {
        assert_eq!(sudoers_path("com.mrgnhnt.midna.dev"), "/etc/sudoers.d/com-mrgnhnt-midna-dev-wake");
        let s = sudoers("morgan", "com.mrgnhnt.midna");
        assert!(s.ends_with("morgan ALL=(root) NOPASSWD: /Library/PrivilegedHelperTools/com.mrgnhnt.midna.wake *\n"), "{s}");
        let script = grant_script("morgan", "com.mrgnhnt.midna", false);
        assert!(script.contains("visudo -cqf") && script.contains("-m 440"), "{script}");
        assert_eq!(grant_script("morgan", "com.mrgnhnt.midna", true), "/bin/rm -f '/etc/sudoers.d/com-mrgnhnt-midna-wake' '/Library/PrivilegedHelperTools/com.mrgnhnt.midna.wake'");
    }

    /// The helper refuses anything but `wake|cancel 'MM/DD/YY HH:MM:SS'`, before pmset runs.
    #[test]
    fn the_helper_checks_its_arguments() {
        let dir = std::env::temp_dir().join(format!("midna-wake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("helper");
        // pmset can't be swapped out (absolute path), so only refusals are checked here.
        std::fs::write(&path, helper_script("com.mrgnhnt.midna.test")).unwrap();
        let code = |args: &[&str]| Command::new("/bin/sh").arg(&path).args(args).output().unwrap().status.code();
        assert_eq!(code(&[]), Some(64));
        assert_eq!(code(&["wake"]), Some(64));
        assert_eq!(code(&["wake", "10/09/26 06:58:00", "extra"]), Some(64));
        assert_eq!(code(&["wake", "10/09/26 06:58"]), Some(64));
        assert_eq!(code(&["wake", "10/09/26 06:58:00; reboot"]), Some(64));
        assert_eq!(code(&["sleepnow", "10/09/26 06:58:00"]), Some(64));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
