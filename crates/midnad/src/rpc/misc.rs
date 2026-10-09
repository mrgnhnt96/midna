//! daemon.info and the parts of the trigger group that already work (read-only lists).
use super::{Ctx, R, Role, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use std::sync::Arc;

pub fn daemon_info(d: &Daemon, ctx: &Ctx) -> R {
    ok(DaemonInfo {
        version: midna_proto::VERSION.into(),
        pid: std::process::id(),
        uptime_secs: d.started.elapsed().as_secs(),
        started_at: d.started_at.clone(),
        home: d.cfg.home.to_string_lossy().into_owned(),
        socket: d.cfg.socket.to_string_lossy().into_owned(),
        role: if ctx.role == Role::Human { "human" } else { "agent" }.into(),
        session: ctx.session.clone(),
        binary: crate::install::running_binary().map(|p| p.to_string_lossy().into_owned()),
    })
}

/// `daemon.upgrade` (human only; agents were turned into a needs-you approval by `call`).
pub fn daemon_upgrade(d: &Arc<Daemon>, p: UpgradeParams) -> R {
    crate::upgrade::request(d, std::path::Path::new(&p.binary_path), "upgrade", p.force)
}

/// `daemon.restart`: the same handoff, re-exec'ing our own (or the installed) binary.
pub fn daemon_restart(d: &Arc<Daemon>, p: RestartParams) -> R {
    let bin = crate::upgrade::restart_binary(&d.cfg);
    crate::upgrade::request(d, &bin, "restart", p.force)
}

/// `daemon.stop` (human only): answer first, then hang up every terminal and stop.
pub fn daemon_stop(d: &Arc<Daemon>) -> R {
    let d = d.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        crate::server::stop_daemon(&d);
    });
    ok(OkResult { ok: true })
}

pub fn deliveries(d: &Daemon, p: TriggerDeliveriesParams) -> R {
    let core = d.core();
    let mut v: Vec<Delivery> =
        core.state.deliveries.iter().filter(|x| p.trigger_id.is_none() || x.trigger_id == p.trigger_id).cloned().collect();
    let limit = p.limit.unwrap_or(50) as usize;
    if v.len() > limit {
        v.drain(..v.len() - limit);
    }
    ok(v)
}
