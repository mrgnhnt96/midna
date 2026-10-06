//! The ⌘K command registry and its fuzzy matcher (CommandBar-A). Pure logic, no GPUI, so
//! it is unit-tested and can grow without touching the view.
//!
//! **Adding a command.** Every row in the palette is one [`Command`]: a title, keywords, a
//! few flags that become badges, and a [`Run`] that says what happens, as data. Three ways
//! to add one:
//! - Built-ins: push to the list in [`build`] (it runs on every keystroke from live state:
//!   projects, terminals, needs-you items, settings).
//! - Project commands: `midna call project.update '{"id":"p_…","commands":[{"name":"test","run":"cargo test","pinned":true}]}'`
//!   (agents can do this). Each becomes "Run <name>", and pinned ones are suggested.
//! - Scripts: `$MIDNA_HOME/commands.json`, a JSON array of `Command` objects (see
//!   [`load_user_commands`]), e.g.
//!   `[{"id":"deploy","title":"Deploy staging","keywords":"ship","danger":"Deploys to staging.","run":{"kind":"rpc","method":"session.open","params":{"kind":"monitor","command":["./deploy.sh"]}}}]`.
//!
//! Every command runs through an RPC (or pure GUI navigation), so the daemon's audit log
//! records what the human ran (`midna events --kind audit`).
use crate::model::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a command does when run. Data only, so commands can come from JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Run {
    /// Any daemon method. A `session.open` result is selected afterwards.
    Rpc {
        method: String,
        #[serde(default)]
        params: Value,
    },
    /// Select a terminal.
    Focus { session: String },
    /// ⌘1–9: select project N (sidebar order).
    Project { index: usize },
    /// `rules` | `triggers` | `insights` | `settings` | `needs_you`, or `sidebar` (collapse / expand it).
    Screen { screen: String },
    /// Open the terminal in a separate always-on-top window.
    PopOut { session: String },
    /// `needs_you.resolve` with an optimistic hide.
    Resolve { need: String, resolution: Resolution },
    /// Replace the query (e.g. "Add a rule: " then hand it to an agent).
    Prefill { text: String },
    /// Add a folder as a project (`project.add`) and open a terminal in it. No path = folder picker.
    OpenProject {
        #[serde(default)]
        path: Option<String>,
    },
    /// Scroll an agent terminal to one of the human's prompts (`session.jump_prompt`), or to
    /// the live end (None) to write a new one.
    JumpPrompt { session: String, n: Option<u32> },
    /// Open a terminal's queued messages (the pill's panel).
    Queue { session: String },
    /// Open a folder in an installed IDE (`ide.rs`), remembered for its project.
    OpenIde { ide: String, dir: String },
    /// Open a new GitHub issue with this Mac's details filled in (`report.rs`).
    ReportIssue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CmdIcon {
    Claude,
    Codex,
    Monitor,
    Shell,
    Project,
    #[default]
    Run,
    Screen,
    Approve,
    Deny,
    Rule,
    Pin,
    New,
    Trigger,
    Restart,
    Ide,
}

/// One palette row. Optional fields become badges: `pending` = "waiting on you",
/// `approval` = "needs approval", `danger` = "destructive" (↩ twice), `human_only` = "you only".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Command {
    /// Stable id, e.g. `focus:abcd1234`. Used to keep the armed (confirm) state.
    pub id: String,
    pub title: String,
    pub sub: String,
    /// Extra words that match by prefix ("kill" finds "Close terminal").
    pub keywords: String,
    /// Shortcut label shown on the row (already pretty, e.g. "⌘T").
    pub keys: Option<String>,
    /// CLI equivalent, for discoverability.
    pub cli: Option<String>,
    pub icon: CmdIcon,
    /// Shown with an empty query, under this heading ("Needs you", "Suggested").
    pub featured: Option<String>,
    pub pending: bool,
    pub approval: Option<String>,
    /// Why it is destructive. Set => ↩ arms, ↩ again runs.
    pub danger: Option<String>,
    pub human_only: bool,
    /// Small state badge ("on", "dark").
    pub state: Option<String>,
    /// Approval rows offer scope chips (once / 15 min / 1 h / session / always).
    pub approve_menu: bool,
    pub run: Option<Run>,
}

impl Command {
    fn new(id: impl Into<String>, icon: CmdIcon, title: impl Into<String>, run: Run) -> Command {
        Command { id: id.into(), icon, title: title.into(), run: Some(run), ..Default::default() }
    }
    fn sub(mut self, s: impl Into<String>) -> Self {
        self.sub = s.into();
        self
    }
    fn kw(mut self, s: &str) -> Self {
        self.keywords = s.into();
        self
    }
    fn keys(mut self, k: String) -> Self {
        if !k.is_empty() {
            self.keys = Some(k);
        }
        self
    }
    fn cli(mut self, s: impl Into<String>) -> Self {
        self.cli = Some(s.into());
        self
    }
    fn featured(mut self, s: &str) -> Self {
        self.featured = Some(s.into());
        self
    }
    fn danger(mut self, s: impl Into<String>) -> Self {
        self.danger = Some(s.into());
        self
    }
}

/// Live state the registry is built from.
pub struct Snapshot<'a> {
    pub projects: &'a [Project],
    /// Folders under `projects.roots` (`project.discover`).
    pub discovered: &'a [ProjectCandidate],
    /// Sessions in sidebar order.
    pub sessions: Vec<&'a Session>,
    pub needs: &'a [NeedsYou],
    pub selected: Option<&'a Session>,
    pub current_project: Option<&'a Project>,
    /// Pretty shortcut label for a `keys.*` setting ("" when unbound).
    pub key: &'a dyn Fn(&str) -> String,
    pub setting: &'a dyn Fn(&str) -> Option<String>,
    pub rules_count: usize,
    /// The agent "New agent" would start in the current project.
    pub last_agent: AgentKind,
    /// `policy.check` decisions for project command lines (`allow` | `ask` | `deny`).
    pub policy: &'a dyn Fn(&str) -> Option<String>,
    pub sidebar_collapsed: bool,
    /// The sidebar's Background group is hidden (`MainWindow::background_hidden`).
    pub background_hidden: bool,
    /// "this window" / "other window" for a terminal whose name another window also uses.
    pub name_note: &'a dyn Fn(&Session) -> Option<&'static str>,
}

pub fn agent_name(a: AgentKind) -> &'static str {
    match a {
        AgentKind::Codex => "Codex",
        _ => "Claude",
    }
}

/// Restart entries for one terminal. Agents that reported their conversation restart into it;
/// with background work in flight the plain restart becomes "when idle" (now would stop it).
fn restart_commands(x: &Session, pname: &str) -> Vec<Command> {
    let rpc = |p: serde_json::Value| Run::Rpc { method: "session.restart".into(), params: p };
    let info = x.agent_info.clone().unwrap_or_default();
    let resumable = x.agent.is_some() && info.conversation_id.is_some();
    let in_flight = info.in_flight();
    let update = info.update_available.as_ref().map(|v| format!(" · Claude {v} installed")).unwrap_or_default();
    let mut out = vec![];
    if let Some(q) = &info.restart {
        let waiting = if q.waiting_for.is_empty() { "about to run".to_string() } else { format!("waiting for: {}", q.waiting_for.join("; ")) };
        out.push(
            Command::new(format!("restart-cancel:{}", x.id), CmdIcon::Restart, format!("Cancel queued restart of {}", x.name), Run::Rpc {
                method: "session.restart_cancel".into(),
                params: json!({"id": x.id}),
            })
            .sub(format!("{pname} · {} · {waiting}", q.reason))
            .kw("restart queue stop abort")
            .cli(format!("midna restart {} --cancel", x.id)),
        );
    }
    if !resumable {
        out.push(
            Command::new(format!("restart:{}", x.id), CmdIcon::Restart, format!("Restart {}", x.name), rpc(json!({"id": x.id, "resume": false})))
                .sub(format!("{pname} · {} · restarts the process in place", agent_label(x)))
                .kw("reload relaunch")
                .cli(format!("midna restart {} --fresh", x.id)),
        );
        return out;
    }
    if in_flight.is_empty() {
        out.push(
            Command::new(format!("restart:{}", x.id), CmdIcon::Restart, format!("Restart {}", x.name), rpc(json!({"id": x.id, "resume": true})))
                .sub(format!("{pname} · same conversation, fresh process{update}"))
                .kw("reload relaunch resume update")
                .cli(format!("midna restart {}", x.id)),
        );
    }
    if info.restart.is_none() {
        let sub = if in_flight.is_empty() { format!("{pname} · once it's idle{update}") } else { format!("{pname} · after {}{update}", in_flight.join("; ")) };
        out.push(
            Command::new(format!("restart-idle:{}", x.id), CmdIcon::Restart, format!("Restart {} when idle", x.name), rpc(json!({"id": x.id, "when": "idle"})))
                .sub(sub)
                .kw("reload relaunch resume update queue later")
                .cli(format!("midna restart {} --idle", x.id)),
        );
    }
    out.push(
        Command::new(format!("restart-fresh:{}", x.id), CmdIcon::Restart, format!("Restart {} in a new conversation", x.name), rpc(json!({"id": x.id, "resume": false, "force": true})))
            .sub(format!("{pname} · {} · starts over{}", agent_label(x), if in_flight.is_empty() { String::new() } else { " · stops its background work".into() }))
            .kw("reload relaunch fresh clear new")
            .cli(format!("midna restart {} --fresh --force", x.id)),
    );
    out
}

fn agent_label(s: &Session) -> &'static str {
    match s.glyph() {
        Glyph::Claude => "Claude",
        Glyph::Codex => "Codex",
        Glyph::Monitor => "monitor",
        Glyph::Shell => "shell",
    }
}

fn icon_for(s: &Session) -> CmdIcon {
    match s.glyph() {
        Glyph::Claude => CmdIcon::Claude,
        Glyph::Codex => CmdIcon::Codex,
        Glyph::Monitor => CmdIcon::Monitor,
        Glyph::Shell => CmdIcon::Shell,
    }
}

fn state_word(s: StatusState) -> &'static str {
    match s {
        StatusState::Working => "working",
        StatusState::NeedsYou => "waiting on you",
        StatusState::Done => "done",
        StatusState::Failed => "failed",
        StatusState::Exited => "exited",
        _ => "idle",
    }
}

pub fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s.to_string() }
}

/// Who raised a needs-you item, for display ("Claude", "Trigger review-prs", "you").
pub fn who(n: &NeedsYou, session: Option<&Session>) -> String {
    match n.asked_by.kind.as_str() {
        "trigger" => format!("Trigger {}", n.asked_by.name.clone().unwrap_or_default()).trim().to_string(),
        "human" => "You".into(),
        "system" => "midna".into(),
        _ => match session {
            Some(s) if s.agent.is_some() => agent_label(s).to_string(),
            Some(_) => n.asked_by.name.clone().unwrap_or_else(|| "A shell command".into()),
            None => n.asked_by.name.clone().unwrap_or_else(|| "An agent".into()),
        },
    }
}

/// Build the registry from live state. Order matters only as a tie-breaker.
pub fn build(s: &Snapshot) -> Vec<Command> {
    let mut out: Vec<Command> = vec![];
    let key = s.key;
    let project_name = |pid: Option<&str>| -> String { pid.and_then(|p| s.projects.iter().find(|x| x.id == p)).map(|p| p.name.clone()).unwrap_or_else(|| "root".into()) };
    let session = |id: Option<&str>| id.and_then(|id| s.sessions.iter().copied().find(|x| x.id == id));

    // ---- Needs you
    if !s.needs.is_empty() {
        out.push(
            Command::new(
                "needs_you.open",
                CmdIcon::Approve,
                format!("Review {} waiting item{} one at a time", s.needs.len(), if s.needs.len() == 1 { "" } else { "s" }),
                Run::Screen { screen: "needs_you".into() },
            )
            .sub("the needs-you card stack")
            .kw("needs you queue pending inbox attention cards")
            .keys(key("keys.next_needs_you"))
            .cli("midna needs")
            .featured("Needs you"),
        );
    }
    for n in s.needs {
        let sess = session(n.session_id.as_deref());
        let where_ = match sess {
            Some(x) => format!("{} › {}", project_name(x.project_id.as_deref()), x.name),
            None => project_name(n.project_id.as_deref()),
        };
        let age = since_short(&n.created_at);
        let what = n.approval.as_ref().map(|a| a.action.value.clone()).filter(|v| !v.is_empty()).unwrap_or_else(|| n.title.clone());
        let sub = [where_.clone(), who(n, sess), age.clone()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
        let resolve = |r: Resolution| Run::Resolve { need: n.id.clone(), resolution: r };
        let selected_here = s.selected.is_some_and(|x| Some(&x.id) == n.session_id.as_ref());
        match n.kind {
            NeedsYouKind::RuleRemoval => {
                let mut c = Command::new(
                    format!("need:{}:remove", n.id),
                    CmdIcon::Rule,
                    format!("Confirm rule removal: {}", n.title),
                    resolve(Resolution::Approve { scope: ApprovalScope::Once }),
                )
                .sub(sub.clone())
                .kw("rules rule policy remove delete proposal pending approve")
                .cli(format!("midna approve {}", n.id))
                .featured("Needs you")
                .danger("Agents lose this guard once the rule is gone.");
                c.pending = true;
                c.human_only = true;
                out.push(c);
                out.push(
                    Command::new(format!("need:{}:keep", n.id), CmdIcon::Deny, format!("Keep rule: {}", n.title), resolve(Resolution::Deny))
                        .sub(sub)
                        .kw("rules rule policy keep reject deny pending")
                        .cli(format!("midna approve {} --deny", n.id)),
                );
            }
            NeedsYouKind::Failed => {
                out.push(
                    Command::new(
                        format!("need:{}:restart", n.id),
                        CmdIcon::Restart,
                        format!("Restart {}", sess.map(|x| x.name.as_str()).unwrap_or(&n.title)),
                        resolve(Resolution::Restart),
                    )
                    .sub(format!("{} · {}", where_, n.title))
                    .kw("failed exit relaunch reload pending")
                    .featured("Needs you")
                    .cli(format!("midna call needs_you.resolve '{{\"id\":\"{}\",\"resolution\":{{\"kind\":\"restart\"}}}}'", n.id)),
                );
                out.push(
                    Command::new(format!("need:{}:dismiss", n.id), CmdIcon::Deny, format!("Dismiss: {}", n.title), resolve(Resolution::Dismiss)).sub(sub).kw("failed clear ignore"),
                );
            }
            NeedsYouKind::Blocked => {
                let mut c = Command::new(format!("need:{}:done", n.id), CmdIcon::Approve, format!("I've done it: {}", n.title), resolve(Resolution::Done))
                    .sub(sub.clone())
                    .kw("blocked done unblock pending resolve")
                    .featured("Needs you");
                c.pending = true;
                out.push(c);
                out.push(
                    Command::new(format!("need:{}:dismiss", n.id), CmdIcon::Deny, format!("Dismiss: {}", n.title), resolve(Resolution::Dismiss))
                        .sub(sub)
                        .kw("blocked clear ignore"),
                );
            }
            NeedsYouKind::Note => {
                out.push(
                    Command::new(format!("need:{}:ack", n.id), CmdIcon::Approve, format!("Got it: {}", n.title), resolve(Resolution::Dismiss))
                        .sub(sub)
                        .kw("note fyi acknowledge dismiss read")
                        .featured("Needs you"),
                );
            }
            NeedsYouKind::SecretNeeded => {
                let mut c = Command::new(format!("need:{}:secret", n.id), CmdIcon::Trigger, format!("Set secret: {}", n.title), Run::Screen { screen: "triggers".into() })
                    .sub(sub)
                    .kw("secret webhook trigger signing pending")
                    .featured("Needs you");
                c.pending = true;
                c.human_only = true;
                out.push(c);
            }
            _ if n.is_approval() || n.kind == NeedsYouKind::TriggerWaiting => {
                let label = if n.kind == NeedsYouKind::TriggerWaiting { "Start" } else { "Approve" };
                let mut c = Command::new(
                    format!("need:{}:approve", n.id),
                    CmdIcon::Approve,
                    format!("{label} {}: {}", sess.map(|x| x.name.as_str()).unwrap_or("request"), what),
                    resolve(Resolution::Approve { scope: ApprovalScope::Once }),
                )
                .sub(sub.clone())
                .kw("approve allow yes pending permission")
                .cli(format!("midna approve {}", n.id))
                .featured("Needs you");
                if selected_here {
                    c = c.keys(key("keys.approve"));
                }
                c.pending = true;
                c.approve_menu = n.kind != NeedsYouKind::TriggerWaiting;
                out.push(c);
                let mut d = Command::new(
                    format!("need:{}:deny", n.id),
                    CmdIcon::Deny,
                    format!("Deny {}: {}", sess.map(|x| x.name.as_str()).unwrap_or("request"), what),
                    resolve(Resolution::Deny),
                )
                .sub(format!("{where_} · the agent is told you said no"))
                .kw("deny reject no pending")
                .cli(format!("midna approve {} --deny", n.id))
                .featured("Needs you");
                if selected_here {
                    d = d.keys(key("keys.deny"));
                }
                out.push(d);
            }
            _ => {
                out.push(Command::new(format!("need:{}:dismiss", n.id), CmdIcon::Deny, format!("Dismiss: {}", n.title), resolve(Resolution::Dismiss)).sub(sub).kw("clear ignore"));
            }
        }
    }

    // ---- Go to terminals (needs-you terminals are featured; other windows' too, after this one's)
    for x in &s.sessions {
        let needs = s.needs.iter().any(|n| n.session_id.as_ref() == Some(&x.id));
        let state = match &x.custom_status {
            Some(c) => c.label.as_str(),
            None if needs => "waiting on you",
            None => state_word(x.status.state),
        };
        let mut c = Command::new(format!("focus:{}", x.id), icon_for(x), format!("Go to {}", x.name), Run::Focus { session: x.id.clone() })
            .sub(match (s.name_note)(x) {
                Some(w) => format!("{} · {} · {} · {w}", project_name(x.project_id.as_deref()), agent_label(x), state),
                None => format!("{} · {} · {}", project_name(x.project_id.as_deref()), agent_label(x), state),
            })
            .kw("jump switch focus terminal tab")
            .cli(format!("midna focus {}", x.id));
        if needs {
            c = c.featured("Needs you");
        }
        out.push(c);
    }
    // Projects with terminals are in the sidebar (⌘1–9 in that order); the rest are closed and
    // "Go to project" reopens them with a new terminal. Background terminals don't count: they
    // sit in their own group, so a project with only those is closed too. The most recently
    // opened closed ones are listed under "Recent" before anything is typed; folders under
    // projects.roots only show up when searched for.
    let open_projects: Vec<&str> = s.projects.iter().filter(|p| s.sessions.iter().any(|x| !x.background && x.project_id.as_deref() == Some(p.id.as_str()))).map(|p| p.id.as_str()).collect();
    let recent: Vec<String> = recent_projects(s.projects, &open_projects).into_iter().take(RECENT_MAX).map(|r| r.path).collect();
    let featured = |c: Command, path: &str| if recent.iter().any(|r| r == path) { c.featured("Recent") } else { c };
    // recent ones first, most recent first, so "Recent" reads in that order
    let mut ordered: Vec<&Project> = s.projects.iter().collect();
    ordered.sort_by_key(|p| recent.iter().position(|r| *r == p.path).unwrap_or(usize::MAX));
    for p in ordered {
        let slot = open_projects.iter().position(|id| *id == p.id);
        let c = Command::new(format!("project:{}", p.id), CmdIcon::Project, format!("Go to project {}", p.name), Run::OpenProject { path: Some(p.path.clone()) })
            .sub(if slot.is_some() { tilde(&p.path) } else { format!("{} · closed, opens a terminal", tilde(&p.path)) })
            .kw("jump switch project open")
            .keys(slot.filter(|i| *i < 9).map(|i| format!("⌘{}", i + 1)).unwrap_or_default());
        out.push(featured(c, &p.path));
    }
    for c in s.discovered.iter().filter(|c| c.project_id.is_none()) {
        let cmd = Command::new(format!("folder:{}", c.path), CmdIcon::Project, format!("Open project {}", c.name), Run::OpenProject { path: Some(c.path.clone()) })
            .sub(format!("{} · in {}, opens a terminal", tilde(&c.path), c.root))
            .kw("open project folder repo directory")
            .cli(format!("midna project add {}", tilde(&c.path)));
        out.push(cmd);
    }

    // ---- Open a project (a folder picker; typing a path offers that folder directly)
    let mut open_project = Command::new("project.open", CmdIcon::Project, "Open project…", Run::OpenProject { path: None })
        .sub("pick a folder; it becomes a project in the sidebar")
        .kw("add folder repo directory new project")
        .keys(key("keys.open_project"))
        .cli("midna project add <path>");
    if s.projects.is_empty() {
        open_project = open_project.featured("Suggested");
    }
    out.push(open_project);

    // ---- New terminals and agents
    let here = s.current_project;
    // With no current project, the "in <project>" rows already open at root, so the
    // separate "at root" rows are left out.
    let here_name = here.map(|p| format!("in {}", p.name)).unwrap_or_else(|| "at root".into());
    let open = |kind: &str, agent: Option<AgentKind>, project: Option<&Project>| {
        let mut p = json!({"kind": kind});
        if let Some(pr) = project {
            p["project_id"] = json!(pr.id);
        }
        if let Some(a) = agent {
            p["agent"] = json!(a);
        }
        Run::Rpc { method: "session.open".into(), params: p }
    };
    let pflag = |p: Option<&Project>| p.map(|p| format!(" --project {}", p.id)).unwrap_or_default();
    out.push(
        Command::new("new.terminal", CmdIcon::New, format!("New terminal {here_name}"), open("shell", None, here))
            .sub(format!("shell in {}", here.map(|p| p.path.as_str()).unwrap_or("the root")))
            .kw("create open shell tab")
            .keys(key("keys.new_terminal"))
            .cli(format!("midna open{}", pflag(here)))
            .featured("Suggested"),
    );
    for a in [AgentKind::Claude, AgentKind::Codex] {
        let name = agent_name(a);
        let lower = name.to_lowercase();
        out.push(
            Command::new(
                format!("new.agent.{lower}"),
                if a == AgentKind::Claude { CmdIcon::Claude } else { CmdIcon::Codex },
                format!("New {name} agent {here_name}"),
                open("agent", Some(a), here),
            )
            .sub("empty prompt · type a prompt instead to ask an agent")
            .kw("create agent start open tab")
            .keys(if a == s.last_agent { key("keys.new_agent") } else { String::new() })
            .cli(format!("midna open --agent {lower}{}", pflag(here))),
        );
        if here.is_none() {
            continue;
        }
        out.push(
            Command::new(
                format!("new.agent.{lower}.root"),
                if a == AgentKind::Claude { CmdIcon::Claude } else { CmdIcon::Codex },
                format!("New {name} agent at root"),
                open("agent", Some(a), None),
            )
            .sub("sees every project")
            .kw("create agent start open tab root")
            .keys(if a == AgentKind::Claude { key("keys.new_agent_root") } else { String::new() })
            .cli(format!("midna open --agent {lower}")),
        );
    }
    if here.is_some() {
        out.push(
            Command::new("new.terminal.root", CmdIcon::New, "New terminal at root", open("shell", None, None))
                .sub("shell in the root project")
                .kw("create open shell tab root")
                .keys(key("keys.new_terminal_root"))
                .cli("midna open"),
        );
    }

    // ---- Project commands (current project first)
    let mut projects: Vec<&Project> = s.projects.iter().collect();
    if let Some(h) = here {
        projects.sort_by_key(|p| p.id != h.id);
    }
    for p in projects {
        for pc in &p.commands {
            let decision = (s.policy)(&pc.run);
            let mut c = Command::new(
                format!("run:{}:{}", p.id, pc.name),
                CmdIcon::Run,
                format!("Run {}", pc.run),
                Run::Rpc { method: "session.open".into(), params: json!({"project_id": p.id, "kind": "monitor", "name": pc.name, "command": [pc.run]}) },
            )
            .sub(format!("{} · project command “{}”", p.name, pc.name))
            .kw(&format!("script command {}", pc.name))
            .cli(format!("midna open --project {} --name {:?} --monitor {:?}", p.id, pc.name, pc.run));
            match decision.as_deref() {
                Some("ask") => c.approval = Some("a rule asks before this runs".into()),
                Some("deny") => c = c.danger("A rule denies this command."),
                _ => {}
            }
            if pc.pinned && here.is_some_and(|h| h.id == p.id) {
                c = c.featured("Suggested");
            }
            out.push(c);
        }
    }

    // ---- Screens
    let rules_sub = match s.rules_count {
        0 => "no rules yet".to_string(),
        1 => "1 rule".to_string(),
        n => format!("{n} rules"),
    };
    out.push(
        Command::new("open.rules", CmdIcon::Rule, "Open Rules", Run::Screen { screen: "rules".into() })
            .sub(rules_sub)
            .kw("rules policy approvals gates screen")
            .keys(key("keys.rules"))
            .cli("midna rules list")
            .featured("Suggested"),
    );
    out.push(
        Command::new("open.triggers", CmdIcon::Trigger, "Open Triggers", Run::Screen { screen: "triggers".into() })
            .sub("webhooks and their deliveries")
            .kw("webhooks github bitbucket workflows screen")
            .keys(key("keys.triggers"))
            .cli("midna call trigger.list"),
    );
    out.push(
        Command::new("open.insights", CmdIcon::Screen, "Open Insights", Run::Screen { screen: "insights".into() })
            .sub("turns, messages, spend over time")
            .kw("stats spend cost usage screen graphs")
            .keys(key("keys.insights"))
            .cli("midna insights"),
    );
    out.push(
        Command::new("open.notifications", CmdIcon::Screen, "Open Notifications", Run::Screen { screen: "notifications".into() })
            .sub("everything midna told you; the status bar's bell")
            .kw("notifications history inbox bell unread alerts screen")
            .cli("midna call notify.history"),
    );
    out.push(
        Command::new("open.settings", CmdIcon::Screen, "Open Settings", Run::Screen { screen: "settings".into() })
            .sub("scripts, keys, permissions, secrets")
            .kw("preferences config screen")
            .keys(key("keys.settings"))
            .cli("midna settings list"),
    );
    out.push(
        Command::new("notify.clear", CmdIcon::Deny, "Clear notifications", Run::Rpc { method: "notify.clear".into(), params: json!({}) })
            .sub("removes every midna notification from Notification Center")
            .kw("notifications banners dismiss remove clear all notification center")
            .cli("midna notify clear"),
    );
    out.push(
        Command::new("report.issue", CmdIcon::Screen, "Report an issue…", Run::ReportIssue)
            .sub("opens a new GitHub issue with your midna, macOS and Mac details filled in")
            .kw("bug issue feedback github report problem crash broken file"),
    );
    out.push(
        Command::new("folder.pick", CmdIcon::Project, "Add project folder…", Run::Prefill { text: format!("{FOLDER_PREFIX}~/") })
            .sub("a folder your projects live in, like ~/Development")
            .kw("projects roots root directory folder workspace"),
    );
    out.push(
        Command::new("rule.add", CmdIcon::New, "Add a rule…", Run::Prefill { text: "Add a rule: ".into() })
            .sub("an agent drafts it, you review")
            .kw("rules policy new create gate"),
    );

    // ---- Terminal actions (selected first)
    let mut terms: Vec<&Session> = s.sessions.clone();
    if let Some(sel) = s.selected {
        terms.sort_by_key(|x| x.id != sel.id);
    }
    for x in terms {
        let is_sel = s.selected.is_some_and(|sel| sel.id == x.id);
        let pname = project_name(x.project_id.as_deref());
        let mut ontop = Command::new(format!("ontop:{}", x.id), CmdIcon::Pin, format!("Keep {} on top", x.name), Run::PopOut { session: x.id.clone() })
            .sub(format!("{pname} · floats above other apps"))
            .kw("pin float always front window pop out")
            .cli(format!("midna window keep_on_top {}", x.id));
        if is_sel {
            ontop = ontop.featured("Suggested");
        }
        out.push(ontop);
        out.push(
            Command::new(format!("popout:{}", x.id), CmdIcon::Pin, format!("Pop out {}", x.name), Run::PopOut { session: x.id.clone() })
                .sub(format!("{pname} · separate window"))
                .kw("window float detach")
                .cli(format!("midna window pop_out {}", x.id)),
        );
        let (title, sub, cli) = if x.background {
            (format!("Move {} to foreground", x.name), format!("{pname} · back under its project in the sidebar"), format!("midna background {} --off", x.id))
        } else {
            (format!("Move {} to background", x.name), format!("{pname} · folded away at the bottom of the sidebar, still running"), format!("midna background {}", x.id))
        };
        out.push(
            Command::new(format!("background:{}", x.id), CmdIcon::Pin, title, Run::Rpc { method: "session.set_background".into(), params: json!({"id": x.id, "background": !x.background}) })
                .sub(sub)
                .kw("background foreground hide unhide tuck away bg")
                .cli(cli),
        );
        out.extend(restart_commands(x, &pname));
        let queued = x.queue.len();
        let mut q = Command::new(format!("queue:{}", x.id), CmdIcon::Run, format!("Queue a message for {}", x.name), Run::Queue { session: x.id.clone() })
            .sub(if queued > 0 { format!("{pname} · {queued} queued, sent in order once it's ready") } else { format!("{pname} · sent once it's ready") })
            .kw("queue later next when idle follow up send after")
            .cli(format!("midna queue add --session {} <text>", x.id));
        if is_sel {
            q = q.keys(key("keys.queue"));
        }
        out.push(q);
        let working = x.status.state == StatusState::Working;
        out.push(
            Command::new(
                format!("close:{}", x.id),
                CmdIcon::Deny,
                format!("Close terminal: {}", x.name),
                Run::Rpc { method: "session.close".into(), params: json!({"id": x.id, "force": true}) },
            )
            .sub(format!("{pname} · ends the process and closes the tab"))
            .kw("kill stop end quit close")
            .cli(format!("midna close {} --force", x.id))
            .danger(if working { format!("{} is working; this ends it mid-task.", x.name) } else { format!("Ends {} and closes its tab.", x.name) }),
        );
    }

    // ---- App
    let theme = (s.setting)("theme").unwrap_or_else(|| "system".into());
    let current = midna_proto::themes::choose(&theme, "", "", true);
    let themes = crate::theme::cached_themes();
    let name = |id: &str| themes.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_else(|| id.to_string());
    let shown = if theme == "system" { "theme follows macOS".to_string() } else { format!("theme is {}", name(&current)) };
    for t in &themes {
        if theme != "system" && t.id == current {
            continue;
        }
        let kind = if t.dark { "dark" } else { "light" };
        out.push(
            Command::new(format!("theme:{}", t.id), CmdIcon::Screen, format!("Theme: {}", t.name), Run::Rpc { method: "settings.set".into(), params: json!({"key": "theme", "value": t.id}) })
                .sub(format!("{kind} · {shown}"))
                .kw(&format!("theme appearance color colour {kind} mode {}", t.id))
                .cli(format!("midna themes use {}", t.id)),
        );
    }
    if theme != "system" {
        out.push(
            Command::new("theme:system", CmdIcon::Screen, "Theme: follow macOS", Run::Rpc { method: "settings.set".into(), params: json!({"key": "theme", "value": "system"}) })
                .sub(format!("dark and light themes switch with macOS · {shown}"))
                .kw("theme appearance toggle color dark light mode system auto")
                .cli("midna themes use system"),
        );
    }
    let sounds_on = (s.setting)("notify.sounds").as_deref() != Some("false");
    out.push(
        Command::new(
            "sounds",
            CmdIcon::Screen,
            if sounds_on { "Mute sounds" } else { "Turn sounds on" },
            Run::Rpc { method: "settings.set".into(), params: json!({"key": "notify.sounds", "value": !sounds_on}) },
        )
        .sub(if sounds_on { "notification sounds and sound effects" } else { "sounds are off" })
        .kw("sound sounds mute unmute quiet silence audio effects volume")
        .cli(format!("midna settings set notify.sounds {}", !sounds_on)),
    );
    let density = (s.setting)("density").unwrap_or_else(|| "comfortable".into());
    let next = if density == "compact" { "comfortable" } else { "compact" };
    let mut dc = Command::new(
        "density",
        CmdIcon::Screen,
        if next == "compact" { "Compact sidebar" } else { "Comfortable sidebar" },
        Run::Rpc { method: "settings.set".into(), params: json!({"key": "density", "value": next}) },
    )
    .sub(if next == "compact" { "denser rows" } else { "roomier rows" })
    .kw("density toggle compact comfortable")
    .cli(format!("midna settings set density {next}"));
    dc.state = Some(density);
    out.push(dc);
    out.push(
        Command::new(
            "sidebar",
            CmdIcon::Screen,
            if s.sidebar_collapsed { "Expand sidebar" } else { "Collapse sidebar" },
            Run::Screen { screen: "sidebar".into() },
        )
        .sub(if s.sidebar_collapsed { "back to full rows" } else { "down to a rail of status dots" })
        .kw("sidebar collapse expand hide show toggle rail")
        .keys(key("keys.sidebar")),
    );
    // Hiding is offered once there is something to hide; showing, always while hidden.
    if s.background_hidden || s.sessions.iter().any(|x| x.background) {
        out.push(
            Command::new(
                "background",
                CmdIcon::Screen,
                if s.background_hidden { "Show background terminals in the sidebar" } else { "Hide background terminals from the sidebar" },
                Run::Screen { screen: "background".into() },
            )
            .sub(if s.background_hidden { "the folded Background group above Today" } else { "they keep running; find them here in ⌘K" })
            .kw("background bg hide show sidebar group"),
        );
    }
    out
}

/// `$MIDNA_HOME/commands.json`: extra commands from scripts (a JSON array of [`Command`]).
/// Unparsable entries are skipped with a log line.
pub fn load_user_commands(home: &std::path::Path) -> Vec<Command> {
    let Ok(text) = std::fs::read_to_string(home.join("commands.json")) else {
        return vec![];
    };
    parse_user_commands(&text)
}

/// commands.json is agent-writable, so a user command may only *offer* an action; the human
/// decides. Daemon methods that act with the human's authority beyond what an agent could do
/// itself (human-only methods, resolving needs-you items, settings, rules, triggers, webhooks,
/// daemon, updates) always need the two-step confirm, with the exact call as its text. A user
/// command can't resolve needs-you items directly, can't pose as a "Needs you" row or badge,
/// and its id is namespaced so it can't share an armed state with a built-in row.
pub fn sanitize_user_command(mut c: Command) -> Option<Command> {
    if matches!(c.run, Some(Run::Resolve { .. }) | None) {
        return None;
    }
    if !c.id.starts_with("user:") {
        c.id = format!("user:{}", if c.id.is_empty() { c.title.as_str() } else { c.id.as_str() });
    }
    c.pending = false;
    c.approval = None;
    c.approve_menu = false;
    c.state = None;
    if c.featured.as_deref().is_some_and(|f| f.trim().eq_ignore_ascii_case("needs you")) {
        c.featured = Some("Suggested".into());
    }
    if let Some(Run::Rpc { method, params }) = &c.run {
        let spec = midna_proto::method(method);
        let sensitive = spec.is_some_and(|m| m.human_only)
            || matches!(method.as_str(), "needs_you.resolve" | "settings.set" | "settings.reset")
            || ["rule.", "trigger.", "webhooks.", "daemon.", "updates."].iter().any(|p| method.starts_with(p));
        c.human_only = spec.is_some_and(|m| m.human_only);
        if sensitive {
            let call = if params.is_null() { "{}".to_string() } else { params.to_string() };
            let call: String = if call.chars().count() > 600 { format!("{}…", call.chars().take(600).collect::<String>()) } else { call };
            c.danger = Some(format!("Runs {method} {call} as you. This command came from commands.json, which agents can edit."));
        }
    }
    Some(c)
}

pub fn parse_user_commands(text: &str) -> Vec<Command> {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("midna-app: commands.json: {e}");
            return vec![];
        }
    };
    parse_list::<Command>(&v).into_iter().filter(|c| !c.title.is_empty()).filter_map(sanitize_user_command).collect()
}

// ------------------------------------------------------------------ matching

/// A match: score (higher is better) and the matched char indices of the title.
#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub score: f32,
    pub hits: Vec<usize>,
}

fn lower_chars(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

fn find(hay: &[char], needle: &[char], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

/// Subsequence match of `tok` in `t` with at most 2 gaps / 6 skipped chars; the tightest wins.
fn subseq(t: &[char], tok: &[char]) -> Option<(Vec<usize>, usize)> {
    let mut best: Option<(Vec<usize>, usize)> = None;
    for start in (0..t.len()).filter(|&i| t[i] == tok[0]) {
        let mut idx = vec![start];
        let (mut p, mut gaps, mut gap_chars, mut ok) = (start, 0, 0, true);
        for &c in &tok[1..] {
            match (p + 1..t.len()).find(|&j| t[j] == c) {
                Some(j) => {
                    if j > p + 1 {
                        gaps += 1;
                        gap_chars += j - p - 1;
                    }
                    idx.push(j);
                    p = j;
                }
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok && gaps <= 2 && gap_chars <= 6 && best.as_ref().is_none_or(|b| gap_chars < b.1) {
            best = Some((idx, gap_chars));
        }
    }
    best
}

/// Every whitespace-separated token must match: a substring of the title (best, more at a
/// word start), a keyword prefix, or a tight subsequence of the title (3+ chars).
pub fn fuzzy(query: &str, title: &str, keywords: &str) -> Option<Match> {
    let t = lower_chars(title);
    let words: Vec<Vec<char>> = keywords.split_whitespace().map(lower_chars).collect();
    let mut hits: Vec<usize> = vec![];
    let mut score = 0f32;
    let mut any = false;
    for tok in query.split_whitespace() {
        any = true;
        let tok = lower_chars(tok);
        if let Some(i) = find(&t, &tok, 0) {
            let word_start = i == 0 || !t[i - 1].is_alphanumeric();
            score += 70. + if word_start { 30. } else { 0. } - (i.min(30) as f32) / 3. + tok.len() as f32;
            hits.extend(i..i + tok.len());
            continue;
        }
        if words.iter().any(|w| w.starts_with(&tok) || (tok.len() >= 4 && w.len() >= 4 && tok.starts_with(w))) {
            score += 45.;
            for k in (3..tok.len()).rev() {
                if let Some(j) = find(&t, &tok[..k], 0) {
                    hits.extend(j..j + k);
                    break;
                }
            }
            continue;
        }
        if tok.len() >= 3
            && let Some((idx, gap_chars)) = subseq(&t, &tok)
        {
            score += 30. - gap_chars as f32 * 2.;
            hits.extend(idx);
            continue;
        }
        return None;
    }
    if !any {
        return None;
    }
    hits.sort_unstable();
    hits.dedup();
    Some(Match { score, hits })
}

/// A ranked result.
#[derive(Clone, Debug)]
pub struct Hit<'a> {
    pub cmd: &'a Command,
    pub hits: Vec<usize>,
    /// Group heading to show above this row (empty query only).
    pub head: Option<String>,
}

/// Empty query: featured commands grouped by heading (Needs you, then Suggested, then any
/// other headings in first-seen order). Otherwise the best `limit` matches by score.
pub fn search<'a>(cmds: &'a [Command], query: &str, limit: usize) -> Vec<Hit<'a>> {
    let q = query.trim();
    if q.is_empty() {
        let mut heads: Vec<&str> = vec!["Needs you", "Suggested"];
        for c in cmds {
            if let Some(f) = c.featured.as_deref()
                && !heads.contains(&f)
            {
                heads.push(f);
            }
        }
        let mut out = vec![];
        for h in heads {
            let mut first = true;
            for c in cmds.iter().filter(|c| c.featured.as_deref() == Some(h)) {
                out.push(Hit { cmd: c, hits: vec![], head: first.then(|| h.to_string()) });
                first = false;
            }
        }
        return out;
    }
    let mut scored: Vec<(f32, Hit)> = cmds
        .iter()
        .enumerate()
        .filter_map(|(n, c)| {
            let hay = if c.sub.is_empty() { c.keywords.clone() } else { format!("{} {}", c.keywords, c.sub) };
            fuzzy(q, &c.title, &hay).map(|m| (m.score + if c.pending { 8. } else { 0. } - n as f32 * 0.01, Hit { cmd: c, hits: m.hits, head: None }))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(limit).map(|(_, h)| h).collect()
}

/// How many recent projects the empty pane and ⌘K (before anything is typed) list.
pub const RECENT_MAX: usize = 5;

/// A project with no terminals open.
#[derive(Clone, Debug, PartialEq)]
pub struct RecentProject {
    pub name: String,
    pub path: String,
    /// "opened 2d ago", or the path when it has no open time yet.
    pub sub: String,
}

/// Closed projects, most recently opened first. Folders under `projects.roots` aren't
/// listed here: ⌘K finds them when searched for.
pub fn recent_projects(projects: &[Project], open: &[&str]) -> Vec<RecentProject> {
    let mut closed: Vec<&Project> = projects.iter().filter(|p| !open.contains(&p.id.as_str())).collect();
    closed.sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at).then(a.order.cmp(&b.order)));
    closed.into_iter().map(|p| RecentProject {
        name: p.name.clone(),
        path: p.path.clone(),
        sub: match p.last_opened_at.as_deref().map(since_short).as_deref() {
            None | Some("") => tilde(&p.path),
            Some("now") => "opened just now".into(),
            Some(ago) => format!("opened {ago} ago"),
        },
    })
    .collect()
}

/// "Open in <IDE>" rows for the selected terminal's folder: the one its rules pick first (with
/// keys.open_ide), then the other installed ones (choosing one remembers it for the project).
pub fn ide_commands(ides: &[crate::ide::Ide], current: Option<&str>, dir: &str, key: &dyn Fn(&str) -> String) -> Vec<Command> {
    let mut ordered: Vec<&crate::ide::Ide> = ides.iter().collect();
    ordered.sort_by_key(|i| Some(i.id.as_str()) != current);
    ordered
        .into_iter()
        .map(|i| {
            let is_default = Some(i.id.as_str()) == current;
            let c = Command::new(format!("ide:{}", i.id), CmdIcon::Ide, format!("Open in {}", i.name), Run::OpenIde { ide: i.id.clone(), dir: dir.to_string() })
                .sub(if is_default { tilde(dir) } else { format!("{} · remembered for this project", tilde(dir)) })
                .kw("ide editor code open folder project vscode cursor zed xcode jetbrains")
                .cli(format!("open -a {:?} {:?}", i.path.display().to_string(), dir));
            if is_default { c.keys(key("keys.open_ide")) } else { c }
        })
        .collect()
}

/// `path` with `$HOME` shown as `~`.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && (path == h || path.starts_with(&format!("{h}/"))) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

/// The CLI line the "Ask an agent" fallback is equivalent to.
pub fn ask_cli(agent: AgentKind, project: Option<&str>, prompt: &str) -> String {
    let p = project.map(|p| format!(" --project {p}")).unwrap_or_default();
    format!("midna open --agent {}{p} --prompt {:?}", agent_name(agent).to_lowercase(), short(prompt, 60))
}

/// `session.open` params for the "Ask an agent" fallback.
/// A query that names an existing folder ("~/Development/kass", "/tmp") offers to open it as a
/// project. `home` expands a leading `~`.
pub fn path_command(query: &str, home: &str, projects: &[Project]) -> Option<Command> {
    let q = query.trim();
    let path = match q.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ if q.starts_with('/') => q.to_string(),
        _ => return None,
    };
    let path = if path.len() > 1 { path.trim_end_matches('/').to_string() } else { path };
    if !std::path::Path::new(&path).is_dir() {
        return None;
    }
    let title = match projects.iter().find(|p| p.path == path) {
        Some(p) => format!("Open project {}", p.name),
        None => format!("Open {path} as a project"),
    };
    Some(Command::new(format!("project.open:{path}"), CmdIcon::Project, title, Run::OpenProject { path: Some(path.clone()) }).sub(path).cli(format!("midna project add {q}")))
}

/// ⌘K query prefix for picking a `projects.roots` folder ("Add project folder…").
pub const FOLDER_PREFIX: &str = "Project folder: ";

/// A query starting with this lists the selected agent terminal's prompts (⌘P).
pub const PROMPT_PREFIX: &str = ">";

/// Rows for a [`PROMPT_PREFIX`] query, newest first, from a `session.prompts` result. Prompts
/// sent before a /clear are listed but can't be jumped to (the agent no longer shows them).
pub fn prompt_rows(session: &str, agent: Option<AgentKind>, v: &Value) -> Vec<Command> {
    let icon = if agent == Some(AgentKind::Codex) { CmdIcon::Codex } else { CmdIcon::Claude };
    let here = v.get("here").and_then(Value::as_u64);
    let scrolled = v.get("scrolled").and_then(Value::as_bool).unwrap_or(false);
    let list = v.get("prompts").and_then(Value::as_array).cloned().unwrap_or_default();
    let live = (!list.is_empty()).then(|| Command {
        id: "prompt:live".into(),
        icon,
        title: "End of the conversation".into(),
        sub: if scrolled { "↓ live · write a new prompt".into() } else { "↓ live · you're here".into() },
        run: Some(Run::JumpPrompt { session: session.into(), n: None }),
        ..Default::default()
    });
    live.into_iter()
        .chain(list.iter().rev().filter_map(|p| {
            let n = p.get("n")?.as_u64()? as u32;
            let text = p.get("text").and_then(Value::as_str).unwrap_or("");
            let on_screen = p.get("on_screen").and_then(Value::as_bool).unwrap_or(true);
            let at = p.get("at").and_then(Value::as_str).unwrap_or("");
            let mut sub = format!("#{n} · {}", crate::ui::screen_kit::clock_or_day(at));
            if here == Some(n as u64) {
                sub.push_str(" · you're here");
            }
            if !on_screen {
                sub.push_str(" · before /clear");
            }
            let mut c = Command { id: format!("prompt:{n}"), icon, title: midna_proto::prompts::key(text), sub, ..Default::default() };
            if on_screen {
                c.run = Some(Run::JumpPrompt { session: session.into(), n: Some(n) });
            }
            Some(c)
        }))
        .collect()
}

/// Most folder completions listed.
const FOLDER_ROWS: usize = 12;

/// Rows for a [`FOLDER_PREFIX`] query: "Add <dir> as a project folder" when the typed path is a
/// folder, then its subfolders (or the folders in its parent that start with the typed name).
/// Picking a subfolder types it in, so ↩ walks down the tree.
pub fn folder_rows(query: &str, home: &str, roots: &[String]) -> Option<Vec<Command>> {
    let typed = query.strip_prefix(FOLDER_PREFIX)?.trim_start();
    let typed = if typed.is_empty() { "~/" } else { typed };
    let expand = |t: &str| match t.strip_prefix('~') {
        Some(rest) => format!("{home}{rest}"),
        None => t.to_string(),
    };
    let abs = expand(typed);
    let mut out = vec![];
    let trimmed = if abs.len() > 1 { abs.trim_end_matches('/') } else { abs.as_str() };
    if (typed.starts_with('/') || typed.starts_with('~')) && std::path::Path::new(trimmed).is_dir() {
        let shown = tilde(trimmed);
        let mut c = Command::new(format!("folder.add:{trimmed}"), CmdIcon::Project, format!("Add {shown} as a project folder"), Run::Prefill { text: query.into() })
            .sub("its subfolders show up as projects you can open");
        if roots.iter().any(|r| expand(r) == trimmed) {
            c = c.sub("already a project folder");
        } else {
            let mut all: Vec<String> = roots.to_vec();
            all.push(shown.clone());
            c.run = Some(Run::Rpc { method: "settings.set".into(), params: json!({"key": "projects.roots", "value": all}) });
            c = c.cli(format!("midna settings set projects.roots {}", all.join(",")));
        }
        out.push(c);
    }
    // list `dir`'s subfolders whose names start with `partial`
    let (dir, partial) = match abs.rfind('/') {
        Some(i) => (abs[..=i].to_string(), abs[i + 1..].to_lowercase()),
        None => return Some(out),
    };
    let mut subs: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    subs.retain(|n| !n.starts_with('.') && n.to_lowercase().starts_with(&partial));
    subs.sort_by_key(|n| n.to_lowercase());
    let typed_dir = &typed[..typed.rfind('/').map_or(0, |i| i + 1)];
    for n in subs.into_iter().take(FOLDER_ROWS) {
        let next = format!("{typed_dir}{n}/");
        out.push(Command::new(format!("folder.go:{next}"), CmdIcon::Project, next.clone(), Run::Prefill { text: format!("{FOLDER_PREFIX}{next}") }).sub("↩ to look inside"));
    }
    Some(out)
}

pub fn ask_params(agent: AgentKind, project: Option<&str>, prompt: &str) -> Value {
    let mut p = json!({"kind": "agent", "agent": agent, "prompt": prompt, "name": short(prompt, 28)});
    if let Some(pid) = project {
        p["project_id"] = json!(pid);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles<'a>(hits: &[Hit<'a>]) -> Vec<&'a str> {
        hits.iter().map(|h| h.cmd.title.as_str()).collect()
    }

    #[test]
    fn restart_entries_follow_what_the_agent_has_in_flight() {
        use midna_proto::{AgentInfo, BackgroundTask};
        let ids = |s: &Session| restart_commands(s, "midna").into_iter().map(|c| c.id.split(':').next().unwrap().to_string()).collect::<Vec<_>>();
        let shell = Session { id: "s0".into(), name: "zsh".into(), ..Default::default() };
        assert_eq!(ids(&shell), ["restart"]);
        let mut agent = Session { id: "s1".into(), name: "review".into(), kind: SessionKind::Agent, agent: Some(AgentKind::Claude), ..Default::default() };
        assert_eq!(ids(&agent), ["restart"], "no conversation reported yet: a plain restart");
        agent.agent_info = Some(AgentInfo { conversation_id: Some("c1".into()), update_available: Some("2.1.290".into()), ..Default::default() });
        assert_eq!(ids(&agent), ["restart", "restart-idle", "restart-fresh"]);
        assert!(restart_commands(&agent, "midna")[0].sub.contains("2.1.290 installed"));
        let info = agent.agent_info.as_mut().unwrap();
        info.background.push(BackgroundTask { id: "b1".into(), kind: "shell".into(), command: Some("npm run dev".into()), ..Default::default() });
        assert_eq!(ids(&agent), ["restart-idle", "restart-fresh"], "restarting now would stop the dev server");
        assert!(restart_commands(&agent, "midna")[0].sub.contains("npm run dev"));
        agent.agent_info.as_mut().unwrap().restart = Some(midna_proto::QueuedRestart {
            reason: "update".into(),
            queued_at: String::new(),
            by: midna_proto::Actor::system(),
            waiting_for: vec!["shell b1 running: npm run dev".into()],
        });
        assert_eq!(ids(&agent), ["restart-cancel", "restart-fresh"]);
    }

    #[test]
    fn typed_paths_offer_to_open_a_project() {
        let home = std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        let c = path_command("~", &home, &[]).expect("home is a dir");
        assert_eq!(c.run, Some(Run::OpenProject { path: Some(home.clone()) }));
        assert!(path_command("/", &home, &[]).is_some());
        assert!(path_command("/definitely/not/here", &home, &[]).is_none());
        assert!(path_command("deploy staging", &home, &[]).is_none());
        assert!(path_command("~foo", &home, &[]).is_none());
        let p = Project { id: "p_1".into(), name: "tmp".into(), path: home.clone(), ..Default::default() };
        assert_eq!(path_command("~/", &home, &[p]).unwrap().title, "Open project tmp");
    }

    #[test]
    fn substring_beats_keyword_beats_subsequence() {
        let sub = fuzzy("rules", "Open Rules", "").unwrap();
        let kw = fuzzy("rules", "Add a gate…", "rules policy").unwrap();
        let seq = fuzzy("rls", "Open Rules", "").unwrap();
        assert!(sub.score > kw.score && kw.score > seq.score, "{sub:?} {kw:?} {seq:?}");
        assert_eq!(sub.hits, (5..10).collect::<Vec<_>>());
        assert_eq!(seq.hits, vec![5, 7, 9]);
    }

    #[test]
    fn word_start_and_position_matter() {
        let start = fuzzy("dev", "Go to dev server", "").unwrap();
        let inner = fuzzy("dev", "Go to undevised", "").unwrap();
        assert!(start.score > inner.score);
    }

    #[test]
    fn every_token_must_match() {
        assert!(fuzzy("kill dev", "Close terminal: dev server", "kill stop").is_some());
        assert!(fuzzy("kill zzz", "Close terminal: dev server", "kill stop").is_none());
        assert!(fuzzy("", "anything", "").is_none());
        // two-letter tokens never match as a loose subsequence
        assert!(fuzzy("xq", "Open Rules", "").is_none());
    }

    #[test]
    fn subsequence_is_tight() {
        assert!(fuzzy("mgt", "migrate", "").is_some());
        assert!(fuzzy("mte", "m-a-b-c-d-e-f-g-t-e", "").is_none(), "too many skipped chars");
    }

    #[test]
    fn case_insensitive_and_unicode_safe() {
        let m = fuzzy("RULE", "Confirm rule removal: ask before git push", "").unwrap();
        assert_eq!(m.hits, (8..12).collect::<Vec<_>>());
        assert!(fuzzy("über", "Grüße über alles", "").is_some());
    }

    fn snapshot_fixture() -> (Vec<Project>, Vec<Session>, Vec<NeedsYou>) {
        let projects = vec![
            Project {
                id: "p_midna".into(),
                name: "midna".into(),
                path: "/src/midna".into(),
                commands: vec![ProjectCommand { name: "test".into(), run: "cargo test".into(), pinned: true }],
                ..Default::default()
            },
            Project { id: "p_api".into(), name: "api".into(), path: "/src/api".into(), ..Default::default() },
        ];
        let sessions = vec![
            Session { id: "s1".into(), project_id: Some("p_midna".into()), name: "review".into(), kind: SessionKind::Agent, agent: Some(AgentKind::Claude), ..Default::default() },
            Session { id: "s2".into(), project_id: Some("p_api".into()), name: "dev server".into(), kind: SessionKind::Monitor, ..Default::default() },
        ];
        let needs = vec![
            NeedsYou {
                id: "n_1".into(),
                session_id: Some("s1".into()),
                kind: NeedsYouKind::Approval,
                title: "Run git push".into(),
                bulk_safe: true,
                approval: Some(ApprovalRequest { action: PolicyAction { kind: "command".into(), value: "git push --force".into(), ..Default::default() }, matched_rule: None }),
                ..Default::default()
            },
            NeedsYou { id: "n_2".into(), kind: NeedsYouKind::RuleRemoval, title: "ask before rm -rf".into(), ..Default::default() },
        ];
        (projects, sessions, needs)
    }

    fn build_fixture(projects: &[Project], sessions: &[Session], needs: &[NeedsYou], policy: &dyn Fn(&str) -> Option<String>) -> Vec<Command> {
        let key = |k: &str| match k {
            "keys.new_terminal" => "⌘T".to_string(),
            "keys.approve" => "⌘↩".to_string(),
            _ => String::new(),
        };
        let setting = |k: &str| (k == "theme").then(|| "dark".to_string());
        let snap = Snapshot {
            projects,
            discovered: &[],
            sessions: sessions.iter().collect(),
            needs,
            selected: sessions.first(),
            current_project: projects.first(),
            key: &key,
            setting: &setting,
            rules_count: 3,
            last_agent: AgentKind::Claude,
            policy,
            sidebar_collapsed: false,
            background_hidden: false,
            name_note: &|_| None,
        };
        build(&snap)
    }

    #[test]
    fn folder_search_walks_down_and_adds_a_root() {
        let home = std::env::temp_dir().join(format!("midna-folders-{}", std::process::id()));
        for d in ["Development/kass", "Development/rust", "Documents", ".hidden"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        let home = home.to_string_lossy().into_owned();
        let q = |t: &str| format!("{FOLDER_PREFIX}{t}");
        assert!(folder_rows("~/Dev", &home, &[]).is_none(), "only with the prefix");
        let titles = |rows: Vec<Command>| rows.into_iter().map(|c| c.title).collect::<Vec<_>>();
        // home itself, then its folders (no hidden ones)
        let rows = titles(folder_rows(&q("~/"), &home, &[]).unwrap());
        assert!(rows[0].starts_with("Add ") && rows[0].ends_with(" as a project folder"));
        assert_eq!(rows[1..], ["~/Development/", "~/Documents/"]);
        // a partial name filters, case-insensitively; picking one types it in
        let rows = folder_rows(&q("~/dev"), &home, &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].run, Some(Run::Prefill { text: q("~/Development/") }));
        // a folder adds itself to the existing list
        let rows = folder_rows(&q("~/Development/"), &home, &["~/work".into()]).unwrap();
        let Some(Run::Rpc { method, params }) = &rows[0].run else { panic!("{:?}", rows[0].run) };
        assert_eq!(method, "settings.set");
        assert_eq!(params["value"][0], "~/work");
        assert!(params["value"][1].as_str().unwrap().ends_with("/Development"));
        assert_eq!(titles(rows[1..].to_vec()), ["~/Development/kass/", "~/Development/rust/"]);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn recent_lists_opened_projects_and_folders_only_show_when_searched() {
        let p = |id: &str, at: Option<&str>, order: u32| Project { id: id.into(), name: id.into(), path: format!("/src/{id}"), order, last_opened_at: at.map(str::to_string), ..Default::default() };
        let projects = [
            p("open", Some("2026-10-03T10:00:00Z"), 0),
            p("old", Some("2026-09-01T00:00:00Z"), 1),
            p("new", Some("2026-10-02T00:00:00Z"), 2),
            p("never", None, 3),
            p("mid", Some("2026-09-15T00:00:00Z"), 4),
            p("older", Some("2026-08-01T00:00:00Z"), 5),
        ];
        let names = |open: &[&str]| recent_projects(&projects, open).into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(&["open"]), ["new", "mid", "old", "older", "never"]);

        let c = |name: &str, project_id: Option<&str>| ProjectCandidate { name: name.into(), path: format!("/src/{name}"), root: "~/src".into(), project_id: project_id.map(str::to_string), ..Default::default() };
        let discovered = [c("kass", None), c("new", Some("new"))];
        let key = |_: &str| String::new();
        let setting = |_: &str| None;
        let snap = Snapshot {
            projects: &projects,
            discovered: &discovered,
            sessions: vec![],
            needs: &[],
            selected: None,
            current_project: None,
            key: &key,
            setting: &setting,
            rules_count: 0,
            last_agent: AgentKind::Claude,
            policy: &|_| None,
            sidebar_collapsed: false,
            background_hidden: false,
            name_note: &|_| None,
        };
        let cmds = build(&snap);
        // nothing typed: the five most recently opened (no terminals here, so "open" counts)
        let shown: Vec<&str> = search(&cmds, "", 50).into_iter().filter(|h| h.cmd.featured.as_deref() == Some("Recent")).map(|h| h.cmd.title.as_str()).collect();
        assert_eq!(shown, ["Go to project open", "Go to project new", "Go to project mid", "Go to project old", "Go to project older"]);
        // folders under projects.roots: searchable, never listed up front, once per path
        assert_eq!(search(&cmds, "kass", 5)[0].cmd.run, Some(Run::OpenProject { path: Some("/src/kass".into()) }));
        assert!(!cmds.iter().any(|c| c.id == "folder:/src/new"));
    }

    #[test]
    fn background_terminals_get_move_and_sidebar_commands() {
        let key = |_: &str| String::new();
        let setting = |_: &str| None;
        let bg = Session { id: "b1".into(), name: "dev server".into(), background: true, ..Default::default() };
        let fg = Session { id: "f1".into(), name: "api".into(), ..Default::default() };
        let build_with = |sessions: Vec<&Session>, hidden: bool| {
            build(&Snapshot {
                projects: &[],
                discovered: &[],
                sessions,
                needs: &[],
                selected: None,
                current_project: None,
                key: &key,
                setting: &setting,
                rules_count: 0,
                last_agent: AgentKind::Claude,
                policy: &|_| None,
                sidebar_collapsed: false,
                background_hidden: hidden,
                name_note: &|_| None,
            })
        };
        let title = |cmds: &[Command], id: &str| cmds.iter().find(|c| c.id == id).map(|c| c.title.clone());
        let cmds = build_with(vec![&bg, &fg], false);
        assert_eq!(title(&cmds, "background:b1").as_deref(), Some("Move dev server to foreground"));
        assert_eq!(title(&cmds, "background:f1").as_deref(), Some("Move api to background"));
        assert_eq!(title(&cmds, "background").as_deref(), Some("Hide background terminals from the sidebar"));
        // Nothing to hide: no sidebar row. Hidden: the row to bring it back, always.
        assert_eq!(title(&build_with(vec![&fg], false), "background"), None);
        assert_eq!(title(&build_with(vec![], true), "background").as_deref(), Some("Show background terminals in the sidebar"));
    }

    #[test]
    fn project_with_only_background_terminals_is_closed() {
        let key = |_: &str| String::new();
        let setting = |_: &str| None;
        let projects = [Project { id: "p1".into(), name: "api".into(), path: "/src/api".into(), ..Default::default() }];
        let bg = Session { id: "b1".into(), project_id: Some("p1".into()), background: true, ..Default::default() };
        let fg = Session { id: "f1".into(), project_id: Some("p1".into()), ..Default::default() };
        let sub = |sessions: Vec<&Session>| {
            let cmds = build(&Snapshot {
                projects: &projects,
                discovered: &[],
                sessions,
                needs: &[],
                selected: None,
                current_project: None,
                key: &key,
                setting: &setting,
                rules_count: 0,
                last_agent: AgentKind::Claude,
                policy: &|_| None,
                sidebar_collapsed: false,
                background_hidden: false,
                name_note: &|_| None,
            });
            cmds.into_iter().find(|c| c.id == "project:p1").map(|c| (c.sub, c.keys)).unwrap()
        };
        assert_eq!(sub(vec![&bg]), ("/src/api · closed, opens a terminal".to_string(), None));
        assert_eq!(sub(vec![&bg, &fg]), ("/src/api".to_string(), Some("⌘1".to_string())));
    }

    #[test]
    fn registry_is_built_from_live_state() {
        let (p, s, n) = snapshot_fixture();
        let cmds = build_fixture(&p, &s, &n, &|run| (run == "cargo test").then(|| "ask".to_string()));
        let get = |id: &str| cmds.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("missing {id}"));
        // ids are unique
        let mut ids: Vec<&str> = cmds.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate command ids");

        let approve = get("need:n_1:approve");
        assert!(approve.pending && approve.approve_menu);
        assert_eq!(approve.keys.as_deref(), Some("⌘↩"), "selected session's approval shows the shortcut");
        assert_eq!(approve.run, Some(Run::Resolve { need: "n_1".into(), resolution: Resolution::Approve { scope: ApprovalScope::Once } }));

        let removal = get("need:n_2:remove");
        assert!(removal.danger.is_some() && removal.human_only);

        assert_eq!(get("focus:s2").run, Some(Run::Focus { session: "s2".into() }));
        assert_eq!(get("new.terminal").keys.as_deref(), Some("⌘T"));
        assert!(get("new.terminal").title.ends_with("in midna"));
        assert!(get("close:s2").danger.is_some());
        assert_eq!(get("project:p_api").keys.as_deref(), Some("⌘2"));

        let run = get("run:p_midna:test");
        assert!(run.approval.is_some(), "policy ask becomes a needs-approval badge");
        assert_eq!(run.featured.as_deref(), Some("Suggested"));
        match &run.run {
            Some(Run::Rpc { method, params }) => {
                assert_eq!(method, "session.open");
                assert_eq!(params["command"], json!(["cargo test"]));
            }
            other => panic!("{other:?}"),
        }
        // the current theme is not offered
        // theme = dark (legacy) is Twilight: every other theme, and "follow macOS", is offered.
        assert!(cmds.iter().all(|c| c.id != "theme:twilight"));
        assert!(cmds.iter().any(|c| c.id == "theme:daylight"));
        assert!(cmds.iter().any(|c| c.id == "theme:nord"));
        assert!(cmds.iter().any(|c| c.id == "theme:system"));
    }

    #[test]
    fn empty_query_groups_featured() {
        let (p, s, n) = snapshot_fixture();
        let cmds = build_fixture(&p, &s, &n, &|_| None);
        let hits = search(&cmds, "  ", 8);
        assert_eq!(hits[0].head.as_deref(), Some("Needs you"));
        let first_suggested = hits.iter().position(|h| h.cmd.featured.as_deref() == Some("Suggested")).unwrap();
        assert_eq!(hits[first_suggested].head.as_deref(), Some("Suggested"));
        assert!(hits[..first_suggested].iter().all(|h| h.cmd.featured.as_deref() == Some("Needs you")));
        assert_eq!(hits.iter().filter(|h| h.head.is_some()).count(), 2);
    }

    #[test]
    fn search_ranks_and_limits() {
        let (p, s, n) = snapshot_fixture();
        let cmds = build_fixture(&p, &s, &n, &|_| None);
        let top = titles(&search(&cmds, "rules", 8));
        assert_eq!(top[0], "Open Rules");
        let kill = titles(&search(&cmds, "kill dev", 8));
        assert_eq!(kill[0], "Close terminal: dev server");
        // project words in the subtitle match too
        assert!(titles(&search(&cmds, "dev api", 8)).contains(&"Go to dev server"));
        assert!(search(&cmds, "e", 3).len() <= 3);
        assert!(search(&cmds, "why does the build fail on main", 8).is_empty(), "free text falls through to Ask");
    }

    #[test]
    fn user_commands_parse() {
        let cmds = parse_user_commands(
            r#"[{"title":"Deploy staging","keywords":"ship","danger":"Deploys.","run":{"kind":"rpc","method":"session.open","params":{"kind":"monitor","command":["./deploy.sh"]}}},
                {"title":"no run"},
                {"title":"Focus","run":{"kind":"focus","session":"s1"}}]"#,
        );
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].id, "user:Deploy staging");
        assert!(cmds[0].danger.is_some());
        assert_eq!(cmds[1].run, Some(Run::Focus { session: "s1".into() }));
    }

    #[test]
    fn user_commands_cannot_auto_run_human_actions() {
        let cmds = parse_user_commands(
            r#"[{"id":"focus:s1","title":"Open tests","danger":null,"run":{"kind":"rpc","method":"needs_you.resolve","params":{"id":"n_1","resolution":{"approve":{"scope":"always"}}}}},
                {"title":"Harmless","danger":"nothing","pending":true,"featured":"Needs you","approval":"x","run":{"kind":"rpc","method":"trigger.set_enabled","params":{"id":"t_1","enabled":true}}},
                {"title":"Sneaky resolve","run":{"kind":"resolve","need":"n_1","resolution":{"approve":{"scope":"once"}}}},
                {"title":"Stop","run":{"kind":"rpc","method":"daemon.stop"}},
                {"title":"Shell","run":{"kind":"rpc","method":"session.open","params":{"kind":"shell"}}}]"#,
        );
        let titles: Vec<&str> = cmds.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Open tests", "Harmless", "Stop", "Shell"], "resolve commands are dropped");
        let open = &cmds[0];
        assert_eq!(open.id, "user:focus:s1", "can't share a built-in row's id");
        assert!(open.danger.as_deref().unwrap().contains("needs_you.resolve"), "armed with the exact call");
        let h = &cmds[1];
        assert!(h.danger.as_deref().unwrap().starts_with("Runs trigger.set_enabled"), "agent wording replaced");
        assert!(!h.pending && h.approval.is_none() && !h.approve_menu);
        assert_eq!(h.featured.as_deref(), Some("Suggested"));
        assert!(cmds[2].human_only && cmds[2].danger.is_some());
        assert!(cmds[3].danger.is_none(), "ordinary commands keep one-step run");
    }

    #[test]
    fn ask_fallback_params() {
        let p = ask_params(AgentKind::Codex, Some("p_api"), "why does it fail");
        assert_eq!(p["kind"], "agent");
        assert_eq!(p["agent"], "codex");
        assert_eq!(p["prompt"], "why does it fail");
        assert_eq!(p["project_id"], "p_api");
        assert!(ask_params(AgentKind::Claude, None, "x").get("project_id").is_none());
        assert!(ask_cli(AgentKind::Claude, None, "hi").starts_with("midna open --agent claude --prompt"));
    }
}
