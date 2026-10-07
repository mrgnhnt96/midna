//! `midna mcp`: a stdio MCP server (newline-delimited JSON-RPC 2.0, MCP 2025-06-18) exposing
//! every catalog method as a tool, plus three guide tools (`capabilities`, `explain`, `guide`)
//! and the guide as a resource (`midna://skill`).
//!
//! - Tool names replace `.` with `_` (MCP clients accept only `[A-Za-z0-9_-]`).
//! - Descriptions are written for an agent and say what is human only and what a refusal means.
//! - Daemon refusals come back as tool results with `isError: true` and the next step to take
//!   (they are not protocol errors: the model should read them). Unknown tools, missing or
//!   non-object arguments are JSON-RPC `-32602`; unknown methods `-32601`; bad JSON `-32700`.
use crate::Res;
use crate::guide::{self, Surface};
use midna_proto::{Client, ClientError, MethodSpec, RpcError, catalog};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

/// Methods that need a dedicated streaming connection and can't work as one-shot tools.
/// `secret.exec_env` returns values; only `midna secret exec` may call it, never a tool.
const SKIP: &[&str] = &["stream.attach", "events.subscribe", "secret.exec_env"];

/// Protocol versions we speak, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "midna is the AI-managed terminal you are running in (if MIDNA_SESSION is set). These tools \
drive it: list/read/open terminals (session_*), get the human's attention (needs_you_raise), test and add policy rules \
(policy_check, rule_add), draft webhook triggers or add local hook/event/idle triggers (trigger_add), change settings (settings_set), and move the GUI \
(window_command). Start with the `capabilities` tool; `explain` answers \"why\" about any id; `guide` is the full guide. \
Human-only actions (removing rules, secrets, enabling webhook triggers, human-only settings, project_remove, daemon_stop) are \
never done for you: calling them asks the human and returns an error with the needs-you id and the next step. Never \
route around a refusal.";

pub fn tool_name(method: &str) -> String {
    method.replace('.', "_")
}

/// Rewrite dotted method names in a description to their tool names (`rule.request_removal`
/// → `rule_request_removal`), leaving longer words such as event kinds alone.
fn toolify(text: &str) -> String {
    let mut out = text.to_string();
    let mut names: Vec<&str> = catalog().iter().map(|m| m.name).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    for n in names {
        let mut res = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(n) {
            let before = rest[..i].chars().last();
            let mut after = rest[i + n.len()..].chars();
            let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            let (a1, a2) = (after.next(), after.next());
            // `.` continues the word only when more of it follows (`session.input_by_agent`, not a full stop).
            let joined_after = word(a1) || (a1 == Some('.') && word(a2));
            res.push_str(&rest[..i]);
            res.push_str(&if word(before) || before == Some('.') || joined_after { n.to_string() } else { tool_name(n) });
            rest = &rest[i + n.len()..];
        }
        res.push_str(rest);
        out = res;
    }
    out
}

/// Extra, MCP-specific notes per method: when to use it and what a refusal means.
fn notes(m: &MethodSpec) -> Option<&'static str> {
    Some(match m.name {
        "rule.remove" => "Agents: this is refused. Use rule_request_removal {id, reason}; the human sees it and decides.",
        "trigger.set_secret" => "Agents: never call this; the secret is discarded and the human is asked to paste it in the GUI.",
        "secret.set" => "Prefer piping in a shell (`<command> | midna secret save NAME`): a value you pass here is already in \
            your context, so the secret is marked exposed.",
        "secret.replace" => "Agents: never call this; call secret_set, which asks the human when it would replace their secret.",
        "trigger.add" | "trigger.update" | "trigger.list" | "trigger.test" => "Local triggers (source local) fire on this Mac and \
            act on the terminal that fired. event: hook.<HookEvent> (hook.Stop, hook.Notification, …), a midna event kind \
            (agent.prompt_blocked data {hook, message, prompt}, agent.turn_ended, session.status, …), idle (with \
            filter.idle_minutes) or schedule (filter.cron: 5 fields in local time or @daily etc.; without a session/project/agent \
            filter it fires once about no terminal; also filter.window {from, until} HH:MM local, until excluded, starts_at, \
            ends_at (RFC 3339) and max_runs (1 = once)); globs ok. filter: session, project, agent, idle_minutes, cron, match {dotted.path: glob} \
            (case-insensitive). action: {kind:send_to_session, steps:[{text, enter:true}]} (in order, each waits for the agent) | \
            {kind:set_status, label, color, icon?, base, clear_on: prompt|turn|status|never} | {kind:clear_status} | {kind:notify, title, body?, sound:true} | attention | \
            run_command | start_agent. Templates: {{last_prompt}} {{event}} {{session.id|name|project_id|agent|status}} {{data.<path>}} or bare {{<path>}}. \
            cooldown_secs defaults to 60 per terminal; events triggers cause never fire triggers. enabled:true (add, local only) or \
            trigger_set_enabled turns a local trigger on with no approval; do it when the human asked for the trigger. trigger_test takes \
            session for local triggers. The `guide` tool (section “Local triggers”) has worked examples.",
        "needs_you.resolve" => "Agents normally don't call this: answering is the human's job. Refused unless the human enabled \
            approve.from_cli, and then only for your own session's approvals.",
        "policy.request" => "Blocks until the human answers (up to timeout_secs). Prefer policy_check to just test an action. \
            Add no_wait: true (works on any tool) to get the needs-you id back at once (error 6) and follow it with needs_you_get.",
        "session.close" => "Closing a working terminal or another one may ask the human and block meanwhile; add no_wait: true \
            to get the needs-you id back at once (error 6): the close then happens if they approve, and needs_you_get \
            {id, wait_secs} tells you how it went.",
        "agent.hook" => "Internal: midna's hook bridge calls this. Agents don't need it.",
        "session.input" => "To message another agent, send the text with enter=true; add images as absolute paths in `images` \
            (e.g. a screenshot you saved). To press special keys use session_key. Read the result with session_read.",
        "needs_you.raise" => "Etiquette: one short line, only when blocked (kind=blocked) or the human must know (kind=note). \
            Check needs_you_list first to avoid duplicates.",
        "settings.set" | "settings.reset" => "settings_list shows which keys are human_only; for those this asks the human \
            (error 2 with the needs-you id) instead of changing anything.",
        "window.command" => "Refused for agents unless the human-only setting agents.may_move_windows is on (front and \
            open_screen are always allowed).",
        _ => return None,
    })
}

fn description(m: &MethodSpec) -> String {
    let mut d = toolify(m.description);
    if let Some(n) = notes(m) {
        d.push(' ');
        d.push_str(n);
    }
    if m.human_only && !["rule.remove", "trigger.set_secret", "secret.replace"].contains(&m.name) {
        d.push_str(
            " HUMAN ONLY: calling this does not do it. midna asks the human (a needs-you approval) and returns an error with \
             the needs-you id; if they approve, midna runs it for them. Tell the user, don't retry or work around it.",
        );
    }
    if m.stub {
        d.push_str(" (Not implemented yet.)");
    }
    d
}

fn input_schema(m: &MethodSpec) -> Value {
    let mut schema = (m.params)();
    if let Some(o) = schema.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
        o.insert("type".into(), json!("object"));
        o.entry("properties").or_insert(json!({}));
    }
    schema
}

fn destructive(name: &str) -> bool {
    [".close", ".remove", ".stop", ".upgrade", ".restart", ".reset", "set_secret"].iter().any(|s| name.ends_with(s))
}

/// The guide tools that aren't catalog methods.
fn extra_tools() -> Vec<Value> {
    let ro = json!({ "readOnlyHint": true, "openWorldHint": false });
    vec![
        json!({
            "name": "capabilities",
            "title": "midna capabilities",
            "description": "Start here. A short overview of everything midna lets you do (terminals, attention, approvals, rules, \
                triggers, settings, windows), what is human only and what to do instead, plus every method with its flags.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        }),
        json!({
            "name": "explain",
            "title": "midna explain",
            "description": "Explain one thing in plain words: a terminal id (why it has its status, recent status events, what \
                it waits on), a rule r_… (what it matches, who added it, when it fired, how to get it removed), a trigger t_… \
                (what it does, what it waits for), a needs-you item n_…, a project p_…, a delivery d_…, a method or tool name, a \
                setting key, or a topic (status, rules, approvals, triggers, needs-you, settings, scripts, themes, usage, windows, human-only, mcp). \
                With `value` set, `target` is an action kind (command|tool|path|cli|window) and it explains which rule decides \
                that action.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "description": "An id, method, setting key, topic, or an action kind when value is set." },
                    "value": { "type": "string", "description": "The action value to check, e.g. `git push --force` for target=command." }
                },
                "required": ["target"]
            },
            "annotations": ro,
        }),
        json!({
            "name": "guide",
            "title": "midna agent guide",
            "description": "The full midna agent guide (SKILL.md): when to use which tool, attention etiquette, approvals, rules, \
                triggers, settings, windows, and never routing around a denial.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        }),
    ]
}

pub fn tools() -> Vec<Value> {
    let mut list = extra_tools();
    list.extend(catalog().iter().filter(|m| !SKIP.contains(&m.name)).map(|m| {
        json!({
            "name": tool_name(m.name),
            "title": m.name,
            "description": description(m),
            "inputSchema": input_schema(m),
            "annotations": {
                "readOnlyHint": !m.mutating,
                "destructiveHint": m.mutating && destructive(m.name),
                "idempotentHint": !m.mutating,
                "openWorldHint": false,
            },
        })
    }));
    list
}

fn method_for(tool: &str) -> Option<&'static str> {
    catalog().iter().find(|m| tool_name(m.name) == tool && !SKIP.contains(&m.name)).map(|m| m.name)
}

fn text_result(text: String, structured: Option<Value>, is_error: bool) -> Value {
    let mut r = json!({ "content": [{ "type": "text", "text": text }], "isError": is_error });
    if let Some(s) = structured {
        r["structuredContent"] = s;
    }
    r
}

/// One RPC call on the lazily (re)connected client, so the server survives daemon restarts.
fn rpc(client: &mut Option<Client>, method: &str, params: Value) -> Result<Value, RpcError> {
    for _ in 0..2 {
        if client.is_none() {
            *client = Client::connect_default().ok();
        }
        let Some(c) = client.as_mut() else { break };
        match c.call_value(method, params.clone()) {
            Ok(v) => return Ok(v),
            Err(ClientError::Io(_)) => {
                *client = None;
                continue;
            }
            Err(ClientError::Rpc(e)) => return Err(e),
            Err(ClientError::Decode(e)) => return Err(RpcError::internal(e)),
        }
    }
    Err(RpcError::internal(format!(
        "midnad is not reachable at {}. Is the midna app running? (`midna info` checks)",
        midna_proto::paths::socket_path().display()
    )))
}

fn error_result(e: &RpcError) -> Value {
    let mut text = format!("{} (midna error {})", e.message, e.code);
    if let Some(d) = &e.data {
        text.push_str(&format!("\n{d}"));
    }
    text_result(text, Some(json!({ "error": e })), true)
}

fn call_tool(client: &mut Option<Client>, params: &Value) -> Result<Value, Value> {
    let tool = params.get("name").and_then(Value::as_str).ok_or_else(|| json!({ "code": -32602, "message": "tools/call needs params.name" }))?;
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(a) if a.is_object() => a.clone(),
        Some(_) => return Err(json!({ "code": -32602, "message": "tools/call arguments must be an object" })),
    };
    match tool {
        "capabilities" => return Ok(text_result(guide::capabilities(), Some(guide::capabilities_json()), false)),
        "guide" => return Ok(text_result(guide::SKILL_MD.to_string(), None, false)),
        "explain" => {
            let mut words: Vec<String> = args.get("target").and_then(Value::as_str).map(|t| vec![t.to_string()]).unwrap_or_default();
            if let Some(v) = args.get("value").and_then(Value::as_str) {
                words.push(v.to_string());
            }
            let mut caller = |m: &str, p: Value| rpc(client, m, p);
            return Ok(match guide::explain(&mut caller, &words, Surface::Mcp) {
                Ok(t) => text_result(t, None, false),
                Err(e) => error_result(&e),
            });
        }
        _ => {}
    }
    let Some(method) = method_for(tool) else {
        return Err(json!({ "code": -32602, "message": format!("unknown tool `{tool}`; tools/list lists them (or call the `capabilities` tool)") }));
    };
    // `no_wait: true` on any tool: don't block on an approval (caller.no_wait), on a connection of its own.
    let mut args = args;
    let no_wait = args.as_object_mut().and_then(|o| o.remove("no_wait")).and_then(|v| v.as_bool()) == Some(true);
    let res = if no_wait {
        let mut once = Client::connect_default().ok().map(Client::no_wait);
        rpc(&mut once, method, args.clone())
    } else {
        rpc(client, method, args.clone())
    };
    Ok(match res {
        Ok(v) => text_result(
            serde_json::to_string_pretty(&v).unwrap_or_default(),
            Some(if v.is_object() { v } else { json!({ "result": v }) }),
            false,
        ),
        Err(e) => error_result(&guide::with_next_step(method, &args, e, Surface::Mcp)),
    })
}

fn negotiate(requested: Option<&str>) -> &'static str {
    requested.and_then(|r| VERSIONS.iter().find(|v| **v == r)).copied().unwrap_or(VERSIONS[0])
}

pub fn handle(client: &mut Option<Client>, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response to us (we never send requests) or garbage.
        if msg.get("result").is_some() || msg.get("error").is_some() {
            return None;
        }
        return Some(json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": { "code": -32600, "message": "invalid request: no method" } }));
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result: Result<Value, Value> = match method {
        "initialize" => Ok(json!({
            "protocolVersion": negotiate(params.get("protocolVersion").and_then(Value::as_str)),
            "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false, "subscribe": false } },
            "serverInfo": { "name": "midna", "title": "midna terminal", "version": midna_proto::VERSION },
            "instructions": INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(client, &params),
        "resources/list" => Ok(json!({ "resources": [{
            "uri": "midna://skill", "name": "midna-skill", "title": "midna agent guide",
            "description": "How to use midna as an agent (SKILL.md).", "mimeType": "text/markdown",
        }] })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "resources/read" => match params.get("uri").and_then(Value::as_str) {
            Some("midna://skill") => Ok(json!({ "contents": [{ "uri": "midna://skill", "mimeType": "text/markdown", "text": guide::SKILL_MD }] })),
            Some(u) => Err(json!({ "code": -32002, "message": format!("resource not found: {u}") })),
            None => Err(json!({ "code": -32602, "message": "resources/read needs params.uri" })),
        },
        m if m.starts_with("notifications/") => return None,
        _ => Err(json!({ "code": -32601, "message": format!("method not found: {method}") })),
    };
    let id = id?; // notifications get no response
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
    })
}

pub fn run() -> Res {
    let mut client: Option<Client> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(_)) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "batches are not supported" } })),
            Ok(msg) if msg.is_object() => handle(&mut client, &msg),
            Ok(_) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "invalid request" } })),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(r) = reply {
            let _ = writeln!(stdout, "{r}");
            let _ = stdout.flush();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_valid_and_unique() {
        let tools = tools();
        let mut seen = std::collections::HashSet::new();
        for t in &tools {
            let n = t["name"].as_str().unwrap();
            assert!(!n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'), "bad name {n}");
            assert!(seen.insert(n.to_string()), "duplicate {n}");
            assert_eq!(t["inputSchema"]["type"], "object", "{n}");
            assert!(t["inputSchema"]["properties"].is_object(), "{n}");
            assert!(t["description"].as_str().unwrap().len() > 20, "{n} needs a real description");
        }
    }

    #[test]
    fn descriptions_use_tool_names() {
        let t = tools();
        let rr = t.iter().find(|t| t["name"] == "rule_remove").unwrap()["description"].as_str().unwrap().to_string();
        assert!(rr.contains("rule_request_removal") && !rr.contains("rule.request_removal"), "{rr}");
        let ps = t.iter().find(|t| t["name"] == "project_remove").unwrap()["description"].as_str().unwrap().to_string();
        assert!(ps.contains("HUMAN ONLY") && ps.contains("asks the human"), "{ps}");
        // Event kinds that merely start with a method name are left alone.
        assert_eq!(toolify("logged as session.input_by_agent; see session.input."), "logged as session.input_by_agent; see session_input.");
    }

    #[test]
    fn schemas_resolve_their_refs() {
        for t in tools() {
            let s = t["inputSchema"].to_string();
            for part in s.split("\"$ref\":\"#/$defs/").skip(1) {
                let name: String = part.chars().take_while(|c| *c != '"').collect();
                assert!(t["inputSchema"]["$defs"].get(&name).is_some(), "{}: missing $defs/{name}", t["name"]);
            }
        }
    }
}
