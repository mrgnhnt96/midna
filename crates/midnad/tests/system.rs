//! Runaway work: `system.load`, pausing and resuming a terminal, stopping what it started, and
//! the git worktrees agents leave behind (`worktrees.list|clean`).
mod common;
use common::*;
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// `ps` state letter(s) of a process (`T` = stopped), or None once it's gone.
fn state(pid: i64) -> Option<String> {
    let o = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// A shell terminal running `sleep 300` as a child; (terminal id, shell pid, sleep pid).
fn sleeper(h: &mut Client) -> (String, i64, i64) {
    let s = call(h, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/sh", "-c", "sleep 300 & wait"] }));
    let id = s["id"].as_str().unwrap().to_string();
    let procs = wait_for(10, "the sleep child", || {
        let p = call(h, "session.processes", json!({ "id": id }));
        p.as_array().filter(|a| a.iter().any(|p| p["name"] == "sleep")).cloned()
    });
    let pid = |name: &str| procs.iter().find(|p| p["name"] == name).unwrap()["pid"].as_i64().unwrap();
    (id, procs[0]["pid"].as_i64().unwrap(), pid("sleep"))
}

#[test]
fn load_is_reported() {
    let d = TestDaemon::start();
    let l = call(&mut d.human(), "system.load", json!({}));
    assert!(l["cpus"].as_u64().unwrap() >= 1, "{l}");
    assert_eq!(l["busy_at"], 150, "{l}");
    assert!(l["top"].is_array(), "{l}");
}

#[test]
fn pause_stops_the_whole_tree_and_resume_continues_it() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let (id, sh, sleep) = sleeper(&mut h);
    let r = call(&mut h, "session.pause", json!({ "session_id": id }));
    assert_eq!((r["paused"].as_bool(), r["processes"].as_u64()), (Some(true), Some(2)), "{r}");
    for pid in [sh, sleep] {
        assert!(state(pid).unwrap().starts_with('T'), "{pid} not stopped: {:?}", state(pid));
    }
    // The terminal says it's paused, and system.load lists it though it uses no CPU now.
    let s = call(&mut h, "session.get", json!({ "id": id }));
    assert_eq!((s["paused"]["processes"].as_u64(), s["paused"]["pid"].as_i64()), (Some(2), Some(sh)), "{s}");
    let l = call(&mut h, "system.load", json!({}));
    assert!(l["top"].as_array().unwrap().iter().any(|t| t["session_id"] == id.as_str() && t["paused"] == true), "{l}");
    // Agents can't pause a terminal by themselves.
    let e = call_err(&mut d.agent(None), "session.pause", json!({ "session_id": id }));
    assert_ne!(e.code, 0, "{e:?}");
    call(&mut h, "session.resume", json!({ "session_id": id }));
    for pid in [sh, sleep] {
        assert!(!state(pid).unwrap().starts_with('T'), "{pid} still stopped: {:?}", state(pid));
    }
    let s = call(&mut h, "session.get", json!({ "id": id }));
    assert!(s.get("paused").is_none(), "{s}");
    let kinds: Vec<String> = call(&mut h, "events.list", json!({ "since_seq": 0, "limit": 1000 })).as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect();
    assert!(kinds.contains(&"session.paused".into()) && kinds.contains(&"session.resumed".into()), "{kinds:?}");
}

#[test]
fn stop_processes_keeps_the_terminal_running() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let (id, sh, sleep) = sleeper(&mut h);
    // The terminal's own process can't be picked.
    let e = call_err(&mut h, "session.stop_processes", json!({ "session_id": id, "pids": [sh] }));
    assert!(e.message.contains("isn't a process under"), "{e:?}");
    let r = call(&mut h, "session.stop_processes", json!({ "session_id": id }));
    let stopped: Vec<i64> = r["stopped"].as_array().unwrap().iter().map(|p| p["pid"].as_i64().unwrap()).collect();
    assert_eq!(stopped, vec![sleep], "{r}");
    wait_for(10, "sleep to end", || state(sleep).is_none_or(|s| s.starts_with('Z')).then_some(()));
    // `sh -c 'sleep & wait'` ends once its child does; that's the shell's choice, not ours.
    let _ = sh;
}

// ------------------------------------------------------------------ worktrees

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(tag: &str) -> Repo {
        let root = PathBuf::from(format!("/tmp/midna-wt-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("main")).unwrap();
        let r = Repo { root };
        r.git("main", &["init", "-q", "-b", "main"]);
        std::fs::write(r.main().join("a.txt"), "a\n").unwrap();
        r.git("main", &["add", "."]);
        r.git("main", &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "one"]);
        r
    }

    fn main(&self) -> PathBuf {
        self.root.join("main")
    }

    fn wt(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn git(&self, dir: &str, args: &[&str]) {
        let o = Command::new("git").args(args).current_dir(self.root.join(dir)).output().unwrap();
        assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    }

    fn add(&self, name: &str) -> String {
        self.git("main", &["worktree", "add", "-q", "-b", name, &self.wt(name).to_string_lossy()]);
        // git reports the realpath (/private/tmp on macOS).
        std::fs::canonicalize(self.wt(name)).unwrap().to_string_lossy().into_owned()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn worktrees(h: &mut Client) -> Vec<Value> {
    call(h, "worktrees.list", json!({}))["worktrees"].as_array().unwrap().clone()
}

fn find<'a>(list: &'a [Value], path: &str) -> &'a Value {
    list.iter().find(|w| w["path"] == path).unwrap_or_else(|| panic!("{path} not listed: {list:?}"))
}

#[test]
fn worktrees_are_cleaned_only_when_idle_unused_clean_and_unlocked() {
    let repo = Repo::new("rules");
    let d = TestDaemon::start();
    let mut h = d.human();
    // A terminal in the main checkout makes midna watch the repo.
    call(&mut h, "session.open", json!({ "kind": "shell", "cwd": repo.main(), "command": ["/bin/sh"] }));
    let idle = repo.add("idle");
    let used = repo.add("used");
    let dirty = repo.add("dirty");
    let stale_lock = repo.add("stale");
    let live_lock = repo.add("live");
    call(&mut h, "session.open", json!({ "kind": "shell", "cwd": used, "command": ["/bin/sh"] }));
    std::fs::write(Path::new(&dirty).join("a.txt"), "changed\n").unwrap();
    repo.git("main", &["worktree", "lock", "--reason", "claude agent agent-x (pid 999999 start Fri Oct  9 02:01:00 2026)", &stale_lock]);
    let me = std::process::id();
    repo.git("main", &["worktree", "lock", "--reason", &format!("claude agent agent-y (pid {me} start Fri Oct  9 02:01:00 2026)"), &live_lock]);

    let list = worktrees(&mut h);
    assert_eq!(list.len(), 5, "{list:?}");
    let keep = |p: &str| find(&list, p)["keep_because"].as_str().unwrap_or("").to_string();
    assert!(keep(&idle).starts_with("it was used"), "{}", keep(&idle));
    assert!(keep(&used).contains("terminal"), "{}", keep(&used));
    assert!(keep(&dirty).contains("uncommitted"), "{}", keep(&dirty));
    assert!(keep(&live_lock).contains("locked"), "{}", keep(&live_lock));
    // A lock whose pid is gone doesn't keep it; only the idle time does.
    assert!(keep(&stale_lock).starts_with("it was used"), "{}", keep(&stale_lock));
    assert!(list.iter().all(|w| w["removable"] == false), "fresh worktrees aren't idle yet");

    // Nothing qualifies by the idle rule; ignore_idle takes the idle and stale-locked ones.
    let r = call(&mut h, "worktrees.clean", json!({}));
    assert_eq!(r["removed"], json!([]), "{r}");
    let dry = call(&mut h, "worktrees.clean", json!({ "ignore_idle": true, "dry_run": true }));
    let mut gone: Vec<String> = dry["removed"].as_array().unwrap().iter().map(|w| w["path"].as_str().unwrap().to_string()).collect();
    gone.sort();
    let mut want = vec![idle.clone(), stale_lock.clone()];
    want.sort();
    assert_eq!(gone, want, "{dry}");
    assert!(Path::new(&idle).exists(), "dry run removed it");

    let r = call(&mut h, "worktrees.clean", json!({ "ignore_idle": true }));
    assert_eq!(r["removed"].as_array().unwrap().len(), 2, "{r}");
    assert!(!Path::new(&idle).exists() && !Path::new(&stale_lock).exists());
    for p in [&used, &dirty, &live_lock] {
        assert!(Path::new(p).exists(), "{p} was removed");
    }
    // The branches stay.
    let branches = String::from_utf8(Command::new("git").args(["branch", "--list"]).current_dir(repo.main()).output().unwrap().stdout).unwrap();
    assert!(branches.contains("idle") && branches.contains("stale"), "{branches}");
    let removed: Vec<Value> = call(&mut h, "events.list", json!({ "since_seq": 0, "limit": 1000, "filter": { "kinds": ["worktree.removed"] } })).as_array().unwrap().clone();
    assert_eq!(removed.len(), 2, "{removed:?}");
}

#[test]
fn clean_one_path() {
    let repo = Repo::new("one");
    let d = TestDaemon::start();
    let mut h = d.human();
    call(&mut h, "project.add", json!({ "path": repo.main() }));
    let a = repo.add("a");
    let b = repo.add("b");
    let e = call_err(&mut h, "worktrees.clean", json!({ "path": "/nowhere", "ignore_idle": true }));
    assert!(e.message.contains("isn't a linked worktree"), "{e:?}");
    let r = call(&mut h, "worktrees.clean", json!({ "path": a, "ignore_idle": true }));
    assert_eq!(r["removed"].as_array().unwrap().len(), 1, "{r}");
    assert!(!Path::new(&a).exists() && Path::new(&b).exists());
}
