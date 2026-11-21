//! Reaps the daemon's own child processes that nothing waits for, so they don't pile up as
//! zombies.
//!
//! Almost everything midnad starts has an owner that waits for it: a `Child` polled with
//! `try_wait`, or a terminal's `waitpid`. A few don't: a command still running when an upgrade
//! or a restart execs the new binary (the new image never had a `Child` for it; the live daemon
//! kept the git and gh calls in flight at each release as zombies), and a spawn that drops its
//! `Child`. Every owner polls at least once a second, so a child that has stayed a zombie from
//! one sweep to the next has no owner, and it's reaped then. A zombie its owner is about to reap
//! is left alone, so no exit status is taken from code that is waiting for it.
//!
//! Only the real daemon process runs it (`main.rs`), never a daemon started inside a test
//! binary, whose tests hold children of their own.
use std::time::Duration;

/// How often it looks. A zombie is reaped by the second sweep that sees it.
pub const SWEEP: Duration = Duration::from_secs(30);

/// A child of ours that has exited and not been waited for.
#[derive(Clone, Debug, PartialEq)]
pub struct Zombie {
    pub pid: i32,
    /// When it started (seconds, microseconds), to tell it from a later process with its pid.
    pub start: (i64, i32),
    pub name: String,
}

/// Sweep every [`SWEEP`] (`MIDNA_REAP_SECS` overrides it, for tests).
pub fn start() {
    let every = std::env::var("MIDNA_REAP_SECS").ok().and_then(|v| v.parse::<u64>().ok()).filter(|&s| s > 0).map(Duration::from_secs).unwrap_or(SWEEP);
    let _ = std::thread::Builder::new().name("reap-children".into()).spawn(move || {
        let mut seen = vec![];
        loop {
            std::thread::sleep(every);
            let (old, new) = due(&seen, zombie_children());
            seen = new;
            let reaped: Vec<String> = old.iter().filter(|z| reap(z.pid)).map(|z| format!("{} (pid {})", z.name, z.pid)).collect();
            if !reaped.is_empty() {
                eprintln!("midnad[{}]: reaped {} child process(es) nothing waited for: {}", std::process::id(), reaped.len(), reaped.join(", "));
            }
        }
    });
}

/// Split this sweep's zombies into the ones the last sweep saw too (no owner: reap them) and
/// the new ones (an owner may still wait for them: look again next sweep).
pub fn due(before: &[Zombie], now: Vec<Zombie>) -> (Vec<Zombie>, Vec<Zombie>) {
    now.into_iter().partition(|z| before.contains(z))
}

fn reap(pid: i32) -> bool {
    let mut st = 0;
    unsafe { libc::waitpid(pid, &mut st, libc::WNOHANG) == pid }
}

/// This process's children that are zombies.
pub fn zombie_children() -> Vec<Zombie> {
    let me = std::process::id() as i32;
    children(me).into_iter().filter_map(|pid| zombie(pid, me)).collect()
}

fn children(ppid: i32) -> Vec<i32> {
    let n = unsafe { libc::proc_listchildpids(ppid, std::ptr::null_mut(), 0) };
    if n <= 0 {
        return vec![];
    }
    let mut buf = vec![0i32; n as usize + 64];
    let got = unsafe { libc::proc_listchildpids(ppid, buf.as_mut_ptr() as *mut _, (buf.len() * 4) as i32) };
    buf.truncate((got.max(0) as usize).min(buf.len()));
    buf.retain(|&p| p > 0);
    buf
}

// `struct kinfo_proc` from <sys/sysctl.h> (the same on arm64 and x86_64). libproc can't see a
// zombie (proc_pidinfo fails), but this sysctl, which ps uses, still can. The libc crate doesn't
// declare the struct, so it's read at its offsets, and `p_pid` is checked to match first.
const KINFO_SIZE: usize = 648;
const KP_STARTTIME: usize = 0; // kp_proc.p_starttime: struct timeval
const KP_STAT: usize = 36; // kp_proc.p_stat
const KP_PID: usize = 40; // kp_proc.p_pid
const KP_COMM: usize = 243; // kp_proc.p_comm: char[MAXCOMLEN + 1]
const KP_PPID: usize = 560; // kp_eproc.e_ppid

/// `pid` as a [`Zombie`] if it is one and `parent` is its parent.
fn zombie(pid: i32, parent: i32) -> Option<Zombie> {
    let mut buf = [0u8; KINFO_SIZE];
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut size = KINFO_SIZE;
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 4, buf.as_mut_ptr() as *mut _, &mut size, std::ptr::null_mut(), 0) };
    let i32_at = |o: usize| i32::from_ne_bytes(buf[o..o + 4].try_into().unwrap());
    if r != 0 || size != KINFO_SIZE || i32_at(KP_PID) != pid || i32_at(KP_PPID) != parent || buf[KP_STAT] as u32 != libc::SZOMB {
        return None;
    }
    let secs = i64::from_ne_bytes(buf[KP_STARTTIME..KP_STARTTIME + 8].try_into().unwrap());
    let comm = &buf[KP_COMM..KP_COMM + 17];
    let name = String::from_utf8_lossy(&comm[..comm.iter().position(|&b| b == 0).unwrap_or(comm.len())]).into_owned();
    Some(Zombie { pid, start: (secs, i32_at(KP_STARTTIME + 8)), name })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn z(pid: i32, start: i64) -> Zombie {
        Zombie { pid, start: (start, 0), name: "git".into() }
    }

    #[test]
    fn a_zombie_is_due_once_two_sweeps_have_seen_it() {
        let (old, new) = due(&[], vec![z(10, 1), z(11, 1)]);
        assert_eq!((old, new.clone()), (vec![], vec![z(10, 1), z(11, 1)]));
        // 10 is still there; 11 was reaped by its owner; 12 is new.
        let (old, new) = due(&new, vec![z(10, 1), z(12, 1)]);
        assert_eq!((old, new), (vec![z(10, 1)], vec![z(12, 1)]));
        // A later process with 10's pid starts over.
        let (old, new) = due(&[z(10, 1)], vec![z(10, 2)]);
        assert_eq!((old, new), (vec![], vec![z(10, 2)]));
    }

    #[test]
    fn a_child_nobody_waits_for_shows_up_as_a_zombie_until_reaped() {
        let pid = std::process::Command::new("/usr/bin/true").spawn().unwrap().id() as i32;
        let t0 = Instant::now();
        let found = loop {
            if let Some(found) = zombie_children().into_iter().find(|z| z.pid == pid) {
                break found;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "pid {pid} never showed up as a zombie");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(found.name, "true");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((now - found.start.0).abs() < 60, "start {:?}, now {now}", found.start);
        assert!(reap(pid));
        assert!(!zombie_children().iter().any(|z| z.pid == pid));
    }

    #[test]
    fn a_running_child_is_not_a_zombie() {
        let mut c = std::process::Command::new("/bin/sleep").arg("5").spawn().unwrap();
        let pid = c.id() as i32;
        assert!(children(std::process::id() as i32).contains(&pid));
        assert!(!zombie_children().iter().any(|z| z.pid == pid));
        let _ = c.kill();
        let _ = c.wait();
    }
}
