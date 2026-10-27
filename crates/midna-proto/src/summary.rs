//! A project's Summary: the terminals that closed in it lately, with what they did and how they
//! ended. midnad's `summary.rs` keeps the records; these are the wire types.
//!
//! When a terminal in a project closes (by anyone, or its process exiting with
//! `close_on_exit`), midnad writes down its name, agent, conversation, worktree and branch,
//! when it opened and closed, who closed it, how it ended, the prompts the human sent, its cost
//! and its links. Records older than `summary.keep_days` go.
//!
//! Apps add their own panel to a record with `summary.annotate`: Taskboard puts the task a
//! terminal worked on there. An app may annotate a terminal while it still runs; the panel
//! carries over to its record when it closes.
use crate::methods::PromptMark;
use crate::types::{Actor, AgentKind, Id, Link, SessionKind, StatusState, Timestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Prompts kept per record (the first ones: they say what the work was).
pub const MAX_PROMPTS: usize = 20;
/// Links kept per record, pinned ones first.
pub const MAX_LINKS: usize = 20;
/// Records kept per project, newest first, whatever their age.
pub const MAX_PER_PROJECT: usize = 50;

/// A closed terminal, as its project's Summary remembers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ClosedTerminal {
    /// The terminal's id when it ran.
    pub id: Id,
    pub project_id: Id,
    pub name: String,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// Where it worked: the agent's own folder (a worktree it moved into), else the terminal's.
    pub cwd: String,
    /// The agent's conversation, which `summary.resume` reopens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// The caller's own agent arguments (`session.open {agent_args}`), kept for a resume.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agent_args: Vec<String>,
    /// The linked git worktree it was in (its folder name); none in the main checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub opened_at: Timestamp,
    pub closed_at: Timestamp,
    /// `close`, `force_close` or `exited` (its process exited and `close_on_exit` closed it).
    pub how: String,
    pub by: Actor,
    /// Its status when it closed.
    pub state: StatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Why it ended that way, when the status said (`killed by signal 9`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The prompts the human sent it, oldest first (at most `MAX_PROMPTS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompts: Vec<PromptMark>,
    /// Every prompt the human sent, including ones past `MAX_PROMPTS`.
    #[serde(default)]
    pub turns: u32,
    /// What its agent spent, from its last cost report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Its session links (`links.list`), pinned first (at most `MAX_LINKS`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Apps' panels (`summary.annotate`), by owner.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub apps: BTreeMap<String, SummaryPanel>,
}

impl ClosedTerminal {
    /// The first app panel that has a list line (owner order), for the Summary's list row.
    pub fn line(&self) -> Option<&SummaryPanel> {
        self.apps.values().find(|p| p.line.is_some())
    }
}

/// What an app shows on a closed terminal (`summary.annotate`). Everything is optional but the title.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SummaryPanel {
    /// The panel's heading: the app's name (defaults to the owner).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    /// What the terminal worked on (`T731 Dropped files type their path`).
    pub title: String,
    /// Opened by a click on the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// A short state shown as a colored chip (`Done`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// The badge's and the list line's color: a status color (`green`, `amber`, `red`, …) or `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// One more line beside the badge (`G12 Drag and drop · 3/5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Opened by a click on the subtitle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle_url: Option<String>,
    /// The record's second line in the Summary's list, in `color` (`T731 Done`); the time follows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<String>,
    /// Buttons, each opening a URL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<SummaryButton>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SummaryButton {
    pub label: String,
    /// A URL (`https://…`, or an app's own scheme such as `taskboard://task/T731`).
    pub open: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SummaryListParams {
    /// One project's records (by id or name); all projects when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SummaryListResult {
    /// Newest first.
    pub closed: Vec<ClosedTerminal>,
    /// How many days records are kept (`summary.keep_days`).
    pub keep_days: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SummaryRemoveParams {
    /// The closed terminal's id. Leave it out and give `project_id` to clear that project's Summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SummaryRemoveResult {
    pub removed: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SummaryAnnotateParams {
    /// A running terminal's id (the panel carries over when it closes) or a closed one's.
    pub id: Id,
    /// The app's name, as in `links.actions.register` (`taskboard`). Each owner has one panel per terminal.
    pub owner: String,
    /// The panel; null (or left out) removes this owner's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<SummaryPanel>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SummaryAnnotateResult {
    /// The terminal is still running: the panel waits for it to close.
    pub running: bool,
}

/// What `summary.resume` opened.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct SummaryResumeResult {
    /// The new terminal.
    pub session: crate::types::Session,
}

/// Checks a panel from an app before it's stored.
pub fn check_panel(p: &SummaryPanel) -> Result<(), String> {
    if p.title.trim().is_empty() {
        return Err("a panel needs a title".into());
    }
    if let Some(c) = p.color.as_deref().filter(|c| !crate::types::valid_status_color(c)) {
        return Err(format!("color `{c}` must be one of {} or #rrggbb", crate::types::STATUS_COLORS.join(", ")));
    }
    let urls = p.url.iter().chain(p.subtitle_url.iter()).chain(p.buttons.iter().map(|b| &b.open));
    if let Some(u) = urls.into_iter().find(|u| !is_url(u)) {
        return Err(format!("`{u}` isn't a URL (scheme://…)"));
    }
    if p.buttons.len() > 4 {
        return Err("a panel has at most 4 buttons".into());
    }
    Ok(())
}

/// `scheme://…` with a plain scheme. `file:` and `javascript:`-like schemes aren't URLs here.
fn is_url(u: &str) -> bool {
    let Some((scheme, rest)) = u.split_once("://") else { return false };
    !rest.is_empty()
        && scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && !scheme.eq_ignore_ascii_case("file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel() -> SummaryPanel {
        SummaryPanel { title: "T731 Dropped files".into(), ..Default::default() }
    }

    #[test]
    fn panel_needs_a_title() {
        assert!(check_panel(&panel()).is_ok());
        assert!(check_panel(&SummaryPanel { title: " ".into(), ..Default::default() }).is_err());
    }

    #[test]
    fn panel_colors_are_status_colors() {
        assert!(check_panel(&SummaryPanel { color: Some("green".into()), ..panel() }).is_ok());
        assert!(check_panel(&SummaryPanel { color: Some("#4cc38a".into()), ..panel() }).is_ok());
        assert!(check_panel(&SummaryPanel { color: Some("chartreuse".into()), ..panel() }).is_err());
    }

    #[test]
    fn panel_links_are_urls() {
        let b = |open: &str| SummaryPanel { buttons: vec![SummaryButton { label: "Open".into(), open: open.into() }], ..panel() };
        assert!(check_panel(&b("taskboard://task/T731")).is_ok());
        assert!(check_panel(&b("https://github.com/x/y/pull/1")).is_ok());
        assert!(check_panel(&b("file:///etc/passwd")).is_err());
        assert!(check_panel(&b("rm -rf /")).is_err());
        assert!(check_panel(&SummaryPanel { url: Some("javascript:alert(1)".into()), ..panel() }).is_err());
    }
}
