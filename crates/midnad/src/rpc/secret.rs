//! `secret.*`: the human's pasted secrets. The GUI stores a secret when the human pastes one
//! into an agent terminal and the agent sees `[secret:NAME]` instead. Values live in the
//! Keychain (`d.vault`), metadata in `state.secrets`; events and audits never carry a value.
//!
//! A name resolves in the caller's project first, then among the global secrets.
use super::{Ctx, R, ok, session_project};
use crate::daemon::Daemon;
use crate::state::hex_id;
use midna_proto::*;
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;

/// Explicit, else the caller's terminal's project, else the project whose folder holds `cwd`
/// (the deepest one).
fn project_of(d: &Daemon, ctx: &Ctx, explicit: Option<&Id>, cwd: Option<&str>) -> Option<Id> {
    explicit.cloned().or_else(|| session_project(d, ctx.session.as_deref())).or_else(|| {
        let cwd = std::fs::canonicalize(cwd?).ok()?;
        let core = d.core();
        core.state
            .projects
            .iter()
            .filter_map(|p| Some((std::fs::canonicalize(&p.path).ok()?, p)))
            .filter(|(path, _)| cwd.starts_with(path))
            .max_by_key(|(path, _)| path.as_os_str().len())
            .map(|(_, p)| p.id.clone())
    })
}

/// The secret `name` as seen from `project`: the project's own, else the global one.
fn resolve(d: &Daemon, name: &str, project: Option<&Id>) -> Result<Secret, RpcError> {
    let core = d.core();
    let all = &core.state.secrets;
    let hit = project.and_then(|p| all.iter().find(|s| s.name == name && s.project_id.as_ref() == Some(p))).or_else(|| all.iter().find(|s| s.name == name && s.project_id.is_none()));
    match hit {
        Some(s) => Ok(s.clone()),
        None => {
            let elsewhere: Vec<&str> = all.iter().filter(|s| s.name == name).filter_map(|s| s.project_id.as_deref()).collect();
            if !elsewhere.is_empty() {
                let here = project.map(|p| format!("this is project {p}")).unwrap_or_else(|| "no project here".into());
                return Err(RpcError::not_found(format!(
                    "{name} belongs to project {} ({here}); run this from that project's terminal or folder",
                    elsewhere.join(", ")
                )));
            }
            let names: Vec<&str> = all.iter().filter(|s| s.project_id.is_none() || s.project_id.as_ref() == project).map(|s| s.name.as_str()).collect();
            let known = if names.is_empty() { "none are stored here".to_string() } else { format!("stored here: {}", names.join(", ")) };
            Err(RpcError::not_found(format!("no secret {name} ({known}); ask the human to paste it into the terminal, midna will offer to store it")))
        }
    }
}

pub fn list(d: &Daemon, ctx: &Ctx, p: SecretListParams) -> R {
    let all = d.core().state.secrets.clone();
    if p.all || (ctx.is_human() && p.project_id.is_none()) {
        return ok(all);
    }
    let project = project_of(d, ctx, p.project_id.as_ref(), p.cwd.as_deref());
    ok(all.into_iter().filter(|s| s.project_id.is_none() || s.project_id == project).collect::<Vec<_>>())
}

/// Values longer than this aren't secrets (a PEM key is a few KB).
const MAX_VALUE: usize = 64 << 10;

/// Store a secret. Humans store anything. An agent may add new names and replace secrets agents
/// stored; replacing one the human stored asks the human first, with the agent's value parked in
/// the vault (never in state or the needs-you item) until they answer.
pub fn set(d: &Daemon, ctx: &Ctx, p: SecretSetParams) -> R {
    if !secrets::valid_name(&p.name) {
        return Err(RpcError::bad_params(format!("bad secret name {:?}: use A-Z, 0-9 and _ (an environment variable name)", p.name)));
    }
    if p.value.is_empty() {
        return Err(RpcError::bad_params("value must not be empty"));
    }
    if p.value.len() > MAX_VALUE {
        return Err(RpcError::bad_params(format!("value is over {} KB; that isn't a secret", MAX_VALUE >> 10)));
    }
    let project = if p.global {
        None
    } else if ctx.is_human() {
        p.project_id.clone()
    } else {
        project_of(d, ctx, p.project_id.as_ref(), p.cwd.as_deref())
    };
    let project = project.filter(|p| p != ROOT_PROJECT_ID);
    if let Some(pid) = &project
        && d.core().state.project(pid).is_none()
    {
        return Err(RpcError::not_found(format!("no project {pid}")));
    }
    let exposed = !ctx.is_human() && !p.piped;
    let existing = d.core().state.secrets.iter().find(|s| s.name == p.name && s.project_id == project).cloned();
    if !ctx.is_human()
        && let Some(e) = existing.as_ref().filter(|e| e.humans())
    {
        let pending = format!("p_{}", hex_id(6));
        d.vault.set(&pending, p.value.as_bytes()).map_err(|e| RpcError::internal(format!("could not hold the value: {e}")))?;
        let params = json!({ "pending": pending, "name": e.name, "project_id": project, "label": p.label, "exposed": exposed, "added_by": ctx.actor() });
        let where_ = project.as_deref().map(|p| format!(" in project {p}")).unwrap_or_default();
        return Err(super::defer_to_human(
            d,
            ctx,
            &format!("Agent asks to replace your secret {}", e.name),
            &format!("secret save {}{where_} (replaces the value you stored; the new value is held in the Keychain until you answer)", e.name),
            "secret.replace",
            &params,
        ));
    }
    ok(store(d, ctx, &p.name, project, p.value.as_bytes(), p.label, exposed, ctx.actor())?)
}

/// The human approved an agent's replacement: move the parked value into place.
pub fn replace(d: &Daemon, ctx: &Ctx, p: SecretReplaceParams) -> R {
    let value = d.vault.get(&p.pending).ok_or_else(|| RpcError::not_found("the replacement value is gone; the agent can save it again"))?;
    let s = store(d, ctx, &p.name, p.project_id, &value, p.label, p.exposed, p.added_by)?;
    d.vault.delete(&p.pending);
    ok(s)
}

/// A deferred `secret.replace` was answered or dropped: forget its parked value.
pub fn drop_pending(d: &Daemon, def: &crate::state::Deferred) {
    if def.method == "secret.replace"
        && let Some(id) = def.params.get("pending").and_then(serde_json::Value::as_str)
    {
        d.vault.delete(id);
    }
}

#[allow(clippy::too_many_arguments)]
fn store(d: &Daemon, ctx: &Ctx, name: &str, project: Option<Id>, value: &[u8], label: Option<String>, exposed: bool, by: Actor) -> Result<Secret, RpcError> {
    let now = time::now_rfc3339();
    let existing = d.core().state.secrets.iter().find(|s| s.name == name && s.project_id == project).cloned();
    let mut s = existing.clone().unwrap_or_else(|| Secret {
        id: format!("s_{}", hex_id(6)),
        name: name.to_string(),
        project_id: project.clone(),
        label: None,
        created_at: now.clone(),
        updated_at: None,
        used: 0,
        last_used_at: None,
        written_to: vec![],
        added_by: None,
        exposed: false,
    });
    d.vault.set(&s.id, value).map_err(|e| RpcError::internal(format!("could not store the secret: {e}")))?;
    if label.is_some() {
        s.label = label;
    }
    if existing.is_some() {
        s.updated_at = Some(now);
    }
    s.added_by = (by.kind != ActorKind::Human).then_some(by);
    s.exposed = exposed;
    {
        let mut core = d.core();
        core.state.secrets.retain(|x| x.id != s.id);
        core.state.secrets.push(s.clone());
    }
    d.mark_dirty();
    let data = json!({ "id": s.id, "name": s.name, "label": s.label, "replaced": existing.is_some(), "added_by": s.added_by, "exposed": s.exposed });
    d.emit(kinds::SECRET_SET, ctx.actor(), s.project_id.clone(), None, data);
    Ok(s)
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: SecretNameParams) -> R {
    let s = resolve(d, &p.name, project_of(d, ctx, p.project_id.as_ref(), p.cwd.as_deref()).as_ref())?;
    d.vault.delete(&s.id);
    d.core().state.secrets.retain(|x| x.id != s.id);
    d.mark_dirty();
    d.emit(kinds::SECRET_REMOVED, ctx.actor(), s.project_id.clone(), None, json!({ "id": s.id, "name": s.name }));
    ok(OkResult { ok: true })
}

fn value_of(d: &Daemon, s: &Secret) -> Result<String, RpcError> {
    let v = d.vault.get(&s.id).ok_or_else(|| RpcError::internal(format!("the value of {} is missing from the Keychain; ask the human to paste it again", s.name)))?;
    String::from_utf8(v).map_err(|_| RpcError::internal(format!("{} is not UTF-8", s.name)))
}

fn mark_used(d: &Daemon, ctx: &Ctx, ids: &[Id], data: serde_json::Value, written: Option<&str>) {
    let now = time::now_rfc3339();
    let mut project = None;
    {
        let mut core = d.core();
        for s in core.state.secrets.iter_mut().filter(|s| ids.contains(&s.id)) {
            s.used += 1;
            s.last_used_at = Some(now.clone());
            if let Some(w) = written
                && !s.written_to.iter().any(|x| x == w)
            {
                s.written_to.push(w.to_string());
            }
            project = project.or(s.project_id.clone());
        }
    }
    d.mark_dirty();
    d.emit(kinds::SECRET_USED, ctx.actor(), project, ctx.session.clone(), data);
}

/// Only the midna CLI binary itself may resolve values (for `midna secret exec`). The CLI
/// refuses `secret.exec_env` in `midna call` and `midna mcp`, so an agent can't fetch a value
/// by accident. (Same-user processes can't be fully authenticated: this keeps values out of
/// the agent's context, it doesn't stop a determined one. See docs/SECURITY.md.)
fn from_cli(d: &Daemon, ctx: &Ctx) -> bool {
    let canon = |p: &str| std::fs::canonicalize(p).ok();
    let Some(exe) = ctx.pid.and_then(crate::peer::pid_path) else { return false };
    canon(&exe).is_some() && canon(&exe) == canon(&d.cfg.cli_path)
}

pub fn exec_env(d: &Daemon, ctx: &Ctx, p: SecretExecEnvParams) -> R {
    if !from_cli(d, ctx) {
        return Err(RpcError::refused("secret.exec_env answers only `midna secret exec`; run `midna secret exec NAME -- <command>`"));
    }
    if p.names.is_empty() {
        return Err(RpcError::bad_params("names is empty"));
    }
    let project = project_of(d, ctx, p.project_id.as_ref(), p.cwd.as_deref());
    let mut values = std::collections::BTreeMap::new();
    let mut ids = vec![];
    for n in &p.names {
        let s = resolve(d, n, project.as_ref())?;
        values.insert(n.clone(), value_of(d, &s)?);
        ids.push(s.id);
    }
    let cmd = p.command.join(" ");
    let cmd = if cmd.chars().count() > 200 { format!("{}…", cmd.chars().take(200).collect::<String>()) } else { cmd };
    mark_used(d, ctx, &ids, json!({ "names": p.names, "how": "exec", "command": cmd }), None);
    ok(SecretExecEnvResult { values })
}

pub fn write(d: &Daemon, ctx: &Ctx, p: SecretWriteParams) -> R {
    let path = std::path::PathBuf::from(&p.path);
    if !path.is_absolute() {
        return Err(RpcError::bad_params("path must be absolute"));
    }
    if path.is_dir() {
        return Err(RpcError::bad_params(format!("{} is a directory", p.path)));
    }
    let key = p.key.clone().unwrap_or_else(|| p.name.clone());
    if !key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') || key.is_empty() || key.starts_with(|c: char| c.is_ascii_digit()) {
        return Err(RpcError::bad_params(format!("bad key {key:?}")));
    }
    let s = resolve(d, &p.name, project_of(d, ctx, p.project_id.as_ref(), p.cwd.as_deref()).as_ref())?;
    let value = value_of(d, &s)?;
    let old = match std::fs::read_to_string(&path) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(RpcError::internal(format!("could not read {}: {e}", p.path))),
    };
    let (text, replaced) = upsert_env(old.as_deref().unwrap_or(""), &key, &value);
    write_file(&path, &text, old.is_none()).map_err(|e| RpcError::internal(format!("could not write {}: {e}", p.path)))?;
    mark_used(d, ctx, std::slice::from_ref(&s.id), json!({ "names": [s.name], "how": "write", "path": p.path, "key": key }), Some(&p.path));
    ok(SecretWriteResult { path: p.path, key, replaced })
}

/// Set `key` in .env text: replace its line (keeping an `export ` prefix) or append one.
pub fn upsert_env(text: &str, key: &str, value: &str) -> (String, bool) {
    let line = format!("{key}={}", env_quote(value));
    let mut out = String::new();
    let mut replaced = false;
    for l in text.split_inclusive('\n') {
        let t = l.trim_start();
        let (export, rest) = match t.strip_prefix("export ") {
            Some(r) => ("export ", r.trim_start()),
            None => ("", t),
        };
        if !replaced && rest.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('=')) {
            out.push_str(export);
            out.push_str(&line);
            out.push('\n');
            replaced = true;
        } else {
            out.push_str(l);
        }
    }
    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&line);
        out.push('\n');
    }
    (out, replaced)
}

/// Bare when it's safe in every .env dialect, else double-quoted with escapes (`\n` for
/// newlines, as dotenv expands them in double quotes).
fn env_quote(v: &str) -> String {
    if !v.is_empty() && v.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-.+/=:@,".contains(&c)) {
        return v.to_string();
    }
    let mut s = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '$' => s.push_str("\\$"),
            '`' => s.push_str("\\`"),
            '\n' => s.push_str("\\n"),
            '\r' => {}
            c => s.push(c),
        }
    }
    s.push('"');
    s
}

/// Write through a temp file in the same directory; a new file is 0600, an existing one keeps
/// its permissions.
fn write_file(path: &std::path::Path, text: &str, new: bool) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| std::io::Error::other("no parent directory"))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.midna-{}", hex_id(4)));
    let mode = if new { 0o600 } else { std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(path)?.permissions()) & 0o7777 };
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
    let res = f.write_all(text.as_bytes()).and_then(|_| f.sync_all()).and_then(|_| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_or_appends() {
        let (t, r) = upsert_env("A=1\nexport TOKEN=old # c\nB=2", "TOKEN", "n3w");
        assert_eq!((t.as_str(), r), ("A=1\nexport TOKEN=n3w\nB=2", true));
        let (t, r) = upsert_env("A=1", "TOKEN", "has space\nand \"quote\" $x");
        assert_eq!((t.as_str(), r), ("A=1\nTOKEN=\"has space\\nand \\\"quote\\\" \\$x\"\n", false));
        assert_eq!(upsert_env("", "K", "v").0, "K=v\n");
        // TOKEN_2 is another key.
        assert_eq!(upsert_env("TOKEN_2=x\n", "TOKEN", "v").0, "TOKEN_2=x\nTOKEN=v\n");
    }
}
