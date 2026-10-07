//! Agent-facing guidance shared by the CLI and `midna mcp`: the capabilities overview, the
//! bundled skill, "what to do next" hints for refusals, topic notes, and `explain`.
use midna_proto::error::{BAD_PARAMS, CONFLICT, HUMAN_ONLY, NOT_FOUND, PENDING, REFUSED, UNKNOWN_METHOD};
use midna_proto::settings::{SETTINGS, setting};
use midna_proto::{RpcError, catalog, time};
use serde_json::{Value, json};

pub const SKILL_MD: &str = include_str!("../assets/SKILL.md");

/// Where the caller is, so hints name the right thing to run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
}

/// `midna capabilities`: short, agent-oriented, works offline.
pub fn capabilities() -> String {
    let human_only: Vec<&str> = catalog().iter().filter(|m| m.human_only).map(|m| m.name).collect();
    let human_settings: Vec<&str> = SETTINGS.iter().filter(|s| s.human_only).map(|s| s.key).collect();
    format!(
        "midna: an AI-managed terminal. The daemon owns every terminal, rule, trigger and setting; the human
watches the GUI. You can drive almost all of it: CLI `midna <verb>`, MCP tools (server `midna`), or
`midna call <method> <json>`. Everything you do is logged (`midna events`).

WHAT YOU CAN DO
  see        midna list | get <id> | projects | needs [get <n_id>] | read <id> | explain <id> | events | insights | usage
  terminals  midna open [--agent claude|codex --prompt T --resume ID -- agent-args | --monitor CMD | -- argv] [--background] · background · send · key · rename · restart · close
  queue      midna queue add <text> [--after <id> | --idle 10m | --at 18:00] (typed once the agent is ready) · list · rm
  attention  midna attention \"<one line>\" (blocked) | --note (FYI)      [MCP needs_you_raise]
  policy     midna check <kind> <value> · rules add <allow|ask|deny> <kind> <glob> [--scope …] [--expires S]
  triggers   midna triggers add … (webhook drafts; local hook/event/idle triggers you may enable) · test · update · disable
  secrets    [secret:NAME] = a stored secret: midna secret exec NAME -- <cmd> ($NAME set, output masked) · write NAME .env · list
             save a token a command produced: <cmd> | midna secret save NAME (stdin only; never echo it)
  settings   midna settings list | get | set | reset   (human-only keys: setting one asks the human)
  windows    midna focus <id> · window open_screen <rules|triggers|insights|notifications|settings|needs_you>
  projects   midna projects add <path> · update <id> --name/--icon · add-command <id> --name N -- <cmd>
  learn      midna help · midna <verb> --help · midna schema [method|--list] · midna skill · midna explain <topic>

HUMAN ONLY (calling these makes a request the human sees; nothing happens until they approve)
  methods    {}
  settings   {}
  also       removing rules (use `midna rules request-removal <id> --reason …`), webhook secrets, enabling
             webhook triggers, approving approvals (unless the human enabled approve.from_cli, own session only),
             dismissing needs-you items, macOS permission grants (ask with `midna attention`).

RULES OF THE ROAD
  - Agents add, humans remove. A refusal tells you the next step; follow it, don't retry another way.
  - Never route around a denial: no other tool, no editing $MIDNA_HOME, no other agent doing it for you.
  - Raise attention only when you are blocked or the human must know; one short line.
  - Don't answer another agent's prompt unless `midna read <id> --screen` shows it.

Full guide: `midna skill` (also $MIDNA_SKILL). Your role and session: `midna info`.",
        human_only.join(", "),
        human_settings.join(", "),
    )
}

/// `midna capabilities --json` / the MCP `capabilities` tool's structured content.
pub fn capabilities_json() -> Value {
    let methods: Vec<Value> = catalog()
        .iter()
        .map(|m| json!({ "method": m.name, "tool": m.name.replace('.', "_"), "mutating": m.mutating, "human_only": m.human_only, "description": m.description }))
        .collect();
    let settings: Vec<Value> = SETTINGS.iter().map(|s| json!({ "key": s.key, "human_only": s.human_only, "description": s.description })).collect();
    json!({ "overview": capabilities(), "methods": methods, "settings": settings })
}

fn cmd(surface: Surface, cli: &str, tool: &str) -> String {
    match surface {
        Surface::Cli => format!("`midna {cli}`"),
        Surface::Mcp => format!("the `{tool}` tool"),
    }
}

/// What to do next after `method` failed with `e`. Appended to the error message.
pub fn next_step(method: &str, params: &Value, e: &RpcError, surface: Surface) -> Option<String> {
    let id = params.get("id").and_then(Value::as_str).unwrap_or("<id>");
    let needs_you = e.data.as_ref().and_then(|d| d.get("needs_you_id")).and_then(Value::as_str).map(str::to_string);
    let wait = |n: &str| {
        format!(
            "Nothing was done yet: the human sees needs-you item {n} and decides; if they approve, midna runs it for them. \
             Don't retry it or try another way. Carry on with other work; check later with {}.",
            cmd(surface, &format!("explain {n}"), "explain")
        )
    };
    Some(match (e.code, method) {
        (HUMAN_ONLY, "rule.remove") => format!(
            "rule.remove is human-only; use {} and the human will see it.",
            match surface {
                Surface::Cli => format!("`midna rules request-removal {id} --reason \"…\"`"),
                Surface::Mcp => format!("the `rule_request_removal` tool with {{\"id\":\"{id}\",\"reason\":\"…\"}}"),
            }
        ),
        (HUMAN_ONLY, "trigger.set_secret") => "Webhook secrets are the human's: midna asked them to paste it in Triggers and discarded \
             the one you sent. Never put secrets in commands or prompts; tell the human what the secret is for."
            .into(),
        (HUMAN_ONLY, "needs_you.resolve") => format!(
            "Only the human answers this item. Leave it; if you are blocked meanwhile, say so once with {}.",
            cmd(surface, "attention \"…\"", "needs_you_raise")
        ),
        (HUMAN_ONLY, _) if needs_you.is_some() => wait(needs_you.as_deref().unwrap_or("")),
        (HUMAN_ONLY, _) => "This is the human's call. Ask them in one line with `midna attention` and continue with something else.".into(),
        (REFUSED, "window.command") => format!(
            "Front and open_screen are always allowed. For the rest the human must turn on agents.may_move_windows; \
             {} asks them (human-only setting).",
            cmd(surface, "settings set agents.may_move_windows true", "settings_set")
        ),
        (PENDING, _) => {
            let n = needs_you.as_deref().unwrap_or("<n_id>");
            format!(
                "Nothing was done yet: needs-you {n} waits on the human, and the call carries on by itself if they approve. \
                 Don't retry it. Follow it with {} (state, and the call's result or error once answered).",
                match surface {
                    Surface::Cli => format!("`midna needs get {n}` or `midna needs wait {n}`"),
                    Surface::Mcp => format!("the `needs_you_get` tool with {{\"id\":\"{n}\",\"wait_secs\":60}}"),
                }
            )
        }
        (REFUSED, _) if e.message.contains("approval withdrawn") => format!(
            "Nothing was done: the approval was withdrawn before the human answered, because what it was about (or the asker) \
             is gone. Check with {} before asking again.",
            cmd(surface, "list", "session_list")
        ),
        (REFUSED, _) if needs_you.is_some() => format!(
            "The human denied it (or didn't answer in time). Don't retry another way. If it matters, explain why in one line \
             with {}.",
            cmd(surface, "attention \"…\"", "needs_you_raise")
        ),
        (REFUSED, _) => format!(
            "A deny rule or the default policy refused it. {} shows which rule; if you think it is wrong, \
             {} and let the human decide. Don't work around it.",
            cmd(surface, "check <kind> <value>", "policy_check"),
            cmd(surface, "rules request-removal <rule-id> --reason \"…\"", "rule_request_removal")
        ),
        (CONFLICT, "session.close") => format!(
            "The terminal is busy. Close it anyway with {} (policy-checked; may ask the human), or wait until it is idle.",
            match surface {
                Surface::Cli => format!("`midna close {id} --force`"),
                Surface::Mcp => "force: true".into(),
            }
        ),
        (CONFLICT, _) if e.message.contains("upgrading") => "midnad is restarting; retry in a second.".into(),
        (NOT_FOUND, m) => {
            let list = match m.split('.').next().unwrap_or("") {
                "session" | "stream" | "script" => cmd(surface, "list", "session_list"),
                "rule" => cmd(surface, "rules list", "rule_list"),
                "trigger" => cmd(surface, "triggers list", "trigger_list"),
                "queue" => cmd(surface, "queue", "queue_list"),
                "needs_you" => cmd(surface, "needs", "needs_you_list"),
                "project" => cmd(surface, "projects", "project_list"),
                "settings" => cmd(surface, "settings list", "settings_list"),
                _ => return None,
            };
            format!("List the valid ids with {list}.")
        }
        (BAD_PARAMS, m) => format!("See the params with {}.", cmd(surface, &format!("schema {m}"), "explain")),
        (UNKNOWN_METHOD, _) => format!("List the methods with {}.", cmd(surface, "schema --list", "capabilities")),
        _ => return None,
    })
}

/// The error with its next step appended.
pub fn with_next_step(method: &str, params: &Value, mut e: RpcError, surface: Surface) -> RpcError {
    if let Some(hint) = next_step(method, params, &e, surface) {
        e.message = format!("{}\nNext: {hint}", e.message);
        let mut data = e.data.take().unwrap_or_else(|| json!({}));
        if let Some(o) = data.as_object_mut() {
            o.insert("next".into(), json!(hint));
        }
        e.data = Some(data);
    }
    e
}

// ------------------------------------------------------------------ topics

const TOPICS: &[(&str, &str)] = &[
    ("status", "Terminal status comes only from real signals: agent hooks (Claude), Codex notify, the OSC title glyph \
        (◐◑ working, ✳ stopped), a check of the screen for a visible prompt, and process exit.\n\
        idle = nothing running for you / waiting for input. working = an agent turn or approval answered.\n\
        needs_you = waiting on the human (approval, permission prompt, blocked). done = an agent finished its turn and \
        the human hasn't looked yet. failed = non-zero exit (monitors and agents raise a needs-you item). exited = clean exit.\n\
        `midna explain <terminal-id>` shows the current reason and the events that led to it."),
    ("rules", "Rules decide allow / ask / deny for actions of kind command, tool (`Bash(git push*)`), path, cli \
        (midna verbs like `close --force …`) and window. Patterns are globs. The narrowest scope wins (session > \
        project > global); within a scope deny > ask > allow; no match = setting policy.default (auto: ask for \
        destructive midna verbs, allow otherwise). Agents may add rules; only the human removes them — agents run \
        `midna rules request-removal <id> --reason …`. `midna explain <kind> <value>` shows which rule decides an action."),
    ("approvals", "An `ask` decision raises an approval needs-you item and blocks the caller until the human answers \
        (setting policy.request_timeout_secs, default 300). Approving with a scope adds a rule: minutes(n) and session \
        → session rule, always → project rule. Agents can't approve unless the human enabled approve.from_cli, and \
        then only their own session's requests. Approvals that confirm a human-only action are the human's alone. \
        `--no-wait` (MCP/RPC: caller.no_wait) returns at once with the needs-you id (exit 4 / error 6) and the call \
        finishes when the human answers; follow it with `midna needs get|wait <id>` (needs_you.get). An approval is \
        withdrawn if the terminal it is about closes, or its asker disconnects or closes, before anyone answers."),
    ("triggers", "A webhook trigger maps a GitHub/Bitbucket event (`pull_request.opened`, `pullrequest:created`, globs \
        ok) plus filters (repo, branch, action, label) to an action: start_agent (prompt template), run_command (a \
        monitor terminal; values shell-quoted) or attention. States: needs_secret → draft → active ⇄ paused. Agents \
        draft; the human pastes the secret and enables. `midna triggers test <id> --payload f.json` dry-runs one.\n\
        A local trigger (source local) fires on this Mac. event: `hook.<HookEvent>` (hook.Stop, hook.UserPromptSubmit, \
        hook.Notification, …), a midna event kind (agent.prompt_blocked {hook, message, prompt}, agent.turn_ended, \
        session.status, …), `idle` (no turn started/ended for filter idle_minutes) or `schedule` (filter cron: five \
        fields in local time, `0 9 * * mon-fri`, `*/30 * * * *`, or @hourly/@daily/@weekly/@monthly/@yearly, or `@every 55m` for an interval cron can't say \
        (`*/55` is :00 and :55; it counts from starts_at, filled in as one interval after it's added); data \
        {cron, scheduled_for, local_time}; with no session/project/agent filter it fires once about no terminal, with \
        one it acts on every running terminal that matches; a missed run fires late only within 10 minutes; filter window \
        {from, until} (HH:MM local, until not included, may wrap midnight), starts_at / ends_at (RFC 3339) and max_runs \
        (1 = once; a new value restarts the count) narrow it); globs ok. \
        Filters: session, project, agent (claude|codex), idle_minutes, cron, match {dotted.path: glob} (case-insensitive, all must match the \
        hook payload / event data). Actions on the terminal that fired: send_to_session {steps:[{text, enter}]} (in \
        order, each waits until the agent is ready), set_status {label, color, icon?, base: idle|working|needs_you|done|failed, \
        clear_on: prompt|turn|status|never} (shown instead of the built-in status, which still drives sorting and \
        Needs You), clear_status, notify {title, body?, sound, category?} (a macOS notification, category from_trigger or a kind the human added); also attention, run_command, start_agent. Templates: {{last_prompt}} (the terminal's \
        latest full prompt), {{event}}, {{session.id|name|project_id|agent|status}}, {{data.<path>}} or bare {{<path>}} \
        like {{message}}; run_command shell-quotes values. cooldown_secs (default 60) per terminal. Events caused by a trigger never fire triggers. Agents may \
        add, enable and pause local triggers directly (no approval) when the human asked for one (`--enable` on add). Built in: \
        prompt_blocked_status shows “Prompt blocked” when a hook refuses a prompt; edit, pause or remove it like any other."),
    ("secrets", "When the human pastes a secret into an agent terminal, the midna app stores it (Keychain) and the \
        agent sees `[secret:NAME]` instead, so the value never reaches the model. `midna secret exec NAME -- <cmd>` \
        runs cmd with $NAME set (VAR=NAME sets $VAR) and masks the value (raw, base64, URL-encoded, JSON-escaped) in \
        its output as ‹NAME›. `midna secret write NAME .env [--as KEY]` sets it in a .env file. `midna secret list` \
        shows names. A project's secret shadows a global one of the same name. Agents save tokens a command \
        produced with `<cmd> | midna secret save NAME [--global]` (stdin only, so the value never enters the context); \
        a value an agent passes itself is marked exposed. Replacing a secret the human stored asks the human; only \
        the human removes secrets. Never ask for a value in chat."),
    ("needs-you", "Needs-you items are what the human must look at: approval, permission_prompt, blocked, note, failed, \
        trigger_waiting, rule_removal, secret_needed. Agents raise blocked/note with `midna attention`. The human \
        resolves: approve{scope}, deny, dismiss, done, restart. An agent's \"trust this folder?\" startup dialog is a \
        permission_prompt too (approve = Yes, deny = No; any Yes, here or in the terminal, adds the folder to agents.trust_folders); \
        a folder agents.trust_folders covers is answered Yes without one. `midna needs get <id>` says where one item stands."),
    ("settings", "Settings live in the daemon; `midna settings list` shows every key with its value, default and \
        description. Agents may change any key not marked human only; setting a human-only key asks the human. The GUI \
        updates live on settings.changed. Keybindings are settings too (keys.*)."),
    ("scripts", "What the terminal header, each sidebar row and the status bar show is set by scripts. Settings: \
        ui.header.script, ui.row.script, ui.status.script (run for the selected terminal), and ui.status.items, the \
        status bar's items left to right (daemon, policy, webhooks, triggers, hooks, accessibility, script, spacer, update, \
        keys, or an absolute path to a script, one item each; leave an item out to hide it).\n\
        Header buttons: ui.header.buttons lists the toolbar left to right (subagents, links, ide, image, split, popout, \
        restart, or a script path; More is always last). A built-in left out moves into the … menu and its shortcut \
        keeps working. A script path is your own button, \
        printing its button's look (segments; [] hides it) and run again with MIDNA_CLICK=1 when the human clicks it \
        (its output is the new look). A click script can call `midna` itself, e.g. `midna open …` or `midna attention …`.\n\
        A script setting is built-in parts joined with + (worktree, branch, sync, diff, files, pr, agent; github = \
        worktree+branch+sync+diff+files+pr; none) or an absolute path to an executable. midnad runs it in the terminal's \
        cwd with the terminal as JSON on stdin and MIDNA_SESSION, MIDNA_PROJECT, MIDNA_SLOT (header|row|status|button) set; \
        5 s timeout; it runs again on the terminal's events and every git.refresh_secs, so keep it fast.\n\
        It prints a JSON array of segments (or {\"segments\": [...]}), each {text, tone?: dim|ok|err|need|work|accent, \
        icon?: dot|check|cross|bell|lock|branch|worktree|pr|bolt|play|globe|link|file|search|shield|…, tooltip?, \
        link? (opened on click), mono?, join? (attach to the previous segment)}. Example: \
        [{\"text\":\"CI\",\"tone\":\"ok\",\"icon\":\"check\",\"tooltip\":\"14 checks passed\"}]. An empty array hides it.\n\
        Statuses: ui.status.looks restyles built-in statuses, one rule per status, only the parts given: \
        `needs_you = pink icon:bell label:Your turn`, `claude.working = teal icon:bolt` (color = the dot, icon = in place \
        of the agent icon, label = header chip and row line; agent rules add to the plain one). A trigger's set_status still wins.\n\
        Agents may pick built-ins and hide, show or reorder items directly. Pointing a setting at a script path, or \
        adding a path to ui.status.items or ui.header.buttons, asks the human: write the script, make it executable, test it by running it, \
        then `midna settings set …`. `midna settings reset ui.status.items` restores the default bar."),
    ("themes", "Colors come from a theme: `midna themes` lists them (8 built-ins: twilight, nord, dracula, gruvbox, \
        tokyo-night dark; daylight, solarized-light, latte light; plus the human's custom ones) and which one shows. \
        Switch with `midna themes use <id>` (setting theme; system follows macOS using theme.dark / theme.light). One \
        color on top of any theme: setting theme.colors, rules `<color> = #RRGGBB` or `<theme id>:<color> = #RRGGBB` \
        (e.g. `accent = #FF79C6`). A theme covers the UI (bg, panel, raised, line, fg, dim, accent, accent-fg, need, ok, \
        err, work, term) and the terminal's 16 ANSI colors.\n\
        To make one, write $MIDNA_HOME/themes/<id>.json (the file name is the id): {\"name\": \"My Night\", \"kind\": \
        \"dark\", \"extends\": \"nord\", \"colors\": {\"accent\": \"#FF79C6\"}, \"terminal\": {\"red\": \"#FF5555\"}}. \
        Everything left out comes from extends; terminal keys are black … white, bright-black … bright-white, or \
        \"ansi\": [16 colors]. The app picks the file up within seconds; check `midna themes` for load errors, then \
        `midna themes use <id>`. `midna themes format` prints the shape."),
    ("usage", "Claude's plan usage limits are account-wide; Claude Code's status line reports them on Pro/Max plans \
        (midna's status line, setting agents.claude.statusline). `midna usage` (usage.get) has the latest any terminal \
        reported: five_hour and seven_day {used_percentage 0-100, resets_at}, observed_at and the terminal. limited = a \
        window is at 100% and has not reset (new turns fail until limited_until); a window whose resets_at passed is \
        marked expired. Each terminal's own last report is in session.get agent_info.rate_limits. Event \
        usage.limit_reached {agent, window, used_percentage, resets_at} fires once per window when it hits 100%: \
        subscribe to it (or a local trigger on it) instead of polling."),
    ("windows", "`midna focus <id>` and `midna window front|open_screen <screen>` are always allowed. pop_out, \
        keep_on_top, snap and close need the human-only setting agents.may_move_windows, and are policy-checked as \
        `window` actions."),
    ("human-only", "Human only: removing rules, setting webhook secrets, removing stored secrets (or replacing the human's), enabling webhook triggers, human-only settings, \
        project.remove, daemon.stop/upgrade, webhooks.configure, answering approvals (unless approve.from_cli), \
        dismissing items, macOS permissions. Calling one never does it: it becomes a needs-you request (or tells you \
        the request path) and the human decides. Never route around a refusal."),
    ("mcp", "`midna mcp` is a stdio MCP server; agents midna launches get it automatically (setting agents.mcp). Tool \
        names are method names with `.` → `_` (session_open, rule_add, …) plus capabilities, explain and guide. A \
        refused call returns isError with the next step to take."),
];

pub fn topic(name: &str) -> Option<&'static str> {
    let n = name.trim_end_matches('s');
    TOPICS.iter().find(|(k, _)| *k == name || k.trim_end_matches('s') == n || k.replace('-', "_") == name).map(|(_, t)| *t)
}

// ------------------------------------------------------------------ explain

pub type Caller<'a> = &'a mut dyn FnMut(&str, Value) -> Result<Value, RpcError>;

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => String::new(),
        Some(x) => x.to_string(),
    }
}

fn ago(ts: &str) -> String {
    match time::parse_rfc3339(ts) {
        Some(t) => {
            let d = (time::now_unix() - t).max(0);
            if d < 90 {
                format!("{d}s ago")
            } else if d < 5400 {
                format!("{}m ago", d / 60)
            } else if d < 172_800 {
                format!("{}h ago", d / 3600)
            } else {
                format!("{}d ago", d / 86400)
            }
        }
        None => ts.to_string(),
    }
}

fn actor(a: &Value) -> String {
    match (a["kind"].as_str(), a["session"].as_str(), a["name"].as_str()) {
        (Some("human"), ..) => "the human".into(),
        (Some("agent"), Some(sid), _) => format!("an agent in terminal {sid}"),
        (Some("agent"), None, _) => "an agent (outside midna terminals)".into(),
        (Some("trigger"), _, Some(n)) => format!("trigger “{n}”"),
        (Some(k), ..) => k.to_string(),
        _ => "unknown".into(),
    }
}

fn scope_text(sc: &Value) -> String {
    match (sc["kind"].as_str(), sc["id"].as_str()) {
        (Some("global"), _) => "everywhere (global)".into(),
        (Some(k), Some(id)) => format!("in {k} {id}"),
        _ => sc.to_string(),
    }
}

fn state_meaning(state: &str) -> &'static str {
    match state {
        "idle" => "nothing is running for anyone; it is waiting for input",
        "working" => "an agent turn (or command) is in progress",
        "needs_you" => "it is waiting on the human",
        "done" => "the agent finished its turn and the human hasn't looked at it yet",
        "failed" => "its process exited with an error",
        "exited" => "its process exited",
        _ => "",
    }
}

fn reason_source(reason: &str) -> &'static str {
    if reason.contains("(title)") {
        "the terminal title's spinner glyph"
    } else if reason.contains("(screen)") {
        "a check of the screen"
    } else if reason.starts_with("approval") {
        "a midna approval that is waiting for the human"
    } else if reason.starts_with("permission") {
        "the agent's own permission prompt"
    } else if reason.contains("daemon restarted") {
        "a daemon restart (terminals don't survive a crash)"
    } else if reason == "started" {
        "the terminal just starting"
    } else {
        "an agent hook or process event"
    }
}

fn recent_events(call: Caller, filter: Value, limit: u32, keep: impl Fn(&Value) -> bool) -> Vec<Value> {
    let v = call("events.list", json!({ "limit": limit, "filter": filter })).unwrap_or(json!([]));
    let mut list: Vec<Value> = v.as_array().cloned().unwrap_or_default().into_iter().filter(|e| keep(e)).collect();
    let n = list.len();
    if n > 8 {
        list.drain(..n - 8);
    }
    list
}

fn event_line(e: &Value) -> String {
    let mut data = e["data"].clone();
    if let Some(o) = data.as_object_mut() {
        o.remove("since");
    }
    let mut d = data.to_string();
    if d.chars().count() > 110 {
        d = format!("{}…", d.chars().take(110).collect::<String>());
    }
    format!("  {:>8}  {:<22} {:<28} {d}", ago(&s(e, "at")), s(e, "kind"), actor(&e["actor"]))
}

fn find_in(list: &Value, id: &str) -> Option<Value> {
    list.as_array().into_iter().flatten().find(|x| x["id"] == id).cloned()
}

fn explain_session(call: Caller, id: &str, surface: Surface) -> Result<String, RpcError> {
    let x = call("session.get", json!({ "id": id }))?;
    let st = &x["status"];
    let state = s(st, "state");
    let reason = s(st, "reason");
    let kind = match x["agent"].as_str() {
        Some(a) => format!("{a} agent"),
        None => s(&x, "kind"),
    };
    let mut out = format!("{} “{}” — {kind} in project {}, cwd {}\n", id, s(&x, "name"), s(&x, "project_id"), s(&x, "cwd"));
    out.push_str(&format!("status: {state} since {} — {}\n", ago(&s(st, "since")), state_meaning(&state)));
    if !reason.is_empty() {
        out.push_str(&format!("why: reason “{reason}”, set by {}\n", reason_source(&reason)));
    }
    if let Some(c) = x.get("custom_status").filter(|c| c.is_object()) {
        let by = c["trigger_id"].as_str().map(|t| format!(" by trigger {t}")).unwrap_or_default();
        let detail = c["detail"].as_str().map(|d| format!(" — {d}")).unwrap_or_default();
        out.push_str(&format!(
            "shown as: “{}” ({}) on top of {}, set{by} {}{detail}; clears on {}\n",
            s(c, "label"),
            s(c, "color"),
            s(c, "base"),
            ago(&s(c, "since")),
            c["clear_on"].as_str().unwrap_or("prompt")
        ));
    }
    if let Some(c) = x["status"]["exit_code"].as_i64() {
        out.push_str(&format!("exit code: {c}\n"));
    }
    if !s(&x, "title").is_empty() {
        out.push_str(&format!("title: {}\n", s(&x, "title")));
    }
    let needs = call("needs_you.list", json!({ "session_id": id })).unwrap_or(json!([]));
    let needs = needs.as_array().cloned().unwrap_or_default();
    if !needs.is_empty() {
        out.push_str("waiting on the human:\n");
        for n in &needs {
            out.push_str(&format!("  {} {} “{}” — asked by {}, {}\n", s(n, "id"), s(n, "kind"), s(n, "title"), actor(&n["asked_by"]), ago(&s(n, "created_at"))));
        }
        if state != "needs_you" {
            out.push_str("  (the GUI shows this terminal as needs-you because of these items)\n");
        }
    }
    let evs = recent_events(call, json!({ "session_id": id, "kinds": ["session.status", "session.exited", "needs_you.", "agent.turn", "agent.prompt"] }), 500, |_| true);
    if !evs.is_empty() {
        out.push_str("recent:\n");
        for e in &evs {
            out.push_str(&event_line(e));
            out.push('\n');
        }
    }
    let next = match state.as_str() {
        _ if !needs.is_empty() => format!("the human answers {}; you can read the screen with {}", s(&needs[0], "id"), cmd(surface, &format!("read {id} --screen"), "session_read")),
        "failed" => format!("read the output with {}; the human can restart it", cmd(surface, &format!("read {id}"), "session_read")),
        "working" => format!("wait, or watch with {}", cmd(surface, &format!("read {id}"), "session_read")),
        _ => format!("read it with {}, type with {}", cmd(surface, &format!("read {id}"), "session_read"), cmd(surface, &format!("send {id} …"), "session_input")),
    };
    out.push_str(&format!("next: {next}"));
    Ok(out)
}

fn explain_rule(call: Caller, id: &str, surface: Surface) -> Result<String, RpcError> {
    let rules = call("rule.list", json!({}))?;
    let r = find_in(&rules, id).ok_or_else(|| RpcError::not_found(format!("no rule {id} (it may have expired or been removed)")))?;
    let m = &r["matcher"];
    let mut out = format!(
        "{id}: {} {} actions matching `{}` {}\n",
        s(&r, "effect"),
        s(m, "kind"),
        s(m, "pattern"),
        scope_text(&r["scope"])
    );
    out.push_str(&format!("added by {} {}", actor(&r["added_by"]), ago(&s(&r, "added_at"))));
    if let Some(o) = r.get("origin").filter(|o| !o.is_null()) {
        out.push_str(&format!(" (from approving {} with scope {})", s(o, "needs_you_id"), o["approval_scope"]["kind"].as_str().unwrap_or("")));
    }
    out.push('\n');
    if let Some(e) = r["expires_at"].as_str() {
        out.push_str(&format!("expires {e}\n"));
    }
    match r["last_fired_at"].as_str() {
        Some(at) => out.push_str(&format!("fired {} time(s), last {}\n", s(&r, "fired"), ago(at))),
        None => out.push_str("never fired yet\n"),
    }
    let fired = recent_events(call, json!({ "kinds": ["rule.fired"] }), 1000, |e| e["data"]["rule_id"] == id);
    for e in fired.iter().rev().take(5) {
        out.push_str(&format!("  {} matched {} `{}`\n", ago(&s(e, "at")), e["data"]["action"]["kind"].as_str().unwrap_or(""), e["data"]["action"]["value"].as_str().unwrap_or("")));
    }
    out.push_str("how it decides: the narrowest scope with a match wins (session > project > global); within it deny > ask > allow.\n");
    match r.get("removal_request").filter(|x| !x.is_null()) {
        Some(rr) => out.push_str(&format!("removal requested by {}: “{}” — waiting on the human ({})", actor(&rr["requested_by"]), s(rr, "reason"), s(rr, "needs_you_id"))),
        None => out.push_str(&format!(
            "removal: only the human removes rules; agents use {}",
            cmd(surface, &format!("rules request-removal {id} --reason \"…\""), "rule_request_removal")
        )),
    }
    Ok(out)
}

fn action_text(a: &Value) -> String {
    match a["kind"].as_str() {
        Some("start_agent") => format!("starts a {} agent in project {} with the prompt “{}”", s(a, "agent"), s(a, "project_id"), s(a, "prompt_template")),
        Some("run_command") => format!("runs `{}` in a monitor terminal in project {}", s(a, "command"), s(a, "project_id")),
        Some("attention") => format!("raises a note for the human: “{}”", s(a, "message")),
        Some("send_to_session" | "set_status" | "clear_status") => format!("acts on the terminal that fired: {}", crate::triggers::action_text(a)),
        _ => a.to_string(),
    }
}

fn explain_trigger(call: Caller, id: &str, surface: Surface) -> Result<String, RpcError> {
    let list = call("trigger.list", json!({}))?;
    let t = find_in(&list, id).ok_or_else(|| RpcError::not_found(format!("no trigger {id}")))?;
    let local = t["source"] == "local";
    let filters = crate::triggers::filter_text(&t["filter"]);
    let mut out = if local {
        format!("{id} “{}”: when `{}` happens on this Mac", s(&t, "name"), s(&t, "event"))
    } else {
        format!("{id} “{}”: when {} sends `{}`", s(&t, "name"), s(&t, "source"), s(&t, "event"))
    };
    if !filters.is_empty() {
        out.push_str(&format!(" ({filters})"));
    }
    out.push_str(&format!(", it {}.\n", action_text(&t["action"])));
    if local {
        out.push_str(&format!("cooldown: {}s per terminal\n", t["cooldown_secs"].as_u64().unwrap_or(60)));
    }
    if let Some(b) = t["builtin"].as_str() {
        out.push_str(&format!("built in ({b}): ships with midna; edit, pause or remove it like any other\n"));
    }
    out.push_str(&format!("created by {} {}\n", actor(&t["created_by"]), ago(&s(&t, "created_at"))));
    let state = s(&t, "state");
    let st = match state.as_str() {
        "needs_secret" => "needs_secret — waiting for the human to paste the webhook signing secret (human only), then enable it".to_string(),
        "draft" | "paused" if local => format!(
            "{state} — enable with {} (agents may, when the human asked for this trigger)",
            cmd(surface, &format!("triggers enable {id}"), "trigger_set_enabled")
        ),
        "active" if local => "active — it fires on matching local events (events triggers cause never fire triggers)".into(),
        "draft" => "draft — the secret is set; waiting for the human to enable it".into(),
        "active" => "active — it fires on matching verified deliveries".into(),
        "paused" => format!("paused — anyone may keep it paused; enabling asks the human ({})", cmd(surface, &format!("triggers enable {id}"), "trigger_set_enabled")),
        other => other.to_string(),
    };
    out.push_str(&format!("state: {st}\n"));
    match t["last_fired_at"].as_str() {
        Some(at) => out.push_str(&format!("fired {} time(s), last {} {}\n", s(&t, "fired"), ago(at), s(&t, "last_fired_summary"))),
        None => out.push_str("never fired\n"),
    }
    if let Ok(d) = call("trigger.deliveries", json!({ "trigger_id": id, "limit": 3 })) {
        for x in d.as_array().into_iter().flatten() {
            out.push_str(&format!("  {} {} {}: {}\n", s(x, "id"), ago(&s(x, "received_at")), s(x, "verdict"), s(x, "summary")));
        }
    }
    let sample = if local { format!("triggers test {id} --session <terminal-id> --payload '{{\"message\":\"…\"}}'") } else { format!("triggers test {id} --payload sample.json") };
    out.push_str(&format!("try it: {}", cmd(surface, &sample, "trigger_test")));
    Ok(out)
}

fn kind_meaning(kind: &str) -> &'static str {
    match kind {
        "approval" => "a midna approval: an action waits for the human's yes/no",
        "permission_prompt" => "the agent's own permission dialog is open on its screen",
        "blocked" => "an agent says it cannot continue without the human",
        "note" => "an FYI for the human",
        "failed" => "a monitor or agent terminal failed",
        "trigger_waiting" => "a trigger wants to start something and waits for the human",
        "rule_removal" => "an agent asked the human to remove a rule",
        "secret_needed" => "a trigger needs its webhook secret pasted by the human",
        _ => "",
    }
}

fn explain_needs_you(call: Caller, id: &str) -> Result<String, RpcError> {
    let list = call("needs_you.list", json!({}))?;
    let Some(n) = find_in(&list, id) else {
        let evs = recent_events(call, json!({ "kinds": ["needs_you.resolved"] }), 2000, |e| e["data"]["id"] == id || e["data"]["needs_you_id"] == id);
        return match evs.last() {
            Some(e) => Ok(format!("{id} is resolved: {} by {} {}", e["data"]["resolution"]["kind"].as_str().unwrap_or("resolved"), actor(&e["actor"]), ago(&s(e, "at")))),
            None => Err(RpcError::not_found(format!("no open needs-you item {id}"))),
        };
    };
    let kind = s(&n, "kind");
    let mut out = format!("{id}: {kind} — {}\n“{}”\n", kind_meaning(&kind), s(&n, "title"));
    if !s(&n, "detail").is_empty() {
        out.push_str(&format!("{}\n", s(&n, "detail")));
    }
    out.push_str(&format!("asked by {} {}", actor(&n["asked_by"]), ago(&s(&n, "created_at"))));
    if let Some(sid) = n["session_id"].as_str() {
        out.push_str(&format!(", terminal {sid}"));
    }
    out.push('\n');
    if let Some(a) = n.get("approval").filter(|a| !a.is_null()) {
        out.push_str(&format!("action: {} `{}`", s(&a["action"], "kind"), s(&a["action"], "value")));
        if let Some(r) = a["matched_rule"].as_str() {
            out.push_str(&format!(" (rule {r} said ask)"));
        }
        out.push('\n');
    }
    let who = match kind.as_str() {
        "approval" if s(&n, "detail").starts_with("Human-only") => "only the human (it confirms a human-only action)",
        "approval" | "permission_prompt" => "the human; an agent only for its own session when approve.from_cli is on",
        "blocked" | "failed" => "the human (done / restart / dismiss)",
        _ => "the human",
    };
    out.push_str(&format!("who answers: {who}"));
    Ok(out)
}

fn explain_project(call: Caller, id: &str) -> Result<String, RpcError> {
    let list = call("project.list", json!({}))?;
    let p = find_in(&list, id).ok_or_else(|| RpcError::not_found(format!("no project {id}")))?;
    let mut out = format!("{id} “{}” at {}\n", s(&p, "name"), s(&p, "path"));
    let sessions = call("session.list", json!({ "project_id": id }))?;
    for x in sessions.as_array().into_iter().flatten() {
        out.push_str(&format!("  terminal {} {} “{}”\n", s(x, "id"), crate::print::status_text(x), s(x, "name")));
    }
    for c in p["commands"].as_array().into_iter().flatten() {
        out.push_str(&format!("  command “{}”: {}{}\n", s(c, "name"), s(c, "run"), if c["pinned"] == true { " (pinned)" } else { "" }));
    }
    Ok(out.trim_end().to_string())
}

fn explain_delivery(call: Caller, id: &str) -> Result<String, RpcError> {
    let list = call("trigger.deliveries", json!({ "limit": 500 }))?;
    let d = find_in(&list, id).ok_or_else(|| RpcError::not_found(format!("no delivery {id}")))?;
    let mut out = format!("{id}: {} {} — verdict {}: {}\n", s(&d, "source"), s(&d, "event"), s(&d, "verdict"), s(&d, "summary"));
    for e in d["eval"].as_array().into_iter().flatten() {
        out.push_str(&format!("  {}\n", e.as_str().unwrap_or("")));
    }
    Ok(out.trim_end().to_string())
}

fn explain_action(call: Caller, kind: &str, value: &str) -> Result<String, RpcError> {
    let r = call("policy.check", json!({ "action": { "kind": kind, "value": value } }))?;
    let mut out = format!("{kind} `{value}` → {}", s(&r, "decision"));
    match r.get("rule").filter(|x| !x.is_null()) {
        Some(rule) => out.push_str(&format!(
            " by rule {} ({} `{}` {})\n",
            s(rule, "id"),
            s(rule, "effect"),
            s(&rule["matcher"], "pattern"),
            scope_text(&rule["scope"])
        )),
        None => out.push_str(&format!(" by {} (no rule matched; setting policy.default decides)\n", s(&r, "source"))),
    }
    out.push_str("narrowest scope wins (session > project > global); within a scope deny > ask > allow.\n");
    for t in r["trace"].as_array().into_iter().flatten() {
        out.push_str(&format!("  {} {:<10} {}\n", if t["matched"] == true { "✓" } else { "·" }, s(t, "rule_id"), s(t, "reason")));
    }
    Ok(out.trim_end().to_string())
}

pub fn explain_method(name: &str) -> Option<String> {
    let m = catalog().iter().find(|m| m.name == name || m.name.replace('.', "_") == name)?;
    let mut out = format!("{} (MCP tool {})\n{}\n", m.name, m.name.replace('.', "_"), m.description);
    out.push_str(&format!(
        "{}{}\n",
        if m.mutating { "changes state (audited)" } else { "read only" },
        if m.human_only { "; HUMAN ONLY: an agent's call asks the human instead" } else { "" }
    ));
    let params = (m.params)();
    let req: Vec<&str> = params["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if let Some(props) = params["properties"].as_object().filter(|p| !p.is_empty()) {
        out.push_str("params:\n");
        for (k, v) in props {
            let ty = v["type"].as_str().map(str::to_string).or_else(|| v["$ref"].as_str().map(|r| r.rsplit('/').next().unwrap_or("").to_string())).unwrap_or_else(|| "object".into());
            out.push_str(&format!("  {k}{}: {ty}  {}\n", if req.contains(&k.as_str()) { "" } else { "?" }, v["description"].as_str().unwrap_or("").replace('\n', " ")));
        }
    }
    out.push_str(&format!("full schema: midna schema {}", m.name));
    Some(out)
}

fn explain_setting(call: Caller, key: &str) -> Option<String> {
    let spec = setting(key)?;
    let mut out = format!("{key}: {}\n", spec.description);
    out.push_str(&format!("type {:?}, default {}", spec.setting_type(), spec.default.to_json()));
    if let Ok(v) = call("settings.get", json!({ "key": key })) {
        out.push_str(&format!(", current {}", v["value"]));
    }
    out.push('\n');
    out.push_str(if spec.human_only {
        "human only: agents can read it; `midna settings set` from an agent asks the human"
    } else {
        "agents may change it: `midna settings set <key> <value>`"
    });
    Some(out)
}

/// `midna explain …` / the MCP `explain` tool. `args[0]` is the target; a policy kind takes a value.
pub fn explain(call: Caller, args: &[String], surface: Surface) -> Result<String, RpcError> {
    let Some(target) = args.first().map(String::as_str) else {
        return Err(RpcError::bad_params(
            "explain what? pass an id (terminal, r_…, t_…, n_…, p_…, d_…), a method, a setting key, a topic \
             (status, rules, approvals, triggers, needs-you, settings, scripts, themes, windows, human-only, mcp), or <kind> <value>",
        ));
    };
    if matches!(target, "command" | "tool" | "path" | "cli" | "window") && args.len() > 1 {
        return explain_action(call, target, &args[1..].join(" "));
    }
    let hexish = target.len() == 8 && target.chars().all(|c| c.is_ascii_hexdigit());
    match target.split_once('_').map(|(p, _)| p) {
        _ if hexish => return explain_session(call, target, surface),
        Some("r") => return explain_rule(call, target, surface),
        Some("t") => return explain_trigger(call, target, surface),
        Some("n") => return explain_needs_you(call, target),
        Some("p") => return explain_project(call, target),
        Some("d") => return explain_delivery(call, target),
        _ => {}
    }
    if let Some(t) = topic(target) {
        return Ok(t.to_string());
    }
    if let Some(m) = explain_method(target) {
        return Ok(m);
    }
    if let Some(sv) = explain_setting(call, target) {
        return Ok(sv);
    }
    Err(RpcError::not_found(format!(
        "nothing called `{target}`: not an id, method, setting or topic. Topics: {}",
        TOPICS.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_name_the_request_path() {
        let e = RpcError::human_only("rule.remove is human only");
        let h = next_step("rule.remove", &json!({"id": "r_1"}), &e, Surface::Cli).unwrap();
        assert!(h.contains("midna rules request-removal r_1 --reason"), "{h}");
        let h = next_step("rule.remove", &json!({"id": "r_1"}), &e, Surface::Mcp).unwrap();
        assert!(h.contains("rule_request_removal"), "{h}");
        let e = RpcError::human_only("x").with_data(json!({"needs_you_id": "n_9"}));
        assert!(next_step("project.remove", &json!({}), &e, Surface::Cli).unwrap().contains("n_9"));
    }

    #[test]
    fn topics_and_methods_explain_offline() {
        let mut nope = |_: &str, _: Value| -> Result<Value, RpcError> { Err(RpcError::internal("offline")) };
        assert!(explain(&mut nope, &["rules".into()], Surface::Cli).unwrap().contains("request-removal"));
        assert!(explain(&mut nope, &["rule_add".into()], Surface::Cli).unwrap().contains("rule.add"));
        assert!(explain(&mut nope, &["approve.from_cli".into()], Surface::Cli).unwrap().contains("human only"));
        assert!(explain(&mut nope, &["zzz".into()], Surface::Cli).is_err());
    }
}
