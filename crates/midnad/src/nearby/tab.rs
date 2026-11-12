//! Sending a terminal tab to a paired device (`share.send` kind `tab`, `share.accept`).
//!
//! The sender reads the terminal ([`snapshot`]): its name, its directory, the git repo it is in
//! (the remote as [`nb::remote_key`] gives it, which drops any credential, and the path inside
//! the repo), its shell's program name and its scrollback as plain text. Nothing of its
//! environment or its processes goes. The receiver finds its own checkout of that repo among its
//! projects ([`place`]), and on the human's accept opens a new shell there with the sender's
//! scrollback shown above it ([`open`]). The scrollback is only ever text on the new terminal's
//! screen: it reaches the engine before the shell starts printing, stripped of every control
//! character, so nothing in it can ask the terminal a question whose answer would be typed into
//! the shell. A repo this Mac has no checkout of still opens, in the home folder, with the
//! scrollback alone.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::nearby::{self as nb, SharePayload, ShareSendParams, TabProject};
use midna_proto::{RpcError, SessionKind, SessionOpenParams};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const GIT_WAIT: Duration = Duration::from_secs(3);

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["git"];
    argv.extend_from_slice(args);
    crate::git::run_cmd(&argv, cwd, GIT_WAIT, None, &[]).map(|s| s.trim().to_string())
}

/// `s` with control characters dropped (tabs kept).
pub(super) fn plain(s: &str) -> String {
    s.chars().filter(|c| !c.is_control() || *c == '\t').collect()
}

/// `s` cut to at most `max` bytes, at a character boundary.
pub(super) fn cut_bytes(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Terminal rows as a tab's scrollback: plain text, trailing blank rows dropped, the newest
/// rows that fit in [`nb::MAX_SCROLLBACK`].
pub fn scrollback_text(rows: &[String]) -> String {
    let rows: Vec<String> = rows.iter().map(|r| plain(r).trim_end().to_string()).collect();
    let end = rows.iter().rposition(|r| !r.is_empty()).map_or(0, |i| i + 1);
    let mut kept = Vec::new();
    let mut size = 0;
    for r in rows[..end].iter().rev() {
        let r = cut_bytes(r, nb::MAX_SCROLLBACK);
        if size + r.len() + 1 > nb::MAX_SCROLLBACK {
            break;
        }
        size += r.len() + 1;
        kept.push(r);
    }
    kept.reverse();
    kept.join("\n")
}

/// The repo `cwd` is in, as other Macs can find it: its remote (`origin`, else the first) and
/// the path inside it. None outside git or without a remote.
pub fn project_of(cwd: &str) -> Option<TabProject> {
    let url = git(cwd, &["remote", "get-url", "origin"]).or_else(|| {
        let first = git(cwd, &["remote"])?.lines().next()?.trim().to_string();
        git(cwd, &["remote", "get-url", &first])
    })?;
    let remote = nb::remote_key(&url)?;
    let path = git(cwd, &["rev-parse", "--show-prefix"]).unwrap_or_default().trim_end_matches('/').to_string();
    Some(TabProject { remote, path })
}

/// `share.send` kind `tab`: the payload for terminal `session` (default the caller's).
pub fn snapshot(d: &Daemon, ctx: &Ctx, p: &ShareSendParams) -> Result<SharePayload, RpcError> {
    if p.text.is_some() || p.url.is_some() || p.reply_to.is_some() || p.path.is_some() {
        return Err(RpcError::bad_params("a tab is read from its terminal: give `session` (and a `title` if you like), not text, a url or a path"));
    }
    let sid = p
        .session
        .clone()
        .or_else(|| ctx.session.clone())
        .ok_or_else(|| RpcError::bad_params("which terminal? pass `session`, or send from inside a midna terminal"))?;
    let (s, rt) = {
        let core = d.core();
        let s = core.state.session(&sid).cloned().ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
        (s, core.rt.get(&sid).cloned())
    };
    let rt = rt.ok_or_else(|| RpcError::conflict(format!("terminal {sid} isn't running")))?;
    let rows = rt.read(false).map(|(rows, _, _)| rows).ok_or_else(|| RpcError::conflict(format!("terminal {sid} isn't responding")))?;
    let cwd = crate::git::work_cwd(&s, Some(rt.pid));
    let title = title_of(p, &s.name);
    let shell = (s.kind == SessionKind::Shell)
        .then(|| s.command.first().and_then(|c| c.rsplit('/').next()).map(|n| cut_bytes(&plain(n), nb::MAX_SHELL).to_string()))
        .flatten()
        .filter(|n| !n.is_empty());
    let payload = SharePayload::Tab { title, project: project_of(&cwd), cwd, shell, scrollback: scrollback_text(&rows) };
    payload.check().map_err(RpcError::bad_params)?;
    Ok(payload)
}

/// What a sent terminal is called: the `title` given, else its name; one plain line.
pub(super) fn title_of(p: &ShareSendParams, name: &str) -> String {
    let title = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(name);
    let title = plain(title).replace('\t', " ");
    match cut_bytes(title.trim(), nb::MAX_TITLE) {
        "" => "terminal".to_string(),
        t => t.to_string(),
    }
}

/// Where a tab or an agent session was on the sending Mac: its project and directory.
fn origin(payload: &SharePayload) -> Option<(Option<&TabProject>, &str)> {
    match payload {
        SharePayload::Tab { project, cwd, .. } | SharePayload::Agent { project, cwd, .. } => Some((project.as_ref(), cwd)),
        _ => None,
    }
}

/// Where an incoming tab (or agent session) opens on this Mac.
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    /// In this project's checkout of the tab's repo, at `dir` (the tab's folder in it, or the
    /// checkout's top when that folder isn't there: `missing`).
    Project { id: String, name: String, dir: String, missing: Option<String> },
    /// At the sending Mac's own directory, which exists here too (a tab outside any repo).
    Same { dir: String },
    /// Where this Mac's person said (`share.accept` `cwd`).
    Chosen { dir: String },
    /// Its repo isn't checked out here: the home folder, with the scrollback alone.
    Unknown,
}

impl Place {
    /// For people: where it opens.
    pub fn words(&self, payload: &SharePayload) -> String {
        let Some((project, cwd)) = origin(payload) else { return String::new() };
        match self {
            Place::Project { name, dir, missing: None, .. } => format!("It opens in {dir} (project {name})."),
            Place::Project { name, dir, missing: Some(m), .. } => format!("It opens in {dir} (project {name}); {m} isn't in your checkout."),
            Place::Same { dir } => format!("It opens in {dir}."),
            Place::Chosen { dir } => format!("It opens in {dir}."),
            Place::Unknown => match project {
                Some(p) => format!("{} isn't checked out in any of your projects, so it opens in your home folder.", p.remote),
                None => format!("{cwd} isn't on this Mac, so it opens in your home folder."),
            },
        }
    }

    pub(super) fn dir(&self) -> Option<&str> {
        match self {
            Place::Project { dir, .. } | Place::Same { dir } | Place::Chosen { dir } => Some(dir),
            Place::Unknown => None,
        }
    }

    /// The folder the person picked (`share.accept` `cwd`): absolute, and a folder here.
    pub(super) fn chosen(c: &str) -> Result<Place, RpcError> {
        let c = c.trim();
        if !c.starts_with('/') || !Path::new(c).is_dir() {
            return Err(RpcError::bad_params(format!("{c} isn't a folder on this Mac (give an absolute path)")));
        }
        Ok(Place::Chosen { dir: c.to_string() })
    }

    /// The project a terminal opened here belongs to.
    pub(super) fn project_id(&self, d: &Daemon) -> String {
        match self {
            Place::Project { id, .. } => id.clone(),
            Place::Same { dir } | Place::Chosen { dir } => {
                crate::rpc::project::containing(d, dir).map(|p| p.id).unwrap_or_else(|| midna_proto::ROOT_PROJECT_ID.to_string())
            }
            Place::Unknown => midna_proto::ROOT_PROJECT_ID.to_string(),
        }
    }

    /// For the new terminal's first line and the share's result: `in <dir>`.
    pub(super) fn here(&self) -> String {
        match (self, self.dir()) {
            (Place::Unknown, _) | (_, None) => "in your home folder (its project isn't on this Mac)".to_string(),
            (Place::Project { missing: Some(m), .. }, Some(dir)) => format!("in {dir} ({m} isn't in your checkout)"),
            (_, Some(dir)) => format!("in {dir}"),
        }
    }
}

/// A checkout of `remote` among this Mac's projects: (project id, name, top of the checkout).
/// The most recently used project wins when several have one.
fn checkout_of(d: &Daemon, remote: &str) -> Option<(String, String, String)> {
    let mut projects: Vec<(String, String, String, Option<String>)> = {
        let core = d.core();
        core.state.projects.iter().map(|p| (p.id.clone(), p.name.clone(), p.path.clone(), p.last_opened_at.clone())).collect()
    };
    projects.sort_by(|a, b| b.3.cmp(&a.3));
    let mut seen = Vec::new();
    for (id, name, path, _) in projects {
        if !Path::new(&path).is_dir() {
            continue;
        }
        let Some(top) = git(&path, &["rev-parse", "--show-toplevel"]) else { continue };
        if seen.contains(&top) {
            continue;
        }
        let remotes = git(&top, &["remote", "-v"]).unwrap_or_default();
        if remotes.lines().filter_map(|l| l.split_whitespace().nth(1)).any(|u| nb::remote_key(u).as_deref() == Some(remote)) {
            return Some((id, name, top));
        }
        seen.push(top);
    }
    None
}

/// Where `payload` (a tab or an agent session) opens on this Mac, unless its person picks a
/// folder.
pub fn place(d: &Daemon, payload: &SharePayload) -> Place {
    let Some((project, cwd)) = origin(payload) else { return Place::Unknown };
    match project {
        Some(p) => match checkout_of(d, &p.remote) {
            Some((id, name, top)) => {
                let inside = Path::new(&top).join(&p.path);
                match (p.path.is_empty(), inside.is_dir()) {
                    (true, _) => Place::Project { id, name, dir: top, missing: None },
                    (false, true) => Place::Project { id, name, dir: inside.to_string_lossy().into_owned(), missing: None },
                    (false, false) => Place::Project { id, name, dir: top, missing: Some(p.path.clone()) },
                }
            }
            None => Place::Unknown,
        },
        None if Path::new(cwd).is_dir() => Place::Same { dir: cwd.to_string() },
        None => Place::Unknown,
    }
}

/// The new terminal's first screenful: a line saying whose tab it is, their scrollback (dimmed),
/// and a line saying the shell below is new. Plain text and SGR only.
pub fn preface(from: &str, payload: &SharePayload, here: &str) -> Vec<u8> {
    let SharePayload::Tab { title, scrollback, .. } = payload else { return Vec::new() };
    let (from, title, here) = (plain(from), plain(title), plain(here));
    let mut out = format!("\x1b[1m── {title} · sent from {from} · read-only: nothing here ran on this Mac ──\x1b[0m\r\n\x1b[2m");
    for line in scrollback.split('\n') {
        out.push_str(&plain(line));
        out.push_str("\r\n");
    }
    out.push_str(&format!("\x1b[0m\x1b[1m── end of {from}'s scrollback · a new shell {here} ──\x1b[0m\r\n"));
    out.into_bytes()
}

/// For a tab's approval and the guard: a one-line summary of where it was, then the end of its
/// scrollback after a blank line.
pub fn describe(payload: &SharePayload, shown: usize) -> String {
    let SharePayload::Tab { project, cwd, shell, scrollback, .. } = payload else { return String::new() };
    let tail: Vec<&str> = scrollback.rsplit('\n').take(30).collect();
    let mut tail = tail.into_iter().rev().collect::<Vec<_>>().join("\n");
    if tail.len() > shown {
        let mut start = tail.len() - shown;
        while !tail.is_char_boundary(start) {
            start += 1;
        }
        tail = format!("…{}", &tail[start..]);
    }
    let lines = if scrollback.is_empty() { 0 } else { scrollback.split('\n').count() };
    let shell = shell.as_deref().map(|s| format!(", {s}")).unwrap_or_default();
    let place = match project {
        Some(p) if p.path.is_empty() => format!("{} (in {cwd}{shell})", p.remote),
        Some(p) => format!("{}/{} (in {cwd}{shell})", p.remote, p.path),
        None => format!("{cwd}{}", shell.replacen(", ", " · ", 1)),
    };
    let summary = format!("Tab: {place}, {lines} line{} of scrollback.", if lines == 1 { "" } else { "s" });
    if tail.is_empty() { summary } else { format!("{summary}\n\n{tail}") }
}

/// Accept an incoming tab: open a new shell where it maps to (or `chosen`), under the sender's
/// scrollback. Returns what it did, for the share's `result`.
pub fn open(d: &Arc<Daemon>, ctx: &Ctx, from: &str, payload: &SharePayload, chosen: Option<&str>) -> Result<String, RpcError> {
    let SharePayload::Tab { title, .. } = payload else { return Err(RpcError::bad_params("not a tab")) };
    let place = match chosen {
        Some(c) => Place::chosen(c)?,
        None => place(d, payload),
    };
    let here = place.here();
    let params = SessionOpenParams {
        project_id: Some(place.project_id(d)),
        kind: SessionKind::Shell,
        agent: None,
        name: Some(title.clone()),
        cwd: place.dir().map(str::to_string),
        command: None,
        prompt: None,
        agent_args: vec![],
        resume: None,
        cols: None,
        rows: None,
        background: false,
        close_on_exit: false,
        on_default_branch: false,
        switch_to_default_branch: false,
    };
    let s = crate::rpc::session::open_session_with(d, ctx, params, Some(preface(from, payload, &here)))?;
    Ok(format!("opened as {} {here}", s.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(scrollback: &str) -> SharePayload {
        SharePayload::Tab {
            title: "api".into(),
            project: Some(TabProject { remote: "github.com/acme/widget".into(), path: "src".into() }),
            cwd: "/w/widget/src".into(),
            shell: Some("zsh".into()),
            scrollback: scrollback.into(),
        }
    }

    #[test]
    fn scrollback_is_plain_text_and_keeps_the_newest_rows() {
        let rows = vec!["$ printf '\x1b]52;c;aGk=\x07'".to_string(), "out\x1b[6n  ".into(), "\tok".into(), "".into(), "  ".into()];
        assert_eq!(scrollback_text(&rows), "$ printf ']52;c;aGk='\nout[6n\n\tok");
        assert!(tab(&scrollback_text(&rows)).check().is_ok());
        let many: Vec<String> = (0..nb::MAX_SCROLLBACK / 4).map(|n| format!("{n:07}")).collect();
        let kept = scrollback_text(&many);
        assert!(kept.len() <= nb::MAX_SCROLLBACK, "{}", kept.len());
        assert!(kept.ends_with(many.last().unwrap()) && !kept.starts_with("0000000"));
    }

    #[test]
    fn the_preface_carries_no_control_sequences_but_its_own() {
        let p = preface("Al\x1bpha", &tab("$ ls\nREADME"), "in /w/widget/src");
        let text = String::from_utf8(p).unwrap();
        let stripped = text.replace("\x1b[1m", "").replace("\x1b[2m", "").replace("\x1b[0m", "");
        assert!(!stripped.contains('\x1b'), "{text:?}");
        assert!(stripped.contains("sent from Alpha") && stripped.contains("$ ls\r\nREADME\r\n") && stripped.contains("a new shell in /w/widget/src"));
    }

    #[test]
    fn describing_a_tab_shows_where_it_was_and_its_last_lines() {
        let lines: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let text = describe(&tab(&lines.join("\n")), 2000);
        assert!(text.starts_with("Tab: github.com/acme/widget/src (in /w/widget/src, zsh), 40 lines of scrollback.\n\nline 10\n"), "{text}");
        assert!(text.ends_with("\nline 39"), "{text}");
        assert!(describe(&tab("x".repeat(100).as_str()), 10).ends_with("scrollback.\n\n…xxxxxxxxxx"));
        assert_eq!(describe(&tab(""), 2000), "Tab: github.com/acme/widget/src (in /w/widget/src, zsh), 0 lines of scrollback.");
    }
}
