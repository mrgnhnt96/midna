//! Domain types shared by the daemon, the GUI, the CLI and MCP. See docs/ARCHITECTURE.md
//! "Domain model". Everything is snake_case on the wire; optional fields are omitted when None.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type Id = String;

/// `project_id` of a session opened outside any project. It starts in `$HOME`; the app
/// shows these terminals under a "root" group.
pub const ROOT_PROJECT_ID: &str = "root";
/// RFC 3339 UTC timestamp, e.g. `2026-10-03T14:07:00Z`.
pub type Timestamp = String;

// ------------------------------------------------------------------ actors

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    Agent,
    System,
    Trigger,
}

/// Who did something. Agents inside a midna terminal carry their session id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Actor {
    pub kind: ActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Actor {
    pub fn human() -> Actor {
        Actor { kind: ActorKind::Human, session: None, name: None }
    }
    pub fn system() -> Actor {
        Actor { kind: ActorKind::System, session: None, name: None }
    }
    pub fn agent(session: Option<Id>) -> Actor {
        Actor { kind: ActorKind::Agent, session, name: None }
    }
    pub fn is_human(&self) -> bool {
        self.kind == ActorKind::Human
    }
}

// ------------------------------------------------------------------ projects

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ProjectCommand {
    pub name: String,
    pub run: String,
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Project {
    pub id: Id,
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub order: u32,
    #[serde(default)]
    pub commands: Vec<ProjectCommand>,
    /// When a terminal last opened in it (RFC 3339). Orders "Recent" projects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<String>,
}

/// A folder under one of the `projects.roots` settings that could be opened as a project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ProjectCandidate {
    pub name: String,
    /// Absolute path.
    pub path: String,
    /// The `projects.roots` entry it was found under, as written in the setting.
    pub root: String,
    /// Has a `.git` entry.
    pub git: bool,
    /// The project already registered at this path, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    /// Folder modification time (RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

// ------------------------------------------------------------------ sessions

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    Shell,
    Agent,
    Monitor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Claude,
    Codex,
}

impl AgentKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusState {
    Idle,
    Working,
    NeedsYou,
    Done,
    Failed,
    Exited,
}

impl StatusState {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusState::Idle => "idle",
            StatusState::Working => "working",
            StatusState::NeedsYou => "needs_you",
            StatusState::Done => "done",
            StatusState::Failed => "failed",
            StatusState::Exited => "exited",
        }
    }
    /// The process is gone (failed or exited).
    pub fn is_terminal(&self) -> bool {
        matches!(self, StatusState::Failed | StatusState::Exited)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Status {
    pub state: StatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub since: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    None,
    Pending,
    Passing,
    Failing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PrInfo {
    pub number: u64,
    pub url: String,
    pub checks: ChecksState,
    #[serde(default)]
    pub failing_count: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct GitInfo {
    pub branch: String,
    pub ahead: u32,
    pub behind: u32,
    pub added: u32,
    pub removed: u32,
    pub files: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrInfo>,
}

/// A terminal: a shell, monitor or agent running in a PTY owned by midnad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Session {
    pub id: Id,
    pub project_id: Id,
    pub name: String,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    pub cwd: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    #[serde(default)]
    pub title: String,
    pub status: Status,
    pub created_at: Timestamp,
    pub last_activity_at: Timestamp,
    #[serde(default)]
    pub keep_on_top: bool,
    /// A background terminal: it runs and is tracked like any other (list, read, send, needs-you),
    /// but the GUI keeps it out of the sidebar's project lists, in a folded, dimmed "Background"
    /// group at the bottom. Set with `session.open` or `session.set_background`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitInfo>,
    /// Agent terminals: what the agent is running, as its hooks report it (see `AgentInfo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<AgentInfo>,
    /// This terminal's notification overrides: `enabled: false` mutes it, and a category key
    /// (`turn_done`, `pr_checks`, …) turns that kind on or off here only. Unset keys follow the
    /// global `notify.*` settings. Change with `notify.set`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub notify: std::collections::BTreeMap<String, bool>,
    /// A label and color a local trigger put on this terminal (`set_status`); shown instead of
    /// the built-in status, which still drives sorting, notifications and Needs You.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_status: Option<CustomStatus>,
    /// Messages waiting to be typed into this terminal, in order (`queue.*`). midnad types the
    /// first one once its `when` holds and the agent is ready for input, then the next.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<QueuedMessage>,
    /// The queue is paused: nothing is typed until it is resumed (`queue.pause`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queue_paused: bool,
}

// ------------------------------------------------------------------ queued messages

/// When a queued message may be typed. Every condition also waits until the agent is ready
/// for input (not working or waiting on the human, no prompt or dialog on screen, nothing
/// typed in its input box).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SendWhen {
    /// As soon as the agent is ready.
    #[default]
    Idle,
    /// Once the terminal has been idle for `minutes` (no prompt, turn or output).
    IdleFor { minutes: u32 },
    /// At or after a time (RFC 3339).
    At { at: Timestamp },
    /// Once another terminal has finished: it is idle (or its process ended, or it closed) and
    /// its own queue is empty.
    After { session: Id },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    /// Waiting for its turn and its `when`.
    #[default]
    Waiting,
    /// Being typed right now.
    Sending,
    /// Typing failed (`error` says why). It stays first and holds the queue until it is
    /// retried (`queue.update` with `retry`), sent now, or removed.
    Failed,
}

/// One message in a terminal's queue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct QueuedMessage {
    pub id: Id,
    pub text: String,
    /// Press Enter after the text (submit it). False types it into the input box only.
    #[serde(default = "yes")]
    pub enter: bool,
    /// Images attached before the text (absolute paths), as `session.input` takes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    #[serde(default)]
    pub when: SendWhen,
    pub by: Actor,
    pub queued_at: Timestamp,
    #[serde(default)]
    pub state: QueueState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The local trigger that queued it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<Id>,
    /// First in line only: what it is waiting for right now, in plain words (empty = about to go).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for: Vec<String>,
}

/// What an agent terminal's agent is doing beyond its turn, from its hooks: the conversation
/// to resume, the version running, background work in flight, scheduled wakeups, and a
/// queued restart. Claude reports background work and crons in every `Stop`/`SubagentStop`
/// (a full snapshot), subagents in `SubagentStart`/`SubagentStop`, and the running version in
/// its status line. Codex reports only its thread id (notify `thread-id`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AgentInfo {
    /// The agent's own conversation id: Claude `session_id`, Codex `thread-id`. What a
    /// restart resumes (`claude --resume <id>`, `codex resume <id>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    /// Version of the agent process that is running (Claude's status line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Model id the agent is using now (Claude's status line); kept across a resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Permission mode the agent is in now (`default`, `plan`, `acceptEdits`, …); kept across a resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// A newer version is installed than the one running: a restart picks it up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_available: Option<String>,
    /// In-flight background work (shells, subagents, monitors, workflows, MCP tasks) as of
    /// `background_at`. Replaced by each `Stop`; shells and agents started mid-turn are added live.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub background: Vec<BackgroundTask>,
    /// Scheduled wakeups (Claude session crons). They come back with a resume, so they don't
    /// hold a restart; a fresh start drops them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crons: Vec<AgentCron>,
    /// Subagents running right now (started, not yet stopped), foreground or background.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagents: Vec<Subagent>,
    /// Subagents that stopped since the human's last prompt, newest last (at most 10).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finished_subagents: Vec<Subagent>,
    /// Agent tool calls seen in `PreToolUse` whose `SubagentStart` has not come yet: where a
    /// subagent's description comes from, since `SubagentStart` carries only its id and type.
    #[serde(skip)]
    #[schemars(skip)]
    pub pending_agents: Vec<PendingAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_at: Option<Timestamp>,
    /// A restart waiting for the agent to be idle with nothing in flight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart: Option<QueuedRestart>,
}

impl AgentInfo {
    /// Why restarting the agent now would lose work, one line each (empty = safe).
    pub fn in_flight(&self) -> Vec<String> {
        let mut out = vec![];
        for t in &self.background {
            let what = t.command.as_deref().or(t.name.as_deref()).or(t.agent_type.as_deref()).unwrap_or(&t.description);
            out.push(format!("{} {} running: {}", t.kind, t.id, clip(what, 80)));
        }
        for a in &self.subagents {
            let what = if a.description.is_empty() { String::new() } else { format!(": {}", clip(&a.description, 80)) };
            out.push(format!("subagent {} ({}) running{what}", a.id, a.agent_type));
        }
        out
    }
}

fn clip(s: &str, n: usize) -> String {
    let s = s.lines().next().unwrap_or("");
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct BackgroundTask {
    pub id: String,
    /// `shell`, `subagent`, `monitor`, `workflow`, `MCP task`, … (Claude's label).
    pub kind: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct AgentCron {
    pub id: String,
    pub schedule: String,
    #[serde(default)]
    pub recurring: bool,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Subagent {
    pub id: String,
    #[serde(default)]
    pub agent_type: String,
    /// The short task the main agent gave it (the Agent tool's `description`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Launched with `run_in_background`: the main turn can end while it runs.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    pub started_at: Timestamp,
    /// Set once it stopped (in `finished_subagents`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Timestamp>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PendingAgent {
    pub tool_use_id: String,
    pub agent_type: String,
    pub description: String,
    pub background: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct QueuedRestart {
    /// Why: `update 2.1.289 -> 2.1.290`, `settings changed`, or what the requester said.
    pub reason: String,
    pub queued_at: Timestamp,
    pub by: Actor,
    /// What it is waiting for right now (empty once it is about to run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_for: Vec<String>,
}

/// One OS process under a terminal (`session.processes`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ProcessInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// 0 = the terminal's own process, 1 = its child, …
    pub depth: u32,
    pub name: String,
    /// Full command line (may be empty when the OS won't say).
    #[serde(default)]
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// The agent's background task this process belongs to, when its command matches one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

// ------------------------------------------------------------------ policy

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Command,
    Tool,
    Path,
    Cli,
    Window,
}

/// Something an agent wants to do, evaluated against rules. `value` examples:
/// `git push --force origin main` (command), `Bash(rm -rf build)` (tool), `close --force` (cli).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct PolicyAction {
    pub kind: ActionKind,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Id>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Allow,
    Ask,
    Deny,
}

impl Effect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Effect::Allow => "allow",
            Effect::Ask => "ask",
            Effect::Deny => "deny",
        }
    }
}

/// Glob matcher (`*` any run, `?` one char, `\` escapes) over the whole action value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Matcher {
    pub kind: ActionKind,
    pub pattern: String,
}

/// Where a rule applies. Wire form: `{"kind":"global"}`, `{"kind":"project","id":"p_1a2b3c"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum RuleScope {
    Global,
    Project(Id),
    Session(Id),
}

impl RuleScope {
    /// 0 = global, 1 = project, 2 = session. Higher wins.
    pub fn specificity(&self) -> u8 {
        match self {
            RuleScope::Global => 0,
            RuleScope::Project(_) => 1,
            RuleScope::Session(_) => 2,
        }
    }
}

/// How long an approval lasts. Wire form: `{"kind":"once"}`, `{"kind":"minutes","minutes":10}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApprovalScope {
    Once,
    Minutes { minutes: u32 },
    Session,
    Always,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RuleOrigin {
    pub needs_you_id: Id,
    pub approval_scope: ApprovalScope,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct RemovalRequest {
    pub requested_by: Actor,
    pub reason: String,
    pub at: Timestamp,
    pub needs_you_id: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Rule {
    pub id: Id,
    pub effect: Effect,
    pub matcher: Matcher,
    pub scope: RuleScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
    pub added_by: Actor,
    pub added_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<RuleOrigin>,
    #[serde(default)]
    pub fired: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removal_request: Option<RemovalRequest>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TraceEntry {
    pub rule_id: Id,
    pub matched: bool,
    pub reason: String,
}

/// Where a policy decision came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// A rule matched.
    Rule,
    /// No rule matched; the built-in default table / `policy.default` setting decided.
    Default,
    /// A human answered the approval.
    Human,
    /// Nobody answered within the timeout.
    Timeout,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CheckResult {
    pub decision: Effect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<Rule>,
    pub trace: Vec<TraceEntry>,
    pub source: DecisionSource,
}

// ------------------------------------------------------------------ needs-you

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NeedsYouKind {
    Approval,
    PermissionPrompt,
    Blocked,
    Note,
    Failed,
    TriggerWaiting,
    RuleRemoval,
    SecretNeeded,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct ApprovalRequest {
    pub action: PolicyAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<Id>,
}

/// How a needs-you item was resolved. Wire form: `{"kind":"approve","scope":{"kind":"once"}}`, `{"kind":"deny"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Resolution {
    Approve { scope: ApprovalScope },
    Deny,
    /// Human only.
    Dismiss,
    /// "I've done it."
    Done,
    /// Restart the failed session.
    Restart,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct NeedsYou {
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    pub kind: NeedsYouKind,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen_excerpt: Option<Vec<String>>,
    pub asked_by: Actor,
    pub created_at: Timestamp,
    #[serde(default)]
    pub bulk_safe: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalRequest>,
    /// The trigger this item is about (e.g. `secret_needed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<Id>,
}

// ------------------------------------------------------------------ triggers (later phase)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerSource {
    Github,
    Bitbucket,
    /// Something midnad sees on this Mac: an agent hook, a midna event or an idle terminal.
    Local,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct TriggerFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Local: only this terminal (session id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Id>,
    /// Local: only terminals in this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Id>,
    /// Local: only this agent (`claude` or `codex`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
    /// Local `idle` triggers: minutes without a turn starting or ending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_minutes: Option<u32>,
    /// Local `schedule` triggers: when to fire, a five-field cron expression in local time
    /// (`0 9 * * mon-fri`, `*/30 * * * *`, `@daily`). See `cron.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    /// Local: dotted path into the hook payload or event data -> case-insensitive glob, e.g.
    /// `{"message": "*Compact first*"}`. Every entry must match.
    #[serde(default, rename = "match", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
}

/// One step of `send_to_session`: text typed into the terminal, then Enter. `{{last_prompt}}`
/// is the terminal's most recent prompt (in full) as of the firing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct SendStep {
    pub text: String,
    #[serde(default = "yes")]
    pub enter: bool,
}

fn yes() -> bool {
    true
}

/// When a custom status set by a trigger goes away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatusClear {
    /// The next prompt sent to the agent.
    #[default]
    Prompt,
    /// The next agent turn starting.
    Turn,
    /// Any change of the built-in status away from `base`.
    Status,
    /// Only a `clear_status` trigger (or the terminal exiting).
    Never,
}

/// What a trigger does. Templates use `{{pr.number}}`, `{{pr.title}}`, `{{repo}}`, `{{branch}}`, `{{sender}}`, `{{url}}`. Templates use `{{pr.number}}`, `{{pr.title}}`, `{{repo}}`, `{{branch}}`, `{{sender}}`, `{{url}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TriggerAction {
    StartAgent { project_id: Id, agent: AgentKind, prompt_template: String },
    RunCommand { project_id: Id, command: String },
    Attention { message: String },
    /// Local only: type `steps` into the terminal that fired, in order. Each step after the
    /// first waits until the agent is ready for input again.
    SendToSession { steps: Vec<SendStep> },
    /// Local only: show `label` in `color` (named: red, orange, amber, yellow, green, teal, blue,
    /// purple, pink, gray; or `#rrggbb`) on the terminal that fired, on top of the built-in
    /// `base` state (which keeps driving sorting, notifications and Needs You).
    SetStatus {
        label: String,
        color: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
        base: StatusState,
        #[serde(default)]
        clear_on: StatusClear,
    },
    /// Local only: remove the custom status from the terminal that fired.
    ClearStatus {},
    /// Post a notification (category `from_trigger`: its sound, volume, image and mute switches
    /// apply). Clicking it selects the terminal that fired, if any. Templates work in both texts.
    Notify {
        title: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        body: String,
        /// Play the category's sound (`notify.sound.from_trigger`).
        #[serde(default = "yes")]
        sound: bool,
    },
}

impl TriggerAction {
    /// The project the action opens a terminal in.
    pub fn project_id(&self) -> Option<&Id> {
        match self {
            TriggerAction::StartAgent { project_id, .. } | TriggerAction::RunCommand { project_id, .. } => Some(project_id),
            _ => None,
        }
    }

    /// Acts on the terminal that fired (local triggers only).
    pub fn needs_session(&self) -> bool {
        matches!(self, TriggerAction::SendToSession { .. } | TriggerAction::SetStatus { .. } | TriggerAction::ClearStatus {})
    }
}

/// Named colors a custom status may use (or `#rrggbb`).
pub const STATUS_COLORS: &[&str] = &["red", "orange", "amber", "yellow", "green", "teal", "blue", "purple", "pink", "gray"];

pub fn valid_status_color(c: &str) -> bool {
    STATUS_COLORS.contains(&c) || (c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriggerState {
    Draft,
    NeedsSecret,
    Active,
    Paused,
}

// ------------------------------------------------------------------ secrets

/// A secret the human stored (usually by pasting it into an agent terminal). Only this
/// metadata ever leaves midnad: the value lives in the Keychain. Agents see `[secret:NAME]`
/// in place of the value and use it with `midna secret exec NAME -- <command>`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Secret {
    pub id: Id,
    /// Also the environment variable `midna secret exec` sets: `[A-Z_][A-Z0-9_]*`.
    pub name: String,
    /// The project it belongs to; None = every project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    /// What it looked like when stored ("GitHub token").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Timestamp>,
    /// Times a command was run with it (`secret exec`) or it was written to a file.
    #[serde(default)]
    pub used: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<Timestamp>,
    /// Files `secret write` put it in (absolute paths).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub written_to: Vec<String>,
    /// Who stored the current value. None = the human (secrets stored before agents could).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<Actor>,
    /// An agent sent the value itself (not piped into `midna secret save`), so it passed
    /// through the agent's context and reached its model. Rotate it if that matters.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exposed: bool,
}

impl Secret {
    /// Stored by the human (agents may not silently replace these).
    pub fn humans(&self) -> bool {
        self.added_by.as_ref().is_none_or(|a| a.kind == ActorKind::Human)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Trigger {
    pub id: Id,
    pub name: String,
    pub source: TriggerSource,
    pub event: String,
    #[serde(default)]
    pub filter: TriggerFilter,
    pub action: TriggerAction,
    pub enabled: bool,
    pub state: TriggerState,
    pub secret_set: bool,
    pub created_by: Actor,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_at: Option<Timestamp>,
    #[serde(default)]
    pub fired: u64,
    /// What the last firing was about, e.g. `PR #231`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fired_summary: Option<String>,
    /// When a human last enabled it (missed-delivery recovery never goes further back).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_set_at: Option<Timestamp>,
    /// Where the secret lives: `keychain` or `file`. The secret itself is never returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_store: Option<String>,
    /// GitHub repo webhook id (Settings → Webhooks → the hook's URL). Enables missed-delivery
    /// recovery through `gh api`. Agents may set it; a verified `ping` fills it in automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_hook_id: Option<u64>,
    /// Name for terminals this trigger opens (same `{{…}}` placeholders). Defaults to the trigger name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name_template: Option<String>,
    /// Local: seconds before the same trigger may fire again for the same terminal (default 60).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_secs: Option<u64>,
    /// Set on triggers midna ships (e.g. `prompt_blocked_status`); they can be edited, paused or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub builtin: Option<String>,
}

/// A status a local trigger put on a terminal (`set_status`), shown instead of the built-in one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct CustomStatus {
    pub label: String,
    pub color: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub base: StatusState,
    pub clear_on: StatusClear,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<Id>,
    /// What caused it, e.g. the hook's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The needs-you item raised for a `needs_you` base (closed with the status).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs_you_id: Option<Id>,
    pub since: Timestamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Verified,
    BadSignature,
    Filtered,
    Replayed,
    Recovered,
    NoTrigger,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Delivery {
    pub id: Id,
    pub source: TriggerSource,
    pub event: String,
    pub delivery_guid: String,
    pub received_at: Timestamp,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_started: Option<Id>,
    pub summary: String,
    /// GitHub `action` (or the part after `:` of a Bitbucket event key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Repository full name from the payload, e.g. `mrgnhnt96/midna`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// What the payload is about, e.g. `#231 Funnel health check` or `main · 3 commits`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// HTTP status midnad answered with (202 accepted, 401 bad signature, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// One line per trigger considered: how its event/filters evaluated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eval: Vec<String>,
    /// Every trigger that fired (trigger_id is the first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers_fired: Vec<Id>,
    /// Every terminal started (session_started is the first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions_started: Vec<Id>,
    /// Recovered from GitHub's delivery log after it was missed (e.g. while asleep).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recovered: bool,
    /// For replays: the delivery that was replayed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<Id>,
    /// sha256 of a verified live body. Delivery GUID headers aren't signed, so a captured
    /// body + signature could be resent under a fresh GUID; the digest catches that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_sha256: Option<String>,
}

// ------------------------------------------------------------------ events

/// One entry in the append-only event log (`events.jsonl`). `seq` is strictly increasing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Event {
    pub seq: u64,
    pub at: Timestamp,
    pub kind: String,
    pub actor: Actor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Id>,
    #[serde(default)]
    pub data: Value,
}

/// Event kinds (dotted). Kept as constants so the daemon and clients agree on spelling.
pub mod kinds {
    pub const PROJECT_ADDED: &str = "project.added";
    pub const PROJECT_UPDATED: &str = "project.updated";
    pub const PROJECT_REMOVED: &str = "project.removed";
    pub const SESSION_OPENED: &str = "session.opened";
    pub const SESSION_RENAMED: &str = "session.renamed";
    pub const SESSION_BACKGROUND: &str = "session.background";
    pub const SESSION_CLOSED: &str = "session.closed";
    pub const SESSION_STATUS: &str = "session.status";
    pub const SESSION_TITLE: &str = "session.title";
    pub const SESSION_EXITED: &str = "session.exited";
    pub const SESSION_INPUT_BY_AGENT: &str = "session.input_by_agent";
    /// A terminal's message queue changed: `{action: added|sent|failed|removed|updated|moved|
    /// paused|resumed|cleared, id?, text?, left, paused}`; read it with `queue.list`.
    pub const SESSION_QUEUE: &str = "session.queue";
    pub const SESSION_GIT: &str = "session.git";
    /// An agent terminal's `AgentInfo` changed in a way worth showing (background work
    /// started/ended, update available, restart queued/blocked). data = the new `AgentInfo`.
    pub const SESSION_AGENT: &str = "session.agent";
    /// A terminal's process was replaced in place (`{resume, conversation_id, reason, from_version}`).
    pub const SESSION_RESTARTED: &str = "session.restarted";
    pub const NEEDS_YOU_RAISED: &str = "needs_you.raised";
    pub const NEEDS_YOU_RESOLVED: &str = "needs_you.resolved";
    pub const RULE_ADDED: &str = "rule.added";
    pub const RULE_FIRED: &str = "rule.fired";
    pub const RULE_REMOVAL_REQUESTED: &str = "rule.removal_requested";
    pub const RULE_REMOVED: &str = "rule.removed";
    pub const RULE_EXPIRED: &str = "rule.expired";
    pub const TRIGGER_ADDED: &str = "trigger.added";
    pub const TRIGGER_UPDATED: &str = "trigger.updated";
    pub const TRIGGER_DELIVERY: &str = "trigger.delivery";
    pub const TRIGGER_FIRED: &str = "trigger.fired";
    pub const TRIGGER_REMOVED: &str = "trigger.removed";
    pub const SECRET_SET: &str = "secret.set";
    pub const SECRET_REMOVED: &str = "secret.removed";
    pub const SECRET_USED: &str = "secret.used";
    pub const SETTINGS_CHANGED: &str = "settings.changed";
    /// midna's hooks in the agents' global config changed (data = `HooksStatus`).
    pub const HOOKS_CHANGED: &str = "hooks.changed";
    pub const WINDOW_COMMAND: &str = "window.command";
    pub const AGENT_TURN_STARTED: &str = "agent.turn_started";
    pub const AGENT_TURN_ENDED: &str = "agent.turn_ended";
    pub const AGENT_PROMPT_SUBMITTED: &str = "agent.prompt_submitted";
    pub const AGENT_COST: &str = "agent.cost";
    /// A UserPromptSubmit hook refused the prompt (`{hook, message, prompt}`); the agent never
    /// started a turn. Seen in Claude's transcript.
    pub const AGENT_PROMPT_BLOCKED: &str = "agent.prompt_blocked";
    /// A terminal's custom status was set or cleared (`{custom_status}`, null when cleared).
    pub const SESSION_CUSTOM_STATUS: &str = "session.custom_status";
    pub const DAEMON_STARTED: &str = "daemon.started";
    pub const DAEMON_UPGRADED: &str = "daemon.upgraded";
    /// An upgrade/restart was attempted but the old image kept running (`{to, error}`).
    pub const DAEMON_UPGRADE_FAILED: &str = "daemon.upgrade_failed";
    /// An explicit stop (`daemon.stop`, SIGINT) is hanging up every terminal.
    pub const DAEMON_STOPPING: &str = "daemon.stopping";
    pub const AUDIT: &str = "audit";
    /// A policy decision made by the defaults table (no rule matched): `{decision, action,
    /// default, passthrough}`. `passthrough` = an unmatched tool call midna has no opinion on.
    pub const POLICY_DECIDED: &str = "policy.decided";
    /// A removed rule came back with its original id (`rule.restore`); data = the rule.
    pub const RULE_RESTORED: &str = "rule.restored";
    /// The palette's user commands changed (`ui.commands.*` or an edit of commands.json).
    pub const UI_COMMANDS_CHANGED: &str = "ui.commands_changed";
    /// The GUI reported a new updater state (data = `UpdatesStatus`).
    pub const UPDATES_STATUS: &str = "updates.status";
    /// `updates.check` / `updates.install` was forwarded to the GUI (`{action, delivered}`).
    pub const UPDATES_REQUESTED: &str = "updates.requested";
    /// `daemon.reset` cleared state (data = counts, `keep_rules`).
    pub const DAEMON_RESET: &str = "daemon.reset";
    /// A terminal's session links changed (`{count, added, pinned}`); read them with `links.list`.
    pub const LINKS_CHANGED: &str = "links.changed";
    /// midnad decided to notify the human (data: `notify::Posted`); the app shows it.
    pub const NOTIFY_POSTED: &str = "notify.posted";
    /// A terminal's notification overrides changed (`{notify}`, the whole map).
    pub const SESSION_NOTIFY: &str = "session.notify";
    /// A notification sound or image was imported or removed (`{action, kind, name}`).
    pub const NOTIFY_MEDIA: &str = "notify.media";
}

// ------------------------------------------------------------------ session links

/// What a session link points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// A GitHub or Bitbucket pull request.
    Pr,
    /// A claude.ai artifact.
    Artifact,
    /// Any other http(s) URL.
    Web,
    /// A file the agent created or edited (absolute path).
    File,
}

/// Where a link came up first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LinkSource {
    /// The human's prompt.
    User,
    /// The agent's reply text.
    Agent,
    /// The agent fetched it (WebFetch).
    Fetched,
    /// A tool's output (e.g. `gh pr create`, an artifact publish).
    Tool,
    /// The agent created the file.
    Created,
    /// The agent edited the file.
    Edited,
    /// Added on purpose with `links.add`.
    Added,
}

/// A link, PR, artifact or file that came up in an agent terminal's conversation. The daemon
/// collects them from the agent's transcript; anyone can pin one or add one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Link {
    /// `l_` + 8 hex, stable for the same target in the same terminal.
    pub id: Id,
    pub kind: LinkKind,
    /// The URL, or the absolute path for a file.
    pub target: String,
    /// Short display name (`PR #12 · owner/repo`, an artifact's title, a host + path, a relative path).
    pub title: String,
    pub source: LinkSource,
    /// The tool or command it came from, when it came from one (`gh pr create`, `WebFetch`, `Edit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// How many transcript entries mentioned it (edits, for a file).
    pub mentions: u32,
    pub first_at: Timestamp,
    pub last_at: Timestamp,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_by: Option<Actor>,
    /// Why it matters, from `links.add`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

// ------------------------------------------------------------------ scripts

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Dim,
    Ok,
    Err,
    Need,
    Work,
    Accent,
}

/// One piece of a header/row script line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub struct Segment {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<Tone>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

impl Segment {
    pub fn new(text: impl Into<String>, tone: Option<Tone>) -> Segment {
        Segment { text: text.into(), tone, link: None }
    }
}
