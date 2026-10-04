//! Spike daemon. Launched by launchd (SMAppService BundleProgram or legacy
//! plist). Serves a Unix socket in ~/Library/Application Support/com.mrgnhnt.midna/.
//!
//! Layout under that dir:
//!   midnad.sock          listener (fd survives execv via MIDNA_LISTEN_FD)
//!   bin/current          symlink -> versions/midnad-<ver> (the stable exec target)
//!   versions/midnad-<v>  copies of each daemon build (survive bundle replacement)
//!   upgrade.pending      "<prev>\n<new>" while an upgrade is unconfirmed
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use std::{env, ffi::CString, fs, thread};

const VERSION: &str = match option_env!("MIDNA_VERSION") { Some(v) => v, None => "0.0.0" };
const BROKEN: bool = option_env!("MIDNA_BROKEN").is_some();

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" { fn AXIsProcessTrusted() -> bool; }

fn home() -> PathBuf { PathBuf::from(env::var("HOME").unwrap()) }
fn support() -> PathBuf { home().join("Library/Application Support/com.mrgnhnt.midna") }
fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() }

fn log(msg: &str) {
    let line = format!("{} pid={} v{} {}\n", now(), std::process::id(), VERSION, msg);
    eprint!("{line}");
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(support().join("daemon.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn execv(path: &Path, args: &[&str]) -> std::io::Error {
    let p = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let mut owned: Vec<CString> = vec![p.clone()];
    owned.extend(args.iter().map(|a| CString::new(*a).unwrap()));
    let mut argv: Vec<*const libc::c_char> = owned.iter().map(|c| c.as_ptr()).collect();
    argv.push(std::ptr::null());
    unsafe { libc::execv(p.as_ptr(), argv.as_ptr()) };
    std::io::Error::last_os_error()
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") => println!("{VERSION}"),
        // Preflight run by the old daemon before it swaps the symlink.
        Some("--selftest") => {
            if !BROKEN { println!("ok {VERSION}") } else { std::process::exit(3) }
        }
        Some("watchdog") => watchdog(args[2].parse().unwrap(), &args[3]),
        _ => serve(),
    }
}

/// Runs from the *previous* binary, detached. Reverts bin/current and kills
/// the daemon if the new image doesn't confirm within the deadline; launchd
/// KeepAlive then restarts it, and the trampoline execs the reverted target.
fn watchdog(pid: i32, prev: &str) {
    unsafe { libc::setsid() };
    let pending = support().join("upgrade.pending");
    let start = Instant::now();
    loop {
        thread::sleep(Duration::from_millis(100));
        if !pending.exists() { log("watchdog: upgrade confirmed, exiting"); return; }
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        let zombie_or_other = fs::read_to_string(&pending).map(|s| s.contains("FAILED")).unwrap_or(false);
        if !alive || zombie_or_other || start.elapsed() > Duration::from_secs(5) {
            break;
        }
    }
    let cur = support().join("bin/current");
    let tmp = support().join("bin/current.tmp");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(prev, &tmp).unwrap();
    fs::rename(&tmp, &cur).unwrap();
    let _ = fs::remove_file(&pending);
    log(&format!("watchdog: new image never confirmed after {:?}; reverted bin/current -> {prev}; SIGKILL {pid}", start.elapsed()));
    if unsafe { libc::kill(pid, 0) } == 0 { unsafe { libc::kill(pid, libc::SIGKILL) }; }
    // NOTE: `launchctl kickstart -k` here was tested and made it worse: it
    // blocks while the job is in launchd's crash throttle (~10s) and recovery
    // took ~20s instead of ~10s. Let KeepAlive restart it on its own.
}

fn serve() {
    let sup = support();
    fs::create_dir_all(sup.join("bin")).unwrap();
    fs::create_dir_all(sup.join("versions")).unwrap();
    let exe = env::current_exe().unwrap();

    // Trampoline: launchd always starts the bundle binary (BundleProgram) or
    // whatever the legacy plist names. If bin/current points elsewhere, exec it
    // so the version chosen by the last confirmed upgrade (or a watchdog revert) runs.
    let cur = sup.join("bin/current");
    if env::var("MIDNA_TRAMPOLINED").is_err() && env::var("MIDNA_LISTEN_FD").is_err() {
        match fs::canonicalize(&cur) {
            Ok(target) if target != fs::canonicalize(&exe).unwrap() => {
                log(&format!("trampoline: launched as {} -> exec {}", exe.display(), cur.display()));
                unsafe { env::set_var("MIDNA_TRAMPOLINED", "1") };
                let e = execv(&cur, &["serve"]);
                log(&format!("trampoline exec failed: {e}; serving from bundle"));
            }
            Ok(_) => {}
            Err(_) => {
                // First boot: install ourselves as the current version.
                let v = sup.join(format!("versions/midnad-{VERSION}"));
                if !v.exists() { fs::copy(&exe, &v).unwrap(); }
                let _ = std::os::unix::fs::symlink(&v, &cur);
                log(&format!("first boot: bin/current -> {}", v.display()));
            }
        }
    }

    // Listener: inherited across execv, or fresh.
    let sock = sup.join("midnad.sock");
    let (listener, boot) = match env::var("MIDNA_LISTEN_FD") {
        Ok(fd) => {
            let boot = env::var("MIDNA_BOOT").unwrap();
            log(&format!("re-exec'd image up, inherited listener fd {fd}, exe={}", exe.display()));
            (unsafe { UnixListener::from_raw_fd(fd.parse().unwrap()) }, boot)
        }
        Err(_) => {
            let _ = fs::remove_file(&sock);
            log(&format!("cold start exe={}", exe.display()));
            (UnixListener::bind(&sock).unwrap(), now().to_string())
        }
    };
    // A long-lived child stands in for PTY sessions: it must survive the re-exec.
    let child = match env::var("MIDNA_CHILD") {
        Ok(c) => c,
        Err(_) => Command::new("/bin/sleep").arg("86400").spawn().unwrap().id().to_string(),
    };
    if BROKEN {
        log("BROKEN build: cannot parse state, exiting 3");
        std::process::exit(3);
    }
    // Confirm a pending upgrade (the watchdog is waiting for this).
    let pending = sup.join("upgrade.pending");
    if pending.exists() { let _ = fs::remove_file(&pending); log("confirmed upgrade"); }

    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let mut r = BufReader::new(conn.try_clone().unwrap());
        let mut line = String::new();
        if r.read_line(&mut line).is_err() { continue; }
        let mut w = conn;
        let mut parts = line.trim().splitn(2, ' ');
        match parts.next().unwrap_or("") {
            "PING" => {
                let _ = writeln!(w, "PONG v{VERSION} pid={} ax={} boot={boot} child={child} exe={}", std::process::id(), unsafe { AXIsProcessTrusted() }, exe.display());
            }
            "UPGRADE" => {
                let new = PathBuf::from(parts.next().unwrap_or(""));
                match upgrade(&new, &cur) {
                    Ok(prev) => {
                        let _ = writeln!(w, "OK execing");
                        drop(w);
                        let fd = listener.as_raw_fd();
                        unsafe {
                            let fl = libc::fcntl(fd, libc::F_GETFD);
                            libc::fcntl(fd, libc::F_SETFD, fl & !libc::FD_CLOEXEC);
                            env::set_var("MIDNA_LISTEN_FD", fd.to_string());
                            env::set_var("MIDNA_BOOT", &boot);
                            env::set_var("MIDNA_CHILD", &child);
                        }
                        // Watchdog = previous binary, detached.
                        Command::new(&prev).args(["watchdog", &std::process::id().to_string(), prev.to_str().unwrap()])
                            .env_remove("MIDNA_LISTEN_FD").spawn().unwrap();
                        log(&format!("execv {} (prev {})", cur.display(), prev.display()));
                        let e = execv(&cur, &["serve"]);
                        log(&format!("execv failed: {e}"));
                        let _ = listener.into_raw_fd();
                        std::process::exit(1);
                    }
                    Err(e) => { let _ = writeln!(w, "ERR {e}"); log(&format!("upgrade refused: {e}")); }
                }
            }
            _ => { let _ = writeln!(w, "ERR unknown"); }
        }
    }
}

/// Copy the new binary out of the bundle (it may be replaced again later),
/// preflight it, then atomically swap bin/current. Returns the previous target.
fn upgrade(new: &Path, cur: &Path) -> Result<PathBuf, String> {
    let sup = support();
    let out = Command::new(new).arg("--version").output().map_err(|e| e.to_string())?;
    let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || ver.is_empty() { return Err("--version failed".into()); }
    let dest = sup.join(format!("versions/midnad-{ver}"));
    let tmp = sup.join(format!("versions/.midnad-{ver}.tmp"));
    fs::copy(new, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    let st = Command::new(&dest).arg("--selftest").output().map_err(|e| e.to_string())?;
    // SELFTEST_BYPASS file lets the test skip preflight to exercise the watchdog.
    if !st.status.success() && !sup.join("SELFTEST_BYPASS").exists() {
        return Err(format!("preflight --selftest failed for v{ver} (exit {:?})", st.status.code()));
    }
    let prev = fs::read_link(cur).map_err(|e| e.to_string())?;
    if prev == dest { return Err(format!("already on v{ver}")); }
    fs::write(sup.join("upgrade.pending"), format!("{}\n{}\n", prev.display(), dest.display())).unwrap();
    let t = sup.join("bin/current.tmp");
    let _ = fs::remove_file(&t);
    std::os::unix::fs::symlink(&dest, &t).map_err(|e| e.to_string())?;
    fs::rename(&t, cur).map_err(|e| e.to_string())?;
    log(&format!("upgrade: bin/current {} -> {}", prev.display(), dest.display()));
    Ok(prev)
}
