//! Linked git worktrees agents leave behind (Claude's `isolation: worktree` subagents make one
//! per task, each with its own multi-GB `target/` or `node_modules/`).
//!
//! midnad watches the repos its terminals and projects are in. Every TICK it lists their
//! worktrees and notes the ones in use (a terminal's folder or any process's working folder
//! inside). A worktree is removed (`git worktree remove`, never forced; the branch stays) once
//! - it has been idle `worktrees.auto_clean_hours`: no use seen, and its HEAD, index and reflog
//!   untouched for that long;
//! - no terminal and no process is in it;
//! - it has no uncommitted changes to tracked files (git itself refuses untracked files);
//! - it isn't locked, or its lock names a pid that is gone (Claude Code locks an agent's
//!   worktree as `claude agent <name> (pid N …)` and leaves the lock when it dies).
//!
//! Book-keeping lives in `MIDNA_HOME/worktrees.json` (repos, last time each was seen in use).
use crate::daemon::Daemon;
use crate::git::run_cmd;
use crate::procs;
use midna_proto::system::{self as sys, WorktreeFailure, WorktreeInfo};
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

const TICK: Duration = Duration::from_secs(10 * 60);
const FIRST_TICK: Duration = Duration::from_secs(90);
const GIT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Runtime(Mutex<Book>);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Book {
    #[serde(default)]
    repos: BTreeSet<String>,
    /// Worktree path -> unix time it was last seen in use.
    #[serde(default)]
    seen: BTreeMap<String, i64>,
    /// Removals git refused, so the log says so once.
    #[serde(default)]
    refused: BTreeMap<String, String>,
    #[serde(skip)]
    loaded: bool,
}

impl Runtime {
    fn book(&self, home: &Path) -> MutexGuard<'_, Book> {
        let mut b = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !b.loaded {
            *b = std::fs::read(file(home)).ok().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
            b.loaded = true;
        }
        b
    }
}

fn file(home: &Path) -> PathBuf {
    home.join("worktrees.json")
}

fn save(home: &Path, b: &Book) {
    if let Ok(v) = serde_json::to_vec_pretty(b) {
        let tmp = file(home).with_extension("json.tmp");
        if std::fs::write(&tmp, v).is_ok() {
            let _ = std::fs::rename(tmp, file(home));
        }
    }
}

fn log(msg: &str) {
    eprintln!("midnad[{}]: {msg}", std::process::id());
}

pub fn start(d: &Arc<Daemon>) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("worktrees".into()).spawn(move || {
        std::thread::sleep(FIRST_TICK);
        loop {
            let Some(d) = w.upgrade() else { return };
            if d.shutting_down.load(Ordering::Relaxed) {
                return;
            }
            let hours = auto_clean_hours(&d);
            let r = scan(&d, None);
            if hours > 0 {
                remove(&d, &r, |w| w.removable, "auto_clean");
            }
            drop(d);
            std::thread::sleep(TICK);
        }
    });
}

fn auto_clean_hours(d: &Daemon) -> u32 {
    d.setting("worktrees.auto_clean_hours").as_i64().unwrap_or(24).max(0) as u32
}

// ------------------------------------------------------------------ git

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["git"];
    argv.extend_from_slice(args);
    run_cmd(&argv, cwd, GIT, None, &[])
}

/// A `git worktree list --porcelain` entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    pub path: String,
    pub branch: String,
    pub locked: Option<String>,
    pub bare: bool,
}

pub fn parse_list(out: &str) -> Vec<Entry> {
    let mut v: Vec<Entry> = vec![];
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            v.push(Entry { path: p.to_string(), ..Default::default() });
        } else if let Some(e) = v.last_mut() {
            if let Some(b) = line.strip_prefix("branch ") {
                e.branch = b.trim_start_matches("refs/heads/").to_string();
            } else if line == "locked" {
                e.locked = Some(String::new());
            } else if let Some(r) = line.strip_prefix("locked ") {
                e.locked = Some(r.to_string());
            } else if line == "bare" {
                e.bare = true;
            }
        }
    }
    v
}

/// The main checkout of the repo `cwd` is in (None outside git or in a bare repo).
fn main_checkout(cwd: &str) -> Option<String> {
    let out = git(cwd, &["worktree", "list", "--porcelain"])?;
    parse_list(&out).into_iter().next().filter(|e| !e.bare).map(|e| e.path)
}

/// Newest mtime (unix) of the worktree's own git files: HEAD moves on checkout/commit, the index
/// on add/status, the reflog on every ref change.
fn git_activity(path: &str) -> Option<i64> {
    let dotgit = std::fs::read_to_string(Path::new(path).join(".git")).ok()?;
    let gitdir = dotgit.trim().strip_prefix("gitdir: ")?;
    let gitdir = if Path::new(gitdir).is_absolute() { PathBuf::from(gitdir) } else { Path::new(path).join(gitdir) };
    ["HEAD", "index", "logs/HEAD"]
        .iter()
        .chain(std::iter::once(&"."))
        .filter_map(|f| std::fs::metadata(gitdir.join(f)).ok()?.modified().ok())
        .chain(std::fs::metadata(path).ok().and_then(|m| m.modified().ok()))
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .max()
}

fn pid_alive(pid: i32) -> bool {
    pid > 0 && (unsafe { libc::kill(pid, 0) } == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
}

// ------------------------------------------------------------------ scan

/// Every tracked worktree with what keeps it (or not). `only_repo` (a folder in the repo or one
/// of its worktrees) limits it to that repo.
fn scan(d: &Daemon, only_repo: Option<&str>) -> Vec<WorktreeInfo> {
    let only_repo = only_repo.map(|f| main_checkout(f).unwrap_or_else(|| f.to_string()));
    let hours = auto_clean_hours(d);
    let (folders, terminals): (Vec<String>, Vec<(Id, String)>) = {
        let core = d.core();
        let live: BTreeSet<Id> = core.rt.keys().cloned().collect();
        let terminals: Vec<(Id, String)> = core.state.sessions.iter().filter(|s| live.contains(&s.id)).map(|s| (s.id.clone(), s.cwd.clone())).collect();
        let mut folders: Vec<String> = terminals.iter().map(|(_, c)| c.clone()).collect();
        folders.extend(core.state.projects.iter().map(|p| p.path.clone()));
        (folders, terminals)
    };
    // Terminal folders and projects name the repos to watch.
    let known: BTreeSet<String> = d.worktrees.book(&d.cfg.home).repos.clone();
    let mut found = BTreeSet::new();
    for f in folders.iter().filter(|f| !f.is_empty() && Path::new(f).is_dir()) {
        if !known.iter().any(|r| sys::path_within(f, r)) && let Some(main) = main_checkout(f) {
            found.insert(main);
        }
    }
    let process_cwds: Vec<String> = procs::all_pids().into_iter().filter(|&p| p > 0).filter_map(procs::cwd).collect();
    let now = time::now_unix();
    let mut out = vec![];
    let mut book = d.worktrees.book(&d.cfg.home);
    book.repos.extend(found);
    let repos: Vec<String> = book.repos.iter().filter(|r| only_repo.as_ref().is_none_or(|o| o == *r)).cloned().collect();
    drop(book);
    for repo in repos {
        let Some(list) = git(&repo, &["worktree", "list", "--porcelain"]) else {
            if !Path::new(&repo).is_dir() {
                d.worktrees.book(&d.cfg.home).repos.remove(&repo);
            }
            continue;
        };
        for e in parse_list(&list).into_iter().skip(1) {
            let mut w = WorktreeInfo { path: e.path.clone(), repo: repo.clone(), branch: e.branch.clone(), ..Default::default() };
            w.terminals = terminals.iter().filter(|(_, c)| sys::path_within(c, &e.path)).map(|(id, _)| id.clone()).collect();
            w.processes = process_cwds.iter().filter(|c| sys::path_within(c, &e.path)).count() as u32;
            let in_use = !w.terminals.is_empty() || w.processes > 0;
            let seen = {
                let mut book = d.worktrees.book(&d.cfg.home);
                if in_use {
                    book.seen.insert(e.path.clone(), now);
                }
                book.seen.get(&e.path).copied()
            };
            let exists = Path::new(&e.path).is_dir();
            let last = seen.into_iter().chain(git_activity(&e.path)).max();
            w.last_active_at = last.map(time::format_unix);
            w.idle_hours = last.map(|t| ((now - t).max(0) as f64 / 3600. * 10.).round() / 10.).unwrap_or(0.);
            w.lock_reason = e.locked.clone().filter(|r| !r.is_empty());
            let live_lock = e.locked.as_deref().is_some_and(|r| sys::lock_pid(r).is_none_or(pid_alive));
            w.locked = e.locked.is_some();
            w.dirty = exists && git(&e.path, &["status", "--porcelain", "--untracked-files=no"]).is_none_or(|s| !s.trim().is_empty());
            w.keep_because = if !exists {
                Some("its folder is gone (git worktree prune clears it)".into())
            } else if !w.terminals.is_empty() {
                Some(format!("{} terminal(s) are in it", w.terminals.len()))
            } else if w.processes > 0 {
                Some(format!("{} process(es) are working in it", w.processes))
            } else if live_lock {
                Some(format!("it is locked ({})", w.lock_reason.as_deref().unwrap_or("no reason given")))
            } else if w.dirty {
                Some("it has uncommitted changes".into())
            } else if hours == 0 {
                Some("worktrees.auto_clean_hours is 0".into())
            } else if last.is_none() || w.idle_hours < hours as f64 {
                Some(format!("it was used {:.1}h ago (removed after {hours}h idle)", w.idle_hours))
            } else {
                None
            };
            w.removable = w.keep_because.is_none();
            out.push(w);
        }
    }
    let book = d.worktrees.book(&d.cfg.home);
    save(&d.cfg.home, &book);
    out
}

/// Remove the worktrees `pick` chooses (dry_run: none); report what went and what git refused.
fn remove(d: &Daemon, list: &[WorktreeInfo], pick: impl Fn(&WorktreeInfo) -> bool, by: &str) -> (Vec<WorktreeInfo>, Vec<WorktreeFailure>) {
    let (mut removed, mut failed) = (vec![], vec![]);
    for w in list.iter().filter(|w| pick(w)) {
        if w.locked {
            // Only stale locks get here (a live one keeps the worktree).
            let _ = git(&w.repo, &["worktree", "unlock", &w.path]);
        }
        let out = std::process::Command::new("git")
            .args(["worktree", "remove", &w.path])
            .current_dir(&w.repo)
            .stdin(std::process::Stdio::null())
            .output();
        match out {
            Ok(o) if o.status.success() => {
                log(&format!("removed worktree {} ({}, idle {:.1}h)", w.path, w.branch, w.idle_hours));
                d.emit(kinds::WORKTREE_REMOVED, Actor::system(), None, None, json!({ "path": w.path, "repo": w.repo, "branch": w.branch, "idle_hours": w.idle_hours, "by": by }));
                let mut book = d.worktrees.book(&d.cfg.home);
                book.seen.remove(&w.path);
                book.refused.remove(&w.path);
                removed.push(w.clone());
            }
            other => {
                let error = match other {
                    Ok(o) => String::from_utf8_lossy(&o.stderr).trim().to_string(),
                    Err(e) => e.to_string(),
                };
                if w.locked {
                    let _ = git(&w.repo, &["worktree", "lock", "--reason", w.lock_reason.as_deref().unwrap_or(""), &w.path]);
                }
                let mut book = d.worktrees.book(&d.cfg.home);
                if book.refused.get(&w.path) != Some(&error) {
                    log(&format!("kept worktree {}: git worktree remove said: {error}", w.path));
                    book.refused.insert(w.path.clone(), error.clone());
                }
                failed.push(WorktreeFailure { path: w.path.clone(), error });
            }
        }
    }
    let book = d.worktrees.book(&d.cfg.home);
    save(&d.cfg.home, &book);
    (removed, failed)
}

// ------------------------------------------------------------------ rpc

pub fn list(d: &Daemon, p: WorktreesListParams) -> Result<Value, RpcError> {
    let worktrees = scan(d, p.repo.as_deref());
    let repos = d.worktrees.book(&d.cfg.home).repos.iter().cloned().collect();
    Ok(json!(WorktreesList { worktrees, auto_clean_hours: auto_clean_hours(d), repos }))
}

pub fn clean(d: &Daemon, p: WorktreesCleanParams) -> Result<Value, RpcError> {
    let path = p.path.as_deref().map(|s| s.trim_end_matches('/').to_string());
    let list = scan(d, None);
    if let Some(path) = &path
        && !list.iter().any(|w| &w.path == path)
    {
        return Err(RpcError::not_found(format!("{path} isn't a linked worktree of a repo midna watches (`midna worktrees` lists them)")));
    }
    // ignore_idle waives only the idle rule (and auto-clean being off); terminals, processes,
    // live locks and uncommitted work still keep a worktree.
    let pick = |w: &WorktreeInfo| {
        let chosen = path.as_ref().is_none_or(|p| &w.path == p);
        let idle_only = w.keep_because.as_deref().is_some_and(|k| k.starts_with("it was used") || k.starts_with("worktrees.auto_clean_hours"));
        chosen && (w.removable || (p.ignore_idle && idle_only))
    };
    if p.dry_run {
        let removed = list.iter().filter(|w| pick(w)).cloned().collect();
        return Ok(json!(WorktreesCleanResult { dry_run: true, removed, failed: vec![] }));
    }
    let (removed, failed) = remove(d, &list, pick, "worktrees.clean");
    Ok(json!(WorktreesCleanResult { dry_run: false, removed, failed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain() {
        let out = "worktree /r\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/.claude/worktrees/agent-a\nHEAD def\nbranch refs/heads/worktree-agent-a\nlocked claude agent agent-a (pid 79735 start Fri Oct  9 02:01:00 2026)\n\nworktree /r2\nHEAD 123\ndetached\nlocked\n";
        let v = parse_list(out);
        assert_eq!(v.len(), 3);
        assert_eq!((v[0].path.as_str(), v[0].branch.as_str(), v[0].locked.is_none()), ("/r", "main", true));
        assert_eq!(v[1].branch, "worktree-agent-a");
        assert_eq!(sys::lock_pid(v[1].locked.as_deref().unwrap()), Some(79735));
        assert_eq!(v[2].locked.as_deref(), Some(""));
    }
}
