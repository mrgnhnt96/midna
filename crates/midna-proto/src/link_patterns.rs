//! `terminal.link_patterns`: text in terminal output that becomes a link. Each rule is
//! `<regex> = <url template> [agents] [project:<name>]…`: what the regex matches is ⌘-clickable
//! and opens the template with `$1`, `${name}` … filled from its capture groups (`$0` is the
//! whole match). `agents` limits a rule to agent terminals; `project:<name>` (repeatable) to
//! terminals in those projects (by name, id or folder). `preview:"<command> $1 …"` names a
//! command whose JSON (`{"title", "lines", …}`) fills the hover card; its words are filled from
//! the captures one by one and it runs without a shell.
use regex::{Captures, Regex};

#[derive(Clone, Debug)]
pub struct LinkPattern {
    pub re: Regex,
    pub template: String,
    pub agents_only: bool,
    pub projects: Vec<String>,
    /// The preview command's argv template (empty: none). The program never holds a capture.
    pub preview: Vec<String>,
}

impl LinkPattern {
    /// Parse one stored rule (`match = value`).
    pub fn parse(rule: &str) -> Result<LinkPattern, String> {
        let (m, v) = rule.split_once('=').map(|(m, v)| (m.trim(), v.trim())).filter(|(m, v)| !m.is_empty() && !v.is_empty()).ok_or("must look like `<regex> = <url template>`")?;
        let re = Regex::new(m).map_err(|e| format!("bad regex: {}", e.to_string().lines().last().unwrap_or("")))?;
        let mut parts = words(v)?.into_iter();
        let template = parts.next().unwrap_or_default();
        if !template.contains("://") {
            return Err(format!("`{template}` is not a URL (`<scheme>://…`)"));
        }
        let (mut agents_only, mut projects, mut preview) = (false, vec![], vec![]);
        for p in parts {
            if let Some(cmd) = p.strip_prefix("preview:") {
                preview = cmd.split_whitespace().map(str::to_string).collect();
                match preview.first() {
                    None => return Err("`preview:` needs a command, like `preview:\"jira-card $1\"`".into()),
                    Some(prog) if prog.contains('$') => return Err(format!("the preview program `{prog}` can't come from a capture")),
                    _ => {}
                }
                continue;
            }
            match p.strip_prefix("project:") {
                Some(name) if !name.is_empty() => projects.push(name.to_string()),
                _ if p == "agents" => agents_only = true,
                _ => return Err(format!("unknown option `{p}` (agents, project:<name>, preview:\"<command>\")")),
            }
        }
        Ok(LinkPattern { re, template, agents_only, projects, preview })
    }

    /// Whether the rule applies in a terminal: `agent` is an agent terminal, `project` its
    /// project's name, id and folder.
    pub fn applies(&self, agent: bool, project: &[&str]) -> bool {
        (!self.agents_only || agent) && (self.projects.is_empty() || self.projects.iter().any(|p| project.contains(&p.as_str())))
    }

    /// The link for the match around byte `at` of `line`: (byte span of the match, URL). A
    /// match glued to a word, or inside a URL or path, doesn't count (see `standalone`).
    pub fn link_at(&self, line: &str, at: usize) -> Option<((usize, usize), String)> {
        let caps = self.captures_at(line, at)?;
        let m = caps.get(0)?;
        let mut url = String::new();
        caps.expand(&self.template, &mut url);
        Some(((m.start(), m.end()), url))
    }

    /// The preview command for the match around byte `at` of `line`, each word filled from the
    /// captures (a capture with spaces stays one argument). None when the rule has no preview.
    pub fn preview_at(&self, line: &str, at: usize) -> Option<Vec<String>> {
        if self.preview.is_empty() {
            return None;
        }
        let caps = self.captures_at(line, at)?;
        Some(self.preview.iter().map(|w| {
            let mut arg = String::new();
            caps.expand(w, &mut arg);
            arg
        }).collect())
    }

    fn captures_at<'l>(&self, line: &'l str, at: usize) -> Option<Captures<'l>> {
        for caps in self.re.captures_iter(line) {
            let m = caps.get(0)?;
            if m.start() > at {
                break;
            }
            if at >= m.end() || m.is_empty() || !standalone(line, m.start(), m.end()) {
                continue;
            }
            return Some(caps);
        }
        None
    }
}

/// A rule's value split into words: whitespace separates them, except inside `"…"` or `'…'`
/// (the quotes are dropped; `\"` inside double quotes is a quote).
fn words(v: &str) -> Result<Vec<String>, String> {
    let (mut out, mut cur, mut quote, mut started) = (vec![], String::new(), None, false);
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '"' | '\'') => (quote, started) = (Some(c), true),
            (None, c) if c.is_whitespace() => {
                if started {
                    out.push(std::mem::take(&mut cur));
                }
                started = false;
            }
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => match chars.next() {
                Some(n @ ('"' | '\\')) => cur.push(n),
                Some(n) => cur.extend(['\\', n]),
                None => cur.push('\\'),
            },
            (_, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if let Some(q) = quote {
        return Err(format!("unclosed `{q}`"));
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// Every valid rule of a `terminal.link_patterns` value, in order (bad ones are skipped: the
/// setting refuses them, so only a hand-edited file has any).
pub fn parse_all(rules: &[String]) -> Vec<LinkPattern> {
    rules.iter().filter_map(|r| LinkPattern::parse(r).ok()).collect()
}

/// `line[a..b]` stands on its own: the word it's in (between spaces) isn't a URL or path, and
/// it isn't glued to the text around it (`xT42`, `T42_x`, `T42.rs`, `T42:12`). Trailing
/// punctuation before a space or the end (`T42.`, `T42:`) is fine.
pub fn standalone(line: &str, a: usize, b: usize) -> bool {
    let ws = |c: char| c.is_whitespace() || c == '\0';
    let start = line[..a].rfind(ws).map(|i| i + line[i..].chars().next().map_or(1, char::len_utf8)).unwrap_or(0);
    let end = line[b..].find(ws).map(|i| b + i).unwrap_or(line.len());
    let word = &line[start..end];
    if word.contains("://") || word.contains('/') || word.contains('\\') {
        return false;
    }
    let glued = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '#' | '$' | '%' | '~' | '=' | '+' | '&');
    if line[..a].chars().next_back().is_some_and(glued) {
        return false;
    }
    let mut after = line[b..].chars();
    match after.next() {
        None => true,
        Some(c) if c.is_alphanumeric() || matches!(c, '_' | '-' | '@' | '=' | '+' | '&' | '#') => false,
        // `T42.rs`, `T42:12`: a file or a position, unless the punctuation ends the word.
        Some('.' | ':') => !after.next().is_some_and(|c| c.is_alphanumeric() || c == '_'),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(rule: &str, line: &str, at: usize) -> Option<String> {
        LinkPattern::parse(rule).unwrap().link_at(line, at).map(|(_, u)| u)
    }

    const TASK: &str = r"\b([TG])(\d+)\b = taskboard://${1}/$1$2";
    const T: &str = r"\b(T\d+)\b = taskboard://task/$1";

    #[test]
    fn builds_the_url_from_captures() {
        assert_eq!(link(T, "on T42 now", 4), Some("taskboard://task/T42".into()));
        assert_eq!(link(TASK, "G3", 0), Some("taskboard://G/G3".into()));
        assert_eq!(link(r"PROJ-(?P<n>\d+) = https://x.atlassian.net/browse/PROJ-${n}", "see PROJ-12", 6), Some("https://x.atlassian.net/browse/PROJ-12".into()));
        assert_eq!(link(r"#(\d+) = https://github.com/o/r/issues/$1 agents", "fixes #7", 6), Some("https://github.com/o/r/issues/7".into()));
        assert_eq!(link(r"T\d+ = taskboard://task/$0", "T9", 1), Some("taskboard://task/T9".into()));
    }

    #[test]
    fn only_the_match_under_the_pointer() {
        let line = "T1 and T22";
        assert_eq!(link(T, line, 0), Some("taskboard://task/T1".into()));
        assert_eq!(link(T, line, 8), Some("taskboard://task/T22".into()));
        assert_eq!(link(T, line, 4), None);
        assert_eq!(LinkPattern::parse(T).unwrap().link_at(line, 9).map(|(s, _)| s), Some((7, 10)));
    }

    #[test]
    fn word_boundaries() {
        for (line, at) in [("xT42", 2), ("T42x", 0), ("T42_a", 0), ("a_T42", 3), ("T420", 0)] {
            assert_eq!(link(r"(T\d+) = taskboard://task/$1", line, at).filter(|u| u.ends_with("T42")), None, "{line}");
        }
        for (line, at) in [("(T42)", 2), ("T42.", 1), ("T42:", 1), ("T42, G3", 0), ("`T42`", 1), ("\"T42\"", 2), ("[T42]", 1), ("T42…", 0)] {
            assert_eq!(link(T, line, at), Some("taskboard://task/T42".into()), "{line}");
        }
    }

    #[test]
    fn not_inside_urls_or_paths() {
        for (line, at) in [
            ("https://example.com/T42", 21),
            ("taskboard://task/T42", 18),
            ("src/T42.rs", 5),
            ("T42/notes.md", 0),
            ("T42.rs", 0),
            ("notes/T42", 7),
            ("T42:12", 0),
            ("C:\\T42", 4),
            ("v1.T42", 4),
            ("--goal=T42", 8),
            ("user@T42", 6),
        ] {
            assert_eq!(link(r"(T\d+) = taskboard://task/$1", line, at), None, "{line}");
        }
    }

    #[test]
    fn parse_options_and_errors() {
        let p = LinkPattern::parse(r"\b(T\d+)\b = taskboard://task/$1 agents project:midna project:p_1").unwrap();
        assert!(p.agents_only);
        assert_eq!(p.projects, ["midna", "p_1"]);
        assert!(p.applies(true, &["midna", "p_9", "/x"]));
        assert!(!p.applies(false, &["midna"]));
        assert!(!p.applies(true, &["other"]));
        assert!(LinkPattern::parse(T).unwrap().applies(false, &[]));
        assert!(LinkPattern::parse(r"(T\d+ = taskboard://task/$1").unwrap_err().contains("bad regex"));
        assert!(LinkPattern::parse(r"T\d+ = task/$0").unwrap_err().contains("not a URL"));
        assert!(LinkPattern::parse(r"T\d+ = taskboard://task/$0 shells").unwrap_err().contains("unknown option"));
        assert!(LinkPattern::parse(r"T\d+").is_err());
        assert!(p.preview.is_empty());
    }

    #[test]
    fn preview_commands() {
        let p = LinkPattern::parse(r#"\b(RRD-\d+)\b = https://x.atlassian.net/browse/$1 agents preview:"jira-card $1 --short" project:roon"#).unwrap();
        assert_eq!(p.template, "https://x.atlassian.net/browse/$1");
        assert_eq!(p.preview, ["jira-card", "$1", "--short"]);
        assert!(p.agents_only);
        assert_eq!(p.projects, ["roon"]);
        assert_eq!(p.link_at("see RRD-12.", 5).map(|(_, u)| u), Some("https://x.atlassian.net/browse/RRD-12".into()));
        assert_eq!(p.preview_at("see RRD-12.", 5), Some(vec!["jira-card".into(), "RRD-12".into(), "--short".into()]));
        assert_eq!(p.preview_at("see RRD-12.", 1), None);
        // A capture with spaces stays one argument; nothing goes through a shell.
        let q = LinkPattern::parse(r"<([^>]+)> = https://x.com/q preview:'card --name=$1'").unwrap();
        assert_eq!(q.preview_at("<a b; rm>", 2), Some(vec!["card".into(), "--name=a b; rm".into()]));
        assert_eq!(LinkPattern::parse(r"(T\d+) = myapp://item/$1 preview:item-card").unwrap().preview_at("T4", 0), Some(vec!["item-card".into()]));
        assert_eq!(LinkPattern::parse(T).unwrap().preview_at("T4", 0), None);
        assert!(LinkPattern::parse(r"(T\d+) = myapp://item/$1 preview:").unwrap_err().contains("needs a command"));
        assert!(LinkPattern::parse(r#"(T\d+) = myapp://item/$1 preview:"$1 x""#).unwrap_err().contains("can't come from a capture"));
        assert!(LinkPattern::parse(r#"(T\d+) = myapp://item/$1 preview:"card $1"#).unwrap_err().contains("unclosed"));
        assert_eq!(words(r#"a "b \"c\" d" e'f g'"#).unwrap(), ["a", "b \"c\" d", "ef g"]);
    }

    #[test]
    fn default_rules_parse() {
        // midna ships no patterns of its own; apps register theirs.
        assert!(crate::settings::DEFAULT_LINK_PATTERNS.is_empty());
        let rules: Vec<String> = [r"\b(T\d+)\b = taskboard://task/$1 agents", r"\b(G\d+)\b = taskboard://goal/$1 agents"].iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_all(&rules).len(), rules.len());
        let all = parse_all(&rules);
        let hit = |line: &str, at: usize| all.iter().find_map(|p| p.link_at(line, at)).map(|(_, u)| u);
        assert_eq!(hit("Took T42.", 6), Some("taskboard://task/T42".into()));
        assert_eq!(hit("goal G3 done", 6), Some("taskboard://goal/G3".into()));
        assert_eq!(hit("GT3 T", 1), None);
    }
}
