//! Sending an agent's conversation to a paired device (`share.send` kind `agent`) and carrying
//! it on there (`share.accept`).
//!
//! The sender reads the agent terminal ([`snapshot`]): the agent, its conversation id and
//! directory (from its hooks), the repo it is in (as a tab has it, see `tab.rs`), and its
//! transcript, which it copies to `nearby/outgoing/<share id>/` as it is now (complete lines
//! only) so the agent writing on doesn't change what was offered. Claude Code keeps the
//! transcript at `<claude dir>/projects/<cwd with every non-alphanumeric character as ->/<id>.jsonl`,
//! Codex at `<codex dir>/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`. Nothing else goes: not
//! the agent's model or permission mode, its settings, its environment or its processes.
//!
//! The offer names the transcript like a file share's one file. The receiver's person sees what
//! it is and where it would open (the same place a tab would, [`tab::place`]); nothing is placed
//! and nothing runs before they accept. On accept the transcript is fetched like a file
//! (`transfer.rs`: chunked, checked, resumable) and then [`arrived`] rewrites the sending Mac's
//! paths in it (its checkout or directory, and its home folder) to this Mac's, puts it where the
//! agent here looks for that directory, and opens an agent terminal that resumes the
//! conversation (`claude --resume <id>`, `codex resume <id>`) idle, waiting for its person.
//! A transcript already there for that conversation is kept beside it (`.before-<share id>`),
//! and a conversation that a terminal here is running isn't replaced.
use super::tab::{self, Place};
use super::transfer;
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::nearby::{self as nb, ShareFile, ShareItem, SharePayload, ShareSendParams, TransferState};
use midna_proto::{AgentKind, RpcError, SessionKind, SessionOpenParams};
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The longest name Claude Code gives a project folder before it shortens it with a hash.
const CLAUDE_MAX_DIR: usize = 200;
/// How deep Codex keeps rollouts under `sessions/` (`YYYY/MM/DD/<file>`).
const CODEX_DEPTH: usize = 3;
/// The name a transcript is staged under while it arrives.
pub const STAGED: &str = "transcript.jsonl";

/// What people call the agent.
pub fn agent_name(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "Claude Code",
        AgentKind::Codex => "Codex",
    }
}

/// Where an outgoing agent session's transcript snapshot is kept until it was fetched.
pub fn outgoing_dir(home: &Path, id: &str) -> PathBuf {
    home.join("nearby").join("outgoing").join(id)
}

/// Forget outgoing share `id`'s snapshot (it was fetched, declined or dropped).
pub fn forget(home: &Path, id: &str) {
    if nb::valid_share_id(id) {
        let _ = std::fs::remove_dir_all(outgoing_dir(home, id));
    }
}

/// `share.send` kind `agent`: the payload for agent terminal `session` (default the caller's),
/// with its transcript copied for share `id`, and the copy to serve it from.
pub fn snapshot(d: &Daemon, ctx: &Ctx, p: &ShareSendParams, id: &str) -> Result<(SharePayload, PathBuf), RpcError> {
    if p.text.is_some() || p.url.is_some() || p.reply_to.is_some() || p.path.is_some() {
        return Err(RpcError::bad_params("an agent session is read from its terminal: give `session` (and a `title` if you like), not text, a url or a path"));
    }
    let sid = p
        .session
        .clone()
        .or_else(|| ctx.session.clone())
        .ok_or_else(|| RpcError::bad_params("which terminal? pass `session`, or send from inside a midna agent terminal"))?;
    let s = d.core().state.session(&sid).cloned().ok_or_else(|| RpcError::not_found(format!("no terminal {sid}")))?;
    let agent = s.agent.ok_or_else(|| RpcError::bad_params(format!("terminal {sid} isn't an agent's; send it as a tab (kind tab)")))?;
    let info = s.agent_info.clone().unwrap_or_default();
    let conversation = info
        .conversation_id
        .clone()
        .filter(|c| nb::valid_conversation(c))
        .ok_or_else(|| RpcError::conflict(format!("{}'s agent hasn't reported its conversation yet; send it once it has answered a prompt", s.name)))?;
    let cwd = info.cwd.clone().filter(|c| c.starts_with('/')).unwrap_or_else(|| s.cwd.clone());
    let (source, rel) = find_transcript(d, agent, &conversation, info.transcript_path.as_deref(), &cwd)?;
    let dir = outgoing_dir(&d.cfg.home, id);
    let copy = dir.join(rel.rsplit('/').next().unwrap_or(STAGED));
    let (size, sha256) = copy_complete_lines(&source, &copy).map_err(|e| {
        forget(&d.cfg.home, id);
        e
    })?;
    // Only so the receiver can map paths under it; "/" when there is nothing to map.
    let home = std::env::var("HOME").ok().map(|h| h.trim_end_matches('/').to_string()).filter(|h| h.starts_with('/') && h.len() <= nb::MAX_DIR);
    let payload = SharePayload::Agent {
        title: tab::title_of(p, &s.name),
        agent,
        project: tab::project_of(&cwd),
        cwd,
        home: home.unwrap_or_else(|| "/".into()),
        conversation,
        transcript: ShareFile { path: rel, size, sha256 },
    };
    if let Err(e) = payload.check() {
        forget(&d.cfg.home, id);
        return Err(RpcError::bad_params(e));
    }
    Ok((payload, copy))
}

/// The agent's transcript on this Mac and its path as the offer names it.
fn find_transcript(d: &Daemon, agent: AgentKind, conversation: &str, reported: Option<&str>, cwd: &str) -> Result<(PathBuf, String), RpcError> {
    let file = format!("{conversation}.jsonl");
    let missing = || RpcError::not_found(format!("{}'s transcript of conversation {conversation} isn't on this Mac", agent_name(agent)));
    match agent {
        AgentKind::Claude => {
            let projects = d.cfg.claude_dir.join("projects");
            let reported = reported.map(PathBuf::from).filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(file.as_str()) && p.is_file());
            let by_cwd = || claude_project(&projects, cwd).map(|dir| projects.join(dir).join(&file)).filter(|p| p.is_file());
            // Any project's: the agent may have moved directory since it started.
            let anywhere = || {
                let dirs = std::fs::read_dir(&projects).ok()?;
                dirs.flatten().map(|e| e.path().join(&file)).find(|p| p.is_file())
            };
            let path = reported.or_else(by_cwd).or_else(anywhere).ok_or_else(missing)?;
            Ok((path, file))
        }
        AgentKind::Codex => {
            let sessions = d.cfg.codex_dir.join("sessions");
            let rel = codex_rollout(&sessions, conversation).ok_or_else(missing)?;
            Ok((sessions.join(&rel), rel))
        }
    }
}

/// Claude Code's folder under `projects/` for directory `cwd`: every character but an ASCII
/// letter or digit becomes `-` (one per UTF-16 unit, as it counts them). A name over
/// [`CLAUDE_MAX_DIR`] is cut and given a hash only Claude Code knows how to make, so a long one is
/// found among the folders already there, and None if it isn't (exactly once).
pub fn claude_project(projects: &Path, cwd: &str) -> Option<String> {
    let mut name = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c.is_ascii_alphanumeric() {
            name.push(c);
        } else {
            name.extend(std::iter::repeat_n('-', c.len_utf16()));
        }
    }
    if name.len() <= CLAUDE_MAX_DIR {
        return Some(name);
    }
    let prefix = format!("{}-", &name[..CLAUDE_MAX_DIR]);
    let mut found = std::fs::read_dir(projects).ok()?.flatten().filter_map(|e| e.file_name().to_str().map(str::to_string)).filter(|n| n.starts_with(&prefix));
    let one = found.next()?;
    found.next().is_none().then_some(one)
}

/// Is `name` Codex's rollout file for conversation `id`: `rollout-<YYYY-MM-DDThh-mm-ss>-<id>.jsonl`?
fn is_rollout(name: &str, id: &str) -> bool {
    const STAMP: usize = "2026-01-01T00-00-00".len();
    let Some(rest) = name.strip_prefix("rollout-").and_then(|r| r.strip_suffix(".jsonl")) else { return false };
    rest.as_bytes().get(STAMP) == Some(&b'-') && rest.get(STAMP + 1..) == Some(id)
}

/// Conversation `id`'s rollout under Codex's `sessions/`: `YYYY/MM/DD/rollout-<time>-<id>.jsonl`.
/// The newest day first.
pub fn codex_rollout(sessions: &Path, id: &str) -> Option<String> {
    fn find(dir: &Path, rel: &str, depth: usize, id: &str) -> Option<String> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).ok()?.flatten().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_string).filter(|n| nb::valid_name(n)) else { continue };
            let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let Ok(kind) = e.file_type() else { continue };
            if depth < CODEX_DEPTH && kind.is_dir() {
                if let Some(found) = find(&e.path(), &path, depth + 1, id) {
                    return Some(found);
                }
            } else if depth == CODEX_DEPTH && kind.is_file() && is_rollout(&name, id) {
                return Some(path);
            }
        }
        None
    }
    find(sessions, "", 0, id)
}

/// Copy `src`'s complete lines (up to its last line break, so a line the agent is writing now is
/// left out) to `to`, readable only by this user; the copy's length and SHA-256.
fn copy_complete_lines(src: &Path, to: &Path) -> Result<(u64, String), RpcError> {
    let io = |e: std::io::Error| RpcError::internal(format!("copying the transcript: {e}"));
    let mut from = OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(src).map_err(|e| RpcError::not_found(format!("{}: {e}", src.display())))?;
    let len = from.metadata().map_err(io)?.len();
    if len > nb::MAX_TRANSCRIPT {
        return Err(RpcError::refused(format!("the transcript is {}; at most {} can be sent", nb::size_label(len), nb::size_label(nb::MAX_TRANSCRIPT))));
    }
    // The last line break within what is there now.
    let mut end = len;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if end == 0 {
            return Err(RpcError::conflict("the transcript has no complete line yet; send it once the agent has answered a prompt"));
        }
        let start = end.saturating_sub(buf.len() as u64);
        let n = (end - start) as usize;
        from.seek(SeekFrom::Start(start)).map_err(io)?;
        from.read_exact(&mut buf[..n]).map_err(io)?;
        if let Some(i) = buf[..n].iter().rposition(|&b| b == b'\n') {
            end = start + i as u64 + 1;
            break;
        }
        end = start;
    }
    let dir = to.parent().expect("a snapshot has a folder");
    midna_proto::paths::guard_write(dir).map_err(io)?;
    std::fs::create_dir_all(dir).map_err(io)?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let mut out = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(to).map_err(io)?;
    from.seek(SeekFrom::Start(0)).map_err(io)?;
    let copied = std::io::copy(&mut (&mut from).take(end), &mut out).map_err(io)?;
    if copied != end {
        return Err(RpcError::conflict("the transcript got shorter while it was copied; send it again"));
    }
    out.sync_all().map_err(io)?;
    transfer::hash_file(to).map_err(io)
}

/// For an agent session's approval and the guard: what it is and that all of it goes.
pub fn describe(payload: &SharePayload) -> String {
    let SharePayload::Agent { agent, project, cwd, conversation, transcript, .. } = payload else { return String::new() };
    let place = match project {
        Some(p) if p.path.is_empty() => format!("{} (in {cwd})", p.remote),
        Some(p) => format!("{}/{} (in {cwd})", p.remote, p.path),
        None => cwd.clone(),
    };
    format!(
        "{} conversation {conversation}: {place}, transcript {}.\nThe whole conversation goes as it is now: every prompt, reply, file it read and tool output in it.",
        agent_name(*agent),
        nb::size_label(transcript.size)
    )
}

/// A terminal on this Mac running conversation `conversation`, if one is.
fn running(d: &Daemon, conversation: &str) -> Option<String> {
    let core = d.core();
    let s = core.state.sessions.iter().find(|s| {
        core.rt.contains_key(&s.id) && s.agent_info.as_ref().and_then(|i| i.conversation_id.as_deref()) == Some(conversation)
    })?;
    Some(s.id.clone())
}

/// `share.accept` of an agent session: what accepting does, once the folder (`chosen`, else
/// where it maps to) and room are checked and no terminal here is running the conversation.
pub fn accepting(d: &Daemon, it: &ShareItem, chosen: Option<&str>) -> Result<String, RpcError> {
    let SharePayload::Agent { agent, conversation, .. } = &it.payload else { return Err(RpcError::bad_params("not an agent session")) };
    if let Some(t) = it.transfer.as_ref().filter(|t| t.state != TransferState::Failed) {
        if chosen.is_some() {
            return Err(RpcError::conflict(format!("{} was already accepted; its folder can't change now", it.id)));
        }
        return Ok(it.result.clone().unwrap_or_else(|| if t.is_over() { "opened".into() } else { "receiving the transcript".into() }));
    }
    let place = match chosen {
        Some(c) => Place::chosen(c)?,
        None => tab::place(d, &it.payload),
    };
    if let Some(sid) = running(d, conversation) {
        return Err(RpcError::conflict(format!("terminal {sid} here is running that conversation already; close it first")));
    }
    let store = match agent {
        AgentKind::Claude => &d.cfg.claude_dir,
        AgentKind::Codex => &d.cfg.codex_dir,
    };
    if let Some(free) = transfer::free_space(store)
        && free < it.size.saturating_mul(2)
    {
        return Err(RpcError::refused(format!("{} needs {} and {} has {} free", it.id, nb::size_label(it.size.saturating_mul(2)), store.display(), nb::size_label(free))));
    }
    Ok(format!("receiving the transcript; it opens {}", place.here()))
}

/// The transcript of incoming agent session `item` is staged at `staged` and checked: rewrite
/// the sending Mac's paths in it, put it where the agent looks, and open a terminal resuming
/// it. Where it was put, and what accepting did (the share's `result`).
pub fn arrived(d: &Arc<Daemon>, item: &ShareItem, chosen: Option<&str>, staged: &Path) -> Result<(PathBuf, String), String> {
    let SharePayload::Agent { title, agent, conversation, transcript, .. } = &item.payload else { return Err("not an agent session".into()) };
    let place = match chosen {
        Some(c) => Place::chosen(c).map_err(|e| e.message)?,
        None => tab::place(d, &item.payload),
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = place.dir().map(str::to_string).unwrap_or_else(|| home.clone());
    // The agent resolves its directory, and names its transcripts after the resolved one.
    let real = std::fs::canonicalize(&dir).map_err(|e| format!("{dir}: {e}"))?.to_string_lossy().into_owned();
    if let Some(sid) = running(d, conversation) {
        return Err(format!("terminal {sid} here is running that conversation already; close it and accept again"));
    }
    let target = match agent {
        AgentKind::Claude => {
            let projects = d.cfg.claude_dir.join("projects");
            let name = claude_project(&projects, &real).ok_or_else(|| format!("{real}'s path is too long for Claude Code to find the conversation; accept it into a shorter folder (--cwd)"))?;
            projects.join(name).join(format!("{conversation}.jsonl"))
        }
        AgentKind::Codex => transfer::safe_join(&d.cfg.codex_dir.join("sessions"), &transcript.path).ok_or("the transcript's path isn't a plain one")?,
    };
    let pairs = pairs(&item.payload, &place, &real, &home);
    place_transcript(staged, &target, &item.id, &pairs)?;
    let here = match &place {
        Place::Project { missing: Some(m), .. } => format!("in {real} ({m} isn't in your checkout)"),
        Place::Unknown => format!("in {real} (its project isn't on this Mac)"),
        _ => format!("in {real}"),
    };
    let params = SessionOpenParams {
        project_id: Some(place.project_id(d)),
        kind: SessionKind::Agent,
        agent: Some(*agent),
        name: Some(title.clone()),
        cwd: Some(real.clone()),
        command: None,
        prompt: None,
        agent_args: vec![],
        resume: Some(conversation.clone()),
        cols: None,
        rows: None,
        background: false,
        close_on_exit: false,
        on_default_branch: false,
        switch_to_default_branch: false,
    };
    let preface = format!(
        "\x1b[1m── {} · {} conversation sent from {} · resumed {here}; nothing runs until you prompt it ──\x1b[0m\r\n",
        tab::plain(title),
        agent_name(*agent),
        tab::plain(&item.peer_name)
    );
    let s = crate::rpc::session::open_session_with(d, &Ctx::internal_human(), params, Some(preface.into_bytes())).map_err(|e| e.message)?;
    let changed = {
        let mut core = d.core();
        core.state.session_mut(&s.id).map(|s| {
            let info = s.agent_info.get_or_insert_with(Default::default);
            info.conversation_id = Some(conversation.clone());
            info.transcript_path = Some(target.to_string_lossy().into_owned());
            info.cwd = Some(real.clone());
            (s.project_id.clone(), info.clone())
        })
    };
    if let Some((project, info)) = changed {
        d.mark_dirty();
        d.emit(midna_proto::kinds::SESSION_AGENT, midna_proto::Actor::system(), Some(project), Some(s.id.clone()), serde_json::to_value(&info).unwrap_or_default());
    }
    Ok((target, format!("opened as {} resuming the {} conversation {here}", s.id, agent_name(*agent))))
}

/// The sending Mac's paths and this Mac's, longest first: its checkout's top (or its directory)
/// and its home folder.
fn pairs(payload: &SharePayload, place: &Place, real: &str, home: &str) -> Vec<(String, String)> {
    let SharePayload::Agent { project, cwd, home: their_home, .. } = payload else { return vec![] };
    let mut out = vec![];
    let resolve = |p: &str| std::fs::canonicalize(p).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| p.to_string());
    match (place, project) {
        // The checkout's top for theirs: every path in the repo maps, not only below `cwd`.
        (Place::Project { dir, missing, .. }, Some(p)) if !p.path.is_empty() => {
            let their_top = cwd.strip_suffix(&format!("/{}", p.path)).unwrap_or(cwd);
            let top = if missing.is_some() { dir.as_str() } else { dir.strip_suffix(&format!("/{}", p.path)).unwrap_or(dir) };
            out.push((their_top.to_string(), resolve(top)));
        }
        _ => out.push((cwd.clone(), real.to_string())),
    }
    let home = home.trim_end_matches('/');
    if their_home.len() > 1 && !home.is_empty() {
        out.push((their_home.trim_end_matches('/').to_string(), home.to_string()));
    }
    out.retain(|(from, to)| from != to && from.len() > 1);
    out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    out
}

/// Write `staged` with `pairs` rewritten to `target`, through a temporary file; what was at
/// `target` is kept as `<target>.before-<id>`.
fn place_transcript(staged: &Path, target: &Path, id: &str, pairs: &[(String, String)]) -> Result<(), String> {
    let dir = target.parent().ok_or("no folder for the transcript")?;
    let io = |what: &Path, e: std::io::Error| format!("{}: {e}", what.display());
    midna_proto::paths::guard_write(target).map_err(|e| io(target, e))?;
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let name = target.file_name().and_then(|n| n.to_str()).ok_or("no name for the transcript")?;
    let tmp = dir.join(format!(".{name}.midna-{id}"));
    let write = || -> std::io::Result<()> {
        let src = BufReader::new(OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(staged)?);
        let mut out = std::io::BufWriter::new(OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&tmp)?);
        for line in src.split(b'\n') {
            let line = line?;
            match std::str::from_utf8(&line).ok().and_then(|l| rewrite_line(l, pairs)) {
                Some(new) => out.write_all(new.as_bytes())?,
                None => out.write_all(&line)?,
            }
            out.write_all(b"\n")?;
        }
        let f: File = out.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(target, e));
    }
    if std::fs::symlink_metadata(target).is_ok() {
        let kept = dir.join(format!("{name}.before-{id}"));
        std::fs::rename(target, &kept).map_err(|e| io(target, e))?;
    }
    std::fs::rename(&tmp, target).map_err(|e| io(target, e))
}

/// One JSONL line with `pairs` rewritten in every string and key; None when nothing changed (or
/// it isn't JSON), so the line is kept as it was.
pub fn rewrite_line(line: &str, pairs: &[(String, String)]) -> Option<String> {
    if pairs.is_empty() || !pairs.iter().any(|(from, _)| line.contains(from.as_str())) {
        return None;
    }
    let mut v: Value = serde_json::from_str(line).ok()?;
    rewrite_value(&mut v, pairs).then(|| serde_json::to_string(&v).ok()).flatten()
}

fn rewrite_value(v: &mut Value, pairs: &[(String, String)]) -> bool {
    match v {
        Value::String(s) => match replace_paths(s, pairs) {
            Some(new) => {
                *s = new;
                true
            }
            None => false,
        },
        Value::Array(items) => items.iter_mut().fold(false, |changed, v| rewrite_value(v, pairs) | changed),
        Value::Object(map) => {
            let mut changed = false;
            let old = std::mem::take(map);
            for (k, mut v) in old {
                changed |= rewrite_value(&mut v, pairs);
                let k = match replace_paths(&k, pairs) {
                    Some(new) => {
                        changed = true;
                        new
                    }
                    None => k,
                };
                map.insert(k, v);
            }
            changed
        }
        _ => false,
    }
}

/// Is `c` part of a path name, so a path can't start or end next to it?
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// `s` with each of `pairs`' paths replaced, only where it is a whole path (or the start of one):
/// not inside a longer name (`/Users/me` isn't in `/Users/meg` or `/x/Users/me`), the longest
/// first, in one pass so a replacement is never replaced again. None if none was.
pub fn replace_paths(s: &str, pairs: &[(String, String)]) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    let mut changed = false;
    let mut prev: Option<char> = None;
    let mut i = 0;
    'scan: while i < s.len() {
        let rest = &s[i..];
        if prev.is_none_or(|c| !name_char(c) && !matches!(c, '.' | '/' | '~')) {
            for (from, to) in pairs {
                let Some(after) = rest.strip_prefix(from.as_str()) else { continue };
                let mut next = after.chars();
                let ends = match next.next() {
                    None => true,
                    Some('.') => next.next().is_none_or(|c| !name_char(c) && c != '.'),
                    Some(c) => !name_char(c),
                };
                if ends {
                    out.push_str(to);
                    changed = true;
                    i += from.len();
                    prev = from.chars().last();
                    continue 'scan;
                }
            }
        }
        let c = rest.chars().next().expect("not at the end");
        out.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("midna-agent-share-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn pairs_of(list: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = list.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        v.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        v
    }

    #[test]
    fn paths_are_replaced_only_where_they_are_whole() {
        let p = pairs_of(&[("/Users/al/work/widget", "/Users/bo/src/widget"), ("/Users/al", "/Users/bo")]);
        let r = |s: &str| replace_paths(s, &p);
        assert_eq!(r("/Users/al/work/widget/src/main.rs").as_deref(), Some("/Users/bo/src/widget/src/main.rs"));
        assert_eq!(r("cd /Users/al/work/widget && ls").as_deref(), Some("cd /Users/bo/src/widget && ls"));
        assert_eq!(r("see /Users/al/.zshrc").as_deref(), Some("see /Users/bo/.zshrc"), "home, not the project");
        assert_eq!(r("in /Users/al/work/widget.").as_deref(), Some("in /Users/bo/src/widget."), "a full stop ends it");
        assert_eq!(r("'/Users/al'").as_deref(), Some("'/Users/bo'"));
        for kept in ["/Users/alice/x", "/x/Users/al/y", "~/Users/al", "a/Users/al", "/Users/al-old", "/Users/al.bak"] {
            assert_eq!(r(kept), None, "{kept}");
        }
        // A sibling of the project is still under the home folder.
        assert_eq!(r("/Users/al/work/widget2").as_deref(), Some("/Users/bo/work/widget2"));
        let project = pairs_of(&[("/w/al/widget", "/w/bo/widget")]);
        for kept in ["/w/al/widget2", "/w/al/widget.bak", "/w/al/widget-old", "/w/al/widget_x"] {
            assert_eq!(replace_paths(kept, &project), None, "{kept}");
        }
        assert_eq!(replace_paths("(/w/al/widget)", &project).as_deref(), Some("(/w/bo/widget)"));
        // One pass: a replacement holding another `from` isn't replaced again.
        let swap = pairs_of(&[("/a/one", "/b/two/a/one")]);
        assert_eq!(replace_paths("/a/one/x /a/one", &swap).as_deref(), Some("/b/two/a/one/x /b/two/a/one"));
    }

    #[test]
    fn a_jsonl_line_is_rewritten_in_values_and_keys_or_kept() {
        let p = pairs_of(&[("/w/al/widget", "/w/bo/widget")]);
        let line = r#"{"cwd":"/w/al/widget","message":{"content":[{"type":"text","text":"edit /w/al/widget/a.rs"}]},"snap":{"/w/al/widget/a.rs":1},"n":3}"#;
        let out = rewrite_line(line, &p).unwrap();
        assert_eq!(out, r#"{"cwd":"/w/bo/widget","message":{"content":[{"type":"text","text":"edit /w/bo/widget/a.rs"}]},"snap":{"/w/bo/widget/a.rs":1},"n":3}"#);
        assert_eq!(rewrite_line(r#"{"cwd":"/elsewhere"}"#, &p), None, "untouched lines keep their bytes");
        assert_eq!(rewrite_line("not json /w/al/widget", &p), None);
    }

    #[test]
    fn claude_names_project_folders_after_the_directory() {
        let t = tmp("claude");
        assert_eq!(claude_project(&t, "/Users/al/my.repo_x").as_deref(), Some("-Users-al-my-repo-x"));
        assert_eq!(claude_project(&t, "/w/é😀").as_deref(), Some("-w----"), "one - per UTF-16 unit");
        let long = format!("/{}", "a".repeat(250));
        assert_eq!(claude_project(&t, &long), None, "a long one only matches a folder that is there");
        let cut = format!("-{}", "a".repeat(199));
        std::fs::create_dir_all(t.join(format!("{cut}-k3xyz"))).unwrap();
        assert_eq!(claude_project(&t, &long), Some(format!("{cut}-k3xyz")));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn codex_rollouts_are_found_by_conversation() {
        let t = tmp("codex");
        let day = t.join("2026/11/12");
        std::fs::create_dir_all(&day).unwrap();
        std::fs::write(day.join("rollout-2026-11-12T10-00-00-abc-123.jsonl"), "{}\n").unwrap();
        std::fs::write(day.join("rollout-2026-11-12T11-00-00-zzz.jsonl"), "{}\n").unwrap();
        assert_eq!(codex_rollout(&t, "abc-123").as_deref(), Some("2026/11/12/rollout-2026-11-12T10-00-00-abc-123.jsonl"));
        assert_eq!(codex_rollout(&t, "123"), None, "the whole id");
        assert_eq!(codex_rollout(&t, "missing"), None);
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn a_snapshot_keeps_complete_lines_only() {
        let t = tmp("copy");
        let src = t.join("src.jsonl");
        std::fs::write(&src, "{\"a\":1}\n{\"b\":2}\n{\"c\":").unwrap();
        let (size, sha) = copy_complete_lines(&src, &t.join("out/x.jsonl")).unwrap();
        assert_eq!(std::fs::read_to_string(t.join("out/x.jsonl")).unwrap(), "{\"a\":1}\n{\"b\":2}\n");
        assert_eq!((size, sha.len()), (16, 64));
        std::fs::write(&src, "{\"partial\":").unwrap();
        assert!(copy_complete_lines(&src, &t.join("out/y.jsonl")).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn placing_keeps_what_was_there() {
        let t = tmp("place");
        let staged = t.join("staged.jsonl");
        std::fs::write(&staged, "{\"cwd\":\"/w/al\"}\nnot json\n").unwrap();
        let target = t.join("projects/-w-bo/c.jsonl");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "mine\n").unwrap();
        place_transcript(&staged, &target, "sh_abcd", &pairs_of(&[("/w/al", "/w/bo")])).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"cwd\":\"/w/bo\"}\nnot json\n");
        assert_eq!(std::fs::read_to_string(t.join("projects/-w-bo/c.jsonl.before-sh_abcd")).unwrap(), "mine\n");
        let _ = std::fs::remove_dir_all(&t);
    }
}
