//! Headless `run_command`: a trigger's command run with no terminal (`TriggerAction::RunCommand`
//! with `headless`). It runs through the login shell in the project's folder with the env a
//! terminal gets (less `MIDNA_SESSION`, plus `MIDNA_TRIGGER` / `MIDNA_DELIVERY`), stdin closed,
//! in its own process group. Its exit code and the end of its output go on the delivery that
//! fired it (`Delivery::command_runs`), then `trigger.command_finished` is emitted.
//!
//! Each run has a thread of its own that waits for it, so the trigger thread never blocks.
//! Past its timeout the group gets SIGTERM, then SIGKILL 5 seconds later.
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
pub const MAX_TIMEOUT_SECS: u64 = 86_400;
const TAIL_LINES: usize = 50;
const TAIL_BYTES: usize = 4096;
/// Output kept while it runs (the tail is cut from this).
const KEEP_BYTES: usize = 64 * 1024;
/// After it exits, how long a process it left behind may hold its output open.
const DRAIN: Duration = Duration::from_secs(2);
/// What a run still going when midnad last stopped says.
pub const LOST: &str = "midnad restarted while it ran";

/// Start `command` for trigger `t` (delivery `delivery_id`) in `project_id`'s folder. Returns
/// the run as it starts; when it can't start, already finished with `error`.
pub fn start(d: &Arc<Daemon>, t: &Trigger, delivery_id: &str, project_id: &str, command: &str, timeout_secs: Option<u64>) -> CommandRun {
    let mut run = CommandRun {
        trigger_id: t.id.clone(),
        command: command.to_string(),
        started_at: time::now_rfc3339(),
        finished_at: None,
        exit_code: None,
        signal: None,
        timed_out: false,
        output: String::new(),
        error: None,
    };
    let cwd = match d.core().state.project(project_id) {
        Some(p) => p.path.clone(),
        None => {
            run.error = Some(format!("no project {project_id}"));
            run.finished_at = Some(time::now_rfc3339());
            return run;
        }
    };
    let argv = crate::rpc::session::exec_argv(&[command.to_string()]);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(if std::path::Path::new(&cwd).is_dir() { cwd.as_str() } else { "/" }).stdin(Stdio::null());
    for k in crate::pty::ENV_SCRUB {
        cmd.env_remove(k);
    }
    for (k, v) in crate::rpc::session::session_env(d, "", project_id) {
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
    cmd.env_remove("MIDNA_SESSION").env("MIDNA_TRIGGER", &t.id).env("MIDNA_DELIVERY", delivery_id);
    let (reader, writer) = match std::io::pipe() {
        Ok(p) => p,
        Err(e) => return failed(run, e),
    };
    let writer2 = match writer.try_clone() {
        Ok(w) => w,
        Err(e) => return failed(run, e),
    };
    cmd.stdout(writer).stderr(writer2);
    unsafe {
        cmd.pre_exec(|| {
            // Its own process group, so a timeout stops everything it started.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
            Ok(())
        });
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return failed(run, e),
    };
    // The pipe's write ends are the child's now: drop ours so EOF comes when it's done.
    drop(cmd);
    let out = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (out2, mut reader) = (out.clone(), reader);
    let read_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let read_done2 = read_done.clone();
    let _ = std::thread::Builder::new().name("headless-out".into()).spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let mut o = out2.lock().unwrap_or_else(|e| e.into_inner());
            o.extend_from_slice(&buf[..n]);
            if o.len() > KEEP_BYTES * 2 {
                let cut = o.len() - KEEP_BYTES;
                o.drain(..cut);
            }
        }
        read_done2.store(true, std::sync::atomic::Ordering::Release);
    });
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).clamp(1, MAX_TIMEOUT_SECS));
    let (d, del, started) = (d.clone(), delivery_id.to_string(), run.clone());
    let _ = std::thread::Builder::new().name("headless-run".into()).spawn(move || {
        let mut child = child;
        let pid = child.id() as i32;
        let began = Instant::now();
        let mut killed_at: Option<Instant> = None;
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) => {}
                Err(_) => break None,
            }
            match killed_at {
                None if began.elapsed() >= timeout => {
                    timed_out = true;
                    unsafe { libc::kill(-pid, libc::SIGTERM) };
                    killed_at = Some(Instant::now());
                }
                Some(k) if k.elapsed() >= Duration::from_secs(5) => {
                    unsafe { libc::kill(-pid, libc::SIGKILL) };
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let drain = Instant::now();
        while !read_done.load(std::sync::atomic::Ordering::Acquire) && drain.elapsed() < DRAIN {
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut run = started;
        run.finished_at = Some(time::now_rfc3339());
        run.timed_out = timed_out;
        run.output = tail(&out.lock().unwrap_or_else(|e| e.into_inner()));
        match status {
            Some(s) => {
                run.exit_code = s.code();
                run.signal = s.signal();
            }
            None => run.error = Some("lost track of the process".into()),
        }
        finish(&d, &del, run);
    });
    run
}

fn failed(mut run: CommandRun, e: std::io::Error) -> CommandRun {
    run.error = Some(format!("could not start: {e}"));
    run.finished_at = Some(time::now_rfc3339());
    run
}

/// Put the finished run on its delivery (recorded just after the run started, so wait a
/// little for it) and emit `trigger.command_finished`.
fn finish(d: &Daemon, delivery_id: &str, run: CommandRun) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let project = loop {
        let mut core = d.core();
        let st = &mut core.state;
        let found = st.deliveries.iter_mut().find(|x| x.id == delivery_id).and_then(|del| {
            del.command_runs.iter_mut().find(|r| r.trigger_id == run.trigger_id && r.started_at == run.started_at && r.finished_at.is_none())
        });
        if let Some(r) = found {
            *r = run.clone();
            let project = st.triggers.iter().find(|t| t.id == run.trigger_id).and_then(|t| t.action.project_id().cloned());
            break Some(project);
        }
        drop(core);
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(project) = project else { return };
    d.mark_dirty();
    d.emit(kinds::TRIGGER_COMMAND_FINISHED, Actor::system(), project, None, json!({ "trigger_id": run.trigger_id, "delivery_id": delivery_id, "run": run }));
}

/// The end of `out`: its last TAIL_LINES lines, at most TAIL_BYTES (cut at a character).
pub fn tail(out: &[u8]) -> String {
    let text = String::from_utf8_lossy(out);
    let text = text.trim_end();
    let lines: Vec<&str> = text.lines().collect();
    let mut s = lines[lines.len().saturating_sub(TAIL_LINES)..].join("\n");
    if s.len() > TAIL_BYTES {
        let mut cut = s.len() - TAIL_BYTES;
        while !s.is_char_boundary(cut) {
            cut += 1;
        }
        s = s[cut..].to_string();
    }
    s
}

/// Runs still going when midnad stopped: it lost them (their output pipe went with it).
pub fn settle_lost(st: &mut crate::state::State) {
    for r in st.deliveries.iter_mut().flat_map(|d| d.command_runs.iter_mut()).filter(|r| r.finished_at.is_none()) {
        r.finished_at = Some(time::now_rfc3339());
        r.error = Some(LOST.into());
    }
}

/// One line about a run, for summaries: `exit 0`, `exit 2`, `timed out`, `killed by signal 9`.
pub fn result_text(r: &CommandRun) -> String {
    match (r.finished_at.is_some(), r.timed_out, r.exit_code, r.signal, r.error.as_deref()) {
        (false, ..) => "running".into(),
        (_, true, ..) => "timed out".into(),
        (_, _, _, _, Some(e)) => e.to_string(),
        (_, _, Some(c), ..) => format!("exit {c}"),
        (_, _, None, Some(s), _) => format!("killed by signal {s}"),
        _ => "finished".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines_and_bytes() {
        let many: String = (1..=80).map(|i| format!("line {i}\n")).collect();
        let t = tail(many.as_bytes());
        assert!(t.starts_with("line 31\n") && t.ends_with("line 80"), "{t}");
        let long = "é".repeat(5000);
        let t = tail(long.as_bytes());
        assert!(t.len() <= TAIL_BYTES && t.chars().all(|c| c == 'é'));
        assert_eq!(tail(b"ok\n\n"), "ok");
    }

    #[test]
    fn result_text_says_how_it_ended() {
        let r = CommandRun {
            trigger_id: "t".into(),
            command: "x".into(),
            started_at: "2026-10-07T00:00:00Z".into(),
            finished_at: None,
            exit_code: None,
            signal: None,
            timed_out: false,
            output: String::new(),
            error: None,
        };
        assert_eq!(result_text(&r), "running");
        let done = CommandRun { finished_at: Some("2026-10-07T00:00:01Z".into()), ..r };
        assert_eq!(result_text(&CommandRun { exit_code: Some(2), ..done.clone() }), "exit 2");
        assert_eq!(result_text(&CommandRun { signal: Some(9), ..done.clone() }), "killed by signal 9");
        assert_eq!(result_text(&CommandRun { timed_out: true, signal: Some(15), ..done.clone() }), "timed out");
        assert_eq!(result_text(&CommandRun { error: Some(LOST.into()), ..done }), LOST);
    }
}
