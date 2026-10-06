//! midnad: the midna daemon.
//!
//!   midnad [--foreground|-f] [--home DIR] [--socket PATH]
//!   midnad --upgrade-to PATH      ask the running daemon to upgrade in place (daemon.upgrade)
//!   midnad --install-self         copy this binary to $MIDNA_HOME/bin/<version>-<hash>, point bin/current at it
//!   midnad --launchd              how launchd starts us (SMAppService plist in Midna.app): foreground,
//!                                 output to $MIDNA_HOME/midnad.log, and hand off to bin/current/midnad
//!   midnad --selftest             upgrade preflight: print {"ok":true,"handoff_versions":[..]}
//!   midnad --resume FILE [--fallback]   (internal) the new image after an upgrade execv
//!   midnad --upgrade-watchdog FILE      (internal) fallback if the new image doesn't come up
//!
//! Without --foreground it re-launches itself detached (new session, output to
//! $MIDNA_HOME/midnad.log) and exits once the socket answers.
//!
//! Signals: SIGTERM = graceful restart (same handoff as an upgrade, re-exec'ing this binary;
//! terminals keep running; if the handoff fails it stops like SIGINT). SIGINT = stop: every
//! terminal's process groups get SIGHUP, then midnad exits. `MIDNA_SIGTERM=stop` makes SIGTERM
//! stop too (scripts that expect `kill` to end the daemon).
use std::time::{Duration, Instant};

fn usage() -> ! {
    eprintln!(
        "usage: midnad [--foreground|-f] [--home DIR] [--socket PATH]\n       midnad --upgrade-to PATH | --install-self | --selftest | --version"
    );
    std::process::exit(2);
}

fn main() {
    midnad::install::running_binary(); // resolve bin/current before anything can re-point it
    let mut foreground = false;
    let mut cfg = midnad::Config::from_env();
    cfg.owns_process = true;
    let mut args = std::env::args().skip(1);
    let mut passthrough = vec![];
    let (mut resume, mut fallback, mut upgrade_to, mut install) = (None::<String>, false, None::<String>, false);
    let mut launchd = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-f" | "--foreground" => foreground = true,
            "--launchd" => {
                launchd = true;
                foreground = true;
            }
            "--selftest" => match midnad::upgrade::selftest() {
                Ok(v) => {
                    println!("{v}");
                    return;
                }
                Err(e) => {
                    eprintln!("midnad selftest failed: {e}");
                    std::process::exit(1);
                }
            },
            "--upgrade-watchdog" => {
                let f = args.next().unwrap_or_else(|| usage());
                midnad::upgrade::watchdog_main(std::path::Path::new(&f));
            }
            "--resume" => {
                resume = Some(args.next().unwrap_or_else(|| usage()));
                foreground = true;
            }
            "--fallback" => fallback = true,
            "--upgrade-to" => upgrade_to = Some(args.next().unwrap_or_else(|| usage())),
            "--install-self" => install = true,
            "--home" => {
                let h = args.next().unwrap_or_else(|| usage());
                passthrough.extend(["--home".to_string(), h.clone()]);
                let sock_default = cfg.socket == cfg.home.join("midnad.sock");
                cfg.home = h.into();
                if sock_default {
                    cfg.socket = cfg.home.join("midnad.sock");
                }
            }
            "--socket" => {
                let s = args.next().unwrap_or_else(|| usage());
                passthrough.extend(["--socket".to_string(), s.clone()]);
                cfg.socket = s.into();
            }
            "-h" | "--help" => usage(),
            "--version" | "-V" => {
                println!("midnad {}", midna_proto::VERSION);
                return;
            }
            _ => usage(),
        }
    }
    // Midna Dev's daemon never serves, installs into or upgrades the installed Midna's home.
    if let Err(e) = midna_proto::paths::guard_write(&cfg.home).and_then(|()| midna_proto::paths::guard_write(&cfg.socket)) {
        eprintln!("midnad: {e}");
        std::process::exit(2);
    }
    if install {
        match midnad::install::install_self(&cfg.home) {
            Ok(dir) => println!("installed {} (bin/current -> {})", dir.join("midnad").display(), dir.display()),
            Err(e) => {
                eprintln!("midnad --install-self: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Some(p) = upgrade_to {
        upgrade_client(&cfg, &p);
        return;
    }
    let resume = match resume {
        Some(f) => match midnad::upgrade::load(std::path::Path::new(&f), fallback) {
            Ok(r) => Some(r),
            Err(e) => {
                // Exiting lets the upgrade watchdog fall back to the old binary.
                eprintln!("midnad: can't resume {f}: {e}");
                std::process::exit(1);
            }
        },
        None => None,
    };
    if launchd && resume.is_none() {
        launchd_setup(&cfg, &passthrough);
    }
    if !foreground {
        detach(&cfg, &passthrough);
        return;
    }
    match midnad::start_with(cfg.clone(), resume) {
        Ok(h) => {
            let c = &h.daemon.cfg;
            eprintln!("midnad {} listening on {} (home {}, pid {})", midna_proto::VERSION, c.socket.display(), c.home.display(), std::process::id());
            install_signals(h.daemon.clone());
            h.wait();
        }
        Err(e) => {
            eprintln!("midnad: {e}");
            std::process::exit(1);
        }
    }
}

/// `midnad --upgrade-to PATH`: the same request as `midna daemon upgrade PATH`. This process
/// isn't the GUI, so the daemon turns it into a needs-you approval for the human.
fn upgrade_client(cfg: &midnad::Config, path: &str) {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|e| {
        eprintln!("midnad: {path}: {e}");
        std::process::exit(2);
    });
    let mut c = match midna_proto::Client::connect(&cfg.socket) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("midnad is not reachable at {}: {e}", cfg.socket.display());
            std::process::exit(3);
        }
    };
    match c.call_value("daemon.upgrade", serde_json::json!({ "binary_path": abs })) {
        Ok(v) => println!("upgrade scheduled: {v}"),
        Err(e) => {
            eprintln!("midnad: {e}");
            std::process::exit(1);
        }
    }
}

static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// Signals go through a self-pipe to a normal thread (a handoff can't run in a handler).
/// SIGTERM: graceful restart, falling back to a stop. SIGINT: stop.
fn install_signals(d: std::sync::Arc<midnad::daemon::Daemon>) {
    let mut p = [0; 2];
    if unsafe { libc::pipe(p.as_mut_ptr()) } != 0 {
        return;
    }
    for fd in p {
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    SIGNAL_PIPE.store(p[1], std::sync::atomic::Ordering::SeqCst);
    extern "C" fn on_signal(sig: libc::c_int) {
        let fd = SIGNAL_PIPE.load(std::sync::atomic::Ordering::SeqCst);
        let b = sig as u8;
        unsafe { libc::write(fd, &b as *const u8 as *const _, 1) };
    }
    unsafe {
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
    }
    let rfd = p[0];
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            loop {
                let mut b = 0u8;
                let n = unsafe { libc::read(rfd, &mut b as *mut u8 as *mut _, 1) };
                if n <= 0 {
                    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return;
                }
                match b as libc::c_int {
                    libc::SIGTERM if std::env::var("MIDNA_SIGTERM").as_deref() == Ok("stop") => {
                        eprintln!("midnad: SIGTERM (MIDNA_SIGTERM=stop): stopping (hanging up every terminal)");
                        midnad::server::stop_daemon(&d);
                    }
                    libc::SIGTERM => {
                        eprintln!("midnad: SIGTERM: graceful restart (terminals keep running)");
                        let e = midnad::upgrade::restart_now(&d, "sigterm");
                        eprintln!("midnad: graceful restart failed ({e}); stopping instead");
                        midnad::server::stop_daemon(&d);
                    }
                    _ => {
                        eprintln!("midnad: SIGINT: stopping (hanging up every terminal)");
                        midnad::server::stop_daemon(&d);
                    }
                }
            }
        })
        .expect("signal thread");
}

/// launchd runs the binary inside Midna.app (SMAppService needs a `BundleProgram`). That copy
/// is replaced by every app update, so hand off to the stable install at `bin/current/midnad`
/// (installing ourselves first if there is none) — a crash restart or a login then runs the
/// version the last upgrade chose, and `daemon.restart` finds us under `bin/`. launchd gives us
/// no useful stdout/stderr, so both go to `$MIDNA_HOME/midnad.log`.
fn launchd_setup(cfg: &midnad::Config, passthrough: &[String]) {
    use std::os::unix::ffi::OsStrExt;
    let _ = std::fs::create_dir_all(&cfg.home);
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(cfg.home.join("midnad.log")) {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::dup2(f.as_raw_fd(), 1);
            libc::dup2(f.as_raw_fd(), 2);
        }
    }
    if std::env::var_os("MIDNA_TRAMPOLINED").is_some() {
        unsafe { std::env::remove_var("MIDNA_TRAMPOLINED") }; // don't leak into terminals
        return;
    }
    let current = midnad::install::current_binary(&cfg.home);
    if !current.is_file()
        && let Err(e) = midnad::install::install_self(&cfg.home)
    {
        eprintln!("midnad --launchd: install into {} failed ({e}); serving from {:?}", cfg.home.display(), std::env::current_exe());
        return;
    }
    let me = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let target = std::fs::canonicalize(&current).ok();
    if target.is_none() || target == me {
        return;
    }
    eprintln!("midnad --launchd: started as {:?}; handing off to {}", me, current.display());
    unsafe { std::env::set_var("MIDNA_TRAMPOLINED", "1") };
    let path = std::ffi::CString::new(current.as_os_str().as_bytes()).unwrap();
    let mut args = vec![path.clone(), c"--launchd".into()];
    args.extend(passthrough.iter().filter_map(|a| std::ffi::CString::new(a.as_str()).ok()));
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    unsafe { libc::execv(path.as_ptr(), argv.as_ptr()) };
    eprintln!("midnad --launchd: exec {} failed: {}; serving from this binary", current.display(), std::io::Error::last_os_error());
}

fn detach(cfg: &midnad::Config, passthrough: &[String]) {
    use std::os::unix::process::CommandExt;
    if std::os::unix::net::UnixStream::connect(&cfg.socket).is_ok() {
        eprintln!("midnad already running on {}", cfg.socket.display());
        return;
    }
    let _ = std::fs::create_dir_all(&cfg.home);
    let log = cfg.home.join("midnad.log");
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&log).expect("open midnad.log");
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("current exe"));
    cmd.arg("--foreground").args(passthrough).stdin(std::process::Stdio::null()).stdout(out.try_clone().unwrap()).stderr(out);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    if let Err(e) = cmd.spawn() {
        eprintln!("midnad: could not start: {e}");
        std::process::exit(1);
    }
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if std::os::unix::net::UnixStream::connect(&cfg.socket).is_ok() {
            println!("midnad started ({})", cfg.socket.display());
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("midnad: did not come up within 5s; see {}", log.display());
    std::process::exit(1);
}
