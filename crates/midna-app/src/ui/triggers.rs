//! Triggers screen (docs/design/Triggers-A.dc.html): the delivery-path strip
//! (`webhooks.status`, Change path = `webhooks.configure`, human only), a list with
//! switches and drafts pinned under "Waiting on you", and a detail pane (When / Only if /
//! Then, the prompt template, the secret status with a human-only paste field, and recent
//! deliveries with Replay). Secrets are never displayed or logged.
//!
//! While midnad still answers trigger/webhook methods with error 5 ("not implemented"),
//! the screen says so and rechecks every 20s.
mod schedule;

use super::rules::{Names, ask};
use super::screen_kit::{self as kit, KeyOutcome, LineInput};
use crate::app::{MainWindow, Overlay, refresh};
use crate::backend::{Backend, BackendEvent, ConnState};
use crate::icons::Icon;
use crate::model::{Actor, parse_list, parse_rfc3339};
use crate::theme::Theme;
use midna_proto::kinds;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

// ------------------------------------------------------------------ lenient wire types

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct FilterItem {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub action: Option<String>,
    pub label: Option<String>,
    /// Local: only this terminal / project / agent.
    pub session: Option<String>,
    pub project: Option<String>,
    pub agent: Option<String>,
    /// Local `idle`: minutes without a turn.
    pub idle_minutes: Option<u32>,
    /// Local `schedule`: a cron expression in local time.
    pub cron: Option<String>,
    /// Local `schedule`: only between these local times of day (`HH:MM`).
    pub window: Option<midna_proto::TimeWindow>,
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    /// Local `schedule`: stop after this many firings.
    pub max_runs: Option<u64>,
    /// Local: dotted path into the hook payload / event data -> glob.
    #[serde(rename = "match")]
    pub fields: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct TriggerItem {
    pub id: String,
    pub name: String,
    pub source: String,
    pub event: String,
    pub filter: FilterItem,
    /// Tagged by `kind`: start_agent{project_id, agent, prompt_template} | run_command{project_id, command} | attention{message}
    /// | send_to_session{steps} | set_status{label, color, icon, base, clear_on} | clear_status{}.
    pub action: Value,
    pub enabled: bool,
    pub state: String,
    pub secret_set: bool,
    pub created_by: Actor,
    pub created_at: String,
    pub last_fired_at: Option<String>,
    pub fired: u64,
    pub last_fired_summary: Option<String>,
    pub enabled_at: Option<String>,
    pub secret_set_at: Option<String>,
    pub secret_store: Option<String>,
    pub github_hook_id: Option<u64>,
    pub session_name_template: Option<String>,
    /// Local: seconds before it may fire again for the same terminal (daemon default 60).
    pub cooldown_secs: Option<u64>,
    /// Set on triggers midna ships (e.g. `prompt_blocked_status`).
    pub builtin: Option<String>,
}

/// A local trigger's `set_status` action, read leniently.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct SetStatusItem {
    pub label: String,
    pub color: String,
    pub icon: Option<String>,
    pub base: String,
    pub clear_on: String,
}

/// Quote a CLI argument for display (single quotes when the shell would mangle it).
fn sh_quote(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c)) {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// Words for a status state (`needs_you` -> "needs you").
fn state_words(s: &str) -> String {
    s.replace('_', " ")
}

/// Where a trigger stands for the human.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pending {
    /// Draft without a secret: paste it.
    Secret,
    /// Secret set, never enabled: switch it on.
    Enable,
}

impl TriggerItem {
    /// Drafts (never enabled by a human) are pinned under "Waiting on you".
    pub fn pending(&self) -> Option<Pending> {
        if self.enabled || self.state == "active" || self.state == "paused" {
            return None;
        }
        if self.is_local() {
            // Local triggers have no secret; a draft only needs switching on.
            return Some(Pending::Enable);
        }
        if !self.secret_set || self.state == "needs_secret" { Some(Pending::Secret) } else { Some(Pending::Enable) }
    }

    fn s(&self, key: &str) -> String {
        self.action.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
    }

    fn action_kind(&self) -> String {
        self.s("kind")
    }

    fn project_id(&self) -> Option<String> {
        Some(self.s("project_id")).filter(|s| !s.is_empty())
    }

    pub fn is_local(&self) -> bool {
        self.source == "local"
    }

    fn source_label(&self) -> &'static str {
        match self.source.as_str() {
            "bitbucket" => "Bitbucket",
            "local" => "Local",
            _ => "GitHub",
        }
    }

    /// Source icon for local triggers (this Mac's terminals).
    fn source_icon(&self) -> Option<Icon> {
        self.is_local().then_some(Icon::Shell)
    }

    fn icon(&self) -> Icon {
        match (self.action_kind().as_str(), self.s("agent").as_str()) {
            ("start_agent", "codex") => Icon::Codex,
            ("start_agent", _) => Icon::Claude,
            ("run_command", _) => Icon::Shell,
            ("send_to_session", _) => Icon::Keyboard,
            ("set_status", _) => Icon::Triggers,
            ("clear_status", _) => Icon::Cross,
            _ => Icon::Bell,
        }
    }

    /// `send_to_session` steps' text, in order.
    fn steps(&self) -> Vec<String> {
        self.action.get("steps").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|st| st.get("text").and_then(|t| t.as_str()).map(String::from)).collect()).unwrap_or_default()
    }

    fn set_status(&self) -> Option<SetStatusItem> {
        (self.action_kind() == "set_status").then(|| serde_json::from_value(self.action.clone()).unwrap_or_default())
    }

    /// "Start Claude in midna", "Run a command in api", "Raise attention",
    /// `Send "/compact" → "{{last_prompt}}"`, "Set status Prompt blocked", "Clear status".
    fn action_label(&self, n: &Names) -> String {
        let proj = self.project_id().map(|p| format!(" in {}", n.project(&p))).unwrap_or_default();
        match self.action_kind().as_str() {
            "start_agent" => format!("Start {}{proj}", if self.s("agent") == "codex" { "Codex" } else { "Claude" }),
            "run_command" => format!("Run a command{proj}"),
            "attention" => "Raise attention".into(),
            "send_to_session" => {
                let steps = self.steps();
                if steps.is_empty() { "Send nothing".into() } else { format!("Send {}", steps.iter().map(|s| format!("“{}”", s.replace('\n', " "))).collect::<Vec<_>>().join(" → ")) }
            }
            "set_status" => format!("Set status “{}”", self.set_status().unwrap_or_default().label),
            "clear_status" => "Clear status".into(),
            "notify" => format!("Notify “{}”", self.s("title")),
            k => k.replace('_', " "),
        }
    }

    /// The template/command/message shown in the Then box.
    fn template(&self) -> String {
        match self.action_kind().as_str() {
            "start_agent" => self.s("prompt_template"),
            "run_command" => self.s("command"),
            "send_to_session" => {
                let steps = self.steps();
                if steps.len() < 2 { steps.join("") } else { steps.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n") }
            }
            "set_status" | "clear_status" => String::new(),
            "notify" => self.s("body"),
            _ => self.s("message"),
        }
    }

    fn repo(&self) -> String {
        self.filter.repo.clone().unwrap_or_else(|| "any repo".into())
    }

    /// Where a trigger listens: the repo, or for local triggers the terminals it watches
    /// ("any terminal", "api", "Claude terminals in zonai").
    fn scope(&self, n: &Names) -> String {
        if !self.is_local() {
            return self.repo();
        }
        let f = &self.filter;
        if let Some(sid) = &f.session {
            return n.term(sid);
        }
        if f.cron.is_some() && f.project.is_none() && f.agent.is_none() {
            return "no terminal".into();
        }
        let agent = f.agent.as_deref().map(|a| if a == "codex" { "Codex " } else { "Claude " }).unwrap_or("");
        match &f.project {
            Some(p) => format!("{agent}terminals in {}", n.project(p)),
            None if agent.is_empty() => "any terminal".into(),
            None => format!("any {agent}terminal"),
        }
    }

    /// The Only-if line, built from the structured filter (local session/project/agent are in
    /// the When line's scope instead).
    fn filter_text(&self) -> Option<String> {
        let f = &self.filter;
        let mut parts: Vec<String> =
            [("branch", &f.branch), ("action", &f.action), ("label", &f.label)].iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("{k} == \"{v}\""))).collect();
        if let Some(m) = f.idle_minutes {
            parts.push(format!("idle {m} min"));
        }
        if let Some(c) = &f.cron {
            parts.push(format!("cron \"{c}\"{}", self.next_run().map(|r| format!(" · next {r}")).unwrap_or_default()));
        }
        parts.extend(f.fields.iter().map(|(k, v)| format!("{k} ~ \"{v}\"")));
        (!parts.is_empty()).then(|| parts.join(" && "))
    }

    /// A local schedule's full rule (cron, window, start, end, run limit).
    pub fn schedule(&self) -> Option<midna_proto::cron::Schedule> {
        self.filter.cron.as_ref()?;
        let f: midna_proto::TriggerFilter = serde_json::from_value(serde_json::to_value(&self.filter).ok()?).ok()?;
        midna_proto::cron::Schedule::of(&f, self.fired).ok()
    }

    /// `Every 5 min, weekdays, 1 PM–5 PM · until Oct 10`.
    pub fn schedule_words(&self) -> Option<String> {
        Some(self.schedule()?.describe(self.filter.cron.as_deref()?))
    }

    /// Out of runs or past its end: it won't fire again.
    pub fn ended(&self) -> bool {
        self.schedule().is_some_and(|s| s.ended(midna_proto::time::now_unix()))
    }

    /// A schedule's next run (`Tue Oct 6 09:00`, local time).
    fn next_run(&self) -> Option<String> {
        self.schedule()?.upcoming(midna_proto::time::now_unix(), 1).first().copied().map(midna_proto::cron::local_label)
    }

    /// The `midna triggers add` command that makes this local trigger.
    fn cli(&self) -> String {
        let mut a: Vec<String> = vec![format!("midna triggers add --source local --name {}", sh_quote(&self.name)), format!("--event {}", sh_quote(&self.event))];
        let f = &self.filter;
        if let Some(x) = &f.session {
            a.push(format!("--session {}", sh_quote(x)));
        }
        if let Some(x) = &f.project {
            a.push(format!("--in-project {}", sh_quote(x)));
        }
        if let Some(x) = &f.agent {
            a.push(format!("--for-agent {}", sh_quote(x)));
        }
        if let Some(m) = f.idle_minutes {
            a.push(format!("--idle-for {m}m"));
        }
        if let Some(c) = &f.cron {
            a.push(format!("--cron {}", sh_quote(c)));
        }
        if let Some(w) = &f.window {
            a.push(format!("--between {}-{}", w.from, w.until));
        }
        for (flag, at) in [("starts", &f.starts_at), ("ends", &f.ends_at)] {
            if let Some(t) = at.as_deref().and_then(midna_proto::time::parse_rfc3339) {
                a.push(format!("--{flag} {}", sh_quote(&midna_proto::time::format_local(t))));
            }
        }
        if let Some(n) = f.max_runs {
            a.push(format!("--max-runs {n}"));
        }
        for (k, v) in &f.fields {
            a.push(format!("--match {}", sh_quote(&format!("{k}={v}"))));
        }
        match self.action_kind().as_str() {
            "send_to_session" => a.extend(self.steps().iter().map(|s| format!("--send {}", sh_quote(s)))),
            "set_status" => {
                let st = self.set_status().unwrap_or_default();
                a.push(format!("--set-status {}", sh_quote(&st.label)));
                a.push(format!("--color {}", sh_quote(&st.color)));
                if let Some(i) = &st.icon {
                    a.push(format!("--icon {}", sh_quote(i)));
                }
                if !st.base.is_empty() {
                    a.push(format!("--base {}", st.base));
                }
                if !st.clear_on.is_empty() && st.clear_on != "prompt" {
                    a.push(format!("--clear-on {}", st.clear_on));
                }
            }
            "clear_status" => a.push("--clear-status".into()),
            "notify" => {
                a.push(format!("--notify {}", sh_quote(&self.s("title"))));
                if !self.s("body").is_empty() {
                    a.push(format!("--notify-body {}", sh_quote(&self.s("body"))));
                }
                if self.action.get("sound").and_then(|v| v.as_bool()) == Some(false) {
                    a.push("--silent".into());
                }
            }
            _ => a.push(format!("--action-json {}", sh_quote(&self.action.to_string()))),
        }
        if let Some(c) = self.cooldown_secs {
            a.push(format!("--cooldown {c}s"));
        }
        if self.enabled {
            a.push("--enable".into());
        }
        a.join(" ")
    }

    fn last_text(&self) -> String {
        match &self.last_fired_at {
            None => "Never fired".into(),
            Some(ts) => format!("Fired {}{}", kit::ago(Some(ts)), self.last_fired_summary.as_ref().map(|s| format!(" · {s}")).unwrap_or_default()),
        }
    }

    fn last_short(&self) -> String {
        if self.ended() {
            return "ended".into();
        }
        match &self.last_fired_at {
            None => "never".into(),
            Some(ts) => kit::ago(Some(ts)),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct DeliveryItem {
    pub id: String,
    pub source: String,
    pub event: String,
    pub delivery_guid: String,
    pub received_at: String,
    pub verdict: String,
    pub trigger_id: Option<String>,
    pub session_started: Option<String>,
    pub summary: String,
    pub action: Option<String>,
    pub repo: Option<String>,
    pub subject: Option<String>,
    pub http_status: Option<u16>,
    pub eval: Vec<String>,
    pub triggers_fired: Vec<String>,
    pub sessions_started: Vec<String>,
    pub recovered: bool,
    pub replay_of: Option<String>,
}

impl DeliveryItem {
    fn concerns(&self, trigger: &str) -> bool {
        self.trigger_id.as_deref() == Some(trigger) || self.triggers_fired.iter().any(|t| t == trigger)
    }

    fn event_label(&self) -> String {
        match &self.action {
            Some(a) if !a.is_empty() && !self.event.contains('.') && !self.event.contains(':') => {
                format!("{}.{a}", self.event)
            }
            _ => self.event.clone(),
        }
    }

    fn verdict_meta(&self, t: &Theme) -> (&'static str, Hsla) {
        match self.verdict.as_str() {
            "verified" => ("Verified", t.ok),
            "bad_signature" => ("Bad signature", t.err),
            "filtered" => ("Filtered out", t.dim),
            "replayed" => ("Replayed", t.accent),
            "recovered" => ("Recovered", t.ok),
            "no_trigger" => ("No trigger", t.dim),
            _ => ("Received", t.dim),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ReconcileItem {
    pub last_run_at: Option<String>,
    pub reason: Option<String>,
    pub recovered: u32,
    pub hooks_checked: u32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct WebhooksItem {
    pub path: String,
    pub path_name: String,
    pub health: String,
    pub detail: String,
    pub public_url: Option<String>,
    pub host: Option<String>,
    pub relay_url: Option<String>,
    pub last_delivery_at: Option<String>,
    pub reconcile: ReconcileItem,
    pub secret_store: String,
    pub fix: Option<String>,
}

const PATHS: [(&str, &str, &str, &str); 4] = [
    ("tailscale_funnel", "Tailscale Funnel", "free", "Through your tailnet on :8443. Misses deliveries while asleep; midnad reconciles from GitHub on wake."),
    ("self_relay", "Self-hosted relay", "free", "midna-relay on a box you run. Queues while this Mac sleeps."),
    ("midna_relay", "midna relay", "paid", "Hosted by midna. Queues while asleep and keeps 7 days for replay."),
    ("off", "Off", "", "Stop receiving webhooks. Triggers stay, nothing fires."),
];

/// Paths listed in the menu (and Settings) but not yet pickable.
pub const SOON: [&str; 1] = ["midna_relay"];

fn path_meta(path: &str) -> (&'static str, &'static str) {
    PATHS.iter().find(|p| p.0 == path).map(|p| (p.1, p.2)).unwrap_or(("Webhooks", ""))
}

fn not_implemented(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("(code 5)") || s.contains("not implemented")
}

// ------------------------------------------------------------------ view

/// The list column's tabs: midna's triggers, or Claude's session crons.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Triggers,
    Crons,
}

pub struct TriggersView {
    main: WeakEntity<MainWindow>,
    backend: Arc<dyn Backend>,
    triggers: Vec<TriggerItem>,
    deliveries: Vec<DeliveryItem>,
    status: Option<WebhooksItem>,
    loaded: bool,
    list_error: Option<String>,
    /// `webhooks.status` answered "not implemented".
    status_unavailable: bool,
    /// Some trigger method answered "not implemented" (e.g. set_secret).
    unavailable: Option<String>,
    selected: Option<String>,
    path_menu: bool,
    /// When a click outside closed the menu; that same click landing on "Change path"
    /// shouldn't reopen it.
    path_menu_closed_at: Option<std::time::Instant>,
    secret: LineInput,
    /// Trigger whose "Replace…" secret field is open.
    replacing: Option<String>,
    confirm_discard: Option<String>,
    recon_dismissed: Option<String>,
    refetch_scheduled: bool,
    tab: Tab,
    /// The selected cron on the Crons tab (`CronRow::key`).
    sel_cron: Option<String>,
    /// The schedule editor, open in place of the detail pane.
    editor: Option<schedule::ScheduleEditor>,
    _tasks: Vec<Task<()>>,
}

impl TriggersView {
    pub fn new(main: WeakEntity<MainWindow>, backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        // Recheck stubbed methods (the daemon side may land while we run) and keep "12m ago"
        // labels fresh.
        let tick = cx.spawn(async move |this, cx| {
            let mut n = 0u32;
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                n += 1;
                let Ok(()) = this.update(cx, |v, cx| {
                    if (v.status_unavailable || v.unavailable.is_some()) && n.is_multiple_of(4) {
                        v.unavailable = None;
                        v.refetch(cx);
                    }
                    let visible = v.main.upgrade().is_some_and(|m| m.read(cx).screen == crate::app::Screen::Triggers);
                    if visible {
                        cx.notify();
                    }
                }) else {
                    break;
                };
            }
        });
        let mut v = TriggersView {
            main,
            backend,
            triggers: vec![],
            deliveries: vec![],
            status: None,
            loaded: false,
            list_error: None,
            status_unavailable: false,
            unavailable: None,
            selected: crate::dev::var("MIDNA_DEBUG_TRIGGER").ok(),
            path_menu: crate::dev::var("MIDNA_DEBUG_PATH_MENU").is_ok(),
            path_menu_closed_at: None,
            secret: LineInput::new(cx, true, "Paste the webhook secret"),
            replacing: None,
            confirm_discard: None,
            recon_dismissed: None,
            refetch_scheduled: false,
            tab: if crate::dev::var("MIDNA_DEBUG_TRIGGERS_TAB").is_ok_and(|v| v == "crons") { Tab::Crons } else { Tab::Triggers },
            sel_cron: None,
            editor: None,
            _tasks: vec![tick],
        };
        v.refetch(cx);
        v
    }

    fn refetch(&mut self, cx: &mut Context<Self>) {
        if self.refetch_scheduled {
            return;
        }
        self.refetch_scheduled = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(60)).await;
            let res = cx
                .background_executor()
                .spawn(
                    async move { (backend.call("trigger.list", json!({})), backend.call("trigger.deliveries", json!({"limit": 100})), backend.call("webhooks.status", json!({}))) },
                )
                .await;
            let _ = this.update(cx, |v, cx| {
                v.refetch_scheduled = false;
                let first = !v.loaded;
                v.loaded = true;
                match res.0 {
                    Ok(l) => {
                        v.triggers = parse_list(&l);
                        v.list_error = None;
                    }
                    Err(e) => v.list_error = Some(format!("{e:#}")),
                }
                if let Ok(d) = res.1 {
                    let mut d: Vec<DeliveryItem> = parse_list(&d);
                    d.sort_by(|a, b| b.received_at.cmp(&a.received_at));
                    v.deliveries = d;
                }
                match res.2 {
                    Ok(s) => {
                        v.status = serde_json::from_value(s).ok();
                        v.status_unavailable = false;
                    }
                    Err(e) => {
                        v.status_unavailable = not_implemented(&e);
                        v.status = None;
                    }
                }
                let valid = v.selected.as_ref().is_some_and(|s| v.triggers.iter().any(|t| &t.id == s));
                if !valid {
                    v.selected = v.triggers.iter().find(|t| t.pending().is_some()).or(v.triggers.first()).map(|t| t.id.clone());
                }
                if first {
                    v.debug_actions(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn on_backend_event(&mut self, ev: &BackendEvent, cx: &mut Context<Self>) {
        match ev {
            BackendEvent::Conn(ConnState::Connected) => self.refetch(cx),
            BackendEvent::Event(e) => {
                let k = e.kind.as_str();
                let wh = k == "settings.changed" && e.data.get("key").and_then(|x| x.as_str()).is_some_and(|k| k.starts_with("webhooks."));
                if k.starts_with("trigger.") || k.starts_with("webhooks.") || wh {
                    self.refetch(cx);
                }
                // A terminal's crons changed (or it went away).
                if [kinds::SESSION_AGENT, kinds::SESSION_EXITED, kinds::SESSION_CLOSED, kinds::SESSION_RENAMED].contains(&k) {
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    fn toast(&self, msg: String, cx: &mut Context<Self>) {
        let _ = self.main.update(cx, |m, cx| m.toast(msg, cx));
    }

    fn call(&mut self, method: &'static str, params: Value, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
            let _ = this.update(cx, |v, cx| match res {
                Ok(val) => then(v, val, cx),
                Err(e) if not_implemented(&e) => {
                    v.unavailable = Some(method.to_string());
                    v.toast(format!("This midnad doesn't implement {method} yet. midna will pick it up once it does."), cx);
                    cx.notify();
                }
                Err(e) => v.toast(format!("{method} failed: {e:#}"), cx),
            });
        })
        .detach();
    }

    /// Dev: `MIDNA_DEBUG_TRIGGERS="secret:t_1=abc;enable:t_1;disable:t_2;replay:d_1;select:t_3;type:xyz"`,
    /// `new-schedule`, `edit:t_1`, `copy-cron:0`; `MIDNA_DEBUG_TRIGGERS_TAB=crons` opens on the Crons tab.
    /// drives the real code paths for screenshots (test secrets only, file store).
    fn debug_actions(&mut self, cx: &mut Context<Self>) {
        let Ok(spec) = crate::dev::var("MIDNA_DEBUG_TRIGGERS") else {
            return;
        };
        for step in spec.split(';') {
            let (k, v) = step.split_once(':').unwrap_or((step, ""));
            match k {
                "secret" => {
                    if let Some((id, sec)) = v.split_once('=') {
                        self.secret.set_text(sec, cx);
                        self.save_secret(id.to_string(), cx);
                    }
                }
                "type" => self.secret.set_text(v, cx),
                "enable" => self.set_enabled(v.to_string(), true, cx),
                "disable" => self.set_enabled(v.to_string(), false, cx),
                "select" => self.selected = Some(v.to_string()),
                "replace" => self.replacing = Some(v.to_string()),
                "replay" => {
                    if let Some(d) = self.deliveries.iter().find(|d| d.id == v).cloned() {
                        self.replay(d, cx);
                    }
                }
                // Schedules: `new-schedule`, `edit:t_1`, `copy-cron:0` (the Nth cron, soonest first).
                "new-schedule" => self.open_editor(None, None, None, cx),
                "edit" => {
                    if let Some(t) = self.triggers.iter().find(|t| t.id == v).cloned() {
                        self.open_editor(Some(&t), None, None, cx);
                    }
                }
                "copy-cron" => {
                    let rows = self.main.upgrade().map(|m| schedule::cron_rows(m.read(cx))).unwrap_or_default();
                    if let Some(r) = rows.get(v.parse::<usize>().unwrap_or(0)).cloned() {
                        self.open_editor(None, Some(&r), None, cx);
                    }
                }
                _ => {}
            }
        }
    }

    fn set_enabled(&mut self, id: String, on: bool, cx: &mut Context<Self>) {
        if let Some(t) = self.triggers.iter_mut().find(|t| t.id == id) {
            t.enabled = on; // optimistic; the trigger.updated event confirms
        }
        self.call("trigger.set_enabled", json!({"id": id, "enabled": on}), cx, |v, _, cx| {
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::INSIGHTS | refresh::NEEDS, cx));
            v.refetch(cx);
        });
        cx.notify();
    }

    /// Send the pasted secret straight to midnad (Keychain) and forget it.
    fn save_secret(&mut self, id: String, cx: &mut Context<Self>) {
        if self.secret.text(cx).trim().is_empty() {
            self.toast("Paste the secret first (⌘V).".into(), cx);
            return;
        }
        let secret = self.secret.text(cx);
        self.secret.clear(cx);
        self.replacing = None;
        self.call("trigger.set_secret", json!({"id": id, "secret": secret.trim()}), cx, |v, _, cx| {
            let store = v.status.as_ref().map(|s| s.secret_store.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "Keychain".into());
            v.toast(format!("Secret saved to {}. It is never shown again.", if store == "keychain" { "Keychain" } else { &store }), cx);
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::NEEDS, cx));
            v.refetch(cx);
        });
        cx.notify();
    }

    fn discard(&mut self, id: String, cx: &mut Context<Self>) {
        self.confirm_discard = None;
        self.call("trigger.remove", json!({"id": id}), cx, |v, _, cx| {
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::NEEDS, cx));
            v.refetch(cx);
        });
    }

    fn replay(&mut self, d: DeliveryItem, cx: &mut Context<Self>) {
        let label = format!("{} ({})", d.event_label(), kit::clock_or_day(&d.received_at));
        self.call("trigger.replay", json!({"delivery_id": d.id}), cx, move |v, _, cx| {
            v.toast(format!("Replayed {label} through the same filters"), cx);
            v.refetch(cx);
        });
    }

    fn configure(&mut self, path: &'static str, cx: &mut Context<Self>) {
        self.path_menu = false;
        if self.status.as_ref().is_some_and(|s| s.path == path) {
            self.toast(format!("{} is already the delivery path", path_meta(path).0), cx);
            cx.notify();
            return;
        }
        self.call("webhooks.configure", json!({"path": path}), cx, |v, val, cx| {
            let msg = val.get("message").and_then(|m| m.as_str()).unwrap_or("Delivery path changed").to_string();
            if let Some(url) = val.get("enable_url").and_then(|u| u.as_str()) {
                v.toast(format!("{msg} Enable Funnel at {url}"), cx);
                cx.open_url(url);
            } else {
                v.toast(msg, cx);
            }
            if let Ok(s) = serde_json::from_value::<WebhooksItem>(val.get("status").cloned().unwrap_or(Value::Null)) {
                v.status = Some(s);
            }
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::WEBHOOKS | refresh::SETTINGS, cx));
            v.refetch(cx);
        });
        cx.notify();
    }
}

fn trigger_prompt(text: &str) -> String {
    format!("Draft a midna trigger: {text}")
}

fn suggestions(n: &Names) -> Vec<String> {
    let p = n.current_project_name().unwrap_or_else(|| "this project".into());
    vec![
        format!("Start Claude on every PR opened in {p}"),
        "When a check fails on a feature branch, start Codex to fix it".into(),
        "Raise attention when someone requests my review on Bitbucket".into(),
        format!("Start Claude to triage every new issue labeled bug in {p}"),
    ]
}

// ------------------------------------------------------------------ main-window glue

pub fn on_backend_event(m: &mut MainWindow, ev: &BackendEvent, cx: &mut Context<MainWindow>) {
    if let Some(v) = m.triggers_view.clone() {
        v.update(cx, |v, cx| v.on_backend_event(ev, cx));
    }
}

fn view(m: &mut MainWindow, cx: &mut Context<MainWindow>) -> Entity<TriggersView> {
    match &m.triggers_view {
        Some(v) => v.clone(),
        None => {
            let weak = cx.entity().downgrade();
            let backend = m.backend.clone();
            let v = cx.new(|cx| TriggersView::new(weak, backend, cx));
            m.triggers_view = Some(v.clone());
            v
        }
    }
}

/// Status bar "Set up webhooks": the Triggers screen with the delivery-path picker open.
pub fn open_delivery_path(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    view(m, cx).update(cx, |v, cx| {
        v.path_menu = true;
        cx.notify();
    });
}

pub fn render(m: &mut MainWindow, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let view = view(m, cx);
    div().id("screen-triggers").key_context("MidnaOverlay").track_focus(&m.overlay_focus).flex().flex_1().min_w_0().h_full().child(view).into_any_element()
}

// ------------------------------------------------------------------ rendering

impl Render for TriggersView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let names = self.main.upgrade().map(|m| Names::from_main(m.read(cx))).unwrap_or_default();
        let cmdk = self.main.upgrade().map(|m| m.read(cx).key_label("keys.command_bar")).unwrap_or_else(|| "⌘K".into());
        let crons = self.main.upgrade().map(|m| schedule::cron_rows(m.read(cx)).len()).unwrap_or(0);
        let empty = self.loaded && self.triggers.is_empty() && crons == 0 && self.editor.is_none() && self.list_error.is_none();
        let down = self.status.as_ref().is_some_and(|s| s.health == "down");
        let recon = self.status.as_ref().map(|s| &s.reconcile).filter(|r| r.recovered > 0 && r.last_run_at != self.recon_dismissed).cloned();
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header(&t, &cmdk, cx))
            .child(self.render_path(&t, cx))
            .when(down, |d| d.child(self.render_down(&t)))
            .when_some(recon, |d, r| d.child(self.render_recon(&t, r, cx)))
            .when_some(self.list_error.clone(), |d, e| {
                d.child(div().mx(px(18.)).mt(px(12.)).p(px(12.)).rounded(px(10.)).border_1().border_color(t.err).bg(t.panel).child(format!("Couldn't load triggers: {e}")))
            })
            .when(empty, |d| d.child(self.render_empty(&t, &names)))
            .when(!empty && self.loaded && self.list_error.is_none(), |d| d.child(self.render_body(&t, &names, window, cx)))
            .when(self.path_menu, |d| d.child(self.render_path_menu(&t, cx)))
    }
}

impl TriggersView {
    fn render_header(&self, t: &Theme, cmdk: &str, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let active = self.triggers.iter().filter(|t| t.pending().is_none()).count();
        let pending = self.triggers.len() - active;
        let count = if self.triggers.is_empty() {
            String::new()
        } else {
            format!(
                "{active} trigger{}{}",
                if active == 1 { "" } else { "s" },
                if pending > 0 { format!(" · {pending} draft{} waiting on you", if pending == 1 { "" } else { "s" }) } else { String::new() }
            )
        };
        let main = self.main.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(44.))
            .pl(px(18.))
            .pr(px(12.))
            .border_b_1()
            .border_color(t.line)
            .child(Icon::Triggers.el(16., t.accent))
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Triggers"))
            .child(div().text_color(t.dim).child(count))
            .child(div().flex_1())
            .child(kit::btn(t, "trig-new-schedule", "New schedule").on_click(cx.listener(|v, _, window, cx| v.open_editor(None, None, Some(window), cx))))
            .child(kit::btn(t, "trig-new", "New trigger: just ask").child(kit::mono(t, cmdk.to_string(), 11.).text_color(t.dim)).on_click(cx.listener(move |_, _, window, cx| {
                let _ = main.update(cx, |m, cx| {
                    if m.overlay != Overlay::CommandBar {
                        m.set_overlay(Overlay::CommandBar, window, cx);
                    }
                });
            })))
            .child(kit::back_btn(t, "trig-back", self.main.clone()))
    }

    fn render_path(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let strip = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .px(px(18.))
            .py(px(10.))
            .border_b_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(kit::cap(t, "Delivery path"));
        let change = kit::btn(t, "trig-path", "Change path").child(Icon::Chevron.el(11., t.fg)).when(self.path_menu, |d| d.bg(t.raised)).on_click(cx.listener(|v, _, _, cx| {
            let just_closed = v.path_menu_closed_at.is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(400));
            v.path_menu = !v.path_menu && !just_closed;
            v.path_menu_closed_at = None;
            cx.notify();
        }));
        let Some(s) = &self.status else {
            let (title, detail) = if self.status_unavailable {
                ("Not available yet", "this midnad doesn't serve webhooks.status yet · checking again every 20s")
            } else if !self.loaded {
                ("Checking…", "")
            } else {
                ("Unknown", "webhooks.status failed")
            };
            return strip
                .child(div().flex().items_center().gap(px(7.)).child(kit::dot(t.dim, 8.)).child(div().font_weight(FontWeight::BOLD).child("Webhooks")))
                .child(div().text_color(t.dim).font_weight(FontWeight::BOLD).child(title))
                .child(div().text_size(px(12.)).text_color(t.dim).child(detail))
                .child(div().flex_1())
                .child(change);
        };
        let (name, cost) = path_meta(&s.path);
        let name = if s.path_name.is_empty() { name.to_string() } else { s.path_name.clone() };
        let (status, color) = match (s.path.as_str(), s.health.as_str()) {
            ("off", _) | (_, "off") => ("Off", t.dim),
            (_, "healthy") | (_, "ok") => (if s.path == "tailscale_funnel" { "Healthy" } else { "Connected" }, t.ok),
            (_, "degraded") => ("Degraded", t.need),
            (_, "down") => ("Unreachable", t.err),
            _ => ("Unknown", t.dim),
        };
        let host = s.host.clone().or_else(|| s.public_url.clone()).unwrap_or_default();
        strip
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(kit::dot(color, 8.))
                    .child(div().font_weight(FontWeight::BOLD).child(name))
                    .when(!cost.is_empty(), |d| d.child(kit::chip(t, cost))),
            )
            .when(!host.is_empty(), |d| d.child(kit::mono(t, host, 12.).text_color(t.fg)))
            .when(s.path != "off", |d| d.child(div().text_color(color).font_weight(FontWeight::BOLD).child(status)))
            .child(div().text_size(px(12.)).text_color(t.dim).min_w_0().truncate().child(s.detail.clone()))
            .child(div().flex_1())
            .child(change)
    }

    fn render_path_menu(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let cur = self.status.as_ref().map(|s| s.path.clone()).unwrap_or_default();
        let mut menu = div()
            .id("trig-path-menu")
            .absolute()
            .top(px(44. + 46.))
            .right(px(18.))
            .w(px(360.))
            .p(px(6.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|v, _, _, cx| {
                v.path_menu = false;
                v.path_menu_closed_at = Some(std::time::Instant::now());
                cx.notify();
            }));
        for (key, name, cost, desc) in PATHS {
            let on = cur == key;
            let soon = SOON.contains(&key);
            menu = menu.child(
                div()
                    .id(key)
                    .flex()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(9.))
                    .rounded(px(7.))
                    .when(soon, |d| d.opacity(0.55))
                    .when(!soon, |d| d.cursor_pointer().hover(|s| s.bg(t.accent_soft)).on_click(cx.listener(move |v, _, _, cx| v.configure(key, cx))))
                    .child(div().w(px(16.)).flex_none().text_color(t.accent).child(if on { "✓" } else { "" }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().flex_1().font_weight(FontWeight::BOLD).child(name))
                                    .when(soon, |d| d.child(soon_pill(t)))
                                    .when(!soon && !cost.is_empty(), |d| d.child(kit::chip(t, cost))),
                            )
                            .child(div().text_size(px(12.)).text_color(t.dim).child(desc)),
                    ),
            );
        }
        deferred(
            menu.child(
                div()
                    .mt(px(4.))
                    .px(px(10.))
                    .pt(px(8.))
                    .pb(px(4.))
                    .border_t_1()
                    .border_color(t.line)
                    .text_size(px(11.5))
                    .text_color(t.dim)
                    .child("Only you can change this. midnad sets the path up; secrets carry over."),
            ),
        )
        .with_priority(1)
    }

    fn render_down(&self, t: &Theme) -> impl IntoElement + use<> {
        let s = self.status.clone().unwrap_or_default();
        let fix = s.fix.clone();
        let main = self.main.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .mx(px(18.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(t.panel)
            .child(kit::dot(t.err, 8.))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .gap(px(6.))
                    .child(div().font_weight(FontWeight::BOLD).child("Webhooks can’t reach this Mac."))
                    .child(div().text_color(t.dim).truncate().child(s.detail)),
            )
            .when_some(fix, |d, fix| {
                let prompt = format!("Fix midna webhooks: {fix}");
                d.child(kit::btn_primary(t, "trig-fix", format!("Ask: {fix}")).on_click(move |_, window, cx| ask(&main, prompt.clone(), window, cx)))
            })
            .map(|d| super::border_w(d, 1.5).border_color(t.err))
    }

    fn render_recon(&self, t: &Theme, r: ReconcileItem, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let when = r
            .reason
            .as_deref()
            .map(|w| {
                if w == "wake" {
                    "On wake"
                } else if w == "startup" {
                    "At startup"
                } else {
                    "On a manual check"
                }
            })
            .unwrap_or("Earlier");
        let at = r.last_run_at.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .mx(px(18.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(9.))
            .rounded(px(10.))
            .border_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(Icon::Restart.el(16., t.ok))
            .child(div().font_weight(FontWeight::BOLD).whitespace_nowrap().child(format!("{} missed, recovered", r.recovered)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.))
                    .text_color(t.dim)
                    .child(format!("{when}, midnad compared GitHub’s delivery log with its own and redelivered the {} it never received.", r.recovered)),
            )
            .child(kit::btn(t, "trig-recon-x", "Dismiss").on_click(cx.listener(move |v, _, _, cx| {
                v.recon_dismissed = at.clone();
                cx.notify();
            })))
    }

    fn render_empty(&self, t: &Theme, n: &Names) -> impl IntoElement + use<> {
        let mut list = div().flex().flex_col().gap(px(6.));
        for (i, s) in suggestions(n).into_iter().enumerate() {
            let main = self.main.clone();
            let prompt = trigger_prompt(&s);
            list = list.child(
                div()
                    .id(("trig-sugg", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.panel)
                    .cursor_pointer()
                    .hover(|st| st.bg(t.raised))
                    .on_click(move |_, window, cx| ask(&main, prompt.clone(), window, cx))
                    .child(div().text_color(t.accent).font_weight(FontWeight::BOLD).child("Ask"))
                    .child(div().flex_1().child(s))
                    .child(kit::mono(t, "↩", 11.).text_color(t.dim)),
            );
        }
        let arrival = match &self.status {
            Some(s) if s.path != "off" => {
                let host = s.host.clone().or_else(|| s.public_url.clone()).unwrap_or_default();
                Some(format!(
                    "Webhooks will arrive via {}{}.",
                    if s.path_name.is_empty() { path_meta(&s.path).0.to_string() } else { s.path_name.clone() },
                    if host.is_empty() { String::new() } else { format!(" at {host}") }
                ))
            }
            Some(_) => Some("Webhooks are off. Pick a delivery path above before switching a trigger on.".into()),
            None => None,
        };
        div().flex().flex_1().min_h_0().items_center().justify_center().p(px(24.)).child(
            div()
                .w(px(560.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(Icon::Triggers.el(28., t.accent))
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child("No triggers yet"))
                .child(div().text_color(t.dim).child(
                    "A trigger turns a GitHub or Bitbucket event into an agent terminal, a command, or a ping. Local triggers react to this Mac's terminals: an agent hook, a blocked prompt, an idle terminal. Describe one and an agent drafts it. You switch it on.",
                ))
                .child(list)
                .when_some(arrival, |d, a| d.child(div().text_size(px(12.)).text_color(t.dim).child(a))),
        )
    }

    fn render_body(&mut self, t: &Theme, n: &Names, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sel = self.selected.as_ref().and_then(|id| self.triggers.iter().find(|t| &t.id == id)).cloned().or_else(|| self.triggers.first().cloned());
        let crons = self.main.upgrade().map(|m| schedule::cron_rows(m.read(cx))).unwrap_or_default();
        let cron_sel = self.sel_cron.as_ref().and_then(|k| crons.iter().find(|c| &c.key() == k)).or(crons.first()).cloned();
        let column = div()
            .flex()
            .flex_col()
            .w(px(360.))
            .flex_none()
            .min_h_0()
            .border_r_1()
            .border_color(t.line)
            .child(self.render_tabs(t, crons.len(), cx))
            .child(match self.tab {
                Tab::Triggers => self.render_list(t, n, sel.as_ref().map(|s| s.id.clone()), cx).into_any_element(),
                Tab::Crons => self.render_cron_list(t, &crons, cron_sel.as_ref().map(|c| c.key()), cx).into_any_element(),
            });
        let detail = if self.editor.is_some() {
            Some(self.render_editor(t, window, cx).into_any_element())
        } else {
            match self.tab {
                Tab::Triggers => sel.map(|s| self.render_detail(t, n, &s, window, cx).into_any_element()),
                Tab::Crons => Some(self.render_cron_detail(t, cron_sel.as_ref(), cx).into_any_element()),
            }
        };
        div().flex().flex_1().min_h_0().mt(px(12.)).border_t_1().border_color(t.line).child(column).children(detail)
    }

    fn render_tabs(&self, t: &Theme, crons: usize, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let tab = |id: &'static str, label: &'static str, count: usize, which: Tab, cx: &mut Context<Self>| {
            let on = self.tab == which;
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(38.))
                .px(px(2.))
                .border_b_2()
                .border_color(if on { t.accent } else { transparent_black() })
                .text_color(if on { t.fg } else { t.dim })
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg))
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.tab = which;
                    cx.notify();
                }))
                .child(label)
                .child(div().px(px(6.)).rounded(px(8.)).bg(t.line).text_size(px(11.)).text_color(t.dim).child(count.to_string()))
        };
        div()
            .flex()
            .flex_none()
            .gap(px(18.))
            .px(px(16.))
            .border_b_1()
            .border_color(t.line)
            .child(tab("trig-tab-triggers", "Triggers", self.triggers.len(), Tab::Triggers, cx))
            .child(tab("trig-tab-crons", "Crons", crons, Tab::Crons, cx))
    }

    fn render_list(&self, t: &Theme, n: &Names, sel: Option<String>, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pending: Vec<&TriggerItem> = self.triggers.iter().filter(|t| t.pending().is_some()).collect();
        let active: Vec<&TriggerItem> = self.triggers.iter().filter(|t| t.pending().is_none()).collect();
        let mut list = div().id("trig-list").flex().flex_col().flex_1().min_h_0().overflow_y_scroll();
        let row_bg = |on: bool, d: Stateful<Div>| {
            if on { d.bg(t.raised).border_l_2().border_color(t.accent) } else { d.border_l_2().border_color(transparent_black()) }
        };
        if !pending.is_empty() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(16.))
                    .pt(px(12.))
                    .pb(px(4.))
                    .child(kit::cap(t, "Waiting on you").text_color(t.need))
                    .child(div().text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(t.need).child(pending.len().to_string())),
            );
            for tr in pending {
                let id = tr.id.clone();
                let state = match tr.pending() {
                    Some(Pending::Secret) => "Needs secret",
                    _ => "Ready to enable",
                };
                list = list.child(
                    row_bg(
                        sel.as_deref() == Some(tr.id.as_str()),
                        div().id(SharedString::from(format!("trig-p-{}", tr.id))).flex().gap(px(10.)).pl(px(14.)).pr(px(16.)).py(px(9.)).cursor_pointer().hover(|s| s.bg(t.raised)),
                    )
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.selected = Some(id.clone());
                        v.secret.clear(cx);
                        v.replacing = None;
                        cx.notify();
                    }))
                    .child(div().w(px(30.)).flex_none().flex().justify_center().pt(px(2.)).child(Icon::Lock.el(14., t.need)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::BOLD).child(tr.name.clone()))
                                    .child(div().text_size(px(11.5)).font_weight(FontWeight::BOLD).text_color(t.need).child(state)),
                            )
                            .child(div().text_size(px(12.)).text_color(t.dim).truncate().child(drafted_line(n, tr))),
                    ),
                );
            }
        }
        list = list.child(div().px(px(16.)).pt(px(14.)).pb(px(4.)).child(kit::cap(t, "Triggers")));
        if active.is_empty() {
            list = list.child(div().px(px(16.)).py(px(6.)).text_size(px(12.)).text_color(t.dim).child("None switched on yet."));
        }
        for tr in active {
            let (id, id2) = (tr.id.clone(), tr.id.clone());
            let on = tr.enabled;
            list = list.child(
                row_bg(
                    sel.as_deref() == Some(tr.id.as_str()),
                    div().id(SharedString::from(format!("trig-a-{}", tr.id))).flex().items_center().gap(px(10.)).pl(px(14.)).cursor_pointer().hover(|s| s.bg(t.raised)),
                )
                .on_click(cx.listener(move |v, _, _, cx| {
                    v.selected = Some(id.clone());
                    v.secret.clear(cx);
                    v.replacing = None;
                    cx.notify();
                }))
                .child(kit::switch(t, SharedString::from(format!("trig-sw-{}", tr.id)), on).on_click(cx.listener(move |v, _, _, cx| {
                    cx.stop_propagation();
                    v.set_enabled(id2.clone(), !on, cx);
                })))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .pr(px(16.))
                        .py(px(9.))
                        .when(!on, |d| d.opacity(0.6))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(div().flex_1().min_w_0().truncate().font_weight(FontWeight::BOLD).child(tr.name.clone()))
                                .when(tr.builtin.is_some(), |d| d.child(builtin_tag(t)))
                                .child(match tr.set_status() {
                                    Some(st) => kit::dot(t.status_color(&st.color), 9.).mx(px(2.)).into_any_element(),
                                    None => tr.icon().el(14., t.dim).into_any_element(),
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .child(kit::mono(t, format!("{} · {}", tr.schedule_words().unwrap_or_else(|| tr.event.clone()), tr.scope(n)), 11.5).flex_1().min_w_0().truncate().text_color(t.dim))
                                .child(div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child(tr.last_short())),
                        ),
                ),
            );
        }
        let sel_tr = self.selected.as_ref().and_then(|s| self.triggers.iter().find(|t| &t.id == s));
        let hint_name = sel_tr.map(|t| t.name.clone()).unwrap_or_else(|| "Review new PRs".into());
        let hint = if sel_tr.is_some_and(|t| t.is_local()) { format!("Only fire {hint_name} in Claude terminals") } else { format!("Skip dependabot PRs in {hint_name}") };
        let main = self.main.clone();
        let prompt = trigger_prompt(&hint);
        list.child(div().flex_1().min_h(px(8.))).child(
            div()
                .id("trig-edit-hint")
                .m(px(16.))
                .px(px(12.))
                .py(px(10.))
                .rounded(px(9.))
                .border_1()
                .border_color(t.line)
                .text_size(px(12.))
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.border_color(t.accent))
                .on_click(move |_, window, cx| ask(&main, prompt.clone(), window, cx))
                .child(div().flex().flex_wrap().gap(px(4.)).child("Edit by asking.").child(div().text_color(t.fg).child(format!("“{hint}”")))),
        )
    }

    fn secret_field(&self, t: &Theme, id: &str, window: &Window, cx: &mut Context<Self>) -> Stateful<Div> {
        let focus = self.secret.focus.clone();
        let tid = id.to_string();
        self.secret
            .render(t, SharedString::from(format!("trig-secret-{id}")), window)
            .flex_1()
            .min_w(px(160.))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.focus(window, cx))
            .on_key_down(cx.listener(move |v, ev: &KeyDownEvent, _, cx| match v.secret.on_key(ev, cx) {
                KeyOutcome::Submit => {
                    v.save_secret(tid.clone(), cx);
                    cx.stop_propagation();
                }
                KeyOutcome::Cancel if !v.secret.is_empty(cx) || v.replacing.is_some() => {
                    v.secret.clear(cx);
                    v.replacing = None;
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {}
            }))
    }

    fn render_detail(&self, t: &Theme, n: &Names, s: &TriggerItem, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let pending = s.pending();
        let project = s.project_id().map(|p| n.project(&p));
        let (id_sw, id_save, id_enable, id_discard, id_discard2, id_replace) = (s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone(), s.id.clone());
        let on = s.enabled;
        let confirming = self.confirm_discard.as_deref() == Some(s.id.as_str());
        let discard_btn = |label: &'static str, idd: String, cx: &mut Context<Self>| -> AnyElement {
            if confirming {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(kit::btn_danger(t, "trig-discard-yes", "Discard", 32.).on_click(cx.listener(move |v, _, _, cx| v.discard(idd.clone(), cx))))
                    .child(kit::btn(t, "trig-discard-no", "Keep").h(px(32.)).on_click(cx.listener(|v, _, _, cx| {
                        v.confirm_discard = None;
                        cx.notify();
                    })))
                    .into_any_element()
            } else {
                let idc = idd.clone();
                kit::btn(t, "trig-discard", label)
                    .h(px(32.))
                    .on_click(cx.listener(move |v, _, _, cx| {
                        v.confirm_discard = Some(idc.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            }
        };
        let header = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).child(s.name.clone()))
            .children(project.clone().map(|p| kit::chip(t, p)))
            .child(match s.source_icon() {
                Some(i) => div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.))
                    .h(px(20.))
                    .px(px(7.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(t.line)
                    .text_size(px(11.5))
                    .text_color(t.dim)
                    .child(i.el(11., t.dim))
                    .child(s.source_label()),
                None => kit::chip(t, s.source_label()),
            })
            .when(s.builtin.is_some(), |d| d.child(builtin_tag(t)))
            .when(s.ended(), |d| d.child(kit::chip(t, "Ended")))
            .child(div().flex_1())
            .when(s.is_local() && s.filter.cron.is_some(), |d| {
                let tr = s.clone();
                d.child(kit::btn(t, "trig-edit-schedule", "Edit schedule").on_click(cx.listener(move |v, _, window, cx| v.open_editor(Some(&tr), None, Some(window), cx))))
            })
            .when(pending.is_none(), |d| {
                d.child(div().text_size(px(12.)).text_color(t.dim).child(s.last_text()))
                    .child(kit::switch(t, "trig-detail-sw", on).on_click(cx.listener(move |v, _, _, cx| v.set_enabled(id_sw.clone(), !on, cx))))
                    .child(div().min_w(px(22.)).font_weight(FontWeight::BOLD).child(if on { "On" } else { "Off" }))
            });
        let callout = |body: Div| super::border_w(div().mt(px(16.)).px(px(16.)).py(px(14.)).rounded(px(10.)).bg(t.need_soft).child(body), 1.5).border_color(t.need);
        let drafted = drafted_line(n, s);
        let secret_box = (pending == Some(Pending::Secret)).then(|| {
            callout(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .child(div().font_weight(FontWeight::BOLD).child("Paste the webhook secret to finish this draft."))
                            .child(div().text_color(t.dim).child(format!("{drafted}. Agents can’t see or set secrets; it goes straight to Keychain."))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(self.secret_field(t, &s.id, window, cx))
                            .child(kit::btn_primary(t, "trig-save-secret", "Save secret").h(px(32.)).on_click(cx.listener(move |v, _, _, cx| v.save_secret(id_save.clone(), cx))))
                            .child(discard_btn("Discard draft", id_discard.clone(), cx)),
                    ),
            )
        });
        let enable_box = (pending == Some(Pending::Enable)).then(|| {
            callout(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_wrap()
                            .gap(px(4.))
                            .child(div().font_weight(FontWeight::BOLD).child("Secret is set. Switch it on when it looks right."))
                            .child(div().text_color(t.dim).child(format!("{drafted}. Agents can draft triggers but only you can enable them."))),
                    )
                    .child(kit::btn_primary(t, "trig-enable", "Enable trigger").h(px(32.)).on_click(cx.listener(move |v, _, _, cx| v.set_enabled(id_enable.clone(), true, cx))))
                    .child(discard_btn("Discard", id_discard2.clone(), cx)),
            )
        });

        // When / Only if / Then / Secret
        let dt = |label: &str, last: bool| {
            div().w(px(96.)).flex_none().px(px(14.)).pt(px(12.)).pb(px(12.)).when(!last, |d| d.border_b_1().border_color(t.line)).child(kit::cap(t, label))
        };
        let dd = |last: bool| div().flex_1().min_w_0().px(px(14.)).py(px(10.)).when(!last, |d| d.border_b_1().border_color(t.line));
        let row = |a: Div, b: Div| div().flex().child(a).child(b);
        let when = dd(false).child(
            div()
                .flex()
                .flex_wrap()
                .items_baseline()
                .gap(px(5.))
                .child(match s.schedule_words() {
                    Some(w) => div().font_weight(FontWeight::BOLD).child(w),
                    None => kit::mono(t, s.event.clone(), 13.).text_color(t.accent),
                })
                .child(div().text_color(t.dim).child("on"))
                .child(if s.is_local() { div().child(s.scope(n)) } else { kit::mono(t, s.scope(n), 12.5) })
                .child(div().text_color(t.dim).child(format!("· {}", s.source_label()))),
        );
        let only_if = dd(false).child(match s.filter_text() {
            Some(f) => kit::mono(t, f, 12.5),
            None => div().text_color(t.dim).child("every matching event"),
        });
        let term_name = match s.action_kind().as_str() {
            "start_agent" => Some(s.session_name_template.clone().unwrap_or_else(|| s.name.clone())),
            _ => None,
        };
        let then = dd(false).flex().flex_col().gap(px(8.)).child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(s.icon().el(14., t.dim))
                .child(div().font_weight(FontWeight::BOLD).child(s.action_label(n)))
                .when_some(term_name, |d, tn| d.child(div().text_color(t.dim).child("as")).child(kit::mono(t, tn, 12.))),
        );
        let then = then.when_some(s.set_status(), |d, st| {
            let color = t.status_color(&st.color);
            let clears = match st.clear_on.as_str() {
                "turn" => "clears when the next turn starts",
                "status" => "clears when the status changes",
                "never" => "stays until cleared",
                _ => "clears on prompt",
            };
            let base = if st.base.is_empty() { String::new() } else { format!("counts as {} · ", state_words(&st.base)) };
            d.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .px(px(10.))
                            .py(px(5.))
                            .rounded(px(7.))
                            .bg(color.opacity(0.12))
                            .child(kit::dot(color, 9.))
                            .children(st.icon.as_deref().and_then(Icon::from_name).map(|i| i.el(12., color)))
                            .child(div().font_weight(FontWeight::BOLD).text_color(color).child(st.label.clone())),
                    )
                    .child(div().text_size(px(12.)).text_color(t.dim).child(format!("{base}{clears}"))),
            )
        });
        let template = s.template();
        let then = then.when(!template.is_empty(), |d| {
            d.child(div().px(px(12.)).py(px(10.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.)).line_height(px(12. * 1.55)).child(template))
        });
        let replacing = self.replacing.as_deref() == Some(s.id.as_str());
        let secret_text = if !s.secret_set {
            "Not set".to_string()
        } else {
            let store = match s.secret_store.as_deref() {
                Some("file") => "file",
                _ => "Keychain",
            };
            match &s.secret_set_at {
                Some(at) => format!("Set {} · {store}", kit::day_label(at)),
                None => format!("Set · {store}"),
            }
        };
        let secret_row = dd(true).flex().items_center().gap(px(10.)).py(px(8.)).when(!replacing, |d| {
            d.child(kit::mono(t, if s.secret_set { "••••••••" } else { "—" }, 12.).text_color(t.dim))
                .child(div().flex_1().text_size(px(12.)).text_color(t.dim).child(secret_text))
                .when(pending.is_none(), |d| {
                    d.child(kit::btn(t, "trig-replace", "Replace…").on_click(cx.listener(move |v, _, window, cx| {
                        v.replacing = Some(id_replace.clone());
                        v.secret.clear(cx);
                        v.secret.focus.focus(window, cx);
                        cx.notify();
                    })))
                })
        });
        let secret_row = if replacing {
            let idr = s.id.clone();
            secret_row
                .child(self.secret_field(t, &s.id, window, cx))
                .child(kit::btn_primary(t, "trig-replace-save", "Save secret").on_click(cx.listener(move |v, _, _, cx| v.save_secret(idr.clone(), cx))))
                .child(kit::btn(t, "trig-replace-cancel", "Cancel").on_click(cx.listener(|v, _, _, cx| {
                    v.replacing = None;
                    v.secret.clear(cx);
                    cx.notify();
                })))
        } else {
            secret_row
        };
        let dl = div()
            .mt(px(18.))
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(t.line)
            .bg(t.panel)
            .overflow_hidden()
            .child(row(dt("When", false), when))
            .child(row(dt("Only if", false), only_if))
            .child(row(dt("Then", false), then));
        let dl = if s.is_local() {
            // No secret: local triggers fire on what midnad sees here. Show the cooldown and
            // the CLI that makes this trigger instead of a form.
            let cooldown = s.cooldown_secs.unwrap_or(60);
            let cli = s.cli();
            let copy = cli.clone();
            dl.child(row(
                dt("Cooldown", false),
                dd(false).child(div().text_color(t.dim).child(format!("{} per terminal{}", cooldown_text(cooldown), if s.cooldown_secs.is_none() { " (default)" } else { "" }))),
            ))
            .child(row(
                dt("CLI", true),
                dd(true)
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .py(px(8.))
                    .child(kit::mono(t, cli, 11.5).flex_1().min_w_0().text_color(t.dim))
                    .child(kit::btn(t, "trig-cli-copy", "Copy").on_click(cx.listener(move |v, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                        v.toast("Copied the CLI command".into(), cx);
                    }))),
            ))
        } else {
            dl.child(row(dt("Secret", true), secret_row))
        };

        // Deliveries
        let mine: Vec<DeliveryItem> = self.deliveries.iter().filter(|d| d.concerns(&s.id)).take(20).cloned().collect();
        let orphans: Vec<DeliveryItem> = self.deliveries.iter().filter(|d| d.trigger_id.is_none() && d.triggers_fired.is_empty()).take(10).cloned().collect();
        let today = mine.iter().filter(|d| kit::clock_or_day(&d.received_at).contains(':')).count();
        let mut detail = div()
            .id("trig-detail")
            .flex_1()
            .min_w_0()
            .overflow_y_scroll()
            .px(px(24.))
            .pt(px(18.))
            .pb(px(24.))
            .child(header)
            .children(secret_box)
            .children(enable_box)
            .child(dl)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(10.))
                    .mt(px(22.))
                    .mb(px(8.))
                    .child(kit::cap(t, if s.is_local() { "Recent firings" } else { "Recent deliveries" }))
                    .when(today > 0, |d| d.child(div().text_size(px(12.)).text_color(t.dim).child(format!("{today} today")))),
            );
        if mine.is_empty() {
            let empty = if s.is_local() {
                match &s.last_fired_at {
                    Some(at) => format!("Fired {} time{}, most recently {}.", s.fired, if s.fired == 1 { "" } else { "s" }, kit::ago(Some(at.as_str()))),
                    None => match s.next_run() {
                        Some(r) => format!("Hasn’t fired yet. Next run: {r}."),
                        None => format!("Hasn’t fired yet. The first {} on {} will show here.", s.event, s.scope(n)),
                    },
                }
            } else {
                format!("Nothing delivered yet. The first matching {} on {} will show here.", s.event, s.repo())
            };
            detail = detail.child(div().p(px(14.)).rounded(px(10.)).border_1().border_color(t.line).text_color(t.dim).child(empty));
        } else {
            let mut table = div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).overflow_hidden();
            for (i, d) in mine.into_iter().enumerate() {
                table = table.child(self.delivery_row(t, n, d, ("del", i), false, cx));
            }
            detail = detail.child(table);
        }
        if !orphans.is_empty() {
            let mut table = div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).overflow_hidden();
            for (i, d) in orphans.into_iter().enumerate() {
                table = table.child(self.delivery_row(t, n, d, ("orphan", i), true, cx));
            }
            detail = detail.child(div().mt(px(22.)).mb(px(8.)).child(kit::cap(t, "Not matched by any trigger"))).child(table);
        }
        if let Some(m) = &self.unavailable {
            detail = detail.child(div().mt(px(16.)).text_size(px(12.)).text_color(t.dim).child(format!("{m} isn’t implemented by this midnad yet; checking again every 20s.")));
        }
        detail
    }

    fn delivery_row(&self, t: &Theme, n: &Names, d: DeliveryItem, id: (&'static str, usize), orphan: bool, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let (label, color) = d.verdict_meta(t);
        let can_replay = d.verdict != "bad_signature";
        let what = d.subject.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| d.summary.clone());
        let started = d.session_started.as_deref().map(|s| format!("Started {}", n.term(s)));
        let outcome = if d.summary.is_empty() || (d.subject.is_none() && !orphan) { started.clone().unwrap_or_default() } else { d.summary.clone() };
        let recovered = d.recovered || d.verdict == "recovered";
        let replay_btn = if can_replay {
            let dd = d.clone();
            kit::btn(t, id, "Replay").on_click(cx.listener(move |v, _, _, cx| v.replay(dd.clone(), cx))).into_any_element()
        } else {
            div().text_size(px(11.5)).text_color(t.dim).whitespace_nowrap().child("Unsigned, can’t replay").into_any_element()
        };
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .px(px(12.))
            .py(px(9.))
            .border_b_1()
            .border_color(t.line)
            .child(kit::mono(t, kit::clock_or_day(&d.received_at), 12.).w(px(78.)).flex_none().text_color(t.dim))
            .child(
                div().w(px(112.)).flex_none().flex().child(
                    div()
                        .flex()
                        .items_center()
                        .h(px(20.))
                        .px(px(7.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(color)
                        .text_color(color)
                        .text_size(px(11.5))
                        .font_weight(FontWeight::BOLD)
                        .whitespace_nowrap()
                        .child(label),
                ),
            )
            .child(if orphan {
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(kit::mono(t, d.event_label(), 12.))
                    .child(div().text_color(t.dim).truncate().child(format!("· {}", if outcome.is_empty() { what } else { outcome })))
            } else {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(kit::mono(t, d.event_label(), 12.))
                            .when(!what.is_empty(), |x| x.child(div().truncate().child(format!("· {what}"))))
                            .when(recovered, |x| x.child(kit::chip(t, "recovered").h(px(17.)).text_color(t.ok).border_color(t.ok))),
                    )
                    .when(!outcome.is_empty(), |x| x.child(div().text_size(px(12.)).text_color(t.dim).truncate().child(outcome)))
            })
            .child(div().w(px(if orphan { 150. } else { 88. })).flex_none().flex().justify_end().child(replay_btn))
    }
}

/// "60s", "5 min", "1 h".
fn cooldown_text(secs: u64) -> String {
    match secs {
        s if s >= 3600 && s % 3600 == 0 => format!("{} h", s / 3600),
        s if s >= 60 && s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s}s"),
    }
}

/// The "Coming soon" pill on paths that aren't pickable yet.
pub fn soon_pill(t: &Theme) -> Div {
    kit::chip(t, "Coming soon").h(px(18.)).rounded_full().text_size(px(10.5)).border_color(t.accent).text_color(t.accent).bg(t.accent_soft)
}

/// The small "built-in" tag on triggers midna ships.
fn builtin_tag(t: &Theme) -> Div {
    kit::chip(t, "built-in").h(px(17.)).text_size(px(10.5)).text_color(t.dim)
}

fn drafted_line(n: &Names, tr: &TriggerItem) -> String {
    let who = match (tr.created_by.kind.as_str(), &tr.created_by.session) {
        ("human", _) => "you".to_string(),
        (_, Some(sid)) => {
            let agent = match n.glyph(sid) {
                Some(crate::model::Glyph::Codex) => "Codex in ",
                Some(crate::model::Glyph::Claude) => "Claude in ",
                _ => "",
            };
            format!("{agent}{}", n.term(sid))
        }
        _ => "an agent".into(),
    };
    let when = kit::ago(Some(&tr.created_at));
    let _ = parse_rfc3339;
    format!("Drafted by {who} · {when}")
}

#[cfg(test)]
mod tests {
    use super::{Icon, Pending, TriggerItem, WebhooksItem, not_implemented};
    use crate::model::parse_list;
    use serde_json::json;

    fn proto_trigger(enabled: bool, state: midna_proto::TriggerState, secret: bool) -> midna_proto::Trigger {
        use midna_proto as p;
        p::Trigger {
            id: "t_1".into(),
            name: "Review new PRs".into(),
            source: p::TriggerSource::Github,
            event: "pull_request.opened".into(),
            filter: p::TriggerFilter { repo: Some("mrgnhnt96/midna".into()), branch: Some("main".into()), ..Default::default() },
            action: p::TriggerAction::StartAgent { project_id: "p_aaaaaa".into(), agent: p::AgentKind::Codex, prompt_template: "Review {{pr.number}}".into() },
            enabled,
            state,
            secret_set: secret,
            created_by: p::Actor::agent(Some("abcd1234".into())),
            created_at: "2026-10-03T10:00:00Z".into(),
            last_fired_at: None,
            fired: 0,
            last_fired_summary: None,
            enabled_at: None,
            secret_set_at: None,
            secret_store: None,
            github_hook_id: None,
            session_name_template: None,
            cooldown_secs: None,
            builtin: None,
        }
    }

    #[test]
    fn reads_local_triggers() {
        use midna_proto as p;
        let names = super::Names::default();
        let mut tr = proto_trigger(true, p::TriggerState::Active, false);
        tr.source = p::TriggerSource::Local;
        tr.event = "agent.prompt_blocked".into();
        tr.filter = p::TriggerFilter { fields: [("message".to_string(), "*Compact first*".to_string())].into(), ..Default::default() };
        tr.action = p::TriggerAction::SendToSession {
            steps: vec![p::SendStep { text: "/compact".into(), enter: true }, p::SendStep { text: "{{last_prompt}}".into(), enter: true }],
        };
        let got: Vec<TriggerItem> = parse_list(&json!([tr.clone()]));
        let g = &got[0];
        assert_eq!(g.source_label(), "Local");
        assert_eq!(g.pending(), None);
        assert_eq!(g.action_label(&names), "Send “/compact” → “{{last_prompt}}”");
        assert_eq!(g.filter_text().as_deref(), Some("message ~ \"*Compact first*\""));
        assert_eq!(g.scope(&names), "any terminal");
        assert_eq!(
            g.cli(),
            "midna triggers add --source local --name 'Review new PRs' --event agent.prompt_blocked --match 'message=*Compact first*' --send /compact --send '{{last_prompt}}' --enable"
        );

        tr.builtin = Some("prompt_blocked_status".into());
        tr.filter = p::TriggerFilter { idle_minutes: Some(55), agent: Some(p::AgentKind::Claude), ..Default::default() };
        tr.action = p::TriggerAction::SetStatus { label: "Prompt blocked".into(), color: "amber".into(), icon: None, base: p::StatusState::NeedsYou, clear_on: p::StatusClear::Prompt };
        let g = parse_list::<TriggerItem>(&json!([tr]))[0].clone();
        assert_eq!(g.builtin.as_deref(), Some("prompt_blocked_status"));
        assert_eq!(g.set_status().unwrap().base, "needs_you");
        assert_eq!(g.filter_text().as_deref(), Some("idle 55 min"));
        assert_eq!(g.scope(&names), "any Claude terminal");
        assert_eq!(
            g.cli(),
            "midna triggers add --source local --name 'Review new PRs' --event agent.prompt_blocked --for-agent claude --idle-for 55m --set-status 'Prompt blocked' --color amber --base needs_you --enable"
        );

        tr.event = "schedule".into();
        tr.filter = p::TriggerFilter { cron: Some("0 9 * * mon-fri".into()), ..Default::default() };
        tr.action = p::TriggerAction::Notify { title: "Standup".into(), body: String::new(), sound: true, category: None };
        let g = parse_list::<TriggerItem>(&json!([tr]))[0].clone();
        assert_eq!(g.scope(&names), "no terminal");
        let text = g.filter_text().unwrap();
        assert!(text.starts_with("cron \"0 9 * * mon-fri\" · next ") && text.ends_with(" 09:00"), "{text}");
        assert!(g.cli().contains("--event schedule --cron '0 9 * * mon-fri' --notify Standup"), "{}", g.cli());
    }

    #[test]
    fn reads_proto_trigger_and_pending_state() {
        use midna_proto::TriggerState as S;
        let got: Vec<TriggerItem> = parse_list(&json!([proto_trigger(false, S::NeedsSecret, false)]));
        assert_eq!(got[0].pending(), Some(Pending::Secret));
        assert_eq!(got[0].icon(), Icon::Codex);
        assert_eq!(got[0].template(), "Review {{pr.number}}");
        assert_eq!(got[0].filter_text().as_deref(), Some("branch == \"main\""));
        let got: Vec<TriggerItem> = parse_list(&json!([proto_trigger(false, S::Draft, true)]));
        assert_eq!(got[0].pending(), Some(Pending::Enable));
        let got: Vec<TriggerItem> = parse_list(&json!([proto_trigger(false, S::Paused, true)]));
        assert_eq!(got[0].pending(), None);
        let got: Vec<TriggerItem> = parse_list(&json!([proto_trigger(true, S::Active, true)]));
        assert_eq!(got[0].pending(), None);
    }

    #[test]
    fn reads_proto_webhooks_status() {
        let s = midna_proto::methods::WebhooksStatus {
            path: "tailscale_funnel".into(),
            path_name: "Tailscale Funnel".into(),
            health: "down".into(),
            fix: Some("turn funnel on".into()),
            ..Default::default()
        };
        let got: WebhooksItem = serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
        assert_eq!(got.health, "down");
        assert_eq!(got.fix.as_deref(), Some("turn funnel on"));
    }

    #[test]
    fn code_5_is_not_implemented() {
        assert!(not_implemented(&anyhow::anyhow!("trigger.add is not implemented yet (later phase) (code 5)")));
        assert!(!not_implemented(&anyhow::anyhow!("no trigger t_1 (code 3)")));
    }
}
