//! policy.* and rule.* handlers, the approval wait, and the gate used for agent CLI/window verbs.
use super::{Ctx, R, ok};
use crate::daemon::{Answer, Daemon};
use crate::policy::{escape_glob, evaluate, is_expired};
use crate::state::hex_id;
use midna_proto::*;
use serde_json::json;
use std::time::Duration;

/// Fill the action's session/project from the caller when missing.
fn complete(d: &Daemon, ctx: &Ctx, mut a: PolicyAction) -> PolicyAction {
    if a.session.is_none() {
        a.session = ctx.session.clone();
    }
    if a.project.is_none() {
        a.project = super::session_project(d, a.session.as_deref());
    }
    a
}

fn check_now(d: &Daemon, a: &PolicyAction) -> CheckResult {
    let core = d.core();
    evaluate(&core.state.rules, a, time::now_unix(), &core.state.setting_str("policy.default"))
}

/// Evaluate and, when a rule decided, count it as fired. Decisions of the defaults table
/// (no rule matched) are logged as `policy.decided`, so the Rules feed can show them too.
fn evaluate_and_fire(d: &Daemon, a: &PolicyAction, actor: &Actor) -> CheckResult {
    let res = check_now(d, a);
    if res.rule.is_none() {
        let setting = d.core().state.setting_str("policy.default");
        // An unmatched tool call has no midna opinion: the agent's own permission flow decides.
        let passthrough = a.kind == ActionKind::Tool;
        d.emit(
            kinds::POLICY_DECIDED,
            actor.clone(),
            a.project.clone(),
            a.session.clone(),
            json!({ "decision": res.decision, "source": "default", "default": setting, "passthrough": passthrough, "action": a }),
        );
    }
    if let Some(r) = &res.rule {
        let now = time::now_rfc3339();
        let mut core = d.core();
        if let Some(rule) = core.state.rules.iter_mut().find(|x| x.id == r.id) {
            rule.fired += 1;
            rule.last_fired_at = Some(now);
        }
        d.mark_dirty();
        d.emit(
            kinds::RULE_FIRED,
            actor.clone(),
            a.project.clone(),
            a.session.clone(),
            json!({ "rule_id": r.id, "effect": r.effect, "action": a }),
        );
    }
    res
}

pub fn check(d: &Daemon, ctx: &Ctx, p: PolicyCheckParams) -> R {
    ok(check_now(d, &complete(d, ctx, p.action)))
}

pub fn request(d: &Daemon, ctx: &Ctx, p: PolicyRequestParams) -> R {
    let a = complete(d, ctx, p.action);
    let res = evaluate_and_fire(d, &a, &ctx.actor());
    let decided = |r: &CheckResult| PolicyRequestResult {
        decision: r.decision,
        source: r.source,
        rule: r.rule.clone(),
        needs_you_id: None,
        reason: r.rule.as_ref().map(|x| format!("rule {} ({} `{}`)", x.id, x.effect.as_str(), x.matcher.pattern)),
    };
    // Unmatched tool calls have no midna opinion: the agent's own permission flow decides.
    if res.decision != Effect::Ask || (res.source == DecisionSource::Default && a.kind == ActionKind::Tool) {
        return ok(decided(&res));
    }
    let timeout = p.timeout_secs.unwrap_or_else(|| d.core().state.setting_i64("policy.request_timeout_secs").max(1) as u64);
    ok(ask_human(d, ctx, &a, res.rule.map(|r| r.id), p.detail, timeout, None))
}

/// How often a blocked approval checks that its caller is still connected.
const PEER_CHECK: Duration = Duration::from_millis(500);

/// Raise an approval and block until the human answers or `timeout_secs` passes. `target` is
/// the terminal the action is about, when not the asker's own. The approval is withdrawn (and
/// this returns deny, source `withdrawn`) if the caller disconnects, or if its terminal or the
/// target closes first (`session::close_inner`). A `caller.no_wait` call lets its caller go
/// as soon as the item is up and keeps it open for at least a day.
pub fn ask_human(d: &Daemon, ctx: &Ctx, a: &PolicyAction, matched_rule: Option<Id>, detail: Option<String>, timeout_secs: u64, target: Option<Id>) -> PolicyRequestResult {
    let title = match a.kind {
        ActionKind::Tool => format!("Allow {}?", a.value),
        _ => format!("Allow `{}`?", a.value),
    };
    let mut item = d.new_needs_you(NeedsYouKind::Approval, title, ctx.actor(), a.session.clone());
    item.project_id = item.project_id.or(a.project.clone());
    item.detail = detail.unwrap_or_default();
    let target = target.filter(|t| a.session.as_ref() != Some(t));
    item.approval = Some(ApprovalRequest { action: a.clone(), matched_rule, target_session: target });
    let timeout_secs = if ctx.detached.is_some() { timeout_secs.max(super::NO_WAIT_TIMEOUT_SECS) } else { timeout_secs };
    let (tx, rx) = std::sync::mpsc::channel();
    d.waiters.lock().unwrap_or_else(|e| e.into_inner()).insert(item.id.clone(), tx);
    if let Some(nw) = &ctx.detached {
        nw.raising(d, &item.id);
    }
    // The asking agent is blocked on the human until this returns (Claude's PreToolUse hook
    // keeps its title spinning meanwhile, so nothing else would show it is waiting).
    // "Agent" = launched as one, or a terminal whose agent has been sending hooks.
    let agent_sid = a.session.clone().filter(|sid| {
        let core = d.core();
        core.state.session(sid).is_some_and(|s| (s.agent.is_some() || core.agents.contains_key(sid)) && s.status.state != StatusState::Exited)
    });
    if let Some(sid) = &agent_sid {
        d.set_status(sid, StatusState::NeedsYou, Some(format!("approval {}", a.value)), None, Actor::system());
    }
    let item = d.raise_needs_you(item);
    if let Some(nw) = &ctx.detached {
        nw.raised(&item.id);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let answer = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break None;
        }
        match rx.recv_timeout(left.min(PEER_CHECK)) {
            Ok(a) => break Some(a),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            // Nobody is left to act on the answer: take the question back.
            Err(_) if ctx.detached.is_none() && ctx.out.peer_gone() => {
                let why = "the caller that asked disconnected";
                d.withdraw_needs_you(&item.id, why);
                break Some(Answer::Withdrawn(why.into()));
            }
            Err(_) => {}
        }
    };
    d.waiters.lock().unwrap_or_else(|e| e.into_inner()).remove(&item.id);
    if let Some(sid) = &agent_sid {
        let waiting = d.core().state.session(sid).is_some_and(|s| s.status.state == StatusState::NeedsYou);
        if waiting {
            d.set_status(sid, StatusState::Working, Some("approval answered".into()), None, Actor::system());
        }
    }
    let mut r = PolicyRequestResult { decision: Effect::Deny, source: DecisionSource::Human, rule: None, needs_you_id: Some(item.id.clone()), reason: None };
    match answer {
        Some(Answer::Resolved(Resolution::Approve { scope })) => {
            r.decision = Effect::Allow;
            r.reason = Some(format!("approved by the human ({})", scope_name(&scope)));
        }
        Some(Answer::Resolved(other)) => r.reason = Some(format!("{} by the human", resolution_name(&other))),
        Some(Answer::Withdrawn(why)) => {
            r.source = DecisionSource::Withdrawn;
            r.reason = Some(format!("approval withdrawn: {why}"));
        }
        None => {
            d.close_needs_you(&item.id, json!({ "kind": "timeout" }), Actor::system());
            r.decision = Effect::Ask;
            r.source = DecisionSource::Timeout;
            r.reason = Some(format!("no answer within {timeout_secs}s"));
        }
    }
    r
}

pub fn scope_name(s: &ApprovalScope) -> String {
    match s {
        ApprovalScope::Once => "once".into(),
        ApprovalScope::Minutes { minutes } => format!("{minutes} min"),
        ApprovalScope::Session => "this session".into(),
        ApprovalScope::Always => "always".into(),
    }
}

fn resolution_name(r: &Resolution) -> &'static str {
    match r {
        Resolution::Approve { .. } => "approved",
        Resolution::Deny => "denied",
        Resolution::Dismiss => "dismissed",
        Resolution::Done => "marked done",
        Resolution::Restart => "restarted",
    }
}

/// Policy gate for agent-initiated CLI/window verbs. `force_ask` turns a default allow into ask.
pub fn gate(d: &Daemon, ctx: &Ctx, kind: ActionKind, value: &str, target: Option<&Session>, force_ask: bool) -> Result<(), RpcError> {
    gate_with(d, ctx, kind, value, target, force_ask.then_some(Effect::Ask))
}

/// [`gate`] with the decision to use when no rule matches (`None` = `policy.default` and its
/// defaults table). A matching rule always wins. An approval it raises names `target`, so it
/// is withdrawn if that terminal closes first.
pub fn gate_with(d: &Daemon, ctx: &Ctx, kind: ActionKind, value: &str, target: Option<&Session>, default: Option<Effect>) -> Result<(), RpcError> {
    let mut a = complete(d, ctx, PolicyAction { kind, value: value.to_string(), session: None, project: None });
    if a.project.is_none() {
        a.project = target.map(|s| s.project_id.clone());
    }
    let res = evaluate_and_fire(d, &a, &ctx.actor());
    let decision = match default {
        Some(e) if res.source == DecisionSource::Default => e,
        _ => res.decision,
    };
    match decision {
        Effect::Allow => Ok(()),
        Effect::Deny => Err(RpcError::refused(format!(
            "`{value}` denied by {}",
            res.rule.map(|r| format!("rule {}", r.id)).unwrap_or_else(|| "policy.default".into())
        ))),
        Effect::Ask => {
            let timeout = d.core().state.setting_i64("policy.request_timeout_secs").max(1) as u64;
            let r = ask_human(d, ctx, &a, res.rule.map(|r| r.id), None, timeout, target.map(|s| s.id.clone()));
            if r.decision == Effect::Allow {
                Ok(())
            } else {
                Err(RpcError::refused(format!("`{value}`: {}", r.reason.unwrap_or_default()))
                    .with_data(json!({ "needs_you_id": r.needs_you_id, "source": r.source })))
            }
        }
    }
}

// ------------------------------------------------------------------ rules

pub fn rule_list(d: &Daemon) -> R {
    expire_rules(d);
    ok(d.core().state.rules.clone())
}

pub fn new_rule(effect: Effect, matcher: Matcher, scope: RuleScope, expires_in: Option<u64>, by: Actor) -> Rule {
    let now = time::now_unix();
    Rule {
        id: format!("r_{}", hex_id(6)),
        effect,
        matcher,
        scope,
        expires_at: expires_in.map(|s| time::format_unix(now + s as i64)),
        added_by: by,
        added_at: time::format_unix(now),
        origin: None,
        fired: 0,
        last_fired_at: None,
        removal_request: None,
    }
}

pub fn insert_rule(d: &Daemon, rule: Rule) -> Rule {
    let project = match &rule.scope {
        RuleScope::Project(p) => Some(p.clone()),
        _ => None,
    };
    let session = match &rule.scope {
        RuleScope::Session(s) => Some(s.clone()),
        _ => None,
    };
    d.core().state.rules.push(rule.clone());
    d.mark_dirty();
    d.emit(kinds::RULE_ADDED, rule.added_by.clone(), project, session, serde_json::to_value(&rule).unwrap_or_default());
    rule
}

pub fn rule_add(d: &Daemon, ctx: &Ctx, p: RuleAddParams) -> R {
    if p.matcher.pattern.trim().is_empty() {
        return Err(RpcError::bad_params("matcher.pattern must not be empty"));
    }
    let scope = p.scope.unwrap_or(RuleScope::Global);
    {
        let core = d.core();
        match &scope {
            RuleScope::Project(id) if core.state.project(id).is_none() => return Err(RpcError::not_found(format!("no project {id}"))),
            RuleScope::Session(id) if core.state.session(id).is_none() => return Err(RpcError::not_found(format!("no session {id}"))),
            _ => {}
        }
    }
    ok(insert_rule(d, new_rule(p.effect, p.matcher, scope, p.expires_in_secs, ctx.actor())))
}

/// The rule an approval scope creates (None for `once`).
pub fn rule_from_approval(d: &Daemon, item: &NeedsYou, scope: &ApprovalScope) -> Option<Rule> {
    let a = &item.approval.as_ref()?.action;
    let session = a.session.clone().or(item.session_id.clone());
    let project = a.project.clone().or(item.project_id.clone());
    let (rule_scope, expires) = match scope {
        ApprovalScope::Once => return None,
        ApprovalScope::Minutes { minutes } => {
            let s = session.map(RuleScope::Session).or(project.map(RuleScope::Project)).unwrap_or(RuleScope::Global);
            (s, Some(*minutes as u64 * 60))
        }
        ApprovalScope::Session => (session.map(RuleScope::Session).or(project.map(RuleScope::Project)).unwrap_or(RuleScope::Global), None),
        // "Always" is remembered for the project it was asked in (global if none).
        ApprovalScope::Always => (project.map(RuleScope::Project).unwrap_or(RuleScope::Global), None),
    };
    let matcher = Matcher { kind: a.kind, pattern: escape_glob(&a.value) };
    let mut rule = new_rule(Effect::Allow, matcher, rule_scope, expires, Actor::human());
    rule.origin = Some(RuleOrigin { needs_you_id: item.id.clone(), approval_scope: scope.clone() });
    Some(insert_rule(d, rule))
}

pub fn rule_request_removal(d: &Daemon, ctx: &Ctx, p: RuleRequestRemovalParams) -> R {
    let rule = d.core().state.rules.iter().find(|r| r.id == p.id).cloned().ok_or_else(|| RpcError::not_found(format!("no rule {}", p.id)))?;
    if let Some(rr) = &rule.removal_request
        && let Some(n) = d.core().state.needs_you.iter().find(|n| n.id == rr.needs_you_id).cloned() {
            return ok(n); // already pending
        }
    let mut item = d.new_needs_you(
        NeedsYouKind::RuleRemoval,
        format!("Remove rule: {} `{}`?", rule.effect.as_str(), rule.matcher.pattern),
        ctx.actor(),
        ctx.session.clone(),
    );
    item.detail = p.reason.clone();
    item.bulk_safe = false;
    let rr = RemovalRequest { requested_by: ctx.actor(), reason: p.reason.clone(), at: time::now_rfc3339(), needs_you_id: item.id.clone() };
    if let Some(r) = d.core().state.rules.iter_mut().find(|r| r.id == p.id) {
        r.removal_request = Some(rr);
    }
    d.emit(kinds::RULE_REMOVAL_REQUESTED, ctx.actor(), None, ctx.session.clone(), json!({ "rule_id": p.id, "reason": p.reason, "needs_you_id": item.id }));
    ok(d.raise_needs_you(item))
}

pub fn remove_rule(d: &Daemon, id: &str, by: Actor) -> Result<Rule, RpcError> {
    let rule = {
        let mut core = d.core();
        let idx = core.state.rules.iter().position(|r| r.id == id).ok_or_else(|| RpcError::not_found(format!("no rule {id}")))?;
        core.state.rules.remove(idx)
    };
    d.mark_dirty();
    d.emit(kinds::RULE_REMOVED, by.clone(), None, None, serde_json::to_value(&rule).unwrap_or_default());
    if let Some(rr) = &rule.removal_request {
        d.close_needs_you(&rr.needs_you_id, json!({ "kind": "done", "reason": "rule removed" }), by);
    }
    Ok(rule)
}

/// `rule.restore` (human only): put a removed rule back under its original id.
pub fn rule_restore(d: &Daemon, ctx: &Ctx, p: RuleRestoreParams) -> R {
    let mut rule = p.rule;
    if !rule.id.starts_with("r_") || rule.matcher.pattern.trim().is_empty() {
        return Err(RpcError::bad_params("rule must be a removed rule object (id r_…, non-empty matcher.pattern)"));
    }
    if is_expired(&rule, time::now_unix()) {
        return Err(RpcError::conflict(format!("rule {} has expired; add a new one with rule.add", rule.id)));
    }
    {
        let core = d.core();
        if core.state.rules.iter().any(|r| r.id == rule.id) {
            return Err(RpcError::conflict(format!("rule {} already exists", rule.id)));
        }
        match &rule.scope {
            RuleScope::Project(id) if core.state.project(id).is_none() => return Err(RpcError::not_found(format!("no project {id}"))),
            RuleScope::Session(id) if core.state.session(id).is_none() => return Err(RpcError::not_found(format!("no session {id}"))),
            _ => {}
        }
    }
    // A pending removal request was answered by the removal itself.
    rule.removal_request = None;
    let (project, session) = match &rule.scope {
        RuleScope::Project(p) => (Some(p.clone()), None),
        RuleScope::Session(s) => (None, Some(s.clone())),
        RuleScope::Global => (None, None),
    };
    d.core().state.rules.push(rule.clone());
    d.mark_dirty();
    d.emit(kinds::RULE_RESTORED, ctx.actor(), project, session, serde_json::to_value(&rule).unwrap_or_default());
    ok(rule)
}

pub fn rule_remove(d: &Daemon, ctx: &Ctx, p: IdParams) -> R {
    remove_rule(d, &p.id, ctx.actor())?;
    ok(OkResult { ok: true })
}

/// Drop expired rules, emitting `rule.expired` for each.
pub fn expire_rules(d: &Daemon) {
    let now = time::now_unix();
    let expired: Vec<Rule> = {
        let mut core = d.core();
        let (gone, keep): (Vec<Rule>, Vec<Rule>) = std::mem::take(&mut core.state.rules).into_iter().partition(|r| is_expired(r, now));
        core.state.rules = keep;
        gone
    };
    for r in expired {
        d.mark_dirty();
        d.emit(kinds::RULE_EXPIRED, Actor::system(), None, None, serde_json::to_value(&r).unwrap_or_default());
    }
}
