//! script.run: header/row scripts. Built-ins `github`, `github+agent`, `git-diff-stats`, `none`,
//! or an executable path that prints JSON segments (an array, or `{"segments": [...]}`).
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
        };
        (s, core.state.setting_str(key))
    };
    let segments = match script.as_str() {
        "none" | "" => vec![],
        "github" => github(&session),
        "github+agent" => {
            let mut v = github(&session);
            v.extend(agent(&session));
            v
        }
        "git-diff-stats" => diff_stats(git::git_info(&session.cwd).as_ref()),
        path => custom(path, &session, p.slot),
    };
    ok(ScriptRunResult { segments })
}

fn diff_stats(g: Option<&GitInfo>) -> Vec<Segment> {
    let Some(g) = g else { return vec![] };
    if g.added == 0 && g.removed == 0 {
        return vec![];
    }
    vec![Segment::new(format!("+{}", g.added), Some(Tone::Ok)), Segment::new(format!("−{}", g.removed), Some(Tone::Err))]
}

fn github(s: &Session) -> Vec<Segment> {
    let Some(g) = git::git_info(&s.cwd) else { return vec![] };
    let mut v = vec![Segment::new(g.branch.clone(), Some(Tone::Accent))];
    if g.ahead > 0 || g.behind > 0 {
        v.push(Segment::new(format!("↑{} ↓{}", g.ahead, g.behind), Some(Tone::Dim)));
    }
    v.extend(diff_stats(Some(&g)));
    if g.files > 0 {
        v.push(Segment::new(format!("{} file{}", g.files, if g.files == 1 { "" } else { "s" }), Some(Tone::Dim)));
    }
    if let Some(pr) = &g.pr {
        v.push(Segment { text: format!("#{}", pr.number), tone: Some(Tone::Accent), link: Some(pr.url.clone()) });
        match pr.checks {
            ChecksState::Passing => v.push(Segment::new("checks passing", Some(Tone::Ok))),
            ChecksState::Failing => v.push(Segment::new(format!("{} failing", pr.failing_count), Some(Tone::Err))),
            ChecksState::Pending => v.push(Segment::new("checks running", Some(Tone::Work))),
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

fn custom(path: &str, s: &Session, slot: ScriptSlot) -> Vec<Segment> {
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
