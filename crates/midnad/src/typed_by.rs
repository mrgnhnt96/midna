//! Prompts something other than the human typed into a terminal: an agent's `session.input` or
//! `session.key` Enter, a trigger's send, or a message an agent, a trigger or midna queued. The
//! prompt the agent then submits is matched against them (`Typed::by_human`), so what only the
//! human's answer should settle (a `blocked` item the agent raised) survives the rest.
//!
//! They're read off the events those paths emit (`session.input_by_agent` and the queue's
//! `sent`), which come out the moment the keys are written, well before the agent's
//! `UserPromptSubmit` hook can come back. A message the human queued counts as theirs.
use midna_proto::{Actor, ActorKind, kinds};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// How long a typed prompt waits to be submitted: Claude holds one typed mid-turn until the
/// turn ends.
const TEXT_TTL: Duration = Duration::from_secs(600);
/// An Enter with no text of its own (a key, an image-only paste) only owns the prompt right after it.
const ENTER_TTL: Duration = Duration::from_secs(10);
/// A queued message sent through `session.input` reports twice: one send.
const SAME_SEND: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Typed(Mutex<HashMap<String, Vec<Sent>>>);

struct Sent {
    /// Whitespace dropped; empty for a bare Enter.
    text: String,
    at: Instant,
}

/// Whitespace dropped (the prompt hook and the event's preview wrap differently), and a
/// clipped preview's trailing ellipsis.
fn norm(s: &str) -> String {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    s.strip_suffix('…').map(str::to_string).unwrap_or(s)
}

fn live(x: &Sent) -> bool {
    x.at.elapsed() < if x.text.is_empty() { ENTER_TTL } else { TEXT_TTL }
}

impl Typed {
    fn book(&self) -> MutexGuard<'_, HashMap<String, Vec<Sent>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every event midnad emits passes through here.
    pub fn observe(&self, kind: &str, actor: &Actor, session: Option<&str>, data: &Value) {
        let Some(sid) = session else { return };
        let s = |k: &str| data.get(k).and_then(Value::as_str);
        match kind {
            kinds::SESSION_INPUT_BY_AGENT if data.get("enter").and_then(Value::as_bool) == Some(true) => self.typed(sid, s("text").unwrap_or("")),
            kinds::SESSION_INPUT_BY_AGENT if s("key").is_some_and(|k| matches!(k.to_ascii_lowercase().as_str(), "enter" | "return")) => self.typed(sid, ""),
            kinds::SESSION_QUEUE if s("action") == Some("sent") && data.get("enter").and_then(Value::as_bool) != Some(false) && actor.kind != ActorKind::Human => {
                self.typed(sid, s("text").unwrap_or(""))
            }
            kinds::SESSION_CLOSED | kinds::SESSION_EXITED => {
                self.book().remove(sid);
            }
            _ => {}
        }
    }

    fn typed(&self, sid: &str, text: &str) {
        let text = norm(text);
        let mut book = self.book();
        let sent = book.entry(sid.to_string()).or_default();
        // A shell never submits a prompt to use them up.
        sent.retain(live);
        if sent.iter().any(|x| x.text == text && x.at.elapsed() < SAME_SEND) {
            return;
        }
        sent.push(Sent { text, at: Instant::now() });
    }

    /// `prompt` was just submitted in `sid`: true unless it's one of the prompts typed for
    /// someone else, which it then uses up.
    pub fn by_human(&self, sid: &str, prompt: &str) -> bool {
        let mut book = self.book();
        let Some(sent) = book.get_mut(sid) else { return true };
        sent.retain(live);
        let prompt = norm(prompt);
        let hit = sent.iter().position(|x| !x.text.is_empty() && prompt.contains(&x.text)).or_else(|| sent.iter().position(|x| x.text.is_empty()));
        if let Some(i) = hit {
            sent.remove(i);
        }
        if sent.is_empty() {
            book.remove(sid);
        }
        hit.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent() -> Actor {
        Actor { kind: ActorKind::Agent, session: Some("b".into()), name: None, trigger: None, via: None, peer: None }
    }

    fn human() -> Actor {
        Actor { kind: ActorKind::Human, session: None, name: None, trigger: None, via: None, peer: None }
    }

    #[test]
    fn an_agents_typed_prompt_is_not_the_humans_and_is_used_up() {
        let t = Typed::default();
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "text": "check the others", "enter": true }));
        assert!(t.by_human("b", "check the others"), "another terminal");
        assert!(!t.by_human("a", "check  the\nothers"));
        assert!(t.by_human("a", "check the others"), "used up");
    }

    #[test]
    fn text_typed_without_enter_is_not_a_send() {
        let t = Typed::default();
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "text": "draft", "enter": false }));
        assert!(t.by_human("a", "draft"));
    }

    #[test]
    fn a_bare_enter_owns_the_next_prompt() {
        let t = Typed::default();
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "key": "Enter" }));
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "key": "down" }));
        assert!(!t.by_human("a", "whatever was in the box"));
        assert!(t.by_human("a", "the human's next one"));
    }

    #[test]
    fn a_queued_send_counts_once_and_a_humans_queued_message_is_theirs() {
        let t = Typed::default();
        // Through session.input, then the queue's own `sent`, for the same message.
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "text": "ping", "enter": true }));
        t.observe(kinds::SESSION_QUEUE, &agent(), Some("a"), &json!({ "action": "sent", "text": "ping", "enter": true }));
        assert!(!t.by_human("a", "ping"));
        assert!(t.by_human("a", "ping"));
        t.observe(kinds::SESSION_QUEUE, &human(), Some("a"), &json!({ "action": "sent", "text": "go on", "enter": true }));
        assert!(t.by_human("a", "go on"));
        t.observe(kinds::SESSION_QUEUE, &agent(), Some("a"), &json!({ "action": "sent", "text": "left in the box", "enter": false }));
        assert!(t.by_human("a", "left in the box"), "not submitted");
    }

    #[test]
    fn a_clipped_preview_or_an_image_prefix_still_matches() {
        let t = Typed::default();
        let long = "x ".repeat(150);
        let preview: String = long.chars().take(200).collect::<String>() + "…";
        t.observe(kinds::SESSION_QUEUE, &agent(), Some("a"), &json!({ "action": "sent", "text": preview, "enter": true }));
        assert!(!t.by_human("a", &long));
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "text": "look at this", "enter": true, "images": ["/x.png"] }));
        assert!(!t.by_human("a", "[Image #1] look at this"));
    }

    #[test]
    fn a_closed_terminal_forgets_its_sends() {
        let t = Typed::default();
        t.observe(kinds::SESSION_INPUT_BY_AGENT, &agent(), Some("a"), &json!({ "text": "hi", "enter": true }));
        t.observe(kinds::SESSION_CLOSED, &human(), Some("a"), &json!({}));
        assert!(t.by_human("a", "hi"));
    }
}
