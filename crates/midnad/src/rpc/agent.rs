//! agent.hook: hook events from `midna hook claude|codex` drive status, turns, prompt counts
//! and cost, using the state machine proven in spikes/agent-status.
use super::{Ctx, R, ok};
use crate::agent_state::transition;
use crate::daemon::Daemon;
use midna_proto::*;
use serde_json::{Value, json};
use std::sync::Arc;

pub fn hook(d: &Arc<Daemon>, ctx: &Ctx, p: AgentHookParams) -> R {
    let sid = p.session.clone().or(ctx.session.clone()).ok_or_else(|| RpcError::bad_params("no session: run inside a midna terminal or pass session"))?;
    let (cur, project) = {
        let core = d.core();
        let s = core.state.session(&sid).ok_or_else(|| RpcError::not_found(format!("no session {sid}")))?;
        (s.status.state, s.project_id.clone())
    };
    let actor = Actor { kind: ActorKind::Agent, session: Some(sid.clone()), name: Some(p.agent.as_str().into()) };
    let ev = p.event.as_str();
    let payload = &p.payload;
    if !(p.agent == AgentKind::Codex && is_codex_title_turn(payload)) {
        track(d, &sid, &project, p.agent, ev, payload);
        crate::links::after_hook(d, &sid, p.agent, ev, payload);
        crate::local::hook(d, &sid, p.agent, ev, payload);
    }
    match ev {
        "statusline" => {
            record_cost(d, &sid, &project, &actor, payload);
            if p.agent == AgentKind::Claude {
                crate::usage::observe(d, &sid, &project, payload);
            }
            return ok(AgentHookResult { ok: true, status: Some(cur) });
        }
        "UserPromptSubmit" => {
            // The whole prompt: local triggers resend it (`{{last_prompt}}`).
            let prompt = payload.get("prompt").and_then(Value::as_str).unwrap_or("");
            // The conversation lets prompt fast travel tell prompts before a /clear apart.
            let conversation = payload.get("session_id").and_then(Value::as_str);
            d.emit(kinds::AGENT_PROMPT_SUBMITTED, actor.clone(), Some(project.clone()), Some(sid.clone()), json!({ "agent": p.agent, "prompt": prompt, "conversation": conversation }));
            crate::auto_name::on_prompt(d, &sid, prompt);
            d.clear_agent_blocked(&sid);
            start_turn(d, &sid, &project, &actor);
        }
        "Stop" | "StopFailure" => {
            end_turn(d, &sid, &project, &actor, ev, payload.get("last_assistant_message").and_then(Value::as_str));
            if ev == "StopFailure" {
                let s = |k: &str| payload.get(k).and_then(Value::as_str).unwrap_or("");
                crate::resume::on_failure(d, &sid, s("error"), s("error_details"));
            }
        }
        "agent-turn-complete" => {
            // Codex 0.160.0 also notifies for its internal title-generation turn (a separate
            // thread); that is not the user's work.
            if is_codex_title_turn(payload) {
                return ok(AgentHookResult { ok: true, status: Some(cur) });
            }
            // Codex notify arrives once per finished turn; it carries the inputs of that turn.
            let msgs: Vec<String> = payload.get("input-messages").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default();
            for m in &msgs {
                d.emit(kinds::AGENT_PROMPT_SUBMITTED, actor.clone(), Some(project.clone()), Some(sid.clone()), json!({ "agent": p.agent, "via": "notify", "prompt": m, "conversation": payload.get("thread-id") }));
            }
            if let Some(m) = msgs.first() {
                crate::auto_name::on_prompt(d, &sid, m);
            }
            // The title spinner usually opened this turn already. If the title also ended it
            // moments ago, this notify belongs to that turn: don't open another.
            let title_just_ended = d.core().agents.get(&sid).and_then(|a| a.title_ended_at).is_some_and(|t| t.elapsed().as_secs() < 15);
            if !title_just_ended {
                start_turn(d, &sid, &project, &actor);
            }
            end_turn(d, &sid, &project, &actor, ev, payload.get("last-assistant-message").and_then(Value::as_str));
        }
        _ => {}
    }
    let mut status = cur;
    if let Some((next, reason)) = transition(cur, ev, payload) {
        if cur == StatusState::NeedsYou && next != StatusState::NeedsYou {
            d.clear_session_needs_you(&sid, NeedsYouKind::PermissionPrompt);
        }
        if next == StatusState::NeedsYou {
            raise_prompt(d, &sid, &actor, &reason, crate::agent_state::question(payload));
        }
        d.set_status(&sid, next, Some(reason), None, actor.clone());
        status = next;
    } else if ev == "PermissionRequest" && cur == StatusState::NeedsYou {
        // Claude's generic `Notification` ("Claude needs your permission") can open the prompt
        // first; this hook names what it actually asks.
        retitle_prompt(d, &sid, &crate::agent_state::prompt_label(payload), crate::agent_state::question(payload));
    }
    ok(AgentHookResult { ok: true, status: Some(status) })
}

/// Fold the hook into the session's `AgentInfo` (conversation, version, background work,
/// subagents, wakeups); emits `session.agent` when that changed.
fn track(d: &Daemon, sid: &str, project: &str, agent: AgentKind, ev: &str, payload: &Value) {
    let now = time::now_rfc3339();
    let changed = {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return };
        let info = s.agent_info.get_or_insert_with(Default::default);
        crate::agent_work::apply_hook(info, agent, ev, payload, &now).then(|| info.clone())
    };
    if let Some(info) = changed {
        d.mark_dirty();
        d.emit(kinds::SESSION_AGENT, Actor::system(), Some(project.into()), Some(sid.into()), serde_json::to_value(&info).unwrap_or_default());
    }
}

fn raise_prompt(d: &Daemon, sid: &str, actor: &Actor, reason: &str, question: Option<NeedsYouQuestion>) {
    let open = d.core().state.needs_you.iter().any(|n| n.kind == NeedsYouKind::PermissionPrompt && n.session_id.as_deref() == Some(sid));
    if open {
        return;
    }
    let title = if reason.is_empty() { "Agent is waiting for permission".to_string() } else { reason.to_string() };
    let mut item = d.new_needs_you(NeedsYouKind::PermissionPrompt, title, actor.clone(), Some(sid.to_string()));
    item.screen_excerpt = d.rt(sid).and_then(|rt| rt.read(true)).map(|(l, _, _)| super::session::tail_nonempty(l, 12));
    item.question = question;
    d.raise_needs_you(item);
    d.core().agents.entry(sid.to_string()).or_default().prompt_raised_at = Some(std::time::Instant::now());
}

fn retitle_prompt(d: &Daemon, sid: &str, title: &str, question: Option<NeedsYouQuestion>) {
    let item = {
        let mut core = d.core();
        let Some(n) = core.state.needs_you.iter_mut().find(|n| n.kind == NeedsYouKind::PermissionPrompt && n.session_id.as_deref() == Some(sid)) else { return };
        if (n.title == title && n.question == question) || crate::trust::is_trust_item(n) {
            return;
        }
        n.title = title.to_string();
        n.question = question;
        n.clone()
    };
    d.mark_dirty();
    d.emit(kinds::NEEDS_YOU_UPDATED, Actor::system(), item.project_id.clone(), item.session_id.clone(), serde_json::to_value(&item).unwrap_or_default());
}

pub fn start_turn(d: &Daemon, sid: &str, project: &str, actor: &Actor) {
    let started = {
        let mut core = d.core();
        let a = core.agents.entry(sid.to_string()).or_default();
        !std::mem::replace(&mut a.in_turn, true)
    };
    if started {
        d.emit(kinds::AGENT_TURN_STARTED, actor.clone(), Some(project.into()), Some(sid.into()), json!({}));
    }
}

/// `message`: the start of the agent's last reply, when the hook carries it (notifications).
fn end_turn(d: &Daemon, sid: &str, project: &str, actor: &Actor, reason: &str, message: Option<&str>) -> bool {
    let ended = d.core().agents.get_mut(sid).map(|a| std::mem::replace(&mut a.in_turn, false)).unwrap_or(false);
    if ended {
        let mut data = json!({ "reason": reason });
        if let Some(m) = message.map(|m| m.chars().take(300).collect::<String>()).filter(|m| !m.trim().is_empty()) {
            data["message"] = json!(m);
        }
        d.emit(kinds::AGENT_TURN_ENDED, actor.clone(), Some(project.into()), Some(sid.into()), data);
    }
    ended
}

/// End an open turn without a Stop hook (interrupt, process exit). True if one was open.
pub fn end_turn_if_open(d: &Daemon, sid: &str, reason: &str) -> bool {
    let project = d.core().state.session(sid).map(|s| s.project_id.clone()).unwrap_or_default();
    end_turn(d, sid, &project, &Actor::system(), reason, None)
}

/// Codex's background "generate a task title" turn: its reply is a `{"title": …}` object.
pub fn is_codex_title_turn(p: &Value) -> bool {
    let reply = p.get("last-assistant-message").and_then(Value::as_str).unwrap_or("");
    let titled = serde_json::from_str::<Value>(reply).ok().and_then(|v| v.as_object().map(|o| o.len() == 1 && o.contains_key("title"))).unwrap_or(false);
    let asked = p.pointer("/input-messages/0").and_then(Value::as_str).is_some_and(|m| m.starts_with("Generate a concise, single-line task title"));
    titled || asked
}

/// Claude's status line payload carries the session's running total cost.
fn record_cost(d: &Daemon, sid: &str, project: &str, actor: &Actor, payload: &Value) {
    let Some(total) = payload.pointer("/cost/total_cost_usd").and_then(Value::as_f64) else { return };
    let delta = {
        let mut core = d.core();
        let a = core.agents.entry(sid.to_string()).or_default();
        // A lower total means a new agent process (restart): count it from zero.
        let delta = if total < a.last_cost { total } else { total - a.last_cost };
        a.last_cost = total;
        delta
    };
    if delta > 1e-9 {
        let model = payload.pointer("/model/display_name").cloned().unwrap_or(Value::Null);
        d.emit(kinds::AGENT_COST, actor.clone(), Some(project.into()), Some(sid.into()), json!({ "total_usd": total, "delta_usd": delta, "model": model }));
    }
}
