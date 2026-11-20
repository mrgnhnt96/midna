//! streams.publish: a program on this Mac hands midna an event of its own. It rides the
//! daemon's unix socket like every call (there is no other listener) and lands in the event
//! log as `stream.<name>.<kind>`, where the local trigger worker matches it like any event.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::RpcError;
use midna_proto::streams::{StreamsPublishParams, StreamsPublishResult, check_publish};
use serde_json::{Value, json};

pub fn publish(d: &Daemon, ctx: &Ctx, p: StreamsPublishParams) -> R {
    let stream = check_publish(&p).map_err(RpcError::bad_params)?;
    let data = match p.data {
        Value::Null => json!({}),
        v => v,
    };
    let e = d.log.append_stream(stream, ctx.actor(), data);
    ok(StreamsPublishResult { seq: e.seq, event: e.kind })
}
