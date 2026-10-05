//! Rules screen (docs/design/Rules-B.dc.html): agent removal requests in a band on top,
//! three scope lanes (Global, Project, Terminal), a tiny "test a command" strip
//! (`policy.check`), and the live "Last fired" feed (`rule.fired` events) docked at the
//! bottom. No form builder: rules are added by agents and by prompting.
use super::screen_kit::{self as kit, KeyOutcome, LineInput};
use crate::app::{MainWindow, refresh};
use crate::backend::{Backend, BackendEvent, ConnState};
use crate::icons::Icon;
use crate::model::{Actor, ApprovalScope, Event, Glyph, Resolution, parse_list, parse_rfc3339};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ lenient wire types

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct MatcherItem {
    pub kind: String,
    pub pattern: String,
}

/// `{"kind":"global"}`, `{"kind":"project","id":"p_.."}`, `{"kind":"session","id":".."}`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ScopeItem {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct OriginItem {
    pub needs_you_id: String,
    pub approval_scope: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct RemovalItem {
    pub requested_by: Actor,
    pub reason: String,
    pub at: String,
    pub needs_you_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct RuleItem {
    pub id: String,
    pub effect: String,
    pub matcher: MatcherItem,
    pub scope: ScopeItem,
    pub expires_at: Option<String>,
    pub added_by: Actor,
    pub added_at: String,
    pub origin: Option<OriginItem>,
    pub fired: u64,
    pub last_fired_at: Option<String>,
    pub removal_request: Option<RemovalItem>,
}

impl RuleItem {
    fn effect_rank(&self) -> u8 {
        match self.effect.as_str() {
            "deny" => 0,
            "ask" => 1,
            _ => 2,
        }
    }

    fn last_secs(&self) -> i64 {
        self.last_fired_at.as_deref().and_then(parse_rfc3339).unwrap_or(0)
    }

    /// Lane order: deny, ask, allow; then most recently fired.
    fn sort_key(&self) -> (u8, i64) {
        (self.effect_rank(), -self.last_secs())
    }
}

/// "Approve for 15 minutes" etc. from an origin's approval scope.
pub fn choice_label(scope: &Value) -> String {
    match serde_json::from_value::<ApprovalScope>(scope.clone()) {
        Ok(ApprovalScope::Once) => "Approve once".into(),
        Ok(ApprovalScope::Minutes { minutes }) if minutes % 60 == 0 && minutes >= 60 => {
            let h = minutes / 60;
            format!("Approve for {h} hour{}", if h == 1 { "" } else { "s" })
        }
        Ok(ApprovalScope::Minutes { minutes }) => format!("Approve for {minutes} minutes"),
        Ok(ApprovalScope::Session) => "Approve for this session".into(),
        Ok(ApprovalScope::Always) => "Always approve".into(),
        Err(_) => "Approved".into(),
    }
}

/// "you approved for 15 min" etc. from a `needs_you.resolved` resolution.
fn outcome_label(res: &Value) -> String {
    match res.get("kind").and_then(|k| k.as_str()) {
        Some("approve") => match res.get("scope").cloned().map(serde_json::from_value::<ApprovalScope>) {
            Some(Ok(ApprovalScope::Once)) => "you approved once".into(),
            Some(Ok(ApprovalScope::Minutes { minutes })) => {
                format!("you approved for {minutes} min")
            }
            Some(Ok(ApprovalScope::Session)) => "you approved for this session".into(),
            Some(Ok(ApprovalScope::Always)) => "you approved always".into(),
            _ => "you approved".into(),
        },
        Some("deny") => "you denied".into(),
        Some("timeout") => "timed out, nobody answered".into(),
        Some("dismiss") => "dismissed".into(),
        Some(k) => k.replace('_', " "),
        None => "answered".into(),
    }
}

// ------------------------------------------------------------------ feed

#[derive(Clone, Debug, Default)]
struct FeedEntry {
    at: String,
    session: Option<String>,
    input: String,
    verdict: String,
    rule_id: String,
    needs_id: Option<String>,
    outcome: Option<String>,
    /// Decided by the defaults table (`policy.decided`): no rule matched.
    default: bool,
    /// An unmatched tool call: midna had no opinion, the agent's own permission flow decided.
    passthrough: bool,
}

/// What a feed row points at: the rule, or "no rule" for defaults-table decisions.
fn feed_target(f: &FeedEntry) -> &'static str {
    if f.passthrough { "no rule · agent's own prompt decides" } else { "no rule · default" }
}

/// Names the screen needs from the main window (projects, terminals, selection).
#[derive(Default)]
pub struct Names {
    projects: HashMap<String, String>,
    sessions: HashMap<String, (String, Option<String>, Glyph)>,
    pub selected: Option<String>,
    pub selected_project: Option<String>,
    first_project: Option<String>,
}

impl Names {
    pub fn from_main(m: &MainWindow) -> Names {
        let selected_project = m.selected_session().and_then(|s| s.project_id.clone());
        Names {
            projects: m.projects.iter().map(|p| (p.id.clone(), p.name.clone())).collect(),
            sessions: m.sessions.iter().map(|s| (s.id.clone(), (s.name.clone(), s.project_id.clone(), s.glyph()))).collect(),
            selected: m.selected.clone(),
            first_project: selected_project.clone().or_else(|| m.projects.first().map(|p| p.id.clone())),
            selected_project,
        }
    }

    pub fn project(&self, id: &str) -> String {
        self.projects.get(id).cloned().unwrap_or_else(|| id.to_string())
    }

    /// "midna › spike", or the bare id when the terminal is gone.
    pub fn term(&self, id: &str) -> String {
        match self.sessions.get(id) {
            Some((name, Some(pid), _)) => format!("{} › {}", self.project(pid), name),
            Some((name, None, _)) => name.clone(),
            None => format!("terminal {id}"),
        }
    }

    pub fn glyph(&self, id: &str) -> Option<Glyph> {
        self.sessions.get(id).map(|s| s.2)
    }

    pub fn current_project_name(&self) -> Option<String> {
        self.first_project.as_ref().map(|p| self.project(p))
    }
}

fn scope_label(n: &Names, s: &ScopeItem) -> String {
    match (s.kind.as_str(), &s.id) {
        ("project", Some(id)) => format!("Project · {}", n.project(id)),
        ("session", Some(id)) => format!("Terminal · {}", n.term(id)),
        _ => "Global".into(),
    }
}

/// Glyph for an actor (agents carry their session).
fn actor_glyph(n: &Names, a: &Actor) -> Option<Glyph> {
    a.session.as_deref().and_then(|s| n.glyph(s))
}

fn actor_name(n: &Names, a: &Actor) -> String {
    match (a.kind.as_str(), &a.session) {
        ("human", _) => "you".into(),
        (_, Some(s)) => n.term(s),
        ("trigger", _) => a.name.clone().map(|x| format!("trigger {x}")).unwrap_or_else(|| "a trigger".into()),
        ("system", _) => "midna".into(),
        _ => a.name.clone().unwrap_or_else(|| "an agent".into()),
    }
}

// ------------------------------------------------------------------ view

const TEST_KINDS: [&str; 5] = ["command", "tool", "path", "cli", "window"];

pub struct RulesView {
    main: WeakEntity<MainWindow>,
    backend: Arc<dyn Backend>,
    pub rules: Vec<RuleItem>,
    loaded: bool,
    error: Option<String>,
    /// Newest first.
    feed: Vec<FeedEntry>,
    /// Entries that arrived while paused.
    held: Vec<FeedEntry>,
    paused: bool,
    paused_at: Option<String>,
    /// Rule highlighted from the feed (or the test result).
    hl: Option<String>,
    /// Rule whose removal request is flashed in the band.
    flash: Option<(String, Instant)>,
    confirm: Option<String>,
    undo: Option<(RuleItem, Instant)>,
    /// Snapshot (effect, pattern) of every rule seen, so the feed can name removed ones.
    known: HashMap<String, (String, String)>,
    test: LineInput,
    test_kind: usize,
    test_result: Option<Value>,
    pending_scroll: Option<String>,
    lane_scroll: [ScrollHandle; 3],
    refetch_scheduled: bool,
    max_seq: u64,
    _tasks: Vec<Task<()>>,
}

impl RulesView {
    pub fn new(main: WeakEntity<MainWindow>, backend: Arc<dyn Backend>, cx: &mut Context<Self>) -> Self {
        // A 1s tick drives expiry countdowns, the "fired in the last 2 minutes" dot and
        // the undo timeout.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(()) = this.update(cx, |v, cx| v.tick(cx)) else {
                    break;
                };
            }
        });
        let mut v = RulesView {
            main,
            backend,
            rules: vec![],
            loaded: false,
            error: None,
            feed: vec![],
            held: vec![],
            paused: false,
            paused_at: None,
            hl: None,
            flash: None,
            confirm: None,
            undo: None,
            known: HashMap::new(),
            test: LineInput::new(cx, false, "git push --force origin main"),
            test_kind: 0,
            test_result: None,
            pending_scroll: None,
            lane_scroll: [ScrollHandle::new(), ScrollHandle::new(), ScrollHandle::new()],
            refetch_scheduled: false,
            max_seq: 0,
            _tasks: vec![tick],
        };
        v.load(cx);
        v
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.undo.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(10)) {
            self.undo = None;
        }
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(3)) {
            self.flash = None;
        }
        let visible = self.main.upgrade().is_some_and(|m| m.read(cx).screen == crate::app::Screen::Rules);
        if visible {
            cx.notify();
        }
    }

    /// Full load: rules plus the recent feed history from the event log.
    fn load(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let rules = backend.call("rule.list", json!({}));
                    let events = backend.call("events.list", json!({"limit": 500, "filter": {"kinds": ["rule.", "needs_you.", "policy.decided"]}}));
                    (rules, events)
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                match res.0 {
                    Ok(r) => v.set_rules(parse_list(&r)),
                    Err(e) => v.error = Some(format!("{e:#}")),
                }
                if let Ok(ev) = res.1 {
                    let mut evs: Vec<Event> = parse_list(&ev);
                    evs.sort_by_key(|e| e.seq);
                    for e in &evs {
                        v.apply_event(e, true);
                    }
                }
                let first = !v.loaded;
                v.loaded = true;
                if first {
                    v.debug_actions(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Dev: `MIDNA_DEBUG_RULES="test:git push --force x;hl:r_1;confirm:r_2;remove:r_3;keep:r_4;flash:r_5"`
    /// drives the screen through its real code paths for screenshots.
    fn debug_actions(&mut self, cx: &mut Context<Self>) {
        let Ok(spec) = crate::dev::var("MIDNA_DEBUG_RULES") else {
            return;
        };
        for step in spec.split(';') {
            let (k, v) = step.split_once(':').unwrap_or((step, ""));
            let v = v.to_string();
            match k {
                "test" => {
                    self.test.set_text(&v, cx);
                    self.run_test(cx);
                }
                "hl" => self.hl = Some(v),
                "confirm" => self.confirm = Some(v),
                "remove" => self.remove_rule(v, cx),
                "keep" => self.answer_request(v, false, cx),
                "approve" => self.answer_request(v, true, cx),
                "flash" => self.flash = Some((v, Instant::now())),
                // After a remove/approve step the undo is set when its call returns: wait for it.
                "undo" => {
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(Duration::from_millis(1500)).await;
                        let _ = this.update(cx, |v, cx| v.undo_remove(cx));
                    })
                    .detach();
                }
                "pause" => self.toggle_pause(cx),
                _ => {}
            }
        }
    }

    fn set_rules(&mut self, rules: Vec<RuleItem>) {
        for r in &rules {
            self.known.insert(r.id.clone(), (r.effect.clone(), r.matcher.pattern.clone()));
        }
        if self.hl.as_ref().is_some_and(|h| !rules.iter().any(|r| &r.id == h)) {
            self.hl = None;
        }
        self.rules = rules;
        self.error = None;
    }

    fn refetch(&mut self, cx: &mut Context<Self>) {
        if self.refetch_scheduled {
            return;
        }
        self.refetch_scheduled = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(60)).await;
            let res = cx.background_executor().spawn(async move { backend.call("rule.list", json!({})) }).await;
            let _ = this.update(cx, |v, cx| {
                v.refetch_scheduled = false;
                if let Ok(r) = res {
                    v.set_rules(parse_list(&r));
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn on_backend_event(&mut self, ev: &BackendEvent, cx: &mut Context<Self>) {
        match ev {
            BackendEvent::Conn(ConnState::Connected) if self.loaded => self.load(cx),
            BackendEvent::Event(e) => {
                if self.apply_event(e, false) {
                    self.refetch(cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// Fold one event into the feed. Returns true when the rule list should be refetched.
    fn apply_event(&mut self, e: &Event, history: bool) -> bool {
        let k = e.kind.as_str();
        if !(k.starts_with("rule.") || k.starts_with("needs_you.") || k == "policy.decided") {
            return false;
        }
        if e.seq != 0 && e.seq <= self.max_seq {
            return false; // already seen (history and live overlap)
        }
        self.max_seq = self.max_seq.max(e.seq);
        let d = &e.data;
        let s = |v: &Value, key: &str| v.get(key).and_then(|x| x.as_str()).unwrap_or("").to_string();
        match k {
            "rule.fired" | "policy.decided" => {
                let action = d.get("action").cloned().unwrap_or(Value::Null);
                let default = k == "policy.decided";
                let entry = FeedEntry {
                    at: e.at.clone(),
                    session: action.get("session").and_then(|x| x.as_str()).map(str::to_string).or(e.session_id.clone()),
                    input: s(&action, "value"),
                    verdict: if default { s(d, "decision") } else { s(d, "effect") },
                    rule_id: if default { String::new() } else { s(d, "rule_id") },
                    needs_id: None,
                    outcome: None,
                    default,
                    passthrough: d.get("passthrough").and_then(|x| x.as_bool()).unwrap_or(false),
                };
                if self.paused && !history {
                    self.held.insert(0, entry);
                } else {
                    self.feed.insert(0, entry);
                    self.feed.truncate(200);
                }
                !history
            }
            "rule.added" | "rule.removed" | "rule.expired" | "rule.restored" => {
                let id = s(d, "id");
                if !id.is_empty() {
                    let m = d.get("matcher").cloned().unwrap_or(Value::Null);
                    self.known.insert(id, (s(d, "effect"), s(&m, "pattern")));
                }
                !history
            }
            "rule.removal_requested" => !history,
            "needs_you.raised" => {
                // Tie an `ask` feed entry to the approval it raised, to show the answer later.
                let rid = d.get("approval").and_then(|a| a.get("matched_rule")).and_then(|x| x.as_str()).unwrap_or("");
                let value = d.get("approval").and_then(|a| a.get("action")).and_then(|a| a.get("value")).and_then(|x| x.as_str()).unwrap_or("");
                if rid.is_empty() && !value.is_empty() {
                    // A defaults-table ask (e.g. `close --force …`): tie it by the action itself.
                    let sid = d.get("session_id").and_then(|x| x.as_str());
                    let nid = s(d, "id");
                    if let Some(f) = self
                        .feed
                        .iter_mut()
                        .chain(self.held.iter_mut())
                        .filter(|f| f.default && f.verdict == "ask" && f.needs_id.is_none() && f.input == value)
                        .find(|f| sid.is_none() || f.session.as_deref() == sid)
                    {
                        f.needs_id = Some(nid);
                        f.outcome = Some("waiting on you".into());
                    }
                }
                if !rid.is_empty() {
                    let sid = d.get("session_id").and_then(|x| x.as_str());
                    let nid = s(d, "id");
                    if let Some(f) = self
                        .feed
                        .iter_mut()
                        .chain(self.held.iter_mut())
                        .filter(|f| f.rule_id == rid && f.verdict == "ask" && f.needs_id.is_none())
                        .find(|f| sid.is_none() || f.session.as_deref() == sid)
                    {
                        f.needs_id = Some(nid);
                        f.outcome = Some("waiting on you".into());
                    }
                }
                false
            }
            "needs_you.resolved" => {
                let nid = s(d, "id");
                if let Some(f) = self.feed.iter_mut().chain(self.held.iter_mut()).find(|f| f.needs_id.as_deref() == Some(nid.as_str())) {
                    f.outcome = Some(outcome_label(d.get("resolution").unwrap_or(&Value::Null)));
                }
                !history && s(d, "kind") == "rule_removal"
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------------ actions

    fn toast(&self, msg: String, cx: &mut Context<Self>) {
        let _ = self.main.update(cx, |m, cx| m.toast(msg, cx));
    }

    fn call(&self, method: &'static str, params: Value, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Value, &mut Context<Self>) + 'static) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
            let _ = this.update(cx, |v, cx| match res {
                Ok(val) => then(v, val, cx),
                Err(e) => {
                    // Undo optimistic edits.
                    v.refetch(cx);
                    v.toast(format!("{method} failed: {e:#}"), cx)
                }
            });
        })
        .detach();
    }

    fn remove_rule(&mut self, id: String, cx: &mut Context<Self>) {
        self.confirm = None;
        let Some(rule) = self.rules.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        self.rules.retain(|r| r.id != id);
        if self.hl.as_deref() == Some(id.as_str()) {
            self.hl = None;
        }
        self.call("rule.remove", json!({"id": id}), cx, move |v, _, cx| {
            v.undo = Some((rule, Instant::now()));
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::RULES | refresh::NEEDS, cx));
            v.refetch(cx);
        });
        cx.notify();
    }

    /// Answer an agent's removal request: Remove (approve) or Keep (deny).
    fn answer_request(&mut self, rule_id: String, remove: bool, cx: &mut Context<Self>) {
        let Some(rule) = self.rules.iter().find(|r| r.id == rule_id).cloned() else {
            return;
        };
        let Some(rr) = rule.removal_request.clone() else {
            return;
        };
        let res = if remove { Resolution::Approve { scope: ApprovalScope::Once } } else { Resolution::Deny };
        if remove {
            self.rules.retain(|r| r.id != rule_id);
        } else if let Some(r) = self.rules.iter_mut().find(|r| r.id == rule_id) {
            r.removal_request = None;
        }
        self.flash = None;
        crate::sounds::play(if remove { "approved" } else { "denied" });
        let params = json!({"id": rr.needs_you_id, "resolution": res});
        self.call("needs_you.resolve", params, cx, move |v, _, cx| {
            if remove {
                v.undo = Some((rule, Instant::now()));
            }
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::RULES | refresh::NEEDS | refresh::SESSIONS, cx));
            v.refetch(cx);
        });
        cx.notify();
    }

    /// Undo a removal: `rule.restore` puts the rule back with its original id, author, origin
    /// and fired count.
    fn undo_remove(&mut self, cx: &mut Context<Self>) {
        let Some((r, _)) = self.undo.take() else {
            return;
        };
        if r.expires_at.as_deref().and_then(parse_rfc3339).is_some_and(|exp| exp <= kit::now_unix()) {
            self.toast("That rule had already expired, so it was not restored.".into(), cx);
            return;
        }
        let mut rule = serde_json::to_value(&r).unwrap_or_default();
        rule["removal_request"] = Value::Null;
        self.call("rule.restore", json!({ "rule": rule }), cx, |v, _, cx| {
            let _ = v.main.update(cx, |m, cx| m.request_refresh(refresh::RULES, cx));
            v.refetch(cx);
        });
        cx.notify();
    }

    fn run_test(&mut self, cx: &mut Context<Self>) {
        let value = self.test.text(cx).trim().to_string();
        if value.is_empty() {
            self.test_result = None;
            cx.notify();
            return;
        }
        let names = self.main.upgrade().map(|m| Names::from_main(m.read(cx))).unwrap_or_default();
        let mut action = json!({"kind": TEST_KINDS[self.test_kind], "value": value});
        if let Some(s) = &names.selected {
            action["session"] = json!(s);
        }
        if let Some(p) = &names.selected_project {
            action["project"] = json!(p);
        }
        self.call("policy.check", json!({"action": action}), cx, |v, val, cx| {
            v.hl = val.get("rule").and_then(|r| r.get("id")).and_then(|x| x.as_str()).map(str::to_string);
            v.pending_scroll = v.hl.clone();
            v.test_result = Some(val);
            cx.notify();
        });
    }

    fn pick_feed(&mut self, rule_id: String, cx: &mut Context<Self>) {
        if rule_id.is_empty() {
            return; // a defaults-table row has no rule to find
        }
        let exists = self.rules.iter().any(|r| r.id == rule_id);
        self.hl = if self.hl.as_deref() == Some(rule_id.as_str()) || !exists { None } else { Some(rule_id) };
        self.pending_scroll = self.hl.clone();
        cx.notify();
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        if self.paused {
            self.paused_at = self.feed.first().map(|f| f.at.clone()).or_else(|| Some(crate::model::now_rfc3339()));
        } else {
            let mut held = std::mem::take(&mut self.held);
            held.append(&mut self.feed);
            self.feed = held;
            self.feed.truncate(200);
            self.paused_at = None;
        }
        cx.notify();
    }
}

/// The ⌘K text for an "Ask: …" rule suggestion.
fn rule_prompt(text: &str) -> String {
    format!("Add a midna rule: {text}")
}

/// "Ask: …" buttons: open ⌘K prefilled with `text` (the palette's free-text fallback starts an
/// agent with it, so the human can edit before sending).
pub fn ask(main: &WeakEntity<MainWindow>, text: String, window: &mut Window, cx: &mut App) {
    let _ = main.update(cx, |m, cx| {
        if m.overlay != crate::app::Overlay::CommandBar {
            m.set_overlay(crate::app::Overlay::CommandBar, window, cx);
        }
        m.palette.query = text.clone();
        m.palette.sel = 0;
        m.palette.armed = None;
        cx.notify();
    });
}

fn suggestions(n: &Names) -> Vec<String> {
    let p = n.current_project_name().unwrap_or_else(|| "this project".into());
    vec![
        format!("deny force pushes in {p}"),
        "always ask before gh pr merge".into(),
        format!("allow cargo test in {p} without asking"),
        "ask before an agent closes another terminal".into(),
    ]
}

// ------------------------------------------------------------------ main-window glue

/// Forward backend events to the Rules view (if it exists).
pub fn on_backend_event(m: &mut MainWindow, ev: &BackendEvent, cx: &mut Context<MainWindow>) {
    if let Some(v) = m.rules_view.clone() {
        v.update(cx, |v, cx| v.on_backend_event(ev, cx));
    }
}

/// The screen element that replaces the terminal pane.
pub fn render(m: &mut MainWindow, _window: &mut Window, cx: &mut Context<MainWindow>) -> AnyElement {
    let view = match &m.rules_view {
        Some(v) => v.clone(),
        None => {
            let weak = cx.entity().downgrade();
            let backend = m.backend.clone();
            let v = cx.new(|cx| RulesView::new(weak, backend, cx));
            m.rules_view = Some(v.clone());
            v
        }
    };
    div().id("screen-rules").key_context("MidnaOverlay").track_focus(&m.overlay_focus).flex().flex_1().min_w_0().h_full().child(view).into_any_element()
}

// ------------------------------------------------------------------ rendering

impl Render for RulesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let names = self.main.upgrade().map(|m| Names::from_main(m.read(cx))).unwrap_or_default();
        let cmdk = self.main.upgrade().map(|m| m.read(cx).key_label("keys.command_bar")).unwrap_or_else(|| "⌘K".into());
        let requests: Vec<RuleItem> = {
            let mut v: Vec<RuleItem> = self.rules.iter().filter(|r| r.removal_request.is_some()).cloned().collect();
            v.sort_by(|a, b| b.removal_request.as_ref().map(|x| x.at.clone()).cmp(&a.removal_request.as_ref().map(|x| x.at.clone())));
            v
        };
        let empty = self.loaded && self.rules.is_empty() && self.error.is_none();

        let body: AnyElement = if empty { self.render_empty(&t, &names, &cmdk, cx).into_any_element() } else { self.render_lanes(&t, &names, cx).into_any_element() };

        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.render_header(&t, &names, &cmdk, requests.len(), cx))
            .when(!requests.is_empty(), |d| d.child(self.render_requests(&t, &names, &requests, cx)))
            .child(self.render_test(&t, &names, window, cx))
            .when_some(self.error.clone(), |d, e| {
                d.child(div().mx(px(18.)).mt(px(12.)).p(px(12.)).rounded(px(10.)).border_1().border_color(t.err).bg(t.panel).child(format!("Couldn't load rules: {e}")))
            })
            .child(body)
            .child(self.render_feed(&t, &names, cx))
            .when_some(self.undo.clone(), |d, (r, _)| d.child(self.render_undo(&t, &r, cx)))
    }
}

impl RulesView {
    fn render_header(&self, t: &Theme, n: &Names, cmdk: &str, proposals: usize, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let suggestion = suggestions(n).remove(0);
        let main = self.main.clone();
        let prompt = rule_prompt(&suggestion);
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
            .child(Icon::Rules.el(16., t.accent))
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child("Rules"))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .text_color(t.dim)
                    .child("by scope, broad to narrow · the narrowest scope wins; within a scope deny beats ask beats allow"),
            )
            .child(div().flex_1())
            .when(proposals > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(6.))
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(13.))
                        .bg(t.need_soft)
                        .text_color(t.need)
                        .font_weight(FontWeight::BOLD)
                        .whitespace_nowrap()
                        .child(kit::dot(t.need, 7.))
                        .child(if proposals == 1 { "1 removal proposed".to_string() } else { format!("{proposals} removals proposed") }),
                )
            })
            .child(
                div()
                    .id("rules-ask")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(px(30.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_color(t.dim)
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.accent))
                    .on_click(move |_, window, cx| ask(&main, prompt.clone(), window, cx))
                    .child(kit::mono(t, cmdk.to_string(), 11.).text_color(t.fg))
                    .child(div().flex().gap(px(4.)).child("Ask:").child(div().text_color(t.fg).child(suggestion))),
            )
            .child(kit::back_btn(t, "rules-back", self.main.clone()))
    }

    fn render_requests(&self, t: &Theme, n: &Names, reqs: &[RuleItem], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut band = div().flex().flex_none().flex_col().gap(px(6.)).px(px(12.)).pt(px(10.)).pb(px(12.)).border_b_1().border_color(t.line).bg(t.need_soft).child(
            div()
                .flex()
                .items_baseline()
                .gap(px(8.))
                .px(px(4.))
                .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).text_color(t.need).child("Removal requests"))
                .child(div().text_size(px(12.)).text_color(t.dim).child(reqs.len().to_string()))
                .child(div().text_size(px(12.)).text_color(t.dim).child("· agents can add rules, only you can remove one")),
        );
        for r in reqs {
            let rr = r.removal_request.clone().unwrap_or_default();
            let flashing = self.flash.as_ref().is_some_and(|(id, _)| id == &r.id);
            let who = actor_name(n, &rr.requested_by);
            let glyph = actor_glyph(n, &rr.requested_by);
            let (rid_a, rid_b) = (r.id.clone(), r.id.clone());
            let row = div()
                .flex()
                .items_center()
                .gap(px(16.))
                .pl(px(12.))
                .pr(px(10.))
                .py(px(8.))
                .rounded(px(8.))
                .bg(t.panel)
                .border_1()
                .border_color(if flashing { t.accent } else { t.need })
                .when(flashing, |d| d.shadow(kit::highlight_ring(t)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .w(px(330.))
                        .flex_shrink(1.)
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .min_w_0()
                                .child(kit::pill(t, &r.effect))
                                .child(kit::mono(t, r.matcher.pattern.clone(), 12.5).flex_1().min_w_0().truncate()),
                        )
                        .child(div().text_size(px(12.)).text_color(t.dim).truncate().child(scope_label(n, &r.scope))),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(12.))
                                .text_color(t.dim)
                                .min_w_0()
                                .children(glyph.map(|g| Icon::from_glyph(g).el(13., t.dim)))
                                .child(div().text_color(t.fg).font_weight(FontWeight::BOLD).whitespace_nowrap().child(who))
                                .child(div().whitespace_nowrap().child(format!("asked {}", kit::ago(Some(&rr.at))))),
                        )
                        .child(div().text_size(px(12.)).truncate().child(format!("“{}”", rr.reason))),
                )
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(6.))
                        .child(
                            kit::btn_danger(t, SharedString::from(format!("req-rm-{}", r.id)), "Remove", 28.)
                                .on_click(cx.listener(move |v, _, _, cx| v.answer_request(rid_a.clone(), true, cx))),
                        )
                        .child(
                            kit::btn(t, SharedString::from(format!("req-keep-{}", r.id)), "Keep")
                                .bg(t.raised)
                                .rounded(px(6.))
                                .px(px(12.))
                                .text_size(px(13.))
                                .on_click(cx.listener(move |v, _, _, cx| v.answer_request(rid_b.clone(), false, cx))),
                        ),
                );
            band = band.child(row);
        }
        band
    }

    fn render_test(&self, t: &Theme, n: &Names, window: &Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let kind = TEST_KINDS[self.test_kind];
        let focus = self.test.focus.clone();
        let as_term = n.selected.as_ref().map(|s| format!("as {}", n.term(s))).unwrap_or_else(|| "with no terminal".into());
        let field = self
            .test
            .render(t, "rules-test-field", window)
            .h(px(28.))
            .flex_1()
            .min_w(px(160.))
            .max_w(px(460.))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| focus.focus(window, cx))
            .on_key_down(cx.listener(|v, ev: &KeyDownEvent, _, cx| match v.test.on_key(ev, cx) {
                KeyOutcome::Submit => {
                    v.run_test(cx);
                    cx.stop_propagation();
                }
                KeyOutcome::Cancel if !v.test.is_empty(cx) || v.test_result.is_some() => {
                    v.test.clear(cx);
                    v.test_result = None;
                    cx.stop_propagation();
                    cx.notify();
                }
                _ => {}
            }));
        let result = self.test_result.as_ref().map(|r| {
            let decision = r.get("decision").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let source = r.get("source").and_then(|x| x.as_str()).unwrap_or("");
            let rule = r.get("rule").cloned().and_then(|x| serde_json::from_value::<RuleItem>(x).ok());
            let why = match &rule {
                Some(rule) => format!("{} · {}", rule.matcher.pattern, scope_label(n, &rule.scope)),
                None if source == "default" => "no rule matched · policy.default".into(),
                None => source.to_string(),
            };
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .min_w_0()
                .child(div().text_color(t.dim).child("→"))
                .child(kit::pill(t, &decision))
                .child(kit::mono(t, why, 12.).text_color(if rule.is_some() { t.fg } else { t.dim }).truncate())
        });
        let trace: Vec<(bool, String)> = self
            .test_result
            .as_ref()
            .and_then(|r| r.get("trace"))
            .and_then(|x| x.as_array())
            .map(|a| a.iter().map(|e| (e.get("matched").and_then(|m| m.as_bool()).unwrap_or(false), e.get("reason").and_then(|m| m.as_str()).unwrap_or("").to_string())).collect())
            .unwrap_or_default();
        let matched: Vec<&(bool, String)> = trace.iter().filter(|(m, _)| *m).collect();
        let others = trace.len() - matched.len();
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(4.))
            .px(px(18.))
            .py(px(8.))
            .border_b_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(12.))
                    .child(kit::cap(t, "Test"))
                    .child(kit::chip(t, format!("{kind} ▾")).id("rules-test-kind").cursor_pointer().hover(|s| s.border_color(t.accent)).on_click(cx.listener(|v, _, _, cx| {
                        v.test_kind = (v.test_kind + 1) % TEST_KINDS.len();
                        v.test_result = None;
                        cx.notify();
                    })))
                    .child(field)
                    .child(div().text_color(t.dim).whitespace_nowrap().child(as_term))
                    .children(result),
            )
            .when(!trace.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .pl(px(48.))
                        .text_size(px(11.5))
                        .text_color(t.dim)
                        .font_family(t.mono_font.clone())
                        .children(matched.iter().map(|(_, reason)| div().truncate().child(format!("✓ {reason}"))))
                        .child(div().child(if others == 0 {
                            "no other rules".to_string()
                        } else {
                            format!("· {others} other rule{} didn't match", if others == 1 { "" } else { "s" })
                        })),
                )
            })
    }

    fn render_lanes(&mut self, t: &Theme, n: &Names, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut global: Vec<&RuleItem> = self.rules.iter().filter(|r| r.scope.kind != "project" && r.scope.kind != "session").collect();
        let mut project: Vec<&RuleItem> = self.rules.iter().filter(|r| r.scope.kind == "project").collect();
        let mut term: Vec<&RuleItem> = self.rules.iter().filter(|r| r.scope.kind == "session").collect();
        global.sort_by_key(|r| r.sort_key());
        project.sort_by_key(|r| r.sort_key());
        term.sort_by_key(|r| r.sort_key());

        // Group project/terminal lanes by their scope target, in first-seen order.
        let group = |rs: &[&RuleItem], label: &dyn Fn(&str) -> String| -> Vec<(String, Vec<RuleItem>)> {
            let mut out: Vec<(String, Vec<RuleItem>)> = vec![];
            for r in rs {
                let key = label(r.scope.id.as_deref().unwrap_or(""));
                match out.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, v)) => v.push((*r).clone()),
                    None => out.push((key, vec![(*r).clone()])),
                }
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        };
        let lanes: [(&str, &str, usize, Vec<(String, Vec<RuleItem>)>); 3] = [
            ("Global", "every terminal", global.len(), vec![(String::new(), global.iter().map(|r| (*r).clone()).collect())]),
            ("Project", "one project’s terminals", project.len(), group(&project, &|id| n.project(id))),
            ("Terminal", "one terminal, often time-boxed", term.len(), group(&term, &|id| n.term(id))),
        ];
        let mut row = div().flex().flex_1().min_h_0().bg(t.bg);
        for (li, (title, hint, count, groups)) in lanes.into_iter().enumerate() {
            let mut body = div()
                .id(SharedString::from(format!("lane-{li}")))
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .gap(px(6.))
                .px(px(12.))
                .pb(px(14.))
                .overflow_y_scroll()
                .track_scroll(&self.lane_scroll[li]);
            let mut ix = 0usize;
            if count == 0 {
                body = body.child(div().px(px(4.)).pt(px(4.)).text_size(px(12.)).text_color(t.dim).child(match li {
                    0 => "No global rules.",
                    1 => "No project rules.",
                    _ => "No terminal rules. “Approve for 15 min” and “this session” land here.",
                }));
            }
            for (label, cards) in groups {
                if !label.is_empty() {
                    body = body.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .px(px(4.))
                            .pt(px(8.))
                            .pb(px(2.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.dim)
                            .truncate()
                            .child(label.to_uppercase()),
                    );
                    ix += 1;
                }
                for r in cards {
                    if self.pending_scroll.as_deref() == Some(r.id.as_str()) {
                        self.lane_scroll[li].scroll_to_item(ix);
                        self.pending_scroll = None;
                    }
                    body = body.child(self.render_card(t, n, &r, cx));
                    ix += 1;
                }
            }
            row = row.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(li < 2, |d| d.border_r_1().border_color(t.line))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_baseline()
                            .gap(px(8.))
                            .px(px(16.))
                            .pt(px(14.))
                            .pb(px(8.))
                            .child(div().text_size(px(13.)).font_weight(FontWeight::BOLD).child(title))
                            .child(div().text_size(px(12.)).text_color(t.dim).child(count.to_string()))
                            .child(div().flex_1())
                            .child(div().text_size(px(11.5)).text_color(t.dim).truncate().child(hint)),
                    )
                    .child(body),
            );
        }
        row
    }

    fn render_card(&self, t: &Theme, n: &Names, r: &RuleItem, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let now = kit::now_unix();
        let hot = r.last_fired_at.as_deref().and_then(parse_rfc3339).is_some_and(|s| now - s <= 120);
        let is_hl = self.hl.as_deref() == Some(r.id.as_str());
        let dimmed = self.hl.is_some() && !is_hl;
        let by_human = r.added_by.kind == "human";
        let by_short = match (&r.origin, by_human) {
            (Some(_), _) => "you via approval".to_string(),
            (None, true) => format!("you · {}", kit::day_label(&r.added_at)),
            (None, false) => format!("{} · {}", actor_name(n, &r.added_by), kit::day_label(&r.added_at)),
        };
        let glyph = if by_human { None } else { actor_glyph(n, &r.added_by) };
        let expiry = r.expires_at.as_deref().and_then(parse_rfc3339).map(|exp| {
            let added = parse_rfc3339(&r.added_at).unwrap_or(exp - 1);
            let total = (exp - added).max(1) as f32;
            let left = exp - now;
            ((left as f32 / total).clamp(0., 1.), kit::left(left))
        });
        let session_scoped_forever = r.expires_at.is_none() && r.origin.as_ref().is_some_and(|o| o.approval_scope.get("kind").and_then(|k| k.as_str()) == Some("session"));
        let origin = r.origin.as_ref().map(|o| {
            let mut s = format!("↳ from approval · {}", choice_label(&o.approval_scope));
            if let ("session", Some(id)) = (r.scope.kind.as_str(), &r.scope.id) {
                s.push_str(&format!(" · {}", n.term(id)));
            }
            s
        });
        let pending = r.removal_request.is_some();
        let confirming = self.confirm.as_deref() == Some(r.id.as_str());
        let confirm_q = match r.scope.kind.as_str() {
            "project" => format!("Remove for every {} terminal?", r.scope.id.as_deref().map(|p| n.project(p)).unwrap_or_default()),
            "session" => "Remove for this terminal?".to_string(),
            _ => "Remove for every terminal?".to_string(),
        };
        let (id1, id2, id3) = (r.id.clone(), r.id.clone(), r.id.clone());
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(6.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(9.))
            .bg(t.panel)
            .border_1()
            .border_color(if is_hl { t.accent } else { t.line })
            .when(is_hl, |d| d.shadow(kit::highlight_ring(t)))
            .when(dimmed, |d| d.opacity(0.55))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(kit::pill(t, &r.effect))
                    .child(kit::mono(t, r.matcher.pattern.clone(), 12.5).flex_1().min_w_0().truncate())
                    .when(hot, |d| d.child(kit::dot(t.accent, 7.))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(t.dim)
                    .child(r.matcher.kind.clone())
                    .child("·")
                    .children(glyph.map(|g| Icon::from_glyph(g).el(13., t.dim)))
                    .child(by_short)
                    .child("·")
                    .child(
                        div()
                            .flex()
                            .child(div().text_color(t.fg).font_weight(FontWeight::BOLD).child(r.fired.to_string()))
                            .child(format!("× · {}", kit::ago(r.last_fired_at.as_deref()))),
                    ),
            )
            .when_some(expiry, |d, (pct, text)| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .child(div().flex_1().h(px(3.)).rounded(px(2.)).bg(t.line).child(div().h(px(3.)).rounded(px(2.)).bg(t.accent).w(relative(pct))))
                        .child(div().text_color(t.fg).child(text)),
                )
            })
            .when(session_scoped_forever, |d| {
                d.child(div().flex().items_center().gap(px(8.)).text_size(px(12.)).child(div().flex_1().h(px(3.)).rounded(px(2.)).bg(t.accent)).child(
                    div().text_color(t.fg).child(match &r.scope.id {
                        Some(id) if r.scope.kind == "session" => format!("until {} exits", n.term(id).rsplit(" › ").next().unwrap_or("")),
                        _ => "for this session".into(),
                    }),
                ))
            })
            .when_some(origin, |d, o| d.child(div().text_size(px(12.)).text_color(t.accent).truncate().child(o)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .mt(px(-2.))
                    .text_size(px(12.))
                    .when(pending, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("goreq-{}", r.id)))
                                .text_color(t.need)
                                .cursor_pointer()
                                .hover(|s| s.underline())
                                .on_click(cx.listener(move |v, _, _, cx| {
                                    v.flash = Some((id1.clone(), Instant::now()));
                                    cx.notify();
                                }))
                                .child("removal requested ↑"),
                        )
                    })
                    .when(!pending && confirming, |d| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().text_color(t.dim).child(confirm_q))
                                .child(
                                    kit::btn_danger(t, SharedString::from(format!("rm-{}", r.id)), "Remove", 24.)
                                        .on_click(cx.listener(move |v, _, _, cx| v.remove_rule(id2.clone(), cx))),
                                )
                                .child(kit::btn(t, SharedString::from(format!("cancel-{}", r.id)), "Cancel").h(px(24.)).px(px(8.)).rounded(px(6.)).on_click(cx.listener(
                                    |v, _, _, cx| {
                                        v.confirm = None;
                                        cx.notify();
                                    },
                                ))),
                        )
                    })
                    .when(!pending && !confirming, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("ask-rm-{}", r.id)))
                                .h(px(22.))
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .rounded(px(5.))
                                .text_color(t.dim)
                                .cursor_pointer()
                                .hover(|s| s.bg(t.raised).text_color(t.fg))
                                .on_click(cx.listener(move |v, _, _, cx| {
                                    v.confirm = Some(id3.clone());
                                    cx.notify();
                                }))
                                .child("Remove"),
                        )
                    }),
            )
    }

    fn render_empty(&self, t: &Theme, n: &Names, cmdk: &str, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let mut list = div().flex().flex_col().gap(px(6.));
        for (i, s) in suggestions(n).into_iter().enumerate() {
            let main = self.main.clone();
            let prompt = rule_prompt(&s);
            list = list.child(
                div()
                    .id(SharedString::from(format!("rule-sugg-{i}")))
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
        div().flex().flex_1().min_h_0().items_center().justify_center().p(px(24.)).bg(t.bg).child(
            div()
                .w(px(560.))
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(Icon::Rules.el(28., t.accent))
                .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child("No rules yet"))
                .child(div().text_color(t.dim).child(
                    "Rules decide what agents may run without asking. Agents add them as they work, and “Always” or “for 15 min” approvals become rules here. Describe one and an agent adds it; only you can remove one.",
                ))
                .child(list)
                .child(div().text_size(px(12.)).text_color(t.dim).child(format!("Or press {cmdk} and type what should be allowed, asked or denied."))),
        )
    }

    fn render_feed(&self, t: &Theme, n: &Names, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let sub = if self.paused {
            format!("paused · {} new since {}", self.held.len(), self.paused_at.as_deref().map(kit::clock).unwrap_or_default())
        } else {
            "live from rule.fired and policy.decided · click a row to find its rule".to_string()
        };
        let mut list = div().id("rules-feed").flex().flex_col().h(px(196.)).overflow_y_scroll().text_size(px(12.));
        if self.feed.is_empty() {
            list = list.child(
                div()
                    .px(px(16.))
                    .py(px(10.))
                    .text_color(t.dim)
                    .child("Nothing has been decided yet. Each time a rule (or the default policy) decides an agent’s action, it shows up here."),
            );
        }
        for (i, f) in self.feed.iter().enumerate() {
            let rule = self.rules.iter().find(|r| r.id == f.rule_id);
            let (rule_text, gone) = match (rule, self.known.get(&f.rule_id)) {
                _ if f.default => (feed_target(f).to_string(), true),
                (Some(r), _) => (format!("{} · {}", r.effect, r.matcher.pattern), false),
                (None, Some((e, p))) => (format!("{e} · {p} (removed)"), true),
                (None, None) => (format!("{} (removed)", f.rule_id), true),
            };
            let on = self.hl.as_deref() == Some(f.rule_id.as_str());
            let outcome = f.outcome.clone().unwrap_or_else(|| match f.verdict.as_str() {
                _ if f.passthrough => "unsupervised".into(),
                "deny" => "blocked".into(),
                "allow" => "ran".into(),
                _ => "asked".into(),
            });
            let rid = f.rule_id.clone();
            let term = f.session.as_deref().map(|s| n.term(s)).unwrap_or_else(|| "outside midna".into());
            let glyph = f.session.as_deref().and_then(|s| n.glyph(s));
            list = list.child(
                div()
                    .id(("feed-row", i))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(6.))
                    .border_b_1()
                    .border_color(t.line)
                    .cursor_pointer()
                    .hover(|s| s.bg(t.panel))
                    .when(on, |d| d.bg(t.accent_soft).border_l_2().border_color(t.accent).pl(px(14.)))
                    .on_click(cx.listener(move |v, _, _, cx| v.pick_feed(rid.clone(), cx)))
                    .child(kit::mono(t, kit::clock(&f.at), 12.).w(px(70.)).flex_none().text_color(t.dim))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(6.))
                            .w(px(190.))
                            .min_w_0()
                            .children(glyph.map(|g| Icon::from_glyph(g).el(13., t.dim)))
                            .child(div().truncate().child(term)),
                    )
                    .child(kit::mono(t, f.input.clone(), 12.).flex_1().min_w_0().truncate())
                    .child(div().w(px(62.)).flex_none().child(kit::pill(t, &f.verdict)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .w(px(300.))
                            .flex_shrink(1.)
                            .min_w_0()
                            .text_color(t.dim)
                            .child("→")
                            .child(kit::mono(t, rule_text, 12.).truncate().text_color(if gone { t.dim } else { t.fg })),
                    )
                    .child(div().w(px(170.)).flex_shrink(1.).min_w_0().truncate().text_color(t.dim).child(outcome)),
            );
        }
        div()
            .flex()
            .flex_none()
            .flex_col()
            .border_t_1()
            .border_color(t.line)
            .bg(t.term)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(36.))
                    .pl(px(16.))
                    .pr(px(12.))
                    .border_b_1()
                    .border_color(t.line)
                    .child(kit::dot(if self.paused { t.dim } else { t.ok }, 7.))
                    .child(div().text_size(px(12.)).font_weight(FontWeight::BOLD).child("Last fired"))
                    .child(div().text_size(px(12.)).text_color(t.dim).truncate().child(sub))
                    .child(div().flex_1())
                    .when(self.hl.is_some(), |d| {
                        d.child(kit::btn(t, "feed-clear", "Clear highlight").h(px(24.)).px(px(8.)).rounded(px(6.)).text_color(t.dim).on_click(cx.listener(|v, _, _, cx| {
                            v.hl = None;
                            cx.notify();
                        })))
                    })
                    .child(
                        kit::btn(t, "feed-pause", if self.paused { "Resume" } else { "Pause" }).h(px(24.)).rounded(px(6.)).on_click(cx.listener(|v, _, _, cx| v.toggle_pause(cx))),
                    ),
            )
            .child(list)
    }

    fn render_undo(&self, t: &Theme, r: &RuleItem, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        div().absolute().bottom(px(212.)).left_0().right_0().flex().justify_center().child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .pl(px(14.))
                .pr(px(10.))
                .py(px(9.))
                .rounded(px(10.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_size(px(12.5))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
                .child(format!("Removed {} ", r.effect))
                .child(kit::mono(t, r.matcher.pattern.clone(), 12.))
                .child(kit::btn(t, "rules-undo", "Undo").on_click(cx.listener(|v, _, _, cx| v.undo_remove(cx))))
                .child(
                    div()
                        .id("rules-undo-x")
                        .text_color(t.dim)
                        .cursor_pointer()
                        .on_click(cx.listener(|v, _, _, cx| {
                            v.undo = None;
                            cx.notify();
                        }))
                        .child("✕"),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{RuleItem, ScopeItem, choice_label, outcome_label};
    use crate::model::parse_list;
    use serde_json::json;

    #[test]
    fn reads_proto_rule() {
        use midna_proto as p;
        let mut r = p::Rule {
            id: "r_1".into(),
            effect: p::Effect::Deny,
            matcher: p::Matcher { kind: p::ActionKind::Command, pattern: "git push --force*".into() },
            scope: p::RuleScope::Project("p_aaaaaa".into()),
            expires_at: Some("2026-10-03T10:15:00Z".into()),
            added_by: p::Actor::agent(Some("abcd1234".into())),
            added_at: "2026-10-03T10:00:00Z".into(),
            origin: Some(p::RuleOrigin { needs_you_id: "n_1".into(), approval_scope: p::ApprovalScope::Minutes { minutes: 15 } }),
            fired: 3,
            last_fired_at: None,
            removal_request: None,
        };
        r.removal_request = Some(p::RemovalRequest {
            requested_by: p::Actor::agent(Some("abcd1234".into())),
            reason: "noisy".into(),
            at: "2026-10-03T10:05:00Z".into(),
            needs_you_id: "n_2".into(),
        });
        let got: Vec<RuleItem> = parse_list(&json!([r]));
        assert_eq!(got[0].scope, ScopeItem { kind: "project".into(), id: Some("p_aaaaaa".into()) });
        assert_eq!(got[0].effect, "deny");
        assert_eq!(choice_label(&got[0].origin.as_ref().unwrap().approval_scope), "Approve for 15 minutes");
        assert_eq!(got[0].removal_request.as_ref().unwrap().needs_you_id, "n_2");
        // Undo re-adds with the daemon's scope wire form.
        let back: p::RuleScope = serde_json::from_value(serde_json::to_value(&got[0].scope).unwrap()).unwrap();
        assert_eq!(back, p::RuleScope::Project("p_aaaaaa".into()));
        let g: p::RuleScope = serde_json::from_value(serde_json::to_value(ScopeItem { kind: "global".into(), id: None }).unwrap()).unwrap();
        assert_eq!(g, p::RuleScope::Global);
    }

    #[test]
    fn feed_ties_ask_to_answer() {
        assert_eq!(outcome_label(&json!({"kind": "approve", "scope": {"kind": "minutes", "minutes": 15}})), "you approved for 15 min");
        assert_eq!(outcome_label(&json!({"kind": "deny"})), "you denied");
        assert_eq!(outcome_label(&json!({"kind": "timeout"})), "timed out, nobody answered");
    }
}
