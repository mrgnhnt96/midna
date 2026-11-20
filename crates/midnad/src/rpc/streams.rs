//! streams.publish: a program on this Mac hands midna an event of its own. It rides the
//! daemon's unix socket like every call (there is no other listener) and lands in the event
//! log as `stream.<name>.<kind>`, where the local trigger worker matches it like any event.
//!
//! A publish a trigger caused ([`Ctx::caused_by`]: a process its run_command or start_agent
//! started, or its headless run) is recorded with `caused_by` on its envelope and fires no
//! triggers, so a trigger whose command publishes to the stream it listens on fires once.
//!
//! The intake (`crate::streams`) drops duplicates, publishes over the stream's rate limit and
//! everything while its queue is full; streams.events lists each publish's verdict.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::streams::{EVENTS_KEPT, MAX_PAYLOAD_BYTES, StreamsEventsParams, StreamsEventsResult, StreamsPublishParams, StreamsPublishResult, check_publish, oversize, valid_kind, valid_name};
use midna_proto::error::BUSY;
use midna_proto::{RpcError, StreamVerdict};
use serde_json::{Value, json};

pub fn publish(d: &Daemon, ctx: &Ctx, p: StreamsPublishParams) -> R {
    let mut stream = match check_publish(&p) {
        Ok(s) => s,
        Err(m) => {
            // Too big but otherwise fine: the stream's events list shows it was dropped.
            if valid_name(&p.stream) && valid_kind(&p.kind) && p.data.is_object() && let Some(size) = oversize(&p) {
                d.streams.reject(&p.stream, &p.kind, p.id.clone(), StreamVerdict::TooLarge, format!("{size} bytes, over {MAX_PAYLOAD_BYTES}"));
                return Err(RpcError::bad_params(m).with_data(json!({ "verdict": StreamVerdict::TooLarge })));
            }
            return Err(RpcError::bad_params(m));
        }
    };
    stream.caused_by = ctx.caused_by();
    let caused_by = stream.caused_by.clone();
    let data = match p.data {
        Value::Null => json!({}),
        v => v,
    };
    let event = stream.event_kind();
    let actor = ctx.actor();
    let a = d.streams.admit(&stream, || d.log.append_stream(stream.clone(), actor, data));
    match (a.verdict, a.seq) {
        (StreamVerdict::Accepted | StreamVerdict::Duplicate, Some(seq)) => ok(StreamsPublishResult { seq, event, verdict: a.verdict, caused_by }),
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
