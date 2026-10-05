//! An agent's command line (Claude Code, Codex), read the way the agent's own parser reads it
//! (commander for Claude, clap for Codex), so midna can tell what a typed `claude …` or
//! `codex …` asks for: an interactive session (which midna may run in a shell terminal,
//! `session.adopt`), a subcommand or a one-shot run (left alone), and which words are the
//! first prompt (dropped when the session is resumed, or it would be sent again).
//!
//! The option table comes from the binary's `--help`: `<x>` takes one value, `<x...>` /
//! `<x>...` take every following word up to the next option, `[x]` takes the next word unless
//! it is an option. A copy of each agent's help is built in for when the installed binary's
//! can't be read.
use crate::AgentKind;
use std::collections::{HashMap, HashSet};

/// `claude --help` of Claude Code 2.1.289.
pub const BUILTIN_CLAUDE_HELP: &str = include_str!("claude-2.1.289-help.txt");
/// `codex --help` of Codex 0.160.0.
pub const BUILTIN_CODEX_HELP: &str = include_str!("codex-0.160.0-help.txt");

pub fn builtin_help(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => BUILTIN_CLAUDE_HELP,
        AgentKind::Codex => BUILTIN_CODEX_HELP,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arity {
    Flag,
    One,
    Optional,
    Many,
}

/// The options and subcommands one `--help` lists.
#[derive(Clone, Debug, Default)]
pub struct Spec {
    /// Every spelling (`-r`, `--resume`) to (the canonical long name, its arity).
    options: HashMap<String, (String, Arity)>,
    commands: HashSet<String>,
}

/// One word of a command line, as the parser reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    /// An option (by its canonical name), and how many of the following words are its values.
    Opt { name: String, values: usize },
    /// A value of the option before it.
    Value,
    /// A positional word: the subcommand when it comes first and names one, else the prompt
    /// (or a subcommand's own arguments).
    Positional,
}

/// What a typed command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub words: Vec<Word>,
    /// The subcommand (`mcp`, `exec`, `resume`, …) when there is one.
    pub command: Option<String>,
}

impl Spec {
    pub fn from_help(help: &str) -> Spec {
        let mut s = Spec::default();
        let mut in_commands = false;
        for line in help.lines() {
            if !line.starts_with(' ') && !line.is_empty() {
                in_commands = line.trim_end().ends_with("Commands:");
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            let entry = line.trim_start();
            // The entry ends at its description, two or more spaces later (or the next line).
            let head = entry.split("  ").next().unwrap_or("").trim();
            if in_commands {
                if indent == 2 {
                    let name = head.split_whitespace().next().unwrap_or("");
                    s.commands.extend(name.split('|').filter(|n| !n.is_empty()).map(str::to_string));
                }
                // clap: `exec   Run Codex non-interactively [aliases: e]`, also on a wrapped line.
                if let Some(a) = entry.split("[alias").nth(1).and_then(|r| r.split_once(':')).map(|(_, r)| r.trim_end_matches(']')) {
                    s.commands.extend(a.split(',').map(|x| x.trim().trim_end_matches(']').to_string()).filter(|x| !x.is_empty()));
                }
            } else if !in_commands && indent <= 6 && head.starts_with('-') {
                // `-r, --resume [value]`, `--allowedTools, --allowed-tools <tools...>`,
                // `-i, --image <FILE>...`: names are comma-separated, the argument comes last.
                let (names, arg) = match head.find([' ', '<', '[']) {
                    _ if head.contains(", ") => {
                        let mut parts: Vec<&str> = head.split(", ").collect();
                        let last = parts.pop().unwrap_or("");
                        let (n, a) = last.split_once(' ').unwrap_or((last, ""));
                        parts.push(n);
                        (parts, a.trim())
                    }
                    Some(i) => (vec![head[..i].trim()], head[i..].trim()),
                    None => (vec![head], ""),
                };
                let arity = match arg.as_bytes().first() {
                    Some(b'<') if arg.contains("...") => Arity::Many,
                    Some(b'<') => Arity::One,
                    Some(b'[') if arg.contains("...") => Arity::Many,
                    Some(b'[') => Arity::Optional,
                    _ => Arity::Flag,
                };
                let names: Vec<&str> = names.into_iter().filter(|n| n.starts_with('-')).collect();
                // The last long spelling is the canonical one (`--bg, --background`).
                let long = names.iter().rev().find(|n| n.starts_with("--")).or(names.first()).map(|n| n.to_string()).unwrap_or_default();
                for n in names {
                    s.options.insert(n.to_string(), (long.clone(), arity));
                }
            }
        }
        s
    }

    pub fn builtin(agent: AgentKind) -> Spec {
        Spec::from_help(builtin_help(agent))
    }

    /// The option table is plausible (a help text that didn't parse has none of these).
    pub fn is_usable(&self, agent: AgentKind) -> bool {
        let need: &[&str] = match agent {
            AgentKind::Claude => &["--resume", "--model", "--print"],
            AgentKind::Codex => &["--model", "--config", "--sandbox"],
        };
        need.iter().all(|o| self.options.contains_key(*o))
    }

    pub fn parse(&self, args: &[String]) -> Parsed {
        let mut words = Vec::with_capacity(args.len());
        let mut command = None;
        let mut seen_positional = false;
        let mut i = 0;
        let is_opt = |a: &str| a.starts_with('-') && a.len() > 1;
        while i < args.len() {
            let a = args[i].as_str();
            if a == "--" {
                words.push(Word::Opt { name: "--".into(), values: 0 });
                words.extend((i + 1..args.len()).map(|_| Word::Positional));
                break;
            }
            if !is_opt(a) {
                if !seen_positional && self.commands.contains(a) {
                    command = Some(a.to_string());
                }
                seen_positional = true;
                words.push(Word::Positional);
                i += 1;
                continue;
            }
            let (spelled, inline) = match a.split_once('=') {
                Some((n, _)) if a.starts_with("--") => (n, true),
                _ => (a, false),
            };
            let (name, arity) = self.options.get(spelled).cloned().unwrap_or_else(|| (spelled.to_string(), Arity::Flag));
            let values = if inline {
                0
            } else {
                let rest = &args[i + 1..];
                match arity {
                    Arity::Flag => 0,
                    Arity::One => rest.len().min(1),
                    Arity::Optional => usize::from(rest.first().is_some_and(|n| !is_opt(n))),
                    Arity::Many => rest.iter().take_while(|n| !is_opt(n)).count(),
                }
            };
            words.push(Word::Opt { name, values });
            words.extend((0..values).map(|_| Word::Value));
            i += 1 + values;
        }
        Parsed { words, command }
    }
}

impl Parsed {
    pub fn has(&self, names: &[&str]) -> bool {
        self.words.iter().any(|w| matches!(w, Word::Opt { name, .. } if names.contains(&name.as_str())))
    }

    /// An interactive session in this terminal. Claude: no subcommand, and nothing that prints
    /// and exits or runs elsewhere (background, desktop, cloud). Codex: no subcommand but
    /// `resume` / `fork` (which open the TUI), and no `--help` / `--version`.
    pub fn interactive(&self, agent: AgentKind) -> bool {
        match agent {
            AgentKind::Claude => self.command.is_none() && !self.has(&["--print", "--help", "--version", "--background", "--desktop", "--cloud", "--environment"]),
            AgentKind::Codex => matches!(self.command.as_deref(), None | Some("resume" | "fork")) && !self.has(&["--help", "--version"]),
        }
    }
}

/// `args` minus the prompt (and, for a Codex `resume` / `fork`, the subcommand and its session
/// id), the options that pick a conversation, and `--worktree`, which would make a new
/// worktree: what reopening the conversation by id keeps of how it was started.
pub fn resume_args(agent: AgentKind, spec: &Spec, args: &[String]) -> Vec<String> {
    let drop: &[&str] = match agent {
        AgentKind::Claude => &["--continue", "--resume", "--fork-session", "--from-pr", "--session-id", "--worktree", "--tmux"],
        AgentKind::Codex => &["--last", "--all", "--include-non-interactive", "--worktree"],
    };
    let parsed = spec.parse(args);
    let mut out = vec![];
    let mut dropping = false;
    for (a, w) in args.iter().zip(&parsed.words) {
        match w {
            Word::Opt { name, .. } => {
                dropping = drop.contains(&name.as_str()) || name == "--";
                if !dropping {
                    out.push(a.clone());
                }
            }
            Word::Value if !dropping => out.push(a.clone()),
            Word::Value | Word::Positional => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentKind::{Claude, Codex};

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn reads_claudes_help() {
        let s = Spec::builtin(Claude);
        assert!(s.is_usable(Claude));
        assert_eq!(s.options["-r"], ("--resume".into(), Arity::Optional));
        assert_eq!(s.options["--model"], ("--model".into(), Arity::One));
        assert_eq!(s.options["--mcp-config"], ("--mcp-config".into(), Arity::Many));
        assert_eq!(s.options["--allowedTools"], ("--allowed-tools".into(), Arity::Many));
        assert_eq!(s.options["--bg"], ("--background".into(), Arity::Flag));
        assert_eq!(s.options["-p"], ("--print".into(), Arity::Flag));
        assert!(s.commands.contains("mcp") && s.commands.contains("plugins") && s.commands.contains("kill"));
        assert!(!Spec::from_help("nothing useful").is_usable(Claude));
    }

    #[test]
    fn reads_codexs_help() {
        let s = Spec::builtin(Codex);
        assert!(s.is_usable(Codex));
        assert_eq!(s.options["-c"], ("--config".into(), Arity::One));
        assert_eq!(s.options["--enable"], ("--enable".into(), Arity::One), "a long option indented under the short ones");
        assert_eq!(s.options["-i"], ("--image".into(), Arity::Many));
        assert_eq!(s.options["--search"], ("--search".into(), Arity::Flag));
        assert_eq!(s.options["-V"], ("--version".into(), Arity::Flag));
        assert!(s.commands.contains("exec") && s.commands.contains("e") && s.commands.contains("resume") && s.commands.contains("a"));
        assert!(!s.options.contains_key("-"), "description bullets aren't options");
    }

    #[test]
    fn interactive_or_not() {
        let c = Spec::builtin(Claude);
        let i = |a: &[&str]| c.parse(&v(a)).interactive(Claude);
        assert!(i(&[]));
        assert!(i(&["fix the flaky test"]));
        assert!(i(&["--model", "opus", "--dangerously-skip-permissions"]));
        assert!(i(&["-r", "abc"]));
        assert!(i(&["--model", "mcp"]), "a value, not the subcommand");
        assert!(!i(&["mcp", "list"]));
        assert!(!i(&["-p", "hi"]));
        assert!(!i(&["--version"]));
        assert!(!i(&["-h"]));
        assert!(!i(&["--bg", "do it"]));

        let x = Spec::builtin(Codex);
        let i = |a: &[&str]| x.parse(&v(a)).interactive(Codex);
        assert!(i(&[]));
        assert!(i(&["-m", "gpt-5", "fix it"]));
        assert!(i(&["resume", "--last"]));
        assert!(i(&["fork", "abc"]));
        assert!(!i(&["-c", "model=\"o3\"", "exec"]), "after -c's value, exec is the subcommand");
        assert!(!i(&["exec", "do it"]));
        assert!(!i(&["e", "do it"]), "an alias");
        assert!(!i(&["mcp", "list"]));
        assert!(!i(&["--version"]));
        assert!(!i(&["resume", "--help"]));
    }

    #[test]
    fn claude_resume_keeps_flags_and_drops_the_prompt() {
        let s = Spec::builtin(Claude);
        let r = |a: &[&str]| resume_args(Claude, &s, &v(a));
        assert_eq!(r(&["--model", "opus", "fix the bug"]), v(&["--model", "opus"]));
        assert_eq!(r(&["fix the bug", "--dangerously-skip-permissions"]), v(&["--dangerously-skip-permissions"]));
        assert_eq!(r(&["-c"]), v(&[]));
        assert_eq!(r(&["-r", "abc", "--model", "opus"]), v(&["--model", "opus"]));
        assert_eq!(r(&["-r", "--model", "opus"]), v(&["--model", "opus"]), "-r without an id opens the picker");
        assert_eq!(r(&["--resume=abc", "-n", "api work"]), v(&["-n", "api work"]));
        assert_eq!(r(&["--session-id", "u-1", "hi"]), v(&[]));
        assert_eq!(r(&["-w", "feat-x", "--model", "opus"]), v(&["--model", "opus"]), "a resume must not make a new worktree");
        // A variadic option takes the words after it, as Claude reads it.
        assert_eq!(r(&["--add-dir", "../a", "../b"]), v(&["--add-dir", "../a", "../b"]));
        assert_eq!(r(&["--add-dir", "../a", "--", "hi"]), v(&["--add-dir", "../a"]));
        assert_eq!(r(&["-d", "api", "--settings", "x.json", "go"]), v(&["-d", "api", "--settings", "x.json"]));
    }

    #[test]
    fn codex_resume_keeps_flags_and_drops_the_prompt() {
        let s = Spec::builtin(Codex);
        let r = |a: &[&str]| resume_args(Codex, &s, &v(a));
        assert_eq!(r(&["-m", "gpt-5", "fix the bug"]), v(&["-m", "gpt-5"]));
        assert_eq!(r(&["-c", "model_reasoning_effort=\"high\"", "--search", "hi"]), v(&["-c", "model_reasoning_effort=\"high\"", "--search"]));
        assert_eq!(r(&["resume", "--last", "-s", "workspace-write"]), v(&["-s", "workspace-write"]));
        assert_eq!(r(&["fork", "019a-uuid", "and now this", "--full-auto"]), v(&["--full-auto"]));
        assert_eq!(r(&["-i", "a.png", "b.png", "--", "what is this"]), v(&["-i", "a.png", "b.png"]));
    }
}
