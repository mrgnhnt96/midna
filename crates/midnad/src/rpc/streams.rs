//! streams.publish: a program on this Mac hands midna an event of its own. It rides the
//! daemon's unix socket like every call (there is no other listener) and lands in the event
//! log as `stream.<name>.<kind>`, where the local trigger worker matches it like any event.
//!
//! A publish a trigger caused ([`Ctx::caused_by`]: a process its run_command or start_agent
//! started, or its headless run) is recorded with `caused_by` on its envelope and fires no
//! triggers, so a trigger whose command publishes to the stream it listens on fires once.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::RpcError;
use midna_proto::streams::{StreamsPublishParams, StreamsPublishResult, check_publish};
use serde_json::{Value, json};

pub fn publish(d: &Daemon, ctx: &Ctx, p: StreamsPublishParams) -> R {
    let mut stream = check_publish(&p).map_err(RpcError::bad_params)?;
    stream.caused_by = ctx.caused_by();
    let caused_by = stream.caused_by.clone();
    let data = match p.data {
        Value::Null => json!({}),
        v => v,
    };
    let e = d.log.append_stream(stream, ctx.actor(), data);
    ok(StreamsPublishResult { seq: e.seq, event: e.kind, caused_by })
}
