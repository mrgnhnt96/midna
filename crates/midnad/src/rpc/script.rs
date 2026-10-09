//! script.run: header/row/status scripts. Built-in parts joined with `+` (`worktree+branch`,
//! `github+agent`; see `settings::SCRIPT_PARTS`), or an executable path that prints JSON
//! segments (an array, or `{"segments": [...]}`).
use super::{R, ok};
use crate::daemon::Daemon;
use crate::git;
use midna_proto::*;
use serde_json::Value;
use std::time::Duration;

const CUSTOM_TIMEOUT: Duration = Duration::from_secs(5);

pub fn run(d: &Daemon, p: ScriptRunParams) -> R {
    let (session, script) = {
        let core = d.core();
        let s = core.state.session(&p.session_id).cloned().ok_or_else(|| RpcError::not_found(format!("no session {}", p.session_id)))?;
        let key = match p.slot {
            ScriptSlot::Header => "ui.header.script",
            ScriptSlot::Row => "ui.row.script",
            ScriptSlot::Status => "ui.status.script",
            ScriptSlot::Button => "",
        };
        let script = match (&p.script, p.slot) {
            (None, ScriptSlot::Button) => return Err(RpcError::bad_params("slot button needs `script`")),
            (None, _) => core.state.setting_str(key),
            (Some(path), ScriptSlot::Status) => listed(&core.state, "ui.status.items", path)?,
            (Some(path), ScriptSlot::Button) => listed(&core.state, "ui.header.buttons", path)?,
            (Some(_), _) => return Err(RpcError::bad_params("`script` goes with slot status or button")),
        };
        (s, script)
    };
    let segments = if midna_proto::settings::is_builtin_script(&script) { built_in(&script, &session) } else { custom(&script, &session, p.slot, false) };
    ok(ScriptRunResult { segments })
}

/// script.click: a custom header button was clicked. Runs its script with MIDNA_CLICK=1; what
/// it prints is the button's new look (empty output keeps the old one: the app refetches).
pub fn click(d: &Daemon, p: ScriptClickParams) -> R {
    let (session, script) = {
        let core = d.core();
        let s = core.state.session(&p.session_id).cloned().ok_or_else(|| RpcError::not_found(format!("no session {}", p.session_id)))?;
        (s, listed(&core.state, "ui.header.buttons", &p.script)?)
    };
    ok(ScriptRunResult { segments: custom(&script, &session, ScriptSlot::Button, true) })
}

/// Only a path the human put in that list: script.run/click never run an arbitrary one.
fn listed(state: &crate::state::State, key: &str, path: &str) -> Result<String, RpcError> {
    let ok = state.setting(key).as_array().is_some_and(|a| a.iter().any(|i| i.as_str() == Some(path)));
    if ok { Ok(path.to_string()) } else { Err(RpcError::bad_params(format!("{path} is not a script in {key}"))) }
}

/// Built-in parts in the order given; git is read once, and only when a part needs it.
fn built_in(script: &str, s: &Session) -> Vec<Segment> {
    let parts: Vec<&str> = script.split('+').map(str::trim).filter(|p| !p.is_empty() && *p != "none").collect();
    let g = parts.iter().any(|p| *p != "agent").then(|| git::git_info(&git::work_cwd(s, None))).flatten();
    let mut v = vec![];
    for part in parts {
        match (part, &g) {
            ("agent", _) => v.extend(agent(s)),
            ("github", Some(g)) => {
                for p in ["worktree", "branch", "sync", "diff", "files", "pr"] {
                    v.extend(git_part(p, g));
                }
            }
            (p, Some(g)) => v.extend(git_part(p, g)),
            (_, None) => {}
        }
    }
    v
}

fn git_part(part: &str, g: &GitInfo) -> Vec<Segment> {
    match part {
        // icon only; the name shows on hover
        "worktree" => g.worktree.iter().map(|w| Segment { tooltip: Some(w.clone()), ..Segment::new("", Some(Tone::Work)).icon("worktree") }).collect(),
        "branch" => vec![Segment::new(g.branch.clone(), Some(Tone::Accent)).icon("branch")],
        "sync" if g.ahead > 0 || g.behind > 0 => vec![Segment::new(format!("↑{} ↓{}", g.ahead, g.behind), Some(Tone::Dim))],
        "diff" | "git-diff-stats" => diff_stats(Some(g)),
        "files" if g.files > 0 => vec![Segment::new(format!("{} file{}", g.files, if g.files == 1 { "" } else { "s" }), Some(Tone::Dim))],
        "pr" => pr(g),
        _ => vec![],
    }
}

fn diff_stats(g: Option<&GitInfo>) -> Vec<Segment> {
    let Some(g) = g else { return vec![] };
    if g.added == 0 && g.removed == 0 {
        return vec![];
    }
    vec![Segment::new(format!("+{}", g.added), Some(Tone::Ok)), Segment::new(format!("−{}", g.removed), Some(Tone::Err))]
}

fn pr(g: &GitInfo) -> Vec<Segment> {
    let mut v = vec![];
    if let Some(pr) = &g.pr {
        v.push(Segment { link: Some(pr.url.clone()), ..Segment::new(format!("#{}", pr.number), Some(Tone::Accent)).icon("pr") });
        match pr.checks {
            ChecksState::Passing => v.push(Segment::new("checks passing", Some(Tone::Ok)).icon("dot")),
            ChecksState::Failing => v.push(Segment::new(format!("{} failing", pr.failing_count), Some(Tone::Err)).icon("dot")),
            ChecksState::Pending => v.push(Segment::new("checks running", Some(Tone::Work)).icon("dot")),
            ChecksState::None => {}
        }
    }
    v
}

fn agent(s: &Session) -> Vec<Segment> {
    let Some(a) = s.agent else { return vec![] };
    let tone = match s.status.state {
        StatusState::Working => Tone::Work,
        StatusState::NeedsYou => Tone::Need,
        StatusState::Failed => Tone::Err,
        StatusState::Done => Tone::Ok,
        _ => Tone::Dim,
    };
    vec![Segment::new(format!("{} {}", a.as_str(), s.status.state.as_str().replace('_', " ")), Some(tone))]
}

fn custom(path: &str, s: &Session, slot: ScriptSlot, click: bool) -> Vec<Segment> {
    let err = |m: String| vec![Segment::new(m, Some(Tone::Err))];
    if !path.starts_with('/') {
        return err(format!("script path must be absolute: {path}"));
    }
    if !std::path::Path::new(path).is_file() {
        return err(format!("script not found: {path}"));
    }
    let env = vec![
        ("MIDNA_SESSION".to_string(), s.id.clone()),
        ("MIDNA_PROJECT".to_string(), s.project_id.clone()),
        ("MIDNA_SLOT".to_string(), format!("{slot:?}").to_lowercase()),
    ];
    let env = if click { env.into_iter().chain([("MIDNA_CLICK".to_string(), "1".to_string())]).collect() } else { env };
    let input = serde_json::to_vec(s).unwrap_or_default();
    let Some(out) = git::run_cmd(&[path], &s.cwd, CUSTOM_TIMEOUT, Some(&input), &env) else {
        return err(format!("script failed or timed out: {path}"));
    };
    let v: Value = match serde_json::from_str(out.trim()) {
        Ok(v) => v,
        Err(e) => return err(format!("script output is not JSON: {e}")),
    };
    let arr = v.get("segments").cloned().unwrap_or(v);
    serde_json::from_value(arr).unwrap_or_else(|e| err(format!("bad segments: {e}")))
}
