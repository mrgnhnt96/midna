//! Terminals that name themselves (`terminal.auto_name`), without calling any model:
//! - agent: the task summary Claude and Codex already put in the terminal title
//!   (`◑ Auto-rename tabs with summary`, Codex `⠧ ⠧ Fix the login bug | midna`);
//! - prompt: the prompt itself, filler words dropped and cut to a few words;
//! - context: the git branch (`feat/auto-tab-names` → `Auto tab names`), the worktree, the folder.
//!
//! A terminal is only renamed while its name is still its default (`claude`, `zsh`, …) or the
//! last name midna gave it (`Session.auto_name`); `session.rename` clears that, so a name
//! anyone chose stays. Stronger sources replace weaker ones (agent > prompt > context); a
//! source replaces its own earlier name only when `terminal.auto_name_updates` is `follow`.
use crate::agent_state::title_text;
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;

/// Longest name midna gives a terminal (chars); longer ones are cut at a word.
const MAX_CHARS: usize = 40;
/// Most words a prompt name keeps, not counting the small words between them.
const MAX_WORDS: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Agent,
    Prompt,
    Context,
    Off,
}

fn mode(d: &Daemon) -> (Mode, bool) {
    let core = d.core();
    let mode = match core.state.setting_str("terminal.auto_name").as_str() {
        "agent" => Mode::Agent,
        "prompt" => Mode::Prompt,
        "context" => Mode::Context,
        _ => Mode::Off,
    };
    (mode, core.state.setting_str("terminal.auto_name_updates") != "first")
}

/// May `source` name terminal `s` in this mode?
fn allowed(mode: Mode, source: AutoNameSource, s: &Session) -> bool {
    let agent = s.agent.is_some();
    match (mode, source) {
        (Mode::Off, _) => false,
        (Mode::Agent, AutoNameSource::Agent) => agent,
        (Mode::Agent | Mode::Prompt, AutoNameSource::Prompt) => agent,
        // As a fallback the branch only names agents; shells keep their names unless asked.
        (Mode::Agent | Mode::Prompt, AutoNameSource::Context) => agent,
        (Mode::Context, AutoNameSource::Context) => s.kind != SessionKind::Monitor,
        _ => false,
    }
}

/// The names a terminal gets when nobody names it (see `session::open`).
fn is_default_name(s: &Session) -> bool {
    let n = s.name.as_str();
    s.agent.is_some_and(|a| a.as_str() == n)
        || s.adopted.as_ref().is_some_and(|a| a.agent.as_str() == n)
        || s.command.first().is_some_and(|c| c.rsplit('/').next() == Some(n))
        || n == "shell"
}

/// Should `name` from `source` replace terminal `s`'s name? Pure, for tests.
fn should_rename(s: &Session, mode: Mode, follow: bool, source: AutoNameSource, name: &str) -> bool {
    if !allowed(mode, source, s) || name.is_empty() {
        return false;
    }
    let ours = s.auto_name.as_ref().filter(|a| a.name == s.name);
    if ours.is_none() && !is_default_name(s) {
        return false; // someone named it
    }
    // A name from a source this mode no longer uses counts as no name at all.
    match ours.map(|a| a.source).filter(|&cur| allowed(mode, cur, s)) {
        None => true,
        Some(cur) => source > cur || (source == cur && follow && name != s.name),
    }
}

/// Offer a name for terminal `sid`; renames it (event `session.renamed`, `auto: <source>`) if
/// it wins. Returns whether it did.
fn offer(d: &Daemon, sid: &str, source: AutoNameSource, name: Option<String>) -> bool {
    let Some(name) = name else { return false };
    let (mode, follow) = mode(d);
    let (old, project) = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return false };
        if !should_rename(s, mode, follow, source, &name) {
            return false;
        }
        s.auto_name = Some(AutoName { name: name.clone(), source });
        if s.name == name {
            drop(core);
            d.mark_dirty();
            return false;
        }
        (std::mem::replace(&mut s.name, name.clone()), s.project_id.clone())
    };
    d.mark_dirty();
    d.emit(kinds::SESSION_RENAMED, Actor::system(), Some(project), Some(sid.into()), json!({ "name": name, "old": old, "auto": source }));
    true
}

/// The terminal title changed (OSC 0/2).
pub fn on_title(d: &Daemon, sid: &str) {
    let Some((agent, title, cwd)) = d.core().state.session(sid).and_then(|s| Some((s.agent?, s.title.clone(), s.cwd.clone()))) else { return };
    offer(d, sid, AutoNameSource::Agent, from_title(agent, &title, &cwd));
}

/// The human sent a prompt.
pub fn on_prompt(d: &Daemon, sid: &str, prompt: &str) {
    offer(d, sid, AutoNameSource::Prompt, from_prompt(prompt));
}

/// The terminal's git info changed, or it opened.
pub fn on_context(d: &Daemon, sid: &str) {
    let (mode, _) = mode(d);
    let Some(name) = d.core().state.session(sid).map(|s| from_context(s.git.as_ref(), &s.cwd, mode == Mode::Context)) else { return };
    offer(d, sid, AutoNameSource::Context, name);
}

/// `terminal.auto_name` changed: name every terminal from what is known now (title, last
/// prompt, branch), weakest first so the strongest wins.
pub fn refresh_all(d: &Daemon) {
    let ids: Vec<Id> = d.core().state.sessions.iter().map(|s| s.id.clone()).collect();
    for sid in &ids {
        on_context(d, sid);
        if let Some(p) = crate::local::last_prompt(d, sid) {
            on_prompt(d, sid, &p);
        }
        on_title(d, sid);
    }
}

// ------------------------------------------------------------------ sources

/// The agent's own summary from its title, or None for a placeholder (`Claude Code` before the
/// first prompt, Codex's bare `| folder`).
pub fn from_title(agent: AgentKind, title: &str, cwd: &str) -> Option<String> {
    let text = title_text(title);
    // Codex: `<task title> | <folder>`.
    let text = match agent {
        AgentKind::Codex => text.rsplit_once(" | ").map(|(t, _)| t.to_string()).unwrap_or_else(|| if text.starts_with('|') { String::new() } else { text.clone() }),
        AgentKind::Claude => text,
    };
    let text = text.trim();
    let folder = cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let placeholder = ["claude", "claude code", "codex", "openai codex"].iter().any(|p| text.eq_ignore_ascii_case(p)) || text == folder;
    if text.is_empty() || placeholder {
        return None;
    }
    Some(fit(text))
}

/// Branch names that say nothing about the work.
const PLAIN_BRANCHES: &[&str] = &["main", "master", "develop", "dev", "trunk", "head", "staging", "release"];
/// Branch prefixes that say what kind of change it is, not what it is.
const BRANCH_KINDS: &[&str] = &["feat", "feature", "fix", "bugfix", "hotfix", "chore", "refactor", "docs", "test", "tests", "perf", "ci", "build", "style", "wip", "release"];

/// The branch made readable (`feat/auto-tab-names` → `Auto tab names`), else the worktree's
/// name, else (with `folder`) the folder's.
pub fn from_context(git: Option<&GitInfo>, cwd: &str, folder: bool) -> Option<String> {
    let branch = git.map(|g| g.branch.as_str()).unwrap_or("");
    let last = branch.rsplit('/').next().unwrap_or("");
    let branch_name = (!PLAIN_BRANCHES.contains(&last.to_ascii_lowercase().as_str())).then(|| {
        // Keep everything after a kind or user prefix: `mrgnhnt/fix/login-bug` → `login-bug`.
        let parts: Vec<&str> = branch.split('/').collect();
        let from = parts.iter().rposition(|p| BRANCH_KINDS.contains(&p.to_ascii_lowercase().as_str())).map(|i| i + 1).unwrap_or(parts.len().saturating_sub(1));
        readable(&parts[from.min(parts.len())..].join(" "))
    });
    let worktree = git.and_then(|g| g.worktree.as_deref()).map(readable);
    let dir = folder.then(|| cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string());
    [branch_name, worktree, dir].into_iter().flatten().find(|n| !n.is_empty()).map(|n| fit(&n))
}

/// `auto-tab_names` → `Auto tab names`; a ticket id (`ABC-123`) stays as it is.
fn readable(s: &str) -> String {
    let words: Vec<String> = s
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    // Put a ticket id back together: `abc`, `123` → `ABC-123`.
    let mut out: Vec<String> = Vec::new();
    for w in words {
        let ticket = w.chars().all(|c| c.is_ascii_digit()) && out.last().is_some_and(|p| p.len() <= 6 && p.chars().all(|c| c.is_ascii_alphabetic()) && p.chars().any(|c| c.is_ascii_uppercase()));
        if ticket {
            let p = out.pop().unwrap_or_default();
            out.push(format!("{p}-{w}"));
        } else {
            out.push(w);
        }
    }
    capitalize(&out.join(" "))
}

/// Words that carry no meaning in a name: articles, pronouns, helper verbs and the filler
/// people type or dictate around a request.
const FILLER: &[&str] = &[
    "a", "an", "the", "i", "i'd", "i'm", "i've", "i'll", "me", "my", "mine", "we", "we're", "we've", "we'll", "us", "our", "you", "you're", "your", "yours", "it", "it's", "its",
    "they", "them", "their", "he", "she", "him", "her", "this", "that", "these", "those", "there", "here", "what", "what's", "which", "who", "how", "why", "where", "do", "does", "did",
    "doing", "done", "is", "are", "was", "were", "be", "been", "being", "am", "have", "has", "had", "having", "can", "could", "would", "should", "will", "shall", "may", "might", "must",
    "please", "pls", "ok", "okay", "hey", "hi", "hello", "so", "just", "really", "actually", "basically", "um", "uh", "yeah", "yes", "no", "nope", "well", "now", "then", "also", "maybe",
    "kind", "sort", "thing", "things", "stuff", "some", "any", "all", "want", "wanna", "need", "needs", "think", "guess", "know", "let", "let's", "lets", "go", "ahead", "options",
    "option", "idea", "ideas", "sound", "sounds", "good", "great", "nice", "cool", "thanks", "thank", "too", "very", "quite", "much", "many", "more", "as", "far", "able", "try",
    "trying", "like", "liked", "something", "anything", "everything", "way", "ways", "same", "other", "another", "still", "again", "already", "right", "sure", "dont", "don't",
    "i'd", "id", "im", "ive", "youre", "were", "lets", "gonna", "got", "get", "getting", "see", "look", "looking", "take", "help", "tell", "give", "us", "around", "about",
];
/// Small words kept only between two kept words (`setting for auto names`).
const LINKS: &[&str] = &["in", "on", "for", "to", "of", "with", "from", "into", "by", "and", "or", "at", "per", "vs"];
/// Words that start a side clause: the name stops there once it has two words.
const BREAKS: &[&str] = &["without", "because", "since", "so", "but", "when", "if", "unless", "while", "although", "though", "whereas", "except", "instead", "otherwise", "which", "that"];

/// The prompt cut to its request: filler dropped, the first clause kept, at most `MAX_WORDS`
/// words. None for a slash command or a reply too short to name anything (`yes`, `do those`).
pub fn from_prompt(prompt: &str) -> Option<String> {
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt.starts_with('/') || prompt.starts_with('!') {
        return None;
    }
    // The first sentence that names something; pasted text and code below it don't count.
    sentences(prompt).into_iter().take(4).find_map(sentence_name)
}

/// Split at `?`, `!`, line breaks and a `.` that ends a word (`auth.rs` stays whole).
fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let next_blank = chars.peek().is_none_or(|&(_, n)| n.is_whitespace());
        if matches!(c, '?' | '!' | '\n') || (c == '.' && next_blank) {
            out.push(&text[start..i]);
            start = i + c.len_utf8();
        }
    }
    out.push(&text[start..]);
    out.retain(|s| !s.trim().is_empty());
    out
}

/// A token that names something concrete: a file, path, id, number or code word.
fn concrete(w: &str) -> bool {
    w.contains(['/', '_', '#', ':']) || w.chars().any(|c| c.is_ascii_digit()) || (w.contains('.') && !w.ends_with('.')) || w.starts_with('`')
}

fn sentence_name(sentence: &str) -> Option<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut words = 0;
    let mut pending: Vec<String> = Vec::new(); // links waiting for the next kept word
    for raw in sentence.split_whitespace() {
        // A comma, semicolon or dash ends the first clause once it named something.
        let ends_clause = raw.ends_with([',', ';', ':']) && !raw.contains("://");
        let w = raw.trim_matches(|c: char| !(c.is_alphanumeric() || "/_#.-'`".contains(c))).trim_end_matches('.').trim_matches('`');
        if w.is_empty() || w == "-" || w == "—" {
            if words >= 2 && (raw == "-" || raw == "—" || raw == "–") {
                break;
            }
            continue;
        }
        let lower = w.to_lowercase();
        let lower = lower.trim_end_matches("'s");
        if words >= 2 && BREAKS.contains(&lower) {
            break;
        }
        if !concrete(w) && FILLER.contains(&lower) {
            continue;
        }
        if !concrete(w) && LINKS.contains(&lower) {
            if words > 0 {
                pending.push(w.to_string());
            }
            continue;
        }
        if !concrete(w) && BREAKS.contains(&lower) {
            continue;
        }
        kept.append(&mut pending);
        kept.push(w.to_string());
        words += 1;
        if words >= MAX_WORDS || (ends_clause && words >= 2) {
            break;
        }
    }
    (words >= 2).then(|| fit(&capitalize(&kept.join(" "))))
}

/// At most `MAX_CHARS`, cut at a word.
fn fit(s: &str) -> String {
    let s = s.trim().to_string();
    if s.chars().count() <= MAX_CHARS {
        return s;
    }
    let cut: String = s.chars().take(MAX_CHARS).collect();
    let at = cut.rfind(' ').filter(|&i| i > MAX_CHARS / 2).unwrap_or(cut.len());
    format!("{}…", cut[..at].trim_end())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use AutoNameSource::{Agent, Context, Prompt};

    #[test]
    fn prompts_shorten_to_their_request() {
        let cases = [
            ("What options do we have as far as renaming tabs automatically without using the user's whole prompt?", Some("Renaming tabs automatically")),
            ("can you fix the login bug in auth.rs", Some("Fix login bug in auth.rs")),
            ("Add a setting for auto names", Some("Add setting for auto names")),
            ("Can you make the sidebar wider so it fits long names", Some("Make sidebar wider")),
            ("please look into ABC-123, the build fails on CI", Some("ABC-123 build fails on CI")),
            ("the release workflow is failing again - can you check it", Some("Release workflow failing")),
            ("Okay, all of those ideas sound good to me. Do those.", None),
            ("yes", None),
            ("continue", None),
            ("/compact", None),
            ("", None),
        ];
        for (p, want) in cases {
            assert_eq!(from_prompt(p).as_deref(), want, "{p}");
        }
    }

    #[test]
    fn long_names_are_cut_at_a_word() {
        let n = from_prompt("refactor the extraordinarily complicated notification preferences synchronisation layer").unwrap();
        assert!(n.chars().count() <= MAX_CHARS + 1, "{n}");
        assert!(n.ends_with('…'), "{n}");
    }

    #[test]
    fn titles_drop_spinners_folders_and_placeholders() {
        use AgentKind::{Claude, Codex};
        assert_eq!(from_title(Claude, "◑ Auto-rename tabs with summary", "/x/midna").as_deref(), Some("Auto-rename tabs with summary"));
        assert_eq!(from_title(Claude, "✳ Phrase replacement feature design", "/x").as_deref(), Some("Phrase replacement feature design"));
        assert_eq!(from_title(Claude, "✳ Claude Code", "/x"), None);
        assert_eq!(from_title(Claude, "", "/x"), None);
        assert_eq!(from_title(Codex, "⠧ ⠧ Fix the login bug | midna", "/x/midna").as_deref(), Some("Fix the login bug"));
        assert_eq!(from_title(Codex, "⠧ ⠧ | midna", "/x/midna"), None);
        assert_eq!(from_title(Codex, "midna", "/x/midna"), None);
    }

    fn git(branch: &str, worktree: Option<&str>) -> GitInfo {
        GitInfo { branch: branch.into(), ahead: 0, behind: 0, added: 0, removed: 0, files: 0, pr: None, worktree: worktree.map(str::to_string) }
    }

    #[test]
    fn branches_read_as_names() {
        let c = |b: &str, folder| from_context(Some(&git(b, None)), "/x/midna", folder);
        assert_eq!(c("feat/auto-tab-names", false).as_deref(), Some("Auto tab names"));
        assert_eq!(c("mrgnhnt/fix/login_bug", false).as_deref(), Some("Login bug"));
        assert_eq!(c("ABC-123-retry-uploads", false).as_deref(), Some("ABC-123 retry uploads"));
        assert_eq!(c("main", false), None);
        assert_eq!(c("main", true).as_deref(), Some("midna"), "a folder keeps its spelling");
        assert_eq!(from_context(Some(&git("main", Some("wt-search"))), "/x", false).as_deref(), Some("Wt search"));
        assert_eq!(from_context(None, "/x/midna", false), None);
    }

    fn session(name: &str, agent: Option<AgentKind>, auto: Option<(&str, AutoNameSource)>) -> Session {
        let mut s: Session = serde_json::from_value(json!({
            "id": "s1", "project_id": "p", "name": name, "kind": if agent.is_some() { "agent" } else { "shell" }, "agent": agent,
            "cwd": "/x", "command": [agent.map(|a| a.as_str()).unwrap_or("/bin/zsh")],
            "status": { "state": "idle", "since": "2026-01-01T00:00:00Z" }, "created_at": "2026-01-01T00:00:00Z", "last_activity_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        s.auto_name = auto.map(|(n, source)| AutoName { name: n.into(), source });
        s
    }

    #[test]
    fn only_default_or_own_names_change() {
        let claude = Some(AgentKind::Claude);
        assert!(should_rename(&session("claude", claude, None), Mode::Agent, true, Agent, "Fix bug"));
        // Someone named it.
        assert!(!should_rename(&session("Slow dictations", claude, None), Mode::Agent, true, Agent, "Fix bug"));
        // midna named it, then someone renamed it (rename clears auto_name, but stale state too).
        assert!(!should_rename(&session("Mine", claude, Some(("Fix bug", Agent))), Mode::Agent, true, Agent, "Other"));
        // Off names nothing.
        assert!(!should_rename(&session("claude", claude, None), Mode::Off, true, Agent, "Fix bug"));
    }

    #[test]
    fn stronger_sources_win_and_first_keeps_a_name() {
        let claude = Some(AgentKind::Claude);
        let by = |src| session("Old", claude, Some(("Old", src)));
        assert!(should_rename(&by(Context), Mode::Agent, false, Prompt, "New"));
        assert!(should_rename(&by(Prompt), Mode::Agent, false, Agent, "New"));
        assert!(!should_rename(&by(Agent), Mode::Agent, true, Prompt, "New"));
        assert!(should_rename(&by(Agent), Mode::Agent, true, Agent, "New"), "follow");
        assert!(!should_rename(&by(Agent), Mode::Agent, false, Agent, "New"), "first");
        // prompt mode ignores the agent's title, and a name from it can be replaced.
        assert!(!should_rename(&by(Prompt), Mode::Prompt, true, Agent, "New"));
        assert!(should_rename(&by(Agent), Mode::Prompt, false, Prompt, "New"));
    }

    #[test]
    fn shells_are_named_only_in_context_mode() {
        let zsh = session("zsh", None, None);
        assert!(!should_rename(&zsh, Mode::Agent, true, Context, "Midna"));
        assert!(should_rename(&zsh, Mode::Context, true, Context, "Midna"));
        assert!(!should_rename(&zsh, Mode::Context, true, Prompt, "Fix bug"));
    }
}
