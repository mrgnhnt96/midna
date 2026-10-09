//! Cleaning up after a terminal closes. midnad's `cleanup.rs` owns the behaviour; these are the
//! wire types and the pure parts the CLI and the app share.
//!
//! While a terminal lives, midnad notes the git branches and worktrees it works in. When it
//! closes (`cleanup.enabled`, `cleanup.sessions`), midnad works out what the terminal left
//! behind and hands that list, with the user's own items, to a cheap headless model
//! (`cleanup.model`) that removes what is safe to remove and says what it kept and why.
//!
//! `cleanup.items` is one ordered list: the built-ins below (a built-in left out is off), and
//! the user's own items, each a plain-English instruction ("stop the docker compose stack
//! started here"). `cleanup.keep` names branches and folders never touched.
use crate::types::{Id, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The linked git worktree the terminal worked in (or made), never forced.
pub const WORKTREE: &str = "worktree";
/// The local branches it made, once merged.
pub const BRANCH: &str = "branch";
/// Those branches' upstreams on the remote, once merged.
pub const REMOTE_BRANCH: &str = "remote_branch";
pub const BUILTINS: &[&str] = &[WORKTREE, BRANCH, REMOTE_BRANCH];

/// What a built-in is called in the UI.
pub fn builtin_label(item: &str) -> Option<&'static str> {
    match item {
        WORKTREE => Some("Git worktree"),
        BRANCH => Some("Local branch"),
        REMOTE_BRANCH => Some("Remote branch"),
        _ => None,
    }
}

/// A built-in's name from what a person might type ("worktrees", "local branch", "remote").
pub fn builtin(text: &str) -> Option<&'static str> {
    let t = text.trim().to_ascii_lowercase().replace([' ', '-'], "_");
    match t.trim_end_matches('s') {
        "worktree" | "git_worktree" => Some(WORKTREE),
        "branch" | "branche" | "local_branch" | "local_branche" => Some(BRANCH),
        "remote_branch" | "remote_branche" | "remote" => Some(REMOTE_BRANCH),
        _ => None,
    }
}

/// `cleanup.items` normalised: built-ins by their names, the user's own items trimmed, no
/// repeats, in order.
pub fn normalize_items<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for i in items.into_iter().map(str::trim).filter(|i| !i.is_empty()) {
        let i = builtin(i).map(str::to_string).unwrap_or_else(|| i.to_string());
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// (built-ins that are on, the user's own items).
pub fn split_items(items: &[String]) -> (Vec<&'static str>, Vec<String>) {
    let on = BUILTINS.iter().copied().filter(|b| items.iter().any(|i| i == b)).collect();
    let own = items.iter().filter(|i| builtin(i).is_none()).cloned().collect();
    (on, own)
}

/// `items` with built-in `b` turned on (put back among the built-ins, in their order) or off.
pub fn set_builtin(items: &[String], b: &str, on: bool) -> Vec<String> {
    let rank = |i: &str| BUILTINS.iter().position(|x| *x == i);
    let mut out: Vec<String> = items.iter().filter(|i| *i != b).cloned().collect();
    if on {
        let at = out.iter().position(|i| rank(i).is_none_or(|r| Some(r) > rank(b))).unwrap_or(out.len());
        out.insert(at, b.to_string());
    }
    out
}

/// `items` with `dragged` moved to `target`'s place.
pub fn moved(items: &[String], dragged: &str, target: &str) -> Vec<String> {
    let mut out = items.to_vec();
    let (Some(from), Some(to)) = (out.iter().position(|i| i == dragged), out.iter().position(|i| i == target)) else { return out };
    let item = out.remove(from);
    out.insert(to, item);
    out
}

/// One thing a run was asked to clean up.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct CleanupTarget {
    /// `worktree`, `branch` or `remote_branch`.
    pub kind: String,
    /// The worktree's path, the branch's name, or `remote/branch`.
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    #[default]
    Running,
    Done,
    Failed,
}

/// One cleanup: what it was asked to do, what the model did, and what it said.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CleanupRun {
    pub id: Id,
    pub session_id: Id,
    /// The terminal's name when it closed.
    pub session_name: String,
    pub project_id: Id,
    /// The repo's main checkout (where the model ran); None outside git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The terminal's folder when it closed.
    pub cwd: String,
    pub model: String,
    /// Asked to say what it would do, and to change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// `session.close` (it closed) or `cleanup.run` (someone asked).
    pub by: String,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    pub state: CleanupState,
    pub targets: Vec<CleanupTarget>,
    /// The user's own items it was given.
    #[serde(default)]
    pub items: Vec<String>,
    /// What it removed, one line each (its `REMOVED:` lines).
    #[serde(default)]
    pub removed: Vec<String>,
    /// What it left and why (its `KEPT:` lines).
    #[serde(default)]
    pub kept: Vec<String>,
    /// The model's whole answer, then a `DENIED: <command>` line for each command it tried that
    /// it wasn't allowed to run.
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What closing a terminal would clean up (`cleanup.preview`), without running anything.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CleanupPlan {
    pub session_id: Id,
    /// Whether closing it would start a run.
    pub would_run: bool,
    /// Why not, when it wouldn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub targets: Vec<CleanupTarget>,
    /// Branches and worktrees it saw but leaves alone (`cleanup.keep`, another terminal in it).
    #[serde(default)]
    pub skipped: Vec<String>,
    pub items: Vec<String>,
    pub model: String,
    /// The prompt the model would get.
    pub prompt: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CleanupRunsParams {
    /// Only this terminal's runs.
    #[serde(default)]
    pub session_id: Option<Id>,
    /// Newest first; 20 by default.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct CleanupRunsResult {
    pub runs: Vec<CleanupRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CleanupGetParams {
    pub id: Id,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CleanupPreviewParams {
    /// A live terminal.
    pub session_id: Id,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct CleanupRunParams {
    /// A live terminal: clean up what it has left so far (it stays open).
    pub session_id: Id,
    /// Have the model say what it would remove, and change nothing.
    #[serde(default)]
    pub dry_run: bool,
    /// Run even when `cleanup.enabled` is off or the terminal isn't in `cleanup.sessions`.
    #[serde(default)]
    pub force: bool,
}

/// The model's `REMOVED: …` and `KEPT: …` lines (any case, `-`/`*` bullets allowed).
pub fn parse_report(text: &str) -> (Vec<String>, Vec<String>) {
    let (mut removed, mut kept) = (vec![], vec![]);
    for line in text.lines() {
        let l = line.trim().trim_start_matches(['-', '*', ' ']).trim_matches('`');
        let Some((tag, rest)) = l.split_once(':') else { continue };
        let rest = rest.trim();
        if rest.is_empty() || rest.eq_ignore_ascii_case("nothing") || rest.eq_ignore_ascii_case("none") {
            continue;
        }
        match tag.trim().to_ascii_uppercase().as_str() {
            "REMOVED" | "WOULD REMOVE" => removed.push(rest.to_string()),
            "KEPT" | "WOULD KEEP" => kept.push(rest.to_string()),
            _ => {}
        }
    }
    (removed, kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_by_any_name() {
        assert_eq!(builtin("Worktrees"), Some(WORKTREE));
        assert_eq!(builtin("local branch"), Some(BRANCH));
        assert_eq!(builtin("branches"), Some(BRANCH));
        assert_eq!(builtin("remote-branches"), Some(REMOTE_BRANCH));
        assert_eq!(builtin("stop docker"), None);
    }

    #[test]
    fn items_normalised_and_split() {
        let v = normalize_items(["worktrees", " stop the dev server ", "branch", "worktree", "", "stop the dev server"]);
        assert_eq!(v, vec!["worktree", "stop the dev server", "branch"]);
        let (on, own) = split_items(&v);
        assert_eq!(on, vec![WORKTREE, BRANCH]);
        assert_eq!(own, vec!["stop the dev server"]);
    }

    #[test]
    fn builtins_toggle_in_place() {
        let v: Vec<String> = ["worktree", "remote_branch", "stop docker"].map(String::from).to_vec();
        assert_eq!(set_builtin(&v, BRANCH, true), vec!["worktree", "branch", "remote_branch", "stop docker"]);
        assert_eq!(set_builtin(&v, WORKTREE, false), vec!["remote_branch", "stop docker"]);
        assert_eq!(moved(&v, "stop docker", "worktree"), vec!["stop docker", "worktree", "remote_branch"]);
    }

    #[test]
    fn report_lines() {
        let text = "Done.\nREMOVED: worktree /r/.claude/worktrees/a\n- removed: branch fix/login\nKEPT: branch wip — 2 unpushed commits\nKEPT: nothing\nnotes: fine";
        let (r, k) = parse_report(text);
        assert_eq!(r, vec!["worktree /r/.claude/worktrees/a", "branch fix/login"]);
        assert_eq!(k, vec!["branch wip — 2 unpushed commits"]);
    }
}
