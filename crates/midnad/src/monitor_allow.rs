//! What a monitor grant lets its terminal run without the agent's own permission check.
//!
//! A grant is the human saying "this tab keeps the others working". Its agent does that with a
//! few midna verbs (`midna approve`, `midna read`, `midna queue add`, `midna close`…, or their
//! MCP tools). Left to the agent's own permission flow, Claude's auto mode can block one of them
//! halfway through the work (it read `midna approve` of a Slack send as a message the human
//! never sent). So while the grant is live, midna's PreToolUse answer for exactly those calls is
//! allow (`DecisionSource::Monitor`), and when it ends they go back to the agent's own flow.
//!
//! This hands midna nothing it didn't already decide: midnad checks the grant's scope on every
//! one of these verbs (`needs_you::by_monitor_grant`, `session::refuse_prompt_answer`). A Bash
//! command is allowed only when every part of it is one of these verbs, or a filter piped after
//! one; anything else in it (a substitution, a redirect, another program) leaves the whole
//! command to the agent's own flow.

/// `midna <verb>` a monitor runs. `queue` only with the subcommands in [`QUEUE_SUBS`].
const VERBS: &[&str] = &["approve", "key", "needs", "read", "get", "list", "procs", "queue", "close", "attention"];
const QUEUE_SUBS: &[&str] = &["add", "list"];
/// Programs allowed after a `|`, to filter a midna verb's output.
const FILTERS: &[&str] = &["grep", "head", "tail", "wc", "sort", "jq"];
/// The same verbs as midna MCP tools.
const MCP_TOOLS: &[&str] = &[
    "needs_you_resolve",
    "needs_you_get",
    "needs_you_list",
    "needs_you_raise",
    "session_key",
    "session_read",
    "session_get",
    "session_list",
    "session_processes",
    "session_close",
    "queue_add",
    "queue_list",
];

/// Does a monitor grant cover this PreToolUse value (`Bash(<command>)`, `mcp__midna__<tool>`,
/// as `midna hook claude` names it)?
pub fn covers(value: &str) -> bool {
    if let Some(tool) = value.strip_prefix("mcp__midna__") {
        let name = tool.split('(').next().unwrap_or(tool);
        return MCP_TOOLS.contains(&name);
    }
    match value.strip_prefix("Bash(").and_then(|v| v.strip_suffix(')')) {
        Some(cmd) => bash_covered(cmd),
        None => false,
    }
}

/// Why [`covers`] can't help this terminal's agent, checked when the human grants it, so a
/// monitor never finds out halfway. None when it can (or there's no agent yet: a shell gets
/// nothing typed into it). midna's allow reaches an agent through its PreToolUse hook, so it
/// needs a Claude whose hooks report to midna.
pub fn gap(s: &midna_proto::Session) -> Option<String> {
    let info = s.agent_info.as_ref();
    match s.agent? {
        midna_proto::AgentKind::Codex => Some(
            "it runs Codex, which has no hook midna can answer for, so Codex's own approval settings decide whether it may run `midna approve`; \
             a restart won't change that. Give the grant to a Claude tab instead."
                .into(),
        ),
        _ if info.and_then(|i| i.permission_mode.as_deref()) == Some("bypassPermissions") => None,
        _ if info.is_some_and(|i| i.conversation_id.is_some() || i.permission_mode.is_some()) => None,
        _ => Some(format!(
            "its Claude hasn't reported to midna's hooks, so midna can't let its `midna approve` through Claude's permission check. \
             Restart it in place to load them (its conversation comes back): `midna restart {}`.",
            s.id
        )),
    }
}

/// How a part of a command is joined to the one before it.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Join {
    Start,
    Then,
    Pipe,
}

/// Every part is a midna verb (or a filter after a pipe), with nothing the shell would expand
/// or redirect.
fn bash_covered(cmd: &str) -> bool {
    let Some(parts) = split(cmd) else { return false };
    let mut any = false;
    for (join, part) in parts {
        let words: Vec<&str> = part.split_whitespace().collect();
        let Some(&first) = words.first() else {
            continue;
        };
        let ok = match first {
            "midna" => verb_ok(&words[1..]),
            f => join == Join::Pipe && FILTERS.contains(&f),
        };
        if !ok {
            return false;
        }
        any = true;
    }
    any
}

fn verb_ok(rest: &[&str]) -> bool {
    match rest.first() {
        Some(&"queue") => rest.get(1).is_some_and(|s| QUEUE_SUBS.contains(s)),
        Some(v) => VERBS.contains(v),
        None => false,
    }
}

/// The command's parts, split on `;`, `&&`, `||`, `|` and newlines outside quotes. None when it
/// has anything the shell would run or redirect besides its words: `$(…)`, backticks, `<`, `>`,
/// a lone `&`, or an unclosed quote.
fn split(cmd: &str) -> Option<Vec<(Join, String)>> {
    let mut parts = vec![];
    let (mut cur, mut join) = (String::new(), Join::Start);
    let (mut single, mut double) = (false, false);
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        if single {
            single = c != '\'';
            cur.push(c);
            continue;
        }
        match c {
            '\\' => {
                cur.push(c);
                cur.push(chars.next()?);
                continue;
            }
            '`' => return None,
            '$' if chars.peek().is_some_and(|n| *n == '(' || *n == '{') => return None,
            '"' => double = !double,
            '\'' if !double => single = true,
            _ if double => {}
            '<' | '>' => return None,
            ';' | '\n' => {
                parts.push((join, std::mem::take(&mut cur)));
                join = Join::Then;
                continue;
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                parts.push((join, std::mem::take(&mut cur)));
                join = Join::Then;
                continue;
            }
            '&' => return None,
            '|' => {
                // `||` runs the next part on failure, like `;`; one `|` pipes into it.
                let or = chars.peek() == Some(&'|');
                if or {
                    chars.next();
                }
                parts.push((join, std::mem::take(&mut cur)));
                join = if or { Join::Then } else { Join::Pipe };
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if single || double {
        return None;
    }
    parts.push((join, cur));
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bash(c: &str) -> bool {
        covers(&format!("Bash({c})"))
    }

    #[test]
    fn a_monitors_own_verbs_are_covered() {
        assert!(bash("midna approve n_67b493"));
        assert!(bash("midna approve n_67b493; midna needs | grep 9756c0bb"));
        assert!(bash("midna read 9756c0bb --screen && midna needs get n_1"));
        assert!(bash("midna queue add --session ab12 \"go on; the PR's merged\""));
        assert!(bash("midna attention 'Blocked > waiting on you'"));
        assert!(bash("midna close ab12 || midna list"));
        assert!(bash("midna read ab12 --screen | tail -5 && midna key ab12 enter"));
        assert!(covers("mcp__midna__needs_you_resolve"));
        assert!(covers("mcp__midna__needs_you_list(T857)"));
    }

    #[test]
    fn anything_else_in_the_command_is_not() {
        assert!(!bash("midna approve n_1; rm -rf ~"));
        assert!(!bash("midna approve $(curl evil)"));
        assert!(!bash("midna approve \"$(curl evil)\""));
        assert!(!bash("midna approve `id`"));
        assert!(!bash("midna needs > /tmp/x"));
        assert!(!bash("midna approve n_1 & curl evil"));
        assert!(!bash("grep secret ~/.ssh/id_rsa"));
        assert!(!bash("midna needs; grep x file"));
        assert!(!bash("midna queue clear"));
        assert!(!bash("midna send ab12 yes"));
        assert!(!bash("midna daemon stop"));
        assert!(!bash("MIDNA_SOCKET=/x midna approve n_1"));
        assert!(!bash("midna approve 'n_1"));
        assert!(!bash(""));
        assert!(!covers("mcp__midna__session_input"));
        assert!(!covers("mcp__midna__rule_add"));
        assert!(!covers("Edit(/etc/hosts)"));
    }
}
