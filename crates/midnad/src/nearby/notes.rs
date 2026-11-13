//! Handoff notes with an agent start (`share.send` kind `agent_start` with `notes`), between my
//! own devices only.
//!
//! Short notes ride in the prompt ([`attach`]). Longer ones are written to
//! `nearby/outgoing/<share id>/notes.md` on the sender and named in the offer like an agent
//! session's transcript, with their first lines so the receiver's person sees them before
//! approving. Accepting fetches them like a file share's file (`transfer.rs`: checked and
//! resumable); [`arrived`] then saves them in Midna's home as `nearby/notes/<share id>.md` (never
//! downloads or the project) and starts the agent with a prompt that points at them.
use super::agent_ask;
use super::tab::{self, Place};
use super::{session, transfer};
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::nearby::{self as nb, AgentNotes, ShareFile, ShareItem, SharePayload, TransferState};
use midna_proto::{Actor, RpcError};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Why a device kept as a contact can't take notes.
const ONLY_MINE: &str = "only your own devices take handoff notes";

/// Where incoming share `id`'s notes are saved.
pub fn saved_path(home: &Path, id: &str) -> PathBuf {
    home.join("nearby").join("notes").join(format!("{id}.md"))
}

/// Put `notes` with an agent start's `prompt` for share `id`: in the prompt when they are short
/// and fit, else in a file to serve (returned) that the offer names.
pub fn attach(d: &Daemon, id: &str, prompt: String, notes: &str) -> Result<(String, Option<AgentNotes>, Option<PathBuf>), RpcError> {
    let notes = notes.replace("\r\n", "\n");
    let notes = notes.trim();
    nb::check_notes(notes).map_err(RpcError::bad_params)?;
    let inline = format!("{prompt}\n\n---\nHandoff notes:\n\n{notes}");
    if notes.len() <= nb::MAX_INLINE_NOTES && inline.len() <= nb::MAX_PROMPT {
        return Ok((inline, None, None));
    }
    let path = session::outgoing_dir(&d.cfg.home, id).join(nb::NOTES_FILE);
    let saved = write_private(&path, notes.as_bytes()).and_then(|()| transfer::hash_file(&path));
    let (size, sha256) = saved.map_err(|e| {
        session::forget(&d.cfg.home, id);
        RpcError::internal(format!("keeping the notes to send: {e}"))
    })?;
    let file = ShareFile { path: nb::NOTES_FILE.into(), size, sha256 };
    Ok((prompt, Some(AgentNotes { file, head: nb::notes_head(notes).to_string() }), Some(path)))
}

/// `bytes` at `path`, in a folder only this user can read.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().expect("a notes file has a folder");
    midna_proto::paths::guard_write(dir)?;
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// The sender's check: a contact gets no notes, short or long.
pub(super) fn may_send(d: &Daemon, fp: &str, notes: bool) -> Result<(), RpcError> {
    if !notes {
        return Ok(());
    }
    match d.nearby.inner().store(&d.cfg.home).get(fp).map(|s| (s.tier, s.name.clone())) {
        Some((nb::Tier::Contact, name)) => Err(RpcError::refused(format!("{name} is kept as a contact: {ONLY_MINE}"))),
        _ => Ok(()),
    }
}

/// The receiver's check: notes come only from one of my devices.
pub(super) fn admit(d: &Daemon, from: &str, payload: &SharePayload) -> Result<(), String> {
    if matches!(payload, SharePayload::AgentStart { notes: Some(_), .. }) && !agent_ask::mine(d, from) {
        return Err(ONLY_MINE.into());
    }
    Ok(())
}

/// For the approval and the guard: the notes' size and first lines, if a file of them comes.
pub fn words(payload: &SharePayload) -> Option<String> {
    let SharePayload::AgentStart { notes: Some(n), .. } = payload else { return None };
    let more = if (n.head.len() as u64) < n.file.size { "\n…" } else { "" };
    Some(format!("Handoff notes ({}), first lines:\n{}{more}", nb::size_label(n.file.size), n.head.trim_end()))
}

/// `share.accept` of an agent start with a notes file: what accepting does, once the folder
/// (`chosen`, else where it maps to) is checked. Accepting it again while the notes come, or
/// once it started, changes nothing; after it failed it tries again.
pub fn accepting(d: &Daemon, it: &ShareItem, chosen: Option<&str>) -> Result<String, RpcError> {
    if let Some(t) = it.transfer.as_ref().filter(|t| t.state != TransferState::Failed) {
        if chosen.is_some() {
            return Err(RpcError::conflict(format!("{} was already accepted; its folder can't change now", it.id)));
        }
        return Ok(it.result.clone().unwrap_or_else(|| if t.is_over() { "started".into() } else { "receiving the notes".into() }));
    }
    let place = match chosen {
        Some(c) => Place::chosen(c)?,
        None => tab::place(d, &it.payload),
    };
    place.usable()?;
    Ok(format!("receiving the notes; the agent starts {} once they are saved", place.here()))
}

/// The notes of incoming agent start `item` are staged at `staged` and checked: save them in
/// Midna's home and start the agent, its prompt pointing at them. Where they were saved, and
/// what accepting did (the share's `result`).
pub fn arrived(d: &Arc<Daemon>, item: &ShareItem, chosen: Option<&str>, staged: &Path) -> Result<(PathBuf, String), String> {
    let SharePayload::AgentStart { prompt, want_reply, notes: Some(n), .. } = &item.payload else { return Err("not an agent start with notes".into()) };
    let text = std::fs::read(staged).map_err(|e| format!("reading the notes: {e}"))?;
    let text = String::from_utf8(text).map_err(|_| "the notes aren't text".to_string())?;
    nb::check_notes(&text)?;
    if !text.starts_with(&n.head) {
        return Err("the notes don't start with the lines the offer showed".into());
    }
    let place = match chosen {
        Some(c) => Place::chosen(c).map_err(|e| e.message)?,
        None => tab::place(d, &item.payload),
    };
    place.usable().map_err(|e| e.message)?;
    let saved = saved_path(&d.cfg.home, &item.id);
    let dir = saved.parent().expect("notes have a folder");
    midna_proto::paths::guard_write(dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    std::fs::rename(staged, &saved).map_err(|e| format!("saving the notes: {e}"))?;
    let _ = std::fs::set_permissions(&saved, std::fs::Permissions::from_mode(0o600));
    let prompt = format!("{prompt}\n\nHandoff notes from {} are in {}. Read them first.", tab::plain(&item.peer_name), saved.display());
    let ctx = if item.auto { Ctx::internal_trigger(Actor::system()) } else { Ctx::internal_human() };
    let (result, sid) = super::agent_start::start(d, &ctx, item, &place, &prompt).map_err(|e| e.message)?;
    if *want_reply {
        let mut i = d.nearby.inner();
        let inbox = i.inbox(&d.cfg.home);
        if let Some(e) = inbox.get_mut(&item.id) {
            e.watch = Some(sid);
            let _ = inbox.save();
        }
    }
    let result = format!("{result}; notes in {}", saved.display());
    Ok((saved, result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_show_the_size_and_first_lines() {
        let notes = AgentNotes { file: ShareFile { path: nb::NOTES_FILE.into(), size: 9000, sha256: "0".repeat(64) }, head: "# Plan\nstep one\n".into() };
        let p = SharePayload::AgentStart {
            title: "t".into(),
            agent: midna_proto::AgentKind::Claude,
            project: None,
            cwd: "/w".into(),
            project_name: None,
            prompt: "go".into(),
            want_reply: false,
            notes: Some(notes),
        };
        let w = words(&p).unwrap();
        assert!(w.starts_with("Handoff notes (") && w.contains("# Plan\nstep one\n…"), "{w}");
    }
}
