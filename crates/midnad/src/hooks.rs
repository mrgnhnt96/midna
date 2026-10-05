//! Agent wiring that midna owns (never the user's global config): the Claude settings file
//! passed with `claude --settings`, the MCP config passed with `claude --mcp-config`, the
//! bundled agent guide (`MIDNA_SKILL`), the short system hint, and the Codex `-c` overrides.
use crate::daemon::Daemon;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// `Stop`/`SubagentStop` carry Claude's snapshot of background work and scheduled wakeups, and
/// `SubagentStart`/`SubagentStop` bracket subagents (see `agent_work`).
///
/// Claude Code 2.1.288 does not send `Notification`/`permission_prompt` when its permission
/// dialog opens (verified live and in spikes/agent-status); `PermissionRequest` is the hook that
/// fires. `PostToolUseFailure`, `PermissionDenied` and `StopFailure` keep the status honest when a
/// tool fails, a prompt is denied, or a turn dies on an API error.
pub(crate) const CLAUDE_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionDenied",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "SessionEnd",
];
/// Tool events take a matcher; `*` matches every tool.
pub(crate) const CLAUDE_TOOL_EVENTS: &[&str] = &["PreToolUse", "PermissionRequest", "PostToolUse", "PostToolUseFailure", "PermissionDenied"];

pub fn claude_settings_path(home: &Path) -> PathBuf {
    home.join("hooks").join("claude-settings.json")
}

/// The same without `hooks`: for agents midna starts while the global install (see
/// `global_hooks`) already reports for them.
pub fn claude_base_settings_path(home: &Path) -> PathBuf {
    home.join("hooks").join("claude-settings-base.json")
}

/// `$MIDNA_HOME/hooks/mcp.json`: registers `midna mcp` as the stdio MCP server `midna`.
pub fn mcp_config_path(home: &Path) -> PathBuf {
    home.join("hooks").join("mcp.json")
}

/// `$MIDNA_HOME/hooks/SKILL.md`: the bundled agent guide (also `midna skill`); every terminal
/// gets its path as `MIDNA_SKILL`.
pub fn skill_path(home: &Path) -> PathBuf {
    home.join("hooks").join("SKILL.md")
}

/// The guide agents read. The source of truth lives with the CLI (`midna skill` prints it).
pub const SKILL_MD: &str = include_str!("../../midna-cli/assets/SKILL.md");

/// Read-only midna MCP tools Claude may call without its own permission prompt. Everything
/// that acts still goes through Claude's normal permission flow and midna's own policy.
const CLAUDE_MCP_READ_ONLY: &[&str] = &[
    "capabilities", "explain", "guide", "rpc_discover", "daemon_info", "project_list", "project_discover", "session_list", "session_get", "session_read", "session_prompts",
    "needs_you_list", "policy_check", "rule_list", "trigger_list", "trigger_deliveries", "trigger_test", "webhooks_status",
    "settings_list", "settings_get", "insights_summary", "insights_series", "insights_activity", "events_list", "window_list", "links_list", "queue_list", "session_subagent_log",
];

/// Two or three lines appended to the agent's system prompt (setting `agents.system_hint`).
pub fn system_hint(mcp: bool) -> String {
    let tools = if mcp { " The `midna` MCP tools do the same as the CLI." } else { "" };
    format!(
        "You are running inside a midna terminal (midna = an AI-managed terminal; the human watches it). \
         Run `midna capabilities` to see what you can do here (terminals, attention, approvals, rules, triggers, settings, windows); \
         the full guide is `midna skill` (file $MIDNA_SKILL).{tools} \
         `[secret:NAME]` is a stored secret midna keeps out of your context: use it with `midna secret exec NAME -- <command>` ($NAME is set there); save a token a command produced with `<command> | midna secret save NAME`. Never ask for, echo or print a secret's value. \
         Human-only actions become requests the human sees: when midna refuses or denies something, never route around it."
    )
}

pub fn mcp_config(cli: &str) -> Value {
    json!({ "mcpServers": { "midna": { "type": "stdio", "command": cli, "args": ["mcp"] } } })
}

/// `-c` overrides that give Codex the midna MCP server. Codex starts stdio servers with a
/// minimal environment, so the session's identity is passed explicitly.
pub fn codex_mcp_args(cli: &str, env: &[(String, String)]) -> Vec<String> {
    let toml_str = |s: &str| serde_json::to_string(s).unwrap_or_default(); // JSON strings are TOML basic strings
    let env: Vec<String> = env
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "MIDNA_SESSION" | "MIDNA_PROJECT" | "MIDNA_SOCKET" | "MIDNA_HOME"))
        .map(|(k, v)| format!("{k}={}", toml_str(v)))
        .collect();
    vec![
        "-c".into(),
        format!("mcp_servers.midna.command={}", toml_str(cli)),
        "-c".into(),
        r#"mcp_servers.midna.args=["mcp"]"#.into(),
        "-c".into(),
        format!("mcp_servers.midna.env={{{}}}", env.join(",")),
    ]
}

pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn claude_settings(cli: &str, statusline: bool) -> Value {
    let cmd = format!("{} hook claude", sh_quote(cli));
    let mut hooks = serde_json::Map::new();
    for ev in CLAUDE_EVENTS {
        let mut h = json!({ "type": "command", "command": cmd });
        // PreToolUse may wait for a human approval (policy.request_timeout_secs, default 300s).
        if *ev == "PreToolUse" {
            h["timeout"] = json!(600);
        }
        let mut entry = json!({ "hooks": [h] });
        if CLAUDE_TOOL_EVENTS.contains(ev) {
            entry["matcher"] = json!("*");
        }
        hooks.insert(ev.to_string(), json!([entry]));
    }
    let allow: Vec<String> = CLAUDE_MCP_READ_ONLY.iter().map(|t| format!("mcp__midna__{t}")).collect();
    let mut v = json!({ "hooks": hooks, "permissions": { "allow": allow } });
    if statusline {
        v["statusLine"] = json!({ "type": "command", "command": format!("{cmd} statusline"), "padding": 0 });
    }
    v
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let tmp = path.with_extension("tmp");
    let ok = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if let Err(e) = ok {
        eprintln!("midnad: writing {} failed: {e}", path.display());
    }
}

/// (Re)write the agent files under $MIDNA_HOME/hooks/: claude-settings.json (and its -base
/// twin without hooks), mcp.json, SKILL.md.
pub fn write_claude_settings(d: &Daemon) {
    let statusline = d.core().state.setting_bool("agents.claude.statusline");
    let mut v = claude_settings(&d.cfg.cli_path, statusline);
    write_atomic(&claude_settings_path(&d.cfg.home), &serde_json::to_vec_pretty(&v).unwrap_or_default());
    if let Some(o) = v.as_object_mut() {
        o.remove("hooks");
    }
    write_atomic(&claude_base_settings_path(&d.cfg.home), &serde_json::to_vec_pretty(&v).unwrap_or_default());
    write_atomic(&mcp_config_path(&d.cfg.home), &serde_json::to_vec_pretty(&mcp_config(&d.cfg.cli_path)).unwrap_or_default());
    write_atomic(&skill_path(&d.cfg.home), SKILL_MD.as_bytes());
}

/// `notify=[...]` for `codex -c`: Codex appends the event JSON as the last argv.
pub fn codex_notify_arg(cli: &str) -> String {
    format!("notify={}", json!([cli, "hook", "codex", "notify"]))
}
