//! What the person driving this Mac from another of my Macs can't do over the drive link
//! (DECISIONS.md, "Nearby: driving another of my Macs", Local-only). Over a link they are the
//! human here (`Actor::peer`), except for these: trust between Macs, secrets (storing one is fine),
//! the daemon and what's installed, removing protections or widening what agents and the outside
//! may do, this Mac's own app, and approving anything that would run one of them. Each is refused with
//! "Do this at <this Mac>", never turned into a needs-you (its answer would come back to the
//! same window).
use crate::daemon::Daemon;
use crate::rpc::{Ctx, trigger};
use midna_proto::types::{NeedsYou, NeedsYouKind, TriggerSource};
use midna_proto::{RpcError, StreamsAddParams, StreamsUpdateParams, TriggerAddParams, TriggerUpdateParams};
use serde_json::{Value, json};

/// Human-only methods the person driving may still call: they approve, close and pause like the
/// human here. `drive.open` and `drive.close` answer only this Mac's own app anyway, and
/// `session.send_image` only the person driving. A monitor grant lets a terminal approve once
/// what the person driving could approve for always anyway, so they give and end it too.
const OVER_A_LINK: &[&str] = &[
    "session.send_image",
    "session.close_hold",
    "session.pause",
    "session.stop_processes",
    "session.monitor_grant",
    "session.monitor_revoke",
    "notify.respond",
    "script.click",
    "project.remove",
    "drive.open",
    "drive.close",
];

/// Local-only methods that agents may call (or ask for) here, so the catalog doesn't mark them
/// human only. `secret.set` isn't one: the person driving may store a secret here, as the human
/// here would type it into the Add a secret sheet; using, writing out and removing one stay here.
const ALSO_LOCAL: &[&str] = &["peers.pair", "secret.write", "secret.exec_env", "daemon.restart"];

/// Would `method(params)` be local-only? Every human-only method is, unless [`OVER_A_LINK`]
/// lets it through, so a new one is local-only until its section in DECISIONS.md says otherwise.
pub fn method(d: &Daemon, method: &str, params: &Value) -> bool {
    if ALSO_LOCAL.contains(&method) {
        return true;
    }
    match method {
        "settings.set" | "settings.reset" => setting(d, method, params),
        // Turning on a webhook trigger lets the outside start work here; a local one agents may
        // turn on themselves.
        "trigger.set_enabled" => {
            let on = params.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let id = params.get("id").and_then(Value::as_str).unwrap_or_default();
            on && d.core().state.triggers.iter().find(|t| t.id == id).is_some_and(|t| t.source != TriggerSource::Local)
        }
        // How much the agents a trigger starts may do without asking (agents ask the human for
        // it), and the action or source of a webhook trigger that is on or paused (an agent's
        // edit sends it back to draft).
        "trigger.add" => serde_json::from_value::<TriggerAddParams>(params.clone()).is_ok_and(|p| p.action.agent_access().is_some()),
        "trigger.update" => {
            let Ok(p) = serde_json::from_value::<TriggerUpdateParams>(params.clone()) else { return false };
            let t = d.core().state.triggers.iter().find(|t| t.id == p.id).cloned();
            t.is_some_and(|t| trigger::changes_access(&t, &p) || trigger::edits_live_webhook(&t, &p))
        }
        // A stream runs its command in the background with this Mac's secrets in its environment:
        // adding one with secrets, changing one that has or gets them, and starting one with them
        // that awaits approval (what an agent's would ask the human for). Agents resume an
        // approved one themselves.
        "streams.add" => serde_json::from_value::<StreamsAddParams>(params.clone()).is_ok_and(|p| !p.secrets.is_empty()),
        "streams.update" => {
            let Ok(p) = serde_json::from_value::<StreamsUpdateParams>(params.clone()) else { return false };
            let old = d.core().state.streams.iter().find(|s| s.name == p.name).map(|s| s.secrets.clone());
            old.is_some_and(|old| !p.secrets.unwrap_or(old).is_empty())
        }
        "streams.set_enabled" => {
            let on = params.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            on && d.core().state.streams.iter().find(|s| s.name == name).is_some_and(|s| s.needs_approval && !s.secrets.is_empty())
        }
        _ => midna_proto::method(method).is_some_and(|s| s.human_only) && !OVER_A_LINK.contains(&method),
    }
}

/// A setting change is local-only for a human-only setting, every `nearby.*` setting, and a
/// script path that agents would have to ask for.
fn setting(d: &Daemon, method: &str, params: &Value) -> bool {
    let key = params.get("key").and_then(Value::as_str).unwrap_or_default();
    if key.starts_with("nearby.") {
        return true;
    }
    let Some(spec) = d.core().state.setting_spec(key) else { return false };
    if spec.human_only {
        return true;
    }
    let value = params.get("value").and_then(|v| spec.coerce(v).ok());
    method == "settings.set" && value.is_some_and(|v| crate::rpc::settings::custom_script(spec, &v, &d.core().state.setting(key)))
}

/// Would approving `item` run something local-only: an agent's deferred call that is (a pairing's
/// code, an incoming share, a human-only setting or method), or a rule's removal?
pub fn approval(d: &Daemon, item: &NeedsYou) -> bool {
    if item.kind == NeedsYouKind::RuleRemoval {
        return true;
    }
    let deferred = d.core().state.deferred.get(&item.id).cloned();
    deferred.is_some_and(|def| method(d, &def.method, &def.params))
}

/// The refusal for a call the person driving can't make over a link: None for anyone else, and
/// for what they may do.
pub fn refusal(d: &Daemon, ctx: &Ctx, method_name: &str, params: &Value) -> Option<RpcError> {
    ctx.as_actor.as_ref()?.drive_peer()?;
    method(d, method_name, params).then(|| refuse(d, &format!("{method_name} can't be done from another Mac")))
}

/// `Do this at <this Mac>`, error `2`, with why after it.
pub fn refuse(d: &Daemon, why: &str) -> RpcError {
    let mac = super::display_name(d);
    RpcError::human_only(format!("Do this at {mac}: {why}")).with_data(json!({ "local_only": true, "mac": mac }))
}
