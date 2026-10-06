//! Agent status state machine (from spikes/agent-status, proven against real Claude Code and
//! Codex hook traces), plus the OSC title glyph heuristic for gaps no hook covers.
use midna_proto::StatusState;
use serde_json::Value;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// `Bash(git push)`, `Edit(/path/file.rs)`, or just the tool name (same form as policy values).
pub fn tool_label(p: &Value) -> String {
    let name = p.get("tool_name").and_then(Value::as_str).unwrap_or("tool");
    let input = p.get("tool_input").cloned().unwrap_or(Value::Null);
    let arg = ["command", "file_path", "notebook_path", "path", "url", "pattern", "query"].iter().find_map(|k| input.get(k).and_then(Value::as_str));
    match arg {
        Some(a) => format!("{name}({})", a.chars().take(160).collect::<String>()),
        None => name.to_string(),
    }
}

/// What a `PermissionRequest` asks: `question: <text>` for AskUserQuestion (it's a question
/// dialog, not a permission), otherwise `permission <tool_label>`.
pub fn prompt_label(p: &Value) -> String {
    if s(p, "tool_name") == "AskUserQuestion" {
        let q = p.pointer("/tool_input/questions/0/question").and_then(Value::as_str).map(str::trim).filter(|q| !q.is_empty());
        return match q {
            Some(q) => format!("question: {}", q.chars().take(160).collect::<String>()),
            None => "question".into(),
        };
    }
    format!("permission {}", tool_label(p))
}

/// Pure transition: (current state, hook event, payload) -> next (state, reason). None = no change.
/// Shared by Claude Code and Codex (Codex reuses Claude's hook names; `agent-turn-complete` is
/// Codex's legacy notify).
pub fn transition(cur: StatusState, event: &str, p: &Value) -> Option<(StatusState, String)> {
    use StatusState::*;
    let needs_you = cur == NeedsYou;
    // Subagent-originated tool events carry agent_id; they keep the parent working but must
    // never resolve a parent's needs-you.
    let from_subagent = p.get("agent_id").is_some();
    let next = match event {
        "SessionStart" => (Idle, "session started".to_string()),
        "UserPromptSubmit" => (Working, "prompt".into()),
        "PreToolUse" => {
            if needs_you {
                return None; // a parallel tool starting doesn't answer the prompt
            }
            (Working, format!("tool {}", s(p, "tool_name")))
        }
        "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
            if needs_you && from_subagent {
                return None;
            }
            (Working, format!("after {}", s(p, "tool_name")))
        }
        "PermissionRequest" => (NeedsYou, prompt_label(p)),
        "Elicitation" => (NeedsYou, "elicitation".into()),
        "Notification" => match s(p, "notification_type") {
            "permission_prompt" | "elicitation_dialog" => (NeedsYou, s(p, "message").to_string()),
            // idle_prompt fires ~60s after Stop: confirms done, never "needs you" by itself
            "idle_prompt" => {
                if matches!(cur, Done | Idle) {
                    return None;
                }
                (Done, "idle".into())
            }
            _ => return None,
        },
        "SubagentStart" | "SubagentStop" | "TaskCreated" | "TaskCompleted" => {
            if matches!(cur, NeedsYou | Done | Idle) {
                return None;
            }
            (Working, event.to_lowercase())
        }
        "PreCompact" => (Working, "compacting".into()),
        "Stop" => (Done, "turn finished".into()),
        "StopFailure" => (Failed, format!("{} {}", s(p, "error"), s(p, "error_details")).trim().to_string()),
        "SessionEnd" => (Idle, "agent session ended".into()),
        "agent-turn-complete" => (Done, "turn finished".into()),
        _ => return None,
    };
    if next.0 == cur { None } else { Some(next) }
}

/// What the terminal title's leading glyph says about an agent. Claude Code animates a spinner
/// while working and shows `✳` when stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum TitleHint {
    Working,
    Stopped,
    Unknown,
}

/// Agent-aware hint. Codex animates a Braille spinner while working and simply drops it when
/// the turn ends (no stopped glyph), so a non-empty Codex title without a spinner is Stopped.
pub fn agent_title_hint(agent: midna_proto::AgentKind, title: &str) -> TitleHint {
    match (title_hint(title), agent) {
        (TitleHint::Unknown, midna_proto::AgentKind::Codex) if !title.trim().is_empty() => TitleHint::Stopped,
        (h, _) => h,
    }
}

fn is_title_glyph(c: char) -> bool {
    matches!(c, '◐' | '◑' | '◒' | '◓' | '✳' | '✻' | '\u{2801}'..='\u{28FF}')
}

/// The title with every spinner/status glyph removed, so animation frames compare equal.
/// Codex 0.160.0 puts the spinner in twice (`⠧ ⠧ | dir`), Claude once (`◐ Task`).
pub fn title_text(t: &str) -> String {
    t.split_whitespace().filter(|w| !w.chars().all(is_title_glyph)).collect::<Vec<_>>().join(" ")
}

pub fn title_hint(title: &str) -> TitleHint {
    let Some(c) = title.trim_start().chars().next() else { return TitleHint::Unknown };
    match c {
        '◐' | '◑' | '◒' | '◓' => TitleHint::Working,
        // Braille spinner frames used by several TUIs.
        '\u{2801}'..='\u{28FF}' => TitleHint::Working,
        '✳' | '✻' => TitleHint::Stopped,
        _ => TitleHint::Unknown,
    }
}

/// Screen check: does the visible screen show an agent *permission* prompt (one where Enter
/// means "yes" and Esc means "no")? Only the bottom of the screen counts, where the dialogs
/// render, so the same words quoted in the transcript don't match. Tuned on real screens
/// (tests/fixtures/*.screen.txt): Claude Code 2.1.288's tool dialogs ("Do you want to proceed?",
/// "Do you want to make this edit to …?" over a `1. Yes` list) and Codex's approval overlay.
/// Startup dialogs (Claude's folder trust, Codex's hooks review) must NOT match: Enter there
/// selects "No, exit" / "Review hooks".
pub fn screen_shows_prompt(lines: &[String]) -> bool {
    let tail: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let tail = &tail[tail.len().saturating_sub(25)..];
    let has = |p: &str| tail.iter().any(|l| l.contains(p));
    let claude = has("Do you want to") && tail.iter().any(|l| l.trim_start_matches(['❯', '>', ' ']).starts_with("1. Yes"));
    let codex = has("Would you like to run the following command") || has("Would you like to make the following edits") || has("Yes, proceed") || has("Allow command?");
    claude || codex
}

/// An agent's "do you trust this folder?" startup dialog, as found on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustDialog {
    /// Position of the "Yes …" and "No …" choices in the list, and of the highlighted one.
    pub yes: usize,
    pub no: Option<usize>,
    pub cursor: Option<usize>,
}

/// Screen check for the folder-trust dialog Claude Code (and Codex) show before they start in a
/// folder they haven't seen. No hook fires while it is up, so only the screen shows that the
/// agent waits on the human. The wording changes between versions ("Do you trust the files in
/// this folder?" over `1. Yes, proceed` / `2. No, exit`; 2.1.288: "Is this a project you created
/// or one you trust?" over `❯ No, exit` / `Yes, I trust this folder`), so this looks for a
/// trust question about a folder/files/directory above a Yes/No choice list, in the bottom of
/// the screen only.
pub fn screen_shows_trust(lines: &[String]) -> Option<TrustDialog> {
    let tail: Vec<&str> = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let tail = &tail[tail.len().saturating_sub(25)..];
    let asks = tail.iter().any(|l| {
        let l = l.to_lowercase();
        l.contains("trust") && (l.contains("folder") || l.contains("files") || l.contains("directory") || l.contains("project you created"))
    });
    if !asks {
        return None;
    }
    let (mut yes, mut no, mut cursor, mut n) = (None, None, None, 0usize);
    for l in tail {
        let marked = l.starts_with(['❯', '›', '>']);
        let rest = l.trim_start_matches(['❯', '›', '>', ' ']);
        // An optional `1.` before the choice.
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let rest = if digits > 0 && rest[digits..].starts_with('.') { rest[digits + 1..].trim_start() } else { rest };
        let is_yes = rest.starts_with("Yes");
        let is_no = rest == "No" || rest.starts_with("No,") || rest.starts_with("No ");
        if !is_yes && !is_no {
            continue;
        }
        if is_yes && yes.is_none() {
            yes = Some(n);
        }
        if is_no && no.is_none() {
            no = Some(n);
        }
        if marked {
            cursor = Some(n);
        }
        n += 1;
    }
    Some(TrustDialog { yes: yes?, no, cursor })
}

/// Keys that pick "Yes" (`approve`) or "No" in a trust dialog: arrows from the highlighted
/// choice, then Enter. With no "No" choice on screen, deny is Esc.
pub fn trust_keys(t: &TrustDialog, approve: bool) -> Vec<crate::term::Key> {
    use crate::term::Key;
    let Some(to) = (if approve { Some(t.yes) } else { t.no }) else { return vec![Key::Escape] };
    let from = t.cursor.unwrap_or(0);
    let step = if to < from { Key::Up } else { Key::Down };
    let mut keys = vec![step; from.abs_diff(to)];
    keys.push(Key::Enter);
    keys
}

/// The screen shows something only the human should answer: a permission prompt or a
/// folder-trust dialog. Queued messages and title heuristics hold off while it is up.
pub fn screen_waits_on_human(lines: &[String]) -> bool {
    screen_shows_prompt(lines) || screen_shows_trust(lines).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use StatusState::*;

    #[test]
    fn claude_flow() {
        let p = json!({});
        assert_eq!(transition(Idle, "UserPromptSubmit", &p).unwrap().0, Working);
        let n = json!({"notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"});
        assert_eq!(transition(Working, "Notification", &n).unwrap().0, NeedsYou);
        assert!(transition(NeedsYou, "PreToolUse", &json!({"tool_name": "Bash"})).is_none());
        assert!(transition(NeedsYou, "PostToolUse", &json!({"agent_id": "x"})).is_none());
        assert_eq!(transition(NeedsYou, "PostToolUse", &json!({"tool_name": "Bash"})).unwrap().0, Working);
        assert_eq!(transition(Working, "Stop", &p).unwrap().0, Done);
        assert!(transition(Done, "Notification", &json!({"notification_type": "idle_prompt"})).is_none());
    }

    fn screen(name: &str) -> Vec<String> {
        let p = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}")).lines().map(str::to_string).collect()
    }

    #[test]
    fn real_screens() {
        assert!(screen_shows_prompt(&screen("claude-2.1.288-permission.screen.txt")));
        assert!(!screen_shows_prompt(&screen("claude-2.1.288-interrupted.screen.txt")));
        assert!(!screen_shows_prompt(&screen("claude-2.1.288-trust-folder.screen.txt")), "Enter there would exit");
        assert!(!screen_shows_prompt(&screen("codex-0.160.0-idle.screen.txt")));
        assert!(!screen_shows_prompt(&screen("codex-0.160.0-hooks-review.screen.txt")), "Enter there opens the review");
        // Quoted in the transcript, far above the input box: not a live prompt.
        let mut quoted = vec!["Do you want to proceed?".to_string(), "1. Yes".to_string()];
        quoted.extend(screen("claude-2.1.288-interrupted.screen.txt"));
        quoted.extend((0..30).map(|i| format!("line {i}")));
        assert!(!screen_shows_prompt(&quoted));
    }

    #[test]
    fn trust_dialogs() {
        use crate::term::Key::*;
        // Claude Code 2.1.288: "No, exit" first and highlighted.
        let t = screen_shows_trust(&screen("claude-2.1.288-trust-folder.screen.txt")).expect("trust dialog");
        assert_eq!(t, TrustDialog { yes: 1, no: Some(0), cursor: Some(0) });
        assert_eq!(trust_keys(&t, true), vec![Down, Enter]);
        assert_eq!(trust_keys(&t, false), vec![Enter]);
        assert!(screen_waits_on_human(&screen("claude-2.1.288-trust-folder.screen.txt")));
        // Older wording: numbered, "Yes, proceed" first.
        let old: Vec<String> = ["Do you trust the files in this folder?", "", "/tmp/x", "", "❯ 1. Yes, proceed", "  2. No, exit", "", "Enter to confirm · Esc to exit"]
            .map(str::to_string)
            .to_vec();
        let t = screen_shows_trust(&old).expect("old trust dialog");
        assert_eq!(t, TrustDialog { yes: 0, no: Some(1), cursor: Some(0) });
        assert_eq!(trust_keys(&t, true), vec![Enter]);
        assert_eq!(trust_keys(&t, false), vec![Down, Enter]);
        // Not a trust dialog: permission prompts, idle screens, the words without a choice list.
        assert!(screen_shows_trust(&screen("claude-2.1.288-permission.screen.txt")).is_none());
        assert!(screen_shows_trust(&screen("claude-2.1.288-interrupted.screen.txt")).is_none());
        assert!(screen_shows_trust(&screen("codex-0.160.0-idle.screen.txt")).is_none());
        assert!(screen_shows_trust(&["I trust the files in this folder now".to_string(), "> ".to_string()]).is_none());
        // Quoted far above the input box: not live.
        let mut quoted = old.clone();
        quoted.extend((0..30).map(|i| format!("line {i}")));
        assert!(screen_shows_trust(&quoted).is_none());
    }

    #[test]
    fn real_titles() {
        use midna_proto::AgentKind::{Claude, Codex};
        // Claude Code 2.1.288: `◐`/`◑` while working, `✳` when stopped (also while its
        // permission dialog is up).
        assert_eq!(agent_title_hint(Claude, "◑ Touch c.txt and list"), TitleHint::Working);
        assert_eq!(agent_title_hint(Claude, "✳ Touch c.txt and list"), TitleHint::Stopped);
        assert_eq!(agent_title_hint(Claude, ""), TitleHint::Unknown);
        // Codex 0.160.0: Braille spinner (twice) while working, plain title when stopped.
        assert_eq!(agent_title_hint(Codex, "⠧ ⠧ | mrgnhnt96"), TitleHint::Working);
        assert_eq!(agent_title_hint(Codex, "⠸ Run echo midna-codex-two | mrgnhnt96"), TitleHint::Working);
        assert_eq!(agent_title_hint(Codex, "Run echo midna-codex-two | mrgnhnt96"), TitleHint::Stopped);
        assert_eq!(agent_title_hint(Codex, ""), TitleHint::Unknown);
        // Spinner frames compare equal, so they don't flood the event log.
        assert_eq!(title_text("⠧ ⠧ | mrgnhnt96"), title_text("⠏ ⠏ | mrgnhnt96"));
        assert_eq!(title_text("◐ Fix tests"), title_text("✳ Fix tests"));
        assert_ne!(title_text("⠸ mrgnhnt96"), title_text("⠸ Run echo | mrgnhnt96"));
    }

    #[test]
    fn permission_request_names_the_action() {
        let p = json!({"tool_name": "Bash", "tool_input": {"command": "touch c.txt && ls"}});
        assert_eq!(transition(Working, "PermissionRequest", &p).unwrap(), (NeedsYou, "permission Bash(touch c.txt && ls)".to_string()));
    }

    #[test]
    fn ask_user_question_is_a_question() {
        let p = json!({"tool_name": "AskUserQuestion", "tool_input": {"questions": [{"question": "What should the bell open?", "header": "Bell opens"}]}});
        assert_eq!(transition(Working, "PermissionRequest", &p).unwrap(), (NeedsYou, "question: What should the bell open?".to_string()));
        assert_eq!(prompt_label(&json!({"tool_name": "AskUserQuestion"})), "question");
    }

    #[test]
    fn titles() {
        assert_eq!(title_hint("✳ Claude Code"), TitleHint::Stopped);
        assert_eq!(title_hint("◐ Fixing tests"), TitleHint::Working);
        assert_eq!(title_hint("⠂ Working"), TitleHint::Working);
        assert_eq!(title_hint("zsh"), TitleHint::Unknown);
    }
}
