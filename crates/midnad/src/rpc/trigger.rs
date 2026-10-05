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

fn validate(d: &Daemon, t: &Trigger) -> Result<(), RpcError> {
    let (name, event, action) = (&t.name, &t.event, &t.action);
    if name.trim().is_empty() {
        return Err(RpcError::bad_params("name must not be empty"));
    }
    let local = t.source == TriggerSource::Local;
    if event.trim().is_empty() {
        return Err(RpcError::bad_params(if local {
            "event must not be empty, e.g. agent.prompt_blocked, hook.Stop, idle or schedule"
        } else {
            "event must not be empty, e.g. pull_request.opened or pullrequest:created"
        }));
    }
    if action.needs_session() && !local {
        return Err(RpcError::bad_params(format!("{} acts on the terminal that fired, so it needs source local", crate::local::action_name(action))));
    }
    if local && event.trim().eq_ignore_ascii_case("idle") && t.filter.idle_minutes.unwrap_or(0) == 0 {
        return Err(RpcError::bad_params("idle triggers need filter.idle_minutes (e.g. 55)"));
    }
    let schedule = local && event.trim().eq_ignore_ascii_case("schedule");
    match (&t.filter.cron, schedule) {
        (None, true) => return Err(RpcError::bad_params("schedule triggers need filter.cron, e.g. \"0 9 * * mon-fri\" (local time)")),
        (Some(_), false) => return Err(RpcError::bad_params("filter.cron only applies to local triggers with event schedule")),
        (Some(c), true) => {
            cron::Cron::parse(c).map_err(|e| RpcError::bad_params(format!("filter.cron: {e}")))?;
            let f = &t.filter;
            if action.needs_session() && f.session.is_none() && f.project.is_none() {
                return Err(RpcError::bad_params(format!(
                    "a schedule isn't about a terminal: {} needs filter.session (or filter.project, for every terminal in it)",
                    crate::local::action_name(action)
                )));
            }
        }
        (None, false) => {}
    }
    match action {
        TriggerAction::StartAgent { project_id, prompt_template, .. } => {
            if prompt_template.trim().is_empty() {
                return Err(RpcError::bad_params("start_agent needs a prompt_template"));
            }
            d.core().state.project(project_id).ok_or_else(|| RpcError::not_found(format!("no project {project_id}")))?;
        }
        TriggerAction::RunCommand { project_id, command } => {
            if command.trim().is_empty() {
                return Err(RpcError::bad_params("run_command needs a command"));
            }
            d.core().state.project(project_id).ok_or_else(|| RpcError::not_found(format!("no project {project_id}")))?;
        }
        TriggerAction::Attention { message } => {
            if message.trim().is_empty() {
                return Err(RpcError::bad_params("attention needs a message"));
            }
        }
        TriggerAction::SendToSession { steps } => {
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
        TriggerAction::ClearStatus {} => {}
        TriggerAction::Notify { title, .. } => {
            if title.trim().is_empty() {
                return Err(RpcError::bad_params("notify needs a title"));
            }
        }
    }
    Ok(())
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

pub fn add(d: &Daemon, ctx: &Ctx, p: TriggerAddParams) -> R {
    let local = p.source == TriggerSource::Local;
    if p.enabled && !local {
        return Err(RpcError::bad_params("only local triggers can be enabled on add; webhook triggers need their secret first"));
    }
    let now = time::now_rfc3339();
    let t = Trigger {
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
    validate(d, &t)?;
    d.core().state.triggers.push(t.clone());
    d.mark_dirty();
    d.emit(kinds::TRIGGER_ADDED, ctx.actor(), project_of(&t), None, json!({ "trigger": t }));
    if !ctx.is_human() && !local {
        raise_secret_needed(d, ctx, &t);
    }
    ok(t)
}

pub fn update(d: &Daemon, ctx: &Ctx, p: TriggerUpdateParams) -> R {
    let mut t = get(d, &p.id)?;
    let mut changed = vec![];
    let mut sensitive = false;
    if let Some(n) = p.name {
        t.name = n.trim().to_string();
        changed.push("name");
    }
    if let Some(e) = p.event {
        t.event = e.trim().to_string();
        changed.push("event");
    }
    if let Some(f) = p.filter {
        t.filter = f;
        changed.push("filter");
    }
    if let Some(s) = p.source
        && s != t.source
    {
        t.source = s;
        changed.push("source");
        sensitive = true;
    }
    if let Some(a) = p.action
        && a != t.action
    {
        t.action = a;
        changed.push("action");
        sensitive = true;
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
    let mut back_to_draft = false;
    if sensitive && !ctx.is_human() && t.source != TriggerSource::Local && matches!(t.state, TriggerState::Active | TriggerState::Paused) {
        t.enabled = false;
        t.state = TriggerState::Draft;
        back_to_draft = true;
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
    let original = d.core().state.deliveries.iter().find(|x| x.id == p.delivery_id).cloned().ok_or_else(|| RpcError::not_found(format!("no delivery {}", p.delivery_id)))?;
    ok(process::replay(d, &original)?)
}

pub fn test(d: &Daemon, p: TriggerTestParams) -> R {
    let t = get(d, &p.trigger_id)?;
    if t.source == TriggerSource::Local {
        let event = p.event.unwrap_or_else(|| t.event.clone());
        return ok(crate::local::dry_run(d, &t, &event, p.session.as_deref(), &p.payload));
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
