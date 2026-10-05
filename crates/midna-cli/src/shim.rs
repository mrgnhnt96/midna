//! `midna shim claude|codex ARGS…`: what `claude` / `codex` run in a midna shell terminal
//! (midnad's `$MIDNA_HOME/shims/*`; setting `agents.adopt_typed`). It asks midnad
//! (`session.adopt`) whether to run the agent under midna, runs the reply's command as its
//! child, and when it exits asks `session.adopt_end`, whose reply is the next command when
//! midna restarted it.
//! The human shouldn't notice it: it prints nothing, keeps Claude's exit code, and whenever
//! midnad says no or doesn't answer, it becomes the real `claude` as typed.
use midna_proto::{Client, SessionAdoptEndParams, SessionAdoptParams, SessionAdoptResult};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

/// Claude's pid, for the signal handlers.
static CHILD: AtomicI32 = AtomicI32::new(0);

pub fn run(raw: &[String]) -> ! {
    let Some((agent, args)) = raw.split_first() else {
        eprintln!("usage: midna shim claude [args…]");
        std::process::exit(2)
    };
    let Some(real) = find_real(agent) else {
        eprintln!("{agent}: command not found");
        std::process::exit(127)
    };
    let as_typed = || -> ! {
        let e = Command::new(&real).arg0(agent).args(args).exec();
        eprintln!("{agent}: {e}");
        std::process::exit(126)
    };
    let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    let kind = match agent.as_str() {
        "claude" => midna_proto::AgentKind::Claude,
        "codex" => midna_proto::AgentKind::Codex,
        _ => as_typed(),
    };
    if !tty || std::env::var("MIDNA_SESSION").map_or(true, |s| s.is_empty()) {
        as_typed();
    }
    let pid = std::process::id() as i32;
    let adopt = SessionAdoptParams {
        agent: kind,
        bin: real.to_string_lossy().into_owned(),
        args: args.to_vec(),
        pid,
        session: None,
    };
    let Some(mut next) = ask("session.adopt", &adopt).and_then(|r| Some((r.run?, r.env, r.cwd))) else { as_typed() };
    forward_signals();
    loop {
        let (argv, env, cwd) = next;
        let code = run_child(agent, &argv, &env, cwd.as_deref());
        let end = SessionAdoptEndParams { pid, code: Some(code), session: None };
        match ask("session.adopt_end", &end).and_then(|r| Some((r.run?, r.env, r.cwd))) {
            Some(n) => {
                // Claude left its full-screen view on exit, which shows the shell's screen until
                // the next one draws: go back to a blank full-screen view at once.
                let _ = std::io::Write::write_all(&mut std::io::stdout(), b"\x1b[?1049h\x1b[H\x1b[2J");
                next = n;
            }
            None => std::process::exit(code),
        }
    }
}

/// One call to midnad, or None when it says no or doesn't answer in time.
fn ask(method: &str, params: &impl serde::Serialize) -> Option<SessionAdoptResult> {
    let mut c = Client::connect_default().ok()?;
    c.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    let v = c.call_value(method, serde_json::to_value(params).ok()?).ok()?;
    serde_json::from_value(v).ok()
}

/// Run Claude in the foreground (same process group and tty) and return its exit code.
fn run_child(agent: &str, argv: &[String], env: &std::collections::BTreeMap<String, String>, cwd: Option<&str>) -> i32 {
    let Some((bin, args)) = argv.split_first() else { return 127 };
    let mut cmd = Command::new(bin);
    cmd.arg0(agent).args(args).envs(env);
    if let Some(dir) = cwd.filter(|d| Path::new(d).is_dir()) {
        cmd.current_dir(dir);
    }
    // The shim ignores ^C and ^\ (they're for Claude); the child gets the defaults back.
    unsafe {
        cmd.pre_exec(|| {
            for s in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
                libc::signal(s, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{agent}: {e}");
            return 126;
        }
    };
    let cpid = child.id() as i32;
    CHILD.store(cpid, Ordering::Relaxed);
    let mut status = 0;
    loop {
        let r = unsafe { libc::waitpid(cpid, &mut status, 0) };
        if r == cpid {
            break;
        }
        if r < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return 1;
        }
    }
    CHILD.store(0, Ordering::Relaxed);
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else if libc::WIFSIGNALED(status) {
        128 + libc::WTERMSIG(status)
    } else {
        1
    }
}

extern "C" fn pass_on(sig: libc::c_int) {
    let c = CHILD.load(Ordering::Relaxed);
    if c > 0 {
        unsafe { libc::kill(c, sig) };
    }
}

/// ^C and ^\ reach Claude from the tty already. A hangup or terminate sent to the shim alone
/// goes on to Claude, and the shim exits with it.
fn forward_signals() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        for s in [libc::SIGHUP, libc::SIGTERM] {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = pass_on as *const () as usize;
            sa.sa_flags = libc::SA_RESTART;
            libc::sigaction(s, &sa, std::ptr::null_mut());
        }
    }
}

/// The `agent` the shell would have run without midna: the binary an alias named
/// (`MIDNA_SHIM_REAL`, set by midna's shell integration), else the first on PATH that isn't
/// the shim.
fn find_real(agent: &str) -> Option<PathBuf> {
    if let Some(r) = std::env::var_os("MIDNA_SHIM_REAL").map(PathBuf::from) {
        // Not for the agent's own children.
        unsafe { std::env::remove_var("MIDNA_SHIM_REAL") };
        if r.is_file() && !is_shim(&r) {
            return Some(r);
        }
    }
    let shims = std::env::var_os("MIDNA_SHIMS")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("MIDNA_HOME").map(|h| Path::new(&h).join("shims")))
        .and_then(|p| std::fs::canonicalize(p).ok());
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| !d.as_os_str().is_empty()).find_map(|dir| {
        if shims.is_some() && std::fs::canonicalize(&dir).ok() == shims {
            return None;
        }
        let c = dir.join(agent);
        let m = std::fs::metadata(&c).ok()?;
        let exec = std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0;
        (m.is_file() && exec && !is_shim(&c)).then_some(c)
    })
}

/// midna's own shim, wherever it is (another MIDNA_HOME's on PATH too).
fn is_shim(p: &Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.len() < 4096 && b.windows(14).any(|w| w == b"exec \"$cli\" sh"))
}
