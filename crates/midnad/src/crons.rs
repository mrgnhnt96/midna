//! Claude's session crons, taken over by midna (setting `agents.claude.crons_to_midna`).
//!
//! A cron Claude makes (`CronCreate`) lives in its process: it's gone when the session ends,
//! and a recurring one expires after 7 days. midna makes it a local `schedule` trigger instead,
//! a `start_agent` that follows the terminal (`TriggerAction::StartAgent::session`): the prompt
//! is typed into that terminal while it runs, and once it's gone a new Claude agent in its
//! project gets it.
//!
//! The PreToolUse hook reaches midnad twice: `agent.hook` with the tool's input, then
//! `policy.request` with only `CronCreate` to decide. The first parks the input here; the
//! second (unless a rule denies the call) adds the trigger and denies Claude's own cron, telling
//! it the trigger's id. A `CronDelete` of such an id removes the trigger the same way.
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::*;
use serde_json::{Value, json};

/// A cron call parked between `agent.hook` and `policy.request`.
#[derive(Clone, Debug, PartialEq)]
pub enum Parked {
    Create(Value),
    Delete(Id),
}

/// `agent.hook` PreToolUse: park a `CronCreate`'s input, or a `CronDelete` of one of ours.
pub fn on_pre_tool(d: &Daemon, sid: &str, agent: AgentKind, payload: &Value) {
    if agent != AgentKind::Claude || !d.core().state.setting_bool("agents.claude.crons_to_midna") {
        return;
    }
    let parked = match payload.get("tool_name").and_then(Value::as_str) {
        Some("CronCreate") => Parked::Create(payload.get("tool_input").cloned().unwrap_or(Value::Null)),
        Some("CronDelete") => {
            let Some(id) = payload.pointer("/tool_input/id").and_then(Value::as_str) else { return };
            if !d.core().state.triggers.iter().any(|t| t.id == id && follows(t, sid)) {
                return;
            }
            Parked::Delete(id.to_string())
        }
        _ => return,
    };
    d.core().agents.entry(sid.to_string()).or_default().crons.push(parked);
}

/// Made from a Claude cron, and now following terminal `sid`.
fn follows(t: &Trigger, sid: &str) -> bool {
    t.source == TriggerSource::Local && matches!(&t.action, TriggerAction::StartAgent { session: Some(s), .. } if s == sid)
}

/// `policy.request` for a `CronCreate` / `CronDelete` the hook parked: do it midna's way and
/// return why Claude's call is denied. None leaves the call to Claude. `allowed` is false when a
/// rule denies the call: the parked call is dropped.
pub fn decide(d: &Daemon, ctx: &Ctx, a: &PolicyAction, allowed: bool) -> Option<String> {
    if a.kind != ActionKind::Tool || !matches!(a.value.as_str(), "CronCreate" | "CronDelete") {
        return None;
    }
    let sid = a.session.as_deref()?;
    let parked = {
        let mut core = d.core();
        let crons = &mut core.agents.get_mut(sid)?.crons;
        let i = crons.iter().position(|p| matches!((p, a.value.as_str()), (Parked::Create(_), "CronCreate") | (Parked::Delete(_), "CronDelete")))?;
        crons.remove(i)
    };
    if !allowed {
        return None;
    }
    match parked {
        Parked::Create(input) => create(d, ctx, sid, &input),
        Parked::Delete(id) => {
            // The terminal's own cron: removing it needs no human (an agent removing an enabled
            // trigger otherwise asks).
            crate::rpc::trigger::remove(d, &Ctx::internal_system(), serde_json::from_value(json!({ "id": id })).ok()?).ok()?;
            Some(format!("{id} was a midna schedule (this terminal's cron); it's removed. Nothing else to do."))
        }
    }
}

fn create(d: &Daemon, ctx: &Ctx, sid: &str, input: &Value) -> Option<String> {
    let cron = input.get("cron").and_then(Value::as_str).filter(|c| !c.trim().is_empty())?.trim().to_string();
    let prompt = input.get("prompt").and_then(Value::as_str).filter(|p| !p.trim().is_empty())?.to_string();
    let recurring = input.get("recurring").and_then(Value::as_bool).unwrap_or(true);
    let (project, term) = d.core().state.session(sid).map(|s| (s.project_id.clone(), s.name.clone()))?;
    let params = json!({
        "name": format!("Claude cron: {}", clip(&prompt, 48)),
        "source": "local",
        "event": "schedule",
        "filter": { "cron": cron, "max_runs": (!recurring).then_some(1) },
        "action": { "kind": "start_agent", "project_id": project, "agent": "claude", "prompt_template": prompt, "session": sid },
        "session_name_template": term,
        "enabled": true,
    });
    // A cron midna can't read (or any other refusal) stays Claude's.
    let t: Trigger = match crate::rpc::trigger::add(d, ctx, serde_json::from_value(params).ok()?) {
        Ok(v) => serde_json::from_value(v).ok()?,
        Err(_) => return None,
    };
    let when = if recurring { format!("`{cron}`") } else { format!("once, at `{cron}`") };
    Some(format!(
        "midna scheduled this instead of a session cron (setting agents.claude.crons_to_midna): trigger {id}, {when} local time. \
         It types the prompt into this terminal when it's ready, and once this terminal is gone it opens a new Claude agent in the project. \
         It's done: don't retry CronCreate or schedule it another way, and tell the human it's midna trigger {id}. \
         CronList won't show it; `midna triggers show {id}` does. To cancel it, CronDelete {id} (or `midna triggers remove {id}`).",
        id = t.id
    ))
}

fn clip(s: &str, n: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= n { line.to_string() } else { format!("{}…", line.chars().take(n).collect::<String>().trim_end()) }
}
