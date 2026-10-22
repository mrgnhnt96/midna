//! Link menu (board "Link Actions", design A): a plain click on a link (one that doesn't turn
//! into a drag) opens a menu under it, styled like the right-click menu. Its header is an icon
//! tinted by the link's state, a bold title and a dim subtitle, filled from what the hover
//! preview loads (`link_preview.rs`: the open card's body when the click came from it, else
//! `link_preview::load`, which shares the card's caches). Below it, what fits the link: open,
//! copy, add to links, reveal, mention in the agent's prompt, a new terminal there. GitHub pull
//! requests, issues and Actions runs add their own: check out, watch checks, re-run, or a Claude
//! terminal that reviews, works on or explains them, opened in the project whose git remote is the
//! link's repository (else this terminal's).
//!
//! The menu is a list of entries (items, dividers and labelled sections) built per kind by
//! `entries`, so more items and app sections can be added to it. ↑/↓ move, ↩ runs, esc closes,
//! and an item's own keys (⌘C, ⇧⌘C, ⌘L) run it while the menu is open.

use super::link_preview::{self, Body, Check, Link, RunState, Tone};
use super::pr_status::State;
use super::{LINE_H, PAD_X, PAD_Y, TerminalView, link_span};
use crate::icons::Icon;
use crate::ide::{self, Ide};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The link under a click, by what can be done with it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Kind {
    Url,
    /// A GitHub pull request, issue or Actions run (`n` is the run's id).
    Pr(Gh),
    Issue(Gh),
    Run(Gh),
    /// A local or private web server (never fetched), with the terminal running it once
    /// `links.served_by` finds one.
    Server(Option<midna_proto::ServedBy>),
    /// A link pattern's match (`T711`, `PROJ-12`), opened in `app` when it names one.
    Pattern { app: Option<String>, text: String },
    File { line: Option<u32> },
    Folder,
    /// Another terminal's id.
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Gh {
    pub(super) owner: String,
    pub(super) repo: String,
    pub(super) n: u64,
}

impl Gh {
    fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Act {
    Browse,
    CopyLink,
    CopyMarkdown,
    CopyText(String),
    AddLink,
    OpenIde,
    Reveal,
    CopyPath,
    CopyRelative,
    CopyPathLine,
    Mention,
    NewTerminal,
    GoToTerminal,
    CopyBranch,
    Checkout,
    Review,
    WatchChecks,
    StartAgent,
    WhyFailed,
    Rerun,
    WatchRun,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Item {
    pub(super) act: Act,
    pub(super) label: String,
    pub(super) keys: &'static str,
    /// Shown dim in the keys' place: a pull request's branch.
    pub(super) detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    Item(Item),
    Sep,
    /// A labelled group (an app's own actions, or GitHub's), which later menus add.
    #[allow(dead_code)]
    Section(String),
}

/// What the header's icon is tinted by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Tint {
    State(State),
    Tone(Tone),
    Accent,
    Dim,
    File,
    Folder,
    Server,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Head {
    pub(super) icon: Icon,
    pub(super) tint: Tint,
    pub(super) title: String,
    pub(super) sub: String,
}

/// The menu's state on its terminal: the open menu, the click it belongs to, and the hover
/// card's body a click closed (so the header doesn't load it again).
#[derive(Default)]
pub(super) struct LinkMenus {
    pub(super) menu: Option<LinkMenu>,
    /// Bumped by every press: a `session.link_at` answer for an older click is dropped.
    pub(super) click: u64,
    pub(super) seed: Option<(Link, Body)>,
}

pub(super) struct LinkMenu {
    pos: Point<Pixels>,
    /// The `session.link_at` result.
    v: Value,
    kind: Kind,
    target: String,
    project: Option<String>,
    /// The terminal's working directory, for relative paths and mentions.
    cwd: Option<String>,
    ide: Option<Ide>,
    agent: bool,
    entries: Vec<Entry>,
    /// Index into the items (not the entries).
    sel: usize,
    body: Option<Body>,
    _load: Option<Task<()>>,
}

impl LinkMenu {
    fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Item(i) => Some(i),
            _ => None,
        })
    }

    fn head(&self) -> Head {
        head(&self.kind, &self.target, self.project.as_deref(), self.body.as_ref())
    }
}

/// What a `session.link_at` hit is. `text` is the link as drawn on screen; `is_dir` whether a
/// file target is a folder.
pub(super) fn kind_of(v: &Value, text: &str, is_dir: bool) -> Option<Kind> {
    let target = v.get("target").and_then(Value::as_str).filter(|t| !t.is_empty())?;
    match v.get("kind").and_then(Value::as_str)? {
        "session" => Some(Kind::Session),
        "file" if is_dir => Some(Kind::Folder),
        "file" => Some(Kind::File { line: v.get("line").and_then(Value::as_u64).map(|n| n as u32).filter(|n| *n > 0) }),
        "url" => {
            let app = v.get("app").and_then(Value::as_str).filter(|a| !a.is_empty()).map(str::to_string);
            let preview = v.get("preview").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
            // A pattern's match is drawn as itself, not as the URL it stands for.
            let bare = target.split_once("://").map_or(target, |(_, r)| r).trim_end_matches('/');
            let matched = !text.is_empty() && text != target && text != bare && !text.contains("://") && !text.contains('/');
            if app.is_some() || preview || matched {
                return Some(Kind::Pattern { app, text: text.to_string() });
            }
            let gh = |(owner, repo, n)| Gh { owner, repo, n };
            if let Some(g) = link_preview::github_pr(target) {
                return Some(Kind::Pr(gh(g)));
            }
            if let Some(g) = link_preview::github_issue(target) {
                return Some(Kind::Issue(gh(g)));
            }
            if let Some(g) = link_preview::github_run(target) {
                return Some(Kind::Run(gh(g)));
            }
            let web = target.starts_with("http://") || target.starts_with("https://");
            Some(if web && !link_preview::fetchable(target) { Kind::Server(None) } else { Kind::Url })
        }
        _ => None,
    }
}

/// The menu's entries for a link. `ide`: the editor a file or folder opens in; `agent`: the
/// terminal runs an agent, whose prompt a path can be mentioned in; `body`: what the preview
/// loaded (a pull request's branch, whether a run failed), when it has.
pub(super) fn entries(kind: &Kind, ide: Option<&str>, agent: bool, body: Option<&Body>) -> Vec<Entry> {
    let item = |act: Act, label: &str, keys: &'static str| Entry::Item(Item { act, label: label.to_string(), keys, detail: String::new() });
    let open_in = format!("Open in {}", ide.unwrap_or("editor"));
    match kind {
        Kind::Pr(_) => {
            let branch = match body {
                Some(Body::Pr(pr)) => pr.head.clone(),
                _ => String::new(),
            };
            vec![
                item(Act::Browse, "Open in browser", "↩"),
                item(Act::CopyLink, "Copy link", "⌘C"),
                item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
                Entry::Item(Item { act: Act::CopyBranch, label: "Copy branch name".into(), keys: "", detail: branch }),
                Entry::Sep,
                item(Act::Checkout, "Check out in a new terminal", ""),
                item(Act::Review, "Ask an agent to review", ""),
                item(Act::WatchChecks, "Watch checks", ""),
                Entry::Sep,
                item(Act::AddLink, "Add to links", "⌘L"),
            ]
        }
        Kind::Issue(g) => vec![
            item(Act::Browse, "Open in browser", "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
            item(Act::CopyText(format!("#{}", g.n)), &format!("Copy #{}", g.n), ""),
            Entry::Sep,
            item(Act::StartAgent, "Start an agent on it", ""),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::Run(_) => {
            let failed = matches!(body, Some(Body::Run(r)) if r.state == RunState::Failed);
            let mut v = vec![item(Act::Browse, "Open in browser", "↩"), item(Act::CopyLink, "Copy link", "⌘C"), Entry::Sep];
            if failed {
                v.extend([item(Act::WhyFailed, "Ask an agent why it failed", ""), item(Act::Rerun, "Re-run failed jobs", "")]);
            }
            v.extend([item(Act::WatchRun, "Watch in a new terminal", ""), Entry::Sep, item(Act::AddLink, "Add to links", "⌘L")]);
            v
        }
        Kind::Url => vec![
            item(Act::Browse, "Open in browser", "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::Server(served) => {
            let mut list = vec![item(Act::Browse, "Open in browser", "↩"), item(Act::CopyLink, "Copy link", "⌘C"), Entry::Sep];
            if served.is_some() {
                list.extend([item(Act::GoToTerminal, "Go to the terminal serving it", ""), Entry::Sep]);
            }
            list.push(item(Act::AddLink, "Add to links", "⌘L"));
            list
        }
        Kind::Pattern { app, text } => vec![
            item(Act::Browse, &format!("Open in {}", app.as_deref().unwrap_or("browser")), "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
            item(Act::CopyText(text.clone()), &format!("Copy {text}"), ""),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::File { line } => {
            let mut v = vec![
                item(Act::OpenIde, &open_in, "↩"),
                item(Act::Reveal, "Reveal in Finder", ""),
                Entry::Sep,
                item(Act::CopyPath, "Copy path", "⌘C"),
                item(Act::CopyRelative, "Copy relative path", ""),
            ];
            if line.is_some() {
                v.push(item(Act::CopyPathLine, "Copy path:line", ""));
            }
            if agent {
                v.extend([Entry::Sep, item(Act::Mention, "Mention in prompt", "")]);
            }
            v
        }
        Kind::Folder => vec![
            item(Act::NewTerminal, "New terminal here", "↩"),
            item(Act::OpenIde, &open_in, ""),
            item(Act::Reveal, "Reveal in Finder", ""),
            Entry::Sep,
            item(Act::CopyPath, "Copy path", "⌘C"),
            item(Act::CopyRelative, "Copy relative path", ""),
        ],
        Kind::Session => vec![item(Act::GoToTerminal, "Go to terminal", "↩"), Entry::Sep, item(Act::CopyLink, "Copy id", "⌘C")],
    }
}

/// The header for a link, from what the preview loaded (`body`) when it has.
pub(super) fn head(kind: &Kind, target: &str, project: Option<&str>, body: Option<&Body>) -> Head {
    let h = |icon, tint, title: String, sub: String| Head { icon, tint, title, sub };
    match kind {
        Kind::Session => h(Icon::Shell, Tint::Accent, target.to_string(), "Another terminal".into()),
        Kind::File { line } => {
            let (name, sub) = link_preview::path_title(Path::new(target), false, project);
            let sub = match line {
                Some(n) => format!("{sub} · line {n}"),
                None => sub,
            };
            h(Icon::File, Tint::File, name, sub)
        }
        Kind::Folder => {
            let (name, sub) = link_preview::path_title(Path::new(target), true, project);
            let sub = match body {
                Some(Body::Dir { total, .. }) => format!("{sub} · {total} item{}", if *total == 1 { "" } else { "s" }),
                _ => sub,
            };
            h(Icon::Folder, Tint::Folder, name, sub)
        }
        Kind::Server(served) => {
            let (host, path) = link_preview::url_title(target);
            let sub = match served {
                Some(s) => format!("{} · served by terminal “{}”", s.command, s.name),
                None if path.is_empty() => "Local server".into(),
                None => format!("/{path}"),
            };
            h(Icon::Server, Tint::Server, host, sub)
        }
        Kind::Pattern { app, text } => match body {
            Some(Body::App(p)) => {
                let sub = [p.subtitle.clone(), p.status.as_ref().map(|(s, _)| s.clone())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                let tint = p.status.as_ref().map_or(Tint::Accent, |(_, t)| Tint::Tone(*t));
                h(Icon::Tag, tint, p.title.clone(), sub)
            }
            _ => {
                let sub = app.clone().unwrap_or_else(|| link_preview::url_title(target).0);
                h(Icon::Tag, Tint::Accent, text.clone(), sub)
            }
        },
        Kind::Pr(g) => match body {
            Some(Body::Pr(pr)) => {
                let state = State::parse(&pr.state).unwrap_or(State::Open);
                let n = pr.checks.len();
                let failing = pr.checks.iter().filter(|(_, c)| *c == Check::Fail).count();
                let pending = pr.checks.iter().any(|(_, c)| *c == Check::Pending);
                let checks = match (n, failing, pending) {
                    (0, _, _) => String::new(),
                    (_, 0, false) => format!(" · ✓ {n} {}", plural(n, "check")),
                    (_, 0, true) => format!(" · {n} {} running", plural(n, "check")),
                    (_, k, _) => format!(" · ✗ {k} of {n} {} failing", plural(n, "check")),
                };
                h(state.icon(), Tint::State(state), pr.title.clone(), format!("{} #{} · {}{checks}", g.repo, g.n, label(state)))
            }
            _ => h(Icon::Pr, Tint::Dim, format!("#{}", g.n), g.repo.clone()),
        },
        Kind::Issue(g) => match body {
            Some(Body::Issue(i)) => {
                let state = if i.state == "CLOSED" { State::Closed } else { State::Open };
                // Closed issues are GitHub's purple (done), not a closed pull request's red.
                let tint = Tint::State(if state == State::Closed { State::Merged } else { State::Open });
                h(Icon::Issue, tint, i.title.clone(), format!("{} #{} · {}", g.repo, g.n, label(state)))
            }
            _ => h(Icon::Issue, Tint::Dim, format!("#{}", g.n), g.repo.clone()),
        },
        Kind::Run(g) => match body {
            Some(Body::Run(run)) => {
                let state = State::parse(run.state.key()).unwrap_or(State::Queued);
                let workflow = if run.workflow.is_empty() { &run.title } else { &run.workflow };
                let title = if run.branch.is_empty() { workflow.clone() } else { format!("{workflow} · {}", run.branch) };
                let n = run.jobs.len();
                let failed: Vec<&str> = run.jobs.iter().filter(|(_, c)| *c == Check::Fail).map(|(j, _)| j.as_str()).collect();
                let sub = match (state, failed.first()) {
                    (State::Failed, Some(job)) if n > 1 => format!("✗ failed · {job} ({} of {n} jobs)", failed.len()),
                    (State::Failed, Some(job)) => format!("✗ failed · {job}"),
                    (State::Failed, None) => "✗ failed".into(),
                    (State::Passed, _) if n > 0 => format!("✓ passed · {n} {}", plural(n, "job")),
                    (State::Passed, _) => "✓ passed".into(),
                    _ if n > 0 => format!("{} · {n} {}", label(state).to_lowercase(), plural(n, "job")),
                    _ => label(state).to_lowercase(),
                };
                h(state.icon(), Tint::State(state), title, sub)
            }
            _ => h(Icon::RunQueued, Tint::Dim, "Actions run".into(), g.repo.clone()),
        },
        Kind::Url => {
            let (name, place) = link_preview::url_title(target);
            match body {
                Some(Body::Web { title: Some(title), .. }) => {
                    h(Icon::Globe, Tint::Dim, title.clone(), if place.is_empty() { name } else { format!("{name}/{place}") })
                }
                _ => h(Icon::Globe, Tint::Dim, name, place),
            }
        }
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { word.to_string() } else { format!("{word}s") }
}

/// What `session.open` gets for a GitHub act: a shell running a `gh` command (and staying a
/// shell after it), or a Claude terminal with a prompt naming the link. Where it opens is added
/// by the caller.
pub(super) fn open_params(act: &Act, kind: &Kind, url: &str, body: Option<&Body>) -> Option<Value> {
    let g = match kind {
        Kind::Pr(g) | Kind::Issue(g) | Kind::Run(g) => g,
        _ => return None,
    };
    let (n, slug) = (g.n, g.slug());
    let shell = |name: String, cmd: String| json!({ "kind": "shell", "name": name, "command": [format!("{cmd}; exec \"${{SHELL:-/bin/zsh}}\" -l")] });
    let agent = |name: String, prompt: String| json!({ "kind": "agent", "agent": "claude", "name": name, "prompt": prompt });
    Some(match act {
        Act::Checkout => shell(format!("checkout #{n}"), format!("gh pr checkout {n} -R {slug}")),
        Act::WatchChecks => shell(format!("checks #{n}"), format!("gh pr checks {n} -R {slug} --watch")),
        Act::Rerun => shell("rerun failed jobs".into(), format!("gh run rerun {n} -R {slug} --failed")),
        Act::WatchRun => shell("watch run".into(), format!("gh run watch {n} -R {slug}")),
        Act::Review => agent(
            format!("review #{n}"),
            format!(
                "Review pull request #{n} in {slug} ({url}). Read it with `gh pr view {n} -R {slug}` and its diff with `gh pr diff {n} -R {slug}`, \
                 then tell me what's wrong, risky or missing, the most serious first. Don't push or comment on the PR."
            ),
        ),
        Act::StartAgent => {
            let prompt = match body {
                Some(Body::Issue(i)) if i.body.is_empty() => format!("Work on #{n} ({url}): {}", i.title),
                Some(Body::Issue(i)) => format!("Work on #{n} ({url}): {}\n\n{}", i.title, i.body),
                _ => format!("Work on #{n} ({url}). Read it with `gh issue view {n} -R {slug}`."),
            };
            agent(format!("#{n}"), prompt)
        }
        Act::WhyFailed => {
            let what = match body {
                Some(Body::Run(r)) if !r.branch.is_empty() => format!(" ({} on {})", if r.workflow.is_empty() { &r.title } else { &r.workflow }, r.branch),
                _ => String::new(),
            };
            agent(
                "why it failed".into(),
                format!(
                    "The GitHub Actions run {url}{what} failed. Find out why: read the failed jobs' logs with `gh run view {n} -R {slug} --log-failed`, \
                     then explain the cause and how to fix it."
                ),
            )
        }
        _ => return None,
    })
}

/// The GitHub `(owner, repo)` a git remote URL points at: `git@github.com:o/r.git`,
/// `https://github.com/o/r`, `ssh://git@github.com/o/r.git`.
pub(super) fn remote_repo(url: &str) -> Option<(String, String)> {
    let i = url.find("github.com")?;
    let rest = &url[i + "github.com".len()..];
    let rest = rest.strip_prefix(':').or_else(|| rest.strip_prefix('/'))?.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| (owner.to_string(), repo.to_string()))
}

/// The remote URLs in a git config file's text.
fn config_remotes(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("url"))
        .filter_map(|r| r.trim_start().strip_prefix('='))
        .map(|u| u.trim().to_string())
        .collect()
}

/// A checkout's git config: `.git/config`, or a linked worktree's common one.
fn git_config(dir: &Path) -> Option<PathBuf> {
    let dot = dir.join(".git");
    if dot.is_dir() {
        return Some(dot.join("config"));
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = dir.join(text.trim().strip_prefix("gitdir:")?.trim());
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok().map(|c| gitdir.join(c.trim())).unwrap_or(gitdir);
    Some(common.join("config"))
}

/// The project whose git remote is `g`'s repository.
fn project_for(projects: &[String], g: &Gh) -> Option<String> {
    projects
        .iter()
        .find(|p| {
            let text = git_config(Path::new(p)).and_then(|c| std::fs::read_to_string(c).ok()).unwrap_or_default();
            config_remotes(&text).iter().filter_map(|u| remote_repo(u)).any(|(o, r)| o.eq_ignore_ascii_case(&g.owner) && r.eq_ignore_ascii_case(&g.repo))
        })
        .cloned()
}

fn label(s: State) -> &'static str {
    match s {
        State::Open => "Open",
        State::Draft => "Draft",
        State::Merged => "Merged",
        State::Closed => "Closed",
        State::Queued => "Queued",
        State::Running => "Running",
        State::Passed => "Passed",
        State::Failed => "Failed",
        State::Cancelled => "Cancelled",
        State::Skipped => "Skipped",
        State::Approval => "Needs approval",
    }
}

fn tint_color(t: Tint, theme: &Theme) -> Hsla {
    match t {
        Tint::State(s) => s.color(theme),
        Tint::Tone(Tone::Ok) => theme.ok,
        Tint::Tone(Tone::Err) => theme.err,
        Tint::Tone(Tone::Warn) => theme.need,
        Tint::Tone(Tone::Busy) => theme.work,
        Tint::Tone(Tone::Idle) | Tint::Dim => theme.dim,
        Tint::Accent => theme.accent,
        Tint::File => theme.ansi[12],
        Tint::Folder => theme.need,
        Tint::Server => theme.ansi[14],
    }
}

/// `path` from `base` when it's inside it, else with `~` for the home folder.
pub(super) fn relative(path: &str, base: Option<&str>) -> String {
    match base.and_then(|b| Path::new(path).strip_prefix(b).ok()).filter(|r| !r.as_os_str().is_empty()) {
        Some(rel) => rel.to_string_lossy().into_owned(),
        None => crate::commands::tilde(path),
    }
}

fn debug() -> bool {
    crate::dev::var_os("MIDNA_DEBUG_TERM").is_some()
}

impl TerminalView {
    /// Whether a mouse-up opens the link menu: a plain click (an app that reports the mouse
    /// gets it, unless shift is held, as with the right-click menu).
    pub(super) fn link_menu_click(&self, mods: u8, agent_click: bool) -> bool {
        if self.ext.mouse_tracking && !agent_click {
            mods == super::MOD_SHIFT
        } else {
            mods == 0
        }
    }

    /// Open the link menu for the link at a cell, if there is one.
    pub fn open_link_menu_at(&mut self, cell: (i32, i32), cx: &mut Context<Self>) {
        if cell.0 < 0 || cell.1 < 0 || cell.1 >= self.rows as i32 || cell.0 >= self.cols as i32 {
            return;
        }
        let click = self.link_menu.click;
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        let params = json!({ "id": id, "col": cell.0, "row": cell.1 });
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let v = backend.call("session.link_at", params).ok().filter(|v| v.get("kind").and_then(Value::as_str).is_some_and(|k| k != "none"))?;
                    let target = v.get("target").and_then(Value::as_str).unwrap_or("").to_string();
                    let file = v.get("kind").and_then(Value::as_str) == Some("file");
                    let is_dir = file && Path::new(&target).is_dir();
                    let cwd = file.then(|| backend.call("session.get", json!({ "id": id })).ok()).flatten();
                    let cwd = cwd.and_then(|s| s.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string));
                    Some((v, is_dir, cwd))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if let Some((v, is_dir, cwd)) = found
                    && t.link_menu.click == click
                {
                    t.link_menu_show(cell, v, is_dir, cwd, cx);
                }
            });
        })
        .detach();
    }

    fn link_menu_show(&mut self, cell: (i32, i32), v: Value, is_dir: bool, cwd: Option<String>, cx: &mut Context<Self>) {
        let Some(row) = self.grid.get(cell.1 as usize) else { return };
        let chars: Vec<char> = row.cells.iter().map(|c| c.ch).collect();
        let url = v.get("target").and_then(Value::as_str).filter(|_| v.get("kind").and_then(Value::as_str) == Some("url"));
        let Some((a, b)) = link_span(&chars, cell.0 as usize, url) else { return };
        let text: String = chars[a..=b].iter().collect();
        let Some(kind) = kind_of(&v, &text, is_dir) else { return };
        let Some(bounds) = self.bounds.get() else { return };
        let target = v.get("target").and_then(Value::as_str).unwrap_or("").to_string();
        let env = link_preview::env(cx);
        let path = matches!(kind, Kind::File { .. } | Kind::Folder);
        let project = path.then(|| env.projects.iter().filter(|p| Path::new(&target).starts_with(p)).max_by_key(|p| p.len()).cloned()).flatten();
        let ide = path.then(|| self.preview_ide(&env, Path::new(&target), is_dir, project.as_deref())).flatten().map(|(i, _)| i);
        let agent = self.is_agent();
        let link = Link::from_link_at(&v);
        let seed = self.link_menu.seed.take().filter(|(l, _)| Some(l) == link.as_ref()).map(|(_, b)| b);
        let list = entries(&kind, ide.as_ref().map(|i| i.name.as_str()), agent, seed.as_ref());
        let wants = matches!(kind, Kind::Url | Kind::Pr(_) | Kind::Issue(_) | Kind::Run(_) | Kind::Pattern { .. } | Kind::Folder);
        let server = matches!(kind, Kind::Server(_)).then(|| self.link_menu_served_by(target.clone(), cx));
        let load = server.or_else(|| link.filter(|_| seed.is_none() && wants).map(|l| {
            cx.spawn(async move |this, cx| {
                let body = cx.background_executor().spawn(async move { link_preview::load(&l) }).await;
                let _ = this.update(cx, |t, cx| {
                    if let Some(m) = t.link_menu.menu.as_mut() {
                        m.entries = entries(&m.kind, m.ide.as_ref().map(|i| i.name.as_str()), m.agent, Some(&body));
                        m.sel = m.sel.min(m.items().count().saturating_sub(1));
                        m.body = Some(body);
                        cx.notify();
                    }
                });
            })
        }));
        let pos = point(bounds.origin.x + px(PAD_X + self.cell_w * a as f32 - 4.), bounds.origin.y + px(PAD_Y + LINE_H * (cell.1 + 1) as f32 + 2.));
        self.clear_hover(cx);
        self.preview_close(cx);
        self.menu = None;
        self.link_menu.menu = Some(LinkMenu { pos, v, kind, target, project, cwd, ide, agent, entries: list, sel: 0, body: seed, _load: load });
        cx.notify();
    }

    pub(super) fn link_menu_close(&mut self, cx: &mut Context<Self>) {
        if self.link_menu.menu.take().is_some() {
            cx.notify();
        }
    }

    /// A key while the menu is open: true when the menu used it. Any other key closes it and
    /// goes on to the terminal.
    pub(super) fn link_menu_key(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(m) = self.link_menu.menu.as_mut() else { return false };
        let n = m.items().count();
        let mods = &ks.modifiers;
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.shift;
        let keys = match (mods.platform && !mods.control && !mods.alt, mods.shift, ks.key.as_str()) {
            (true, false, "c") => Some("⌘C"),
            (true, true, "c") => Some("⇧⌘C"),
            (true, false, "l") => Some("⌘L"),
            _ => None,
        };
        match ks.key.as_str() {
            "up" if plain => m.sel = (m.sel + n - 1) % n.max(1),
            "down" if plain => m.sel = (m.sel + 1) % n.max(1),
            "enter" if plain => {
                let i = m.sel;
                self.link_menu_run(i, window, cx);
            }
            "escape" => self.link_menu_close(cx),
            _ => match keys.and_then(|k| m.items().position(|i| i.keys == k)) {
                Some(i) => self.link_menu_run(i, window, cx),
                None => {
                    self.link_menu_close(cx);
                    return false;
                }
            },
        }
        cx.notify();
        true
    }

    fn link_menu_copy(&self, text: String, cx: &mut Context<Self>) {
        if debug() {
            eprintln!("midna-app debug-term: linkmenu copied {text}");
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        crate::sounds::play("copied");
    }

    /// Ask midnad which terminal serves a local link, then add it to the open menu. The menu
    /// shows at once; this fills in when lsof answers.
    fn link_menu_served_by(&self, url: String, cx: &mut Context<Self>) -> Task<()> {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { backend.call("links.served_by", json!({ "url": url })) }).await;
            let Some(served) = found.ok().and_then(|v| serde_json::from_value::<midna_proto::ServedBy>(v).ok()) else { return };
            let _ = this.update(cx, |t, cx| {
                if let Some(m) = t.link_menu.menu.as_mut().filter(|m| matches!(m.kind, Kind::Server(None))) {
                    m.kind = Kind::Server(Some(served));
                    m.entries = entries(&m.kind, None, m.agent, None);
                    m.sel = m.sel.min(m.items().count().saturating_sub(1));
                    cx.notify();
                }
            });
        })
    }

    fn link_menu_run(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.link_menu.menu.take() else { return };
        let Some(act) = m.items().nth(i).map(|it| it.act.clone()) else { return };
        let target = m.target.clone();
        let line = match m.kind {
            Kind::File { line } => line,
            _ => None,
        };
        if debug() {
            eprintln!("midna-app debug-term: linkmenu {act:?} {target}");
        }
        match act {
            Act::GoToTerminal if let Kind::Server(Some(s)) = &m.kind => {
                let (backend, id) = (self.backend.clone(), s.session.clone());
                cx.background_executor().spawn(async move { backend.call("session.focus", json!({ "id": id })) }).detach();
            }
            Act::Browse | Act::GoToTerminal => self.open_link(&m.v, cx),
            Act::CopyLink | Act::CopyPath => self.link_menu_copy(target, cx),
            Act::CopyText(t) => self.link_menu_copy(t, cx),
            Act::CopyMarkdown => {
                let text = match &m.kind {
                    Kind::Pattern { text, .. } => text.clone(),
                    _ => m.head().title,
                };
                self.link_menu_copy(format!("[{}]({target})", text.replace(['[', ']'], "")), cx);
            }
            Act::CopyRelative => self.link_menu_copy(relative(&target, m.project.as_deref().or(m.cwd.as_deref())), cx),
            Act::CopyPathLine => self.link_menu_copy(format!("{target}:{}", line.unwrap_or(1)), cx),
            Act::Mention => {
                let at = format!("@{} ", relative(&target, m.cwd.as_deref().or(m.project.as_deref())));
                self.paste_text(&at, window, cx);
            }
            Act::AddLink => {
                let (backend, session) = (self.backend.clone(), self.session_id.clone());
                cx.background_executor().spawn(async move { backend.call("links.add", json!({ "session": session, "target": target })) }).detach();
            }
            Act::Reveal => {
                if !debug() {
                    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(&target).spawn();
                }
            }
            Act::NewTerminal => self.new_terminal_in(target, cx),
            Act::CopyBranch => match &m.body {
                Some(Body::Pr(pr)) => self.link_menu_copy(pr.head.clone(), cx),
                _ => {
                    if let Some(link) = Link::from_link_at(&m.v) {
                        cx.spawn(async move |this, cx| {
                            let body = cx.background_executor().spawn(async move { link_preview::load(&link) }).await;
                            if let Body::Pr(pr) = body {
                                let _ = this.update(cx, |t, cx| t.link_menu_copy(pr.head, cx));
                            }
                        })
                        .detach();
                    }
                }
            },
            Act::Checkout | Act::Review | Act::WatchChecks | Act::StartAgent | Act::WhyFailed | Act::Rerun | Act::WatchRun => self.link_menu_open(&act, m, cx),
            Act::OpenIde => match m.ide {
                Some(ide) if !debug() => {
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(e) = ide::open_file(&ide, &target, line) {
                                eprintln!("midna-app: couldn't open {target} in {}: {e}", ide.name);
                            }
                        })
                        .detach();
                }
                Some(_) => {}
                None => self.open_link(&m.v, cx),
            },
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// A GitHub act's terminal, in the project whose remote is the link's repository (else this
    /// terminal's), brought to the front. An issue's agent waits for its text if it's not in yet.
    fn link_menu_open(&self, act: &Act, m: LinkMenu, cx: &mut Context<Self>) {
        let (backend, session, act) = (self.backend.clone(), self.session_id.clone(), act.clone());
        let projects = link_preview::env(cx).projects;
        let link = Link::from_link_at(&m.v).filter(|_| m.body.is_none() && act == Act::StartAgent);
        cx.background_executor()
            .spawn(async move {
                let body = m.body.or_else(|| link.map(|l| link_preview::load(&l)));
                let Some(mut p) = open_params(&act, &m.kind, &m.target, body.as_ref()) else { return };
                let (Kind::Pr(g) | Kind::Issue(g) | Kind::Run(g)) = &m.kind else { return };
                match project_for(&projects, g) {
                    Some(dir) => p["cwd"] = json!(dir),
                    None => {
                        let s = backend.call("session.get", json!({ "id": session })).unwrap_or(Value::Null);
                        if let Some(v) = s.get("project_id").filter(|v| v.is_string()) {
                            p["project_id"] = v.clone();
                        }
                    }
                }
                if debug() {
                    eprintln!("midna-app debug-term: linkmenu open {p}");
                    return;
                }
                if let Ok(opened) = backend.call("session.open", p)
                    && let Some(id) = opened.get("id").and_then(Value::as_str)
                {
                    let _ = backend.call("session.focus", json!({ "id": id }));
                }
            })
            .detach();
    }

    pub(super) fn render_link_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.link_menu.menu.as_ref()?;
        let head = m.head();
        let header = div()
            .flex()
            .gap(px(9.))
            .pt(px(8.))
            .px(px(10.))
            .pb(px(9.))
            .child(div().mt(px(1.)).child(head.icon.el(15., tint_color(head.tint, theme))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(theme.fg).overflow_hidden().text_ellipsis().whitespace_nowrap().child(head.title))
                    .when(!head.sub.is_empty(), |d| {
                        d.child(div().mt(px(2.)).text_size(px(11.5)).text_color(theme.dim).overflow_hidden().text_ellipsis().whitespace_nowrap().child(head.sub))
                    }),
            );
        let mut menu = div()
            .id("link-menu")
            .occlude()
            .w(px(300.))
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .on_mouse_down_out(cx.listener(|t, _, _, cx| t.link_menu_close(cx)))
            .child(header)
            .child(div().h(px(1.)).mb(px(4.)).mx(px(6.)).bg(theme.line));
        let mut n = 0;
        for e in &m.entries {
            menu = match e {
                Entry::Sep => menu.child(div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(theme.line)),
                Entry::Section(label) => menu.child(div().px(px(10.)).pt(px(4.)).pb(px(2.)).text_size(px(10.5)).text_color(theme.dim).child(label.clone())),
                Entry::Item(it) => {
                    let i = n;
                    n += 1;
                    let on = i == m.sel;
                    menu.child(
                        div()
                            .id(("link-menu-item", i))
                            .flex()
                            .items_center()
                            .gap(px(18.))
                            .h(px(24.))
                            .px(px(10.))
                            .rounded(px(5.))
                            .text_size(px(12.5))
                            .cursor_pointer()
                            .text_color(if on { theme.accent_fg } else { theme.fg })
                            .when(on, |d| d.bg(theme.accent))
                            .on_mouse_move(cx.listener(move |t, _, _, cx| {
                                if let Some(m) = t.link_menu.menu.as_mut()
                                    && m.sel != i
                                {
                                    m.sel = i;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |t, _, window, cx| t.link_menu_run(i, window, cx)))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(it.label.clone()))
                            .child(
                                div()
                                    .text_color(if on { theme.accent_fg.opacity(0.75) } else { theme.dim })
                                    .text_size(px(11.5))
                                    .when(!it.detail.is_empty(), |d| d.max_w(px(130.)).overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(theme.mono_font.clone()))
                                    .child(if it.detail.is_empty() { SharedString::from(it.keys) } else { SharedString::from(it.detail.clone()) }),
                            ),
                    )
                }
            };
        }
        Some(deferred(anchored().position(m.pos).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::{Act, Body, Entry, Gh, Icon, Kind, Tint, config_remotes, entries, head, kind_of, open_params, relative, remote_repo};
    use super::State;
    use super::link_preview::{parse_issue, parse_pr, parse_run};
    use serde_json::json;

    fn vite() -> midna_proto::ServedBy {
        midna_proto::ServedBy { session: "s2".into(), name: "dev".into(), command: "vite".into(), pid: 4242, port: 5173 }
    }

    fn labels(kind: &Kind, ide: Option<&str>, agent: bool) -> Vec<String> {
        labels_with(kind, ide, agent, None)
    }

    fn labels_with(kind: &Kind, ide: Option<&str>, agent: bool, body: Option<&Body>) -> Vec<String> {
        entries(kind, ide, agent, body)
            .into_iter()
            .map(|e| match e {
                Entry::Item(i) if !i.detail.is_empty() => format!("{} {}", i.label, i.detail),
                Entry::Item(i) if i.keys.is_empty() => i.label,
                Entry::Item(i) => format!("{} {}", i.label, i.keys),
                Entry::Sep => "--".into(),
                Entry::Section(s) => format!("[{s}]"),
            })
            .collect()
    }

    #[test]
    fn url_items() {
        assert_eq!(labels(&Kind::Url, None, false), ["Open in browser ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "--", "Add to links ⌘L"]);
        assert_eq!(labels(&Kind::Server(None), None, false), ["Open in browser ↩", "Copy link ⌘C", "--", "Add to links ⌘L"]);
        let served = labels(&Kind::Server(Some(vite())), None, false);
        assert_eq!(served, ["Open in browser ↩", "Copy link ⌘C", "--", "Go to the terminal serving it", "--", "Add to links ⌘L"]);
    }

    #[test]
    fn pattern_items() {
        let k = Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() };
        assert_eq!(labels(&k, None, false), ["Open in Taskboard ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "Copy T711", "--", "Add to links ⌘L"]);
        let k = Kind::Pattern { app: None, text: "PROJ-12".into() };
        assert_eq!(labels(&k, None, false)[0], "Open in browser ↩");
    }

    #[test]
    fn file_items() {
        let k = Kind::File { line: Some(42) };
        assert_eq!(
            labels(&k, Some("Zed"), true),
            ["Open in Zed ↩", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path", "Copy path:line", "--", "Mention in prompt"]
        );
        // No line, not an agent, no editor found.
        assert_eq!(labels(&Kind::File { line: None }, None, false), ["Open in editor ↩", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path"]);
    }

    #[test]
    fn folder_and_session_items() {
        assert_eq!(
            labels(&Kind::Folder, Some("Cursor"), true),
            ["New terminal here ↩", "Open in Cursor", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path"]
        );
        assert_eq!(labels(&Kind::Session, None, false), ["Go to terminal ↩", "--", "Copy id ⌘C"]);
    }

    #[test]
    fn kinds() {
        let url = |t: &str| json!({ "kind": "url", "target": t });
        assert_eq!(kind_of(&url("https://example.com/a"), "https://example.com/a", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("https://example.com"), "example.com", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("https://github.com/a/b"), "github.com/a/b", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("http://localhost:3000/x"), "http://localhost:3000/x", false), Some(Kind::Server(None)));
        assert_eq!(kind_of(&url("https://jira.example.com/browse/PROJ-12"), "PROJ-12", false), Some(Kind::Pattern { app: None, text: "PROJ-12".into() }));
        let app = json!({ "kind": "url", "target": "taskboard://task/T711", "app": "Taskboard" });
        assert_eq!(kind_of(&app, "T711", false), Some(Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() }));
        let file = json!({ "kind": "file", "target": "/tmp/a.rs", "line": 7 });
        assert_eq!(kind_of(&file, "a.rs:7", false), Some(Kind::File { line: Some(7) }));
        assert_eq!(kind_of(&file, "tmp", true), Some(Kind::Folder));
        assert_eq!(kind_of(&json!({ "kind": "session", "target": "s3" }), "s3", false), Some(Kind::Session));
        assert_eq!(kind_of(&json!({ "kind": "none" }), "", false), None);
    }

    #[test]
    fn heads() {
        let f = head(&Kind::File { line: Some(12) }, "/p/proj/src/main.rs", Some("/p/proj"), None);
        assert_eq!((f.icon, f.tint, f.title.as_str(), f.sub.as_str()), (Icon::File, Tint::File, "main.rs", "src · line 12"));
        let body = Body::Dir { rows: vec![], total: 3 };
        let d = head(&Kind::Folder, "/p/proj/src", Some("/p/proj"), Some(&body));
        assert_eq!((d.icon, d.tint, d.title.as_str(), d.sub.as_str()), (Icon::Folder, Tint::Folder, "src/", "proj · 3 items"));
        let s = head(&Kind::Server(None), "http://localhost:3000", None, None);
        assert_eq!((s.icon, s.tint, s.title.as_str(), s.sub.as_str()), (Icon::Server, Tint::Server, "localhost:3000", "Local server"));
        let v = head(&Kind::Server(Some(vite())), "http://localhost:5173/decks", None, None);
        assert_eq!((v.title.as_str(), v.sub.as_str()), ("localhost:5173", "vite · served by terminal “dev”"));
        let p = head(&Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() }, "taskboard://task/T711", None, None);
        assert_eq!((p.icon, p.tint, p.title.as_str(), p.sub.as_str()), (Icon::Tag, Tint::Accent, "T711", "Taskboard"));
        let w = head(&Kind::Url, "https://example.com/docs", None, Some(&Body::Web { title: Some("Docs".into()), desc: None }));
        assert_eq!((w.icon, w.title.as_str(), w.sub.as_str()), (Icon::Globe, "Docs", "example.com/docs"));
    }

    fn gh(n: u64) -> Gh {
        Gh { owner: "mrgnhnt".into(), repo: "work-deck".into(), n }
    }

    fn pr() -> Body {
        let check = |name: &str, conclusion: &str| json!({ "name": name, "status": "COMPLETED", "conclusion": conclusion });
        let v = json!({
            "title": "Sync decks across devices", "state": "OPEN", "headRefName": "feat/deck-sync", "baseRefName": "main",
            "statusCheckRollup": [check("test", "SUCCESS"), check("lint", "SUCCESS"), check("build", "SUCCESS"), check("e2e", "SUCCESS")],
        });
        Body::Pr(parse_pr(&v).unwrap())
    }

    fn run(conclusion: &str) -> Body {
        let job = |name: &str, c: &str| json!({ "name": name, "status": "completed", "conclusion": c });
        let v = json!({
            "displayTitle": "Sync decks", "workflowName": "CI", "headBranch": "feat/deck-sync", "status": "completed", "conclusion": conclusion,
            "jobs": [job("lint", "success"), job("test", conclusion), job("build", "success"), job("e2e", conclusion)],
        });
        Body::Run(parse_run(&v, 0).unwrap())
    }

    fn issue() -> Body {
        Body::Issue(parse_issue(&json!({ "title": "Decks don't sync offline", "state": "OPEN", "body": "Steps:\r\n1. Go offline" })).unwrap())
    }

    #[test]
    fn github_kinds() {
        let url = |t: &str| json!({ "kind": "url", "target": t });
        let k = |t: &str| kind_of(&url(t), t, false);
        assert_eq!(k("https://github.com/mrgnhnt/work-deck/pull/10"), Some(Kind::Pr(gh(10))));
        assert_eq!(k("https://github.com/mrgnhnt/work-deck/issues/42"), Some(Kind::Issue(gh(42))));
        assert_eq!(k("https://github.com/mrgnhnt/work-deck/actions/runs/987"), Some(Kind::Run(gh(987))));
        assert_eq!(k("https://github.com/mrgnhnt/work-deck"), Some(Kind::Url));
    }

    #[test]
    fn pr_items() {
        let pr = pr();
        assert_eq!(
            labels_with(&Kind::Pr(gh(10)), None, false, Some(&pr)),
            [
                "Open in browser ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "Copy branch name feat/deck-sync", "--",
                "Check out in a new terminal", "Ask an agent to review", "Watch checks", "--", "Add to links ⌘L"
            ]
        );
        // Before the pull request loads, the branch is blank.
        assert_eq!(labels(&Kind::Pr(gh(10)), None, false)[3], "Copy branch name");
    }

    #[test]
    fn issue_items() {
        assert_eq!(
            labels(&Kind::Issue(gh(42)), None, false),
            ["Open in browser ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "Copy #42", "--", "Start an agent on it", "--", "Add to links ⌘L"]
        );
    }

    #[test]
    fn run_items() {
        let k = Kind::Run(gh(987));
        assert_eq!(
            labels_with(&k, None, false, Some(&run("failure"))),
            ["Open in browser ↩", "Copy link ⌘C", "--", "Ask an agent why it failed", "Re-run failed jobs", "Watch in a new terminal", "--", "Add to links ⌘L"]
        );
        let passed = ["Open in browser ↩", "Copy link ⌘C", "--", "Watch in a new terminal", "--", "Add to links ⌘L"];
        assert_eq!(labels_with(&k, None, false, Some(&run("success"))), passed);
        assert_eq!(labels(&k, None, false), passed);
    }

    #[test]
    fn github_heads() {
        let target = "https://github.com/mrgnhnt/work-deck/pull/10";
        let p = head(&Kind::Pr(gh(10)), target, None, Some(&pr()));
        assert_eq!((p.icon, p.tint, p.title.as_str(), p.sub.as_str()), (State::Open.icon(), Tint::State(State::Open), "Sync decks across devices", "work-deck #10 · Open · ✓ 4 checks"));
        let p = head(&Kind::Pr(gh(10)), target, None, None);
        assert_eq!((p.tint, p.title.as_str(), p.sub.as_str()), (Tint::Dim, "#10", "work-deck"));

        let i = head(&Kind::Issue(gh(42)), "", None, Some(&issue()));
        assert_eq!((i.icon, i.tint, i.title.as_str(), i.sub.as_str()), (Icon::Issue, Tint::State(State::Open), "Decks don't sync offline", "work-deck #42 · Open"));

        let r = head(&Kind::Run(gh(987)), "", None, Some(&run("failure")));
        assert_eq!((r.tint, r.title.as_str(), r.sub.as_str()), (Tint::State(State::Failed), "CI · feat/deck-sync", "✗ failed · test (2 of 4 jobs)"));
        let r = head(&Kind::Run(gh(987)), "", None, Some(&run("success")));
        assert_eq!(r.sub, "✓ passed · 4 jobs");
    }

    #[test]
    fn github_terminals() {
        let url = "https://github.com/mrgnhnt/work-deck/pull/10";
        let p = open_params(&Act::Checkout, &Kind::Pr(gh(10)), url, None).unwrap();
        assert_eq!(p["kind"], "shell");
        assert_eq!(p["command"][0], "gh pr checkout 10 -R mrgnhnt/work-deck; exec \"${SHELL:-/bin/zsh}\" -l");
        let p = open_params(&Act::WatchChecks, &Kind::Pr(gh(10)), url, None).unwrap();
        assert!(p["command"][0].as_str().unwrap().starts_with("gh pr checks 10 -R mrgnhnt/work-deck --watch;"));
        let p = open_params(&Act::Review, &Kind::Pr(gh(10)), url, None).unwrap();
        assert_eq!((p["kind"].as_str(), p["agent"].as_str()), (Some("agent"), Some("claude")));
        assert!(p["prompt"].as_str().unwrap().contains("#10 in mrgnhnt/work-deck"));

        let issue_url = "https://github.com/mrgnhnt/work-deck/issues/42";
        let p = open_params(&Act::StartAgent, &Kind::Issue(gh(42)), issue_url, Some(&issue())).unwrap();
        assert_eq!(p["prompt"], format!("Work on #42 ({issue_url}): Decks don't sync offline\n\nSteps:\n1. Go offline"));

        let run_kind = Kind::Run(gh(987));
        let p = open_params(&Act::Rerun, &run_kind, "", None).unwrap();
        assert!(p["command"][0].as_str().unwrap().starts_with("gh run rerun 987 -R mrgnhnt/work-deck --failed;"));
        let p = open_params(&Act::WatchRun, &run_kind, "", None).unwrap();
        assert!(p["command"][0].as_str().unwrap().starts_with("gh run watch 987 -R mrgnhnt/work-deck;"));
        let p = open_params(&Act::WhyFailed, &run_kind, "", Some(&run("failure"))).unwrap();
        assert!(p["prompt"].as_str().unwrap().contains("gh run view 987 -R mrgnhnt/work-deck --log-failed"));
        assert!(open_params(&Act::CopyLink, &run_kind, "", None).is_none());
    }

    #[test]
    fn remotes() {
        let want = Some(("mrgnhnt".to_string(), "work-deck".to_string()));
        assert_eq!(remote_repo("git@github.com:mrgnhnt/work-deck.git"), want);
        assert_eq!(remote_repo("https://github.com/mrgnhnt/work-deck"), want);
        assert_eq!(remote_repo("https://github.com/mrgnhnt/work-deck.git/"), want);
        assert_eq!(remote_repo("ssh://git@github.com/mrgnhnt/work-deck.git"), want);
        assert_eq!(remote_repo("https://gitlab.com/mrgnhnt/work-deck"), None);
        assert_eq!(remote_repo("https://github.com/mrgnhnt"), None);
        let config = "[core]\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:mrgnhnt/work-deck.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n";
        assert_eq!(config_remotes(config), ["git@github.com:mrgnhnt/work-deck.git"]);
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative("/p/proj/src/a.rs", Some("/p/proj")), "src/a.rs");
        assert_eq!(relative("/p/proj", Some("/p/proj")), "/p/proj");
        assert_eq!(relative("/elsewhere/a.rs", Some("/p/proj")), "/elsewhere/a.rs");
    }
}
