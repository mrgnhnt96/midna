//! Tiny argument parser: positionals, `--flag`, `--key value` / `--key=value`, and `--`.
use std::collections::HashMap;

/// Flags that take a value (everything else starting with `--` is boolean).
const VALUE_FLAGS: &[&str] = &[
    "since", "limit", "lines", "project", "cwd", "name", "prompt", "resume", "agent", "monitor", "scope", "expires", "reason", "range", "by",
    "kind", "detail", "socket", "timeout", "session", "value", "target",
    "source", "event", "repo", "branch", "action", "label", "run", "attention", "hook-id", "session-name", "trigger", "payload",
    "port", "relay-url", "bucket", "icon", "title", "sub", "keywords", "rpc", "params", "screen", "prefill", "focus", "danger",
    "featured", "id", "image", "why", "jump", "for", "as", "volume", "turn",
    // local triggers
    "in-project", "for-agent", "idle-for", "cron", "match", "send", "send-no-enter", "set-status", "color", "base", "clear-on", "cooldown",
    "action-json", "filter-json", "notify", "notify-body",
];

/// Flags that take a value only when one follows (`read --screen` vs `commands add --screen S`).
const OPTIONAL_VALUE_FLAGS: &[&str] = &["screen"];

#[derive(Debug, Default)]
pub struct Args {
    pub pos: Vec<String>,
    flags: HashMap<String, Option<String>>,
    /// Every value of a repeated flag, in order (`--image a --image b`).
    multi: HashMap<String, Vec<String>>,
    /// Every valued flag in command-line order (`--send a --send-no-enter b --send c`).
    seq: Vec<(String, String)>,
    /// Everything after `--`.
    pub rest: Vec<String>,
}

#[derive(Debug)]
pub struct ArgError(pub String);

impl Args {
    pub fn parse(raw: impl IntoIterator<Item = String>) -> Result<Args, ArgError> {
        Args::parse_with(raw, &[])
    }

    /// `parse`, with `extra` also taking a value (flags one verb uses differently, e.g. `queue --idle 10m`).
    pub fn parse_with(raw: impl IntoIterator<Item = String>, extra: &[&str]) -> Result<Args, ArgError> {
        let mut a = Args::default();
        let mut it = raw.into_iter().peekable();
        while let Some(s) = it.next() {
            if s == "--" {
                a.rest = it.by_ref().collect();
                break;
            }
            if let Some(name) = s.strip_prefix("--") {
                let (name, inline) = match name.split_once('=') {
                    Some((n, v)) => (n.to_string(), Some(v.to_string())),
                    None => (name.to_string(), None),
                };
                let optional = OPTIONAL_VALUE_FLAGS.contains(&name.as_str());
                let v = if optional && inline.is_none() {
                    it.next_if(|next| !next.starts_with('-'))
                } else if VALUE_FLAGS.contains(&name.as_str()) || extra.contains(&name.as_str()) {
                    Some(match inline {
                        Some(v) => v,
                        None => it.next().ok_or_else(|| ArgError(format!("--{name} needs a value")))?,
                    })
                } else {
                    inline
                };
                if let Some(v) = &v {
                    a.multi.entry(name.clone()).or_default().push(v.clone());
                    a.seq.push((name.clone(), v.clone()));
                }
                a.flags.insert(name, v);
            } else if s == "-f" {
                a.flags.insert("follow".into(), None);
            } else if s == "-h" {
                a.flags.insert("help".into(), None);
            } else {
                a.pos.push(s);
            }
        }
        Ok(a)
    }

    pub fn has(&self, name: &str) -> bool {
        self.flags.contains_key(name)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.flags.get(name).and_then(|v| v.as_deref())
    }

    /// All values of a flag that may repeat.
    pub fn all(&self, name: &str) -> &[String] {
        self.multi.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Values of any of `names`, in command-line order, with the flag each came from.
    pub fn ordered(&self, names: &[&str]) -> Vec<(&str, &str)> {
        self.seq.iter().filter(|(k, _)| names.contains(&k.as_str())).map(|(k, v)| (k.as_str(), v.as_str())).collect()
    }

    pub fn num<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>, ArgError> {
        match self.get(name) {
            None => Ok(None),
            Some(v) => v.parse().map(Some).map_err(|_| ArgError(format!("--{name}: `{v}` is not a number"))),
        }
    }

    /// Positional `i` or a usage error mentioning `what`.
    pub fn need(&self, i: usize, what: &str) -> Result<&str, ArgError> {
        self.pos.get(i).map(String::as_str).ok_or_else(|| ArgError(format!("missing {what}")))
    }

    /// Free text: everything after `--` if given (so it may contain flags), else positionals from `from`.
    pub fn text(&self, from: usize) -> String {
        if !self.rest.is_empty() {
            return self.rest.join(" ");
        }
        self.pos.get(from..).map(|p| p.join(" ")).unwrap_or_default()
    }

    /// Unknown flags (anything not in `allowed`).
    pub fn check(&self, allowed: &[&str]) -> Result<(), ArgError> {
        for k in self.flags.keys() {
            if !allowed.contains(&k.as_str()) && !matches!(k.as_str(), "json" | "help" | "socket") {
                return Err(ArgError(format!("unknown flag --{k}")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses() {
        let a = Args::parse(["read", "abc", "--lines", "5", "--json", "--scope=always", "--", "x", "--y"].map(String::from)).unwrap();
        assert_eq!(a.pos, ["read", "abc"]);
        assert_eq!(a.get("lines"), Some("5"));
        assert!(a.has("json"));
        assert_eq!(a.get("scope"), Some("always"));
        assert_eq!(a.rest, ["x", "--y"]);
        assert!(Args::parse(["--lines".to_string()]).is_err());
    }

    #[test]
    fn repeats_and_optional_values() {
        let a = Args::parse(["send", "x", "--image", "a.png", "--image=b.png", "hi"].map(String::from)).unwrap();
        assert_eq!(a.all("image"), ["a.png", "b.png"]);
        assert_eq!(a.pos, ["send", "x", "hi"]);
        let a = Args::parse(["read", "x", "--screen"].map(String::from)).unwrap();
        assert!(a.has("screen") && a.get("screen").is_none());
        let a = Args::parse(["read", "x", "--screen", "--json"].map(String::from)).unwrap();
        assert!(a.has("screen") && a.has("json"));
        let a = Args::parse(["commands", "add", "--screen", "rules"].map(String::from)).unwrap();
        assert_eq!(a.get("screen"), Some("rules"));
    }

    #[test]
    fn ordered_across_flags() {
        let a = Args::parse(["t", "--send", "/compact", "--send-no-enter=x", "--send", "{{last_prompt}}"].map(String::from)).unwrap();
        assert_eq!(a.ordered(&["send", "send-no-enter"]), [("send", "/compact"), ("send-no-enter", "x"), ("send", "{{last_prompt}}")]);
        assert_eq!(a.all("send"), ["/compact", "{{last_prompt}}"]);
    }
}
