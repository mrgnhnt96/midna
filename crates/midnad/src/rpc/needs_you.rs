//! needs_you.* handlers. Resolution side effects: approvals wake policy.request waiters and
//! may create rules; rule_removal approval removes the rule; deferred human-only calls run.
use super::{Ctx, R, ok};
use crate::agent_state::screen_shows_prompt;
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn list(d: &Daemon, p: NeedsYouListParams) -> R {
    let core = d.core();
    ok(core.state.needs_you.iter().filter(|n| p.session_id.is_none() || n.session_id == p.session_id).cloned().collect::<Vec<_>>())
}

/// Most closed items `needs_you.get` remembers.
const ANSWERED_MAX: usize = 500;

struct Entry {
    r: NeedsYouGetResult,
    /// A `caller.no_wait` call is still finishing after the answer.
    call_running: bool,
}

/// How recently closed needs-you items ended, oldest first (`needs_you.get`). In memory only.
#[derive(Default)]
pub struct Answered {
    recent: VecDeque<Entry>,
    /// Open approvals a `caller.no_wait` call is blocked on.
    no_wait: HashSet<Id>,
}

impl Answered {
    /// Called by `close_needs_you` for every item that closes.
    pub fn record(&mut self, id: &str, resolution: &Value, by: &Actor) {
        let state = match resolution.get("kind").and_then(Value::as_str) {
            Some("withdrawn") => NeedsYouState::Withdrawn,
            Some("timeout") => NeedsYouState::Timeout,
            _ => NeedsYouState::Resolved,
        };
        if self.recent.len() >= ANSWERED_MAX {
            self.recent.pop_front();
        }
        let r = NeedsYouGetResult {
            id: id.into(),
            state,
            item: None,
            resolution: Some(resolution.clone()),
            resolved_by: Some(by.clone()),
            resolved_at: Some(time::now_rfc3339()),
            result: None,
            error: None,
        };
        self.recent.push_back(Entry { r, call_running: self.no_wait.remove(id) });
    }

    /// A `caller.no_wait` call is blocked on approval `id`.
    pub fn expect_call(&mut self, id: &str) {
        self.no_wait.insert(id.to_string());
    }

    /// What the `caller.no_wait` call behind `id` ended with.
    pub fn finish(&mut self, id: &str, res: &Result<Value, RpcError>) {
        self.no_wait.remove(id);
        if let Some(e) = self.recent.iter_mut().rev().find(|e| e.r.id == id) {
            match res {
                Ok(v) => e.r.result = Some(v.clone()),
                Err(err) => e.r.error = serde_json::to_value(err).ok(),
            }
            e.call_running = false;
        }
    }

    fn get(&self, id: &str) -> Option<(NeedsYouGetResult, bool)> {
        self.recent.iter().rev().find(|e| e.r.id == id).map(|e| (e.r.clone(), e.call_running))
    }
}

/// `needs_you.get`: an open item, or how a closed one ended. `wait_secs` waits for the answer
/// (and for a no-wait call behind it to finish).
pub fn get(d: &Daemon, p: NeedsYouGetParams) -> R {
    let deadline = Instant::now() + Duration::from_secs(p.wait_secs.unwrap_or(0).min(600));
    loop {
        let waiting = Instant::now() < deadline;
        let open = d.core().state.needs_you.iter().find(|n| n.id == p.id).cloned();
        if let Some(item) = open {
            if waiting {
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
            let r = NeedsYouGetResult { id: p.id, state: NeedsYouState::Open, item: Some(item), resolution: None, resolved_by: None, resolved_at: None, result: None, error: None };
            return ok(r);
        }
        let answered = d.answered.lock().unwrap_or_else(|e| e.into_inner()).get(&p.id);
        match answered {
            Some((_, true)) if waiting => std::thread::sleep(Duration::from_millis(100)),
            Some((r, _)) => return ok(r),
            None => return Err(RpcError::not_found(format!("no needs-you item {} (unknown, or closed before midnad last restarted)", p.id))),
        }
    }
}

pub fn raise(d: &Daemon, ctx: &Ctx, p: NeedsYouRaiseParams) -> R {
    if !matches!(p.kind, NeedsYouKind::Blocked | NeedsYouKind::Note) {
        return Err(RpcError::bad_params("needs_you.raise accepts kind blocked or note"));
    }
    if p.message.trim().is_empty() {
        return Err(RpcError::bad_params("message must not be empty"));
    }
    let mut item = d.new_needs_you(p.kind, p.message, ctx.actor(), ctx.session.clone());
    item.detail = p.detail.unwrap_or_default();
    item.bulk_safe = p.kind == NeedsYouKind::Note;
    if let Some(sid) = &ctx.session {
        item.screen_excerpt = d.rt(sid).and_then(|rt| rt.read(true)).map(|(l, _, _)| super::session::tail_nonempty(l, 8));
    }
    ok(d.raise_needs_you(item))
}

/// May this caller apply this resolution to this item?
fn authorize(d: &Daemon, ctx: &Ctx, item: &NeedsYou, res: &Resolution) -> Result<(), RpcError> {
    if ctx.is_human() {
        return Ok(());
    }
    let own_session = ctx.session.is_some() && item.session_id == ctx.session;
    let raised_by_me = ctx.session.is_some() && item.asked_by.session == ctx.session;
    match res {
        Resolution::Approve { .. } if item.kind == NeedsYouKind::Approval => {
            let deferred = d.core().state.deferred.contains_key(&item.id);
            if deferred {
                return Err(RpcError::human_only("this approval confirms a human-only action; only the human can approve it"));
            }
            if !d.core().state.setting_bool("approve.from_cli") {
                return Err(RpcError::human_only("agents may not approve unless setting approve.from_cli is on (human only)"));
            }
            if !own_session {
                return Err(RpcError::human_only("agents may only approve requests from their own session"));
            }
            Ok(())
        }
        Resolution::Done | Resolution::Deny if raised_by_me && matches!(item.kind, NeedsYouKind::Note | NeedsYouKind::Blocked | NeedsYouKind::Approval) => Ok(()),
        _ => Err(RpcError::human_only("only the human can resolve this item this way")),
    }
}

pub fn resolve(d: &Arc<Daemon>, ctx: &Ctx, p: NeedsYouResolveParams) -> R {
    let item = d.core().state.needs_you.iter().find(|n| n.id == p.id).cloned().ok_or_else(|| RpcError::not_found(format!("no needs-you item {}", p.id)))?;
    authorize(d, ctx, &item, &p.resolution)?;
    if matches!(p.resolution, Resolution::Approve { .. }) && !matches!(item.kind, NeedsYouKind::Approval | NeedsYouKind::RuleRemoval | NeedsYouKind::PermissionPrompt) {
        return Err(RpcError::bad_params(format!("{:?} items can't be approved", item.kind).to_lowercase()));
    }
    let deferred = d.core().state.deferred.get(&item.id).cloned();
    let mut rule = None;
    match (&p.resolution, item.kind) {
        (Resolution::Approve { scope }, NeedsYouKind::Approval) => {
            if let Some(def) = &deferred {
                // Run the human-only call the agent asked for, now as the human.
                super::run_deferred(d, def)?;
            } else {
                rule = super::policy::rule_from_approval(d, &item, scope);
            }
        }
        (Resolution::Approve { .. }, NeedsYouKind::RuleRemoval) => {
            let rid = d.core().state.rules.iter().find(|r| r.removal_request.as_ref().is_some_and(|rr| rr.needs_you_id == item.id)).map(|r| r.id.clone());
            if let Some(rid) = rid {
                // remove_rule closes this item itself.
                super::policy::remove_rule(d, &rid, ctx.actor())?;
                return ok(ResolveResult { ok: true, rule: None });
            }
        }
        // Keep (deny) or dismiss: either way the request is over and the rule stays.
        (Resolution::Deny | Resolution::Dismiss | Resolution::Done, NeedsYouKind::RuleRemoval) => {
            for r in d.core().state.rules.iter_mut() {
                if r.removal_request.as_ref().is_some_and(|rr| rr.needs_you_id == item.id) {
                    r.removal_request = None;
                }
            }
            d.mark_dirty();
        }
        (Resolution::Approve { .. } | Resolution::Deny, NeedsYouKind::PermissionPrompt) if crate::trust::is_trust_item(&item) => {
            crate::trust::answer(d, &item, matches!(p.resolution, Resolution::Approve { .. }))
        }
        (Resolution::Approve { .. } | Resolution::Deny, NeedsYouKind::PermissionPrompt) => answer_prompt(d, &item, &p.resolution),
        (Resolution::Restart, _) => {
            let sid = item.session_id.clone().ok_or_else(|| RpcError::bad_params("item has no session to restart"))?;
            // An update notice restarts when the agent is idle; a failed terminal right away.
            let when = if item.kind == NeedsYouKind::Note { "idle" } else { "now" };
            let reason = if item.kind == NeedsYouKind::Note { item.title.clone() } else { "failed".into() };
            super::session::restart(d, ctx, SessionRestartParams { id: sid, resume: None, when: Some(when.into()), force: false, reason: Some(reason) })?;
        }
        _ => {}
    }
    d.close_needs_you(&item.id, serde_json::to_value(&p.resolution).unwrap_or_default(), ctx.actor());
    if let Some(tx) = d.waiters.lock().unwrap_or_else(|e| e.into_inner()).remove(&item.id) {
        let _ = tx.send(crate::daemon::Answer::Resolved(p.resolution.clone()));
    }
    ok(ResolveResult { ok: true, rule })
}

/// Answer an agent's on-screen permission prompt, only if the screen check confirms one.
fn answer_prompt(d: &Daemon, item: &NeedsYou, res: &Resolution) {
    let Some(sid) = &item.session_id else { return };
    let Some(rt) = d.rt(sid) else { return };
    let Some((screen, _, _)) = rt.read(true) else { return };
    if !screen_shows_prompt(&screen) {
        return;
    }
    // Claude/Codex prompts default to "Yes" on Enter; Esc cancels. Keys are encoded for the
    // app's keyboard mode (Claude runs with the kitty protocol on).
    let key = if matches!(res, Resolution::Approve { .. }) { crate::term::Key::Enter } else { crate::term::Key::Escape };
    rt.key(key);
}
