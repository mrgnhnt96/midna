//! `midna hook <claude|codex> [event]`: the agent hook bridge.
//!
//!   claude: hook JSON on stdin -> agent.hook. For PreToolUse also policy.request, printing
//!           Claude's decision JSON (allow/deny/ask) only when midna has an opinion.
//!   claude statusline: reports cost, prints a short status line.
//!   codex notify '<json>': Codex's legacy notify (payload is the last argv).
//!
//! Never breaks the agent: every failure exits 0 without output. `--global` marks the entries
//! `midna hooks install` writes into the agents' global config.
use crate::args::Args;
use crate::{Fail, Res};
use midna_proto::Client;
use serde_json::{Value, json};
use std::io::Read;
use std::time::Duration;

pub fn run(a: &Args) -> Res {
    let agent = a.need(1, "agent (claude|codex)")?.to_string();
    if !matches!(agent.as_str(), "claude" | "codex") {
        return Err(Fail::Usage(format!("unknown agent `{agent}`")));
    }
    let arg_event = a.pos.get(2).cloned();
    let raw = if arg_event.as_deref() == Some("notify") {
        a.pos.last().cloned().unwrap_or_default()
    } else {
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    };
    let payload: Value = serde_json::from_str(&raw).unwrap_or(Value::String(raw.clone()));
    let event = match arg_event.as_deref() {
        Some("statusline") => "statusline".to_string(),
        other => payload
            .get("hook_event_name")
            .or(payload.get("type"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or(other.map(str::to_string))
            .unwrap_or_else(|| "unknown".into()),
    };
    // Outside a midna terminal there is nothing to report to. `--global` = the entry
    // `hooks.install` put in the agent's global config: it stands down when midna already
    // added its own hooks to this agent (they would report everything twice).
    let in_midna = std::env::var("MIDNA_SESSION").is_ok_and(|s| !s.is_empty());
    let injected = std::env::var("MIDNA_HOOKS_INJECTED").is_ok_and(|s| !s.is_empty());
    if a.has("global") && injected {
        return Ok(());
    }
    let client = if in_midna { Client::connect_default().ok() } else { None };
    if event == "statusline" {
        if let Some(mut c) = client {
            let _ = c.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = c.call_value("agent.hook", json!({ "agent": agent, "event": event, "payload": payload }));
        }
        println!("{}", status_line(&payload));
        return Ok(());
    }
    let Some(mut c) = client else { return Ok(()) };
    let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
    // agent.hook fails when midnad doesn't place this process in a midna terminal (e.g. an app
    // started from one inherited MIDNA_SESSION): then midna's policy must not touch it either.
    let reported = c.call_value("agent.hook", json!({ "agent": agent, "event": event, "payload": payload })).is_ok();
    if reported && agent == "claude" && event == "PreToolUse" {
        // policy.request may block while the human decides.
        let _ = c.set_read_timeout(None);
        let action = json!({ "kind": "tool", "value": tool_value(&payload) });
        if let Ok(r) = c.call_value("policy.request", json!({ "action": action }))
            && let Some(out) = claude_decision(&r) {
                println!("{out}");
            }
    }
    Ok(())
}

/// `Bash(git push)`, `Edit(/path/file.rs)`, `WebFetch(https://…)`, or just the tool name.
pub fn tool_value(p: &Value) -> String {
    let name = p.get("tool_name").and_then(Value::as_str).unwrap_or("unknown");
    let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
    let arg = ["command", "file_path", "notebook_path", "path", "url", "pattern", "query"]
        .iter()
        .find_map(|k| input.get(k).and_then(Value::as_str));
    match arg {
        Some(a) => format!("{name}({a})"),
        None => name.to_string(),
    }
}

/// Claude PreToolUse output, or None when midna has no opinion (Claude's own flow decides).
pub fn claude_decision(r: &Value) -> Option<Value> {
    let decision = r.get("decision")?.as_str()?;
    let source = r.get("source").and_then(Value::as_str).unwrap_or("");
    let permission = match (decision, source) {
        (_, "default") => return None,
        ("allow", _) => "allow",
        ("deny", _) => "deny",
        _ => "ask",
    };
    let reason = r.get("reason").and_then(Value::as_str).unwrap_or("midna policy");
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": permission,
            "permissionDecisionReason": format!("midna: {reason}"),
        }
    }))
}

fn status_line(p: &Value) -> String {
    let model = p.pointer("/model/display_name").and_then(Value::as_str).unwrap_or("claude");
    match p.pointer("/cost/total_cost_usd").and_then(Value::as_f64) {
        Some(c) => format!("{model} · ${c:.2}"),
        None => model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(tool_value(&json!({"tool_name": "Bash", "tool_input": {"command": "ls -la"}})), "Bash(ls -la)");
        assert_eq!(tool_value(&json!({"tool_name": "TodoWrite", "tool_input": {}})), "TodoWrite");
    }

    #[test]
    fn decisions() {
        assert!(claude_decision(&json!({"decision": "allow", "source": "default"})).is_none());
        let d = claude_decision(&json!({"decision": "deny", "source": "rule", "reason": "rule r_1"})).unwrap();
        assert_eq!(d["hookSpecificOutput"]["permissionDecision"], "deny");
        let d = claude_decision(&json!({"decision": "ask", "source": "timeout"})).unwrap();
        assert_eq!(d["hookSpecificOutput"]["permissionDecision"], "ask");
    }
}
