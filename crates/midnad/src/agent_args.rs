//! The caller's own arguments for an agent midna launches (`session.open {agent_args}`, or a
//! `claude …` typed into a shell and adopted), put after midna's per-launch flags so neither
//! side loses anything.
//!
//! Each agent reads some options once, the last one winning, and midna already sets them:
//! - Claude `--settings` (midna's hooks): the caller's settings (a file, relative to the
//!   terminal's directory, or inline JSON) are merged into a copy of midna's file, written to
//!   `$MIDNA_HOME/hooks/merged/<midna's file name>-<hash>.json`, and that one file is passed.
//!   Objects merge key by key, lists are joined (so hooks and permission rules from both
//!   apply), and the caller's value wins anything else (`model`, `statusLine`).
//! - Claude `--append-system-prompt` (midna's hint): the caller's text, then midna's.
//! - Codex `-c developer_instructions=…`: joined the same way.
//! - Codex `-c notify=[…]` (one command): midna's notify becomes a wrapper that reports to
//!   midna and then runs the caller's command with the same arguments.
//!
//! Anything midna can't read (a missing file, a value that isn't TOML) is passed on as given,
//! so the agent reports the problem itself.
use crate::daemon::Daemon;
use midna_proto::AgentKind;
use midna_proto::agent_cli::{Spec, Word};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `$0` of the per-launch Codex notify wrapper (`mark_injected` looks for it).
pub const CODEX_LAUNCH_WRAPPER: &str = "midna-notify-launch";

/// Where merged Claude settings files go.
pub fn merged_dir(home: &Path) -> PathBuf {
    home.join("hooks").join("merged")
}

/// `base` (midna's command, no prompt) followed by `user`, with the options both set combined.
pub fn combine(d: &Daemon, agent: AgentKind, base: Vec<String>, user: &[String], spec: &Spec, cwd: &Path) -> Vec<String> {
    combine_in(&d.cfg.home, &d.cfg.cli_path, agent, base, user, spec, cwd)
}

pub fn combine_in(home: &Path, cli: &str, agent: AgentKind, mut base: Vec<String>, user: &[String], spec: &Spec, cwd: &Path) -> Vec<String> {
    let opts = options(spec, user);
    let mut rest = vec![];
    match agent {
        AgentKind::Claude => {
            let (mut settings, mut appends) = (vec![], vec![]);
            for o in opts {
                match (o.name.as_str(), o.value.clone()) {
                    ("--settings", Some(v)) => settings.push((v, o.raw)),
                    ("--append-system-prompt", Some(v)) => appends.push((v, o.raw)),
                    _ => rest.extend(o.raw),
                }
            }
            if !settings.is_empty() {
                match value_after(&base, "--settings").and_then(|i| merge_settings(home, &base[i], &settings, cwd).map(|p| (i, p))) {
                    Some((i, path)) => base[i] = path,
                    None => rest.extend(settings.into_iter().flat_map(|(_, raw)| raw)),
                }
            }
            if !appends.is_empty() {
                match value_after(&base, "--append-system-prompt") {
                    Some(i) => base[i] = join_texts(appends.iter().map(|(v, _)| v.as_str()), &base[i]),
                    None => rest.extend(appends.into_iter().flat_map(|(_, raw)| raw)),
                }
            }
        }
        AgentKind::Codex => {
            for o in opts {
                let kv = o.value.as_deref().filter(|_| o.name == "--config").and_then(|v| v.split_once('='));
                match kv.map(|(k, v)| (k.trim(), v)) {
                    Some(("developer_instructions", v)) => {
                        let mine = base.iter().position(|a| a.starts_with("developer_instructions="));
                        match (toml_value(v).and_then(|t| t.as_str().map(str::to_string)), mine) {
                            (Some(theirs), Some(i)) => {
                                let ours = toml_value(&base[i]["developer_instructions=".len()..]).and_then(|t| t.as_str().map(str::to_string)).unwrap_or_default();
                                base[i] = format!("developer_instructions={}", toml_str(&join_texts([theirs.as_str()], &ours)));
                            }
                            _ => rest.extend(o.raw),
                        }
                    }
                    Some(("notify", v)) => match toml_value(v).and_then(|t| t.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>())) {
                        Some(theirs) => {
                            let ours = codex_chained_notify(cli, &theirs);
                            match base.iter().position(|a| a.starts_with("notify=")) {
                                Some(i) => base[i] = ours,
                                // The global install covers Codex, but this override replaces it.
                                None => base.splice(1..1, ["-c".to_string(), ours]).for_each(drop),
                            }
                        }
                        None => rest.extend(o.raw),
                    },
                    _ => rest.extend(o.raw),
                }
            }
        }
    }
    base.extend(rest);
    base
}

/// One option (or positional word) of the caller's arguments, with the words it spans.
struct Opt {
    name: String,
    value: Option<String>,
    raw: Vec<String>,
}

fn options(spec: &Spec, args: &[String]) -> Vec<Opt> {
    let parsed = spec.parse(args);
    let mut out: Vec<Opt> = vec![];
    for (a, w) in args.iter().zip(&parsed.words) {
        match w {
            Word::Opt { name, values } => {
                // `--settings=x` (`--settings x` gets its value from the next word)
                let inline = a.split_once('=').filter(|_| a.starts_with("--") && *values == 0).map(|(_, v)| v.to_string());
                out.push(Opt { name: name.clone(), value: inline, raw: vec![a.clone()] });
            }
            Word::Value => {
                if let Some(o) = out.last_mut() {
                    o.value.get_or_insert_with(|| a.clone());
                    o.raw.push(a.clone());
                }
            }
            Word::Positional => out.push(Opt { name: String::new(), value: None, raw: vec![a.clone()] }),
        }
    }
    out
}

fn value_after(v: &[String], flag: &str) -> Option<usize> {
    v.iter().position(|a| a == flag).map(|i| i + 1).filter(|&i| i < v.len())
}

fn join_texts<'a>(theirs: impl IntoIterator<Item = &'a str>, ours: &str) -> String {
    let mut parts: Vec<&str> = theirs.into_iter().filter(|t| !t.trim().is_empty()).collect();
    if !ours.trim().is_empty() {
        parts.push(ours);
    }
    parts.join("\n\n")
}

/// Claude's `--settings` argument: inline JSON, or a file.
fn read_settings(arg: &str, cwd: &Path) -> Option<Value> {
    let text = if arg.trim_start().starts_with('{') {
        arg.to_string()
    } else {
        let p = match arg.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var("HOME").ok()?).join(rest),
            None => cwd.join(arg),
        };
        std::fs::read_to_string(p).ok()?
    };
    serde_json::from_str::<Value>(&text).ok().filter(Value::is_object)
}

/// Objects key by key, lists joined (without repeats), anything else replaced.
pub fn merge_json(into: &mut Value, from: Value) {
    match (into, from) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(x) => merge_json(x, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for v in b {
                if !a.contains(&v) {
                    a.push(v);
                }
            }
        }
        (x, v) => *x = v,
    }
}

/// midna's settings file plus the caller's, as one file. None when any of them can't be read.
fn merge_settings(home: &Path, ours: &str, theirs: &[(String, Vec<String>)], cwd: &Path) -> Option<String> {
    let mut v = read_settings(ours, cwd)?;
    for (t, _) in theirs {
        merge_json(&mut v, read_settings(t, cwd)?);
    }
    let body = serde_json::to_vec_pretty(&v).ok()?;
    let hash: String = sha2::Digest::finalize(<sha2::Sha256 as sha2::Digest>::new_with_prefix(&body)).iter().take(8).map(|b| format!("{b:02x}")).collect();
    let stem = Path::new(ours).file_stem().and_then(|s| s.to_str()).unwrap_or("claude-settings");
    let path = merged_dir(home).join(format!("{stem}-{hash}.json"));
    if !path.is_file() {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}

/// A merged settings file made from midna's full one (with hooks), not the hook-less base.
pub fn is_merged_with_hooks(home: &Path, arg: &str) -> bool {
    let p = Path::new(arg);
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    p.parent() == Some(merged_dir(home).as_path()) && name.starts_with("claude-settings-") && !name.starts_with("claude-settings-base-")
}

fn toml_value(v: &str) -> Option<toml_edit::Value> {
    v.trim().parse::<toml_edit::Value>().ok()
}

fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default() // JSON strings are TOML basic strings
}

/// `notify=[…]` that reports to midna, then runs `theirs` with the event JSON Codex appends.
pub fn codex_chained_notify(cli: &str, theirs: &[String]) -> String {
    let q = crate::hooks::sh_quote(cli);
    let script = format!(r#"for a; do last="$a"; done; if [ -x {q} ]; then {q} hook codex notify "$last" >/dev/null 2>&1; fi; if [ $# -gt 1 ]; then exec "$@"; fi"#);
    let mut argv = vec!["/bin/sh".to_string(), "-c".into(), script, CODEX_LAUNCH_WRAPPER.into()];
    argv.extend(theirs.iter().cloned());
    format!("notify={}", serde_json::to_string(&argv).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("midna-args-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("hooks")).unwrap();
        d
    }

    #[test]
    fn claude_settings_and_system_prompt_merge_into_midnas() {
        let home = tmp("claude");
        let ours = home.join("hooks/claude-settings.json");
        std::fs::write(&ours, json!({ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "midna hook claude" }] }] }, "permissions": { "allow": ["mcp__midna__guide"] } }).to_string()).unwrap();
        std::fs::write(home.join("board.json"), json!({ "model": "opus", "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "/board/stop" }] }] }, "permissions": { "allow": ["Bash(ls)"] } }).to_string()).unwrap();
        let base = v(&["claude", "--mcp-config", "m.json", "--settings", ours.to_str().unwrap(), "--append-system-prompt", "HINT"]);
        let user = v(&["--append-system-prompt", "You work card 12.", "--settings", "board.json", "--model", "sonnet", "--settings={\"env\":{\"A\":\"1\"}}"]);
        let out = combine_in(&home, "/bin/midna", AgentKind::Claude, base, &user, &Spec::builtin(AgentKind::Claude), &home);
        assert_eq!(out.iter().filter(|a| a.starts_with("--settings")).count(), 1, "{out:?}");
        assert_eq!(out.iter().filter(|a| *a == "--append-system-prompt").count(), 1, "{out:?}");
        assert_eq!(out[6], "You work card 12.\n\nHINT");
        assert_eq!(&out[7..], ["--model", "sonnet"]);
        assert!(is_merged_with_hooks(&home, &out[4]), "{}", out[4]);
        let merged: Value = serde_json::from_str(&std::fs::read_to_string(&out[4]).unwrap()).unwrap();
        let stops: Vec<&str> = merged["hooks"]["Stop"].as_array().unwrap().iter().map(|e| e["hooks"][0]["command"].as_str().unwrap()).collect();
        assert_eq!(stops, ["midna hook claude", "/board/stop"], "both hooks run");
        assert_eq!(merged["permissions"]["allow"], json!(["mcp__midna__guide", "Bash(ls)"]));
        assert_eq!((merged["model"].as_str(), merged["env"]["A"].as_str()), (Some("opus"), Some("1")));
        // A file that isn't there goes to Claude as given, which says so.
        let base = v(&["claude", "--settings", ours.to_str().unwrap()]);
        let out = combine_in(&home, "/bin/midna", AgentKind::Claude, base, &v(&["--settings", "nope.json"]), &Spec::builtin(AgentKind::Claude), &home);
        assert_eq!(&out[1..], ["--settings", ours.to_str().unwrap(), "--settings", "nope.json"]);
        // From the hook-less base (global install): not injected.
        assert!(!is_merged_with_hooks(&home, &merged_dir(&home).join("claude-settings-base-0011.json").to_string_lossy()));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_instructions_join_and_notify_chains() {
        let home = tmp("codex");
        let ours = crate::hooks::codex_notify_arg("/bin/midna");
        let base = v(&["codex", "-c", &ours, "-c", "developer_instructions=\"HINT\""]);
        let user = v(&["-c", "developer_instructions=\"Card 12\"", "--config=notify=[\"/board/notify\", \"x\"]", "-m", "o3"]);
        let out = combine_in(&home, "/bin/midna", AgentKind::Codex, base, &user, &Spec::builtin(AgentKind::Codex), &home);
        assert_eq!(out[4], "developer_instructions=\"Card 12\\n\\nHINT\"");
        let notify: Vec<String> = serde_json::from_str(out[2].strip_prefix("notify=").unwrap()).unwrap();
        assert_eq!((notify[3].as_str(), &notify[4..]), (CODEX_LAUNCH_WRAPPER, &v(&["/board/notify", "x"])[..]));
        assert_eq!(&out[5..], ["-m", "o3"]);
        // The wrapper runs the caller's command with the event JSON.
        let log = home.join("notify.log");
        let theirs = v(&["/bin/sh", "-c", &format!("echo \"$@\" > {}", log.display()), "prev", "turn"]);
        let argv: Vec<String> = serde_json::from_str(codex_chained_notify("/nonexistent/midna", &theirs).strip_prefix("notify=").unwrap()).unwrap();
        let o = std::process::Command::new(&argv[0]).args(&argv[1..]).arg("{}").env_remove("MIDNA_SESSION").output().unwrap();
        assert!(o.status.success(), "{o:?}");
        assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "turn {}");
        // No notify of midna's (the global install covers Codex): the override still reports.
        let out = combine_in(&home, "/bin/midna", AgentKind::Codex, v(&["codex"]), &v(&["-c", "notify=[\"/n\"]"]), &Spec::builtin(AgentKind::Codex), &home);
        assert_eq!(out[1], "-c");
        assert!(out[2].contains(CODEX_LAUNCH_WRAPPER) && out.len() == 3, "{out:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
