//! trigger.* and webhooks.configure handlers. Human-only rules:
//! - Agents create triggers; they start as `needs_secret` and never fire until a human sets the
//!   secret and enables them. An agent-created trigger raises a `secret_needed` item.
//! - Setting a secret is human only. An agent asking raises `secret_needed`; its secret is dropped.
//! - Enabling is human only. An agent asking goes through the deferred-approval mechanism
//!   (or `secret_needed` first when there's no secret yet). Anyone may pause.
//! - An agent changing the action or source of an enabled trigger sends it back to draft.
//! - Agents may remove triggers that aren't enabled; removing an enabled one asks the human.
//!
//! Local triggers (`source: local`, see `local.rs`) have no secret: they start as drafts (or
//! active with `enabled: true`), and agents may enable and pause them, since the human asks an
//! agent for them in the first place ("when this happens, do that").
use super::{Ctx, R, ok};
use crate::daemon::Daemon;
use crate::state::hex_id;
use crate::webhooks::{process, tailscale};
use midna_proto::*;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

fn not_found(id: &str) -> RpcError {
    RpcError::not_found(format!("no trigger {id}; see trigger.list"))
}

fn get(d: &Daemon, id: &str) -> Result<Trigger, RpcError> {
    d.core().state.triggers.iter().find(|t| t.id == id).cloned().ok_or_else(|| not_found(id))
}

pub(crate) fn validate(d: &Daemon, t: &Trigger) -> Result<(), RpcError> {
    let (name, event, action) = (&t.name, &t.event, &t.action);
    if name.trim().is_empty() {
        return Err(RpcError::bad_params("name must not be empty"));
    }
    let local = t.source == TriggerSource::Local;
    if event.trim().is_empty() {
        return Err(RpcError::bad_params(if local {
            "event must not be empty, e.g. agent.prompt_blocked, hook.Stop, stream.slack.*, idle or schedule"
        } else {
            "event must not be empty, e.g. pull_request.opened or pullrequest:created"
        }));
    }
    if action.acts_on_terminal() && !local {
        return Err(RpcError::bad_params(format!("{} acts on a terminal on this Mac, so it needs source local", crate::local::action_name(action))));
    }
    if let Some(to) = action.to() {
        if !local {
            return Err(RpcError::bad_params("to names a terminal on this Mac, so it needs source local"));
        }
        if to.trim().is_empty() {
            return Err(RpcError::bad_params("to must name a terminal (its name or session id), or be left out"));
        }
    }
    // `stream.<name>.<kind>` (streams.publish): such an event belongs to no terminal.
    if local && midna_proto::streams::check_event_glob(event).map_err(RpcError::bad_params)? {
        let f = &t.filter;
        if f.session.is_some() || f.agent.is_some() || (f.project.is_some() && action.to().is_none()) {
            return Err(RpcError::bad_params(
                "stream events belong to no terminal, so filter.session, project and agent never match; filter on the payload with filter.match \
                 (filter.project only goes with to, where it says which project's terminal to look for)",
            ));
        }
        if action.needs_session() {
            return Err(RpcError::bad_params(format!(
                "{} acts on the terminal that fired, and stream events have none: name one with to (e.g. \"to\": \"build-angel\"), or use notify, attention, run_command or start_agent",
                crate::local::action_name(action)
            )));
        }
    }
    if local && event.trim().eq_ignore_ascii_case("idle") && t.filter.idle_minutes.unwrap_or(0) == 0 {
        return Err(RpcError::bad_params("idle triggers need filter.idle_minutes (e.g. 55)"));
    }
    if t.filter.wall_clock && !(local && event.trim().eq_ignore_ascii_case("idle")) {
        return Err(RpcError::bad_params("filter.wall_clock only applies to local triggers with event idle"));
    }
    if t.filter.not_self && !(local && action.to().is_some()) {
        return Err(RpcError::bad_params("filter.not_self only applies to local triggers whose action names a terminal with to"));
    }
    let schedule = local && event.trim().eq_ignore_ascii_case("schedule");
    let f = &t.filter;
    if !schedule && (f.window.is_some() || f.starts_at.is_some() || f.ends_at.is_some() || f.max_runs.is_some() || f.wake) {
        return Err(RpcError::bad_params("filter.window, starts_at, ends_at, max_runs and wake only apply to local triggers with event schedule"));
    }
    match (&t.filter.cron, schedule) {
        (None, true) => return Err(RpcError::bad_params("schedule triggers need filter.cron, e.g. \"0 9 * * mon-fri\" (local time)")),
        (Some(_), false) => return Err(RpcError::bad_params("filter.cron only applies to local triggers with event schedule")),
        (Some(_), true) => {
            cron::Schedule::of(&t.filter, t.fired).map_err(RpcError::bad_params)?;
            let f = &t.filter;
            if action.needs_session() && f.session.is_none() && f.project.is_none() {
                return Err(RpcError::bad_params(format!(
                    "a schedule isn't about a terminal: {} needs filter.session (or filter.project, for every terminal in it), or to (one terminal's name or id)",
                    crate::local::action_name(action)
                )));
            }
            if action.to().is_some() && (f.session.is_some() || f.agent.is_some()) {
                return Err(RpcError::bad_params(
                    "a schedule with to fires once, on that terminal: leave out filter.session and agent (filter.project says which project's terminal to look for)",
                ));
            }
        }
        (None, false) => {}
    }
    match action {
        TriggerAction::StartAgent { project_id, agent, prompt_template, permission_mode, allowed_tools, session } => {
            if prompt_template.trim().is_empty() {
                return Err(RpcError::bad_params("start_agent needs a prompt_template"));
            }
            if session.is_some() && !schedule {
                return Err(RpcError::bad_params("start_agent: session is for local triggers with event schedule"));
            }
            if *agent != AgentKind::Claude && (permission_mode.is_some() || !allowed_tools.is_empty()) {
                return Err(RpcError::bad_params("start_agent: permission_mode and allowed_tools are for Claude"));
            }
            if let Some(m) = permission_mode.as_deref().filter(|m| !PERMISSION_MODES.contains(m)) {
                return Err(RpcError::bad_params(format!("start_agent: permission_mode `{m}` must be one of {}", PERMISSION_MODES.join(", "))));
            }
            if allowed_tools.iter().any(|t| t.trim().is_empty() || t.trim().starts_with('-')) {
                return Err(RpcError::bad_params("start_agent: each allowed_tools entry is a tool name, e.g. mcp__claude_ai_Slack__slack_send_message or Bash(git status)"));
            }
            d.core().state.project(project_id).ok_or_else(|| RpcError::not_found(format!("no project {project_id}")))?;
        }
        TriggerAction::RunCommand { project_id, command, background, headless, timeout_secs } => {
            if command.trim().is_empty() {
                return Err(RpcError::bad_params("run_command needs a command"));
            }
            if *background && *headless {
                return Err(RpcError::bad_params("run_command: background opens a terminal and headless runs without one; pick one"));
            }
            if timeout_secs.is_some() && !*headless {
                return Err(RpcError::bad_params("run_command: timeout_secs is for headless runs (a terminal runs until it exits)"));
            }
            if timeout_secs.is_some_and(|t| t == 0 || t > crate::headless::MAX_TIMEOUT_SECS) {
                return Err(RpcError::bad_params(format!("run_command: timeout_secs must be 1–{}", crate::headless::MAX_TIMEOUT_SECS)));
            }
            d.core().state.project(project_id).ok_or_else(|| RpcError::not_found(format!("no project {project_id}")))?;
        }
        TriggerAction::Attention { message, .. } => {
            if message.trim().is_empty() {
                return Err(RpcError::bad_params("attention needs a message"));
            }
        }
        TriggerAction::SendToSession { steps, .. } => {
            if steps.is_empty() || steps.iter().any(|s| s.text.is_empty() && !s.enter) {
                return Err(RpcError::bad_params("send_to_session needs steps, e.g. [{\"text\":\"/compact\"},{\"text\":\"{{last_prompt}}\"}]"));
            }
        }
        TriggerAction::SetStatus { label, color, base, .. } => {
            if label.trim().is_empty() {
                return Err(RpcError::bad_params("set_status needs a label"));
            }
            if !valid_status_color(color) {
                return Err(RpcError::bad_params(format!("color must be one of {} or #rrggbb", STATUS_COLORS.join(", "))));
            }
            if *base == StatusState::Exited {
                return Err(RpcError::bad_params("base must be idle, working, needs_you, done or failed"));
            }
        }
        TriggerAction::ClearStatus { .. } => {}
        TriggerAction::Notify { title, category, open, id, .. } => {
            if title.trim().is_empty() {
                return Err(RpcError::bad_params("notify needs a title"));
            }
            // Checked now unless a template fills it in at firing time.
            if let Some(u) = open.as_deref().filter(|u| !u.contains("{{") && !midna_proto::notify::valid_open_url(u.trim())) {
                return Err(RpcError::bad_params(format!("notify open `{u}` isn't a URL: give one with a scheme (https://…)")));
            }
            if let Some(i) = id.as_deref().filter(|i| !i.contains("{{") && !midna_proto::notify::valid_id(i.trim())) {
                return Err(RpcError::bad_params(format!("notify id `{i}` must be 1–64 of A-Z a-z 0-9 . _ : -")));
            }
            if let Some(k) = category.as_deref().filter(|k| *k != "from_trigger")
                && d.core().state.notify_kind(k).is_none()
            {
                return Err(RpcError::bad_params(format!("no notification kind `{k}`; add it with notify.kinds.add, or leave category out")));
            }
        }
    }
    Ok(())
}

/// An `@every` schedule without a start counts from now: its first run is one interval away.
fn anchor_every(t: &mut Trigger) {
    if t.source == TriggerSource::Local && t.event.trim().eq_ignore_ascii_case("schedule") {
        cron::anchor_every(&mut t.filter, time::now_unix());
    }
}

fn project_of(t: &Trigger) -> Option<Id> {
    t.action.project_id().cloned()
}

/// Replace the trigger in state and emit `trigger.updated` (never includes the secret).
fn save(d: &Daemon, ctx: &Ctx, t: &Trigger, change: Value) {
    {
        let mut core = d.core();
        if let Some(x) = core.state.triggers.iter_mut().find(|x| x.id == t.id) {
            *x = t.clone();
        }
    }
    d.mark_dirty();
    d.emit(kinds::TRIGGER_UPDATED, ctx.actor(), project_of(t), None, json!({ "trigger": t, "change": change }));
}

/// One open `secret_needed` item per trigger.
fn raise_secret_needed(d: &Daemon, ctx: &Ctx, t: &Trigger) -> NeedsYou {
    if let Some(n) = d.core().state.needs_you.iter().find(|n| n.kind == NeedsYouKind::SecretNeeded && n.trigger_id.as_deref() == Some(&t.id)) {
        return n.clone();
    }
    let mut item = d.new_needs_you(NeedsYouKind::SecretNeeded, format!("Paste the webhook secret for “{}”", t.name), ctx.actor(), ctx.session.clone());
    item.detail = format!(
        "{} {} trigger {} needs its signing secret before it can be enabled. Agents can't see or set secrets. \
         Set it in Triggers, or run `midna triggers set-secret {}` yourself.",
        match t.source {
            TriggerSource::Github => "GitHub",
            TriggerSource::Bitbucket => "Bitbucket",
            TriggerSource::Local => "Local",
        },
        t.event,
        t.id,
        t.id
    );
    item.trigger_id = Some(t.id.clone());
    if item.project_id.is_none() {
        item.project_id = project_of(t);
    }
    d.raise_needs_you(item)
}

fn close_secret_needed(d: &Daemon, ctx: &Ctx, id: &str) {
    let ids: Vec<Id> = d.core().state.needs_you.iter().filter(|n| n.kind == NeedsYouKind::SecretNeeded && n.trigger_id.as_deref() == Some(id)).map(|n| n.id.clone()).collect();
    for n in ids {
        d.close_needs_you(&n, json!({ "kind": "done", "auto": true }), ctx.actor());
    }
}

/// What an agent asking for a trigger with `permission_mode` or `allowed_tools` is told: the
/// human approves it (a needs-you item that runs `method` with `params`) or nothing changes.
fn defer_access(d: &Daemon, ctx: &Ctx, name: &str, method: &str, params: Value) -> RpcError {
    let cli = if method == "trigger.add" { "triggers add".to_string() } else { format!("triggers update {}", params["id"].as_str().unwrap_or("")) };
    let title = format!("Agent asks to set the permission mode or allowed tools of trigger “{name}”");
    super::defer_to_human(d, ctx, &title, &cli, method, &params)
}

pub fn add(d: &Daemon, ctx: &Ctx, p: TriggerAddParams) -> R {
    let local = p.source == TriggerSource::Local;
    // How much the agents it starts may do without asking is the human's call.
    if !ctx.is_human() && p.action.agent_access().is_some() {
        let params = serde_json::to_value(&p).unwrap_or_default();
        return Err(defer_access(d, ctx, p.name.trim(), "trigger.add", params));
    }
    if p.enabled && !local {
        return Err(RpcError::bad_params("only local triggers can be enabled on add; webhook triggers need their secret first"));
    }
    let now = time::now_rfc3339();
    let mut t = Trigger {
        id: format!("t_{}", hex_id(6)),
        name: p.name.trim().to_string(),
        source: p.source,
        event: p.event.trim().to_string(),
        filter: p.filter,
        action: p.action,
        enabled: p.enabled,
        state: match (local, p.enabled) {
            (false, _) => TriggerState::NeedsSecret,
            (true, false) => TriggerState::Draft,
            (true, true) => TriggerState::Active,
        },
        secret_set: false,
        created_by: ctx.actor(),
        created_at: now.clone(),
        last_fired_at: None,
        fired: 0,
        last_fired_summary: None,
        enabled_at: p.enabled.then_some(now),
        secret_set_at: None,
        secret_store: None,
        github_hook_id: p.github_hook_id,
        session_name_template: p.session_name_template.filter(|s| !s.trim().is_empty()),
        cooldown_secs: p.cooldown_secs,
        builtin: None,
    };
    anchor_every(&mut t);
    validate(d, &t)?;
    d.core().state.triggers.push(t.clone());
    d.mark_dirty();
    d.emit(kinds::TRIGGER_ADDED, ctx.actor(), project_of(&t), None, json!({ "trigger": t }));
    if !ctx.is_human() && !local {
        raise_secret_needed(d, ctx, &t);
    }
    ok(t)
}

/// Would `p` change how much the agents `t` starts may do without asking? Agents ask the human
/// for that, and the person driving from another Mac does it at this one.
pub(crate) fn changes_access(t: &Trigger, p: &TriggerUpdateParams) -> bool {
    p.action.as_ref().is_some_and(|a| a.agent_access() != t.action.agent_access())
}

/// Would `p` change the action or source of a webhook trigger that is on or paused? An agent's
/// edit sends it back to draft; the person driving from another Mac makes it at this one.
pub(crate) fn edits_live_webhook(t: &Trigger, p: &TriggerUpdateParams) -> bool {
    let sensitive = p.action.as_ref().is_some_and(|a| *a != t.action) || p.source.is_some_and(|s| s != t.source);
    let source = p.source.unwrap_or(t.source);
    sensitive && source != TriggerSource::Local && matches!(t.state, TriggerState::Active | TriggerState::Paused)
}

pub fn update(d: &Daemon, ctx: &Ctx, p: TriggerUpdateParams) -> R {
    let mut t = get(d, &p.id)?;
    if !ctx.is_human() && changes_access(&t, &p) {
        let params = serde_json::to_value(&p).unwrap_or_default();
        return Err(defer_access(d, ctx, &t.name, "trigger.update", params));
    }
    let back_to_draft = !ctx.is_human() && edits_live_webhook(&t, &p);
    let mut changed = vec![];
    if let Some(n) = p.name {
        t.name = n.trim().to_string();
        changed.push("name");
    }
    if let Some(e) = p.event {
        t.event = e.trim().to_string();
        changed.push("event");
    }
    if let Some(f) = p.filter {
        // A new run limit counts from now.
        if f.max_runs != t.filter.max_runs {
            t.fired = 0;
        }
        t.filter = f;
        anchor_every(&mut t);
        changed.push("filter");
    }
    if let Some(s) = p.source
        && s != t.source
    {
        t.source = s;
        changed.push("source");
    }
    if let Some(a) = p.action
        && a != t.action
    {
        t.action = a;
        changed.push("action");
    }
    if let Some(h) = p.github_hook_id {
        t.github_hook_id = Some(h).filter(|h| *h > 0);
        changed.push("github_hook_id");
    }
    if let Some(n) = p.session_name_template {
        t.session_name_template = Some(n).filter(|s| !s.trim().is_empty());
        changed.push("session_name_template");
    }
    if let Some(c) = p.cooldown_secs {
        t.cooldown_secs = Some(c);
        changed.push("cooldown_secs");
    }
    validate(d, &t)?;
    if back_to_draft {
        t.enabled = false;
        t.state = TriggerState::Draft;
    }
    save(d, ctx, &t, json!({ "fields": changed, "back_to_draft": back_to_draft }));
    ok(t)
}

pub fn set_enabled(d: &Daemon, ctx: &Ctx, p: TriggerSetEnabledParams) -> R {
    let mut t = get(d, &p.id)?;
    if p.enabled && t.source == TriggerSource::Local {
        if t.enabled && t.state == TriggerState::Active {
            return ok(t);
        }
        t.enabled = true;
        t.state = TriggerState::Active;
        t.enabled_at = Some(time::now_rfc3339());
    } else if p.enabled {
        if !t.secret_set {
            if !ctx.is_human() {
                let item = raise_secret_needed(d, ctx, &t);
                return Err(RpcError::human_only(format!(
                    "{} has no secret yet; asked the human to paste it (needs-you {}). Enabling is human only too.",
                    t.id, item.id
                ))
                .with_data(json!({ "needs_you_id": item.id })));
            }
            return Err(RpcError::conflict(format!("set the secret first: midna triggers set-secret {}", t.id)));
        }
        if !ctx.is_human() {
            let params = json!({ "id": t.id, "enabled": true });
            return Err(super::defer_to_human(d, ctx, &format!("Agent asks to enable trigger “{}”", t.name), &format!("triggers enable {}", t.id), "trigger.set_enabled", &params));
        }
        if t.enabled && t.state == TriggerState::Active {
            return ok(t);
        }
        t.enabled = true;
        t.state = TriggerState::Active;
        t.enabled_at = Some(time::now_rfc3339());
    } else {
        if !t.enabled && t.state != TriggerState::Active {
            return ok(t);
        }
        t.enabled = false;
        t.state = TriggerState::Paused;
    }
    save(d, ctx, &t, json!({ "enabled": t.enabled }));
    ok(t)
}

/// An agent called trigger.set_secret: raise `secret_needed`, drop whatever it sent.
pub fn secret_refusal(d: &Daemon, ctx: &Ctx, params: &Value) -> RpcError {
    let id = params.get("id").and_then(Value::as_str).unwrap_or("");
    match get(d, id) {
        Ok(t) => {
            let item = raise_secret_needed(d, ctx, &t);
            RpcError::human_only(format!("webhook secrets are human only; asked the human to paste it (needs-you {}). The secret you sent was discarded.", item.id))
                .with_data(json!({ "needs_you_id": item.id }))
        }
        Err(e) => e,
    }
}

pub fn set_secret(d: &Daemon, ctx: &Ctx, p: TriggerSetSecretParams) -> R {
    if !ctx.is_human() {
        return Err(secret_refusal(d, ctx, &json!({ "id": p.id })));
    }
    let mut t = get(d, &p.id)?;
    if p.secret.is_empty() {
        return Err(RpcError::bad_params("secret must not be empty"));
    }
    d.webhooks.secrets.set(&t.id, p.secret.as_bytes()).map_err(|e| RpcError::internal(format!("could not store the secret: {e}")))?;
    t.secret_set = true;
    t.secret_set_at = Some(time::now_rfc3339());
    t.secret_store = Some(d.cfg.webhooks.secrets.as_str().into());
    if t.state == TriggerState::NeedsSecret {
        t.state = TriggerState::Draft;
    }
    save(d, ctx, &t, json!({ "secret_set": true }));
    close_secret_needed(d, ctx, &t.id);
    ok(OkResult { ok: true })
}

pub fn remove(d: &Daemon, ctx: &Ctx, p: IdParams) -> R {
    let t = get(d, &p.id)?;
    if !ctx.is_human() && t.enabled {
        let params = json!({ "id": t.id });
        return Err(super::defer_to_human(d, ctx, &format!("Agent asks to remove enabled trigger “{}”", t.name), &format!("triggers remove {}", t.id), "trigger.remove", &params));
    }
    d.core().state.triggers.retain(|x| x.id != t.id);
    d.webhooks.secrets.delete(&t.id);
    close_secret_needed(d, ctx, &t.id);
    d.mark_dirty();
    d.emit(kinds::TRIGGER_REMOVED, ctx.actor(), project_of(&t), None, json!({ "id": t.id, "name": t.name }));
    ok(OkResult { ok: true })
}

pub fn replay(d: &Arc<Daemon>, _ctx: &Ctx, p: TriggerReplayParams) -> R {
    let (delivery, stream, event) = (p.delivery_id, p.stream, p.event);
    match (delivery, stream, event) {
        (Some(id), None, None) => {
            let original = d.core().state.deliveries.iter().find(|x| x.id == id).cloned().ok_or_else(|| RpcError::not_found(format!("no delivery {id}")))?;
            ok(process::replay(d, &original)?)
        }
        (None, Some(stream), Some(event)) => ok(crate::local::replay_stream(d, &recorded_stream_event(d, &stream, event.trim())?)),
        (None, Some(stream), None) => Err(RpcError::bad_params(format!("name the event to replay: its seq or id (midna stream events {stream} lists them)"))),
        _ => Err(RpcError::bad_params("replay either a delivery_id or a stream event (stream + event), not both")),
    }
}

/// A stream event `event` (its seq, or the publisher's id) among the stream's last
/// [`streams::EVENTS_KEPT`] publishes, read back from the event log.
fn recorded_stream_event(d: &Daemon, stream: &str, event: &str) -> Result<Event, RpcError> {
    use midna_proto::streams::{EVENTS_KEPT, valid_name};
    if !valid_name(stream) {
        return Err(RpcError::bad_params(format!("stream `{stream}` must be 1–32 of a-z 0-9 - (e.g. slack)")));
    }
    let (rows, _) = d.streams.events(stream, EVENTS_KEPT);
    let seq = event.parse::<u64>().ok();
    // Newest first, and a logged one before a dropped one with the same id.
    let mut hits: Vec<&StreamPublishRecord> = rows.iter().filter(|r| (seq.is_some() && r.seq == seq) || r.id.as_deref() == Some(event)).collect();
    hits.sort_by_key(|r| r.seq.is_none());
    let Some(r) = hits.first() else {
        return Err(RpcError::not_found(format!("stream `{stream}` has no event {event} among its last {EVENTS_KEPT} publishes (midna stream events {stream} lists them)")));
    };
    let Some(seq) = r.seq else {
        return Err(RpcError::conflict(format!("stream `{stream}` dropped event {event} ({}), so there is nothing recorded to replay", r.verdict.as_str())));
    };
    let kind = format!("stream.{stream}.{}", r.kind);
    let filter = EventFilter { kinds: Some(vec![kind]), ..Default::default() };
    d.log
        .list(seq - 1, usize::MAX, &filter)
        .into_iter()
        .find(|e| e.seq == seq && e.stream.is_some())
        .ok_or_else(|| RpcError::not_found(format!("event {seq} of stream `{stream}` is no longer in the event log")))
}

pub fn test(d: &Daemon, p: TriggerTestParams) -> R {
    let t = get(d, &p.trigger_id)?;
    if let Some(st) = &p.stream {
        if t.source != TriggerSource::Local {
            return Err(RpcError::bad_params(format!("trigger {} is a webhook trigger; only local triggers see stream events", t.id)));
        }
        // Checked as streams.publish would, so a test can't pass with an event no publish could send.
        let publish = midna_proto::streams::StreamsPublishParams { stream: st.name.clone(), kind: st.kind.clone(), id: st.id.clone(), data: p.payload.clone() };
        let envelope = midna_proto::streams::check_publish(&publish).map_err(RpcError::bad_params)?;
        let event = envelope.event_kind();
        if let Some(e) = p.event.as_deref().filter(|e| !e.eq_ignore_ascii_case(&event)) {
            return Err(RpcError::bad_params(format!("event `{e}` isn't the stream event `{event}`: leave event out with stream")));
        }
        let data = if p.payload.is_null() { json!({}) } else { p.payload };
        return ok(crate::local::dry_run(d, &t, &event, p.session.as_deref(), &data, Some(&envelope)));
    }
    if t.source == TriggerSource::Local {
        let event = p.event.unwrap_or_else(|| t.event.clone());
        return ok(crate::local::dry_run(d, &t, &event, p.session.as_deref(), &p.payload, None));
    }
    let event = p.event.unwrap_or_else(|| match t.source {
        TriggerSource::Github => t.event.split('.').next().unwrap_or(&t.event).to_string(),
        _ => t.event.clone(),
    });
    ok(process::dry_run(&t, &event, p.payload))
}

pub fn configure(d: &Arc<Daemon>, ctx: &Ctx, p: WebhooksConfigureParams) -> R {
    let path = p.path.trim().to_string();
    if !["tailscale_funnel", "self_relay", "midna_relay", "off"].contains(&path.as_str()) {
        return Err(RpcError::bad_params("path must be tailscale_funnel, self_relay, midna_relay or off"));
    }
    let set = |key: &str, value: Value| super::settings::set(d, ctx, SettingSetParams { key: key.into(), value });
    let old_path = d.core().state.setting_str("webhooks.path");
    if let Some(port) = p.port {
        set("webhooks.port", json!(port))?;
    }
    if let Some(u) = p.relay_url {
        set("webhooks.relay_url", json!(u))?;
    }
    set("webhooks.path", json!(path))?;
    crate::webhooks::sync_receiver(d);
    let port = u16::try_from(d.core().state.setting_i64("webhooks.port")).unwrap_or(7787);
    let cli = tailscale::find_cli(d.cfg.webhooks.tailscale_bin.as_deref());
    let mut enable_url = None;
    let (ok_, message): (bool, String) = match path.as_str() {
        "tailscale_funnel" => match &cli {
            None => (false, "Tailscale CLI not found. Install Tailscale.app and sign in, then try again.".into()),
            Some(cli) => {
                let (code, out) = tailscale::run_capture(&tailscale::funnel_on_args(cli, port), Duration::from_secs(20));
                enable_url = tailscale::find_enable_url(&out);
                if let Some(u) = &enable_url {
                    (false, format!("Funnel isn't enabled for your tailnet yet. Open {u}, enable it, then run this again."))
                } else if code == Some(0) {
                    (true, format!("Tailscale Funnel now forwards :{} to 127.0.0.1:{port}", tailscale::FUNNEL_PORT))
                } else if code.is_none() {
                    (false, "tailscale funnel timed out".into())
                } else {
                    let tail: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty()).take(4).collect();
                    (false, format!("tailscale funnel failed: {}", tail.join(" / ")))
                }
            }
        },
        other => {
            // Leaving Funnel: turn it off only if it points at this receiver.
            if old_path == "tailscale_funnel"
                && let Some(cli) = &cli
                && crate::webhooks::tailscale_status(d, port, true).funnel_on
            {
                let _ = tailscale::run_capture(&tailscale::funnel_off_args(cli), Duration::from_secs(20));
            }
            match other {
                "off" => (true, "Webhooks are off".into()),
                _ => (false, "Saved, but the relay client isn't available yet; deliveries won't arrive through it".into()),
            }
        }
    };
    let status = crate::webhooks::status(d, true);
    ok(WebhooksConfigureResult { ok: ok_, message, enable_url, status })
}
