//! midna's hooks in the agents' *global* config, so a `claude` or `codex` typed into a midna
//! terminal reports status like one midna started. Opt in, human only (`hooks.install`).
//!
//! Either/or with the per-launch wiring in `hooks.rs`: while an agent's global install is
//! `current`, agents midna starts don't get midna's hooks a second time; otherwise they do,
//! with `MIDNA_HOOKS_INJECTED=1`, and `midna hook … --global` stands down.
//!
//! Nothing here may affect other apps:
//! - every entry is guarded in the shell: outside a midna terminal (`MIDNA_SESSION` unset) the
//!   `midna` binary never even runs, and a missing binary is skipped instead of failing the hook;
//! - Codex has a single `notify` command, so midna's wraps the one already there and runs it
//!   with the same arguments; uninstall puts the original back;
//! - Claude's other settings (its own `statusLine`, the user's hooks) are left as they are, and
//!   a file that doesn't parse is never rewritten.
use crate::daemon::Daemon;
use crate::hooks::{CLAUDE_EVENTS, CLAUDE_TOOL_EVENTS, sh_quote};
use midna_proto::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Marks midna's Claude hook commands (and the argv of the Codex wrapper).
const GLOBAL_FLAG: &str = "--global";
/// `$0` of the Codex wrapper script; identifies it in `notify`.
const CODEX_WRAPPER_NAME: &str = "midna-notify";

/// The CLI path to write into global config: the stable `bin/current/midna` of an installed
/// midna (it survives updates), else the CLI this daemon uses.
pub fn global_cli(d: &Daemon) -> String {
    let current = d.cfg.home.join("bin/current/midna");
    if current.is_file() { current.to_string_lossy().into_owned() } else { d.cfg.cli_path.clone() }
}

pub fn claude_path(d: &Daemon) -> PathBuf {
    d.cfg.claude_dir.join("settings.json")
}

pub fn codex_path(d: &Daemon) -> PathBuf {
    d.cfg.codex_dir.join("config.toml")
}

// ------------------------------------------------------------------ claude

pub fn claude_command(cli: &str) -> String {
    let q = sh_quote(cli);
    format!(r#"[ -n "$MIDNA_SESSION" ] && [ -x {q} ] && exec {q} hook claude {GLOBAL_FLAG} || true"#)
}

fn claude_entry(ev: &str, cli: &str) -> Value {
    let mut h = json!({ "type": "command", "command": claude_command(cli) });
    if ev == "PreToolUse" {
        h["timeout"] = json!(600);
    }
    let mut entry = serde_json::Map::new();
    if CLAUDE_TOOL_EVENTS.contains(&ev) {
        entry.insert("matcher".into(), json!("*"));
    }
    entry.insert("hooks".into(), json!([h]));
    Value::Object(entry)
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(&format!(" hook claude {GLOBAL_FLAG}")))
}

/// midna's entries per event (an entry counts if any of its hooks is ours).
fn claude_ours(settings: &Value) -> Vec<(String, Value)> {
    let mut out = vec![];
    for (ev, arr) in settings.get("hooks").and_then(Value::as_object).into_iter().flatten() {
        for entry in arr.as_array().into_iter().flatten() {
            if entry.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_ours)) {
                out.push((ev.clone(), entry.clone()));
            }
        }
    }
    out
}

/// `settings` without midna's hooks (other hooks in a shared entry are kept; emptied entries,
/// events and the `hooks` object itself are dropped).
fn claude_strip(settings: &mut Value) {
    let Some(hooks) = settings.get_mut("hooks").and_then(Value::as_object_mut) else { return };
    for arr in hooks.values_mut() {
        let Some(entries) = arr.as_array_mut() else { continue };
        for e in entries.iter_mut() {
            if let Some(hs) = e.get_mut("hooks").and_then(Value::as_array_mut) {
                hs.retain(|h| !is_ours(h));
            }
        }
        entries.retain(|e| e.get("hooks").and_then(Value::as_array).is_none_or(|hs| !hs.is_empty()));
    }
    hooks.retain(|_, arr| arr.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty()
        && let Some(o) = settings.as_object_mut()
    {
        o.remove("hooks");
    }
}

fn claude_with(settings: &Value, cli: &str) -> Value {
    let mut v = settings.clone();
    claude_strip(&mut v);
    if !v.is_object() {
        v = json!({});
    }
    let hooks = v.as_object_mut().unwrap().entry("hooks").or_insert_with(|| json!({}));
    for ev in CLAUDE_EVENTS {
        let arr = hooks.as_object_mut().unwrap().entry(ev.to_string()).or_insert_with(|| json!([]));
        if let Some(a) = arr.as_array_mut() {
            a.push(claude_entry(ev, cli));
        }
    }
    v
}

/// (state, detail) for already-parsed settings.
fn claude_state(settings: &Value, cli: &str) -> (&'static str, Option<String>) {
    let ours = claude_ours(settings);
    if ours.is_empty() {
        return ("not_installed", None);
    }
    let want: Vec<(String, Value)> = CLAUDE_EVENTS.iter().map(|ev| (ev.to_string(), claude_entry(ev, cli))).collect();
    if ours.len() == want.len() && want.iter().all(|w| ours.contains(w)) {
        return ("current", None);
    }
    let expected = claude_command(cli);
    let old_path = ours.iter().flat_map(|(_, e)| e["hooks"].as_array().cloned().unwrap_or_default()).filter(is_ours).any(|h| h["command"] != json!(expected));
    let missing: Vec<&str> = CLAUDE_EVENTS.iter().copied().filter(|ev| !ours.iter().any(|(e, _)| e == ev)).collect();
    let detail = if old_path {
        "They run an older midna, or one from another location.".to_string()
    } else if !missing.is_empty() {
        format!("Missing {} event{}: {}.", missing.len(), if missing.len() == 1 { "" } else { "s" }, missing.join(", "))
    } else {
        "They differ from what this version of midna installs.".to_string()
    };
    ("stale", Some(detail))
}

fn read_claude(path: &Path) -> Result<(Option<String>, Value), String> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.trim().is_empty() => Ok((Some(s), json!({}))),
        Ok(s) => serde_json::from_str::<Value>(&s)
            .map_err(|e| format!("{} isn't plain JSON ({e}); midna won't rewrite it.", path.display()))
            .and_then(|v| if v.is_object() { Ok((Some(s), v)) } else { Err(format!("{} isn't a JSON object.", path.display())) }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((None, json!({}))),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

fn claude_text(v: &Value, before: Option<&str>) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_default();
    if before.is_none_or(|b| b.ends_with('\n')) {
        s.push('\n');
    }
    s
}

// ------------------------------------------------------------------ codex

/// The wrapper script: report to midna inside a midna terminal, then run the notify command
/// that was there before (its argv follows `$0`; Codex appends the event JSON last).
pub fn codex_script(cli: &str) -> String {
    let q = sh_quote(cli);
    format!(
        r#"for a; do last="$a"; done; if [ -n "$MIDNA_SESSION" ] && [ -x {q} ]; then {q} hook codex notify {GLOBAL_FLAG} "$last" >/dev/null 2>&1; fi; if [ $# -gt 1 ]; then exec "$@"; fi"#
    )
}

fn codex_argv(cli: &str, previous: &[String]) -> Vec<String> {
    let mut v = vec!["/bin/sh".to_string(), "-c".into(), codex_script(cli), CODEX_WRAPPER_NAME.into()];
    v.extend(previous.iter().cloned());
    v
}

fn notify_of(doc: &toml_edit::DocumentMut) -> Option<Vec<String>> {
    let arr = doc.get("notify")?.as_array()?;
    Some(arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
}

/// `Some(previous notify argv)` when `notify` is midna's wrapper.
fn codex_previous(argv: &[String]) -> Option<Vec<String>> {
    (argv.len() >= 4 && argv[0] == "/bin/sh" && argv[1] == "-c" && argv[3] == CODEX_WRAPPER_NAME).then(|| argv[4..].to_vec())
}

fn set_notify(doc: &mut toml_edit::DocumentMut, argv: Option<Vec<String>>) {
    match argv {
        Some(a) => {
            let mut arr = toml_edit::Array::new();
            for s in a {
                arr.push(s);
            }
            doc["notify"] = toml_edit::value(arr);
        }
        None => {
            doc.remove("notify");
        }
    }
}

fn codex_state(doc: &toml_edit::DocumentMut, cli: &str) -> (&'static str, Option<String>) {
    let Some(argv) = notify_of(doc) else { return ("not_installed", None) };
    let Some(prev) = codex_previous(&argv) else { return ("not_installed", None) };
    if argv == codex_argv(cli, &prev) {
        ("current", None)
    } else {
        ("stale", Some("The notify wrapper runs an older midna, or one from another location.".into()))
    }
}

fn read_codex(path: &Path) -> Result<(Option<String>, toml_edit::DocumentMut), String> {
    match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map(|d| (Some(s), d)).map_err(|e| format!("{} doesn't parse as TOML ({e}); midna won't rewrite it.", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((None, toml_edit::DocumentMut::new())),
        Err(e) => Err(format!("can't read {}: {e}", path.display())),
    }
}

// ------------------------------------------------------------------ status / plan

fn dir_of(d: &Daemon, a: AgentKind) -> &Path {
    match a {
        AgentKind::Claude => &d.cfg.claude_dir,
        AgentKind::Codex => &d.cfg.codex_dir,
    }
}

fn path_of(d: &Daemon, a: AgentKind) -> PathBuf {
    match a {
        AgentKind::Claude => claude_path(d),
        AgentKind::Codex => codex_path(d),
    }
}

pub fn status_of(d: &Daemon, a: AgentKind) -> HookInstall {
    let path = path_of(d, a);
    let cli = global_cli(d);
    let (state, detail) = if !dir_of(d, a).is_dir() {
        ("unavailable", None)
    } else {
        let r = match a {
            AgentKind::Claude => read_claude(&path).map(|(_, v)| claude_state(&v, &cli)),
            AgentKind::Codex => read_codex(&path).map(|(_, doc)| codex_state(&doc, &cli)),
        };
        r.unwrap_or_else(|e| ("error", Some(e)))
    };
    HookInstall { agent: a, state: state.into(), path: path.to_string_lossy().into_owned(), detail, per_session: state != "current" }
}

pub fn status(d: &Daemon) -> HooksStatus {
    HooksStatus { claude: status_of(d, AgentKind::Claude), codex: status_of(d, AgentKind::Codex) }
}

/// Emit `hooks.changed` when the status differs from the last one seen (someone edited a config
/// file, midna moved). Called every few seconds and after install/uninstall.
pub fn poll(d: &Daemon) {
    static LAST: std::sync::Mutex<Vec<(PathBuf, HooksStatus)>> = std::sync::Mutex::new(vec![]);
    let now = status(d);
    let changed = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        match last.iter_mut().find(|(h, _)| *h == d.cfg.home) {
            Some((_, s)) if *s == now => false,
            Some((_, s)) => {
                *s = now.clone();
                true
            }
            None => {
                last.push((d.cfg.home.clone(), now.clone()));
                false
            }
        }
    };
    if changed {
        d.emit(kinds::HOOKS_CHANGED, Actor::system(), None, None, serde_json::to_value(&now).unwrap_or_default());
    }
}

/// The global install covers this agent, so midna-started ones skip the per-launch hooks.
pub fn covers(d: &Daemon, a: AgentKind) -> bool {
    status_of(d, a).state == "current"
}

/// The file's text before and after (None after = nothing to write).
fn plan(d: &Daemon, a: AgentKind, uninstall: bool) -> Result<(Option<String>, String), String> {
    let path = path_of(d, a);
    let cli = global_cli(d);
    match a {
        AgentKind::Claude => {
            let (before, v) = read_claude(&path)?;
            let mut after = v.clone();
            if uninstall {
                claude_strip(&mut after);
            } else {
                after = claude_with(&v, &cli);
            }
            let text = if after == v { before.clone().unwrap_or_default() } else { claude_text(&after, before.as_deref()) };
            Ok((before, text))
        }
        AgentKind::Codex => {
            let (before, mut doc) = read_codex(&path)?;
            let cur = notify_of(&doc);
            let prev = match &cur {
                Some(argv) => codex_previous(argv).unwrap_or_else(|| argv.clone()),
                None => vec![],
            };
            if uninstall {
                if cur.as_deref().is_some_and(|c| codex_previous(c).is_some()) {
                    set_notify(&mut doc, (!prev.is_empty()).then_some(prev));
                }
            } else {
                set_notify(&mut doc, Some(codex_argv(&cli, &prev)));
            }
            Ok((before, doc.to_string()))
        }
    }
}

fn targets(d: &Daemon, agents: &[AgentKind]) -> Vec<AgentKind> {
    if agents.is_empty() {
        [AgentKind::Claude, AgentKind::Codex].into_iter().filter(|a| dir_of(d, *a).is_dir()).collect()
    } else {
        agents.to_vec()
    }
}

pub fn preview(d: &Daemon, p: HooksPreviewParams) -> HooksPreview {
    let files = targets(d, &p.agents)
        .into_iter()
        .map(|a| {
            let path = path_of(d, a).to_string_lossy().into_owned();
            match plan(d, a, p.uninstall) {
                Ok((before, after)) => {
                    let b = before.clone().unwrap_or_default();
                    HookFileDiff { agent: a, path, creates: before.is_none() && !p.uninstall, unchanged: b == after, lines: diff(&b, &after, 2), error: None }
                }
                Err(e) => HookFileDiff { agent: a, path, creates: false, unchanged: true, lines: vec![], error: Some(e) },
            }
        })
        .collect();
    HooksPreview { files }
}

/// Write the planned change for each target. Errors name the files that couldn't be changed;
/// the others are still written.
pub fn apply(d: &Daemon, agents: &[AgentKind], uninstall: bool) -> Result<HooksStatus, RpcError> {
    let mut errors = vec![];
    for a in targets(d, agents) {
        if !uninstall && !dir_of(d, a).is_dir() {
            errors.push(format!("{} isn't set up on this Mac ({} doesn't exist)", a.as_str(), dir_of(d, a).display()));
            continue;
        }
        let path = path_of(d, a);
        match plan(d, a, uninstall) {
            Ok((before, after)) if before.as_deref() == Some(after.as_str()) => {}
            Ok((None, _)) if uninstall => {}
            Ok((before, after)) => {
                if let Err(e) = write(&path, before.as_deref(), &after) {
                    errors.push(format!("writing {} failed: {e}", path.display()));
                }
            }
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() { Ok(status(d)) } else { Err(RpcError::internal(errors.join("; "))) }
}

/// Atomic write. The first time midna changes a file, the original is kept next to it as
/// `<name>.before-midna`.
fn write(path: &Path, before: Option<&str>, after: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if let Some(b) = before {
        let backup = path.with_file_name(format!("{}.before-midna", path.file_name().unwrap_or_default().to_string_lossy()));
        if !backup.exists() {
            std::fs::write(&backup, b)?;
        }
    }
    let tmp = path.with_file_name(format!(".{}.midna-tmp", path.file_name().unwrap_or_default().to_string_lossy()));
    std::fs::write(&tmp, after)?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path)
}

/// Line diff (LCS) with `context` unchanged lines around each change; longer unchanged runs
/// collapse to one `…` line.
pub fn diff(before: &str, after: &str, context: usize) -> Vec<DiffLine> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut all: Vec<(char, &str)> = vec![];
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            all.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            all.push(('-', a[i]));
            i += 1;
        } else {
            all.push(('+', b[j]));
            j += 1;
        }
    }
    let near = |k: usize| all[k.saturating_sub(context)..(k + context + 1).min(all.len())].iter().any(|(op, _)| *op != ' ');
    let mut out: Vec<DiffLine> = vec![];
    for (k, (op, text)) in all.iter().enumerate() {
        if *op != ' ' || near(k) {
            out.push(DiffLine { op: op.to_string(), text: text.to_string() });
        } else if out.last().is_none_or(|l| l.op != "…") {
            out.push(DiffLine { op: "…".into(), text: String::new() });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLI: &str = "/Users/me/.midna/bin/current/midna";

    #[test]
    fn claude_install_keeps_user_hooks_and_is_current() {
        let user = json!({ "model": "opus", "statusLine": { "type": "command", "command": "mine" }, "hooks": { "Stop": [{ "matcher": "", "hooks": [{ "type": "command", "command": "/u/stop" }] }] } });
        assert_eq!(claude_state(&user, CLI).0, "not_installed");
        let v = claude_with(&user, CLI);
        assert_eq!(claude_state(&v, CLI), ("current", None));
        assert_eq!(v["statusLine"]["command"], "mine", "the user's status line stays");
        assert_eq!(v["hooks"]["Stop"][0]["hooks"][0]["command"], "/u/stop");
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 2);
        // key order is kept
        assert_eq!(v.as_object().unwrap().keys().next().unwrap(), "model");
        // reinstalling doesn't duplicate
        assert_eq!(claude_with(&v, CLI), v);
        // uninstall gives back the original
        let mut u = v.clone();
        claude_strip(&mut u);
        assert_eq!(u, user);
    }

    #[test]
    fn claude_stale_when_path_moves_or_events_missing() {
        let v = claude_with(&json!({}), "/old/midna");
        let (st, detail) = claude_state(&v, CLI);
        assert_eq!(st, "stale");
        assert!(detail.unwrap().contains("older midna"));
        let mut v = claude_with(&json!({}), CLI);
        v["hooks"].as_object_mut().unwrap().remove("PermissionRequest");
        let (st, detail) = claude_state(&v, CLI);
        assert_eq!(st, "stale");
        assert!(detail.unwrap().contains("PermissionRequest"));
    }

    #[test]
    fn claude_command_is_inert_outside_midna() {
        let cmd = claude_command("/nonexistent/midna");
        let run = |envs: &[(&str, &str)]| {
            let mut c = std::process::Command::new("/bin/sh");
            c.arg("-c").arg(&cmd).env_remove("MIDNA_SESSION");
            for (k, v) in envs {
                c.env(k, v);
            }
            c.output().unwrap()
        };
        let o = run(&[]);
        assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty());
        // inside midna, a missing binary is skipped, not an error
        let o = run(&[("MIDNA_SESSION", "s1")]);
        assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty());
    }

    #[test]
    fn codex_wraps_and_restores_existing_notify() {
        let src = "approval_policy = \"never\"\nnotify = [\n    \"/apps/Other\",\n    \"turn-ended\",\n]\nmodel = \"x\"\n\n[desktop]\nmode = \"steer\"\n";
        let mut doc: toml_edit::DocumentMut = src.parse().unwrap();
        assert_eq!(codex_state(&doc, CLI).0, "not_installed");
        let prev = notify_of(&doc).unwrap();
        set_notify(&mut doc, Some(codex_argv(CLI, &prev)));
        assert_eq!(codex_state(&doc, CLI), ("current", None));
        let text = doc.to_string();
        assert!(text.starts_with("approval_policy"), "other keys keep their place: {text}");
        assert!(text.contains("[desktop]"));
        let argv = notify_of(&doc).unwrap();
        assert_eq!(codex_previous(&argv).unwrap(), prev);
        assert_eq!(codex_state(&doc, "/old/midna").0, "stale");
    }

    #[test]
    fn codex_wrapper_runs_previous_notify_and_skips_midna_outside() {
        let dir = std::env::temp_dir().join(format!("midna-codex-wrap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("prev.log");
        let prev = vec!["/bin/sh".to_string(), "-c".into(), format!("echo \"$@\" > {}", log.display()), "prev".into(), "turn-ended".into()];
        let argv = codex_argv("/nonexistent/midna", &prev);
        let o = std::process::Command::new(&argv[0]).args(&argv[1..]).arg(r#"{"type":"agent-turn-complete"}"#).env_remove("MIDNA_SESSION").output().unwrap();
        assert!(o.status.success(), "{o:?}");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), r#"turn-ended {"type":"agent-turn-complete"}"#);
        // with nothing to chain it is a quiet no-op
        let argv = codex_argv("/nonexistent/midna", &[]);
        let o = std::process::Command::new(&argv[0]).args(&argv[1..]).arg("{}").env("MIDNA_SESSION", "s1").output().unwrap();
        assert!(o.status.success() && o.stdout.is_empty() && o.stderr.is_empty(), "{o:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_marks_changes_with_context() {
        let d = diff("a\nb\nc\nd\ne\nf\ng\n", "a\nb\nc\nX\ne\nf\ng\n", 1);
        let ops: Vec<String> = d.iter().map(|l| format!("{}{}", l.op, l.text)).collect();
        assert_eq!(ops, ["…", " c", "-d", "+X", " e", "…"]);
    }
}
