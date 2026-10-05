//! queue.*: a terminal's queued messages (the sender is `crate::queue`).
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use std::sync::Arc;

/// The terminal a call is about: `session`, else the caller's own.
fn session(d: &Daemon, ctx: &Ctx, sid: Option<Id>) -> Result<Id, RpcError> {
    let sid = sid.or(ctx.session.clone()).ok_or_else(|| RpcError::bad_params("no session: pass `session` or run inside a midna terminal"))?;
    if d.core().state.session(&sid).is_none() {
        return Err(RpcError::not_found(format!("no terminal {sid}")));
    }
    Ok(sid)
}

pub fn list(d: &Daemon, ctx: &Ctx, p: QueueListParams) -> R {
    ok(crate::queue::list(d, &session(d, ctx, p.session)?)?)
}

pub fn add(d: &Daemon, ctx: &Ctx, p: QueueAddParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::queue::add(d, &sid, p.text, p.enter, p.images, p.when, p.position, ctx.actor(), None)?)
}

pub fn update(d: &Daemon, ctx: &Ctx, p: QueueUpdateParams) -> R {
    let sid = session(d, ctx, p.session.clone())?;
    ok(crate::queue::update(d, &sid, p, ctx.actor())?)
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: QueueItemParams) -> R {
    let sid = session(d, ctx, p.session)?;
    crate::queue::remove(d, &sid, &p.id, ctx.actor())?;
    ok(OkResult { ok: true })
}

pub fn clear(d: &Daemon, ctx: &Ctx, p: QueueListParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::queue::clear(d, &sid, ctx.actor())?)
}

pub fn move_to(d: &Daemon, ctx: &Ctx, p: QueueMoveParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::queue::move_to(d, &sid, &p.id, p.to, ctx.actor())?)
}

pub fn send_now(d: &Arc<Daemon>, ctx: &Ctx, p: QueueItemParams) -> R {
    let sid = session(d, ctx, p.session)?;
    crate::queue::send_now(d, &sid, &p.id)?;
    ok(OkResult { ok: true })
}

pub fn pause(d: &Daemon, ctx: &Ctx, p: QueuePauseParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::queue::pause(d, &sid, p.paused, ctx.actor())?)
}
