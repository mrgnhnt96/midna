//! midnad out of file handles: starting a terminal fails clearly and leaks nothing, and the
//! daemon's raised limit lets it start again. One test in its own binary: it lowers this
//! process's RLIMIT_NOFILE, which would break tests running beside it.
use midnad::{fd_limit, pty};

fn set_soft(n: u64) {
    let mut r = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut r) }, 0);
    r.rlim_cur = n as libc::rlim_t;
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &r) }, 0, "{}", std::io::Error::last_os_error());
}

fn start() -> std::io::Result<pty::Spawned> {
    pty::spawn(&["/usr/bin/true".to_string()], "/", &[], 80, 24)
}

#[test]
fn out_of_handles_fails_clearly_without_leaking_then_raise_recovers() {
    set_soft(256);
    let (before, after) = fd_limit::raise().unwrap();
    assert_eq!(before, 256);
    assert!(after > 256 && after <= fd_limit::WANT, "{after}");
    assert_eq!(fd_limit::soft_limit(), Some(after));

    // Room for 1..4 more handles fails at openpty, at each dup of the slave, and at std's
    // exec-error pipe.
    for room in 1..=4 {
        let open = fd_limit::open_count().unwrap();
        set_soft((open + room) as u64);
        let r = start();
        set_soft(after);
        let e = r.err().unwrap_or_else(|| panic!("started with room for only {room} more handles"));
        assert!(e.to_string().contains("midnad is out of file handles"), "room {room}: {e}");
        assert_eq!(fd_limit::open_count().unwrap(), open, "room {room}: a failed start left handles open");
    }

    let mut sp = start().unwrap();
    assert!(sp.child.wait().unwrap().success());
    unsafe { libc::close(sp.master) };
}
