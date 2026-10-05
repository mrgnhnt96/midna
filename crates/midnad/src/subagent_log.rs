//! A subagent's own transcript, for its read-only window (`session.subagent_log`).
//!
//! Claude Code writes each subagent to `<transcript without .jsonl>/subagents/agent-<id>.jsonl`
//! (the path `SubagentStop` reports as `agent_transcript_path`), so a running one can be read
//! before it stops. Lines are `user` (the prompt, then tool results) and `assistant` (text and
//! tool calls) entries; `attachment` and the rest are skipped.
use midna_proto::SubagentLogEntry;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Most read in one call; the window asks again from `next`.
const CHUNK: u64 = 4 << 20;

/// Where Claude keeps `agent`'s transcript, or None for an id that isn't one (ids are hex).
pub fn path(transcript: &str, agent: &str) -> Option<PathBuf> {
    if agent.is_empty() || agent.len() > 64 || !agent.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let dir = transcript.strip_suffix(".jsonl")?;
    Some(Path::new(dir).join("subagents").join(format!("agent-{agent}.jsonl")))
}

/// Complete lines from byte `from` on: their entries, the offset after the last one, and the
/// model named by an assistant line. A file that isn't there yet reads as empty.
pub fn read(path: &Path, from: u64) -> (Vec<SubagentLogEntry>, u64, Option<String>) {
    let mut buf = vec![];
    if let Ok(mut f) = std::fs::File::open(path)
        && f.seek(SeekFrom::Start(from)).is_ok()
    {
        let _ = f.take(CHUNK).read_to_end(&mut buf);
    }
    let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |n| n + 1);
    let (mut entries, mut model) = (vec![], None);
    for line in buf[..end].split(|&b| b == b'\n') {
        let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
        if let Some(m) = v.pointer("/message/model").and_then(Value::as_str).filter(|m| !m.starts_with('<')) {
            model = Some(m.to_string());
        }
        entries.extend(entries_of(&v));
    }
    (entries, from + end as u64, model)
}

fn entry(kind: &str, text: impl Into<String>) -> SubagentLogEntry {
    SubagentLogEntry { kind: kind.into(), text: text.into() }
}

fn entries_of(v: &Value) -> Vec<SubagentLogEntry> {
    let content = v.pointer("/message/content");
    match (v.get("type").and_then(Value::as_str), content) {
        (Some("user"), Some(Value::String(s))) => vec![entry("prompt", s.trim())],
        (Some("user"), Some(Value::Array(blocks))) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    let error = b.get("is_error").and_then(Value::as_bool) == Some(true);
                    Some(entry(if error { "error" } else { "result" }, summarize_result(b.get("content"))))
                }
                Some("text") => b.get("text").and_then(Value::as_str).map(|t| entry("prompt", t.trim())),
                _ => None,
            })
            .collect(),
        (Some("assistant"), Some(Value::Array(blocks))) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(|t| entry("text", t)),
                Some("tool_use") => {
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("tool");
                    Some(entry("tool", format!("{name}({})", tool_summary(b.get("input").unwrap_or(&Value::Null)))))
                }
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// The argument a person would recognize a call by, clipped to one line.
fn tool_summary(input: &Value) -> String {
    const KEYS: [&str; 9] = ["description", "command", "file_path", "pattern", "path", "url", "query", "prompt", "skill"];
    let s = KEYS.iter().find_map(|k| input.get(*k).and_then(Value::as_str)).unwrap_or("");
    clip(s, 120)
}

/// First non-empty line of a tool result, with how many more there were.
fn summarize_result(c: Option<&Value>) -> String {
    let text = match c {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    // Skip blank lines and bare wrapper tags (`<persisted-output>`).
    let tag = |l: &str| l.starts_with('<') && l.ends_with('>') && !l.contains(' ');
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !tag(l)).collect();
    match lines.as_slice() {
        [] => "(no output)".into(),
        [one] => clip(one, 160),
        [first, rest @ ..] => format!("{} (+{} lines)", clip(first, 160), rest.len()),
    }
}

fn clip(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("").trim();
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `MIDNA_SUBAGENT_LOG=<agent-….jsonl> cargo test -p midnad real_subagent_log -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_subagent_log() {
        let (e, next, model) = read(Path::new(&std::env::var("MIDNA_SUBAGENT_LOG").unwrap()), 0);
        for x in &e {
            println!("{:>6}  {}", x.kind, x.text.lines().next().unwrap_or(""));
        }
        println!("{} entries, next {next}, model {model:?}", e.len());
    }

    #[test]
    fn path_rejects_odd_ids() {
        assert_eq!(path("/p/c1.jsonl", "a98dd"), Some(PathBuf::from("/p/c1/subagents/agent-a98dd.jsonl")));
        assert_eq!(path("/p/c1.jsonl", "../x"), None);
        assert_eq!(path("/p/c1.jsonl", ""), None);
        assert_eq!(path("/p/c1.txt", "a1"), None);
    }

    #[test]
    fn reads_complete_lines_into_entries() {
        let dir = std::env::temp_dir().join(format!("midna-sublog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("agent-a1.jsonl");
        let lines = [
            json!({"type": "user", "message": {"role": "user", "content": "Find the hooks.\nThen report."}}),
            json!({"type": "attachment"}),
            json!({"type": "assistant", "message": {"model": "claude-haiku-4-5", "content": [
                {"type": "text", "text": "Looking."},
                {"type": "tool_use", "name": "Grep", "input": {"pattern": "SubagentStart", "path": "crates"}}]}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "<persisted-output>\na.rs:1\nb.rs:2\n\nc.rs:3"}]}}),
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "is_error": true, "content": [{"type": "text", "text": "No such file"}]}]}}),
        ];
        let mut body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        body.push_str("{\"type\": \"assistant\""); // half-written line: not read yet
        std::fs::write(&f, &body).unwrap();
        let (e, next, model) = read(&f, 0);
        let got: Vec<(String, String)> = e.into_iter().map(|e| (e.kind, e.text)).collect();
        assert_eq!(got, [
            ("prompt".into(), "Find the hooks.\nThen report.".into()),
            ("text".into(), "Looking.".into()),
            ("tool".into(), "Grep(SubagentStart)".into()),
            ("result".into(), "a.rs:1 (+2 lines)".into()),
            ("error".into(), "No such file".into()),
        ]);
        assert_eq!(model.as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(next as usize, body.len() - "{\"type\": \"assistant\"".len());
        assert_eq!(read(&f, next).0, vec![], "the partial line waits");
        assert_eq!(read(&dir.join("missing.jsonl"), 0), (vec![], 0, None));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
