//! Session links: the URLs, pull requests, artifacts and files that come up in an agent
//! terminal's conversation, for the header's links button (`links.list`).
//!
//! Claude: the daemon reads the transcript (`AgentInfo.transcript_path`, a JSONL file) from
//! where it left off, a moment after each `UserPromptSubmit`, `PostToolUse` and `Stop` hook
//! (the entry a hook is about may not be written yet when it fires). What counts:
//! - every http(s) URL in the human's prompts and the agent's reply text;
//! - the URL a `WebFetch` fetched;
//! - pull request URLs in the output of shell, MCP and subagent tools (not file reads or
//!   fetched pages, which are full of links nobody chose);
//! - artifacts the agent published: `frame-link` entries (with their titles) and the `url`
//!   an `Artifact` publish updates;
//! - files `Write` created and `Edit`/`MultiEdit`/`NotebookEdit` changed, outside temp dirs.
//!
//! Codex has no transcript path in its hooks; its notify carries the turn's prompts and last
//! reply, which are scanned for URLs.
//!
//! Each link remembers the prompts it came up in (`turn`, `turns`: `n` from `session.prompts`),
//! by time: an entry belongs to the latest prompt sent at or before its timestamp (both are
//! whole seconds).
//!
//! Each terminal's links live in `$MIDNA_HOME/links/<session>.json` with the read offset, so a
//! daemon restart neither loses pins nor counts a mention twice.
use crate::daemon::Daemon;
use midna_proto::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Wait this long after a hook before reading, so the entry it is about has been written.
const SETTLE: Duration = Duration::from_millis(800);
/// Longest title kept for a web link.
const TITLE_MAX: usize = 80;

/// One link found in one transcript entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub kind: LinkKind,
    pub target: String,
    pub title: String,
    /// The title is a real name (an artifact's), not one made from the URL.
    pub named: bool,
    pub source: LinkSource,
    pub via: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Store {
    #[serde(default)]
    pub transcript: Option<String>,
    #[serde(default)]
    pub offset: u64,
    /// tool_use id -> tool name and `via`, for results that arrive in a later read.
    #[serde(default)]
    pub tools: HashMap<String, (String, Option<String>)>,
    #[serde(default)]
    pub links: Vec<Link>,
    /// Links removed with links.remove, by target: the transcript mentioning them again doesn't
    /// bring them back; links.remove with `restore` puts one back as it was.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub removed: HashMap<String, Link>,
    /// Fresh "a hook refused the prompt" warnings the last read found (see `local.rs`).
    #[serde(skip)]
    pub blocked: Vec<String>,
}

/// Every terminal's links, loaded on first use.
#[derive(Default)]
pub struct Links {
    stores: Mutex<HashMap<Id, Store>>,
    /// Terminals with a read already scheduled.
    pending: Mutex<HashSet<Id>>,
    /// Per terminal: when each prompt was sent (unix secs, session.prompts' order) and the event
    /// log seq read up to, so only newer events are read next time.
    prompts: Mutex<HashMap<Id, (u64, Vec<i64>)>>,
}

fn dir(home: &Path) -> PathBuf {
    home.join("links")
}

fn file(home: &Path, sid: &str) -> PathBuf {
    dir(home).join(format!("{sid}.json"))
}

impl Links {
    /// Run `f` on a terminal's store (loading it first), saving it when `f` says it changed.
    fn with<T>(&self, home: &Path, sid: &str, f: impl FnOnce(&mut Store) -> (T, bool)) -> T {
        let mut stores = self.stores.lock().unwrap_or_else(|e| e.into_inner());
        let store = stores.entry(sid.to_string()).or_insert_with(|| {
            std::fs::read(file(home, sid)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
        });
        let (out, changed) = f(store);
        if changed {
            save(home, sid, store);
        }
        out
    }

    pub fn list(&self, home: &Path, sid: &str) -> Vec<Link> {
        self.with(home, sid, |s| (sorted(&s.links), false))
    }

    /// Drop a closed terminal's links.
    pub fn forget(&self, home: &Path, sid: &str) {
        self.stores.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);
        self.prompts.lock().unwrap_or_else(|e| e.into_inner()).remove(sid);
        let _ = std::fs::remove_file(file(home, sid));
    }
}

/// When each prompt was sent to `sid` (unix secs), oldest first: prompt `n` is index `n - 1`,
/// as in `session.prompts`.
pub fn prompt_times(d: &Daemon, sid: &str) -> Vec<i64> {
    let mut cache = d.links.prompts.lock().unwrap_or_else(|e| e.into_inner());
    let (seen, times) = cache.entry(sid.to_string()).or_default();
    let upto = d.log.seq();
    if *seen < upto {
        let filter = EventFilter { kinds: Some(vec![kinds::AGENT_PROMPT_SUBMITTED.into()]), session_id: Some(sid.into()), project_id: None };
        for e in d.log.list(*seen, crate::prompts::MAX_PROMPTS, &filter) {
            times.push(time::parse_rfc3339(&e.at).unwrap_or(0));
            *seen = (*seen).max(e.seq);
        }
        *seen = (*seen).max(upto);
    }
    times.clone()
}

/// The prompt an entry at unix time `t` belongs to (`n`), given `prompt_times`.
pub fn turn_at(times: &[i64], t: i64) -> Option<u32> {
    let n = times.iter().filter(|&&p| p <= t).count();
    (n > 0).then_some(n as u32)
}

fn save(home: &Path, sid: &str, store: &Store) {
    let _ = std::fs::create_dir_all(dir(home));
    let path = file(home, sid);
    let tmp = path.with_extension("json.tmp");
    if let Ok(b) = serde_json::to_vec(store)
        && std::fs::write(&tmp, b).is_ok()
    {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Pinned first (latest pin first), then by when they last came up, latest first.
pub fn sorted(links: &[Link]) -> Vec<Link> {
    let mut v = links.to_vec();
    v.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| b.last_at.cmp(&a.last_at)));
    v
}

pub fn link_id(target: &str) -> Id {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(target.as_bytes());
    format!("l_{:02x}{:02x}{:02x}{:02x}", h[0], h[1], h[2], h[3])
}

/// Merge what one entry mentioned (in prompt `turn`). Returns how many links are new.
pub fn record(links: &mut Vec<Link>, found: Vec<Found>, at: &str, turn: Option<u32>) -> usize {
    let mut added = 0;
    let mut seen = HashSet::new();
    for f in found {
        if !seen.insert(f.target.clone()) {
            continue; // once per entry
        }
        if let Some(l) = links.iter_mut().find(|l| l.target == f.target) {
            l.mentions += 1;
            if at > l.last_at.as_str() {
                l.last_at = at.to_string();
            }
            if f.named && l.title != f.title && l.source != LinkSource::Added {
                l.title = f.title;
            }
            add_turn(l, turn);
            continue;
        }
        added += 1;
        links.push(Link {
            id: link_id(&f.target),
            kind: f.kind,
            target: f.target,
            title: f.title,
            source: f.source,
            via: f.via,
            mentions: 1,
            first_at: at.to_string(),
            last_at: at.to_string(),
            turn,
            turns: turn.into_iter().collect(),
            pinned: false,
            pinned_by: None,
            note: None,
        });
    }
    added
}

fn add_turn(l: &mut Link, turn: Option<u32>) {
    let Some(t) = turn else { return };
    if !l.turns.contains(&t) {
        l.turns.push(t);
        l.turns.sort_unstable();
    }
    l.turn = l.turns.last().copied();
}

// ------------------------------------------------------------------ reading transcripts

/// Schedule a read of a Claude terminal's transcript after `hook` (other hooks are ignored).
pub fn after_hook(d: &std::sync::Arc<Daemon>, sid: &str, agent: AgentKind, hook: &str, payload: &Value) {
    match agent {
        AgentKind::Claude if matches!(hook, "UserPromptSubmit" | "PostToolUse" | "Stop" | "SubagentStop" | "SessionStart") => {
            if !d.links.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.to_string()) {
                return; // a read is already on its way and will see this entry too
            }
            let (d, sid) = (d.clone(), sid.to_string());
            std::thread::spawn(move || {
                std::thread::sleep(SETTLE);
                d.links.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&sid);
                read_transcript(&d, &sid);
            });
        }
        AgentKind::Codex if hook == "agent-turn-complete" => {
            let mut found = vec![];
            for m in payload.get("input-messages").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                found.extend(urls_in(m, LinkSource::User, None, true));
            }
            if let Some(m) = payload.get("last-assistant-message").and_then(Value::as_str) {
                found.extend(urls_in(m, LinkSource::Agent, None, true));
            }
            let now = time::now_rfc3339();
            // This notify's prompts are logged right after this (rpc::agent::hook): count them in.
            let prompts = prompt_times(d, sid).len() + payload.get("input-messages").and_then(Value::as_array).map_or(0, Vec::len);
            let turn = (prompts > 0).then_some(prompts as u32);
            let added = d.links.with(&d.cfg.home, sid, |s| {
                let n = record(&mut s.links, found, &now, turn);
                (n, n > 0)
            });
            if added > 0 {
                changed(d, sid, added);
            }
        }
        _ => {}
    }
}

/// Read what the transcript gained since the last read.
pub fn read_transcript(d: &std::sync::Arc<Daemon>, sid: &str) {
    let (path, cwd) = {
        let core = d.core();
        let Some(s) = core.state.session(sid) else { return };
        (s.agent_info.as_ref().and_then(|i| i.transcript_path.clone()), s.cwd.clone())
    };
    let Some(path) = path else { return };
    let mut times: Option<Vec<i64>> = None;
    let mut turn_of = |t: i64| turn_at(times.get_or_insert_with(|| prompt_times(d, sid)), t);
    let (added, blocked) = d.links.with(&d.cfg.home, sid, |s| {
        let before = s.offset;
        let n = read_into(s, &path, &cwd, &mut turn_of);
        ((n, std::mem::take(&mut s.blocked)), n > 0 || s.offset != before)
    });
    if added > 0 {
        changed(d, sid, added);
    }
    for b in blocked {
        crate::local::prompt_blocked(d, sid, &b);
    }
}

/// Read complete new lines of `path` into `s`; `turn_of` maps an entry's time (unix secs) to
/// its prompt. Returns how many links are new.
pub fn read_into(s: &mut Store, path: &str, cwd: &str, turn_of: &mut dyn FnMut(i64) -> Option<u32>) -> usize {
    if s.transcript.as_deref() != Some(path) {
        // A new conversation in the same terminal (`/clear`, a fresh start): read it from the
        // top and keep what the old one found.
        s.transcript = Some(path.to_string());
        s.offset = 0;
        s.tools.clear();
    }
    let Ok(mut f) = std::fs::File::open(path) else { return 0 };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < s.offset {
        s.offset = len; // rewritten under us: don't count it all again
        return 0;
    }
    if f.seek(SeekFrom::Start(s.offset)).is_err() {
        return 0;
    }
    let mut r = std::io::BufReader::new(f);
    let mut added = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        match r.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if line.last() != Some(&b'\n') => break, // half-written: next time
            Ok(n) => {
                s.offset += n as u64;
                let Ok(v) = serde_json::from_slice::<Value>(&line) else { continue };
                if let Some(b) = crate::local::blocked_notice(&v) {
                    s.blocked.push(b);
                }
                let mut found = scan_entry(&v, cwd, &mut s.tools);
                found.retain(|f| !s.removed.contains_key(&f.target));
                if found.is_empty() {
                    continue;
                }
                let t = v.get("timestamp").and_then(Value::as_str).and_then(time::parse_rfc3339).unwrap_or_else(time::now_unix);
                added += record(&mut s.links, found, &time::format_unix(t), turn_of(t));
            }
        }
    }
    added
}

fn changed(d: &Daemon, sid: &str, added: usize) {
    let (count, pinned) = d.links.with(&d.cfg.home, sid, |s| ((s.links.len(), s.links.iter().filter(|l| l.pinned).count()), false));
    let project = d.core().state.session(sid).map(|s| s.project_id.clone());
    d.emit(kinds::LINKS_CHANGED, Actor::system(), project, Some(sid.to_string()), json!({ "count": count, "added": added, "pinned": pinned }));
}

/// Pin or unpin a link by id or exact target.
pub fn pin(d: &Daemon, sid: &str, link: &str, pinned: bool, by: Actor) -> Result<Link, RpcError> {
    let out = d.links.with(&d.cfg.home, sid, |s| match s.links.iter_mut().find(|l| l.id == link || l.target == link) {
        Some(l) => {
            let was = l.pinned;
            l.pinned = pinned;
            l.pinned_by = pinned.then_some(by);
            if pinned && !was {
                l.last_at = time::now_rfc3339(); // the latest pin sorts first
            }
            (Ok(l.clone()), true)
        }
        None => (Err(RpcError::not_found(format!("no link {link} in terminal {sid}; see links.list"))), false),
    })?;
    changed(d, sid, 0);
    Ok(out)
}

/// Remove a link by id or exact target, and keep it out of later reads.
pub fn remove(d: &Daemon, sid: &str, link: &str) -> Result<Link, RpcError> {
    let out = d.links.with(&d.cfg.home, sid, |s| match s.links.iter().position(|l| l.id == link || l.target == link) {
        Some(i) => {
            let l = s.links.remove(i);
            s.removed.insert(l.target.clone(), l.clone());
            (Ok(l), true)
        }
        None => (Err(RpcError::not_found(format!("no link {link} in terminal {sid}; see links.list"))), false),
    })?;
    changed(d, sid, 0);
    Ok(out)
}

/// Put a removed link back as it was (by id or exact target).
pub fn restore(d: &Daemon, sid: &str, link: &str) -> Result<Link, RpcError> {
    let out = d.links.with(&d.cfg.home, sid, |s| match unremove(s, link) {
        Some(l) => (Ok(l), true),
        None => (Err(RpcError::not_found(format!("no removed link {link} in terminal {sid}"))), false),
    })?;
    changed(d, sid, 0);
    Ok(out)
}

/// Move a removed link (by id or exact target) back into `s.links`.
fn unremove(s: &mut Store, link: &str) -> Option<Link> {
    let key = s.removed.iter().find(|(t, l)| l.id == link || *t == link).map(|(t, _)| t.clone())?;
    let l = s.removed.remove(&key)?;
    s.links.push(l.clone());
    Some(l)
}

/// Add (or update) a link on purpose; this brings back a removed one.
pub fn add(d: &Daemon, sid: &str, p: &LinksAddParams, by: Actor) -> Result<Link, RpcError> {
    let target = p.target.trim();
    let (kind, target, derived) = if target.starts_with("http://") || target.starts_with("https://") {
        classify_url(target)
    } else if target.starts_with('/') {
        let cwd = d.core().state.session(sid).map(|s| s.cwd.clone()).unwrap_or_default();
        (LinkKind::File, target.to_string(), file_title(target, &cwd))
    } else {
        return Err(RpcError::bad_params("target must be an http(s) URL or an absolute path"));
    };
    let now = time::now_rfc3339();
    let turn = turn_at(&prompt_times(d, sid), time::now_unix());
    let out = d.links.with(&d.cfg.home, sid, |s| {
        s.removed.remove(&target);
        if !s.links.iter().any(|l| l.target == target) {
            let f = Found { kind, target: target.clone(), title: derived, named: false, source: LinkSource::Added, via: None };
            record(&mut s.links, vec![f], &now, turn);
        } else if let Some(l) = s.links.iter_mut().find(|l| l.target == target) {
            add_turn(l, turn);
        }
        let l = s.links.iter_mut().find(|l| l.target == target).expect("just recorded");
        if let Some(t) = p.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            l.title = t.to_string();
            if l.source != LinkSource::Added {
                l.via = Some("links.add".into());
            }
        }
        if let Some(n) = p.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            l.note = Some(n.to_string());
        }
        if p.pin {
            l.pinned = true;
            l.pinned_by = Some(by);
        }
        l.last_at = now.clone();
        (l.clone(), true)
    });
    changed(d, sid, 1);
    Ok(out)
}

// ------------------------------------------------------------------ one transcript entry

/// What one Claude transcript entry mentions. `tools` remembers tool calls so their results
/// (a later entry) know which tool printed them.
pub fn scan_entry(v: &Value, cwd: &str, tools: &mut HashMap<String, (String, Option<String>)>) -> Vec<Found> {
    let mut out = vec![];
    match v.get("type").and_then(Value::as_str) {
        Some("frame-link") => {
            if let Some(url) = v.get("frameUrl").and_then(Value::as_str) {
                let (kind, target, mut title) = classify_url(url);
                let named = match v.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                    Some(t) => {
                        title = t.to_string();
                        true
                    }
                    None => false,
                };
                out.push(Found { kind, target, title, named, source: LinkSource::Tool, via: Some("Artifact".into()) });
            }
        }
        Some("user") => {
            if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return out;
            }
            match v.pointer("/message/content") {
                Some(Value::String(t)) if !t.starts_with('<') => out.extend(urls_in(t, LinkSource::User, None, true)),
                Some(Value::Array(parts)) => {
                    for part in parts {
                        match part.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                let t = part.get("text").and_then(Value::as_str).unwrap_or("");
                                if !t.starts_with('<') {
                                    out.extend(urls_in(t, LinkSource::User, None, true));
                                }
                            }
                            Some("tool_result") => {
                                let id = part.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                                let Some((name, via)) = tools.remove(id) else { continue };
                                if !reads_output(&name) {
                                    continue;
                                }
                                for t in result_texts(part.get("content")) {
                                    out.extend(urls_in(t, LinkSource::Tool, via.clone(), false));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        Some("assistant") => {
            for part in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => out.extend(urls_in(part.get("text").and_then(Value::as_str).unwrap_or(""), LinkSource::Agent, None, true)),
                    Some("tool_use") => {
                        let name = part.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                        let input = part.get("input").unwrap_or(&Value::Null);
                        match name.as_str() {
                            "WebFetch" => {
                                if let Some(u) = input.get("url").and_then(Value::as_str) {
                                    out.extend(urls_in(u, LinkSource::Fetched, Some("WebFetch".into()), true));
                                }
                            }
                            "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
                                let path = input.get("file_path").or(input.get("notebook_path")).and_then(Value::as_str).unwrap_or("");
                                if path.starts_with('/') && !scratch(path) {
                                    let source = if name == "Write" { LinkSource::Created } else { LinkSource::Edited };
                                    out.push(Found { kind: LinkKind::File, target: path.to_string(), title: file_title(path, cwd), named: false, source, via: Some(name.clone()) });
                                }
                            }
                            // A publish names the artifact it updates; a new one comes as a frame-link.
                            "Artifact" if matches!(input.get("action").and_then(Value::as_str), None | Some("publish")) && input.get("asset").is_none() => {
                                if let Some(u) = input.get("url").and_then(Value::as_str) {
                                    out.extend(urls_in(u, LinkSource::Tool, Some("Artifact".into()), true).into_iter().filter(|f| f.kind == LinkKind::Artifact));
                                }
                            }
                            _ => {}
                        }
                        if let Some(id) = part.get("id").and_then(Value::as_str) {
                            let via = match name.as_str() {
                                "Bash" => input.get("command").and_then(Value::as_str).map(command_head),
                                n => Some(n.rsplit("__").next().unwrap_or(n).to_string()),
                            };
                            tools.insert(id.to_string(), (name, via));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Tools whose output is worth reading for pull request links: what the agent ran, not what
/// it read (a `cat` of notes is full of links nobody chose here).
fn reads_output(tool: &str) -> bool {
    matches!(tool, "Bash" | "Agent" | "Task") || tool.starts_with("mcp__")
}

/// Scratch files (the agent's temp and scratchpad dirs) are not worth coming back to.
fn scratch(path: &str) -> bool {
    let tmp = std::env::temp_dir();
    let tmp = tmp.to_str().unwrap_or("/tmp").trim_end_matches('/');
    [tmp, "/tmp", "/private/tmp", "/var/folders", "/private/var/folders"].iter().any(|d| path.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
}

fn result_texts(content: Option<&Value>) -> Vec<&str> {
    match content {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect(),
        _ => vec![],
    }
}

/// `cd x && gh pr create --fill` -> `gh pr create`.
pub fn command_head(cmd: &str) -> String {
    let seg = cmd
        .split(['&', ';', '|', '\n'])
        .map(str::trim)
        .find(|s| !s.is_empty() && !s.starts_with("cd "))
        .unwrap_or(cmd.trim());
    let seg = seg.split_whitespace().skip_while(|w| w.contains('=')).collect::<Vec<_>>().join(" ");
    let words: Vec<&str> = seg.split_whitespace().take_while(|w| !w.starts_with('-') && !w.contains(['/', '"', '\'', '$', '.'])).take(3).collect();
    if words.is_empty() { seg.split_whitespace().next().unwrap_or("").to_string() } else { words.join(" ") }
}

// ------------------------------------------------------------------ urls

/// The http(s) URLs in `text`. `all = false` keeps only pull requests.
pub fn urls_in(text: &str, source: LinkSource, via: Option<String>, all: bool) -> Vec<Found> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find("http") {
        let tail = &rest[i..];
        let scheme = if tail.starts_with("https://") {
            8
        } else if tail.starts_with("http://") {
            7
        } else {
            rest = &rest[i + 4..];
            continue;
        };
        // A URL glued to a word (`xhttps://`) is not one.
        let glued = rest[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric());
        let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '|' | '\\' | '{' | '}')).unwrap_or(tail.len());
        let url = trim_url(&tail[..end]);
        rest = &tail[end..];
        if glued || url.len() <= scheme + 2 || !url[scheme..].contains('.') && !url[scheme..].starts_with("localhost") {
            continue;
        }
        let (kind, target, title) = classify_url(url);
        if all || kind == LinkKind::Pr {
            out.push(Found { kind, target, title, named: false, source, via: via.clone() });
        }
    }
    out
}

/// Strip trailing punctuation, and a closing bracket the URL didn't open (`(see https://x.y)`).
fn trim_url(mut u: &str) -> &str {
    loop {
        let Some(c) = u.chars().next_back() else { return u };
        let unbalanced = |open: char, close: char| c == close && u.matches(open).count() < u.matches(close).count();
        if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '*' | '_') || unbalanced('(', ')') || unbalanced('[', ']') {
            u = &u[..u.len() - c.len_utf8()];
        } else {
            return u;
        }
    }
}

/// (kind, normalized target, display title).
pub fn classify_url(url: &str) -> (LinkKind, String, String) {
    let no_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let (host_full, path) = no_scheme.split_once('/').unwrap_or((no_scheme, ""));
    let host = host_full.trim_start_matches("www.");
    let segs: Vec<&str> = path.split(['?', '#']).next().unwrap_or("").split('/').filter(|s| !s.is_empty()).collect();
    let number = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    match (host, segs.as_slice()) {
        ("github.com", [owner, repo, "pull", n, ..]) if number(n) => {
            (LinkKind::Pr, format!("https://github.com/{owner}/{repo}/pull/{n}"), format!("PR #{n} · {owner}/{repo}"))
        }
        ("bitbucket.org", [ws, repo, "pull-requests", n, ..]) if number(n) => {
            (LinkKind::Pr, format!("https://bitbucket.org/{ws}/{repo}/pull-requests/{n}"), format!("PR #{n} · {ws}/{repo}"))
        }
        ("github.com", [owner, repo, "issues", n, ..]) if number(n) => (LinkKind::Web, url.to_string(), format!("Issue #{n} · {owner}/{repo}")),
        ("claude.ai", ["artifact", id, ..]) | ("claude.ai", ["code", "artifact", id, ..]) => {
            let target = format!("https://claude.ai/{}", segs[..segs.iter().position(|s| s == id).unwrap_or(0) + 1].join("/"));
            (LinkKind::Artifact, target, format!("Artifact {}", id.chars().take(8).collect::<String>()))
        }
        _ => {
            let shown = format!("{host}{}", if segs.is_empty() { String::new() } else { format!("/{}", segs.join("/")) });
            let title = if shown.chars().count() > TITLE_MAX { format!("{}…", shown.chars().take(TITLE_MAX - 1).collect::<String>()) } else { shown };
            (LinkKind::Web, url.to_string(), title)
        }
    }
}

/// A path relative to the terminal's directory when inside it, else with `~` for home.
pub fn file_title(path: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    if !cwd.is_empty()
        && let Some(rel) = path.strip_prefix(cwd).and_then(|r| r.strip_prefix('/'))
    {
        return rel.to_string();
    }
    if let Ok(home) = std::env::var("HOME")
        && let Some(rel) = path.strip_prefix(home.trim_end_matches('/')).filter(|r| r.starts_with('/'))
    {
        return format!("~{rel}");
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(f: &[Found]) -> Vec<(LinkKind, &str)> {
        f.iter().map(|f| (f.kind, f.target.as_str())).collect()
    }

    #[test]
    fn finds_urls_in_prose_and_markdown() {
        let t = "See [the docs](https://docs.claude.com/en/hooks). Also (https://en.wikipedia.org/wiki/Rust_(language)), \
                 and https://github.com/o/r/pull/12#discussion_r1, plus xhttps://nope.com and http://localhost:7411/debug.";
        let f = urls_in(t, LinkSource::Agent, None, true);
        assert_eq!(
            kinds(&f),
            vec![
                (LinkKind::Web, "https://docs.claude.com/en/hooks"),
                (LinkKind::Web, "https://en.wikipedia.org/wiki/Rust_(language)"),
                (LinkKind::Pr, "https://github.com/o/r/pull/12"),
                (LinkKind::Web, "http://localhost:7411/debug"),
            ]
        );
        assert_eq!(f[2].title, "PR #12 · o/r");
        assert_eq!(f[0].title, "docs.claude.com/en/hooks");
    }

    #[test]
    fn tool_output_keeps_only_prs() {
        let t = "https://claude.ai/artifact/5dZ55xsmqwfpiW9oZ6D9VG\nsee https://example.com/x\nhttps://github.com/o/r/pull/3";
        let f = urls_in(t, LinkSource::Tool, Some("gh pr create".into()), false);
        assert_eq!(kinds(&f), vec![(LinkKind::Pr, "https://github.com/o/r/pull/3")]);
        assert_eq!(classify_url("https://claude.ai/artifact/5dZ55xsmqwfpiW9oZ6D9VG").2, "Artifact 5dZ55xsm");
    }

    #[test]
    fn classifies_bitbucket_and_issues() {
        assert_eq!(classify_url("https://bitbucket.org/w/r/pull-requests/7/overview").1, "https://bitbucket.org/w/r/pull-requests/7");
        assert_eq!(classify_url("https://github.com/o/r/issues/3").2, "Issue #3 · o/r");
        assert_eq!(classify_url("https://claude.ai/code/artifact/abc-def/x").1, "https://claude.ai/code/artifact/abc-def");
    }

    #[test]
    fn command_heads() {
        assert_eq!(command_head("cd /x && gh pr create --fill"), "gh pr create");
        assert_eq!(command_head("FOO=1 git push -u origin main"), "git push");
        assert_eq!(command_head("git push origin feat/x"), "git push origin");
        assert_eq!(command_head("./scripts/smoke.sh"), "./scripts/smoke.sh");
    }

    #[test]
    fn scans_a_claude_conversation() {
        let mut tools = HashMap::new();
        let lines = [
            json!({"type":"user","message":{"role":"user","content":"the docs are at https://docs.claude.com/hooks"},"timestamp":"2026-10-04T10:00:00.000Z"}),
            json!({"type":"user","isMeta":true,"message":{"role":"user","content":"meta https://meta.example.com"}}),
            json!({"type":"user","message":{"role":"user","content":"<system-reminder>https://reminder.example.com</system-reminder>"}}),
            json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"gh pr create --fill"}},
                {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/p/README.md"}},
                {"type":"tool_use","id":"t3","name":"Write","input":{"file_path":"/p/src/links.rs","content":"https://in.file.com"}},
                {"type":"tool_use","id":"t4","name":"WebFetch","input":{"url":"https://docs.rs/serde_json","prompt":"x"}}
            ]}}),
            json!({"type":"user","message":{"content":[
                {"type":"tool_result","tool_use_id":"t1","content":"https://github.com/o/r/pull/12\n"},
                {"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"https://github.com/o/r/pull/99"}]}
            ]}}),
            json!({"type":"frame-link","frameUrl":"https://claude.ai/artifact/5dZ55xsm","title":"Midna Session Links"}),
            json!({"type":"assistant","message":{"content":[{"type":"text","text":"Opened https://github.com/o/r/pull/12 and https://github.com/o/r/pull/12."}]}}),
        ];
        let mut links = vec![];
        for (i, l) in lines.iter().enumerate() {
            let f = scan_entry(l, "/p", &mut tools);
            record(&mut links, f, &format!("2026-10-04T10:00:0{i}Z"), None);
        }
        let got: Vec<(&str, LinkSource, u32, &str)> = links.iter().map(|l| (l.target.as_str(), l.source, l.mentions, l.title.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("https://docs.claude.com/hooks", LinkSource::User, 1, "docs.claude.com/hooks"),
                ("/p/src/links.rs", LinkSource::Created, 1, "src/links.rs"),
                ("https://docs.rs/serde_json", LinkSource::Fetched, 1, "docs.rs/serde_json"),
                ("https://github.com/o/r/pull/12", LinkSource::Tool, 2, "PR #12 · o/r"),
                ("https://claude.ai/artifact/5dZ55xsm", LinkSource::Tool, 1, "Midna Session Links"),
            ]
        );
        assert_eq!(links[3].via.as_deref(), Some("gh pr create"));
        let mut left: Vec<&String> = tools.keys().collect();
        left.sort();
        assert_eq!(left, ["t3", "t4"], "results consume their tool calls");
    }

    #[test]
    fn publishes_count_and_scratch_files_and_reads_dont() {
        let mut tools = HashMap::new();
        let call = json!({"type":"assistant","message":{"content":[
            {"type":"tool_use","id":"a1","name":"Artifact","input":{"url":"https://claude.ai/artifact/abc123","file_path":"/x/p.html"}},
            {"type":"tool_use","id":"a2","name":"Artifact","input":{"action":"read","url":"https://claude.ai/artifact/other1"}},
            {"type":"tool_use","id":"a3","name":"Write","input":{"file_path":"/private/tmp/claude-501/x/scratchpad/notes.md"}},
            {"type":"tool_use","id":"a4","name":"Bash","input":{"command":"cat notes.md"}}
        ]}});
        let f = scan_entry(&call, "/p", &mut tools);
        assert_eq!(kinds(&f), vec![(LinkKind::Artifact, "https://claude.ai/artifact/abc123")]);
        let result = json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"a4","content":"design: https://claude.ai/artifact/zzz999 and https://example.com"}
        ]}});
        assert!(scan_entry(&result, "/p", &mut tools).is_empty(), "a cat's links are not this session's");
    }

    #[test]
    fn reads_only_whole_new_lines_and_follows_a_new_transcript() {
        let dir = std::env::temp_dir().join(format!("midna-links-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let entry = |u: &str| format!("{}\n", json!({"type":"user","message":{"content":format!("look {u}")}}));
        std::fs::write(&path, format!("{}{}", entry("https://a.com/1"), &entry("https://b.com/2")[..10])).unwrap();
        let mut s = Store::default();
        let p = path.to_str().unwrap();
        assert_eq!(read_into(&mut s, p, "/", &mut |_| None), 1);
        std::fs::write(&path, format!("{}{}", entry("https://a.com/1"), entry("https://b.com/2"))).unwrap();
        assert_eq!(read_into(&mut s, p, "/", &mut |_| None), 1);
        assert_eq!(read_into(&mut s, p, "/", &mut |_| None), 0, "nothing new, nothing counted twice");
        assert_eq!(s.links.iter().map(|l| l.mentions).collect::<Vec<_>>(), vec![1, 1]);

        let other = dir.join("u.jsonl");
        std::fs::write(&other, entry("https://a.com/1")).unwrap();
        assert_eq!(read_into(&mut s, other.to_str().unwrap(), "/", &mut |_| None), 0);
        assert_eq!(s.links[0].mentions, 2, "a new conversation keeps the old links");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removed_links_stay_out() {
        let dir = std::env::temp_dir().join(format!("midna-links-removed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let entry = |u: &str| format!("{}\n", json!({"type":"user","message":{"content":format!("look {u}")}}));
        std::fs::write(&path, entry("https://a.com/1")).unwrap();
        let gone = Found { kind: LinkKind::Web, target: "https://a.com/1".into(), title: "a".into(), named: false, source: LinkSource::User, via: None };
        let mut s = Store::default();
        record(&mut s.links, vec![gone], "2026-01-01T00:00:00Z", None);
        s.removed.insert("https://a.com/1".into(), s.links.remove(0));
        let p = path.to_str().unwrap();
        assert_eq!(read_into(&mut s, p, "/", &mut |_| None), 0);
        std::fs::write(&path, format!("{}{}", entry("https://a.com/1"), entry("https://b.com/2"))).unwrap();
        assert_eq!(read_into(&mut s, p, "/", &mut |_| None), 1);
        assert_eq!(s.links.iter().map(|l| l.target.as_str()).collect::<Vec<_>>(), vec!["https://b.com/2"]);
        let back = unremove(&mut s, &link_id("https://a.com/1")).expect("restores by id");
        assert_eq!((back.title.as_str(), back.first_at.as_str()), ("a", "2026-01-01T00:00:00Z"), "as it was");
        assert!(s.removed.is_empty() && s.links.len() == 2);
        assert!(unremove(&mut s, "https://a.com/1").is_none(), "only once");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn links_remember_the_prompts_they_came_up_in() {
        let times = [100, 200, 300];
        assert_eq!((turn_at(&times, 50), turn_at(&times, 100), turn_at(&times, 250), turn_at(&times, 999)), (None, Some(1), Some(2), Some(3)));
        let dir = std::env::temp_dir().join(format!("midna-links-turns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let edit = |file: &str, at: &str| {
            format!("{}\n", json!({"type":"assistant","timestamp":at,"message":{"content":[{"type":"tool_use","id":file,"name":"Edit","input":{"file_path":file}}]}}))
        };
        let lines = [edit("/p/a.rs", "1970-01-01T00:02:30.500Z"), edit("/p/b.rs", "1970-01-01T00:03:20Z"), edit("/p/a.rs", "1970-01-01T00:05:10Z")];
        std::fs::write(&path, lines.concat()).unwrap();
        let mut s = Store::default();
        read_into(&mut s, path.to_str().unwrap(), "/p", &mut |t| turn_at(&times, t));
        let got: Vec<(&str, Option<u32>, &[u32])> = s.links.iter().map(|l| (l.title.as_str(), l.turn, l.turns.as_slice())).collect();
        assert_eq!(got, vec![("a.rs", Some(3), &[1, 3][..]), ("b.rs", Some(2), &[2][..])]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pinned_sort_first() {
        let mut links = vec![];
        record(&mut links, urls_in("https://a.com/x https://b.com/y", LinkSource::User, None, true), "2026-10-04T10:00:00Z", None);
        record(&mut links, urls_in("https://c.com/z", LinkSource::User, None, true), "2026-10-04T10:05:00Z", None);
        links[0].pinned = true;
        let order: Vec<String> = sorted(&links).into_iter().map(|l| l.title).collect();
        assert_eq!(order, ["a.com/x", "c.com/z", "b.com/y"]);
    }
}
