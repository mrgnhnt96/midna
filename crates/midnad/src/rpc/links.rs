//! links.*: an agent terminal's session links (collected in `crate::links`).
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;

/// The terminal a call is about: `session`, else the caller's own.
fn session(d: &Daemon, ctx: &Ctx, sid: Option<Id>) -> Result<Id, RpcError> {
    let sid = sid.or(ctx.session.clone()).ok_or_else(|| RpcError::bad_params("no session: pass `session` or run inside a midna terminal"))?;
    if d.core().state.session(&sid).is_none() {
        return Err(RpcError::not_found(format!("no terminal {sid}")));
    }
    Ok(sid)
}

pub fn list(d: &Daemon, ctx: &Ctx, p: LinksListParams) -> R {
    let sid = session(d, ctx, p.session)?;
    let turn = match &p.turn {
        None => None,
        Some(TurnRef::N(n)) => Some(*n),
        Some(TurnRef::Name(s)) => match s.trim() {
            "last" | "latest" => Some(crate::links::prompt_times(d, &sid).len() as u32).filter(|n| *n > 0),
            n => Some(n.parse::<u32>().map_err(|_| RpcError::bad_params(format!("turn must be a prompt number or \"last\", not `{n}`")))?),
        },
    };
    let links = d
        .links
        .list(&d.cfg.home, &sid)
        .into_iter()
        .filter(|l| p.kind.is_none_or(|k| l.kind == k) && (!p.pinned || l.pinned) && (p.turn.is_none() || turn.is_some_and(|t| l.turns.contains(&t))))
        .collect();
    ok(LinksListResult { session: sid, turn, links })
}

pub fn pin(d: &Daemon, ctx: &Ctx, p: LinksPinParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(crate::links::pin(d, &sid, &p.link, p.pinned, ctx.actor())?)
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: LinksRemoveParams) -> R {
    let sid = session(d, ctx, p.session)?;
    ok(if p.restore { crate::links::restore(d, &sid, &p.link)? } else { crate::links::remove(d, &sid, &p.link)? })
}

pub fn add(d: &Daemon, ctx: &Ctx, p: LinksAddParams) -> R {
    let sid = session(d, ctx, p.session.clone())?;
    ok(crate::links::add(d, &sid, &p, ctx.actor())?)
}
