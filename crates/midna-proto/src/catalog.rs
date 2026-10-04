//! The method catalog: the single list of every RPC method. `rpc.discover`, `midna schema`
//! and `midna mcp` are all generated from it. Add a feature = add a row here.
use crate::methods::*;
use crate::types::*;
use schemars::JsonSchema;
use serde_json::Value;
use std::sync::LazyLock;

pub struct MethodSpec {
    pub name: &'static str,
    /// Written for an agent reader.
    pub description: &'static str,
    pub params: fn() -> Value,
    pub result: fn() -> Value,
    /// Changes state (and is audited).
    pub mutating: bool,
    /// Only a human (the GUI) may call it; agents get error 2.
    pub human_only: bool,
    /// Not implemented yet (later phase); calls return an error.
    pub stub: bool,
}

impl MethodSpec {
    pub fn group(&self) -> &'static str {
        self.name.split('.').next().unwrap_or(self.name)
    }
}

/// JSON Schema for `T` (self-contained, with `$defs`).
pub fn schema_of<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap_or(Value::Bool(true))
}

fn any() -> Value {
    serde_json::json!({ "title": "Any", "description": "Any JSON value." })
}

struct M {
    name: &'static str,
    params: fn() -> Value,
    result: fn() -> Value,
    mutating: bool,
    human_only: bool,
    stub: bool,
}

const fn m<P: JsonSchema, R: JsonSchema>(name: &'static str) -> M {
    M { name, params: schema_of::<P>, result: schema_of::<R>, mutating: false, human_only: false, stub: false }
}

impl M {
    const fn mutating(mut self) -> M {
        self.mutating = true;
        self
    }
    const fn human(mut self) -> M {
        self.human_only = true;
        self
    }
    #[allow(dead_code)] // no stubs left right now; kept for later phases
    const fn stub(mut self) -> M {
        self.stub = true;
        self
    }
    const fn result_any(mut self) -> M {
        self.result = any;
        self
    }
    fn d(self, description: &'static str) -> MethodSpec {
        MethodSpec {
            name: self.name,
            description,
            params: self.params,
            result: self.result,
            mutating: self.mutating,
            human_only: self.human_only,
            stub: self.stub,
        }
    }
}

static CATALOG: LazyLock<Vec<MethodSpec>> = LazyLock::new(build);

pub fn catalog() -> &'static [MethodSpec] {
    &CATALOG
}

pub fn method(name: &str) -> Option<&'static MethodSpec> {
    catalog().iter().find(|m| m.name == name)
}

fn build() -> Vec<MethodSpec> {
    vec![
        // rpc / daemon
        m::<NoParams, Value>("rpc.discover").result_any().d(
            "Return the OpenRPC document describing every midna method. Start here to learn what you can do."),
        m::<NoParams, DaemonInfo>("daemon.info").d(
            "Daemon version, pid, uptime, MIDNA_HOME, socket path, and the role (human/agent) the daemon assigned to your connection."),
        m::<UpgradeParams, UpgradeResult>("daemon.upgrade").mutating().human().d(
            "Replace the running daemon with another midnad binary without killing terminals: the new binary must pass \
             `--selftest`, then every terminal's screen is saved and the daemon re-execs in place (same pid), keeping \
             PTYs, ids and child processes. Connections drop and must reconnect; `daemon.upgraded` reports the result. \
             Human only: agents (and `midna daemon upgrade`) get a needs-you approval instead."),
        m::<NoParams, UpgradeResult>("daemon.restart").mutating().d(
            "Graceful restart: the same handoff as daemon.upgrade, re-exec'ing the daemon's own binary (or \
             MIDNA_HOME/bin/current). Terminals keep running. Use it to pick up a rebuilt or reinstalled midnad."),
        m::<NoParams, OkResult>("daemon.stop").mutating().human().d(
            "Stop the daemon for good: every terminal's process groups get SIGHUP (SIGKILL after a grace period), \
             then midnad exits. Human only; agents get a needs-you approval. Prefer daemon.restart to keep terminals."),
        m::<DaemonResetParams, DaemonResetResult>("daemon.reset").mutating().human().d(
            "Start over: close every terminal (their processes get SIGHUP), remove every project, trigger (and its \
             secret), webhook delivery and needs-you item, and reset every setting to its default. The event log is kept, \
             and rules are kept unless keep_rules=false. Human only (the Settings window's Danger zone); an agent's call \
             becomes a needs-you approval."),
        // projects
        m::<NoParams, Vec<Project>>("project.list").d("List projects (directories that group terminals), in sidebar order, with their saved commands."),
        m::<NoParams, Vec<ProjectCandidate>>("project.discover").d(
            "List folders under the `projects.roots` setting that can be opened as projects (with project.add), most recently modified first. \
             Direct subfolders count; one that isn't a git repo but contains git repos is replaced by those. Hidden folders are skipped. \
             `project_id` is set when the folder already is a project."),
        m::<ProjectAddParams, Project>("project.add").mutating().d(
            "Add a project for a directory. Returns the existing project if the path is already registered."),
        m::<ProjectUpdateParams, Project>("project.update").mutating().d(
            "Change a project's name, icon, or saved commands. Saved commands ({name, run, pinned}) show up in the human's command bar as \"Run <name>\"; pinned ones are suggested. `commands` replaces the whole list."),
        m::<IdParams, OkResult>("project.remove").mutating().human().d(
            "Remove a project and close its terminals. Human only: an agent's call becomes a needs-you approval the human answers."),
        // sessions
        m::<SessionListParams, Vec<Session>>("session.list").d(
            "List terminals (shells, monitors, agents) with status, title and git info. Filter by project_id."),
        m::<IdParams, Session>("session.get").d("Get one terminal by id: kind, agent, cwd, command, title, status {state, reason, since, exit_code}, git info and, for agents, agent_info (conversation id, running version, update available, background work, subagents, scheduled wakeups, queued restart). `midna explain <id>` says why it has its status."),
        m::<SessionOpenParams, Session>("session.open").mutating().d(
            "Open a new terminal. kind=shell runs the login shell (or `command`), kind=monitor runs `command` for the human to watch, \
             kind=agent launches Claude or Codex (agent=claude|codex) with midna's hooks and an optional initial `prompt`. \
             Use this to delegate durable work to a new agent the human can see."),
        m::<SessionCloseParams, OkResult>("session.close").mutating().d(
            "Close a terminal and kill its process. Closing a working terminal requires force=true, which is policy-checked (`close --force`)."),
        m::<SessionRenameParams, Session>("session.rename").mutating().d("Rename a terminal (the sidebar label the human sees). Give terminals you open a short, task-describing name."),
        m::<SessionInputParams, OkResult>("session.input").mutating().d(
            "Type text into a terminal, optionally followed by Enter; to an agent (Claude Code, Codex) this is a chat message. \
             `images` (absolute paths) are attached first, each pasted as a path the agent turns into [Image #N]. \
             Input from agents is logged as session.input_by_agent. \
             Never answer another agent's permission prompt unless session.read shows the prompt."),
        m::<SessionReadParams, SessionReadResult>("session.read").d(
            "Read a terminal's text as plain text: the last `lines` lines (default 50) or the visible `screen`."),
        m::<SessionResizeParams, OkResult>("session.resize").mutating().d("Resize a terminal's PTY and engine grid (cols x rows). The GUI does this for terminals it shows; agents rarely need it."),
        m::<SessionKeyParams, OkResult>("session.key").mutating().d(
            "Press a key in a terminal (`ctrl-c`, `escape`, `shift-tab`, `up`, `enter`, ...), encoded for the app's keyboard \
             mode (legacy, modifyOtherKeys or the kitty protocol) like a real keypress. Use session.input for text."),
        m::<SessionScrollParams, OkResult>("session.scroll").mutating().d(
            "Scroll a terminal's viewport through its scrollback: `to: top|bottom`, or by `lines` / `pages` (negative = up). \
             All attached windows follow; typing snaps back to the bottom."),
        m::<IdParams, SessionSelectionResult>("session.selection").d(
            "The text currently selected in a terminal (by the human dragging or double/triple-clicking, or by session.find), \
             with soft-wrapped lines joined and wide characters intact. Null when nothing is selected."),
        m::<IdParams, OkResult>("session.select_all").mutating().d(
            "Select everything in a terminal (scrollback and screen), as ⌘A does; read it with session.selection."),
        m::<IdParams, Value>("session.clear").mutating().result_any().d(
            "Clear a terminal like its right-click \"Clear\": drops the scrollback and presses ctrl-l so the shell (or app) \
             redraws. On the alternate screen (vim, less, an agent's TUI) only ctrl-l is sent. Returns {ok, scrollback_cleared}."),
        m::<SessionLinkAtParams, LinkAtResult>("session.link_at").d(
            "What link is at a viewport cell: an OSC 8 hyperlink, a detected http(s) URL, or an existing file path with \
             optional :line[:column] (relative paths resolve against the shell's directory)."),
        m::<SessionFindParams, FindResult>("session.find").mutating().d(
            "Find text in a terminal's scrollback and screen: scrolls to the next (or previous, `backwards`) match and \
             selects it. Returns the match count and the current match's index."),
        m::<SessionRestartParams, Session>("session.restart").mutating().d(
            "Restart a terminal's command in place (same id, same tab, fresh process). Agent terminals resume the same \
             conversation by default (`resume`, with the model and permission mode they had). `when: idle` queues it until the \
             agent is idle with no background shells, subagents or scheduled wakeups in flight and an empty input box; \
             `when: now` refuses while background work is in flight unless `force`. Policy-checked as `restart <id>`; the \
             default policy asks the human."),
        m::<IdParams, Session>("session.restart_cancel").mutating().d("Cancel a terminal's queued restart (`session.restart` with `when: idle`)."),
        m::<IdParams, Vec<ProcessInfo>>("session.processes").d(
            "Every OS process under a terminal (its process tree: pid, ppid, pgid, depth, command), with the agent's \
             background task id when a process belongs to one. Subagents run inside the agent process and appear in \
             `session.get` -> agent_info.subagents / background instead."),
        m::<IdParams, WindowCommandResult>("session.focus").mutating().d(
            "Bring a terminal to the front in the midna GUI (emits window.command front). Always allowed; use it to show the human something you want them to see."),
        // stream
        m::<StreamAttachParams, OkResult>("stream.attach").d(
            "Turn this connection into a binary frame stream for a terminal (GUI renderers). After the {ok:true} line the \
             connection carries u32-LE length-prefixed frames; send 0x01 for credit, 0x02 to resize, 0x03 to type, \
             0x04 key, 0x05 scroll, 0x06 mouse, 0x07 focus, 0x08 paste (see midna_proto::frame::ClientMsg)."),
        // events
        m::<EventsListParams, Vec<Event>>("events.list").d(
            "List events from the append-only log (seq > since_seq), oldest first. Every state change and every acting call (audit) is an event."),
        m::<EventsSubscribeParams, SubscribeResult>("events.subscribe").d(
            "Subscribe this connection to events. Replays events after since_seq, then streams live ones as `event` notifications."),
        // needs-you
        m::<NeedsYouListParams, Vec<NeedsYou>>("needs_you.list").d(
            "List open needs-you items: approvals, permission prompts, blocked agents, notes, failures, rule-removal requests."),
        m::<NeedsYouRaiseParams, NeedsYou>("needs_you.raise").mutating().d(
            "Get the human's attention. kind=blocked when you cannot continue without them, kind=note for an FYI. Keep message to one short line (details in `detail`); don't raise one per step."),
        m::<NeedsYouResolveParams, ResolveResult>("needs_you.resolve").mutating().d(
            "Resolve a needs-you item: approve{scope: once|minutes|session|always}, deny, dismiss (human), done, restart. \
             Agents may only approve their own session's requests and only if setting approve.from_cli is on."),
        // policy
        m::<PolicyCheckParams, CheckResult>("policy.check").d(
            "Evaluate an action against rules without side effects. Returns the decision, the winning rule and a full trace. \
             Most specific scope wins (session > project > global); within a scope deny > ask > allow."),
        m::<PolicyRequestParams, PolicyRequestResult>("policy.request").mutating().d(
            "Check an action and, if a rule says ask, raise an approval for the human and block until they answer or timeout_secs passes."),
        // rules
        m::<NoParams, Vec<Rule>>("rule.list").d(
            "List rules (allow/ask/deny matchers) with fire counts and pending removal requests. Expired rules are removed (rule.expired event)."),
        m::<RuleAddParams, Rule>("rule.add").mutating().d(
            "Add a rule. matcher.kind is command|tool|path|cli|window; pattern is a glob like `git push --force*` or `Bash(rm -rf*)`. \
             Agents may add rules but cannot remove them."),
        m::<RuleRequestRemovalParams, NeedsYou>("rule.request_removal").mutating().d(
            "Ask the human to remove a rule (creates a rule_removal needs-you item with your reason). This is how agents get a rule removed; the rule stays in force until the human removes it."),
        m::<IdParams, OkResult>("rule.remove").mutating().human().d("Remove a rule. Human only; agents use rule.request_removal {id, reason}, which the human sees and decides."),
        m::<RuleRestoreParams, Rule>("rule.restore").mutating().human().d(
            "Undo a rule removal: put a removed rule back with its original id, author, origin and fired count (pass the \
             rule object from the rule.removed event). Human only (the Rules screen's Undo); agents add rules with rule.add."),
        // triggers / webhooks
        m::<NoParams, Vec<Trigger>>("trigger.list").d(
            "List webhook triggers (GitHub/Bitbucket event -> start an agent, run a command, or raise attention). \
             `state`: needs_secret (a human must paste the secret), draft (ready for a human to enable), active, paused."),
        m::<TriggerAddParams, Trigger>("trigger.add").mutating().d(
            "Create a webhook trigger. It starts as needs_secret and never fires until a human sets its secret and enables it; \
             agents can draft freely. event: GitHub `pull_request.opened`/`push`/`check_run.completed` (event[.action]), \
             Bitbucket `pullrequest:created`. filter: repo (owner/name), branch, action, label (globs). action: \
             {kind:start_agent, project_id, agent, prompt_template} | {kind:run_command, project_id, command} | {kind:attention, message}. \
             Templates: {{pr.number}} {{pr.title}} {{repo}} {{branch}} {{sender}} {{url}} {{action}} or any payload path like {{pull_request.head.ref}}. \
             In run_command each value is shell-quoted. Set github_hook_id to enable missed-delivery recovery."),
        m::<TriggerUpdateParams, Trigger>("trigger.update").mutating().d(
            "Edit a trigger (name, event, filter, action, github_hook_id, session_name_template). When an agent changes the \
             action or source of an enabled trigger, it goes back to draft and a human must re-enable it."),
        m::<TriggerSetEnabledParams, Trigger>("trigger.set_enabled").mutating().d(
            "Enable or pause a trigger. Anyone may pause. Enabling is human only and needs the secret set: an agent asking to \
             enable raises a needs-you confirmation (or a secret_needed item if the secret is missing)."),
        m::<TriggerSetSecretParams, OkResult>("trigger.set_secret").mutating().human().d(
            "Set a trigger's webhook signing secret (stored in the macOS Keychain; never returned or logged). Human only: \
             an agent calling this raises a secret_needed needs-you item and the secret it sent is discarded."),
        m::<IdParams, OkResult>("trigger.remove").mutating().d(
            "Remove a trigger and its secret. Agents may remove triggers that are not enabled; removing an enabled trigger \
             asks the human."),
        m::<TriggerDeliveriesParams, Vec<Delivery>>("trigger.deliveries").d(
            "List received webhook deliveries, oldest first (default last 50). verdict: verified (fired), bad_signature, \
             filtered (a trigger listens but filters/state said no), no_trigger, replayed, recovered (missed, then fetched from GitHub)."),
        m::<TriggerReplayParams, Delivery>("trigger.replay").mutating().d(
            "Run a past delivery through the triggers again (same filters, no signature check since it was verified when it \
             arrived). Deliveries that failed signature checks can't be replayed."),
        m::<TriggerTestParams, Delivery>("trigger.test").d(
            "Dry-run one trigger against a sample payload: shows how the event and filters evaluate and the rendered \
             prompt/command. Nothing runs and nothing is recorded."),
        m::<NoParams, WebhooksStatus>("webhooks.status").d(
            "How webhooks reach this Mac: path (tailscale_funnel|self_relay|midna_relay|off), health, the public URL to paste \
             into GitHub/Bitbucket, the local receiver, Tailscale details, the last delivery and missed-delivery recovery."),
        m::<WebhooksConfigureParams, WebhooksConfigureResult>("webhooks.configure").mutating().human().d(
            "Choose how webhooks reach this Mac. Human only. tailscale_funnel runs `tailscale funnel --bg --https=8443 \
             http://127.0.0.1:<port>`; if Funnel isn't enabled for the tailnet the result carries the enable_url."),
        m::<NoParams, ReconcileStatus>("webhooks.reconcile").mutating().d(
            "Check GitHub's delivery log (via an authenticated `gh`) for triggers with github_hook_id and redeliver any \
             delivery from the last 3 days that midnad never received, as `recovered`. Runs on startup and on wake too."),
        // settings
        m::<NoParams, Vec<SettingEntry>>("settings.list").d(
            "List every setting with its value, default, description, whether it is human only, and the CLI command to change it."),
        m::<SettingKeyParams, SettingEntry>("settings.get").d("Get one setting: value, default, type, description, whether it is human_only, and the CLI command to change it."),
        m::<SettingSetParams, SettingEntry>("settings.set").mutating().d(
            "Change a setting. Agents may not change human_only settings (the request becomes a needs-you confirmation)."),
        m::<SettingKeyParams, SettingEntry>("settings.reset").mutating().d("Reset a setting to its default. Same human_only rule as settings.set: for a human-only key an agent's call asks the human instead."),
        // insights
        m::<InsightsSummaryParams, InsightsSummary>("insights.summary").d(
            "Totals computed from the event log for today/yesterday/week: turns, human messages, spend, working and waiting time, \
             approvals, triggers fired, with deltas vs the previous period. Optionally grouped by project/agent/terminal/day."),
        m::<InsightsSeriesParams, InsightsSeries>("insights.series").d(
            "Time-bucketed values of one metric, computed from the event log, for charts. range: today|yesterday|week|month; \
             metric: turns|messages|spend|working|waiting|approvals|triggers; bucket: hour|day (default hour for today, day \
             otherwise); by: project|agent|terminal splits each bucket into per-group values. Returns every bucket in the range \
             (oldest first, empty ones included), the groups largest first with labels, the total and the previous period's total."),
        m::<InsightsActivityParams, Vec<Event>>("insights.activity").d(
            "Recent meaningful activity (status changes, turns, approvals, rules, needs-you), newest first. Use events.list for the raw log."),
        // window
        m::<NoParams, WindowList>("window.list").d("Whether the midna GUI is connected (window commands only reach a running GUI) and which terminals are kept on top."),
        m::<WindowCommandParams, WindowCommandResult>("window.command").mutating().d(
            "Ask the GUI to act on a window: front, keep_on_top, pop_out, snap, close, open_screen, split. split shows \
             terminal `target` beside the selected one in the main window (value: side|stacked; close folds the split, the \
             terminal keeps running). Agents need setting agents.may_move_windows for keep_on_top, pop_out, snap and close; \
             front, open_screen and split are always allowed (still policy-checked as `window` actions, e.g. `split <id>`)."),
        // ui
        m::<NoParams, UiCommandsList>("ui.commands.list").d(
            "List the user commands in the human's ⌘K palette ($MIDNA_HOME/commands.json): each has a title, keywords and \
             a `run` (rpc{method, params} | focus{session} | project{index} | screen{screen} | pop_out{session} | \
             prefill{text}). Entries in the file that fail validation are listed under `invalid`."),
        m::<UiCommandsAddParams, UiCommand>("ui.commands.add").mutating().d(
            "Add a command to the human's ⌘K palette (it appears immediately). Use it for workflows the human repeats, e.g. \
             {title: \"Deploy staging\", keywords: \"ship\", run: {kind: rpc, method: session.open, params: {kind: monitor, \
             command: [\"./deploy.sh\"]}}}. The method must exist; a human-only method gets the two-step destructive \
             confirm. Same id = error unless replace=true. Prefer project.update commands for per-project scripts."),
        m::<IdParams, OkResult>("ui.commands.remove").mutating().d(
            "Remove a user command from the ⌘K palette by id (see ui.commands.list)."),
        // updates
        m::<NoParams, UpdatesStatus>("updates.status").d(
            "The midna app's auto-updater state as the GUI last reported it: current and available version, release \
             notes, channel (setting updates.channel), last check, error. state unknown = no GUI has reported yet."),
        m::<NoParams, UpdatesCommandResult>("updates.check").mutating().d(
            "Ask the running midna app to check its update feed now (harmless; it also checks every 6 hours). The result \
             arrives as an updates.status event and in updates.status. delivered=0 means the GUI isn't running."),
        m::<NoParams, UpdatesCommandResult>("updates.install").mutating().human().d(
            "Install a downloaded update: the app swaps its bundle and relaunches (terminals keep running in midnad). \
             Human only: an agent's call becomes a needs-you approval; tell the human why the update matters."),
        m::<UpdatesReportParams, OkResult>("updates.report").mutating().human().d(
            "Internal: the GUI reports its updater state here (agents call updates.status instead)."),
        // permissions
        m::<NoParams, PermissionsStatus>("permissions.status").d(
            "macOS permissions midna uses (accessibility, notifications, login-items) as far as the daemon can see them, \
             each with the `midna permissions open <name>` command that opens the right System Settings pane. Only the \
             human can grant them; use needs_you.raise to ask."),
        // agent hooks
        m::<AgentHookParams, AgentHookResult>("agent.hook").mutating().d(
            "Report an agent hook event (called by `midna hook claude|codex`). Drives terminal status, turns, prompt counts and cost."),
        // scripts
        m::<ScriptRunParams, ScriptRunResult>("script.run").d(
            "Run the configured header or row script for a terminal and return its segments (built-ins: github, git-diff-stats, \
             or a custom executable printing JSON segments)."),
    ]
}
