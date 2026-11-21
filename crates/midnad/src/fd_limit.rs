//! The daemon's open-file limit. launchd starts midnad with a soft limit of 256, and every
//! terminal holds a PTY master and every client connection a few handles, so ~50 agents with
//! their MCP servers and CLI calls run it out and new terminals fail with EMFILE.
use std::io;

/// What `raise` lifts the soft limit to (never past the hard limit). macOS's OPEN_MAX: setrlimit
/// takes it everywhere, and terminals inherit it.
pub const WANT: u64 = 10240;

/// Lift the soft RLIMIT_NOFILE to `WANT` (or the hard limit, if that's lower). Never lowers it.
/// Returns (before, after).
pub fn raise() -> io::Result<(u64, u64)> {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let before = r.rlim_cur as u64;
    let target = WANT.min(r.rlim_max as u64);
    if before >= target {
        return Ok((before, before));
    }
    r.rlim_cur = target as libc::rlim_t;
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((before, target))
}

/// The soft limit now.
pub fn soft_limit() -> Option<u64> {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    (unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) } == 0).then_some(r.rlim_cur as u64)
}

/// How many files this process has open.
pub fn open_count() -> Option<usize> {
    let dir = if cfg!(target_os = "linux") { "/proc/self/fd" } else { "/dev/fd" };
    // The directory handle itself shows up in the listing.
    std::fs::read_dir(dir).ok().map(|d| d.count().saturating_sub(1))
}

/// Out of file handles: this process (EMFILE) or the whole system (ENFILE).
pub fn exhausted(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(libc::EMFILE | libc::ENFILE))
}

/// `e` again, saying whose handles ran out and how many are open when it's EMFILE/ENFILE.
pub fn explain(e: io::Error) -> io::Error {
    if !exhausted(&e) {
        return e;
    }
    let msg = match e.raw_os_error() {
        Some(libc::ENFILE) => "the Mac is out of file handles (ENFILE): too many files open system-wide".to_string(),
        _ => {
            let open = open_count().map_or("?".into(), |n| n.to_string());
            let limit = soft_limit().map_or("?".into(), |n| n.to_string());
            format!("midnad is out of file handles ({open} open, limit {limit}; EMFILE)")
        }
    };
    io::Error::new(e.kind(), msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exhausted_is_emfile_or_enfile_only() {
        assert!(exhausted(&io::Error::from_raw_os_error(libc::EMFILE)));
        assert!(exhausted(&io::Error::from_raw_os_error(libc::ENFILE)));
        assert!(!exhausted(&io::Error::from_raw_os_error(libc::ENOENT)));
        assert!(!exhausted(&io::Error::other("x")));
    }

    #[test]
    fn explain_names_midnad_and_the_counts() {
        let e = explain(io::Error::from_raw_os_error(libc::EMFILE));
        let s = e.to_string();
        assert!(s.contains("midnad is out of file handles"), "{s}");
        assert!(s.contains("limit "), "{s}");
        assert!(explain(io::Error::from_raw_os_error(libc::ENFILE)).to_string().contains("system-wide"));
        assert_eq!(explain(io::Error::from_raw_os_error(libc::ENOENT)).raw_os_error(), Some(libc::ENOENT));
    }
}
