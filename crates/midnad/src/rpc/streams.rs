//! streams.publish: a program on this Mac hands midna an event of its own. It rides the
//! daemon's unix socket like every call (there is no other listener) and lands in the event
//! log as `stream.<name>.<kind>`, where the local trigger worker matches it like any event.
//!
//! A publish a trigger caused ([`Ctx::caused_by`]: a process its run_command or start_agent
//! started, or its headless run) is recorded with `caused_by` on its envelope and fires no
//! triggers, so a trigger whose command publishes to the stream it listens on fires once.
//!
//! A declared stream (one midna runs, `streams.add`) only takes its own producer's publishes:
//! a call carrying the token midnad gave it at its last start (`MIDNA_STREAM_TOKEN`, sent as
//! `caller.stream_token`), or the human's. Anyone else's is dropped with verdict `refused`, so
//! an agent can't fake its events and fire its triggers. Other names stay open to anyone.
//!
//! The intake (`crate::streams`) drops duplicates, publishes over the stream's rate limit and
//! everything while its queue is full; streams.events lists each publish's verdict.
//!
//! streams.add / update / list / remove / set_enabled / restart manage the streams midnad runs
//! itself (`crate::producers`): their producers' stdout lines come in through [`admit`] too.
//!
//! A stream runs a command in the background with the secrets it names, so an agent's
//! `streams.add` is kept paused (`needs_approval`) and raises one needs-you approval naming its
//! command, cwd, project and secrets; approving it runs `streams.set_enabled` as the human,
//! guarded by a fingerprint of those ([`approval_target`]), so a stream changed after the ask
//! isn't started. An agent's `streams.update` sends it back for approval the same way. Agents may
//! pause, resume an approved stream, restart, and remove a paused one; removing a running one
//! asks the human.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::error::BUSY;
use midna_proto::streams::{
    EVENTS_KEPT, MAX_PAYLOAD_BYTES, StreamSpec, StreamsAddParams, StreamsEventsParams, StreamsEventsResult, StreamsNameParams, StreamsPublishParams, StreamsPublishResult,
    StreamsSetEnabledParams, StreamsUpdateParams, check_publish, oversize, valid_name,
};
use midna_proto::{Actor, OkResult, RpcError, StreamVerdict, TriggerCause, time};
use serde_json::{Value, json};
use std::sync::Arc;

pub fn publish(d: &Daemon, ctx: &Ctx, p: StreamsPublishParams) -> R {
    if !ctx.is_human() && d.core().state.streams.iter().any(|s| s.name == p.stream) && !crate::producers::token_ok(d, &p.stream, ctx.stream_token.as_deref()) {
        // Listed in the stream's events when the rest of the publish is well formed.
        let head = StreamsPublishParams { stream: p.stream.clone(), kind: p.kind.clone(), id: p.id.clone(), data: Value::Null };
        if check_publish(&head).is_ok() {
            d.streams.reject(&p.stream, &p.kind, p.id.clone(), StreamVerdict::Refused, "not its producer or the human".into());
        }
        return Err(RpcError::refused(format!(
            "stream `{}` is one midna runs: only its producer (with its MIDNA_STREAM_TOKEN) or the human can publish to it; dropped",
            p.stream
        ))
        .with_data(json!({ "verdict": StreamVerdict::Refused })));
    }
    ok(admit(d, p, ctx.caused_by(), ctx.actor())?)
}

/// Check a publish and hand it to the intake: `streams.publish`'s and a producer's stdout lines'.
pub(crate) fn admit(d: &Daemon, p: StreamsPublishParams, caused_by: Option<TriggerCause>, actor: Actor) -> Result<StreamsPublishResult, RpcError> {
    let mut stream = match check_publish(&p) {
        Ok(s) => s,
        Err(m) => {
            // Too big but otherwise fine: the stream's events list shows it was dropped.
            let rest_ok = || p.data.is_object() && check_publish(&StreamsPublishParams { stream: p.stream.clone(), kind: p.kind.clone(), id: p.id.clone(), data: Value::Null }).is_ok();
            if let Some(size) = oversize(&p).filter(|_| rest_ok()) {
                d.streams.reject(&p.stream, &p.kind, p.id.clone(), StreamVerdict::TooLarge, format!("{size} bytes, over {MAX_PAYLOAD_BYTES}"));
                return Err(RpcError::bad_params(m).with_data(json!({ "verdict": StreamVerdict::TooLarge })));
            }
            return Err(RpcError::bad_params(m));
        }
    };
    stream.caused_by = caused_by.clone();
    let data = match p.data {
        Value::Null => json!({}),
        v => v,
    };
    let event = stream.event_kind();
    let a = d.streams.admit(&stream, || d.log.append_stream(stream.clone(), actor, data));
    match (a.verdict, a.seq) {
        (StreamVerdict::Accepted | StreamVerdict::Duplicate, Some(seq)) => Ok(StreamsPublishResult { seq, event, verdict: a.verdict, caused_by }),
        (StreamVerdict::RateLimited, _) => {
            let ms = a.retry_after.map_or(1000, |w| w.as_millis().max(1) as u64);
            Err(RpcError::new(BUSY, format!("stream `{}` is over its rate limit (60 events a minute, bursts of 20): dropped; retry in {ms} ms", stream.name))
                .with_data(json!({ "verdict": a.verdict, "retry_after_ms": ms })))
        }
        (v, _) => Err(RpcError::new(BUSY, format!("midna's stream intake is full ({} events waiting): dropped; try again shortly", d.streams.queued())).with_data(json!({ "verdict": v }))),
    }
}

pub fn events(d: &Daemon, p: StreamsEventsParams) -> R {
    if !valid_name(&p.stream) {
        return Err(RpcError::bad_params(format!("stream `{}` must be 1–32 of a-z 0-9 - (e.g. slack)", p.stream)));
    }
    let (events, counts) = d.streams.events(&p.stream, p.limit.unwrap_or(EVENTS_KEPT).min(EVENTS_KEPT));
    ok(StreamsEventsResult { stream: p.stream, events, counts })
}

fn bad_name(name: &str) -> RpcError {
    RpcError::bad_params(format!("stream `{name}` must be 1–32 of a-z 0-9 - (e.g. slack)"))
}

fn spec(d: &Daemon, name: &str) -> Result<StreamSpec, RpcError> {
    let found = d.core().state.streams.iter().find(|s| s.name == name).cloned();
    found.ok_or_else(|| not_found(d, name))
}

fn not_found(d: &Daemon, name: &str) -> RpcError {
    let names: Vec<String> = d.core().state.streams.iter().map(|s| s.name.clone()).collect();
    let known = if names.is_empty() { "midna runs none (`midna stream add`)".to_string() } else { format!("midna runs: {}", names.join(", ")) };
    RpcError::not_found(format!("no stream `{name}` ({known})"))
}

pub fn add(d: &Arc<Daemon>, ctx: &Ctx, p: StreamsAddParams) -> R {
    if !valid_name(&p.name) {
        return Err(bad_name(&p.name));
    }
    check_command(&p.command)?;
    if d.core().state.streams.iter().any(|s| s.name == p.name) {
        return Err(exists(&p.name));
    }
    let explicit = p.project_id.as_deref().map(|id| super::project::resolve(d, id)).transpose()?;
    let cwd = match (&p.cwd, &explicit) {
        (Some(c), _) => c.clone(),
        (None, Some(proj)) => proj.path.clone(),
        (None, None) => ctx.session.as_deref().and_then(|sid| d.core().state.sessions.iter().find(|s| s.id == sid).map(|s| s.cwd.clone())).ok_or_else(|| RpcError::bad_params("give cwd (an absolute folder) or a project"))?,
    };
    check_cwd(&cwd)?;
    let project_id = super::secret::project_of(d, ctx, explicit.as_ref().map(|p| &p.id), Some(&cwd));
    let secrets = check_secrets(d, &p.secrets, project_id.as_ref())?;
    // An agent's stream runs a command in the background with secrets: it waits for the human.
    let agent = !ctx.is_human();
    let spec = StreamSpec { name: p.name.clone(), command: p.command, cwd, project_id, secrets, enabled: !agent, needs_approval: agent, created_at: time::now_rfc3339() };
    {
        let mut core = d.core();
        if core.state.streams.iter().any(|s| s.name == spec.name) {
            return Err(exists(&spec.name));
        }
        core.state.streams.push(spec);
    }
    d.save_now();
    if agent {
        ask_approval(d, ctx, &p.name, &format!("Agent asks to run stream “{}”", p.name));
    } else {
        crate::producers::start_one(d, &p.name);
    }
    ok(crate::producers::status(d, &p.name))
}

/// Change a stream's producer. An agent's change pauses it and asks the human again.
pub fn update(d: &Arc<Daemon>, ctx: &Ctx, p: StreamsUpdateParams) -> R {
    let old = spec(d, &p.name)?;
    let mut next = old.clone();
    if let Some(c) = p.command {
        check_command(&c)?;
        next.command = c;
    }
    if let Some(id) = p.project_id.as_deref() {
        next.project_id = Some(super::project::resolve(d, id)?.id);
    }
    if let Some(c) = p.cwd {
        check_cwd(&c)?;
        next.cwd = c;
    }
    // Checked against the (maybe new) project even when unchanged, or it could never start.
    next.secrets = check_secrets(d, p.secrets.as_deref().unwrap_or(&old.secrets), next.project_id.as_ref())?;
    if producer(&next) == producer(&old) {
        return ok(crate::producers::status(d, &p.name));
    }
    let agent = !ctx.is_human();
    if agent {
        next.enabled = false;
        next.needs_approval = true;
    }
    {
        let mut core = d.core();
        let Some(s) = core.state.streams.iter_mut().find(|s| s.name == p.name) else { return Err(not_found(d, &p.name)) };
        *s = next.clone();
    }
    d.save_now();
    if agent {
        crate::producers::pause(d, &p.name);
        ask_approval(d, ctx, &p.name, &format!("Agent changed stream “{}”: approve to run it", p.name));
    } else {
        // What an earlier approval would have started isn't what it runs now.
        withdraw_approvals(d, &p.name, "the stream was changed after it was asked");
        if next.enabled {
            crate::producers::start_one(d, &p.name);
        }
    }
    ok(crate::producers::status(d, &p.name))
}

pub fn list(d: &Daemon) -> R {
    let names: Vec<String> = d.core().state.streams.iter().map(|s| s.name.clone()).collect();
    ok(names.iter().filter_map(|n| crate::producers::status(d, n)).collect::<Vec<_>>())
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: StreamsNameParams) -> R {
    let s = spec(d, &p.name)?;
    if !ctx.is_human() && s.enabled {
        let params = json!({ "name": p.name });
        let title = format!("Agent asks to remove running stream “{}”", p.name);
        return Err(super::defer_to_human(d, ctx, &title, &format!("stream remove {}", p.name), "streams.remove", &params));
    }
    d.core().state.streams.retain(|s| s.name != p.name);
    d.save_now();
    crate::producers::forget(d, &p.name);
    withdraw_approvals(d, &p.name, "the stream was removed");
    ok(OkResult { ok: true })
}

/// Pause or resume. Resuming a stream that awaits approval is the human's approval; an agent's
/// asks the human (one needs-you approval per stream). Agents may pause and resume a stream the
/// human approved: it runs what they approved.
pub fn set_enabled(d: &Arc<Daemon>, ctx: &Ctx, p: StreamsSetEnabledParams) -> R {
    let s = spec(d, &p.name)?;
    if p.enabled && s.needs_approval && !ctx.is_human() {
        let id = ask_approval(d, ctx, &p.name, &format!("Agent asks to run stream “{}”", p.name));
        return Err(RpcError::human_only(format!("stream {} waits for the human's approval; asked them (needs-you {id})", p.name)).with_data(json!({ "needs_you_id": id })));
    }
    if s.enabled == p.enabled {
        // Already so: resuming a running stream doesn't restart it (that's `streams.restart`).
        return ok(crate::producers::status(d, &p.name));
    }
    if let Some(s) = d.core().state.streams.iter_mut().find(|s| s.name == p.name) {
        s.enabled = p.enabled;
        if p.enabled {
            s.needs_approval = false;
        }
    }
    d.save_now();
    if p.enabled {
        crate::producers::start_one(d, &p.name);
    } else {
        crate::producers::pause(d, &p.name);
    }
    ok(crate::producers::status(d, &p.name))
}

pub fn restart(d: &Arc<Daemon>, p: StreamsNameParams) -> R {
    let s = spec(d, &p.name)?;
    if s.needs_approval {
        let waiting = approval_of(d, &p.name).map(|id| format!(" (needs-you {id})")).unwrap_or_default();
        return Err(RpcError::bad_params(format!("stream `{}` waits for the human's approval{waiting}", p.name)));
    }
    if !s.enabled {
        return Err(RpcError::bad_params(format!("stream `{}` is paused; resume it (`midna stream resume {}`)", p.name, p.name)));
    }
    crate::producers::start_one(d, &p.name);
    ok(crate::producers::status(d, &p.name))
}

fn exists(name: &str) -> RpcError {
    RpcError::bad_params(format!("stream `{name}` exists; change it (`midna stream update {name}`) or remove it first (`midna stream remove {name}`)"))
}

fn check_command(command: &[String]) -> Result<(), RpcError> {
    if command.is_empty() || command[0].trim().is_empty() {
        return Err(RpcError::bad_params("command is empty: give the producer's argv (`midna stream add <name> -- <command...>`)"));
    }
    Ok(())
}

fn check_cwd(cwd: &str) -> Result<(), RpcError> {
    let p = std::path::Path::new(cwd);
    if !p.is_absolute() || !p.is_dir() {
        return Err(RpcError::bad_params(format!("cwd `{cwd}` must be an absolute folder that exists")));
    }
    Ok(())
}

/// Secret names, each stored now (or the producer could never start), without repeats.
fn check_secrets(d: &Daemon, names: &[String], project: Option<&midna_proto::Id>) -> Result<Vec<String>, RpcError> {
    let mut out: Vec<String> = vec![];
    for n in names {
        if !midna_proto::secrets::valid_name(n) {
            return Err(RpcError::bad_params(format!("`{n}` isn't a secret name (A-Z 0-9 _, e.g. SLACK_TOKEN)")));
        }
        super::secret::resolve(d, n, project)?;
        if !out.contains(n) {
            out.push(n.clone());
        }
    }
    Ok(out)
}

/// What a stream runs, the part the human approves.
fn producer(s: &StreamSpec) -> (&[String], &str, Option<&midna_proto::Id>, &[String]) {
    (&s.command, &s.cwd, s.project_id.as_ref(), &s.secrets)
}

/// What a `streams.set_enabled` / `streams.remove` approval acts on, for `deferred_guard`:
/// the stream's command, cwd, project and secrets (and whether it still waits), as the text
/// fingerprinted and as what the human reads. None when there's no such stream.
pub(crate) fn approval_target(d: &Daemon, name: &str) -> Option<(String, String)> {
    let core = d.core();
    let s = core.state.streams.iter().find(|s| s.name == name)?;
    let def = json!({ "command": s.command, "cwd": s.cwd, "project": s.project_id, "secrets": s.secrets, "needs_approval": s.needs_approval });
    let project = match &s.project_id {
        Some(id) => core.state.projects.iter().find(|p| &p.id == id).map_or_else(|| id.to_string(), |p| format!("{} ({id})", p.name)),
        None => "none".into(),
    };
    let secrets = if s.secrets.is_empty() { "none".to_string() } else { format!("{} (their values go only into its environment)", s.secrets.join(", ")) };
    let shown = format!(
        "Stream “{name}” runs in the background, started again whenever it exits:\n  command: {}\n  cwd: {}\n  project: {project}\n  secrets: {secrets}",
        shell_words(&s.command),
        s.cwd
    );
    Some((def.to_string(), shown))
}

/// argv as one line, quoting words a shell would split.
fn shell_words(argv: &[String]) -> String {
    let quote = |w: &String| {
        if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+%".contains(c)) { w.clone() } else { format!("'{}'", w.replace('\'', r"'\''")) }
    };
    argv.iter().map(quote).collect::<Vec<_>>().join(" ")
}

/// Open approvals that would start stream `name` (deferred `streams.set_enabled`).
fn approvals(d: &Daemon, name: &str) -> Vec<(midna_proto::Id, Option<String>)> {
    let core = d.core();
    let is_start = |def: &crate::state::Deferred| def.method == "streams.set_enabled" && def.params["name"] == name && def.params["enabled"] == true;
    core.state.deferred.iter().filter(|(_, def)| is_start(def)).map(|(id, def)| (id.clone(), def.guard.clone())).collect()
}

/// Stream `name`'s open approval, while it awaits one (`StreamStatus::approval`).
pub(crate) fn approval_of(d: &Daemon, name: &str) -> Option<midna_proto::Id> {
    approvals(d, name).into_iter().next().map(|(id, _)| id)
}

/// Take back stream `name`'s open approvals: it was removed or changed.
fn withdraw_approvals(d: &Daemon, name: &str, why: &str) {
    for (id, _) in approvals(d, name) {
        d.withdraw_needs_you(&id, why);
    }
}

/// Ask the human to start stream `name` as it is now: one needs-you approval per stream, which
/// runs `streams.set_enabled` as the human. An open one for what it runs now is reused; one for
/// something it no longer runs is taken back. Returns the approval's id.
fn ask_approval(d: &Daemon, ctx: &Ctx, name: &str, title: &str) -> midna_proto::Id {
    let params = json!({ "name": name, "enabled": true });
    let now = super::deferred_guard(d, "streams.set_enabled", &params).0;
    let mut reuse = None;
    for (id, guard) in approvals(d, name) {
        if reuse.is_none() && guard == now {
            reuse = Some(id);
        } else {
            d.withdraw_needs_you(&id, "the agent asked again for what the stream runs now");
        }
    }
    if let Some(id) = reuse {
        return id;
    }
    let e = super::defer_to_human(d, ctx, title, &format!("stream resume {name}"), "streams.set_enabled", &params);
    e.data.as_ref().and_then(|v| v["needs_you_id"].as_str()).unwrap_or_default().into()
}
