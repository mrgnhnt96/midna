//! Projects' Summaries (`summary.*`): a record of each terminal that closes in a project, kept
//! for `summary.keep_days` (and at most `summary::MAX_PER_PROJECT` per project), with the
//! panels apps put on them (`summary.annotate`).
//!
//! The record is written in `rpc::session::close_inner`, before the terminal's links are
//! forgotten. Terminals that aren't closed for good aren't recorded: a ⌘R replace, a reset, a
//! removed project, a background terminal, one at root (no project to show it in).
//!
//! An app may annotate a terminal while it runs: the panel waits in `live` and moves onto the
//! record when it closes. Everything lives in `$MIDNA_HOME/summary.json`.
use crate::daemon::Daemon;
use crate::rpc::{Ctx, R, ok, parse};
use midna_proto::summary::{MAX_LINKS, MAX_PER_PROJECT, MAX_PROMPTS};
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How a close is described in `session.closed`'s `how` when the terminal is gone for good.
const RECORDED: &[&str] = &["close", "force_close", "exited"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Data {
    /// Newest first.
    #[serde(default)]
    closed: Vec<ClosedTerminal>,
    /// Panels on running terminals, by terminal then owner.
    #[serde(default)]
    live: BTreeMap<Id, BTreeMap<String, SummaryPanel>>,
}

/// The records, loaded on first use.
#[derive(Default)]
pub struct Summary {
    data: Mutex<Option<Data>>,
}

fn file(home: &Path) -> PathBuf {
    home.join("summary.json")
}

impl Summary {
    /// Run `f` on the records (loading them first), saving when `f` says they changed. Lock
    /// `core` before this, never inside it.
    fn with<T>(&self, home: &Path, f: impl FnOnce(&mut Data) -> (T, bool)) -> T {
        let mut g = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let data = g.get_or_insert_with(|| std::fs::read(file(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default());
        let (out, changed) = f(data);
        if changed {
            save(home, data);
        }
        out
    }
}

fn save(home: &Path, data: &Data) {
    let path = file(home);
    let tmp = path.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec(data)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Drop records past `keep_days` and past the per-project cap, and panels on terminals that
/// are neither running nor recorded. True when something went.
fn prune(data: &mut Data, keep_days: i64, now: i64, running: &[Id]) -> bool {
    let before = (data.closed.len(), data.live.len());
    let cutoff = now - keep_days.max(1) * 86_400;
    data.closed.retain(|c| time::parse_rfc3339(&c.closed_at).is_none_or(|t| t >= cutoff));
    let mut per: BTreeMap<Id, usize> = BTreeMap::new();
    data.closed.retain(|c| {
        let n = per.entry(c.project_id.clone()).or_default();
        *n += 1;
        *n <= MAX_PER_PROJECT
    });
    data.live.retain(|id, _| running.contains(id));
    before != (data.closed.len(), data.live.len())
}

fn keep_days(d: &Daemon) -> i64 {
    d.core().state.setting_i64("summary.keep_days")
}

fn running(d: &Daemon) -> Vec<Id> {
    d.core().state.sessions.iter().map(|s| s.id.clone()).collect()
}

/// What `close_inner` records about a terminal that's closing: worked out before it goes (its
/// prompts and links are read from what it leaves behind). None for closes that aren't recorded.
pub fn capture(d: &Daemon, sid: &str, how: &str, by: Actor) -> Option<ClosedTerminal> {
    if !RECORDED.contains(&how) {
        return None;
    }
    let s = d.core().state.session(sid).cloned()?;
    if s.background || s.project_id == ROOT_PROJECT_ID {
        return None;
    }
    let marks = crate::prompts::list(d, sid);
    let turns = marks.len() as u32;
    let mut links = d.links.list(&d.cfg.home, sid);
    links.truncate(MAX_LINKS);
    let filter = EventFilter { kinds: Some(vec![kinds::AGENT_COST.into()]), session_id: Some(sid.into()), project_id: None };
    let cost_usd = d.log.list(0, 1, &filter).first().and_then(|e| e.data.get("total_usd")).and_then(|v| v.as_f64()).filter(|c| *c > 0.);
    let info = s.agent_info.as_ref();
    Some(ClosedTerminal {
        id: s.id.clone(),
        project_id: s.project_id.clone(),
        name: s.name.clone(),
        kind: s.kind,
        agent: s.agent,
        cwd: info.and_then(|a| a.cwd.clone()).unwrap_or_else(|| s.cwd.clone()),
        conversation_id: info.and_then(|a| a.conversation_id.clone()),
        agent_args: s.agent_args.clone(),
        worktree: s.git.as_ref().and_then(|g| g.worktree.clone()),
        branch: s.git.as_ref().map(|g| g.branch.clone()).filter(|b| !b.is_empty()),
        opened_at: s.created_at.clone(),
        closed_at: time::now_rfc3339(),
        how: how.to_string(),
        by,
        state: s.status.state,
        exit_code: s.status.exit_code,
        reason: s.status.reason.clone().filter(|r| !r.is_empty()),
        prompts: marks.into_iter().take(MAX_PROMPTS).collect(),
        turns,
        cost_usd,
        links,
        apps: BTreeMap::new(),
    })
}

/// The terminal closed: keep its record, with the panels apps put on it while it ran.
pub fn record(d: &Daemon, mut rec: ClosedTerminal) {
    let (days, now, running) = (keep_days(d), time::now_unix(), running(d));
    let (id, project) = (rec.id.clone(), rec.project_id.clone());
    d.summary.with(&d.cfg.home, |data| {
        rec.apps = data.live.remove(&rec.id).unwrap_or_default();
        data.closed.retain(|c| c.id != rec.id);
        data.closed.insert(0, rec);
        prune(data, days, now, &running);
        ((), true)
    });
    changed(d, Actor::system(), project, Some(id), "added");
}

fn changed(d: &Daemon, by: Actor, project: Id, id: Option<Id>, action: &str) {
    d.emit(kinds::SUMMARY_CHANGED, by, Some(project), None, json!({ "id": id, "action": action }));
}

/// `summary.list`.
pub fn list(d: &Daemon, p: SummaryListParams) -> R {
    let project = p.project_id.as_deref().map(|id| crate::rpc::project::resolve(d, id)).transpose()?.map(|p| p.id);
    let (days, now, running) = (keep_days(d), time::now_unix(), running(d));
    let closed = d.summary.with(&d.cfg.home, |data| {
        let pruned = prune(data, days, now, &running);
        let out: Vec<ClosedTerminal> = data.closed.iter().filter(|c| project.as_ref().is_none_or(|p| &c.project_id == p)).cloned().collect();
        (out, pruned)
    });
    ok(SummaryListResult { closed, keep_days: days as u32 })
}

/// `summary.remove`: one record, or a project's whole Summary.
pub fn remove(d: &Daemon, ctx: &Ctx, p: SummaryRemoveParams) -> R {
    let project = p.project_id.as_deref().map(|id| crate::rpc::project::resolve(d, id)).transpose()?.map(|p| p.id);
    let (id, project) = match (p.id, project) {
        (Some(id), project) => (Some(id), project),
        (None, Some(project)) => (None, Some(project)),
        (None, None) => return Err(RpcError::bad_params("summary.remove needs an id or a project_id")),
    };
    let gone: Vec<(Id, Id)> = d.summary.with(&d.cfg.home, |data| {
        let hit = |c: &ClosedTerminal| id.as_ref().is_none_or(|i| &c.id == i) && project.as_ref().is_none_or(|p| &c.project_id == p);
        let gone: Vec<(Id, Id)> = data.closed.iter().filter(|c| hit(c)).map(|c| (c.id.clone(), c.project_id.clone())).collect();
        data.closed.retain(|c| !hit(c));
        let any = !gone.is_empty();
        (gone, any)
    });
    if let Some(i) = id.as_ref().filter(|_| gone.is_empty()) {
        return Err(RpcError::not_found(format!("no closed terminal {i} in a Summary")));
    }
    let mut projects: Vec<Id> = gone.iter().map(|(_, p)| p.clone()).collect();
    projects.dedup();
    for p in projects {
        changed(d, ctx.actor(), p, id.clone(), "removed");
    }
    ok(SummaryRemoveResult { removed: gone.len() as u32 })
}

/// `summary.annotate`: an app's panel on a running or closed terminal.
pub fn annotate(d: &Daemon, ctx: &Ctx, p: SummaryAnnotateParams) -> R {
    let owner = p.owner.trim().to_string();
    if owner.is_empty() || owner.len() > 40 {
        return Err(RpcError::bad_params("owner must be 1–40 characters (your app's name)"));
    }
    if let Some(panel) = &p.panel {
        midna_proto::summary::check_panel(panel).map_err(RpcError::bad_params)?;
    }
    let live = d.core().state.session(&p.id).map(|s| s.project_id.clone());
    let project = d.summary.with(&d.cfg.home, |data| {
        if live.is_some() {
            let panels = data.live.entry(p.id.clone()).or_default();
            match p.panel.clone() {
                Some(panel) => panels.insert(owner.clone(), panel),
                None => panels.remove(&owner),
            };
            if panels.is_empty() {
                data.live.remove(&p.id);
            }
            return (live.clone(), true);
        }
        let Some(rec) = data.closed.iter_mut().find(|c| c.id == p.id) else { return (None, false) };
        match p.panel.clone() {
            Some(panel) => rec.apps.insert(owner.clone(), panel),
            None => rec.apps.remove(&owner),
        };
        (Some(rec.project_id.clone()), true)
    });
    let Some(project) = project else { return Err(RpcError::not_found(format!("no terminal {} (running or in a Summary)", p.id))) };
    if live.is_none() {
        changed(d, ctx.actor(), project, Some(p.id.clone()), "annotated");
    }
    ok(SummaryAnnotateResult { running: live.is_some() })
}

/// `summary.resume`: open the closed terminal again, in its conversation, and take it off the Summary.
pub fn resume(d: &Arc<Daemon>, ctx: &Ctx, p: IdParams) -> R {
    let rec = d.summary.with(&d.cfg.home, |data| (data.closed.iter().find(|c| c.id == p.id).cloned(), false));
    let rec = rec.ok_or_else(|| RpcError::not_found(format!("no closed terminal {} in a Summary", p.id)))?;
    // A worktree that's gone since: open in the project instead.
    let cwd = Path::new(&rec.cwd).is_dir().then(|| rec.cwd.clone());
    let mut open = json!({ "project_id": rec.project_id, "kind": rec.kind, "name": rec.name, "cwd": cwd });
    if rec.kind == SessionKind::Agent {
        open["agent"] = json!(rec.agent);
        open["agent_args"] = json!(rec.agent_args);
        open["resume"] = json!(rec.conversation_id);
    }
    let session: Session = serde_json::from_value(crate::rpc::session::open(d, ctx, parse(open)?)?).map_err(|e| RpcError::internal(e.to_string()))?;
    d.summary.with(&d.cfg.home, |data| {
        data.closed.retain(|c| c.id != rec.id);
        ((), true)
    });
    changed(d, ctx.actor(), rec.project_id.clone(), Some(rec.id.clone()), "resumed");
    ok(SummaryResumeResult { session })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, project: &str, closed_at: i64) -> ClosedTerminal {
        ClosedTerminal {
            id: id.into(),
            project_id: project.into(),
            name: id.into(),
            kind: SessionKind::Shell,
            agent: None,
            cwd: "/tmp".into(),
            conversation_id: None,
            agent_args: vec![],
            worktree: None,
            branch: None,
            opened_at: time::format_unix(closed_at - 60),
            closed_at: time::format_unix(closed_at),
            how: "close".into(),
            by: Actor::human(),
            state: StatusState::Idle,
            exit_code: None,
            reason: None,
            prompts: vec![],
            turns: 0,
            cost_usd: None,
            links: vec![],
            apps: BTreeMap::new(),
        }
    }

    #[test]
    fn prune_drops_old_records_and_caps_each_project() {
        let now = 1_800_000_000;
        let mut data = Data::default();
        data.closed.push(rec("new", "p1", now - 60));
        data.closed.push(rec("old", "p1", now - 8 * 86_400));
        for i in 0..MAX_PER_PROJECT + 2 {
            data.closed.push(rec(&format!("p2-{i}"), "p2", now - 120 - i as i64));
        }
        assert!(prune(&mut data, 7, now, &[]));
        assert!(data.closed.iter().any(|c| c.id == "new"));
        assert!(!data.closed.iter().any(|c| c.id == "old"), "past keep_days");
        assert_eq!(data.closed.iter().filter(|c| c.project_id == "p2").count(), MAX_PER_PROJECT);
        assert!(data.closed.iter().any(|c| c.id == "p2-0"), "the newest stay");
    }

    #[test]
    fn prune_drops_panels_on_terminals_that_are_gone() {
        let mut data = Data::default();
        data.live.insert("s1".into(), BTreeMap::from([("taskboard".into(), SummaryPanel { title: "T1".into(), ..Default::default() })]));
        data.live.insert("s2".into(), BTreeMap::new());
        prune(&mut data, 7, 1_800_000_000, &["s1".into()]);
        assert!(data.live.contains_key("s1"));
        assert!(!data.live.contains_key("s2"));
    }
}
