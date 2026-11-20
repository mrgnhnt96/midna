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
//! streams.add / list / remove / set_enabled / restart manage the streams midnad runs itself
//! (`crate::producers`): their producers' stdout lines come in through [`admit`] too.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::error::BUSY;
use midna_proto::streams::{
    EVENTS_KEPT, MAX_PAYLOAD_BYTES, StreamSpec, StreamsAddParams, StreamsEventsParams, StreamsEventsResult, StreamsNameParams, StreamsPublishParams, StreamsPublishResult,
    StreamsSetEnabledParams, check_publish, oversize, valid_name,
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
    found.ok_or_else(|| {
        let names: Vec<String> = d.core().state.streams.iter().map(|s| s.name.clone()).collect();
        let known = if names.is_empty() { "midna runs none (`midna stream add`)".to_string() } else { format!("midna runs: {}", names.join(", ")) };
        RpcError::not_found(format!("no stream `{name}` ({known})"))
    })
}

pub fn add(d: &Arc<Daemon>, ctx: &Ctx, p: StreamsAddParams) -> R {
    if !ctx.is_human() {
        return Err(RpcError::refused(
            "only the human can add a stream for now: it runs a command in the background with the secrets it names, so it needs \
             the human's approval, and asking for that isn't built yet",
        ));
    }
    if !valid_name(&p.name) {
        return Err(bad_name(&p.name));
    }
    if p.command.is_empty() || p.command[0].trim().is_empty() {
        return Err(RpcError::bad_params("command is empty: give the producer's argv (`midna stream add <name> -- <command...>`)"));
    }
    if d.core().state.streams.iter().any(|s| s.name == p.name) {
        return Err(RpcError::bad_params(format!("stream `{}` exists; remove it first (`midna stream remove {}`)", p.name, p.name)));
    }
    let explicit = p.project_id.as_deref().map(|id| super::project::resolve(d, id)).transpose()?;
    let cwd = match (&p.cwd, &explicit) {
        (Some(c), _) => c.clone(),
        (None, Some(proj)) => proj.path.clone(),
        (None, None) => ctx.session.as_deref().and_then(|sid| d.core().state.sessions.iter().find(|s| s.id == sid).map(|s| s.cwd.clone())).ok_or_else(|| RpcError::bad_params("give cwd (an absolute folder) or a project"))?,
    };
    if !std::path::Path::new(&cwd).is_absolute() || !std::path::Path::new(&cwd).is_dir() {
        return Err(RpcError::bad_params(format!("cwd `{cwd}` must be an absolute folder that exists")));
    }
    let project_id = super::secret::project_of(d, ctx, explicit.as_ref().map(|p| &p.id), Some(&cwd));
    let mut secrets: Vec<String> = vec![];
    for n in &p.secrets {
        if !midna_proto::secrets::valid_name(n) {
            return Err(RpcError::bad_params(format!("`{n}` isn't a secret name (A-Z 0-9 _, e.g. SLACK_TOKEN)")));
        }
        // Stored now, or the producer could never start.
        super::secret::resolve(d, n, project_id.as_ref())?;
        if !secrets.contains(n) {
            secrets.push(n.clone());
        }
    }
    let spec = StreamSpec { name: p.name.clone(), command: p.command, cwd, project_id, secrets, enabled: true, created_at: time::now_rfc3339() };
    {
        let mut core = d.core();
        if core.state.streams.iter().any(|s| s.name == spec.name) {
            return Err(RpcError::bad_params(format!("stream `{}` exists; remove it first (`midna stream remove {}`)", spec.name, spec.name)));
        }
        core.state.streams.push(spec);
    }
    d.save_now();
    crate::producers::start_one(d, &p.name);
    ok(crate::producers::status(d, &p.name))
}

pub fn list(d: &Daemon) -> R {
    let names: Vec<String> = d.core().state.streams.iter().map(|s| s.name.clone()).collect();
    ok(names.iter().filter_map(|n| crate::producers::status(d, n)).collect::<Vec<_>>())
}

pub fn remove(d: &Daemon, p: StreamsNameParams) -> R {
    spec(d, &p.name)?;
    d.core().state.streams.retain(|s| s.name != p.name);
    d.save_now();
    crate::producers::forget(d, &p.name);
    ok(OkResult { ok: true })
}

pub fn set_enabled(d: &Arc<Daemon>, p: StreamsSetEnabledParams) -> R {
    if spec(d, &p.name)?.enabled == p.enabled {
        // Already so: resuming a running stream doesn't restart it (that's `streams.restart`).
        return ok(crate::producers::status(d, &p.name));
    }
    if let Some(s) = d.core().state.streams.iter_mut().find(|s| s.name == p.name) {
        s.enabled = p.enabled;
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
    if !spec(d, &p.name)?.enabled {
        return Err(RpcError::bad_params(format!("stream `{}` is paused; resume it (`midna stream resume {}`)", p.name, p.name)));
    }
    crate::producers::start_one(d, &p.name);
    ok(crate::producers::status(d, &p.name))
}
