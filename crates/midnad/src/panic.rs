//! After a kernel panic (#106). macOS writes a `.panic` report into DiagnosticReports when the
//! Mac boots again after one. For a while after midnad starts it looks for a report written since
//! this boot that it hasn't handled yet (`State.panic_seen`). On one, it raises a needs-you saying
//! what each terminal was running before the Mac went down (their last tool calls in the event
//! log), and with `guard.panic_hold`, agents don't start on their own until the human closes it
//! (`State.panic_hold`): triggers, other agents and apps like the task board can't open or
//! restart an agent terminal (`gate`). The human still can. Closing the item any way lifts the
//! hold, `needs_you.expire_hours` withdrawing it too, so the hold never outlasts what explains it.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long after midnad starts it keeps looking: the report can be written after login.
const WATCH_FOR: Duration = Duration::from_secs(15 * 60);
const EVERY: Duration = Duration::from_secs(20);
/// A report this long before the boot still counts as this boot's (clocks settle).
const BOOT_SLACK: i64 = 120;
/// Terminals with a tool call this long before the last event before the panic are listed.
const ACTIVE_BEFORE: i64 = 15 * 60;
/// Reports bigger than this aren't read (they're a few hundred KB).
const MAX_REPORT: u64 = 8 << 20;

/// The folders macOS keeps panic reports in (it moves older ones to Retired).
pub fn report_dirs() -> Vec<PathBuf> {
    let dir = PathBuf::from("/Library/Logs/DiagnosticReports");
    vec![dir.join("Retired"), dir]
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("panic".into()).spawn(move || {
        let started = Instant::now();
        while started.elapsed() < WATCH_FOR {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            check(&d);
            drop(d);
            std::thread::sleep(EVERY);
        }
    });
}

/// When this boot started, unix seconds.
fn boot_time() -> i64 {
    let mut tv: libc::timeval = unsafe { std::mem::zeroed() };
    let mut size = std::mem::size_of::<libc::timeval>();
    let mut mib = [libc::CTL_KERN, libc::KERN_BOOTTIME];
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, &mut tv as *mut _ as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) };
    if r == 0 { tv.tv_sec } else { 0 }
}

/// The newest panic report in `dirs` written at or after `since` (unix seconds).
fn newest_report(dirs: &[PathBuf], since: i64) -> Option<PathBuf> {
    let mut best: Option<(i64, PathBuf)> = None;
    for dir in dirs {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || path.extension().is_none_or(|x| x != "panic") {
                continue;
            }
            let Some(mtime) = e.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()) else { continue };
            let mtime = mtime.as_secs() as i64;
            if mtime >= since && best.as_ref().is_none_or(|(t, _)| mtime > *t) {
                best = Some((mtime, path));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// A report's time (its header's `timestamp`) and the panic's first line
/// (`panic(cpu 0 caller …): userspace watchdog timeout: no successful checkins from WindowServer`).
/// The report is a JSON header line, then a JSON body with `panicString`.
pub fn parse_report(text: &str) -> (Option<String>, Option<String>) {
    let (head, body) = text.split_once('\n').unwrap_or((text, ""));
    let header: Value = serde_json::from_str(head).unwrap_or(Value::Null);
    let when = header.get("timestamp").and_then(Value::as_str).map(str::to_string);
    let panic = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|b| b.get("panicString").and_then(Value::as_str).map(str::to_string))
        .or_else(|| text.lines().find(|l| l.contains("panic(")).map(str::to_string))
        .and_then(|p| p.lines().map(str::trim).find(|l| !l.is_empty()).map(|l| l.chars().take(300).collect()));
    (when, panic)
}

/// A terminal's last tool call before the Mac went down.
struct Ran {
    session: Id,
    value: String,
    at: String,
}

/// Each terminal's last tool call in the event log before this daemon started, within
/// ACTIVE_BEFORE of the last event the previous run logged (when the Mac went down), newest first.
fn before_panic(d: &Daemon) -> Vec<Ran> {
    d.log.with_events(|evs| {
        let start = evs.iter().rposition(|e| e.kind == kinds::DAEMON_STARTED).unwrap_or(evs.len());
        let before = &evs[..start];
        let Some(last) = before.last().and_then(|e| time::parse_rfc3339(&e.at)) else { return vec![] };
        let mut out: Vec<Ran> = vec![];
        for e in before.iter().rev() {
            if time::parse_rfc3339(&e.at).is_some_and(|t| t < last - ACTIVE_BEFORE) {
                break;
            }
            if !matches!(e.kind.as_str(), kinds::RULE_FIRED | kinds::POLICY_DECIDED) {
                continue;
            }
            let (Some(sid), Some(value)) = (e.session_id.as_ref(), e.data.pointer("/action/value").and_then(Value::as_str)) else { continue };
            if !out.iter().any(|r| &r.session == sid) {
                out.push(Ran { session: sid.clone(), value: value.chars().take(160).collect(), at: e.at.clone() });
            }
        }
        out
    })
}

/// Look for a panic report from this boot that midnad hasn't handled; handle it.
pub fn check(d: &Daemon) {
    let Some(path) = newest_report(&d.cfg.panic_dirs, boot_time() - BOOT_SLACK) else { return };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    {
        let mut core = d.core();
        if core.state.panic_seen.as_deref() == Some(name.as_str()) {
            return;
        }
        core.state.panic_seen = Some(name.clone());
    }
    d.mark_dirty();
    let text = std::fs::metadata(&path).ok().filter(|m| m.len() <= MAX_REPORT).and_then(|_| std::fs::read_to_string(&path).ok()).unwrap_or_default();
    let (when, panic) = parse_report(&text);
    let ran = before_panic(d);
    let hold = d.setting("guard.panic_hold").as_bool().unwrap_or(true);
    let names: Vec<(String, bool)> = {
        let core = d.core();
        ran.iter().map(|r| core.state.session(&r.session).map(|s| (s.name.clone(), s.agent.is_some())).unwrap_or_else(|| (r.session.clone(), false))).collect()
    };
    let mut detail = format!("{} ({}).", panic.as_deref().unwrap_or("macOS restarted after a kernel panic"), when.as_deref().unwrap_or("time unknown"));
    if ran.is_empty() {
        detail.push_str("\n\nNo terminal ran a tool call in the 15 minutes before.");
    } else {
        detail.push_str("\n\nWhat each terminal ran last before the Mac went down:");
        for (r, (n, _)) in ran.iter().zip(&names) {
            detail.push_str(&format!("\n• {n} ({}), {}: {}", r.session, r.at, r.value));
        }
    }
    if hold {
        detail.push_str(
            "\n\nUntil you dismiss this, agents don't start on their own: triggers, other agents and apps like the task board can't open or restart an agent terminal (guard.panic_hold). You still can. If a model run or build took the memory, let one at a time go first (guard.model_slots, guard.build_slots).",
        );
    }
    let mut item = d.new_needs_you(NeedsYouKind::Note, "The Mac restarted after a kernel panic".into(), Actor::system(), None);
    item.detail = detail;
    let item = d.raise_needs_you(item);
    if hold {
        d.core().state.panic_hold = Some(item.id.clone());
        d.mark_dirty();
    }
    eprintln!("midnad[{}]: kernel panic report {name}: {}", std::process::id(), panic.as_deref().unwrap_or("?"));
    let terminals: Vec<Value> = ran.iter().zip(&names).map(|(r, (n, agent))| json!({ "session_id": r.session, "name": n, "agent": agent, "last_tool": r.value, "at": r.at })).collect();
    d.emit(
        kinds::SYSTEM_PANICKED,
        Actor::system(),
        None,
        None,
        json!({ "report": path_str(&path), "panic": panic, "at": when, "terminals": terminals, "needs_you_id": item.id, "hold": hold }),
    );
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Why agents may not start on their own now, or None. An open hold whose item is gone is over.
pub fn held(d: &Daemon) -> Option<String> {
    let mut core = d.core();
    let id = core.state.panic_hold.clone()?;
    if core.state.needs_you.iter().any(|n| n.id == id) {
        return Some(format!(
            "the Mac restarted after a kernel panic, and agents don't start on their own until the human has seen what was running and dismissed needs-you {id} (guard.panic_hold). Tell the human; don't retry in a loop."
        ));
    }
    core.state.panic_hold = None;
    drop(core);
    d.mark_dirty();
    None
}

/// Refuse to start an agent for anyone but the human while held.
pub fn gate(d: &Daemon, ctx: &Ctx) -> Result<(), RpcError> {
    if ctx.is_human() {
        return Ok(());
    }
    match held(d) {
        Some(why) => Err(RpcError::refused(why)),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports() {
        let text = "{\"bug_type\":\"210\",\"timestamp\":\"2026-10-23 10:28:00.00 -0600\"}\n{\"panicString\":\"panic(cpu 0 caller 0xfe): userspace watchdog timeout: no successful checkins from WindowServer\\nDebugger message: panic\"}";
        let (when, panic) = parse_report(text);
        assert_eq!(when.as_deref(), Some("2026-10-23 10:28:00.00 -0600"));
        assert_eq!(panic.as_deref(), Some("panic(cpu 0 caller 0xfe): userspace watchdog timeout: no successful checkins from WindowServer"));
        assert_eq!(parse_report("garbage\npanic(cpu 1): watchdog timeout\nmore"), (None, Some("panic(cpu 1): watchdog timeout".into())));
        assert_eq!(parse_report(""), (None, None));
    }
}
