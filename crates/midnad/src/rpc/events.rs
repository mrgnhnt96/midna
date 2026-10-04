//! events.list / events.subscribe.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;

pub fn list(d: &Daemon, p: EventsListParams) -> R {
    let limit = p.limit.unwrap_or(200).clamp(1, 10_000) as usize;
    ok(d.log.list(p.since_seq.unwrap_or(0), limit, &p.filter.unwrap_or_default()))
}

/// Replays then streams `event` notifications on this connection. Human (GUI) subscribers
/// also receive `window.command` notifications.
pub fn subscribe(d: &Daemon, ctx: &Ctx, p: EventsSubscribeParams) -> R {
    let seq = d.log.subscribe(p.since_seq, p.filter.unwrap_or_default(), ctx.out.clone());
    if ctx.is_human() {
        d.register_gui(ctx.conn_id, ctx.out.clone());
    }
    ok(SubscribeResult { ok: true, seq })
}
