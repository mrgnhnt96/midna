//! project.* handlers.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use crate::state::hex_id;
use midna_proto::*;
use serde_json::json;
use std::sync::Arc;

pub fn list(d: &Daemon) -> R {
    let mut v = d.core().state.projects.clone();
    v.sort_by_key(|p| p.order);
    ok(v)
}

fn normalize(path: &str) -> String {
    let p = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string());
    if p.len() > 1 { p.trim_end_matches('/').to_string() } else { p }
}

/// Find the project containing `path` (deepest match) or add one for it.
pub fn find_or_add(d: &Daemon, actor: Actor, path: &str, name: Option<String>) -> Result<Project, RpcError> {
    let path = normalize(path);
    if !std::path::Path::new(&path).is_dir() {
        return Err(RpcError::bad_params(format!("{path} is not a directory")));
    }
    let mut core = d.core();
    if let Some(p) = core.state.projects.iter().find(|p| p.path == path) {
        return Ok(p.clone());
    }
    let name = name.unwrap_or_else(|| path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("root").to_string());
    let order = core.state.projects.iter().map(|p| p.order + 1).max().unwrap_or(0);
    let p = Project { id: format!("p_{}", hex_id(6)), name, path, icon: None, order, commands: vec![], last_opened_at: None };
    core.state.projects.push(p.clone());
    d.mark_dirty();
    d.emit(kinds::PROJECT_ADDED, actor, Some(p.id.clone()), None, serde_json::to_value(&p).unwrap_or_default());
    Ok(p)
}

/// The project whose path contains `path` (deepest wins).
pub fn containing(d: &Daemon, path: &str) -> Option<Project> {
    let path = normalize(path);
    d.core()
        .state
        .projects
        .iter()
        .filter(|p| path == p.path || path.starts_with(&format!("{}/", p.path)) || p.path == "/")
        .max_by_key(|p| p.path.len())
        .cloned()
}

/// Most folders `project.discover` returns.
const DISCOVER_MAX: usize = 500;

pub fn discover(d: &Daemon) -> R {
    let (roots, known): (Vec<String>, Vec<(String, Id)>) = {
        let core = d.core();
        let roots = core.state.setting("projects.roots").as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
        (roots, core.state.projects.iter().map(|p| (p.path.clone(), p.id.clone())).collect())
    };
    ok(candidates(&roots, &super::session::home_dir(), &known))
}

/// Folders under `roots` (see `project.discover`), most recently modified first.
pub fn candidates(roots: &[String], home: &str, known: &[(String, Id)]) -> Vec<ProjectCandidate> {
    use std::path::Path;
    let subdirs = |dir: &Path| -> Vec<std::path::PathBuf> {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return vec![];
        };
        rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.') && e.path().is_dir()).map(|e| e.path()).collect()
    };
    let git = |p: &Path| p.join(".git").exists();
    let mut out: Vec<(i64, ProjectCandidate)> = vec![];
    for root in roots {
        let abs = match root.strip_prefix('~') {
            Some(rest) => format!("{home}{rest}"),
            None => root.clone(),
        };
        for dir in subdirs(Path::new(&normalize(&abs))) {
            let inner: Vec<_> = if git(&dir) { vec![] } else { subdirs(&dir).into_iter().filter(|p| git(p)).collect() };
            for p in if inner.is_empty() { vec![dir] } else { inner } {
                let path = p.to_string_lossy().into_owned();
                if out.iter().any(|(_, c)| c.path == path) {
                    continue;
                }
                let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
                out.push((
                    mtime.unwrap_or(0),
                    ProjectCandidate {
                        name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        project_id: known.iter().find(|(kp, _)| *kp == path).map(|(_, id)| id.clone()),
                        git: git(&p),
                        root: root.clone(),
                        modified_at: mtime.map(time::format_unix),
                        path,
                    },
                ));
            }
        }
    }
    out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    out.into_iter().take(DISCOVER_MAX).map(|(_, c)| c).collect()
}

pub fn add(d: &Daemon, ctx: &Ctx, p: ProjectAddParams) -> R {
    ok(find_or_add(d, ctx.actor(), &p.path, p.name)?)
}

pub fn update(d: &Daemon, ctx: &Ctx, p: ProjectUpdateParams) -> R {
    let mut core = d.core();
    let proj = core.state.projects.iter_mut().find(|x| x.id == p.id).ok_or_else(|| RpcError::not_found(format!("no project {}", p.id)))?;
    if let Some(n) = p.name {
        proj.name = n;
    }
    if let Some(i) = p.icon {
        proj.icon = (!i.is_empty()).then_some(i);
    }
    if let Some(c) = p.commands {
        proj.commands = c;
    }
    let out = proj.clone();
    d.mark_dirty();
    d.emit(kinds::PROJECT_UPDATED, ctx.actor(), Some(out.id.clone()), None, serde_json::to_value(&out).unwrap_or_default());
    ok(out)
}

pub fn remove(d: &Arc<Daemon>, ctx: &Ctx, p: IdParams) -> R {
    let sessions: Vec<Id> = {
        let core = d.core();
        core.state.project(&p.id).ok_or_else(|| RpcError::not_found(format!("no project {}", p.id)))?;
        core.state.sessions.iter().filter(|s| s.project_id == p.id).map(|s| s.id.clone()).collect()
    };
    for sid in sessions {
        super::session::close_inner(d, ctx, &sid, true);
    }
    d.core().state.projects.retain(|x| x.id != p.id);
    d.mark_dirty();
    d.emit(kinds::PROJECT_REMOVED, ctx.actor(), Some(p.id.clone()), None, json!({ "id": p.id }));
    ok(OkResult { ok: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_look_one_level_into_non_repo_folders() {
        let tmp = std::env::temp_dir().join(format!("midna-discover-{}", hex_id(6)));
        let home = tmp.to_string_lossy().into_owned();
        for d in ["dev/kass/.git", "dev/rust/midna/.git", "dev/rust/notes", "dev/scratch", "dev/.hidden", "work/api/.git"] {
            std::fs::create_dir_all(tmp.join(d)).unwrap();
        }
        let roots = vec!["~/dev".to_string(), format!("{home}/work")];
        let known = vec![(normalize(&format!("{home}/work/api")), "p_api".to_string())];
        let mut got = candidates(&roots, &home, &known);
        got.sort_by(|a, b| a.path.cmp(&b.path));
        let names: Vec<(&str, bool)> = got.iter().map(|c| (c.name.as_str(), c.git)).collect();
        // `rust` holds a repo, so it is replaced by `midna`; `scratch` has none, so it stays.
        assert_eq!(names, [("kass", true), ("midna", true), ("scratch", false), ("api", true)]);
        assert_eq!(got[0].root, "~/dev");
        assert_eq!(got[3].project_id.as_deref(), Some("p_api"));
        assert!(candidates(&["~/missing".into()], &home, &[]).is_empty());
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
