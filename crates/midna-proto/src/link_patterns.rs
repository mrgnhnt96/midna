//! `terminal.link_patterns`: text in terminal output that becomes a link. Each rule is
//! `<regex> = <url template> [agents] [project:<name>]…`: what the regex matches is ⌘-clickable
//! and opens the template with `$1`, `${name}` … filled from its capture groups (`$0` is the
//! whole match). `agents` limits a rule to agent terminals; `project:<name>` (repeatable) to
//! terminals in those projects (by name, id or folder).
use regex::Regex;

#[derive(Clone, Debug)]
pub struct LinkPattern {
    pub re: Regex,
    pub template: String,
    pub agents_only: bool,
    pub projects: Vec<String>,
}

impl LinkPattern {
    /// Parse one stored rule (`match = value`).
    pub fn parse(rule: &str) -> Result<LinkPattern, String> {
        let (m, v) = rule.split_once('=').map(|(m, v)| (m.trim(), v.trim())).filter(|(m, v)| !m.is_empty() && !v.is_empty()).ok_or("must look like `<regex> = <url template>`")?;
        let re = Regex::new(m).map_err(|e| format!("bad regex: {}", e.to_string().lines().last().unwrap_or("")))?;
        let mut parts = v.split_whitespace();
        let template = parts.next().unwrap_or("").to_string();
        if !template.contains("://") {
            return Err(format!("`{template}` is not a URL (`<scheme>://…`)"));
        }
        let (mut agents_only, mut projects) = (false, vec![]);
        for p in parts {
            match p.strip_prefix("project:") {
                Some(name) if !name.is_empty() => projects.push(name.to_string()),
                _ if p == "agents" => agents_only = true,
                _ => return Err(format!("unknown option `{p}` (agents, project:<name>)")),
            }
        }
        Ok(LinkPattern { re, template, agents_only, projects })
    }

    /// Whether the rule applies in a terminal: `agent` is an agent terminal, `project` its
    /// project's name, id and folder.
    pub fn applies(&self, agent: bool, project: &[&str]) -> bool {
        (!self.agents_only || agent) && (self.projects.is_empty() || self.projects.iter().any(|p| project.contains(&p.as_str())))
    }

    /// The link for the match around byte `at` of `line`: (byte span of the match, URL). A
    /// match glued to a word, or inside a URL or path, doesn't count (see `standalone`).
    pub fn link_at(&self, line: &str, at: usize) -> Option<((usize, usize), String)> {
        for caps in self.re.captures_iter(line) {
            let m = caps.get(0)?;
            if m.start() > at {
                break;
            }
            if at >= m.end() || m.is_empty() || !standalone(line, m.start(), m.end()) {
                continue;
            }
            let mut url = String::new();
            caps.expand(&self.template, &mut url);
            return Some(((m.start(), m.end()), url));
        }
        None
    }
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
    }

    #[test]
    fn default_rules_parse() {
        let rules: Vec<String> = crate::settings::DEFAULT_LINK_PATTERNS.iter().map(|s| s.to_string()).collect();
        assert_eq!(parse_all(&rules).len(), rules.len());
        let all = parse_all(&rules);
        let hit = |line: &str, at: usize| all.iter().find_map(|p| p.link_at(line, at)).map(|(_, u)| u);
        assert_eq!(hit("Took T42.", 6), Some("taskboard://task/T42".into()));
        assert_eq!(hit("goal G3 done", 6), Some("taskboard://goal/G3".into()));
        assert_eq!(hit("GT3 T", 1), None);
    }
}
