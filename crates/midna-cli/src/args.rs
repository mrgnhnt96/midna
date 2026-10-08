//! Tiny argument parser: positionals, `--flag`, `--key value` / `--key=value`, and `--`.
use std::collections::HashMap;

/// Flags that take a value (everything else starting with `--` is boolean).
const VALUE_FLAGS: &[&str] = &[
    "since", "limit", "lines", "project", "cwd", "name", "prompt", "resume", "agent", "monitor", "scope", "expires", "reason", "range", "by",
    "kind", "detail", "socket", "timeout", "session", "value", "target",
    "source", "event", "repo", "branch", "action", "label", "run", "attention", "hook-id", "session-name", "trigger", "payload",
    "port", "relay-url", "bucket", "icon", "title", "sub", "keywords", "rpc", "params", "screen", "prefill", "focus", "danger",
    "featured", "id", "image", "why", "jump", "for", "as", "volume", "turn", "stay", "description", "set",
    // local triggers
    "in-project", "for-agent", "idle-for", "cron", "between", "starts", "ends", "max-runs", "match", "send", "send-no-enter", "set-status", "color", "base", "clear-on", "cooldown",
    "action-json", "filter-json", "notify", "notify-body", "notify-kind", "notify-open", "notify-id",
    // notify send
    "open", "wait", "on",
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
    /// The arguments as given (`notify send --on` splits them into groups).
    pub raw: Vec<String>,
}

#[derive(Debug)]
pub struct ArgError(pub String);

impl Args {
    pub fn parse(raw: impl IntoIterator<Item = String>) -> Result<Args, ArgError> {
        Args::parse_with(raw, &[])
    }

    /// `parse`, with `extra` also taking a value (flags one verb uses differently, e.g. `queue --idle 10m`).
    pub fn parse_with(raw: impl IntoIterator<Item = String>, extra: &[&str]) -> Result<Args, ArgError> {
        let raw: Vec<String> = raw.into_iter().collect();
        let mut a = Args { raw: raw.clone(), ..Args::default() };
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

    /// Split at each `--on <value>`: the arguments before the first, then (value, the arguments
    /// up to the next `--on`) for each. Nothing after `--` is split.
    pub fn split_on(&self, flag: &str) -> Result<(Vec<String>, Vec<(String, Vec<String>)>), ArgError> {
        let (mut head, mut groups): (Vec<String>, Vec<(String, Vec<String>)>) = (vec![], vec![]);
        let mut it = self.raw.iter();
        while let Some(s) = it.next() {
            if s == "--" {
                let rest = std::iter::once(s.clone()).chain(it.by_ref().cloned());
                match groups.last_mut() {
                    Some((_, g)) => g.extend(rest),
                    None => head.extend(rest),
                }
                break;
            }
            let value = match s.strip_prefix("--").and_then(|n| n.strip_prefix(flag)) {
                Some("") => Some(it.next().cloned().ok_or_else(|| ArgError(format!("--{flag} needs a value")))?),
                Some(v) if v.starts_with('=') => Some(v[1..].to_string()),
                _ => None,
            };
            match (value, groups.last_mut()) {
                (Some(v), _) => groups.push((v, vec![])),
                (None, Some((_, g))) => g.push(s.clone()),
                (None, None) => head.push(s.clone()),
            }
        }
        Ok((head, groups))
    }

    /// Unknown flags (anything not in `allowed`).
    pub fn check(&self, allowed: &[&str]) -> Result<(), ArgError> {
        for k in self.flags.keys() {
            if !allowed.contains(&k.as_str()) && !matches!(k.as_str(), "json" | "help" | "socket" | "no-wait") {
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
    fn splits_on() {
        let a = Args::parse(["notify", "send", "t", "--action", "Go", "--on", "Go", "--run", "make", "--headless", "--on=clicked", "--attention", "hi"].map(String::from)).unwrap();
        let (head, groups) = a.split_on("on").unwrap();
        assert_eq!(head, ["notify", "send", "t", "--action", "Go"]);
        assert_eq!(groups, [("Go".to_string(), vec!["--run".to_string(), "make".into(), "--headless".into()]), ("clicked".to_string(), vec!["--attention".to_string(), "hi".into()])]);
        assert!(Args { raw: vec!["send".into(), "--on".into()], ..Args::default() }.split_on("on").is_err());
        // --one isn't --on
        let (head, groups) = Args::parse(["x", "--one", "1"].map(String::from)).unwrap().split_on("on").unwrap();
        assert_eq!((head.len(), groups.len()), (3, 0));
    }

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
