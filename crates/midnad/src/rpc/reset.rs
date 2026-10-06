//! `daemon.reset` (human only): start over without losing the history.
//!
//! Closes every terminal, removes projects, triggers (and their secrets), stored secrets, deliveries and
//! needs-you items, and resets settings. Built-in triggers come back as shipped. The event log is kept (it is the audit trail), and
//! rules are kept unless `keep_rules: false`. Every removal emits its usual event, followed by
//! one `daemon.reset` summary.
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::json;
use std::sync::Arc;

pub fn reset(d: &Arc<Daemon>, ctx: &Ctx, p: DaemonResetParams) -> R {
    let keep_rules = p.keep_rules.unwrap_or(true);
    let mut out = DaemonResetResult { ok: true, ..Default::default() };

    // Terminals first: their needs-you items go with them.
    let sessions: Vec<Id> = d.core().state.sessions.iter().map(|s| s.id.clone()).collect();
    for sid in &sessions {
        super::session::close_inner(d, ctx, sid, true);
    }
    out.sessions_closed = sessions.len() as u32;

    // Whatever is left, including approvals someone is blocked on (they get a deny).
    let items: Vec<Id> = d.core().state.needs_you.iter().map(|n| n.id.clone()).collect();
    for id in &items {
        d.close_needs_you(id, json!({ "kind": "dismiss", "reason": "daemon reset" }), ctx.actor());
        if let Some(tx) = d.waiters.lock().unwrap_or_else(|e| e.into_inner()).remove(id) {
            let _ = tx.send(crate::daemon::Answer::Resolved(Resolution::Deny));
        }
    }
    out.needs_you_cleared = items.len() as u32;
    let deferred = std::mem::take(&mut d.core().state.deferred);
    for def in deferred.values() {
        super::secret::drop_pending(d, def);
    }

    let triggers: Vec<Trigger> = d.core().state.triggers.iter().filter(|t| t.builtin.is_none()).cloned().collect();
    for t in &triggers {
        d.webhooks.secrets.delete(&t.id);
        d.emit(kinds::TRIGGER_REMOVED, ctx.actor(), None, None, json!({ "id": t.id, "name": t.name, "reason": "daemon reset" }));
    }
    out.triggers_removed = triggers.len() as u32;
    let deliveries: Vec<Id> = {
        let mut core = d.core();
        core.state.triggers.clear();
        core.state.seeded.clear();
        std::mem::take(&mut core.state.deliveries).into_iter().map(|x| x.id).collect()
    };
    for id in deliveries {
        let _ = std::fs::remove_file(d.cfg.home.join("deliveries").join(format!("{id}.json")));
    }

    let secrets = std::mem::take(&mut d.core().state.secrets);
    for s in &secrets {
        d.vault.delete(&s.id);
        d.emit(kinds::SECRET_REMOVED, ctx.actor(), s.project_id.clone(), None, json!({ "id": s.id, "name": s.name, "reason": "daemon reset" }));
    }

    let projects: Vec<Id> = d.core().state.projects.iter().map(|p| p.id.clone()).collect();
    d.core().state.projects.clear();
    for id in &projects {
        d.emit(kinds::PROJECT_REMOVED, ctx.actor(), Some(id.clone()), None, json!({ "id": id, "reason": "daemon reset" }));
    }
    out.projects_removed = projects.len() as u32;

    let keys: Vec<String> = d.core().state.settings.keys().cloned().collect();
    for key in &keys {
        // settings.reset emits settings.changed (and rewrites hook files where needed).
        let _ = super::settings::reset(d, ctx, SettingKeyParams { key: key.clone() });
    }
    // Unknown keys (from a newer build) have no catalog entry: drop them directly.
    d.core().state.settings.clear();
    out.settings_reset = keys.len() as u32;

    if !keep_rules {
        let ids: Vec<Id> = d.core().state.rules.iter().map(|r| r.id.clone()).collect();
        for id in &ids {
            let _ = super::policy::remove_rule(d, id, ctx.actor());
        }
        out.rules_removed = ids.len() as u32;
    }

    crate::local::seed_builtins(d);
    d.mark_dirty();
    d.save_now();
    d.emit(kinds::DAEMON_RESET, ctx.actor(), None, None, json!({ "keep_rules": keep_rules, "result": out }));
    ok(out)
}
