//! Local event streams: a program on this Mac hands midna an event of its own
//! (`streams.publish`, `midna stream publish`, the `streams_publish` tool). It lands in the
//! event log as kind `stream.<name>.<kind>` with the payload as its data, so local triggers
//! match it like any midna event (`event: stream.slack.*`, `filter.match`, `{{data.<path>}}`),
//! and the envelope (`stream`, `kind`, `id`) is kept on the event as [`StreamRef`].
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Event kinds of published events start with this.
pub const EVENT_PREFIX: &str = "stream.";
/// The largest payload `streams.publish` takes, as JSON.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_NAME: usize = 32;
pub const MAX_KIND: usize = 64;
pub const MAX_ID: usize = 128;

/// What `streams.publish` said about an event, kept on it (`Event.stream`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamRef {
    /// The stream's name (`slack`).
    pub name: String,
    /// What happened, as the publisher named it (`message`).
    pub kind: String,
    /// The publisher's id for the event, if it gave one (e.g. Slack's message ts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl StreamRef {
    /// The event kind it is logged as: `stream.<name>.<kind>`.
    pub fn event_kind(&self) -> String {
        format!("{EVENT_PREFIX}{}.{}", self.name, self.kind)
    }

    /// The stream and kind of a published event's kind (`stream.slack.message`); no id.
    pub fn of_event(event: &str) -> Option<StreamRef> {
        let (name, kind) = event.strip_prefix(EVENT_PREFIX)?.split_once('.')?;
        (valid_name(name) && valid_kind(kind)).then(|| StreamRef { name: name.into(), kind: kind.into(), id: None })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StreamsPublishParams {
    /// The stream's name: 1–32 of `a-z 0-9 -` (e.g. `slack`, `ci`).
    pub stream: String,
    /// What happened: 1–64 of `a-z 0-9 _ . -` (e.g. `message`, `build.failed`).
    pub kind: String,
    /// Your id for the event (1–128 characters, e.g. Slack's message ts), recorded on the
    /// event as `{{stream.id}}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The payload: a JSON object, at most 64 KB. It is the event's data, so triggers see it as
    /// `filter.match` paths and `{{data.<path>}}`. Omitted: `{}`.
    #[serde(default)]
    pub data: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct StreamsPublishResult {
    /// The event's seq in the log (`events.list`).
    pub seq: u64,
    /// Its kind: `stream.<name>.<kind>`, what a local trigger's `event` matches.
    pub event: String,
}

/// A stream name: 1–32 of `a-z 0-9 -`.
pub fn valid_name(s: &str) -> bool {
    (1..=MAX_NAME).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// An event kind within a stream: 1–64 of `a-z 0-9 _ . -`.
pub fn valid_kind(s: &str) -> bool {
    (1..=MAX_KIND).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

/// Check a publish and return its envelope. The payload must be an object (or null, read as
/// `{}`) of at most [`MAX_PAYLOAD_BYTES`] as JSON.
pub fn check_publish(p: &StreamsPublishParams) -> Result<StreamRef, String> {
    if !valid_name(&p.stream) {
        return Err(format!("stream `{}` must be 1–{MAX_NAME} of a-z 0-9 - (e.g. slack)", p.stream));
    }
    if !valid_kind(&p.kind) {
        return Err(format!("kind `{}` must be 1–{MAX_KIND} of a-z 0-9 _ . - (e.g. message)", p.kind));
    }
    if let Some(id) = &p.id
        && (id.is_empty() || id.chars().count() > MAX_ID || id.chars().any(char::is_control))
    {
        return Err(format!("id must be 1–{MAX_ID} characters with no control characters"));
    }
    match &p.data {
        Value::Object(_) | Value::Null => {}
        _ => return Err("data must be a JSON object (e.g. {\"text\":\"…\"})".into()),
    }
    let size = p.data.to_string().len();
    if size > MAX_PAYLOAD_BYTES {
        return Err(format!("data is {size} bytes as JSON; the most a stream event carries is {MAX_PAYLOAD_BYTES} (64 KB)"));
    }
    Ok(StreamRef { name: p.stream.clone(), kind: p.kind.clone(), id: p.id.clone() })
}

/// A local trigger's `event` that names streams (`stream.slack.message`, `stream.slack.*`,
/// `stream.*`): `Ok(true)` if it is a well-formed stream glob, `Ok(false)` if it isn't about
/// streams at all, `Err` with the fix if it means streams but can never match one.
pub fn check_event_glob(event: &str) -> Result<bool, String> {
    let e = event.trim().to_ascii_lowercase();
    if e.starts_with("streams.") {
        return Err(format!("stream events are `stream.<name>.<kind>` (e.g. stream.slack.message), not `{}`", event.trim()));
    }
    let Some(rest) = e.strip_prefix(EVENT_PREFIX) else { return Ok(false) };
    let glob = |b: u8| matches!(b, b'*' | b'?');
    let (name, kind) = match rest.split_once('.') {
        Some((n, k)) => (n, Some(k)),
        // `stream.*` / `stream.sl*`: a `*` can stand for the dot too.
        None if rest.contains('*') => (rest, None),
        None => return Err(format!("`{}` names no kind: use stream.{rest}.<kind> or stream.{rest}.* for all of them", event.trim())),
    };
    let name_ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || glob(b));
    let kind_ok = kind.is_none_or(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-') || glob(b)));
    if !name_ok || !kind_ok {
        return Err(format!(
            "`{}` can't match a stream event: they are stream.<name>.<kind>, name a-z 0-9 -, kind a-z 0-9 _ . - (globs * and ? allowed)",
            event.trim()
        ));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(stream: &str, kind: &str, id: Option<&str>, data: Value) -> StreamsPublishParams {
        StreamsPublishParams { stream: stream.into(), kind: kind.into(), id: id.map(Into::into), data }
    }

    #[test]
    fn publish_checks_name_kind_id_and_payload() {
        let r = check_publish(&p("slack", "message", Some("1700000000.0001"), json!({ "text": "hi" }))).unwrap();
        assert_eq!(r.event_kind(), "stream.slack.message");
        assert_eq!(r.id.as_deref(), Some("1700000000.0001"));
        assert!(check_publish(&p("ci-2", "build.failed_x", None, Value::Null)).is_ok(), "null data reads as {{}}");
        for (s, k) in [("", "m"), ("Slack", "m"), ("sl.ack", "m"), ("s_l", "m"), (&"a".repeat(33), "m"), ("s", ""), ("s", "Message"), ("s", "a b"), ("s", &"k".repeat(65))] {
            assert!(check_publish(&p(s, k, None, json!({}))).is_err(), "{s:?} {k:?} should be refused");
        }
        assert!(check_publish(&p("s", "k", Some(""), json!({}))).is_err());
        assert!(check_publish(&p("s", "k", Some("a\nb"), json!({}))).is_err());
        assert!(check_publish(&p("s", "k", Some(&"i".repeat(129)), json!({}))).is_err());
        for bad in [json!([1]), json!("text"), json!(3)] {
            assert!(check_publish(&p("s", "k", None, bad)).unwrap_err().contains("object"));
        }
        let big = json!({ "t": "x".repeat(MAX_PAYLOAD_BYTES) });
        assert!(check_publish(&p("s", "k", None, big)).unwrap_err().contains("64 KB"));
        let fits = json!({ "t": "x".repeat(MAX_PAYLOAD_BYTES - 10) });
        assert!(check_publish(&p("s", "k", None, fits)).is_ok());
    }

    #[test]
    fn event_kind_round_trips() {
        let r = StreamRef::of_event("stream.slack.build.failed").unwrap();
        assert_eq!((r.name.as_str(), r.kind.as_str()), ("slack", "build.failed"));
        assert_eq!(r.event_kind(), "stream.slack.build.failed");
        assert!(StreamRef::of_event("stream.slack").is_none());
        assert!(StreamRef::of_event("session.status").is_none());
        assert!(StreamRef::of_event("stream.Sl ack.m").is_none());
    }

    #[test]
    fn trigger_event_globs() {
        for ok in ["stream.slack.message", "stream.slack.*", "stream.*", "stream.sl*", "Stream.Slack.Message", "stream.ci-?.build.*"] {
            assert_eq!(check_event_glob(ok), Ok(true), "{ok}");
        }
        for other in ["agent.turn_ended", "idle", "hook.Stop", "session.*"] {
            assert_eq!(check_event_glob(other), Ok(false), "{other}");
        }
        assert!(check_event_glob("stream.slack").unwrap_err().contains("stream.slack.*"));
        assert!(check_event_glob("streams.slack.message").unwrap_err().contains("stream.<name>.<kind>"));
        assert!(check_event_glob("stream.sl_ack.message").is_err());
        assert!(check_event_glob("stream..message").is_err());
        assert!(check_event_glob("stream.slack.").is_err());
    }
}
