//! `midna mcp`: a stdio MCP server (newline-delimited JSON-RPC 2.0, MCP 2025-06-18) exposing
//! every catalog method as a tool, plus three guide tools (`capabilities`, `explain`, `guide`)
//! and the guide as resources (`midna://skill`, `midna://skill/<topic>`).
//!
//! - `midna mcp --compact` lists seven tools instead (the three guide tools, `search_methods`,
//!   `describe_method`, `call_read` and `call`) for clients that load every schema up front.
//! - Tool names replace `.` with `_` (MCP clients accept only `[A-Za-z0-9_-]`).
//! - Descriptions are written for an agent and say what is human only and what a refusal means.
//! - A result is one text block, sent once: `content` is the field every client and protocol
//!   version reads. Tools declare no `outputSchema`, so there is no `structuredContent` copy.
//! - `session_list` is a summary of each terminal; `session_get` has the rest.
//! - Daemon refusals come back as tool results with `isError: true` and the next step to take
//!   (they are not protocol errors: the model should read them). Unknown tools, missing or
//!   non-object arguments are JSON-RPC `-32602`; unknown methods `-32601`; bad JSON `-32700`.
use crate::Res;
use crate::guide::{self, Surface};
use midna_proto::{Client, ClientError, MethodSpec, RpcError, catalog};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};

/// Methods that need a dedicated streaming connection and can't work as one-shot tools.
/// `secret.exec_env` returns values; only `midna secret exec` may call it, never a tool.
const SKIP: &[&str] = &["stream.attach", "events.subscribe", "secret.exec_env"];

/// Protocol versions we speak, newest first.
const VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// What an agent's terminal listing keeps of each `Session`. A keep-list, so a bulky field added
/// later stays out until someone chooses it. The app reads the full `session.list` RPC result.
const LISTED: &[&str] = &[
    "id", "project_id", "name", "kind", "agent", "cwd", "pid", "title", "status", "created_at", "last_activity_at", "keep_on_top",
    "background", "git", "custom_status", "queue_paused", "paused",
];

/// The `agent_info` fields a listing keeps.
const LISTED_AGENT_INFO: &[&str] = &["model", "permission_mode"];

/// The longest string a listing shows, unless the entry needs less to fit `LISTED_BYTES`.
const LISTED_TEXT: usize = 120;

/// A listed terminal stays under this, as JSON.
const LISTED_BYTES: usize = 1024;

const INSTRUCTIONS: &str = "midna is the AI-managed terminal you are running in (if MIDNA_SESSION is set). These tools \
drive it: list/read/open terminals (session_*), get the human's attention (needs_you_raise), test and add policy rules \
(policy_check, rule_add), draft webhook triggers or add local hook/event/idle triggers (trigger_add), change settings (settings_set), and move the GUI \
(window_command). Start with the `capabilities` tool; `explain` answers \"why\" about any id; `guide` has the core rules \
and lists its topics. \
Human-only actions (removing rules, secrets, enabling webhook triggers, human-only settings, project_remove, daemon_stop) are \
never done for you: calling them asks the human and returns an error with the needs-you id and the next step. Never \
route around a refusal.";

/// Which tool listing the server offers: one tool per method, or the compact discovery set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Full,
    Compact,
}

impl Mode {
    fn surface(self) -> Surface {
        match self {
            Mode::Full => Surface::Mcp,
            Mode::Compact => Surface::McpCompact,
        }
    }
}

fn compact_instructions() -> String {
    let mut groups: Vec<&str> = catalog().iter().filter(|m| !SKIP.contains(&m.name)).map(|m| m.group()).collect();
    groups.sort_unstable();
    groups.dedup();
    format!(
        "midna is the AI-managed terminal you are running in (if MIDNA_SESSION is set). Its methods are reached through four \
tools: `search_methods` finds one by words or group, `describe_method` shows its params and rules, `call_read` runs a \
read-only method and `call` runs one that changes state. Groups: {}. Start with the `capabilities` tool; `explain` \
answers \"why\" about any id; `guide` has the core rules and lists its topics. Human-only actions (removing rules, secrets, enabling webhook \
triggers, human-only settings, project.remove, daemon.stop) are never done for you: calling them asks the human and \
returns an error with the needs-you id and the next step. Never route around a refusal.",
        groups.join(", ")
    )
}

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

/// The reverse of `toolify`, for compact mode where per-method tools don't exist: tool names in
/// the notes (`rule_request_removal`) go back to method names (`rule.request_removal`).
fn dotted(text: &str) -> String {
    let mut out = text.to_string();
    let mut names: Vec<&str> = catalog().iter().map(|m| m.name).filter(|n| n.contains('.')).collect();
    names.sort_by_key(|n| std::cmp::Reverse(n.len()));
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    for n in names {
        let t = tool_name(n);
        let mut res = String::with_capacity(out.len());
        let mut rest = out.as_str();
        while let Some(i) = rest.find(&t) {
            let before = rest[..i].chars().last();
            let mut after = rest[i + t.len()..].chars();
            let (a1, a2) = (after.next(), after.next());
            let joined = word(before) || before == Some('.') || word(a1) || (a1 == Some('.') && word(a2));
            res.push_str(&rest[..i]);
            res.push_str(if joined { &t } else { n });
            rest = &rest[i + t.len()..];
        }
        res.push_str(rest);
        out = res;
    }
    out
}

/// Extra, MCP-specific notes per method: when to use it and what a refusal means.
fn notes(m: &MethodSpec) -> Option<&'static str> {
    Some(match m.name {
        "session.list" => "Each entry is a summary: long text is cut, and the launch command, agent_args, most of agent_info \
            and the queued messages are left out. session_get {id} has all of it.",
        "rule.remove" => "Agents: this is refused. Use rule_request_removal {id, reason}; the human sees it and decides.",
        "trigger.set_secret" => "Agents: never call this; the secret is discarded and the human is asked to paste it in the GUI.",
        "secret.set" => "Prefer piping in a shell (`<command> | midna secret save NAME`): a value you pass here is already in \
            your context, so the secret is marked exposed.",
        "secret.replace" => "Agents: never call this; call secret_set, which asks the human when it would replace their secret.",
        // The local-trigger how-to lives on trigger_add alone: it was the biggest repeat in tools/list.
        "trigger.add" => "enabled: true (local only) or trigger_set_enabled turns a local trigger on with no approval; do it \
            when the human asked for the trigger. trigger_test dry-runs one first (session names the terminal for a local \
            trigger). The `guide` tool (topic `triggers`) has worked examples.",
        "trigger.update" | "trigger.list" | "trigger.test" => "Local triggers: see trigger_add and the `guide` tool \
            (topic `triggers`).",
        "needs_you.resolve" => "Agents normally don't call this: answering is the human's job. Refused unless the human enabled \
            approve.from_cli, and then only for your own session's approvals.",
        "policy.request" => "Blocks until the human answers (up to timeout_secs). Prefer policy_check to just test an action. \
            Add no_wait: true (works on any tool) to get the needs-you id back at once (error 6) and follow it with needs_you_get.",
        "session.close" => "Closing a working terminal or another one may ask the human and block meanwhile; add no_wait: true \
            to get the needs-you id back at once (error 6): the close then happens if they approve, and needs_you_get \
            {id, wait_secs} tells you how it went.",
        "agent.hook" => "Internal: midna's hook bridge calls this. Agents don't need it.",
        "session.input" => "To message another agent, send the text: Enter is pressed for you (enter=false leaves it in the box). Add images as absolute paths in `images` \
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
    push_flags(m, &mut d);
    d
}

/// What `describe_method` says about a method: the same text as its full-mode tool, with method names.
fn compact_description(m: &MethodSpec) -> String {
    let mut d = m.description.to_string();
    if let Some(n) = notes(m) {
        d.push(' ');
        d.push_str(&dotted(n));
    }
    push_flags(m, &mut d);
    d
}

fn push_flags(m: &MethodSpec, d: &mut String) {
    if m.human_only && !["rule.remove", "trigger.set_secret", "secret.replace"].contains(&m.name) {
        d.push_str(
            " HUMAN ONLY: calling this does not do it. midna asks the human (a needs-you approval) and returns an error with \
             the needs-you id; if they approve, midna runs it for them. Tell the user, don't retry or work around it.",
        );
    }
    if m.stub {
        d.push_str(" (Not implemented yet.)");
    }
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
                triggers, settings, windows), and what is human only and what to do instead.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": ro,
        }),
        json!({
            "name": "explain",
            "title": "midna explain",
            "description": "Explain one thing in plain words: a terminal id (why it has its status, recent status events, what \
                it waits on), a rule r_… (what it matches, who added it, when it fired, how to get it removed), a trigger t_… \
                (what it does, what it waits for), a needs-you item n_…, a project p_…, a delivery d_…, a method or tool name, a \
                setting key, or a topic (status, rules, approvals, triggers, needs-you, settings, scripts, themes, usage, keep-awake, windows, human-only, mcp). \
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
            "description": "The midna agent guide's core (SKILL.md): when to use which tool, attention etiquette, approvals, rules, \
                settings, windows, and never routing around a denial. It lists its topics; `topic` reads one.",
            "inputSchema": {
                "type": "object",
                "properties": { "topic": { "type": "string", "description": guide::SKILL_TOPICS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ") } }
            },
            "annotations": ro,
        }),
    ]
}

/// The guide's core, then a resource per topic, described by its heading.
fn skill_resources() -> Vec<Value> {
    let core = json!({
        "uri": "midna://skill", "name": "midna-skill", "title": "midna agent guide",
        "description": "How to use midna as an agent (SKILL.md).", "mimeType": "text/markdown",
    });
    let topics = guide::SKILL_TOPICS.iter().map(|(name, text)| {
        json!({
            "uri": format!("midna://skill/{name}"), "name": format!("midna-skill-{name}"), "title": format!("midna guide: {name}"),
            "description": text.lines().next().unwrap_or_default().trim_start_matches("## "), "mimeType": "text/markdown",
        })
    });
    std::iter::once(core).chain(topics).collect()
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

/// `midna mcp --compact`: the guide tools plus four that reach every method.
pub fn compact_tools() -> Vec<Value> {
    let method = json!({ "type": "string", "description": "A method name such as `session.list` (or its tool form `session_list`)." });
    let params = json!({ "type": "object", "description": "The method's params, as describe_method shows them." });
    let mut list = extra_tools();
    list.extend([
        json!({
            "name": "search_methods",
            "title": "Find midna methods",
            "description": "Find the midna method for a job (terminals, attention, approvals, rules, triggers, settings, windows…). \
                query: words matched against names and descriptions, e.g. `webhook` or `close terminal`; group: a name prefix \
                such as session or trigger; neither lists the groups. Then describe_method, and call_read or call to run it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "group": { "type": "string" },
                    "detail": { "type": "string", "enum": ["names", "summary", "full"], "default": "summary" },
                    "limit": { "type": "integer", "minimum": 1, "default": 20 }
                }
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        }),
        json!({
            "name": "describe_method",
            "title": "Describe a midna method",
            "description": "A method's full description and rules (what is human only, what a refusal means), the JSON Schema of \
                its params, and whether call_read or call runs it.",
            "inputSchema": { "type": "object", "properties": { "method": method }, "required": ["method"] },
            "annotations": { "readOnlyHint": true, "openWorldHint": false },
        }),
        json!({
            "name": "call_read",
            "title": "Run a read-only midna method",
            "description": "Run a midna method that changes nothing (lists, reads, status). Refuses methods that change state: \
                use call for those.",
            "inputSchema": { "type": "object", "properties": { "method": method, "params": params }, "required": ["method"] },
            "annotations": { "readOnlyHint": true, "idempotentHint": true, "openWorldHint": false },
        }),
        json!({
            "name": "call",
            "title": "Run a midna method",
            "description": "Run any midna method. no_wait: true returns an approval's needs-you id at once (error 6) instead of \
                blocking; follow it with needs_you.get. Human-only methods ask the human and return an error with the needs-you \
                id: tell the user, never retry or route around it. Errors carry the next step.",
            "inputSchema": {
                "type": "object",
                "properties": { "method": method, "params": params, "no_wait": { "type": "boolean" } },
                "required": ["method"]
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "openWorldHint": false },
        }),
    ]);
    list
}

/// A method by name (`session.list` or `session_list`) for the compact tools, or the error result to return.
fn lookup(name: &str) -> Result<&'static MethodSpec, Value> {
    match catalog().iter().find(|m| m.name == name || tool_name(m.name) == name) {
        Some(m) if SKIP.contains(&m.name) => Err(skipped(m.name)),
        Some(m) => Ok(m),
        None => Err(text_result(format!("unknown method `{name}`. Find it with the `search_methods` tool."), true)),
    }
}

fn skipped(method: &str) -> Value {
    let why = if method == "secret.exec_env" {
        "it returns secret values; only `midna secret exec NAME -- <command>` in a shell may use it"
    } else {
        "it needs a streaming connection of its own"
    };
    text_result(format!("`{method}` can't be called over MCP: {why}."), true)
}

/// The first sentence of a method's description.
fn summary(m: &MethodSpec) -> &'static str {
    let d = m.description;
    let mut from = 0;
    while let Some(i) = d[from..].find(". ") {
        let end = from + i + 1;
        if d[end + 1..].chars().next().is_some_and(char::is_uppercase) {
            return &d[..end];
        }
        from = end;
    }
    d
}

fn search_score(m: &MethodSpec, words: &[String]) -> usize {
    let name = m.name.to_lowercase();
    let first = summary(m).to_lowercase();
    let rest = m.description.to_lowercase();
    let notes = notes(m).map(|n| dotted(n).to_lowercase()).unwrap_or_default();
    words
        .iter()
        .map(|w| {
            let w = w.replace('_', ".");
            let in_name = name.contains(&w) || name.replace('.', "_").contains(&w.replace('.', "_"));
            10 * in_name as usize + 4 * first.contains(&w) as usize + 2 * rest.contains(&w) as usize + notes.contains(&w) as usize
        })
        .sum()
}

fn search_methods(args: &Value) -> Value {
    let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    let group = args.get("group").and_then(Value::as_str).unwrap_or("").trim().to_lowercase();
    let detail = args.get("detail").and_then(Value::as_str).unwrap_or("summary");
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).max(1) as usize;
    let methods = catalog().iter().filter(|m| !SKIP.contains(&m.name));
    if query.is_empty() && group.is_empty() {
        let mut groups: Vec<(&str, usize)> = Vec::new();
        for m in methods {
            match groups.iter_mut().find(|(g, _)| *g == m.group()) {
                Some((_, n)) => *n += 1,
                None => groups.push((m.group(), 1)),
            }
        }
        groups.sort_unstable();
        let list: Vec<String> = groups.iter().map(|(g, n)| format!("{g} ({n})")).collect();
        let text = format!("Groups (methods): {}.
Search with query (words) or group.", list.join(", "));
        return text_result(text, false);
    }
    let words: Vec<String> = query.split_whitespace().map(str::to_string).collect();
    let mut hits: Vec<(usize, &MethodSpec)> = methods
        .filter(|m| group.is_empty() || m.group() == group)
        .map(|m| (if words.is_empty() { 1 } else { search_score(m, &words) }, m))
        .filter(|(s, _)| *s > 0)
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(b.1.name)));
    let total = hits.len();
    hits.truncate(limit);
    if hits.is_empty() {
        let text = "No method matches. Try other words, or search_methods with no arguments to see the groups.".to_string();
        return text_result(text, false);
    }
    let mut lines = Vec::new();
    for (_, m) in &hits {
        let tool = if m.mutating { "call" } else { "call_read" };
        let human = if m.human_only { ", human only" } else { "" };
        lines.push(match detail {
            "names" => m.name.to_string(),
            "full" => format!("{} ({tool}{human}): {}\nParams: {}", m.name, compact_description(m), input_schema(m)),
            _ => format!("{} ({tool}{human}): {}", m.name, summary(m)),
        });
    }
    let more = if total > hits.len() { format!("
{} more; raise limit to see them.", total - hits.len()) } else { String::new() };
    let text = format!("{}{more}
describe_method {{method}} shows a method's params.", lines.join("
"));
    text_result(text, false)
}

fn describe_method(args: &Value) -> Value {
    let name = args.get("method").and_then(Value::as_str).unwrap_or("");
    let m = match lookup(name) {
        Ok(m) => m,
        Err(e) => return e,
    };
    let tool = if m.mutating { "call" } else { "call_read" };
    let kind = if m.mutating { "changes state" } else { "read only" };
    text_result(
        format!(
            "{} ({kind}; run it with the `{tool}` tool: {{\"method\":\"{}\",\"params\":{{…}}}})\n{}\nParams (JSON Schema): {}",
            m.name,
            m.name,
            compact_description(m),
            input_schema(m)
        ),
        false,
    )
}

/// `call_read` / `call`: `{method, params?, no_wait?}` to the shared `call_method`.
fn call_compact(client: &mut Option<Client>, args: &Value, read_only: bool) -> Result<Value, Value> {
    let name = args.get("method").and_then(Value::as_str).ok_or_else(|| json!({ "code": -32602, "message": "needs arguments.method" }))?;
    let mut params = match args.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p) if p.is_object() => p.clone(),
        Some(_) => return Err(json!({ "code": -32602, "message": "arguments.params must be an object" })),
    };
    let m = match lookup(name) {
        Ok(m) => m,
        Err(e) => return Ok(e),
    };
    if read_only && m.mutating {
        let text = format!(
            "`{}` changes state, and call_read only runs read-only methods. Use the `call` tool with {{\"method\":\"{}\"}}.",
            m.name, m.name
        );
        return Ok(text_result(text, true));
    }
    if !read_only && args.get("no_wait").and_then(Value::as_bool) == Some(true) {
        params["no_wait"] = json!(true);
    }
    Ok(call_method(client, m.name, params, Surface::McpCompact))
}

fn method_for(tool: &str) -> Option<&'static str> {
    catalog().iter().find(|m| tool_name(m.name) == tool && !SKIP.contains(&m.name)).map(|m| m.name)
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// A copy of `v` with every string cut to `max` characters.
fn clipped(v: &Value, max: usize) -> Value {
    match v {
        Value::String(s) if s.chars().count() > max => Value::String(s.chars().take(max - 1).chain(['…']).collect()),
        Value::Array(a) => a.iter().map(|v| clipped(v, max)).collect(),
        Value::Object(o) => o.iter().map(|(k, v)| (k.clone(), clipped(v, max))).collect(),
        v => v.clone(),
    }
}

fn picked(v: &Value, keys: &[&str], max: usize) -> Map<String, Value> {
    keys.iter().filter_map(|k| Some((k.to_string(), clipped(v.get(*k)?, max)))).collect()
}

fn summarized(s: &Value, max: usize) -> Value {
    let mut entry = picked(s, LISTED, max);
    if let Some(info) = s.get("agent_info") {
        entry.insert("agent_info".to_string(), picked(info, LISTED_AGENT_INFO, max).into());
    }
    if let Some(n) = s.get("queue").and_then(Value::as_array).map(Vec::len).filter(|n| *n > 0) {
        entry.insert("queued".to_string(), n.into());
    }
    entry.into()
}

/// One terminal as a listing shows it: strings are cut shorter until the entry fits.
fn listed_session(s: &Value) -> Value {
    let mut max = LISTED_TEXT;
    loop {
        let entry = summarized(s, max);
        if max <= 8 || entry.to_string().len() < LISTED_BYTES {
            return entry;
        }
        max /= 2;
    }
}

/// What an agent gets of a method's result: the RPC result, but for terminal listings.
fn for_agent(method: &str, v: Value) -> Value {
    match (method, &v) {
        ("session.list", Value::Array(list)) => list.iter().map(listed_session).collect(),
        _ => v,
    }
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
    text_result(text, true)
}

fn call_tool(client: &mut Option<Client>, params: &Value, mode: Mode) -> Result<Value, Value> {
    let tool = params.get("name").and_then(Value::as_str).ok_or_else(|| json!({ "code": -32602, "message": "tools/call needs params.name" }))?;
    let args = match params.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(a) if a.is_object() => a.clone(),
        Some(_) => return Err(json!({ "code": -32602, "message": "tools/call arguments must be an object" })),
    };
    match tool {
        "capabilities" => return Ok(text_result(guide::capabilities(), false)),
        "guide" => {
            return Ok(match guide::skill_text(args.get("topic").and_then(Value::as_str)) {
                Ok(text) => text_result(text.to_string(), false),
                Err(e) => text_result(e, true),
            });
        }
        "explain" => {
            let mut words: Vec<String> = args.get("target").and_then(Value::as_str).map(|t| vec![t.to_string()]).unwrap_or_default();
            if let Some(v) = args.get("value").and_then(Value::as_str) {
                words.push(v.to_string());
            }
            let mut caller = |m: &str, p: Value| rpc(client, m, p);
            return Ok(match guide::explain(&mut caller, &words, mode.surface()) {
                Ok(t) => text_result(t, false),
                Err(e) => error_result(&e),
            });
        }
        _ => {}
    }
    if mode == Mode::Compact {
        return match tool {
            "search_methods" => Ok(search_methods(&args)),
            "describe_method" => Ok(describe_method(&args)),
            "call_read" => call_compact(client, &args, true),
            "call" => call_compact(client, &args, false),
            _ => {
                let hint = match method_for(tool) {
                    Some(m) => format!("; this server is compact, so run it with {}", guide::compact_call(m, "")),
                    None => "; tools/list lists them (or call the `search_methods` tool)".to_string(),
                };
                Err(json!({ "code": -32602, "message": format!("unknown tool `{tool}`{hint}") }))
            }
        };
    }
    let Some(method) = method_for(tool) else {
        return Err(json!({ "code": -32602, "message": format!("unknown tool `{tool}`; tools/list lists them (or call the `capabilities` tool)") }));
    };
    Ok(call_method(client, method, args, Surface::Mcp))
}

/// Run one catalog method for either mode: `no_wait: true` in the args doesn't block on an
/// approval (caller.no_wait, on a connection of its own); refusals get their next step.
fn call_method(client: &mut Option<Client>, method: &str, mut args: Value, surface: Surface) -> Value {
    if SKIP.contains(&method) {
        return skipped(method);
    }
    let no_wait = args.as_object_mut().and_then(|o| o.remove("no_wait")).and_then(|v| v.as_bool()) == Some(true);
    let res = if no_wait {
        let mut once = Client::connect_default().ok().map(Client::no_wait);
        rpc(&mut once, method, args.clone())
    } else {
        rpc(client, method, args.clone())
    };
    match res {
        Ok(v) => text_result(serde_json::to_string(&for_agent(method, v)).unwrap_or_default(), false),
        Err(e) => error_result(&guide::with_next_step(method, &args, e, surface)),
    }
}

fn negotiate(requested: Option<&str>) -> &'static str {
    requested.and_then(|r| VERSIONS.iter().find(|v| **v == r)).copied().unwrap_or(VERSIONS[0])
}

pub fn handle(client: &mut Option<Client>, msg: &Value, mode: Mode) -> Option<Value> {
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
            "instructions": match mode {
                Mode::Full => INSTRUCTIONS.to_string(),
                Mode::Compact => compact_instructions(),
            },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": match mode { Mode::Full => tools(), Mode::Compact => compact_tools() } })),
        "tools/call" => call_tool(client, &params, mode),
        "resources/list" => Ok(json!({ "resources": skill_resources() })),
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "resources/read" => match params.get("uri").and_then(Value::as_str) {
            Some(u) => match guide::skill_resource(u) {
                Some(text) => Ok(json!({ "contents": [{ "uri": u, "mimeType": "text/markdown", "text": text }] })),
                None => Err(json!({ "code": -32002, "message": format!("resource not found: {u}") })),
            },
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

pub fn run(mode: Mode) -> Res {
    let mut client: Option<Client> = None;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Array(_)) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32600, "message": "batches are not supported" } })),
            Ok(msg) if msg.is_object() => handle(&mut client, &msg, mode),
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

    fn assert_refs_resolve(schema: &Value, what: &str) {
        let s = schema.to_string();
        for part in s.split("\"$ref\":\"#/$defs/").skip(1) {
            let name: String = part.chars().take_while(|c| *c != '"').collect();
            assert!(schema["$defs"].get(&name).is_some(), "{what}: missing $defs/{name}");
        }
    }

    #[test]
    fn schemas_resolve_their_refs() {
        for t in tools() {
            assert_refs_resolve(&t["inputSchema"], t["name"].as_str().unwrap());
        }
    }

    /// trigger_add and trigger_update once took 25 KB of tools/list, which a client that loads every
    /// tool up front pays for on each turn. Their schemas stay whole; their descriptions keep to a
    /// budget, and the local-trigger how-to sits on trigger_add only.
    #[test]
    fn trigger_descriptions_stay_lean() {
        let t = tools();
        let desc = |n: &str| t.iter().find(|t| t["name"] == n).unwrap()["description"].as_str().unwrap().to_string();
        let (add, update) = (desc("trigger_add"), desc("trigger_update"));
        assert!(add.len() + update.len() <= 3500, "trigger_add {} + trigger_update {} bytes is over 3500", add.len(), update.len());
        assert!(add.contains("enabled: true") && add.contains("topic `triggers`"), "{add}");
        for n in ["trigger_update", "trigger_list", "trigger_test"] {
            let d = desc(n);
            assert!(d.contains("see trigger_add") && !d.contains("trigger_set_enabled turns"), "{n}: {d}");
        }
    }

    fn is_error(r: &Value) -> bool {
        r["isError"] == true
    }

    fn text(r: &Value) -> &str {
        r["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn full_listing_is_every_method_plus_the_guide_tools() {
        let names: Vec<String> = tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
        let mut want: Vec<String> = ["capabilities", "explain", "guide"].map(String::from).to_vec();
        want.extend(catalog().iter().filter(|m| !SKIP.contains(&m.name)).map(|m| tool_name(m.name)));
        assert_eq!(names, want);
        let listed = handle(&mut None, &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }), Mode::Full).unwrap();
        assert_eq!(listed["result"]["tools"], json!(tools()));
    }

    #[test]
    fn compact_listing_fits_its_budget() {
        let tools = compact_tools();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["capabilities", "explain", "guide", "search_methods", "describe_method", "call_read", "call"]);
        let bytes = serde_json::to_string(&tools).unwrap().len();
        assert!(bytes < 6000, "compact tools/list is {bytes} bytes; keep it under ~6 KB (~1.5k tokens)");
        for t in &tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        }
        let by = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();
        assert_eq!(by("call_read")["annotations"]["readOnlyHint"], true);
        assert_eq!(by("call")["annotations"]["destructiveHint"], true);
        let listed = handle(&mut None, &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }), Mode::Compact).unwrap();
        assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn a_listing_keeps_a_summary_of_each_terminal() {
        let long = "x".repeat(8000);
        let session = json!({
            "id": "s1", "project_id": "p1", "name": long, "kind": "agent", "agent": "claude", "cwd": "/work", "pid": 7,
            "command": ["claude", long], "agent_args": [long], "title": long, "keep_on_top": false,
            "status": { "state": "working", "reason": long, "since": "2026-11-06T10:00:00Z" },
            "created_at": "2026-11-06T10:00:00Z", "last_activity_at": "2026-11-06T10:00:00Z",
            "git": { "branch": "main", "ahead": 0, "behind": 0, "added": 1, "removed": 2, "files": 1 },
            "agent_info": { "model": "opus", "permission_mode": "default", "transcript_path": long, "background": [long], "rate_limits": { "a": long } },
            "custom_status": { "label": "Blocked", "color": "red", "detail": long, "since": "2026-11-06T10:00:00Z" },
            "queue": [{ "id": "q1", "text": long }, { "id": "q2", "text": long }],
            "adopted": { "agent": "claude", "bin": long, "args": [long] },
            "notify": { "turn_done": false }, "allowed_tools": [long], "auto_name": { "name": long },
        });
        let list = for_agent("session.list", json!([session]));
        let entry = &list[0];
        let bytes = entry.to_string().len();
        assert!(bytes < LISTED_BYTES, "a listed terminal is {bytes} bytes: {entry}");
        assert_eq!(entry["id"], "s1");
        assert_eq!(entry["status"]["state"], "working");
        assert_eq!(entry["git"]["branch"], "main");
        assert_eq!(entry["agent_info"], json!({ "model": "opus", "permission_mode": "default" }));
        assert_eq!(entry["queued"], 2);
        for gone in ["command", "agent_args", "queue", "adopted", "notify", "allowed_tools", "auto_name"] {
            assert!(entry.get(gone).is_none(), "{gone} leaked: {entry}");
        }
        let small = for_agent("session.list", json!([{ "id": "s1", "title": long, "queue": [] }]));
        assert_eq!(small[0]["title"].as_str().unwrap().chars().count(), LISTED_TEXT);
        assert!(small[0].get("queued").is_none(), "{small}");
        let wide = "界\"".repeat(4000);
        let every_field = LISTED.iter().map(|k| (k.to_string(), json!(wide))).collect::<Map<_, _>>();
        let full = for_agent("session.list", json!([every_field]));
        assert!(full[0].to_string().len() < LISTED_BYTES, "every kept field at its longest: {}", full[0]);
        let other = json!([{ "id": "s1", "command": ["claude"] }]);
        assert_eq!(for_agent("session.get", other.clone()), other);
    }

    #[test]
    fn search_finds_methods_by_words_and_group() {
        let found = |args: Value| -> Vec<String> {
            let r = search_methods(&args);
            assert!(!is_error(&r), "{r}");
            let t = text(&r);
            let hits = t.lines().filter(|l| l.contains("): ")).map(|l| l.split(' ').next().unwrap().to_string());
            hits.collect()
        };
        assert!(found(json!({ "query": "webhook" })).contains(&"trigger.add".to_string()));
        assert_eq!(found(json!({ "query": "close terminal" }))[0], "session.close");
        assert_eq!(found(json!({ "query": "rule_request_removal" }))[0], "rule.request_removal");
        let session = found(json!({ "group": "session", "limit": 500 }));
        assert!(session.iter().all(|m| m.starts_with("session.")) && session.contains(&"session.open".to_string()));
        assert_eq!(found(json!({ "query": "session", "limit": 3 })).len(), 3);
        let names = text(&search_methods(&json!({ "group": "rule", "detail": "names" }))).to_string();
        assert!(names.lines().any(|l| l == "rule.add"), "{names}");
        let full = text(&search_methods(&json!({ "query": "rule.add", "detail": "full", "limit": 1 }))).to_string();
        let params = full.lines().find_map(|l| l.strip_prefix("Params: ")).unwrap();
        assert!(serde_json::from_str::<Value>(params).unwrap()["properties"].is_object(), "{full}");
        let groups = text(&search_methods(&json!({}))).to_string();
        assert!(groups.contains("trigger ("), "{groups}");
        for skip in SKIP {
            assert!(!found(json!({ "query": skip, "limit": 500 })).contains(&skip.to_string()), "{skip} is searchable");
        }
    }

    #[test]
    fn every_method_is_describable_with_resolving_refs() {
        let tool_names: Vec<String> = catalog().iter().map(|m| tool_name(m.name)).filter(|t| !t.contains('.')).collect();
        for m in catalog().iter().filter(|m| !SKIP.contains(&m.name)) {
            for name in [m.name.to_string(), tool_name(m.name)] {
                let r = describe_method(&json!({ "method": name }));
                assert!(!is_error(&r), "{name}: {}", text(&r));
                let t = text(&r);
                let tool = if m.mutating { "call" } else { "call_read" };
                assert!(t.starts_with(&format!("{} (", m.name)) && t.contains(&format!("the `{tool}` tool")), "{name}: {t}");
                let schema = t.split_once("Params (JSON Schema): ").unwrap().1;
                let schema: Value = serde_json::from_str(schema).unwrap();
                assert_eq!(schema["type"], "object", "{name}");
                assert_refs_resolve(&schema, m.name);
                if m.human_only && !["rule.remove", "trigger.set_secret", "secret.replace"].contains(&m.name) {
                    assert!(text(&r).contains("HUMAN ONLY"), "{name}");
                }
            }
            // Compact mode has no per-method tools, so its descriptions name methods, not tools.
            let d = compact_description(m);
            for t in &tool_names {
                let named = d.match_indices(t.as_str()).any(|(i, _)| {
                    let before = d[..i].chars().last();
                    let after = d[i + t.len()..].chars().next();
                    !before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                        && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
                });
                assert!(!named, "{}: describe_method names the tool `{t}`: {d}", m.name);
            }
        }
        let r = describe_method(&json!({ "method": "no.such" }));
        assert!(is_error(&r) && text(&r).contains("search_methods"), "{r}");
    }

    #[test]
    fn dotted_names_methods_and_leaves_other_words() {
        assert_eq!(dotted("Use rule_request_removal {id}. See session_key."), "Use rule.request_removal {id}. See session.key.");
        assert_eq!(dotted("session_id and keep_awake.wake and x_session_list"), "session_id and keep_awake.wake and x_session_list");
    }

    #[test]
    fn skipped_methods_are_unreachable() {
        let mut client = None;
        for skip in SKIP {
            for name in [skip.to_string(), tool_name(skip)] {
                let r = describe_method(&json!({ "method": name }));
                assert!(is_error(&r) && text(&r).contains("can't be called over MCP"), "{name}: {r}");
                for read_only in [true, false] {
                    let r = call_compact(&mut client, &json!({ "method": name, "params": {} }), read_only).unwrap();
                    assert!(is_error(&r) && text(&r).contains("can't be called over MCP"), "{name}: {r}");
                }
                let r = handle(&mut client, &json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool_name(skip) } }), Mode::Full);
                assert_eq!(r.unwrap()["error"]["code"], -32602, "{name}");
            }
            assert!(is_error(&call_method(&mut client, skip, json!({}), Surface::Mcp)), "{skip}");
        }
        assert!(client.is_none(), "no skipped method reached the daemon");
    }

    #[test]
    fn call_read_refuses_mutating_methods() {
        let mut client = None;
        for m in catalog().iter().filter(|m| m.mutating && !SKIP.contains(&m.name)) {
            let r = call_compact(&mut client, &json!({ "method": m.name }), true).unwrap();
            assert!(is_error(&r) && text(&r).contains("the `call` tool"), "{}: {r}", m.name);
        }
        assert!(client.is_none(), "no refused call reached the daemon");
        let bad = |args: Value| call_compact(&mut None, &args, false).unwrap_err()["code"].clone();
        assert_eq!(bad(json!({})), -32602);
        assert_eq!(bad(json!({ "method": "session.list", "params": [1] })), -32602);
    }

    #[test]
    fn compact_mode_points_per_method_tools_at_call() {
        let msg = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "session_list", "arguments": {} } });
        let r = handle(&mut None, &msg, Mode::Compact).unwrap();
        assert_eq!(r["error"]["code"], -32602);
        assert!(r["error"]["message"].as_str().unwrap().contains(r#"the `call_read` tool with {"method":"session.list"}"#), "{r}");
        let init = handle(&mut None, &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }), Mode::Compact).unwrap();
        let text = init["result"]["instructions"].as_str().unwrap();
        assert!(text.contains("search_methods") && text.contains("session") && !text.contains("session_*"), "{text}");
    }
}
