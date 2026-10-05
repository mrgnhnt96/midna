//! RPC dispatch: caller roles, catalog checks (unknown / stub / human-only), audit events,
//! then the per-group handlers in the submodules.
pub mod agent;
pub mod events;
pub mod insights;
pub mod links;
pub mod misc;
pub mod needs_you;
pub mod notify;
pub mod permissions;
pub mod policy;
pub mod project;
pub mod queue;
pub mod reset;
pub mod script;
pub mod secret;
pub mod session;
pub mod settings;
pub mod trigger;
pub mod ui;
pub mod updates;
pub mod window;

use crate::conn::OutTx;
use crate::daemon::Daemon;
use midna_proto::error::{HUMAN_ONLY, NOT_IMPLEMENTED, REFUSED, UNKNOWN_METHOD};
use midna_proto::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Human,
    Agent,
}

pub struct Ctx {
    pub role: Role,
    /// The caller's midna session (from MIDNA_SESSION), if any.
    pub session: Option<Id>,
    pub conn_id: u64,
    pub out: OutTx,
    pub pid: Option<i32>,
    /// Acting identity when not the connection's (e.g. a webhook trigger).
    pub as_actor: Option<Actor>,
}

impl Ctx {
    /// Role = the connection's role, downgraded to agent if params carry `caller.session`
    /// or `caller.role: "agent"`. Nothing upgrades to human.
    pub fn new(base: Role, params: &Value, conn_id: u64, out: OutTx, pid: Option<i32>) -> Ctx {
        let caller: Caller = params.get("caller").cloned().and_then(|c| serde_json::from_value(c).ok()).unwrap_or_default();
        let downgrade = caller.session.is_some() || caller.role.as_deref() == Some("agent");
        let role = if downgrade { Role::Agent } else { base };
        Ctx { role, session: caller.session, conn_id, out, pid, as_actor: None }
    }

    /// A human context for executing approved deferred actions (no connection).
    pub fn internal_human() -> Ctx {
        Ctx { role: Role::Human, session: None, conn_id: 0, out: OutTx::detached(), pid: None, as_actor: None }
    }

    /// A webhook trigger acting on its own (agent-level privileges, actor kind `trigger`).
    pub fn internal_trigger(actor: Actor) -> Ctx {
        Ctx { role: Role::Agent, session: None, conn_id: 0, out: OutTx::detached(), pid: None, as_actor: Some(actor) }
    }

    pub fn is_human(&self) -> bool {
        self.role == Role::Human
    }

    pub fn actor(&self) -> Actor {
        if let Some(a) = &self.as_actor {
            return a.clone();
        }
        match self.role {
            Role::Human => Actor::human(),
            Role::Agent => Actor::agent(self.session.clone()),
        }
    }
}

/// Check the caller's `caller.session` claim against where the calling process really runs
/// (`MIDNA_SESSION` is just an environment variable, so any process can claim any terminal):
/// - inside one of our terminals: the claim must name that terminal, else the call is refused
///   (an agent may not speak for, e.g. approve the prompts of, another terminal);
/// - a process midnad itself started outside any terminal (header/row scripts, in-process
///   tests): the claim is trusted;
/// - anywhere else (another terminal app, a process that escaped its terminal): the claim is
///   dropped and the caller is an agent with no terminal.
pub fn bind_caller(d: &Daemon, ctx: &mut Ctx) -> Result<(), RpcError> {
    let Some(claim) = ctx.session.clone() else { return Ok(()) };
    let Some(pid) = ctx.pid else {
        ctx.session = None;
        return Ok(());
    };
    match crate::peer::terminal_of(pid, &d.terminal_pids()) {
        Some(actual) if actual == claim => Ok(()),
        Some(actual) => Err(RpcError::refused(format!(
            "caller.session {claim} does not match the terminal this process runs in ({actual}); a terminal can only act as itself"
        ))),
        None if crate::peer::ancestry(pid).contains(&(std::process::id() as i32)) => Ok(()),
        None => {
            ctx.session = None;
            Ok(())
        }
    }
}

pub type R = Result<Value, RpcError>;

pub fn parse<T: DeserializeOwned>(v: Value) -> Result<T, RpcError> {
    serde_json::from_value(v).map_err(|e| RpcError::bad_params(e.to_string()))
}

pub fn ok<T: serde::Serialize>(v: T) -> R {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

pub fn call(d: &Arc<Daemon>, ctx: &Ctx, method: &str, mut params: Value) -> R {
    let Some(spec) = midna_proto::method(method) else {
        return Err(RpcError::new(UNKNOWN_METHOD, format!("unknown method `{method}`; call rpc.discover for the catalog")));
    };
    if let Some(o) = params.as_object_mut() {
        o.remove("caller");
    }
    if params.is_null() {
        params = json!({});
    }
    let res = if spec.mutating && d.upgrading.load(std::sync::atomic::Ordering::SeqCst) && !method.starts_with("daemon.") {
        // Anything that changes state now would land after the snapshot and be lost.
        Err(RpcError::conflict("midnad is upgrading/restarting; reconnect and retry in a moment"))
    } else if spec.stub {
        Err(RpcError::new(NOT_IMPLEMENTED, format!("{method} is not implemented yet (later phase)")))
    } else if spec.human_only && !ctx.is_human() {
        Err(human_only_refusal(d, ctx, method, &params))
    } else {
        dispatch(d, ctx, method, params.clone())
    };
    if spec.mutating && !skip_audit(method, &params) {
        audit(d, ctx, method, &params, &res);
    }
    res
}

fn dispatch(d: &Arc<Daemon>, ctx: &Ctx, method: &str, p: Value) -> R {
    match method {
        "rpc.discover" => Ok(midna_proto::openrpc()),
        "daemon.info" => misc::daemon_info(d, ctx),
        "daemon.upgrade" => misc::daemon_upgrade(d, parse(p)?),
        "daemon.restart" => misc::daemon_restart(d),
        "daemon.stop" => misc::daemon_stop(d),
        "daemon.reset" => reset::reset(d, ctx, parse(p)?),
        "ui.commands.list" => ui::list(d),
        "ui.commands.add" => ui::add(d, ctx, parse(p)?),
        "ui.commands.remove" => ui::remove(d, ctx, parse(p)?),
        "updates.status" => updates::get(d),
        "updates.check" => updates::command(d, ctx, "check"),
        "updates.install" => updates::command(d, ctx, "install"),
        "updates.report" => updates::report(d, ctx, parse(p)?),
        "permissions.status" => permissions::status(d),
        "project.list" => project::list(d),
        "project.add" => project::add(d, ctx, parse(p)?),
        "project.discover" => project::discover(d),
        "project.update" => project::update(d, ctx, parse(p)?),
        "project.remove" => project::remove(d, ctx, parse(p)?),
        "session.list" => session::list(d, parse(p)?),
        "session.get" => session::get(d, parse(p)?),
        "session.open" => session::open(d, ctx, parse(p)?),
        "session.close" => session::close(d, ctx, parse(p)?),
        "session.rename" => session::rename(d, ctx, parse(p)?),
        "session.set_background" => session::set_background(d, ctx, parse(p)?),
        "session.input" => session::input(d, ctx, parse(p)?),
        "session.read" => session::read(d, parse(p)?),
        "session.resize" => session::resize(d, parse(p)?),
        "session.key" => session::key(d, ctx, parse(p)?),
        "session.scroll" => session::scroll(d, parse(p)?),
        "session.selection" => session::selection(d, parse(p)?),
        "session.select_all" => session::select_all(d, parse(p)?),
        "session.clear" => session::clear(d, ctx, parse(p)?),
        "session.link_at" => session::link_at(d, parse(p)?),
        "session.find" => session::find(d, parse(p)?),
        "session.prompts" => session::prompts(d, parse(p)?),
        "session.jump_prompt" => session::jump_prompt(d, ctx, parse(p)?),
        "session.restart" => session::restart(d, ctx, parse(p)?),
        "session.restart_cancel" => session::restart_cancel(d, ctx, parse(p)?),
        "session.update_decline" => session::update_decline(d, parse(p)?),
        "session.adopt" => ok(crate::adopt::adopt(d, ctx, parse(p)?)?),
        "session.adopt_end" => ok(crate::adopt::adopt_end(d, ctx, parse(p)?)?),
        "session.processes" => session::processes(d, parse(p)?),
        "session.subagent_log" => session::subagent_log(d, parse(p)?),
        "session.focus" => window::focus(d, ctx, parse(p)?),
        "links.list" => links::list(d, ctx, parse(p)?),
        "links.pin" => links::pin(d, ctx, parse(p)?),
        "links.add" => links::add(d, ctx, parse(p)?),
        "queue.list" => queue::list(d, ctx, parse(p)?),
        "queue.add" => queue::add(d, ctx, parse(p)?),
        "queue.update" => queue::update(d, ctx, parse(p)?),
        "queue.remove" => queue::remove(d, ctx, parse(p)?),
        "queue.clear" => queue::clear(d, ctx, parse(p)?),
        "queue.move" => queue::move_to(d, ctx, parse(p)?),
        "queue.send_now" => queue::send_now(d, ctx, parse(p)?),
        "queue.pause" => queue::pause(d, ctx, parse(p)?),
        "notify.list" => notify::list(d, ctx, parse(p)?),
        "notify.set" => notify::set(d, ctx, parse(p)?),
        "notify.send" => notify::send(d, ctx, parse(p)?),
        "notify.media" => notify::media(d, parse(p)?),
        "notify.import" => notify::import(d, ctx, parse(p)?),
        "notify.remove" => notify::remove(d, ctx, parse(p)?),
        "notify.test" => notify::test(d, ctx, parse(p)?),
        "notify.play" => notify::play(d, ctx, parse(p)?),
        "events.list" => events::list(d, parse(p)?),
        "events.subscribe" => events::subscribe(d, ctx, parse(p)?),
        "needs_you.list" => needs_you::list(d, parse(p)?),
        "needs_you.raise" => needs_you::raise(d, ctx, parse(p)?),
        "needs_you.resolve" => needs_you::resolve(d, ctx, parse(p)?),
        "policy.check" => policy::check(d, ctx, parse(p)?),
        "policy.request" => policy::request(d, ctx, parse(p)?),
        "rule.list" => policy::rule_list(d),
        "rule.add" => policy::rule_add(d, ctx, parse(p)?),
        "rule.request_removal" => policy::rule_request_removal(d, ctx, parse(p)?),
        "rule.remove" => policy::rule_remove(d, ctx, parse(p)?),
        "rule.restore" => policy::rule_restore(d, ctx, parse(p)?),
        "trigger.list" => ok(d.core().state.triggers.clone()),
        "trigger.deliveries" => misc::deliveries(d, parse(p)?),
        "trigger.add" => trigger::add(d, ctx, parse(p)?),
        "trigger.update" => trigger::update(d, ctx, parse(p)?),
        "trigger.set_enabled" => trigger::set_enabled(d, ctx, parse(p)?),
        "trigger.set_secret" => trigger::set_secret(d, ctx, parse(p)?),
        "trigger.remove" => trigger::remove(d, ctx, parse(p)?),
        "trigger.replay" => trigger::replay(d, ctx, parse(p)?),
        "trigger.test" => trigger::test(d, parse(p)?),
        "secret.list" => secret::list(d, ctx, parse(p)?),
        "secret.set" => secret::set(d, ctx, parse(p)?),
        "secret.replace" => secret::replace(d, ctx, parse(p)?),
        "secret.remove" => secret::remove(d, ctx, parse(p)?),
        "secret.write" => secret::write(d, ctx, parse(p)?),
        "secret.exec_env" => secret::exec_env(d, ctx, parse(p)?),
        "hooks.status" => ok(crate::global_hooks::status(d)),
        "hooks.preview" => ok(crate::global_hooks::preview(d, parse(p)?)),
        "hooks.install" | "hooks.uninstall" => {
            let p: HooksTargetParams = parse(p)?;
            let r = crate::global_hooks::apply(d, &p.agents, method == "hooks.uninstall");
            crate::global_hooks::poll(d);
            ok(r?)
        }
        "webhooks.status" => ok(crate::webhooks::status(d, false)),
        "webhooks.configure" => trigger::configure(d, ctx, parse(p)?),
        "webhooks.reconcile" => ok(crate::webhooks::reconcile::run(d, "manual")),
        "settings.list" => settings::list(d),
        "settings.get" => settings::get(d, parse(p)?),
        "settings.set" => settings::set(d, ctx, parse(p)?),
        "settings.reset" => settings::reset(d, ctx, parse(p)?),
        "insights.summary" => insights::summary(d, parse(p)?),
        "insights.activity" => insights::activity(d, parse(p)?),
        "insights.series" => insights::series(d, parse(p)?),
        "window.list" => window::list(d),
        "window.command" => window::command(d, ctx, parse(p)?),
        "agent.hook" => agent::hook(d, ctx, parse(p)?),
        "script.run" => script::run(d, parse(p)?),
        _ => Err(RpcError::new(NOT_IMPLEMENTED, format!("{method} has no handler yet"))),
    }
}

/// Per-call audit would drown the log for these high-frequency calls; they emit their own events.
fn skip_audit(method: &str, params: &Value) -> bool {
    match method {
        "agent.hook" => params.get("event").and_then(Value::as_str) == Some("statusline"),
        "session.resize" | "session.scroll" | "session.selection" | "session.select_all" | "session.link_at" | "session.find" | "session.jump_prompt" => true,
        _ => false,
    }
}

fn summarize(method: &str, params: &Value) -> String {
    let mut p = params.clone();
    if let Some(o) = p.as_object_mut() {
        for k in ["secret", "payload", "value"] {
            if o.contains_key(k) && (k != "value" || method.starts_with("secret.")) {
                o.insert(k.into(), json!("…"));
            }
        }
        if method == "agent.hook" {
            o.remove("payload");
        }
    }
    let s = p.to_string();
    if s.chars().count() > 240 { format!("{}…", s.chars().take(240).collect::<String>()) } else { s }
}

fn audit(d: &Daemon, ctx: &Ctx, method: &str, params: &Value, res: &R) {
    let outcome = match res {
        Ok(_) => "ok",
        Err(e) if e.code == REFUSED || e.code == HUMAN_ONLY => "denied",
        Err(_) => "error",
    };
    let mut data = json!({ "method": method, "params_summary": summarize(method, params), "outcome": outcome });
    if let Err(e) = res {
        data["error"] = json!(e.message);
    }
    let session = params.get("id").or(params.get("session_id")).and_then(Value::as_str).map(str::to_string);
    let session = session.filter(|s| d.core().state.session(s).is_some());
    d.emit(kinds::AUDIT, ctx.actor(), None, session, data);
}

/// An agent asked for a human-only method. `rule.remove` points at `rule.request_removal`;
/// anything else becomes a needs-you confirmation that runs the call if the human approves.
fn human_only_refusal(d: &Daemon, ctx: &Ctx, method: &str, params: &Value) -> RpcError {
    if method == "trigger.set_secret" {
        // Never defer this one: the deferred call would store the secret the agent sent.
        return trigger::secret_refusal(d, ctx, params);
    }
    if method == "secret.replace" {
        // Only an approval runs it (see secret::set).
        return RpcError::human_only("secret.replace runs when the human approves a replacement; call secret.set (`midna secret save`)");
    }
    if method == "updates.report" {
        // Only the GUI reports updater state; a deferred approval would make no sense.
        return RpcError::human_only("updates.report is for the midna app; agents read updates.status");
    }
    if method == "rule.remove" {
        return RpcError::human_only("rule.remove is human only; use rule.request_removal to ask the human");
    }
    let cli = format!("{method} {}", summarize(method, params));
    defer_to_human(d, ctx, &format!("Agent asks to run {method}"), &cli, method, params)
}

/// Raise an approval needs-you that, if approved, runs `method(params)` as the human.
/// The item shows the exact call (never truncated) plus what it acts on, and remembers a
/// fingerprint of that target ([`deferred_guard`]); approval runs exactly those params, and
/// only if the target is unchanged.
pub fn defer_to_human(d: &Daemon, ctx: &Ctx, title: &str, cli: &str, method: &str, params: &Value) -> RpcError {
    let mut item = d.new_needs_you(NeedsYouKind::Approval, title.to_string(), ctx.actor(), ctx.session.clone());
    let (guard, context) = deferred_guard(d, method, params);
    let mut shown = params.clone();
    if let Some(o) = shown.as_object_mut() {
        for k in ["secret", "value"] {
            if o.contains_key(k) {
                o.insert(k.into(), json!("…"));
            }
        }
    }
    item.detail = format!("Human-only action requested by an agent: {cli}\nApproving runs exactly: {method} {shown}");
    if let Some(c) = context {
        item.detail.push('\n');
        item.detail.push_str(&c);
    }
    item.approval = Some(ApprovalRequest {
        action: PolicyAction { kind: ActionKind::Cli, value: cli.to_string(), session: ctx.session.clone(), project: item.project_id.clone() },
        matched_rule: None,
    });
    d.core().state.deferred.insert(item.id.clone(), crate::state::Deferred { method: method.into(), params: params.clone(), guard });
    let item = d.raise_needs_you(item);
    RpcError::human_only(format!("{method} is human only; asked the human to confirm (needs-you {})", item.id))
        .with_data(json!({ "needs_you_id": item.id }))
}

/// (fingerprint, human-readable context) of what a deferred call acts on.
pub fn deferred_guard(d: &Daemon, method: &str, params: &Value) -> (Option<String>, Option<String>) {
    use sha2::{Digest, Sha256};
    let hex = |b: &[u8]| crate::webhooks::sig::hex_encode(&Sha256::digest(b));
    match method {
        "trigger.set_enabled" | "trigger.remove" | "trigger.update" => {
            let id = params.get("id").and_then(Value::as_str).unwrap_or("");
            let t = d.core().state.triggers.iter().find(|t| t.id == id).cloned();
            match t {
                Some(t) => {
                    let def = json!({ "source": t.source, "event": t.event, "filter": t.filter, "action": t.action, "name": t.session_name_template });
                    let shown = format!("Trigger “{}”: on {} {} {} → {}", t.name, process_source(&t), t.event, json!(t.filter), json!(t.action));
                    (Some(hex(def.to_string().as_bytes())), Some(shown))
                }
                None => (Some("missing".into()), None),
            }
        }
        "secret.replace" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let project = params.get("project_id").and_then(Value::as_str);
            let s = d.core().state.secrets.iter().find(|s| s.name == name && s.project_id.as_deref() == project).cloned();
            match s {
                Some(s) => {
                    let when = s.updated_at.clone().unwrap_or(s.created_at.clone());
                    let shown = format!("Secret {name} ({}), stored {when}. The value isn't shown; nothing changes unless you approve.", s.label.as_deref().unwrap_or("secret"));
                    (Some(hex(format!("{}|{when}", s.id).as_bytes())), Some(shown))
                }
                None => (Some("missing".into()), None),
            }
        }
        "daemon.upgrade" => {
            let path = params.get("binary_path").and_then(Value::as_str).unwrap_or("");
            match std::fs::read(path) {
                Ok(b) => {
                    let h = hex(&b);
                    let shown = format!("Binary {path} (sha256 {h})");
                    (Some(h), Some(shown))
                }
                Err(_) => (Some("missing".into()), None),
            }
        }
        _ => (None, None),
    }
}

fn process_source(t: &Trigger) -> &'static str {
    crate::webhooks::process::source_name(t.source)
}

/// Run an approved deferred call, refusing if its target changed since the human was asked.
pub fn run_deferred(d: &Arc<Daemon>, def: &crate::state::Deferred) -> R {
    if def.guard.is_some() && deferred_guard(d, &def.method, &def.params).0 != def.guard {
        return Err(RpcError::conflict(format!(
            "what this approval would act on changed after you were asked ({}); deny it, the agent can ask again",
            def.method
        )));
    }
    call(d, &Ctx::internal_human(), &def.method, def.params.clone())
}

/// Agents act on behalf of their session. Returns the session's project when known.
pub fn session_project(d: &Daemon, sid: Option<&str>) -> Option<Id> {
    sid.and_then(|s| d.core().state.session(s).map(|s| s.project_id.clone()))
}
