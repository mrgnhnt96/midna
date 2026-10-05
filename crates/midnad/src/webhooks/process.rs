//! The delivery pipeline: parse → dedupe → verify → match → (answer) → run actions → record.
//!
//! `receive` is the fast part done before answering the HTTP request; `complete` runs the
//! trigger actions and records the Delivery. Replays and recovered deliveries skip the
//! signature check (they were verified on arrival / fetched through authenticated `gh`) and
//! go straight to `evaluate` + `complete`.
use super::payload::{self, Facts};
use super::sig;
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use crate::state::hex_id;
use midna_proto::*;
use serde_json::{Value, json};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;

/// Deliveries kept in state (and payload files on disk).
pub const MAX_DELIVERIES: usize = 500;

pub struct Request {
    pub source: TriggerSource,
    /// Lowercased header names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Live,
    Replay,
    Recovered,
}

/// Everything `complete` needs.
pub struct Work {
    pub delivery: Delivery,
    pub firing: Vec<Trigger>,
    pub facts: Facts,
    pub payload: Value,
    /// Keep the payload on disk for replay (only for authenticated deliveries).
    pub store: bool,
}

pub struct Outcome {
    pub status: u16,
    pub body: String,
    pub work: Option<Work>,
}

pub fn source_name(s: TriggerSource) -> &'static str {
    match s {
        TriggerSource::Github => "github",
        TriggerSource::Bitbucket => "bitbucket",
        TriggerSource::Local => "local",
    }
}

fn payload_dir(d: &Daemon) -> PathBuf {
    d.cfg.home.join("deliveries")
}

fn new_delivery(source: TriggerSource, event: &str, guid: &str) -> Delivery {
    Delivery {
        id: format!("d_{}", hex_id(6)),
        source,
        event: event.to_string(),
        delivery_guid: guid.to_string(),
        received_at: time::now_rfc3339(),
        verdict: Verdict::NoTrigger,
        trigger_id: None,
        session_started: None,
        summary: String::new(),
        action: None,
        repo: None,
        subject: None,
        http_status: None,
        eval: vec![],
        triggers_fired: vec![],
        sessions_started: vec![],
        recovered: false,
        replay_of: None,
        body_sha256: None,
    }
}

/// Record a delivery that needs no further work (rejected, duplicate-free, nothing to run).
fn finish_now(d: &Daemon, mut del: Delivery, status: u16, body: &str) -> Outcome {
    del.http_status = Some(status);
    record(d, &del, None);
    Outcome { status, body: body.to_string(), work: None }
}

/// Unauthenticated rejects recorded per minute. Anyone who can reach the public URL can send
/// these; past the cap they are answered but not recorded, so a flood can't grow the state and
/// the event log without bound.
pub const MAX_REJECTS_PER_MIN: u32 = 30;

/// Like `finish_now`, for requests that never proved they came from GitHub/Bitbucket.
fn reject_unauthenticated(d: &Daemon, del: Delivery, status: u16, body: &str) -> Outcome {
    let record_it = {
        let mut g = d.webhooks.rejects.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        if now.duration_since(g.0) > std::time::Duration::from_secs(60) {
            *g = (now, 0);
        }
        g.1 += 1;
        g.1 <= MAX_REJECTS_PER_MIN
    };
    if record_it {
        return finish_now(d, del, status, body);
    }
    Outcome { status, body: body.to_string(), work: None }
}

pub fn body_digest(body: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    sig::hex_encode(&Sha256::digest(body))
}

/// GitHub may send `application/x-www-form-urlencoded` with the JSON in `payload=`.
fn parse_body(body: &[u8]) -> Option<Value> {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        return Some(v);
    }
    let s = std::str::from_utf8(body).ok()?;
    let enc = s.split('&').find_map(|kv| kv.strip_prefix("payload="))?;
    serde_json::from_str(&url_decode(enc)).ok()
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(x) => {
                        out.push(x);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Already recorded with a good signature (GUID, or the same signed body under another GUID)?
fn seen(d: &Daemon, guid: &str, digest: Option<&str>) -> bool {
    d.core().state.deliveries.iter().any(|x| {
        x.verdict != Verdict::BadSignature
            && x.replay_of.is_none()
            && (x.delivery_guid == guid || (digest.is_some() && x.body_sha256.as_deref() == digest))
    })
}

/// Seen already, or being processed right now? Otherwise claim it (until `release`).
fn is_duplicate(d: &Daemon, guid: &str, digest: Option<&str>) -> bool {
    if seen(d, guid, digest) {
        return true;
    }
    let mut inflight = d.webhooks.inflight.lock().unwrap_or_else(|e| e.into_inner());
    let dkey = digest.map(|x| format!("sha256:{x}"));
    if inflight.contains(guid) || dkey.as_ref().is_some_and(|k| inflight.contains(k)) {
        return true;
    }
    inflight.insert(guid.to_string());
    if let Some(k) = dkey {
        inflight.insert(k);
    }
    false
}

fn release(d: &Daemon, guid: &str, digest: Option<&str>) {
    let mut inflight = d.webhooks.inflight.lock().unwrap_or_else(|e| e.into_inner());
    inflight.remove(guid);
    if let Some(x) = digest {
        inflight.remove(&format!("sha256:{x}"));
    }
}

/// The fast path for a live HTTP delivery.
pub fn receive(d: &Arc<Daemon>, req: Request) -> Outcome {
    let (event_h, guid_h, sig_h) = match req.source {
        TriggerSource::Github => ("x-github-event", "x-github-delivery", "x-hub-signature-256"),
        TriggerSource::Bitbucket => ("x-event-key", "x-request-uuid", "x-hub-signature"),
        TriggerSource::Local => return Outcome { status: 404, body: "not found".into(), work: None },
    };
    let event = req.header(event_h).unwrap_or("").trim().to_string();
    let guid = req.header(guid_h).map(|g| g.trim().trim_matches(['{', '}']).to_string()).filter(|g| !g.is_empty());
    let mut del = new_delivery(req.source, &event, guid.as_deref().unwrap_or(""));
    if event.is_empty() || guid.is_none() {
        del.verdict = Verdict::BadSignature;
        del.summary = format!("missing {} or {} header · dropped, nothing ran", header_name(event_h), header_name(guid_h));
        return reject_unauthenticated(d, del, 400, r#"{"error":"missing event or delivery headers"}"#);
    }
    let guid = guid.unwrap_or_default();
    // GitHub redelivers on timeouts: a GUID we already handled is answered and nothing runs.
    // (Only after the signature check is anything claimed, so an unsigned request reusing a
    // GUID can't block the real delivery.)
    if seen(d, &guid, None) {
        return duplicate(&guid);
    }
    verify_and_match(d, &req, del, &event, sig_h, &guid)
}

fn duplicate(guid: &str) -> Outcome {
    Outcome { status: 200, body: json!({ "duplicate": true, "delivery": guid }).to_string(), work: None }
}

fn header_name(h: &str) -> String {
    h.split('-').map(|p| if p == "github" { "GitHub".to_string() } else { p[..1].to_uppercase() + &p[1..] }).collect::<Vec<_>>().join("-")
}

fn verify_and_match(d: &Arc<Daemon>, req: &Request, mut del: Delivery, event: &str, sig_h: &str, guid: &str) -> Outcome {
    let candidates: Vec<Trigger> = d.core().state.triggers.iter().filter(|t| t.source == req.source && t.secret_set).cloned().collect();
    if candidates.is_empty() {
        del.verdict = Verdict::NoTrigger;
        del.summary = format!("No {} trigger has a secret yet · nothing verified, nothing ran", source_name(req.source));
        return reject_unauthenticated(d, del, 202, r#"{"accepted":true,"verdict":"no_trigger"}"#);
    }
    let Some(sig) = req.header(sig_h) else {
        del.verdict = Verdict::BadSignature;
        del.summary = format!("missing {} · dropped, nothing ran", header_name(sig_h));
        return reject_unauthenticated(d, del, 401, r#"{"error":"missing signature"}"#);
    };
    let verified: Vec<Trigger> = candidates.into_iter().filter(|t| d.webhooks.secrets.get(&t.id).is_some_and(|s| sig::verify(&s, &req.body, sig))).collect();
    if verified.is_empty() {
        del.verdict = Verdict::BadSignature;
        del.summary = format!("{} mismatch · dropped, nothing ran", header_name(sig_h));
        return reject_unauthenticated(d, del, 401, r#"{"error":"bad signature"}"#);
    }
    let digest = body_digest(&req.body);
    if is_duplicate(d, guid, Some(&digest)) {
        return duplicate(guid);
    }
    del.body_sha256 = Some(digest.clone());
    let Some(payload) = parse_body(&req.body) else {
        del.verdict = Verdict::NoTrigger;
        del.summary = "signature ok but the body isn't JSON · nothing ran".into();
        let out = finish_now(d, del, 400, r#"{"error":"body is not JSON"}"#);
        release(d, guid, Some(&digest));
        return out;
    };
    if req.source == TriggerSource::Github && event == "ping" {
        let out = ping(d, del, &verified, &payload);
        release(d, guid, Some(&digest));
        return out;
    }
    let work = evaluate(&verified, req.source, event, del, payload, Mode::Live);
    let body = json!({ "accepted": true, "verdict": work.delivery.verdict, "delivery": work.delivery.id }).to_string();
    Outcome { status: 202, body, work: Some(work) }
}

/// A verified GitHub `ping` (sent when a webhook is created): remember the hook id on the
/// triggers whose secret signed it, so missed-delivery recovery works without extra setup.
fn ping(d: &Daemon, mut del: Delivery, verified: &[Trigger], payload: &Value) -> Outcome {
    let hook = payload.get("hook_id").and_then(Value::as_u64);
    let repo = payload.pointer("/repository/full_name").and_then(Value::as_str).map(str::to_string);
    let mut linked = vec![];
    if let Some(h) = hook {
        let mut core = d.core();
        for t in core.state.triggers.iter_mut().filter(|t| verified.iter().any(|v| v.id == t.id)) {
            let repo_ok = match (&t.filter.repo, &repo) {
                (Some(want), Some(got)) => crate::policy::glob_match(&want.to_lowercase(), &got.to_lowercase()),
                _ => true,
            };
            if repo_ok && t.github_hook_id != Some(h) {
                t.github_hook_id = Some(h);
                linked.push(t.name.clone());
            }
        }
    }
    if !linked.is_empty() {
        d.mark_dirty();
    }
    del.verdict = Verdict::Verified;
    del.repo = repo;
    del.trigger_id = verified.first().map(|t| t.id.clone());
    del.summary = match (hook, linked.is_empty()) {
        (Some(h), false) => format!("ping · linked hook {h} to {}", linked.join(", ")),
        _ => "ping · signature ok".into(),
    };
    del.http_status = Some(200);
    record(d, &del, None);
    Outcome { status: 200, body: r#"{"pong":true}"#.into(), work: None }
}

/// Evaluate `triggers` against a payload and decide the verdict (no side effects).
pub fn evaluate(triggers: &[Trigger], source: TriggerSource, event: &str, mut del: Delivery, payload: Value, mode: Mode) -> Work {
    let facts = payload::facts(source, event, &payload);
    let evals: Vec<(&Trigger, payload::Eval)> = triggers.iter().filter(|t| t.source == source).map(|t| (t, payload::evaluate(t, &facts))).collect();
    let firing: Vec<Trigger> = evals.iter().filter(|(_, e)| e.fires()).map(|(t, _)| (*t).clone()).collect();
    let listening: Vec<&(&Trigger, payload::Eval)> = evals.iter().filter(|(_, e)| e.event_ok).collect();
    del.action = facts.action.clone();
    del.repo = facts.repo.clone();
    del.subject = facts.subject.clone();
    del.http_status = Some(202);
    del.eval = evals.iter().filter(|(_, e)| e.event_ok).map(|(_, e)| e.line.clone()).collect();
    del.recovered = mode == Mode::Recovered;
    let full_event = match &facts.action {
        Some(a) if source == TriggerSource::Github => format!("{event}.{a}"),
        _ => event.to_string(),
    };
    if !firing.is_empty() {
        del.verdict = match mode {
            Mode::Live => Verdict::Verified,
            Mode::Replay => Verdict::Replayed,
            Mode::Recovered => Verdict::Recovered,
        };
        del.trigger_id = Some(firing[0].id.clone());
        del.triggers_fired = firing.iter().map(|t| t.id.clone()).collect();
        del.summary = format!("Firing {}", firing.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "));
    } else if let Some((t, e)) = listening.first() {
        del.verdict = if mode == Mode::Replay { Verdict::Replayed } else { Verdict::Filtered };
        del.trigger_id = Some(t.id.clone());
        let why = e.line.split_once(": ").map(|(_, w)| w).unwrap_or(&e.line);
        del.summary = format!("{} said no: {why}", t.name);
    } else {
        del.verdict = if mode == Mode::Replay { Verdict::Replayed } else { Verdict::NoTrigger };
        del.summary = format!("No trigger listens for {full_event}{}", facts.repo.as_ref().map(|r| format!(" on {r}")).unwrap_or_default());
    }
    Work { delivery: del, firing, facts, payload, store: true }
}

/// Run the actions, update the triggers, record the delivery, emit events.
pub fn complete(d: &Arc<Daemon>, w: Work) -> Delivery {
    let Work { mut delivery, firing, facts, payload, store } = w;
    let mut outcomes = vec![];
    for t in &firing {
        let (session, outcome) = run_action(d, t, &facts, &payload);
        if let Some(sid) = &session {
            delivery.sessions_started.push(sid.clone());
        }
        let project = t.action.project_id().cloned();
        let summary = facts.subject.clone().or_else(|| facts.pr_number.as_ref().map(|n| format!("#{n}")));
        {
            let mut core = d.core();
            if let Some(tt) = core.state.triggers.iter_mut().find(|x| x.id == t.id) {
                tt.fired += 1;
                tt.last_fired_at = Some(time::now_rfc3339());
                tt.last_fired_summary = summary.clone();
            }
        }
        d.mark_dirty();
        d.emit(
            kinds::TRIGGER_FIRED,
            trigger_actor(t),
            project,
            session.clone(),
            json!({ "trigger_id": t.id, "delivery_id": delivery.id, "session_id": session, "outcome": outcome, "replay": delivery.replay_of.is_some(), "recovered": delivery.recovered }),
        );
        outcomes.push(outcome);
    }
    delivery.session_started = delivery.sessions_started.first().cloned();
    if !outcomes.is_empty() {
        let prefix = if delivery.replay_of.is_some() { "Replayed · " } else if delivery.recovered { "Recovered · " } else { "" };
        delivery.summary = format!("{prefix}{}", outcomes.join("; "));
    } else if delivery.replay_of.is_some() && !delivery.summary.starts_with("Replayed") {
        delivery.summary = format!("Replayed · {}", delivery.summary);
    }
    record(d, &delivery, store.then_some(&payload));
    release(d, &delivery.delivery_guid, delivery.body_sha256.as_deref());
    delivery
}

pub fn trigger_actor(t: &Trigger) -> Actor {
    Actor { kind: ActorKind::Trigger, session: None, name: Some(t.name.clone()) }
}

fn agent_label(a: AgentKind) -> &'static str {
    match a {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
    }
}

/// Run one trigger's action. Returns (session started, one-line outcome).
pub fn run_action(d: &Arc<Daemon>, t: &Trigger, f: &Facts, payload: &Value) -> (Option<Id>, String) {
    let ctx = Ctx::internal_trigger(trigger_actor(t));
    let name = || {
        let tpl = t.session_name_template.clone().unwrap_or_else(|| match &f.pr_number {
            Some(_) => format!("{} #{{{{pr.number}}}}", t.name),
            None => t.name.clone(),
        });
        let n = payload::render(&tpl, f, payload, false);
        if n.trim().is_empty() { t.name.clone() } else { n }
    };
    let open = |params: Value| -> Result<Id, String> {
        let p: SessionOpenParams = serde_json::from_value(params).map_err(|e| e.to_string())?;
        let v = crate::rpc::session::open(d, &ctx, p).map_err(|e| e.message)?;
        Ok(v["id"].as_str().unwrap_or_default().to_string())
    };
    match &t.action {
        TriggerAction::StartAgent { project_id, agent, prompt_template } => {
            let prompt = payload::render_prompt(prompt_template, f, payload);
            let n = name();
            match open(json!({ "project_id": project_id, "kind": "agent", "agent": agent, "name": n, "prompt": prompt })) {
                Ok(id) => (Some(id), format!("Started {} › {n}", agent_label(*agent))),
                Err(e) => (None, format!("Could not start {}: {e}", agent_label(*agent))),
            }
        }
        TriggerAction::RunCommand { project_id, command } => {
            let cmd = payload::render(command, f, payload, true);
            let n = name();
            match open(json!({ "project_id": project_id, "kind": "monitor", "name": n, "command": [cmd] })) {
                Ok(id) => (Some(id), format!("Ran command › {n}")),
                Err(e) => (None, format!("Could not run command: {e}")),
            }
        }
        TriggerAction::Attention { message } => {
            let msg = payload::render(message, f, payload, false);
            let mut item = d.new_needs_you(NeedsYouKind::Note, if msg.trim().is_empty() { t.name.clone() } else { msg }, trigger_actor(t), None);
            item.detail = [f.subject.clone(), f.url.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ");
            item.bulk_safe = true;
            item.trigger_id = Some(t.id.clone());
            let item = d.raise_needs_you(item);
            (None, format!("Raised attention ({})", item.id))
        }
        TriggerAction::Notify { title, body, sound } => {
            let (title, body) = (payload::render(title, f, payload, false), payload::render(body, f, payload, false));
            let body = if body.trim().is_empty() { [f.subject.clone(), f.url.clone()].into_iter().flatten().collect::<Vec<_>>().join(" · ") } else { body };
            (None, crate::local::notify(d, t, None, &title, &body, *sound))
        }
        a => (None, format!("Skipped: {} needs a local trigger", crate::local::action_name(a))),
    }
}

/// Append a delivery (pruning old ones), keep its payload for replay, emit `trigger.delivery`.
pub fn record(d: &Daemon, del: &Delivery, payload: Option<&Value>) {
    if let Some(p) = payload {
        let dir = payload_dir(d);
        let _ = std::fs::create_dir_all(&dir);
        let body = json!({ "source": del.source, "event": del.event, "guid": del.delivery_guid, "payload": p });
        if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(dir.join(format!("{}.json", del.id))) {
            use std::io::Write;
            let _ = f.write_all(body.to_string().as_bytes());
        }
    }
    let pruned: Vec<Id> = {
        let mut core = d.core();
        core.state.deliveries.push(del.clone());
        let n = core.state.deliveries.len();
        if n > MAX_DELIVERIES { core.state.deliveries.drain(..n - MAX_DELIVERIES).map(|x| x.id).collect() } else { vec![] }
    };
    for id in pruned {
        let _ = std::fs::remove_file(payload_dir(d).join(format!("{id}.json")));
    }
    d.mark_dirty();
    let project = del.trigger_id.as_deref().and_then(|tid| {
        d.core().state.triggers.iter().find(|t| t.id == tid).and_then(|t| t.action.project_id().cloned())
    });
    d.emit(kinds::TRIGGER_DELIVERY, Actor::system(), project, del.session_started.clone(), serde_json::to_value(del).unwrap_or_default());
}

/// The stored payload of a past delivery: (event, payload).
pub fn load_payload(d: &Daemon, delivery_id: &str) -> Option<(String, Value)> {
    if !delivery_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let b = std::fs::read(payload_dir(d).join(format!("{delivery_id}.json"))).ok()?;
    let v: Value = serde_json::from_slice(&b).ok()?;
    Some((v.get("event")?.as_str()?.to_string(), v.get("payload")?.clone()))
}

/// Re-run a recorded delivery through the current triggers (no signature check).
pub fn replay(d: &Arc<Daemon>, original: &Delivery) -> Result<Delivery, RpcError> {
    if original.verdict == Verdict::BadSignature {
        return Err(RpcError::conflict("this delivery failed its signature check; it can't be replayed"));
    }
    let (event, payload) = load_payload(d, &original.id).ok_or_else(|| RpcError::not_found(format!("payload for {} is no longer stored", original.id)))?;
    let triggers: Vec<Trigger> = d.core().state.triggers.clone();
    let mut del = new_delivery(original.source, &event, &original.delivery_guid);
    del.replay_of = Some(original.id.clone());
    let w = evaluate(&triggers, original.source, &event, del, payload, Mode::Replay);
    Ok(complete(d, w))
}

/// A delivery fetched from GitHub's log that midnad never received.
pub fn recover(d: &Arc<Daemon>, event: &str, guid: &str, payload: Value) -> Option<Delivery> {
    if is_duplicate(d, guid, None) {
        return None;
    }
    let triggers: Vec<Trigger> = d.core().state.triggers.clone();
    let del = new_delivery(TriggerSource::Github, event, guid);
    let w = evaluate(&triggers, TriggerSource::Github, event, del, payload, Mode::Recovered);
    Some(complete(d, w))
}

/// Dry run of one trigger (trigger.test): nothing runs, nothing is recorded.
pub fn dry_run(t: &Trigger, event: &str, payload: Value) -> Delivery {
    let mut del = new_delivery(t.source, event, "test");
    del.id = "d_test".into();
    let facts = payload::facts(t.source, event, &payload);
    let e = payload::evaluate(t, &facts);
    let mut w = evaluate(std::slice::from_ref(t), t.source, event, del, payload.clone(), Mode::Live);
    w.delivery.eval = vec![e.line.clone()];
    w.delivery.http_status = None;
    let would = match &t.action {
        TriggerAction::StartAgent { agent, prompt_template, .. } => format!("would start {} with prompt: {}", agent_label(*agent), payload::render_prompt(prompt_template, &facts, &payload)),
        TriggerAction::RunCommand { command, .. } => format!("would run: {}", payload::render(command, &facts, &payload, true)),
        TriggerAction::Attention { message } => format!("would raise attention: {}", payload::render(message, &facts, &payload, false)),
        TriggerAction::Notify { title, body, .. } => format!("would notify: {} {}", payload::render(title, &facts, &payload, false), payload::render(body, &facts, &payload, false)).trim().to_string(),
        a => format!("would do nothing: {} needs a local trigger", crate::local::action_name(a)),
    };
    w.delivery.summary = match (e.event_ok && e.filters_ok, e.enabled_ok) {
        (true, true) => format!("Matches · {would}"),
        (true, false) => format!("Matches, but the trigger isn't enabled · {would}"),
        _ => format!("Doesn't match · {}", e.line.split_once(": ").map(|(_, w)| w).unwrap_or(&e.line)),
    };
    w.delivery.verdict = if e.event_ok && e.filters_ok { Verdict::Verified } else { Verdict::Filtered };
    w.delivery
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_encoded_body() {
        let v = parse_body(b"payload=%7B%22action%22%3A%22opened%22%2C%22n%22%3A%22a+b%22%7D").unwrap();
        assert_eq!(v["action"], "opened");
        assert_eq!(v["n"], "a b");
        assert!(parse_body(b"nope").is_none());
    }
    #[test]
    fn header_names() {
        assert_eq!(header_name("x-hub-signature-256"), "X-Hub-Signature-256");
        assert_eq!(header_name("x-github-event"), "X-GitHub-Event");
    }
}
