//! insights.summary / insights.activity (computed in crate::insights from events only).
use super::{R, ok};
use crate::daemon::Daemon;
use crate::insights::{self, Labels};
use midna_proto::*;

pub fn summary(d: &Daemon, p: InsightsSummaryParams) -> R {
    let labels = {
        let core = d.core();
        Labels {
            projects: core.state.projects.iter().map(|p| (p.id.clone(), p.name.clone())).collect(),
            sessions: core.state.sessions.iter().map(|s| (s.id.clone(), s.name.clone())).collect(),
        }
    };
    let now = time::now_unix();
    ok(d.log.with_events(|ev| insights::summary(ev, p.range, p.by, now, &labels)))
}

pub fn activity(d: &Daemon, p: InsightsActivityParams) -> R {
    let since = match &p.since {
        Some(s) => time::parse_rfc3339(s).ok_or_else(|| RpcError::bad_params("since must be RFC 3339"))?,
        None => time::now_unix() - 86_400,
    };
    let limit = p.limit.unwrap_or(100).clamp(1, 1000) as usize;
    ok(d.log.with_events(|ev| insights::activity(ev, since, &p.filter.unwrap_or_default(), limit)))
}

pub fn series(d: &Daemon, p: InsightsSeriesParams) -> R {
    let labels = {
        let core = d.core();
        Labels {
            projects: core.state.projects.iter().map(|p| (p.id.clone(), p.name.clone())).collect(),
            sessions: core.state.sessions.iter().map(|s| (s.id.clone(), s.name.clone())).collect(),
        }
    };
    let now = time::now_unix();
    ok(d.log.with_events(|ev| insights::series(ev, &p, now, &labels)))
}
