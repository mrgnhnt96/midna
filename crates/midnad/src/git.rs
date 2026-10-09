//! Git info for terminals, from git itself (and the `gh` CLI for PRs, cached ~30s).
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Run a command with a timeout; Some(stdout) on exit status 0.
pub fn run_cmd(argv: &[&str], cwd: &str, timeout: Duration, stdin: Option<&[u8]>, env: &[(String, String)]) -> Option<String> {
    let mut cmd = Command::new(argv[0]);
    cmd.args(&argv[1..]).current_dir(cwd).stdout(Stdio::piped()).stderr(Stdio::null());
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.env("GIT_OPTIONAL_LOCKS", "0").env("GH_PROMPT_DISABLED", "1");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().ok()?;
    if let (Some(input), Some(mut w)) = (stdin, child.stdin.take()) {
        use std::io::Write;
        let _ = w.write_all(input);
    }
    // Read stdout on a thread so a chatty child can't block on a full pipe.
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut out, &mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let s = reader.join().ok()?;
    status.success().then_some(s)
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["git"];
    argv.extend_from_slice(args);
    run_cmd(&argv, cwd, Duration::from_secs(3), None, &[]).map(|s| s.trim().to_string())
}

/// Parse `git diff --shortstat`: "3 files changed, 10 insertions(+), 2 deletions(-)".
pub fn parse_shortstat(s: &str) -> (u32, u32, u32) {
    let (mut files, mut added, mut removed) = (0, 0, 0);
    for part in s.split(',') {
        let mut it = part.split_whitespace();
        let n: u32 = it.next().and_then(|n| n.parse().ok()).unwrap_or(0);
        match it.next().unwrap_or("") {
            w if w.starts_with("file") => files = n,
            w if w.starts_with("insertion") => added = n,
            w if w.starts_with("deletion") => removed = n,
            _ => {}
        }
    }
    (files, added, removed)
}

/// `git rev-parse --git-dir --git-common-dir --show-toplevel --abbrev-ref HEAD` →
/// (branch, linked worktree name). The two dirs differ only in a linked worktree.
fn parse_rev_parse(out: &str) -> Option<(String, Option<String>)> {
    let mut it = out.lines().map(str::trim);
    let (dir, common, top, branch) = (it.next()?, it.next()?, it.next()?, it.next()?);
    let worktree = (dir != common).then(|| top.rsplit('/').next().unwrap_or(top).to_string());
    Some((branch.to_string(), worktree))
}

/// Branch, linked worktree, ahead/behind upstream and uncommitted diff stats. None outside a repo.
pub fn git_info(cwd: &str) -> Option<GitInfo> {
    let (branch, worktree) = parse_rev_parse(&git(cwd, &["rev-parse", "--path-format=absolute", "--git-dir", "--git-common-dir", "--show-toplevel", "--abbrev-ref", "HEAD"])?)?;
    let (behind, ahead) = git(cwd, &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
        .and_then(|s| {
            let mut it = s.split_whitespace().map(|n| n.parse::<u32>().ok());
            Some((it.next()??, it.next()??))
        })
        .unwrap_or((0, 0));
    let (files, added, removed) = git(cwd, &["diff", "--shortstat", "HEAD"]).map(|s| parse_shortstat(&s)).unwrap_or((0, 0, 0));
    let pr = pr_info(cwd, &branch);
    Some(GitInfo { branch, ahead, behind, added, removed, files, pr, worktree })
}

// ------------------------------------------------------------------ gh PR cache

type PrCache = HashMap<(String, String), (Instant, Option<PrInfo>)>;
static PR_CACHE: Mutex<Option<PrCache>> = Mutex::new(None);
const PR_TTL: Duration = Duration::from_secs(30);

fn gh_available() -> bool {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AVAILABLE.get_or_init(|| std::env::var("MIDNA_NO_GH").is_err() && run_cmd(&["gh", "--version"], "/", Duration::from_secs(3), None, &[]).is_some())
}

pub fn checks_state(rollup: &[Value]) -> (ChecksState, u32) {
    if rollup.is_empty() {
        return (ChecksState::None, 0);
    }
    let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_ascii_uppercase();
    let failing = rollup
        .iter()
        .filter(|c| {
            matches!(s(c, "conclusion").as_str(), "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE")
                || matches!(s(c, "state").as_str(), "FAILURE" | "ERROR")
        })
        .count() as u32;
    let pending = rollup.iter().any(|c| {
        let st = s(c, "status");
        (!st.is_empty() && st != "COMPLETED") || matches!(s(c, "state").as_str(), "PENDING" | "EXPECTED")
    });
    let state = if failing > 0 {
        ChecksState::Failing
    } else if pending {
        ChecksState::Pending
    } else {
        ChecksState::Passing
    };
    (state, failing)
}

/// The PR for this branch via `gh pr view`, cached ~30s. None if gh is missing or no PR.
pub fn pr_info(cwd: &str, branch: &str) -> Option<PrInfo> {
    if !gh_available() || branch == "HEAD" {
        return None;
    }
    let key = (cwd.to_string(), branch.to_string());
    if let Some((at, v)) = PR_CACHE.lock().ok()?.get_or_insert_with(HashMap::new).get(&key)
        && at.elapsed() < PR_TTL {
            return v.clone();
        }
    let out = run_cmd(&["gh", "pr", "view", "--json", "number,url,statusCheckRollup"], cwd, Duration::from_secs(10), None, &[]);
    let pr = out.and_then(|s| serde_json::from_str::<Value>(&s).ok()).and_then(|v| {
        let rollup = v.get("statusCheckRollup").and_then(Value::as_array).cloned().unwrap_or_default();
        let (checks, failing_count) = checks_state(&rollup);
        Some(PrInfo { number: v.get("number")?.as_u64()?, url: v.get("url")?.as_str()?.to_string(), checks, failing_count })
    });
    PR_CACHE.lock().ok()?.get_or_insert_with(HashMap::new).insert(key, (Instant::now(), pr.clone()));
    pr
}

// ------------------------------------------------------------------ refresh

/// The folder a terminal works in now: where its agent's hooks say it is (a worktree it moved
/// into), else where its shell (`pid`) is, else where it was opened.
pub fn work_cwd(s: &Session, pid: Option<i32>) -> String {
    let is_dir = |c: &String| std::path::Path::new(c).is_dir();
    s.agent_info.as_ref().and_then(|i| i.cwd.clone()).filter(is_dir)
        .or_else(|| pid.and_then(crate::procs::cwd).filter(is_dir))
        .unwrap_or_else(|| s.cwd.clone())
}

/// Recompute one session's GitInfo; store it and emit `session.git` when it changed.
pub fn refresh_session(d: &Daemon, sid: &str) {
    let Some(cwd) = ({
        let core = d.core();
        core.state.session(sid).map(|s| work_cwd(s, core.rt.get(sid).map(|r| r.pid)))
    }) else { return };
    let info = git_info(&cwd);
    let project = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return };
        if s.git == info {
            return;
        }
        s.git = info.clone();
        s.project_id.clone()
    };
    crate::cleanup::observe(d, sid, &cwd, info.as_ref());
    d.mark_dirty();
    d.emit(kinds::SESSION_GIT, Actor::system(), Some(project), Some(sid.into()), json!({ "git": info }));
    crate::auto_name::on_context(d, sid);
}

pub fn refresh_session_async(d: &Arc<Daemon>, sid: &str) {
    let (d, sid) = (Arc::downgrade(d), sid.to_string());
    std::thread::spawn(move || {
        if let Some(d) = d.upgrade() {
            refresh_session(&d, &sid);
        }
    });
}

/// Background loop: refresh git info for live sessions every `git.refresh_secs`.
pub fn refresh_loop(d: std::sync::Weak<Daemon>) {
    loop {
        let secs = match d.upgrade() {
            Some(d) if !d.shutting_down.load(std::sync::atomic::Ordering::Relaxed) => {
                let live: Vec<Id> = d.core().rt.keys().cloned().collect();
                for sid in live {
                    refresh_session(&d, &sid);
                }
                d.core().state.setting_i64("git.refresh_secs").max(2) as u64
            }
            _ => return,
        };
        std::thread::sleep(Duration::from_secs(secs));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortstat() {
        assert_eq!(parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)"), (3, 10, 2));
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (1, 0, 1));
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }

    #[test]
    fn worktree_only_when_git_dir_is_not_the_common_dir() {
        assert_eq!(parse_rev_parse("/r/.git\n/r/.git\n/r\nmain\n"), Some(("main".into(), None)));
        assert_eq!(parse_rev_parse("/r/.git/worktrees/fix\n/r/.git\n/w/fix-login\nfix/login\n"), Some(("fix/login".into(), Some("fix-login".into()))));
        assert_eq!(parse_rev_parse("/r/.git\n"), None);
    }

    #[test]
    fn works_where_the_agent_is_not_where_the_tab_opened() {
        let tmp = std::env::temp_dir().canonicalize().unwrap().to_string_lossy().into_owned();
        let mut s: Session = serde_json::from_value(json!({
            "id": "s1", "project_id": "p", "name": "t", "kind": "agent", "agent": "claude", "cwd": "/repo", "command": ["claude"],
            "status": { "state": "idle", "since": "2026-01-01T00:00:00Z" }, "created_at": "2026-01-01T00:00:00Z", "last_activity_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        assert_eq!(work_cwd(&s, None), "/repo");
        s.agent_info = Some(AgentInfo { cwd: Some(tmp.clone()), ..Default::default() });
        assert_eq!(work_cwd(&s, None), tmp, "the agent's worktree");
        s.agent_info = Some(AgentInfo { cwd: Some("/gone/worktree".into()), ..Default::default() });
        assert_eq!(work_cwd(&s, None), "/repo", "a removed worktree falls back");
        let here = std::env::current_dir().unwrap().to_string_lossy().into_owned();
        assert_eq!(work_cwd(&s, Some(std::process::id() as i32)), here, "else the shell's folder");
    }

    #[test]
    fn checks() {
        let v: Vec<Value> = serde_json::from_str(r#"[{"status":"COMPLETED","conclusion":"SUCCESS"},{"status":"IN_PROGRESS","conclusion":""}]"#).unwrap();
        assert_eq!(checks_state(&v), (ChecksState::Pending, 0));
        let v: Vec<Value> = serde_json::from_str(r#"[{"status":"COMPLETED","conclusion":"FAILURE"},{"state":"SUCCESS"}]"#).unwrap();
        assert_eq!(checks_state(&v), (ChecksState::Failing, 1));
    }
}
