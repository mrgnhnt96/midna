//! In-memory stand-in for midnad (`MIDNA_BACKEND=fake`).
//!
//! It answers the same JSON-RPC method names the GUI uses with the sample data from
//! docs/design/Main.dc.html, emits events like the daemon would, and (with the
//! `fake-engine` feature) runs a real PTY + libghostty engine per session so the terminal
//! pane, keyboard input and resize can be exercised without the daemon.
//!
//! `MIDNA_FAKE_SETTINGS="theme=light,density=compact"` overrides initial settings.
use super::{AttachRequest, Backend, BackendEvent, ConnState, TermStream};
use crate::frame::FrameSink;
use crate::model::*;
use anyhow::{anyhow, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct FakeBackend {
    st: Arc<Mutex<FakeState>>,
}

struct FakeState {
    projects: Vec<Project>,
    sessions: Vec<Session>,
    needs: Vec<NeedsYou>,
    settings: Vec<SettingEntry>,
    rules: Vec<Value>,
    seq: u64,
    subs: Vec<async_channel::Sender<BackendEvent>>,
    #[cfg(feature = "fake-engine")]
    terms: HashMap<String, Arc<pty::FakeTerm>>,
    scripts: HashMap<String, String>,
    next_id: u64,
}

fn ago(mins: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    rfc3339_from_unix(now - mins * 60)
}

/// Local triggers for the Triggers screen: the built-in prompt-blocked status, an
/// auto-compact one and a schedule.
fn fake_triggers() -> Vec<midna_proto::Trigger> {
    use midna_proto as p;
    let t = |id: &str, name: &str, filter: p::TriggerFilter, action: p::TriggerAction, builtin: Option<&str>, fired: u64| p::Trigger {
        id: id.into(),
        name: name.into(),
        source: p::TriggerSource::Local,
        event: p::kinds::AGENT_PROMPT_BLOCKED.into(),
        filter,
        action,
        enabled: true,
        state: p::TriggerState::Active,
        secret_set: false,
        created_by: if builtin.is_some() { p::Actor::human() } else { p::Actor::agent(Some("a1f00007".into())) },
        created_at: ago(60 * 24 * 3),
        last_fired_at: (fired > 0).then(|| ago(2)),
        fired,
        last_fired_summary: (fired > 0).then(|| "triggers".into()),
        enabled_at: Some(ago(60 * 24 * 3)),
        secret_set_at: None,
        secret_store: None,
        github_hook_id: None,
        session_name_template: None,
        cooldown_secs: None,
        builtin: builtin.map(Into::into),
    };
    vec![
        t(
            "t_lblock",
            "Prompt blocked status",
            p::TriggerFilter::default(),
            p::TriggerAction::SetStatus { label: "Prompt blocked".into(), color: "amber".into(), icon: None, base: p::StatusState::NeedsYou, clear_on: p::StatusClear::Prompt },
            Some("prompt_blocked_status"),
            3,
        ),
        t(
            "t_lcompact",
            "Compact and resend",
            p::TriggerFilter { fields: [("message".to_string(), "*Compact first*".to_string())].into(), ..Default::default() },
            p::TriggerAction::SendToSession {
                steps: vec![p::SendStep { text: "/compact".into(), enter: true }, p::SendStep { text: "{{last_prompt}}".into(), enter: true }],
            },
            None,
            0,
        ),
        p::Trigger {
            event: "schedule".into(),
            ..t(
                "t_lstandup",
                "Morning standup",
                p::TriggerFilter { cron: Some("0 9 * * mon-fri".into()), ..Default::default() },
                p::TriggerAction::Notify { title: "Standup in 5".into(), body: "{{local_time}}".into(), sound: true, category: None, open: None, id: None },
                None,
                0,
            )
        },
    ]
}

/// The settings catalog the GUI reads (subset of the daemon's; same keys).
pub fn settings_catalog() -> Vec<SettingEntry> {
    let e = |key: &str, default: Value, description: &str, human_only: bool| SettingEntry {
        key: key.into(),
        value: default.clone(),
        default,
        description: description.into(),
        human_only,
        cli: String::new(),
    };
    let mut v = vec![
        e("theme", json!("dark"), "system | a theme id (dark = twilight, light = daylight)", false),
        e("theme.dark", json!("twilight"), "theme in macOS dark mode", false),
        e("theme.light", json!("daylight"), "theme in macOS light mode", false),
        e("theme.colors", json!([]), "single-color overrides", false),
        e("density", json!("comfortable"), "comfortable | compact", false),
        e("ui.header.script", json!("github"), "github | github+agent | worktree+branch | none | custom path", false),
        e("ui.row.script", json!("diff"), "diff | worktree+diff | worktree+branch | none | custom path", false),
        e("ui.status.script", json!("worktree+branch"), "worktree+branch | branch | github | none | custom path", false),
        e("ui.status.looks", json!([]), "restyle built-in statuses", false),
        e("ui.header.buttons", json!(midna_proto::settings::DEFAULT_HEADER_BUTTONS), "header toolbar buttons", false),
        e("ui.status.items", json!(midna_proto::settings::setting("ui.status.items").map(|s| s.default.to_json()).unwrap_or_default()), "status bar items", false),
        e("webhooks.path", json!("tailscale_funnel"), "tailscale_funnel | self_relay | midna_relay | off", true),
        e("policy.default", json!("ask"), "default decision when no rule matches", true),
        e("kass.auto_send", json!(false), "send dictated text when Kass finishes", false),
    ];
    // every shortcut, so the Shortcuts tab and tooltips match the daemon
    let keys = midna_proto::settings::SETTINGS.iter().filter(|s| matches!(s.ty, midna_proto::settings::SettingKind::Keybinding));
    v.extend(keys.map(|s| e(s.key, s.default.to_json(), s.description, false)));
    for s in &mut v {
        s.cli = format!("midna settings set {} <value>", s.key);
    }
    v
}

// Dark-theme token colors for the sample transcripts.
const DIM: &str = "\x1b[38;2;154;162;179m";
const ACC: &str = "\x1b[38;2;183;154;232m";
const OK: &str = "\x1b[38;2;76;195;138m";
const ERR: &str = "\x1b[38;2;242;102;92m";
const NEED: &str = "\x1b[38;2;242;169;59m";
const RST: &str = "\x1b[0m";

fn transcript(lines: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (c, t) in lines {
        s.push_str(c);
        s.push_str(t);
        s.push_str(RST);
        s.push_str("\r\n");
    }
    s
}

impl FakeBackend {
    pub fn new() -> Self {
        let p = |id: &str, name: &str, order: u32| Project { id: id.into(), name: name.into(), path: format!("~/Development/{name}"), order, ..Default::default() };
        let projects = vec![p("p_zonai1", "zonai", 0), p("p_drops1", "drops-app", 1), p("p_midna1", "midna", 2)];
        let git = |branch: &str, add: u32, rem: u32, files: u32, pr: Option<(u64, Checks)>| {
            Some(GitInfo {
                branch: branch.into(),
                added: add,
                removed: rem,
                files,
                pr: pr.map(|(number, checks)| PrInfo { number, url: format!("https://github.com/acme/zonai/pull/{number}"), checks, failing_count: 0 }),
                ..Default::default()
            })
        };
        let s =
            |id: &str, project: &str, name: &str, kind: SessionKind, agent: Option<AgentKind>, state: StatusState, reason: Option<&str>, mins: i64, g: Option<GitInfo>| Session {
                id: id.into(),
                project_id: Some(project.into()),
                name: name.into(),
                kind,
                agent,
                cwd: "~".into(),
                status: Status { state, reason: reason.map(Into::into), exit_code: None, since: Some(ago(mins)) },
                created_at: ago(60),
                last_activity_at: ago(mins),
                git: g,
                ..Default::default()
            };
        let sessions = {
            use AgentKind::*;
            use SessionKind::*;
            use StatusState::*;
            let qm = |id: &str, text: &str, when: midna_proto::SendWhen, by: midna_proto::Actor| midna_proto::QueuedMessage {
                id: id.into(),
                text: text.into(),
                enter: true,
                images: vec![],
                when,
                by,
                queued_at: ago(2),
                state: midna_proto::QueueState::Waiting,
                error: None,
                trigger_id: None,
                waiting_for: vec![],
            };
            let trigger = midna_proto::Actor { kind: midna_proto::ActorKind::Trigger, session: None, name: Some("After compact, write notes".into()) };
            vec![
                Session {
                    queue: vec![
                        qm("q_aaaa01", "Now run the full workspace tests and fix anything the refresh change broke", midna_proto::SendWhen::Idle, midna_proto::Actor::human()),
                        qm("q_aaaa02", "/compact", midna_proto::SendWhen::Idle, midna_proto::Actor::human()),
                        qm("q_aaaa03", "Update docs/DECISIONS.md with what changed and why", midna_proto::SendWhen::IdleFor { minutes: 5 }, trigger),
                    ],
                    agent_info: Some(fake_subagents()),
                    ..s("a1f00001", "p_zonai1", "api", Agent, Some(Claude), Working, None, 4, git("feat/auth", 48, 12, 6, Some((231, Checks::Pending))))
                },
                s(
                    "a1f00002",
                    "p_zonai1",
                    "migrate",
                    Agent,
                    Some(Codex),
                    NeedsYou,
                    Some("Approve db:migrate on staging?"),
                    4,
                    git("feat/auth", 96, 0, 3, Some((231, Checks::Pending))),
                ),
                s("a1f00003", "p_zonai1", "tests", Monitor, None, Done, None, 1, git("feat/auth", 0, 0, 0, Some((231, Checks::Pending)))),
                s("a1f00004", "p_zonai1", "zsh", Shell, None, Idle, None, 30, git("feat/auth", 0, 0, 0, Some((231, Checks::Pending)))),
                s("a1f00005", "p_drops1", "golden", Agent, Some(Claude), NeedsYou, Some("Asked a question"), 12, git("main", 0, 0, 0, None)),
                s("a1f00006", "p_drops1", "flutter build", Shell, None, Failed, Some("exit 1"), 9, git("main", 0, 0, 0, None)),
                s("a1f00007", "p_midna1", "spike", Agent, Some(Claude), Working, None, 0, git("main", 210, 41, 9, None)),
                // A local trigger's custom status (the built-in prompt-blocked one).
                Session {
                    custom_status: Some(CustomStatus {
                        label: "Prompt blocked".into(),
                        color: "amber".into(),
                        base: NeedsYou,
                        clear_on: "prompt".into(),
                        trigger_id: Some("t_lblock".into()),
                        detail: Some("Context is full. Compact first, then resend.".into()),
                        since: Some(ago(2)),
                        ..Default::default()
                    }),
                    ..s("a1f00008", "p_midna1", "triggers", Agent, Some(Claude), NeedsYou, Some("Prompt blocked"), 2, git("feat/local-triggers", 64, 8, 4, None).map(|g| GitInfo { worktree: Some("midna-triggers".into()), ..g }))
                },
                // Background terminals (`midna open --background`): the sidebar's folded group.
                Session { background: true, ..s("a1f00009", "p_zonai1", "dev server", Monitor, None, Working, None, 1, None) },
                Session { background: true, ..s("a1f0000a", "p_midna1", "cargo watch", Shell, None, Idle, None, 6, None) },
            ]
        };
        let needs = vec![
            NeedsYou {
                id: "n_migrat".into(),
                session_id: Some("a1f00002".into()),
                project_id: Some("p_zonai1".into()),
                kind: NeedsYouKind::Approval,
                title: "Codex wants to run this on staging".into(),
                detail: "asked by your rule “ask when env = staging”".into(),
                asked_by: Actor { kind: "agent".into(), session: Some("a1f00002".into()), name: Some("codex".into()) },
                created_at: ago(4),
                approval: Some(ApprovalRequest {
                    action: PolicyAction {
                        kind: "command".into(),
                        value: "pnpm db:migrate --env staging".into(),
                        session: Some("a1f00002".into()),
                        project: Some("p_zonai1".into()),
                    },
                    matched_rule: Some("r_staging".into()),
                }),
                ..Default::default()
            },
            NeedsYou {
                id: "n_golden".into(),
                session_id: Some("a1f00005".into()),
                project_id: Some("p_drops1".into()),
                kind: NeedsYouKind::Blocked,
                title: "Asked a question".into(),
                detail: "Which should I take?".into(),
                asked_by: Actor { kind: "agent".into(), session: Some("a1f00005".into()), name: Some("claude".into()) },
                created_at: ago(12),
                question: Some(fake_question()),
                ..Default::default()
            },
            NeedsYou {
                id: "n_blockd".into(),
                session_id: Some("a1f00008".into()),
                project_id: Some("p_midna1".into()),
                kind: NeedsYouKind::Blocked,
                title: "Prompt blocked".into(),
                detail: "Context is full. Compact first, then resend.".into(),
                asked_by: Actor { kind: "agent".into(), session: Some("a1f00008".into()), name: Some("claude".into()) },
                created_at: ago(2),
                ..Default::default()
            },
        ];
        let mut settings = settings_catalog();
        if let Ok(over) = crate::dev::var("MIDNA_FAKE_SETTINGS") {
            for kv in over.split(',') {
                if let Some((k, v)) = kv.split_once('=')
                    && let Some(s) = settings.iter_mut().find(|s| s.key == k.trim())
                {
                    // lists use `|`: ui.status.items=daemon|spacer|script
                    s.value = if s.value.is_array() { json!(v.split('|').map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>()) } else { json!(v.trim()) };
                }
            }
        }
        let scripts: HashMap<String, String> = [
            (
                "a1f00001",
                transcript(&[
                    (DIM, "❯ claude"),
                    ("", ""),
                    (ACC, "⏺ Reading src/auth/session.rs"),
                    (ACC, "⏺ Update(src/auth/session.rs)"),
                    (OK, "    + pub fn refresh(&mut self) -> Result<Token> {"),
                    (OK, "    +     let next = self.issuer.renew(&self.token)?;"),
                    (OK, "    +     self.token = next.clone();"),
                    (OK, "    +     Ok(next)"),
                    (OK, "    + }"),
                    (ERR, "    - pub fn refresh(&self) -> Token"),
                    ("", ""),
                    (ACC, "⏺ Bash(cargo test auth::)"),
                    (OK, "    test result: ok. 18 passed; 0 failed"),
                    ("", ""),
                    (DIM, "✻ Wiring refresh into the middleware… (4m 12s)"),
                    ("", ""),
                    (DIM, &"─".repeat(60)),
                    ("", "❯ "),
                    (DIM, &"─".repeat(60)),
                    (DIM, "  ⏵⏵ accept edits on (shift+tab to cycle)"),
                ]),
            ),
            (
                "a1f00002",
                transcript(&[
                    ("", "Plan: apply 3 migrations"),
                    (DIM, "  0042_add_tokens"),
                    (DIM, "  0043_backfill_sessions"),
                    (DIM, "  0044_drop_legacy_keys"),
                    ("", ""),
                    ("", "Would you like to run the following command?"),
                    (ACC, "  pnpm db:migrate --env staging"),
                    ("", ""),
                    ("", "  1. Yes, proceed (y)"),
                    ("", "  2. Yes, and don’t ask again for this command (p)"),
                    ("", "  3. No, tell Codex what to do differently (esc)"),
                ]),
            ),
            ("a1f00003", transcript(&[(DIM, "cargo watch -x test"), (OK, "   ok  auth::refresh"), ("", "412 passed; 0 failed")])),
            (
                "a1f00005",
                transcript(&[
                    (ACC, "⏺ I found three ways to structure the golden tests:"),
                    ("", ""),
                    ("", "  A. One golden per widget"),
                    ("", "  B. One golden per screen"),
                    ("", "  C. Per screen, with widget goldens for shared parts"),
                    ("", ""),
                    (NEED, "Which should I take?"),
                ]),
            ),
            ("a1f00006", transcript(&[(DIM, "❯ flutter build ios"), (ERR, "error: No signing certificate \"Apple Development\" found"), (ERR, "exit 1")])),
            ("a1f00007", transcript(&[(ACC, "⏺ Bash(cargo build --release)"), (DIM, "   Compiling gpui-pre v0.3.7"), (DIM, "   [812/1190]")])),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        FakeBackend {
            st: Arc::new(Mutex::new(FakeState {
                projects,
                sessions,
                needs,
                settings,
                rules: vec![json!({"id": "r_staging", "effect": "ask", "matcher": {"kind": "command", "pattern": "* --env staging"}, "scope": "global"})],
                seq: 100,
                subs: vec![],
                #[cfg(feature = "fake-engine")]
                terms: HashMap::new(),
                scripts,
                next_id: 8,
            })),
        }
    }
}

impl FakeState {
    fn emit(&mut self, kind: &str, session_id: Option<&str>, data: Value) {
        self.seq += 1;
        let ev = Event {
            seq: self.seq,
            at: now_rfc3339(),
            kind: kind.into(),
            actor: Actor { kind: "human".into(), ..Default::default() },
            project_id: None,
            session_id: session_id.map(Into::into),
            data,
        };
        self.subs.retain(|tx| tx.try_send(BackendEvent::Event(ev.clone())).is_ok());
    }

    fn set_status(&mut self, sid: &str, state: StatusState, reason: Option<String>) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == sid) {
            s.status = Status { state, reason, exit_code: None, since: Some(now_rfc3339()) };
        }
        self.emit("session.status", Some(sid), json!({"state": state}));
    }

    fn setting(&self, key: &str) -> String {
        self.settings.iter().find(|s| s.key == key).and_then(|s| s.value.as_str()).unwrap_or("").to_string()
    }

    fn script(&self, sid: &str, slot: &str) -> Vec<Segment> {
        let Some(s) = self.sessions.iter().find(|s| s.id == sid) else {
            return vec![];
        };
        let Some(g) = &s.git else { return vec![] };
        let seg = |text: String, tone: Option<Tone>, icon: Option<&str>, mono: bool, join: bool| Segment { text, tone, icon: icon.map(Into::into), mono, join, link: None, tooltip: None };
        let stats = |out: &mut Vec<Segment>| {
            if g.files > 0 {
                out.push(seg(format!("+{}", g.added), Some(Tone::Ok), None, true, false));
                out.push(seg(format!("−{}", g.removed), Some(Tone::Err), None, true, true));
                out.push(seg(format!("×{}", g.files), Some(Tone::Dim), None, true, true));
            }
        };
        let pr = |out: &mut Vec<Segment>| {
            if let Some(pr) = &g.pr {
                let mut p = seg(format!("#{}", pr.number), None, Some("pr"), false, false);
                p.link = Some(pr.url.clone());
                out.push(p);
                let (t, tone) = match pr.checks {
                    Checks::Failing => (format!("{} failing", pr.failing_count.max(1)), Tone::Err),
                    Checks::Passing => ("checks passing".into(), Tone::Ok),
                    _ => ("checks running".into(), Tone::Need),
                };
                out.push(seg(t, Some(tone), Some("dot"), false, false));
            }
        };
        let mut out = vec![];
        let which = self.setting(match slot {
            "row" => "ui.row.script",
            "status" => "ui.status.script",
            _ => "ui.header.script",
        });
        for part in which.split('+') {
            match part {
                "github" => {
                    out.extend(g.worktree.iter().map(|w| Segment { tooltip: Some(w.clone()), ..seg(String::new(), Some(Tone::Work), Some("worktree"), false, false) }));
                    out.push(seg(g.branch.clone(), Some(Tone::Dim), Some("branch"), false, false));
                    stats(&mut out);
                    pr(&mut out);
                }
                "worktree" => out.extend(g.worktree.iter().map(|w| Segment { tooltip: Some(w.clone()), ..seg(String::new(), Some(Tone::Work), Some("worktree"), false, false) })),
                "branch" => out.push(seg(g.branch.clone(), Some(Tone::Dim), Some("branch"), false, false)),
                "diff" | "git-diff-stats" => stats(&mut out),
                "pr" => pr(&mut out),
                "agent" if s.kind == SessionKind::Agent => {
                    out.push(seg("$0.82".into(), Some(Tone::Dim), None, false, false));
                    out.push(seg("opus".into(), Some(Tone::Dim), None, false, false));
                }
                _ => {}
            }
        }
        out
    }
}

impl Backend for FakeBackend {
    fn label(&self) -> &'static str {
        "fake"
    }

    fn socket_path(&self) -> PathBuf {
        PathBuf::from("(fake backend)")
    }

    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let mut st = self.st.lock().unwrap();
        let p = |k: &str| params.get(k).and_then(|v| v.as_str()).map(str::to_string);
        Ok(match method {
            "daemon.info" => json!({"version": "fake", "pid": std::process::id()}),
            "project.list" => serde_json::to_value(&st.projects)?,
            "project.discover" => json!([
                {"name": "kass", "path": "~/Development/kass", "root": "~/Development", "git": true},
                {"name": "midna", "path": "~/Development/rust/midna", "root": "~/Development", "git": true},
            ]),
            "session.list" => serde_json::to_value(&st.sessions)?,
            "session.subagent_log" => fake_subagent_log(&p("agent").unwrap_or_default(), params.get("from").and_then(Value::as_u64).unwrap_or(0)),
            "needs_you.list" => serde_json::to_value(&st.needs)?,
            "notify.history" => fake_history(),
            "notify.read" => {
                FAKE_READ.store(true, std::sync::atomic::Ordering::Relaxed);
                json!({ "read_seq": 900, "unread": 0 })
            }
            "settings.list" => serde_json::to_value(&st.settings)?,
            "notify.media" => {
                // The repo's copies of midna's own sounds, so previews play in dev.
                let twilight = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../midnad/assets/sounds/twilight");
                let sounds: Vec<Value> = midna_proto::notify::TWILIGHT
                    .iter()
                    .map(|n| json!({ "kind": "sound", "name": n, "path": twilight.join(format!("{n}.wav")), "builtin": true, "set": "twilight" }))
                    .chain(midna_proto::notify::SYSTEM_SOUNDS.iter().map(|n| {
                        json!({ "kind": "sound", "name": n, "path": midna_proto::notify::system_sound_path(n), "builtin": true, "set": "macos" })
                    }))
                    .collect();
                json!({ "sounds": sounds, "images": [], "dir": "/tmp/midna-fake/notify" })
            }
            "rule.list" => Value::Array(st.rules.clone()),
            "trigger.list" => serde_json::to_value(fake_triggers())?,
            "insights.summary" => {
                json!({"range": "today", "totals": {"turns": 38, "messages": 52, "spend_usd": 4.12, "triggers_fired": 2}})
            }
            // No fake history: Insights shows its charts empty (the seeded midnad has data).
            "insights.series" => {
                json!({"unit": "count", "bucket": "hour", "buckets": [], "groups": [], "total": 0, "previous_total": 0})
            }
            "insights.detail" => {
                let range = params.get("range").and_then(Value::as_str).unwrap_or("today");
                serde_json::to_value(fake_insights_detail(range, &st.sessions, &st.projects))?
            }
            "insights.activity" => json!([]),
            "webhooks.status" => json!({"path": st.setting("webhooks.path"), "health": "ok"}),
            // MIDNA_FAKE_HOOKS = not_installed | stale | current (default) for screenshots.
            "hooks.status" | "hooks.install" | "hooks.uninstall" => {
                let state = match method {
                    "hooks.install" => "current".to_string(),
                    "hooks.uninstall" => "not_installed".to_string(),
                    _ => std::env::var("MIDNA_FAKE_HOOKS").unwrap_or_else(|_| "current".into()),
                };
                let detail = (state == "stale").then_some("Missing 1 event: PermissionRequest.");
                json!({
                    "claude": {"agent": "claude", "state": state, "path": "~/.claude/settings.json", "detail": detail, "per_session": state != "current"},
                    "codex": {"agent": "codex", "state": if state == "stale" { "current" } else { state.as_str() }, "path": "~/.codex/config.toml", "per_session": state != "current"},
                })
            }
            "hooks.preview" => json!({"files": [
                {"agent": "claude", "path": "~/.claude/settings.json", "creates": false, "unchanged": false, "lines": [
                    {"op": "…", "text": ""}, {"op": " ", "text": "  \"hooks\": {"},
                    {"op": "+", "text": "    \"PermissionRequest\": [{ \"matcher\": \"*\", \"hooks\": [{ \"type\": \"command\", \"command\": \"[ -n \\\"$MIDNA_SESSION\\\" ] && … hook claude --global || true\" }] }],"},
                    {"op": " ", "text": "    \"Stop\": ["}, {"op": "…", "text": ""}]},
                {"agent": "codex", "path": "~/.codex/config.toml", "creates": false, "unchanged": true, "lines": []},
            ]}),
            "script.run" => {
                let sid = p("session_id").unwrap_or_default();
                let slot = p("slot").unwrap_or_else(|| "header".into());
                match p("script") {
                    // a custom header button: a stand-in deploy button
                    Some(_) if slot == "button" => json!([{"text": "Deploy", "tone": "dim", "icon": "play", "tooltip": "Deploy a preview"}]),
                    // a status-bar script path: a stand-in CI indicator
                    Some(_) => json!([{"text": "CI", "tone": "ok", "icon": "check", "tooltip": "main: all 14 checks passed"}]),
                    None => serde_json::to_value(st.script(&sid, &slot))?,
                }
            }
            "script.click" => json!([{"text": "Deployed", "tone": "ok", "icon": "check", "tooltip": "Preview is live"}]),
            "settings.set" => {
                let key = p("key").ok_or_else(|| anyhow!("key required"))?;
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                let Some(s) = st.settings.iter_mut().find(|s| s.key == key) else { bail!("rpc error 3: unknown setting") };
                s.value = value.clone();
                st.emit("settings.changed", None, json!({"key": key, "value": value}));
                json!({"ok": true})
            }
            "needs_you.resolve" => {
                let id = p("id").ok_or_else(|| anyhow!("id required"))?;
                let res: Resolution = serde_json::from_value(params.get("resolution").cloned().unwrap_or(Value::Null))?;
                let Some(i) = st.needs.iter().position(|n| n.id == id) else { bail!("rpc error 3: not found") };
                let item = st.needs.remove(i);
                st.emit("needs_you.resolved", item.session_id.as_deref(), json!({"id": id, "resolution": res}));
                if let Some(sid) = &item.session_id {
                    let state = if matches!(res, Resolution::Approve { .. }) { StatusState::Working } else { StatusState::Idle };
                    st.set_status(sid, state, None);
                }
                if let Resolution::Approve { scope: ApprovalScope::Always } = res {
                    let rid = format!("r_{:06x}", st.seq);
                    let pattern = item.approval.map(|a| a.action.value).unwrap_or_default();
                    st.rules.push(json!({"id": rid, "effect": "allow", "matcher": {"kind": "command", "pattern": pattern}, "scope": "global"}));
                    st.emit("rule.added", None, json!({"id": rid}));
                }
                json!({"ok": true})
            }
            "session.open" => {
                st.next_id += 1;
                let id = format!("b2e{:05x}", st.next_id);
                let kind: SessionKind = serde_json::from_value(params.get("kind").cloned().unwrap_or(json!("shell"))).unwrap_or_default();
                let agent: Option<AgentKind> = params.get("agent").and_then(|a| serde_json::from_value(a.clone()).ok());
                let project_id = p("project_id");
                let name = p("name").unwrap_or_else(|| match agent {
                    Some(AgentKind::Claude) => "claude".into(),
                    Some(AgentKind::Codex) => "codex".into(),
                    _ => "zsh".into(),
                });
                st.sessions.push(Session {
                    id: id.clone(),
                    project_id,
                    name,
                    kind,
                    agent,
                    status: Status { state: if agent.is_some() { StatusState::Working } else { StatusState::Idle }, since: Some(now_rfc3339()), ..Default::default() },
                    created_at: now_rfc3339(),
                    background: params.get("background").and_then(Value::as_bool).unwrap_or(false),
                    ..Default::default()
                });
                st.emit("session.opened", Some(&id), json!({}));
                json!({"id": id})
            }
            "session.set_background" => {
                let id = p("id").unwrap_or_default();
                let on = params.get("background").and_then(Value::as_bool).unwrap_or(false);
                if let Some(x) = st.sessions.iter_mut().find(|x| x.id == id) {
                    x.background = on;
                }
                st.emit("session.background", Some(&id), json!({ "background": on }));
                json!({"id": id})
            }
            "session.restart" => {
                let id = p("id").unwrap_or_default();
                #[cfg(feature = "fake-engine")]
                if let Some(t) = st.terms.remove(&id) {
                    t.kill();
                }
                st.set_status(&id, StatusState::Idle, None);
                json!({"ok": true})
            }
            "session.close" => {
                let id = p("id").unwrap_or_default();
                st.sessions.retain(|s| s.id != id);
                #[cfg(feature = "fake-engine")]
                if let Some(t) = st.terms.remove(&id) {
                    t.kill();
                }
                st.emit("session.closed", Some(&id), json!({}));
                json!({"ok": true})
            }
            m if m.starts_with("queue.") => {
                let sid = p("session").ok_or_else(|| anyhow!("session required"))?;
                let id = p("id").unwrap_or_default();
                let Some(s) = st.sessions.iter_mut().find(|s| s.id == sid) else { bail!("rpc error 3: no terminal {sid}") };
                let mut action = m.trim_start_matches("queue.").to_string();
                match m {
                    "queue.list" => {}
                    "queue.add" => {
                        let mut q: midna_proto::QueuedMessage = serde_json::from_value(json!({
                            "id": format!("q_f{:05x}", s.queue.len() + 1), "text": p("text").unwrap_or_default(), "by": {"kind": "human"},
                            "queued_at": now_rfc3339(), "when": params.get("when").cloned().unwrap_or(json!({"kind": "idle"}))
                        }))?;
                        q.enter = true;
                        s.queue.push(q);
                        action = "added".into();
                    }
                    "queue.update" => {
                        if let Some(q) = s.queue.iter_mut().find(|q| q.id == id) {
                            if let Some(t) = p("text") {
                                q.text = t;
                            }
                            if let Some(w) = params.get("when").and_then(|w| serde_json::from_value(w.clone()).ok()) {
                                q.when = w;
                            }
                            q.state = midna_proto::QueueState::Waiting;
                            q.error = None;
                        }
                        action = "updated".into();
                    }
                    "queue.remove" | "queue.send_now" => {
                        s.queue.retain(|q| q.id != id);
                        action = if m == "queue.remove" { "removed".into() } else { "sent".into() };
                    }
                    "queue.move" => {
                        if let Some(i) = s.queue.iter().position(|q| q.id == id) {
                            let q = s.queue.remove(i);
                            let to = params.get("to").and_then(Value::as_u64).unwrap_or(0) as usize;
                            s.queue.insert(to.min(s.queue.len()), q);
                        }
                        action = "moved".into();
                    }
                    "queue.clear" => s.queue.clear(),
                    "queue.pause" => s.queue_paused = params.get("paused").and_then(Value::as_bool).unwrap_or(true),
                    _ => bail!("rpc error -32601: unknown method {m}"),
                }
                let out = json!({ "session": sid, "paused": s.queue_paused, "items": s.queue });
                let left = s.queue.len();
                if m != "queue.list" {
                    st.emit("session.queue", Some(&sid), json!({ "action": action, "id": id, "left": left }));
                }
                out
            }
            "window.command" => {
                let cmd = params.clone();
                st.subs.retain(|tx| tx.try_send(BackendEvent::WindowCommand(cmd.clone())).is_ok());
                json!({"ok": true})
            }
            _ => bail!("rpc error -32601: unknown method {method}"),
        })
    }

    fn subscribe(&self, tx: async_channel::Sender<BackendEvent>) {
        let _ = tx.try_send(BackendEvent::Conn(ConnState::Connected));
        self.st.lock().unwrap().subs.push(tx);
    }

    #[cfg(feature = "fake-engine")]
    fn attach(&self, req: AttachRequest<'_>, sink: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>> {
        let mut st = self.st.lock().unwrap();
        let sess = st.sessions.iter().find(|s| s.id == req.session).cloned().ok_or_else(|| anyhow!("rpc error 3: no such session"))?;
        let term = match st.terms.get(req.session) {
            Some(t) if t.alive() => t.clone(),
            _ => {
                let pre = st.scripts.get(req.session).cloned().unwrap_or_default();
                let t = Arc::new(pty::FakeTerm::spawn(&sess, &pre, req.cols, req.rows));
                st.terms.insert(req.session.to_string(), t.clone());
                t
            }
        };
        drop(st);
        Ok(term.attach(sink, req))
    }

    #[cfg(not(feature = "fake-engine"))]
    fn attach(&self, _req: AttachRequest<'_>, _sink: Arc<FrameSink>) -> anyhow::Result<Arc<dyn TermStream>> {
        bail!("fake backend built without the fake-engine feature")
    }
}

impl Drop for FakeBackend {
    fn drop(&mut self) {
        #[cfg(feature = "fake-engine")]
        for t in self.st.lock().unwrap().terms.values() {
            t.kill();
        }
    }
}

/// Kill every fake PTY child (called on quit).
pub fn shutdown(b: &dyn Backend) {
    let _ = b;
    #[cfg(feature = "fake-engine")]
    pty::kill_all();
}

#[cfg(feature = "fake-engine")]
mod pty {
    use super::super::fake_engine::*;
    use super::*;
    use std::os::fd::RawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{SyncSender, sync_channel};

    static PIDS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

    pub fn kill_all() {
        for pid in PIDS.lock().unwrap().drain(..) {
            unsafe { libc::kill(pid, libc::SIGHUP) };
        }
    }

    enum Msg {
        Bytes(Vec<u8>),
        Resize(u16, u16, u32, u32),
        Want,
        Attach(Arc<FrameSink>),
        Detach(Arc<FrameSink>),
        Client(crate::frame::ClientMsg),
        Eof,
    }

    pub struct FakeTerm {
        fd: RawFd,
        pid: i32,
        tx: SyncSender<Msg>,
        alive: Arc<AtomicBool>,
    }

    impl FakeTerm {
        pub fn spawn(s: &Session, pre: &str, cols: u16, rows: u16) -> FakeTerm {
            let dir = std::env::temp_dir();
            let mut script = String::new();
            if !pre.is_empty() {
                script.push_str(&format!("printf '%s' '{}'; ", pre.replace('\'', "'\\''")));
            }
            let interactive = matches!(s.kind, SessionKind::Shell) || s.agent.is_none();
            if interactive {
                script.push_str("exec /bin/zsh -f");
            } else {
                // Agents in the fake are inert: keystrokes echo, nothing runs.
                script.push_str("exec cat >/dev/null");
            }
            let prompt = format!("%F{{#9AA2B3}}~/Development/{}%f\n❯ ", s.name);
            let home = dir.to_string_lossy().to_string();
            let (fd, pid) = spawn_pty(&script, cols.max(2), rows.max(2), &[("PROMPT", &prompt), ("HOME", &home), ("LANG", "en_US.UTF-8")]);
            PIDS.lock().unwrap().push(pid);
            let (tx, rx) = sync_channel::<Msg>(64);
            let alive = Arc::new(AtomicBool::new(true));
            // engine thread
            let alive_e = alive.clone();
            std::thread::Builder::new()
                .name("fake-engine".into())
                .spawn(move || {
                    let mut eng = Engine::new(cols.max(2), rows.max(2), Some(fd));
                    let mut sink: Option<Arc<FrameSink>> = None;
                    let mut want = false;
                    let handle = |eng: &mut Engine, sink: &mut Option<Arc<FrameSink>>, want: &mut bool, m: Msg| match m {
                        Msg::Bytes(b) => eng.feed(&b, now_ns()),
                        Msg::Resize(c, r, cw, ch) => {
                            eng.resize(c, r, cw, ch);
                            pty_resize(fd, c, r);
                        }
                        Msg::Want => *want = true,
                        Msg::Client(c) => {
                            let out = eng.client(&c);
                            if !out.is_empty() {
                                write_all(fd, &out);
                            }
                        }
                        Msg::Attach(s) => {
                            *sink = Some(s);
                            *want = true;
                            eng.force_full();
                        }
                        Msg::Detach(d) => {
                            if sink.as_ref().is_some_and(|s| Arc::ptr_eq(s, &d)) {
                                *sink = None;
                            }
                        }
                        Msg::Eof => {
                            alive_e.store(false, Ordering::SeqCst);
                            if let Some(s) = sink.take() {
                                s.end();
                            }
                        }
                    };
                    while let Ok(m) = rx.recv() {
                        handle(&mut eng, &mut sink, &mut want, m);
                        let mut n = 0;
                        while n < 256 {
                            match rx.try_recv() {
                                Ok(m) => handle(&mut eng, &mut sink, &mut want, m),
                                Err(_) => break,
                            }
                            n += 1;
                        }
                        if want
                            && let Some(s) = &sink
                            && let Some(f) = eng.frame()
                        {
                            want = false;
                            s.put(f);
                        }
                    }
                })
                .unwrap();
            // pty reader
            let txr = tx.clone();
            std::thread::Builder::new()
                .name("fake-pty".into())
                .spawn(move || {
                    loop {
                        let mut buf = vec![0u8; 65536];
                        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
                        if n <= 0 {
                            let _ = txr.send(Msg::Eof);
                            break;
                        }
                        buf.truncate(n as usize);
                        if txr.send(Msg::Bytes(buf)).is_err() {
                            break;
                        }
                    }
                })
                .unwrap();
            FakeTerm { fd, pid, tx, alive }
        }

        pub fn alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }

        pub fn kill(&self) {
            unsafe { libc::kill(self.pid, libc::SIGHUP) };
        }

        pub fn attach(self: &Arc<Self>, sink: Arc<FrameSink>, req: AttachRequest<'_>) -> Arc<dyn TermStream> {
            let _ = self.tx.send(Msg::Resize(req.cols.max(2), req.rows.max(2), req.cell_w, req.cell_h));
            let _ = self.tx.send(Msg::Attach(sink.clone()));
            Arc::new(FakeStream { term: self.clone(), sink, closed: AtomicBool::new(false) })
        }
    }

    struct FakeStream {
        term: Arc<FakeTerm>,
        sink: Arc<FrameSink>,
        closed: AtomicBool,
    }

    impl TermStream for FakeStream {
        fn want(&self) {
            let _ = self.term.tx.try_send(Msg::Want);
        }
        fn resize(&self, cols: u16, rows: u16, cw: u32, ch: u32) {
            let _ = self.term.tx.send(Msg::Resize(cols, rows, cw, ch));
        }
        fn input(&self, bytes: &[u8]) {
            write_all(self.term.fd, bytes);
        }
        fn send(&self, m: &crate::frame::ClientMsg) {
            let _ = self.term.tx.send(Msg::Client(m.clone()));
        }
        fn close(&self) {
            if !self.closed.swap(true, Ordering::SeqCst) {
                let _ = self.term.tx.send(Msg::Detach(self.sink.clone()));
            }
        }
    }

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.close();
        }
    }
}

/// The `api` terminal's subagents (`MIDNA_DEBUG_SCREEN=subagents|subagent-window`).
fn fake_subagents() -> midna_proto::AgentInfo {
    let ago = |s: i64| midna_proto::time::format_unix(midna_proto::time::now_unix() - s);
    let sub = |id: &str, ty: &str, desc: &str, started: i64, ended: Option<i64>, background: bool| midna_proto::Subagent {
        id: id.into(),
        agent_type: ty.into(),
        description: desc.into(),
        background,
        started_at: ago(started),
        ended_at: ended.map(ago),
    };
    // A one-time cron pinned two hours from now, the way Claude writes them.
    let in_2h = {
        let (_, mo, d, h, mi, _) = midna_proto::time::local_parts(midna_proto::time::now_unix() + 7200);
        format!("{mi} {h} {d} {mo} *")
    };
    midna_proto::AgentInfo {
        conversation_id: Some("c0ffee00-fake".into()),
        subagents: vec![
            sub("a0000000000000001", "Explore", "Map SubagentStart payload fields", 72, None, false),
            sub("a0000000000000002", "general-purpose", "Draft agent_work tests", 48, None, false),
            sub("a0000000000000003", "qa-reviewer", "Review daemon changes", 185, None, true),
        ],
        finished_subagents: vec![sub("a0000000000000000", "Explore", "Find where hooks set agent status", 140, Some(99), false)],
        crons: vec![
            midna_proto::AgentCron { id: "k1".into(), schedule: "*/5 13-16 * * 1-5".into(), recurring: true, prompt: "Poll the deploy and tell me when it is live".into(), created_at: Some(ago(3600)) },
            midna_proto::AgentCron { id: "k2".into(), schedule: in_2h, recurring: false, prompt: "Check CI on #241 and fix anything that failed".into(), created_at: Some(ago(600)) },
            midna_proto::AgentCron { id: "k3".into(), schedule: "57 8 * * 1-5".into(), recurring: true, prompt: "Summarize overnight PR comments".into(), created_at: None },
        ],
        ..Default::default()
    }
}

/// One subagent's transcript, served whole on the first read.
fn fake_subagent_log(agent: &str, from: u64) -> Value {
    let e = |kind: &str, text: &str| json!({ "kind": kind, "text": text });
    let entries = if from > 0 {
        vec![]
    } else {
        vec![
            e("prompt", "Find every field Claude Code 2.1.289 sends in SubagentStart and SubagentStop hook payloads in this repo's recorded fixtures, and report which ones could link a subagent back to the Agent tool call that launched it.\nLook in crates/midnad/tests/fixtures first.\nThen check ~/.claude/projects for meta files.\nReport field names and an example payload for each.\nDon't change any files."),
            e("tool", "Grep(SubagentStart)"),
            e("result", "crates/midnad/tests/fixtures/claude-2.1.289-background.jsonl:11 (+1 lines)"),
            e("tool", "Read(crates/midnad/tests/fixtures/claude-2.1.289-background.jsonl)"),
            e("result", "Read 61 lines"),
            e("text", "SubagentStart carries agent_id and agent_type only. SubagentStop adds agent_transcript_path and last_assistant_message. Neither has the tool_use_id of the Agent call."),
            e("tool", "Grep(tool_use_id)"),
            e("result", "(no output)"),
            e("tool", "Bash(ls ~/.claude/projects/*/subagents | head)"),
            e("error", "ls: no matches found"),
        ]
    };
    let finished = agent == "a0000000000000000";
    json!({ "running": !finished, "entries": entries, "next": 4096, "model": "claude-haiku-4-5" })
}

/// The golden-test terminal's question (`MIDNA_DEBUG_SCREEN=toast-long`): long, with options.
pub fn fake_question() -> NeedsYouQuestion {
    let o = |label: &str, description: &str| midna_proto::QuestionOption { label: label.into(), description: description.into() };
    NeedsYouQuestion {
        text: "The feed golden test fails on CI but passes locally, and I found two causes that both look real.\n\n\
               The CI image has Flutter 3.24.1 while you have 3.24.3 locally. 3.24.3 changed how text baselines round, \
               so every golden with a caption moves by one pixel. Updating the goldens on CI's version fixes CI but \
               breaks them for you; pinning Flutter in .fvmrc fixes both but touches every developer's setup.\n\n\
               Separately, the feed's shimmer animation isn't paused in tests, so the golden captures a random frame. \
               That one is a real bug in the test, and fixing it is safe either way.\n\n\
               I can do one of these, or both. Which should I take?"
            .into(),
        header: "Golden test fix".into(),
        multi_select: false,
        options: vec![
            o("Pin Flutter 3.24.3", "Add .fvmrc and bump the CI image; goldens stay as they are"),
            o("Regenerate on CI's version", "Update the goldens to 3.24.1; yours break until you downgrade"),
            o("Only fix the shimmer", "Pause the animation in tests and leave the version question for later"),
        ],
    }
}

static FAKE_READ: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What the Notifications screen lists in the fake backend: three unread until it's opened.
fn fake_history() -> Value {
    let at = |s: i64| midna_proto::time::format_unix(midna_proto::time::now_unix() - s);
    let read = FAKE_READ.load(std::sync::atomic::Ordering::Relaxed);
    let item = |seq: u64, ago: i64, session: &str, category: &str, body: &str, push: bool, need: Option<&str>| {
        json!({
            "seq": seq, "at": at(ago), "session_id": session, "unread": !read && seq > 890,
            "notification": { "category": category, "title": "", "body": body, "push": push, "via": "app", "needs_you_id": need },
        })
    };
    let items = vec![
        item(899, 120, "a1f00002", "approval", "Permission: command · pnpm db:migrate --env staging", true, Some("n_migrat")),
        item(897, 240, "a1f00008", "attention", "Prompt blocked: Context is full. Compact first, then resend.", true, Some("n_blockd")),
        item(893, 720, "a1f00005", "approval", "Asked a question: Golden test fix", true, Some("n_golden")),
        item(880, 1500, "a1f00001", "turn_done", "Finished in 6m 12s: The PDF renders with the new totals table.", false, None),
        item(870, 2400, "a1f00009", "failed", "Exited with status 1", false, None),
        item(850, 3600, "a1f00003", "agent", "Build is green\nmain @ 3f2a", false, None),
        item(700, 90_000, "a1f00002", "turn_done", "Finished in 14m 2s: Drafted 0042_ledger.sql and its rollback.", false, None),
        item(650, 95_000, "a1f00001", "failed", "Turn failed: API error (overloaded)", false, None),
    ];
    json!({ "items": items, "unread": if read { 0 } else { 3 }, "read_seq": if read { 900 } else { 890 } })
}

/// Sample data for the Insights widgets (`insights.detail`), so they can be screenshotted
/// without a daemon. Shaped like a busy day; deterministic.
fn fake_insights_detail(range: &str, sessions: &[Session], projects: &[Project]) -> midna_proto::InsightsDetail {
    use midna_proto::{InsightsAgentTime, InsightsBests, InsightsConcurrency, InsightsCorrections, InsightsCount, InsightsDetail, InsightsHeatDay, InsightsTurnLengths, InsightsWait, time};
    let now = time::now_unix();
    let agents: Vec<&Session> = sessions.iter().filter(|s| s.agent.is_some()).collect();
    let sessions: Vec<&Session> = if agents.is_empty() { sessions.iter().collect() } else { agents };
    let today = time::local_day_start(now);
    let at = |t: i64| time::format_unix(t);
    let (from, days, step) = match range {
        "yesterday" => (today - 86_400, 1, 600),
        "week" => (today - 6 * 86_400, 7, 3600),
        "month" => (today - 29 * 86_400, 30, 3600),
        _ => (today, 1, 600),
    };
    let axis_end = if range == "yesterday" { today } else { today + 86_400 };
    let reached = if range == "yesterday" { today } else { now };
    // agents at work through the day: quiet nights, a morning and an afternoon peak
    let load = |t: i64| -> f64 {
        let h = ((t - time::local_day_start(t)) as f64) / 3600.0;
        let bump = |c: f64, w: f64, top: f64| top * (-((h - c) / w).powi(2)).exp();
        let wobble = ((t / step) as f64 * 1.7).sin() * 0.25;
        (bump(10.5, 1.8, 2.6) + bump(15.0, 2.2, 3.4) + bump(21.0, 1.0, 0.8) + wobble).max(0.0)
    };
    let n = ((axis_end - from + step - 1) / step) as usize;
    let samples: Vec<f64> = (0..n).map(|i| from + i as i64 * step).map(|t| if t < reached { (load(t) * 100.0).round() / 100.0 } else { 0.0 }).collect();
    let scale = days as i64;
    let sid = |i: usize| sessions.get(i).map(|s| s.id.clone()).unwrap_or_else(|| format!("s_fake{i}"));
    let row = |i: usize, w: i64, b: i64, idle: i64| {
        let s = sessions.get(i);
        InsightsAgentTime {
            key: sid(i),
            label: s.map(|s| s.name.clone()).unwrap_or_else(|| format!("terminal {i}")),
            project_id: s.and_then(|s| s.project_id.clone()),
            working_secs: w * scale,
            blocked_secs: b * scale,
            idle_secs: idle * scale,
        }
    };
    let titles = ["permission Bash(cargo test -p midnad)", "permission Edit(src/ui/insights.rs)", "question: Which chart style?", "permission Bash(git push)", "permission WebFetch(docs.rs)"];
    let waits = [(42, 5400), (310, 4700), (18, 3900), (1260, 3000), (75, 2100), (640, 1300), (25, 600), (95, 240)]
        .iter()
        .enumerate()
        .filter(|(_, (_, ago))| range != "yesterday" || *ago > 0)
        .map(|(i, &(secs, ago))| {
            let resolved = if range == "yesterday" { today - ago } else { (now - ago).max(from + secs) };
            InsightsWait { secs, resolved_at: at(resolved), session_id: Some(sid(i % 4)), title: titles[i % titles.len()].into() }
        })
        .collect();
    let count = |key: &str, label: &str, value: f64| InsightsCount { key: key.into(), label: label.into(), value };
    let project = |i: usize| projects.get(i).map(|p| (p.id.clone(), p.name.clone())).unwrap_or_else(|| (format!("p_{i}"), format!("project {i}")));
    let projects_out = [11_400, 6_300, 2_700]
        .iter()
        .enumerate()
        .map(|(i, secs)| {
            let (k, l) = project(i);
            count(&k, &l, (*secs * scale) as f64)
        })
        .collect();
    let heatmap = (0..7)
        .map(|d| {
            let day = today - (6 - d) * 86_400;
            let weekend = d == 1 || d == 2;
            let working_secs: Vec<i64> = (0..24).map(|h| day + h * 3600).map(|t| if t > now { 0 } else { (load(t) * if weekend { 400.0 } else { 1500.0 }) as i64 }).collect();
            let you = (0..24).map(|h| working_secs[h] > 900 && (h + d as usize) % 5 != 0).collect();
            InsightsHeatDay { day: at(day), working_secs, you }
        })
        .collect();
    InsightsDetail {
        from: at(from),
        to: at(axis_end),
        concurrency: InsightsConcurrency { step_secs: step, samples, peak: 5, peak_at: Some(at(from.max(today - 86_400 * i64::from(range == "yesterday")) + 15 * 3600 + 600)), avg_while_working: 2.3, multi_secs: 13_800 * scale },
        turns: InsightsTurnLengths { bins: [14, 22, 11, 5, 2, 1].map(|b| b * scale).to_vec(), median_secs: 212, longest_secs: 4_380, longest_session: Some(sid(1)) },
        waits,
        agent_time: vec![row(0, 9_600, 1_900, 2_400), row(1, 7_200, 620, 1_100), row(2, 3_300, 1_250, 300), row(3, 1_800, 0, 420)],
        approved: vec![count("Bash(cargo test)", "Bash(cargo test)", 12.0 * scale as f64), count("Edit", "Edit", 9.0), count("Bash(git push)", "Bash(git push)", 4.0), count("WebFetch", "WebFetch", 3.0), count("Question", "Question", 2.0)],
        corrections: (0..days).map(|i| InsightsCorrections { day: at(from + i * 86_400), denied: (i * 7 + 3) % 4, stopped: (i * 5 + 1) % 3 }).collect(),
        heatmap,
        projects: projects_out,
        models: vec![count("Opus 5.5", "Opus 5.5", 18.42 * scale as f64), count("Sonnet 5", "Sonnet 5", 3.17 * scale as f64), count("Haiku 4.5", "Haiku 4.5", 0.21 * scale as f64)],
        bests: InsightsBests {
            busiest_day: Some(at(today - 3 * 86_400)),
            busiest_day_secs: 41_200,
            longest_turn_secs: 7_940,
            longest_turn_at: Some(at(today - 5 * 86_400 + 16 * 3600)),
            peak_agents: 7,
            peak_at: Some(at(today - 2 * 86_400 + 14 * 3600 + 1200)),
        },
    }
}
