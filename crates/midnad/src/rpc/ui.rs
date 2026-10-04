//! `ui.commands.*`: the user commands in the human's ⌘K palette, backed by
//! `$MIDNA_HOME/commands.json` (a JSON array, so scripts can still edit the file directly).
//!
//! The daemon validates what goes in (known method, known screen/icon, unique id) and emits
//! `ui.commands_changed` on every change, including hand edits of the file (polled by mtime),
//! so the app's registry live-updates.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::{Value, json};
use std::path::PathBuf;

const MAX_COMMANDS: usize = 200;
const SCREENS: &[&str] = &["rules", "triggers", "insights", "settings", "needs_you"];
const ICONS: &[&str] = &["claude", "codex", "monitor", "shell", "project", "run", "screen", "approve", "deny", "rule", "pin", "new", "trigger", "restart"];

pub fn path(d: &Daemon) -> PathBuf {
    d.cfg.home.join("commands.json")
}

/// The file's raw entries. A missing file is an empty list; a file that isn't a JSON array is
/// an error (we never overwrite something we can't read).
fn read_raw(d: &Daemon) -> Result<Vec<Value>, RpcError> {
    let p = path(d);
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(RpcError::internal(format!("reading {}: {e}", p.display()))),
    };
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(a)) => Ok(a),
        Ok(_) => Err(RpcError::conflict(format!("{} is not a JSON array; fix or remove it first", p.display()))),
        Err(e) => Err(RpcError::conflict(format!("{} is not valid JSON ({e}); fix or remove it first", p.display()))),
    }
}

fn write_raw(d: &Daemon, entries: &[Value]) -> Result<(), RpcError> {
    let p = path(d);
    let tmp = p.with_extension("json.tmp");
    let mut bytes = serde_json::to_vec_pretty(entries).map_err(|e| RpcError::internal(e.to_string()))?;
    bytes.push(b'\n');
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &p)).map_err(|e| RpcError::internal(format!("writing {}: {e}", p.display())))?;
    // Our own write isn't an external edit.
    *d.commands_mtime.lock().unwrap_or_else(|e| e.into_inner()) = mtime(&p);
    Ok(())
}

fn slug(title: &str) -> String {
    let mut s = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_end_matches('-').chars().take(48).collect()
}

/// Methods that, run from the palette, act with the human's authority beyond what an agent
/// could do itself: human-only methods, plus resolving needs-you items, settings (human-only
/// keys), rules, triggers and webhooks. The palette runs palette commands as the human.
pub fn sensitive(method: &str) -> bool {
    midna_proto::method(method).is_some_and(|m| m.human_only)
        || matches!(method, "needs_you.resolve" | "settings.set" | "settings.reset")
        || ["rule.", "trigger.", "webhooks.", "daemon.", "updates."].iter().any(|p| method.starts_with(p))
}

/// Validate and normalize one command. commands.json is writable by any agent, so a command
/// that runs a [`sensitive`] method always gets the destructive two-step confirm, and its
/// confirm text is the exact call (an agent's own `danger` wording could understate it).
/// Agents can't file commands under the "Needs you" heading either.
pub fn validate(mut c: UiCommand) -> Result<UiCommand, String> {
    c.title = c.title.trim().to_string();
    if c.title.is_empty() {
        return Err("title must not be empty".into());
    }
    if c.title.chars().count() > 80 {
        return Err("title is longer than 80 characters".into());
    }
    if c.id.trim().is_empty() {
        c.id = format!("user:{}", slug(&c.title));
    }
    if c.featured.as_deref().is_some_and(|f| f.trim().eq_ignore_ascii_case("needs you")) {
        c.featured = Some("Suggested".into());
    }
    if let Some(i) = &c.icon
        && !ICONS.contains(&i.as_str()) {
            return Err(format!("unknown icon `{i}`; one of {}", ICONS.join(", ")));
        }
    match &c.run {
        None => return Err("run is required (rpc, focus, project, screen, pop_out or prefill)".into()),
        Some(UiCommandRun::Rpc { method, params }) => {
            let Some(spec) = midna_proto::method(method) else {
                return Err(format!("unknown method `{method}`; see rpc.discover"));
            };
            if matches!(method.as_str(), "stream.attach" | "events.subscribe") {
                return Err(format!("{method} is a streaming method and can't be a palette command"));
            }
            if !(params.is_object() || params.is_null()) {
                return Err("rpc params must be an object".into());
            }
            if sensitive(method) {
                c.human_only = spec.human_only || c.human_only;
                let call = if params.is_null() { "{}".to_string() } else { params.to_string() };
                let call: String = if call.chars().count() > 600 { format!("{}…", call.chars().take(600).collect::<String>()) } else { call };
                c.danger = Some(format!("Runs {method} {call} as you. This command came from commands.json, which agents can edit."));
            }
        }
        Some(UiCommandRun::Screen { screen }) if !SCREENS.contains(&screen.as_str()) => {
            return Err(format!("unknown screen `{screen}`; one of {}", SCREENS.join(", ")));
        }
        Some(UiCommandRun::Focus { session } | UiCommandRun::PopOut { session }) if session.trim().is_empty() => {
            return Err("session must not be empty".into());
        }
        Some(UiCommandRun::Prefill { text }) if text.trim().is_empty() => return Err("prefill text must not be empty".into()),
        Some(_) => {}
    }
    Ok(c)
}

/// Parse and validate every entry: (valid commands, "#index: reason" for the rest).
fn parse_all(raw: &[Value]) -> (Vec<UiCommand>, Vec<String>) {
    let mut good: Vec<UiCommand> = vec![];
    let mut bad = vec![];
    for (i, v) in raw.iter().enumerate() {
        let res = serde_json::from_value::<UiCommand>(v.clone()).map_err(|e| e.to_string()).and_then(validate);
        match res {
            Ok(c) if good.iter().any(|g| g.id == c.id) => bad.push(format!("#{i}: duplicate id `{}`", c.id)),
            Ok(c) => good.push(c),
            Err(e) => bad.push(format!("#{i}: {e}")),
        }
    }
    (good, bad)
}

pub fn list(d: &Daemon) -> R {
    let raw = read_raw(d)?;
    let (commands, invalid) = parse_all(&raw);
    ok(UiCommandsList { path: path(d).to_string_lossy().into_owned(), commands, invalid })
}

fn entry_id(v: &Value) -> Option<String> {
    serde_json::from_value::<UiCommand>(v.clone()).ok().and_then(|c| validate(c).ok()).map(|c| c.id)
}

pub fn add(d: &Daemon, ctx: &Ctx, p: UiCommandsAddParams) -> R {
    let mut c = validate(p.command).map_err(RpcError::bad_params)?;
    c.added_by = Some(ctx.actor());
    c.added_at = Some(time::now_rfc3339());
    let mut raw = read_raw(d)?;
    let existing = raw.iter().position(|v| entry_id(v).as_deref() == Some(c.id.as_str()));
    let value = serde_json::to_value(&c).map_err(|e| RpcError::internal(e.to_string()))?;
    match existing {
        Some(_) if !p.replace => return Err(RpcError::conflict(format!("a command with id `{}` exists; pass replace=true or pick another id", c.id))),
        Some(i) => raw[i] = value,
        None if raw.len() >= MAX_COMMANDS => return Err(RpcError::conflict(format!("the palette already has {MAX_COMMANDS} user commands; remove some first"))),
        None => raw.push(value),
    }
    write_raw(d, &raw)?;
    d.emit(kinds::UI_COMMANDS_CHANGED, ctx.actor(), None, None, json!({ "added": c.id, "title": c.title, "replaced": existing.is_some() }));
    ok(c)
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: IdParams) -> R {
    let mut raw = read_raw(d)?;
    let before = raw.len();
    raw.retain(|v| entry_id(v).as_deref() != Some(p.id.as_str()) && v.get("id").and_then(Value::as_str) != Some(p.id.as_str()));
    if raw.len() == before {
        return Err(RpcError::not_found(format!("no user command `{}`; see ui.commands.list", p.id)));
    }
    write_raw(d, &raw)?;
    d.emit(kinds::UI_COMMANDS_CHANGED, ctx.actor(), None, None, json!({ "removed": p.id }));
    ok(OkResult { ok: true })
}

fn mtime(p: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Called about once a second: a hand edit of commands.json (or its removal) emits
/// `ui.commands_changed` too, so the palette never shows stale entries.
pub fn poll_file(d: &Daemon) {
    let now = mtime(&path(d));
    let changed = {
        let mut last = d.commands_mtime.lock().unwrap_or_else(|e| e.into_inner());
        let changed = *last != now;
        *last = now;
        changed
    };
    if changed {
        d.emit(kinds::UI_COMMANDS_CHANGED, Actor::system(), None, None, json!({ "external_edit": true }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(title: &str, run: UiCommandRun) -> UiCommand {
        UiCommand { title: title.into(), run: Some(run), ..Default::default() }
    }

    #[test]
    fn validates_and_normalizes() {
        let c = validate(cmd(" Deploy Staging! ", UiCommandRun::Prefill { text: "x".into() })).unwrap();
        assert_eq!((c.id.as_str(), c.title.as_str()), ("user:deploy-staging", "Deploy Staging!"));
        assert!(validate(cmd("", UiCommandRun::Prefill { text: "x".into() })).is_err());
        assert!(validate(UiCommand { title: "x".into(), ..Default::default() }).is_err());
        assert!(validate(cmd("x", UiCommandRun::Rpc { method: "nope.nope".into(), params: Value::Null })).is_err());
        assert!(validate(cmd("x", UiCommandRun::Rpc { method: "events.subscribe".into(), params: Value::Null })).is_err());
        assert!(validate(cmd("x", UiCommandRun::Screen { screen: "nowhere".into() })).is_err());
        let mut bad_icon = cmd("x", UiCommandRun::Screen { screen: "rules".into() });
        bad_icon.icon = Some("rocket".into());
        assert!(validate(bad_icon).is_err());
        // Human-only methods are forced through the two-step confirm.
        let h = validate(cmd("Stop", UiCommandRun::Rpc { method: "daemon.stop".into(), params: json!({}) })).unwrap();
        assert!(h.human_only && h.danger.is_some());
    }

    #[test]
    fn duplicates_and_garbage_are_reported() {
        let raw = vec![
            json!({"title": "A", "run": {"kind": "screen", "screen": "rules"}}),
            json!({"title": "A", "run": {"kind": "screen", "screen": "triggers"}}),
            json!({"title": 3}),
        ];
        let (good, bad) = parse_all(&raw);
        assert_eq!(good.len(), 1);
        assert_eq!(bad.len(), 2, "{bad:?}");
    }
}
