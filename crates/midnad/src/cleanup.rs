//! Cleaning up after a terminal closes (proto `cleanup.rs` has the wire types).
//!
//! While a terminal lives, `observe` notes the repos, branches and linked worktrees its folder
//! is in (from `git::refresh_session`, when its git info changes), and which branches and
//! worktrees each repo already had when the terminal first went there. When it closes,
//! `capture` takes its process tree before it is killed and `after_close` waits for that tree
//! to go, then works out the targets:
//! - worktree: the linked worktrees its folder was in, and new ones (not there when it came)
//!   that a process of its own was in or locked (Claude's `claude agent … (pid N)` lock);
//! - branch: the branches of those worktrees, and new branches its folder had checked out;
//! - remote_branch: those branches' upstreams.
//!
//! Left out, with the reason in the plan's `skipped`: `cleanup.keep` and the default branch, a
//! worktree another live terminal is in or with uncommitted or untracked files, a branch checked
//! out in a worktree that isn't a target. When there is nothing to do (no targets and none of
//! the user's own items) nothing runs. Otherwise a cheap headless model (`claude -p`) gets the
//! targets, the rules and the user's own items, with only the git and gh commands it needs
//! (plus `cleanup.tools`), in the repo's main checkout. Runs go one at a time per repo.
//!
//! Book-keeping (trails and the last RUNS runs) lives in `MIDNA_HOME/cleanup.json`.
use crate::daemon::Daemon;
use crate::git::run_cmd;
use midna_proto::cleanup::{self as cl, CleanupPlan, CleanupRun, CleanupState, CleanupTarget};
use midna_proto::system::path_within;
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const GIT: Duration = Duration::from_secs(10);
/// Runs kept in the book.
const RUNS: usize = 100;
/// How long to wait for a closed terminal's processes to go before looking at its worktrees.
const SETTLE: Duration = Duration::from_secs(10);
/// What a run still going when midnad last stopped says.
const LOST: &str = "midnad restarted while it ran";
/// The model's answer kept on the run.
const KEEP_SUMMARY: usize = 8 * 1024;

#[derive(Default)]
pub struct Runtime {
    book: Mutex<Book>,
    /// One run at a time per repo (git takes locks; two models deleting the same branch race).
    repos: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Book {
    /// Session id -> what it touched.
    #[serde(default)]
    trails: BTreeMap<Id, Trail>,
    /// Newest last.
    #[serde(default)]
    runs: Vec<CleanupRun>,
    #[serde(skip)]
    loaded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trail {
    /// Main checkout -> what the terminal did there.
    #[serde(default)]
    pub repos: BTreeMap<String, RepoTrail>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoTrail {
    /// Branches the repo had when the terminal first went there.
    #[serde(default)]
    pub base_branches: BTreeSet<String>,
    /// Linked worktrees it had then.
    #[serde(default)]
    pub base_worktrees: BTreeSet<String>,
    /// Branches seen checked out in the terminal's folder.
    #[serde(default)]
    pub branches: BTreeSet<String>,
    /// Linked worktrees the terminal's folder was in.
    #[serde(default)]
    pub worktrees: BTreeSet<String>,
}

impl Runtime {
    fn book(&self, home: &Path) -> MutexGuard<'_, Book> {
        let mut b = self.book.lock().unwrap_or_else(|e| e.into_inner());
        if !b.loaded {
            *b = std::fs::read(file(home)).ok().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
            for r in b.runs.iter_mut().filter(|r| r.state == CleanupState::Running) {
                r.state = CleanupState::Failed;
                r.error = Some(LOST.into());
            }
            b.loaded = true;
        }
        b
    }

    fn repo_lock(&self, repo: &str) -> Arc<Mutex<()>> {
        self.repos.lock().unwrap_or_else(|e| e.into_inner()).entry(repo.to_string()).or_default().clone()
    }
}

fn file(home: &Path) -> PathBuf {
    home.join("cleanup.json")
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

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["git"];
    argv.extend_from_slice(args);
    run_cmd(&argv, cwd, GIT, None, &[]).map(|s| s.trim_end().to_string())
}

fn lines(s: Option<String>) -> BTreeSet<String> {
    s.unwrap_or_default().lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect()
}

/// (main checkout, linked worktrees) of the repo `cwd` is in.
fn worktree_list(cwd: &str) -> Option<(String, Vec<crate::worktrees::Entry>)> {
    let mut list = crate::worktrees::parse_list(&git(cwd, &["worktree", "list", "--porcelain"])?).into_iter();
    let main = list.next().filter(|e| !e.bare)?;
    Some((main.path, list.collect()))
}

// ------------------------------------------------------------------ while it lives

/// Note the repo, branch and worktree `sid`'s folder is in. Cheap after the first look at a repo.
pub fn observe(d: &Daemon, sid: &str, info: Option<&GitInfo>) {
    let Some(info) = info else { return };
    let Some(cwd) = d.core().state.session(sid).map(|s| s.cwd.clone()) else { return };
    let Some(top) = git(&cwd, &["rev-parse", "--path-format=absolute", "--show-toplevel"]) else { return };
    let known = {
        let book = d.cleanup.book(&d.cfg.home);
        book.trails.get(sid).and_then(|t| t.repos.iter().find(|(main, r)| **main == top || r.worktrees.contains(&top)).map(|(m, _)| m.clone()))
    };
    let (main, base) = match known {
        Some(main) => (main, None),
        None => {
            let Some((main, linked)) = worktree_list(&cwd) else { return };
            let branches = lines(git(&main, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]));
            (main, Some((branches, linked.into_iter().map(|e| e.path).collect::<BTreeSet<_>>())))
        }
    };
    let live: BTreeSet<Id> = d.core().state.sessions.iter().map(|s| s.id.clone()).collect();
    let mut book = d.cleanup.book(&d.cfg.home);
    book.trails.retain(|id, _| live.contains(id));
    let before = book.trails.get(sid).cloned();
    let r = book.trails.entry(sid.to_string()).or_default().repos.entry(main.clone()).or_default();
    if let Some((branches, worktrees)) = base {
        r.base_branches = branches;
        r.base_worktrees = worktrees;
    }
    if info.branch != "HEAD" {
        r.branches.insert(info.branch.clone());
    }
    if top != main {
        r.worktrees.insert(top);
    }
    if book.trails.get(sid) != before.as_ref() {
        save(&d.cfg.home, &book);
    }
}

// ------------------------------------------------------------------ at close

/// A closed terminal, as it was just before it closed.
pub struct Closing {
    session: Session,
    pids: BTreeSet<i32>,
    /// Their working folders, taken before they were killed.
    cwds: Vec<String>,
    trail: Trail,
}

/// Whether `s` is cleaned up after (`cleanup.enabled`, `cleanup.sessions`), or why not.
fn wanted(d: &Daemon, s: &Session) -> Result<(), String> {
    if !d.setting("cleanup.enabled").as_bool().unwrap_or(true) {
        return Err("cleanup.enabled is off".into());
    }
    let agents_only = d.setting("cleanup.sessions").as_str() != Some("all");
    if agents_only && s.kind != SessionKind::Agent && s.adopted.is_none() {
        return Err("it isn't an agent terminal (cleanup.sessions = agents)".into());
    }
    Ok(())
}

/// `sid`'s process tree and the folders its processes are in.
fn tree(d: &Daemon, sid: &str) -> (BTreeSet<i32>, Vec<String>) {
    let Some(root) = d.rt(sid).map(|rt| rt.pid) else { return Default::default() };
    let pids: BTreeSet<i32> = crate::procs::descendants(&crate::procs::table(), root).into_iter().collect();
    let cwds = pids.iter().filter_map(|&p| crate::procs::cwd(p)).collect();
    (pids, cwds)
}

/// Before `sid` is killed: what `after_close` needs, when it is to be cleaned up after.
pub fn capture(d: &Daemon, sid: &str, asked: Option<bool>) -> Option<Closing> {
    let session = d.core().state.session(sid).cloned()?;
    let trail = d.cleanup.book(&d.cfg.home).trails.remove(sid);
    if asked == Some(false) || wanted(d, &session).is_err() {
        return None;
    }
    let (pids, cwds) = tree(d, sid);
    Some(Closing { pids, cwds, session, trail: trail.unwrap_or_default() })
}

/// After it closed: wait for its processes to go, then clean up (on a thread of its own).
pub fn after_close(d: &Arc<Daemon>, c: Closing) {
    let w = Arc::downgrade(d);
    let _ = std::thread::Builder::new().name("cleanup".into()).spawn(move || {
        let start = Instant::now();
        while c.pids.iter().any(|&p| crate::procs::cwd(p).is_some()) && start.elapsed() < SETTLE {
            std::thread::sleep(Duration::from_millis(200));
        }
        let Some(d) = w.upgrade() else { return };
        let plan = plan(&d, &c.session, &c.trail, &c.pids, &c.cwds);
        if plan.would_run {
            run(&d, &c.session, plan, false, "session.close");
        }
    });
}

// ------------------------------------------------------------------ the plan

fn expand(p: &str) -> String {
    match p.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", std::env::var("HOME").unwrap_or_default()),
        _ => p.to_string(),
    }
}

/// A `cleanup.keep` pattern that covers a branch name or a folder.
pub fn kept_by<'a>(keep: &'a [String], branch: Option<&str>, path: Option<&str>) -> Option<&'a str> {
    keep.iter()
        .find(|k| {
            let k = k.trim();
            if k.starts_with('/') || k.starts_with('~') {
                path.is_some_and(|p| path_within(p, expand(k).trim_end_matches('/')))
            } else {
                branch.is_some_and(|b| crate::policy::glob_match(k, b))
            }
        })
        .map(String::as_str)
}

fn list_setting(d: &Daemon, key: &str) -> Vec<String> {
    d.setting(key).as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn default_branch(main: &str) -> Option<String> {
    git(main, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).map(|r| r.split_once('/').map(|(_, b)| b.to_string()).unwrap_or(r))
}

/// What cleaning up after `s` would do. `pids` and `cwds`: its process tree and their folders
/// (as they were, for a closed one).
fn plan(d: &Daemon, s: &Session, trail: &Trail, pids: &BTreeSet<i32>, cwds: &[String]) -> CleanupPlan {
    let items = list_setting(d, "cleanup.items");
    let keep = list_setting(d, "cleanup.keep");
    let (on, own) = cl::split_items(&items);
    let model = d.setting("cleanup.model").as_str().filter(|m| !m.trim().is_empty()).unwrap_or("haiku").to_string();
    let others: Vec<(Id, String)> = {
        let core = d.core();
        core.state.sessions.iter().filter(|o| o.id != s.id && core.rt.contains_key(&o.id)).map(|o| (o.id.clone(), o.cwd.clone())).collect()
    };
    let mut p = CleanupPlan { session_id: s.id.clone(), items: own.clone(), model, ..Default::default() };
    let mut targets: BTreeSet<CleanupTarget> = BTreeSet::new();
    let mut default = None;
    // The repo it was last in goes first (the model runs there).
    let mut repos: Vec<(&String, &RepoTrail)> = trail.repos.iter().collect();
    repos.sort_by_key(|(main, r)| !(path_within(&s.cwd, main) || r.worktrees.iter().any(|w| path_within(&s.cwd, w))));
    for (main, r) in repos {
        let Some((_, linked)) = worktree_list(main) else { continue };
        p.repo.get_or_insert_with(|| main.clone());
        let def = default_branch(main);
        default.get_or_insert(def.clone());
        let mut worktrees: Vec<&crate::worktrees::Entry> = vec![];
        for e in &linked {
            let mine = r.worktrees.contains(&e.path);
            let made = !r.base_worktrees.contains(&e.path)
                && (cwds.iter().any(|c| path_within(c, &e.path)) || e.locked.as_deref().and_then(sys_lock_pid).is_some_and(|pid| pids.contains(&pid)));
            if !(mine || made) {
                continue;
            }
            if let Some(k) = kept_by(&keep, Some(&e.branch), Some(&e.path)) {
                p.skipped.push(format!("worktree {}: cleanup.keep has {k}", e.path));
            } else if let Some((id, _)) = others.iter().find(|(_, c)| path_within(c, &e.path)) {
                p.skipped.push(format!("worktree {}: terminal {id} is in it", e.path));
            } else if !Path::new(&e.path).is_dir() {
                continue;
            } else if git(&e.path, &["status", "--porcelain"]).is_none_or(|st| !st.trim().is_empty()) {
                p.skipped.push(format!("worktree {}: it has uncommitted or untracked files", e.path));
            } else {
                worktrees.push(e);
            }
        }
        let gone: BTreeSet<&str> = worktrees.iter().map(|e| e.path.as_str()).collect();
        if on.contains(&cl::WORKTREE) {
            targets.extend(worktrees.iter().map(|e| CleanupTarget { kind: cl::WORKTREE.into(), name: e.path.clone() }));
        }
        // Branches: those of its worktrees, and new ones it had checked out.
        let mut branches: BTreeSet<String> = worktrees.iter().map(|e| e.branch.clone()).filter(|b| !b.is_empty()).collect();
        branches.extend(r.branches.iter().filter(|b| !r.base_branches.contains(*b)).cloned());
        let existing = lines(git(main, &["for-each-ref", "--format=%(refname:short)", "refs/heads"]));
        let main_branch = git(main, &["rev-parse", "--abbrev-ref", "HEAD"]);
        for b in branches.into_iter().filter(|b| existing.contains(b)) {
            let elsewhere = linked.iter().find(|e| e.branch == b && !gone.contains(e.path.as_str())).map(|e| e.path.clone()).or_else(|| (main_branch.as_deref() == Some(b.as_str())).then(|| main.clone()));
            if def.as_deref() == Some(b.as_str()) {
                p.skipped.push(format!("branch {b}: the default branch"));
            } else if let Some(k) = kept_by(&keep, Some(&b), None) {
                p.skipped.push(format!("branch {b}: cleanup.keep has {k}"));
            } else if let Some(at) = elsewhere {
                p.skipped.push(format!("branch {b}: checked out in {at}"));
            } else {
                if on.contains(&cl::BRANCH) {
                    targets.insert(CleanupTarget { kind: cl::BRANCH.into(), name: b.clone() });
                }
                if on.contains(&cl::REMOTE_BRANCH)
                    && let Some(up) = git(main, &["rev-parse", "--abbrev-ref", &format!("{b}@{{upstream}}")])
                {
                    let remote_name = up.split_once('/').map(|(_, n)| n).unwrap_or(&up);
                    if def.as_deref() == Some(remote_name) || kept_by(&keep, Some(remote_name), None).is_some() {
                        p.skipped.push(format!("remote branch {up}: kept like its name"));
                    } else {
                        targets.insert(CleanupTarget { kind: cl::REMOTE_BRANCH.into(), name: up });
                    }
                }
            }
        }
    }
    p.targets = targets.into_iter().collect();
    p.targets.sort_by_key(|t| cl::BUILTINS.iter().position(|b| *b == t.kind));
    p.reason = if let Err(why) = wanted(d, s) {
        Some(why)
    } else if p.targets.is_empty() && own.is_empty() {
        Some("it left nothing to clean up, and cleanup.items has none of your own".into())
    } else {
        None
    };
    p.would_run = p.reason.is_none();
    let cwd = if Path::new(&s.cwd).is_dir() { s.cwd.clone() } else { p.repo.clone().unwrap_or_default() };
    p.prompt = prompt(&s.name, p.repo.as_deref(), default.flatten().as_deref(), &cwd, &p.targets, &own, &keep, false);
    p
}

fn sys_lock_pid(reason: &str) -> Option<i32> {
    midna_proto::system::lock_pid(reason)
}

// ------------------------------------------------------------------ the model

/// What the model is told. Pure, so it can be tested and shown (`cleanup.preview`).
#[allow(clippy::too_many_arguments)]
pub fn prompt(name: &str, repo: Option<&str>, default: Option<&str>, cwd: &str, targets: &[CleanupTarget], own: &[String], keep: &[String], dry_run: bool) -> String {
    let mut s = format!("A terminal named “{name}” (folder {cwd}) just closed. Clean up what it left behind: the targets below and the user's own items, nothing else.\n");
    if dry_run {
        s.push_str("\nTHIS IS A DRY RUN. Change nothing: use only read-only commands, and say what you would do.\n");
    }
    if let Some(repo) = repo {
        s.push_str(&format!("\nRepo: {repo} (you are in its main checkout). Default branch: {}.\n", default.unwrap_or("unknown, treat main and master as default")));
    }
    if !targets.is_empty() {
        s.push_str("\nTargets:\n");
        for t in targets {
            s.push_str(&format!("- {} {}\n", t.kind, t.name));
        }
        s.push_str(
            "\nRules:\n\
             - Touch only the targets listed. Never touch the default branch",
        );
        if !keep.is_empty() {
            s.push_str(&format!(", or anything matching: {}", keep.join(", ")));
        }
        s.push_str(
            ".\n\
             - Never commit, reset, stash, rebase, check out, or push commits. Never use --force or -f.\n\
             - One plain command per Bash call, as written here: no `&&`, `;`, `|`, `echo`, `cd`, `git -C` or subshells (anything else is denied). You are already in the main checkout.\n\
             - midna already checked that each worktree target has no uncommitted or untracked files and that nothing runs in it.\n\
             - worktree: `git worktree remove <path>`. If it is locked by a pid that is no longer running, `git worktree unlock <path>` first. If git refuses, keep it.\n\
             - branch: `git branch -d <branch>`. If git says it isn't fully merged, use `git branch -D <branch>` only when `gh pr view <branch> --json state,headRefOid` says MERGED and headRefOid equals `git rev-parse <branch>` (a squash or rebase merge). Otherwise keep it and say why (not merged, N commits not pushed, no PR).\n\
             - remote_branch <remote>/<branch>: first `git fetch --prune <remote>`. Delete it with `git push --delete <remote> <branch>` only when its PR is MERGED, or `git merge-base --is-ancestor <remote>/<branch> <remote>/<default branch>` succeeds. Otherwise keep it and say why.\n\
             - If a target is already gone, say so as REMOVED.\n",
        );
    }
    if !own.is_empty() {
        s.push_str("\nThe user's own items. Do each one as written, for what this terminal worked on; skip one, saying why, when it doesn't apply or would need a tool you don't have:\n");
        for i in own {
            s.push_str(&format!("- {i}\n"));
        }
    }
    let (r, k) = if dry_run { ("WOULD REMOVE", "WOULD KEEP") } else { ("REMOVED", "KEPT") };
    s.push_str(&format!(
        "\nWhen you are done, answer with only these lines, one per target or item (none at all when there was nothing):\n{r}: <what, e.g. branch fix/login>\n{k}: <what> — <why>\n"
    ));
    s
}

/// Only `allowed_tools` may run, whatever the user's own Claude settings say: `--restricted`
/// ignores user, project and local settings (a `bypassPermissions` default mode, allow rules,
/// hooks) and `dontAsk` denies anything not allowed instead of asking nobody.
fn lockdown() -> Vec<String> {
    ["--restricted", "--permission-mode", "dontAsk", "--strict-mcp-config", "--tools", "Bash", "Read", "Glob", "Grep"].map(String::from).to_vec()
}

/// Tools the model may use. Reading git and gh always; changing git only for a real run.
fn allowed_tools(dry_run: bool, extra: &[String]) -> Vec<String> {
    let mut v: Vec<String> = [
        "git status", "git log", "git for-each-ref", "git rev-parse", "git rev-list", "git merge-base", "git cherry", "git ls-remote", "git show-ref",
        "git remote", "git config --get", "git diff", "git worktree list", "git branch --list", "git branch -vv", "git branch -r", "git branch -a",
        "git branch --merged", "git branch --no-merged", "gh pr view", "gh pr list", "ls", "pwd",
    ]
    .iter()
    .map(|c| format!("Bash({c}:*)"))
    .collect();
    if !dry_run {
        v.extend(["git worktree remove", "git worktree unlock", "git worktree prune", "git branch -d", "git branch -D", "git fetch", "git push --delete"].iter().map(|c| format!("Bash({c}:*)")));
        v.extend(extra.iter().cloned());
    }
    v.extend(["Read", "Glob", "Grep"].map(String::from));
    v
}

fn record(d: &Daemon, run: &CleanupRun) {
    let mut book = d.cleanup.book(&d.cfg.home);
    match book.runs.iter_mut().find(|r| r.id == run.id) {
        Some(r) => *r = run.clone(),
        None => book.runs.push(run.clone()),
    }
    let over = book.runs.len().saturating_sub(RUNS);
    book.runs.drain(..over);
    save(&d.cfg.home, &book);
}

/// Start a run for `plan` and return it as it starts; the model runs on a thread of its own.
fn run(d: &Arc<Daemon>, s: &Session, plan: CleanupPlan, dry_run: bool, by: &str) -> CleanupRun {
    let run = CleanupRun {
        id: format!("cu-{}", crate::state::hex_id(6)),
        session_id: s.id.clone(),
        session_name: s.name.clone(),
        project_id: s.project_id.clone(),
        repo: plan.repo.clone(),
        cwd: s.cwd.clone(),
        model: plan.model.clone(),
        dry_run,
        by: by.into(),
        started_at: time::now_rfc3339(),
        state: CleanupState::Running,
        targets: plan.targets.clone(),
        items: plan.items.clone(),
        ..Default::default()
    };
    record(d, &run);
    d.emit(kinds::CLEANUP_STARTED, Actor::system(), Some(s.project_id.clone()), Some(s.id.clone()), json!({ "run": run }));
    let prompt = if dry_run {
        let keep = list_setting(d, "cleanup.keep");
        let default = plan.repo.as_deref().and_then(default_branch);
        let cwd = if Path::new(&s.cwd).is_dir() { s.cwd.clone() } else { plan.repo.clone().unwrap_or_default() };
        prompt(&s.name, plan.repo.as_deref(), default.as_deref(), &cwd, &plan.targets, &plan.items, &keep, true)
    } else {
        plan.prompt.clone()
    };
    let (w, started) = (Arc::downgrade(d), run.clone());
    let _ = std::thread::Builder::new().name("cleanup-run".into()).spawn(move || {
        let Some(d) = w.upgrade() else { return };
        let lock = started.repo.as_deref().map(|r| d.cleanup.repo_lock(r));
        let _held = lock.as_ref().map(|l| l.lock().unwrap_or_else(|e| e.into_inner()));
        let mut run = started;
        let folder = run.repo.clone().filter(|r| Path::new(r).is_dir()).or_else(|| Path::new(&run.cwd).is_dir().then(|| run.cwd.clone())).unwrap_or_else(|| std::env::var("HOME").unwrap_or("/".into()));
        let timeout = Duration::from_secs(d.setting("cleanup.timeout_secs").as_u64().unwrap_or(300).clamp(30, 3600));
        let extra = list_setting(&d, "cleanup.tools");
        let mut argv: Vec<String> = ["claude", "-p", "--model", &run.model, "--output-format", "json", "--no-session-persistence"].map(String::from).to_vec();
        argv.extend(lockdown());
        argv.push("--allowedTools".into());
        argv.extend(allowed_tools(run.dry_run, &extra));
        match model(&d, &run.project_id, &folder, &argv, &prompt, timeout) {
            Ok((text, cost)) => {
                let (removed, kept) = cl::parse_report(&text);
                run.removed = removed;
                run.kept = kept;
                run.summary = text.chars().take(KEEP_SUMMARY).collect();
                run.cost_usd = cost;
                run.state = CleanupState::Done;
            }
            Err(e) => {
                run.error = Some(e);
                run.state = CleanupState::Failed;
            }
        }
        run.finished_at = Some(time::now_rfc3339());
        log(&format!("cleanup {} after {} ({}): {} removed, {} kept{}", run.id, run.session_id, run.session_name, run.removed.len(), run.kept.len(), run.error.as_deref().map(|e| format!(", failed: {e}")).unwrap_or_default()));
        record(&d, &run);
        d.emit(kinds::CLEANUP_FINISHED, Actor::system(), Some(run.project_id.clone()), Some(run.session_id.clone()), json!({ "run": run }));
    });
    run
}

/// Run the model in `folder`, prompt on stdin. Ok((its answer, cost)).
fn model(d: &Daemon, project_id: &str, folder: &str, argv: &[String], prompt: &str, timeout: Duration) -> Result<(String, Option<f64>), String> {
    let argv = crate::rpc::session::exec_argv(argv);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).current_dir(folder).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for k in crate::pty::ENV_SCRUB {
        cmd.env_remove(k);
    }
    for (k, v) in crate::rpc::session::session_env(d, "", project_id) {
        if !matches!(k.as_str(), "MIDNA_SESSION" | "COLORTERM" | "TERM") {
            cmd.env(&k, v);
        }
    }
    cmd.env_remove("MIDNA_SESSION").env("TERM", "dumb").env("MIDNA_CLEANUP", "1").env("GH_PROMPT_DISABLED", "1").env("GIT_TERMINAL_PROMPT", "0");
    unsafe {
        cmd.pre_exec(|| {
            // Its own process group, so a timeout stops everything it started.
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("could not start claude: {e}"))?;
    if let Some(mut w) = child.stdin.take() {
        let _ = w.write_all(prompt.as_bytes());
    }
    let reader = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = r.read_to_string(&mut s);
            s
        })
    };
    let out = reader(Box::new(child.stdout.take().ok_or("no stdout")?));
    let err = reader(Box::new(child.stderr.take().ok_or("no stderr")?));
    let pid = child.id() as i32;
    let began = Instant::now();
    let mut killed: Option<Instant> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        match killed {
            None if began.elapsed() >= timeout => {
                unsafe { libc::kill(-pid, libc::SIGTERM) };
                killed = Some(Instant::now());
            }
            Some(k) if k.elapsed() >= Duration::from_secs(5) => unsafe {
                libc::kill(-pid, libc::SIGKILL);
            },
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if killed.is_some() {
        return Err(format!("stopped after {}s (cleanup.timeout_secs)", timeout.as_secs()));
    }
    let v: Option<Value> = serde_json::from_str(out.trim()).ok();
    let mut text = v.as_ref().and_then(|v| v.get("result")).and_then(Value::as_str).map(str::to_string);
    // Commands it tried that weren't allowed, so `cleanup show` says why something was kept.
    let denied: Vec<String> = v.as_ref().and_then(|v| v.get("permission_denials")).and_then(Value::as_array).into_iter().flatten()
        .map(|x| x.pointer("/tool_input/command").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| x.get("tool_name").and_then(Value::as_str).unwrap_or("?").to_string()))
        .collect();
    if let Some(t) = text.as_mut().filter(|_| !denied.is_empty()) {
        t.push_str("\n");
        for c in &denied {
            t.push_str(&format!("\nDENIED: {}", c.lines().next().unwrap_or(c)));
        }
    }
    let cost = v.as_ref().and_then(|v| v.get("total_cost_usd")).and_then(Value::as_f64);
    let is_error = v.as_ref().and_then(|v| v.get("is_error")).and_then(Value::as_bool).unwrap_or(false);
    match (status.map(|s| s.success()), text) {
        (Some(true), Some(t)) if !is_error => Ok((t, cost)),
        (_, Some(t)) => Err(format!("claude said: {}", t.trim())),
        _ => {
            let tail: String = err.trim().lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            Err(if tail.is_empty() { format!("claude exited without an answer ({})", out.trim().chars().take(300).collect::<String>()) } else { tail })
        }
    }
}

/// The notification's text: None for a dry run or a run that did and kept nothing.
pub fn notification_text(run: &CleanupRun) -> Option<String> {
    if run.dry_run {
        return None;
    }
    let who = format!("‘{}’", run.session_name);
    if let Some(e) = &run.error {
        return Some(format!("Cleanup after {who} failed: {}", e.lines().next().unwrap_or(e)));
    }
    match (run.removed.is_empty(), run.kept.is_empty()) {
        (true, true) => None,
        (false, true) => Some(format!("Cleaned up after {who}: removed {}.", run.removed.join(", "))),
        (true, false) => Some(format!("Cleanup after {who} kept {}.", run.kept.join("; "))),
        (false, false) => Some(format!("Cleaned up after {who}: removed {}. Kept {}.", run.removed.join(", "), run.kept.join("; "))),
    }
}

// ------------------------------------------------------------------ rpc

pub fn runs(d: &Daemon, p: CleanupRunsParams) -> Result<Value, RpcError> {
    let book = d.cleanup.book(&d.cfg.home);
    let limit = p.limit.unwrap_or(20).max(1) as usize;
    let runs = book.runs.iter().rev().filter(|r| p.session_id.as_ref().is_none_or(|s| &r.session_id == s)).take(limit).cloned().collect();
    Ok(json!(CleanupRunsResult { runs }))
}

pub fn get(d: &Daemon, p: CleanupGetParams) -> Result<Value, RpcError> {
    let book = d.cleanup.book(&d.cfg.home);
    let run = book.runs.iter().find(|r| r.id == p.id).cloned().ok_or_else(|| RpcError::not_found(format!("no cleanup run {}", p.id)))?;
    Ok(json!(run))
}

fn live(d: &Daemon, sid: &str) -> Result<(Session, Trail), RpcError> {
    let s = d.core().state.session(sid).cloned().ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
    let trail = d.cleanup.book(&d.cfg.home).trails.get(sid).cloned().unwrap_or_default();
    Ok((s, trail))
}

pub fn preview(d: &Daemon, p: CleanupPreviewParams) -> Result<Value, RpcError> {
    let (s, trail) = live(d, &p.session_id)?;
    let (pids, cwds) = tree(d, &s.id);
    Ok(json!(plan(d, &s, &trail, &pids, &cwds)))
}

pub fn run_now(d: &Arc<Daemon>, p: CleanupRunParams) -> Result<Value, RpcError> {
    let (s, trail) = live(d, &p.session_id)?;
    let (pids, cwds) = tree(d, &s.id);
    let plan = plan(d, &s, &trail, &pids, &cwds);
    // force waives the settings (cleanup.enabled, cleanup.sessions), never an empty plan.
    let empty = plan.targets.is_empty() && plan.items.is_empty();
    if empty || (!plan.would_run && !p.force) {
        let why = if empty { "it left nothing to clean up, and cleanup.items has none of your own".into() } else { format!("{} (force runs it anyway)", plan.reason.clone().unwrap_or_default()) };
        return Err(RpcError::conflict(format!("not cleaning up after {}: {why}", s.id)));
    }
    Ok(json!(run(d, &s, plan, p.dry_run, "cleanup.run")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(kind: &str, name: &str) -> CleanupTarget {
        CleanupTarget { kind: kind.into(), name: name.into() }
    }

    #[test]
    fn keep_patterns() {
        let keep: Vec<String> = ["main", "release/*", "/r/keep"].map(String::from).to_vec();
        assert_eq!(kept_by(&keep, Some("release/1.2"), None), Some("release/*"));
        assert_eq!(kept_by(&keep, Some("fix/login"), None), None);
        assert_eq!(kept_by(&keep, Some("x"), Some("/r/keep/a")), Some("/r/keep"));
        assert_eq!(kept_by(&keep, Some("x"), Some("/r/keeper")), None);
    }

    #[test]
    fn prompt_lists_targets_rules_and_items() {
        let p = prompt("Fix login", Some("/r"), Some("main"), "/r/w", &[t("worktree", "/r/w"), t("branch", "fix/login")], &["stop the dev server".into()], &["main".into()], false);
        assert!(p.contains("- worktree /r/w\n- branch fix/login"));
        assert!(p.contains("Default branch: main"));
        assert!(p.contains("anything matching: main"));
        assert!(p.contains("- stop the dev server"));
        assert!(p.contains("REMOVED: <what"));
        assert!(!p.contains("DRY RUN"));
        let dry = prompt("x", None, None, "/tmp", &[], &["clear caches".into()], &[], true);
        assert!(dry.contains("DRY RUN") && dry.contains("WOULD REMOVE:") && !dry.contains("Rules:"));
    }

    #[test]
    fn dry_runs_get_no_changing_tools() {
        let dry = allowed_tools(true, &["Bash(docker compose down:*)".into()]);
        assert!(!dry.iter().any(|t| t.contains("remove") || t.contains("-D") || t.contains("--delete") || t.contains("docker")));
        let real = allowed_tools(false, &["Bash(docker compose down:*)".into()]);
        assert!(real.contains(&"Bash(git push --delete:*)".to_string()) && real.contains(&"Bash(docker compose down:*)".to_string()));
    }

    #[test]
    fn notification() {
        let mut r = CleanupRun { session_name: "Fix login".into(), removed: vec!["branch fix/login".into()], ..Default::default() };
        assert_eq!(notification_text(&r).as_deref(), Some("Cleaned up after ‘Fix login’: removed branch fix/login."));
        r.kept = vec!["branch wip — not merged".into()];
        assert!(notification_text(&r).unwrap().ends_with("Kept branch wip — not merged."));
        r.dry_run = true;
        assert_eq!(notification_text(&r), None);
    }
}
