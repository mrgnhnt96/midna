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
            "Remove a project and close its terminals. Human only: an agent's call becomes a needs-you approval the human answers. \
             Exception: agents may remove a project midna created for one of their session.open calls (`auto_created`) once \
             none of its terminals is running."),
        // sessions
        m::<SessionListParams, Vec<Session>>("session.list").d(
            "List terminals (shells, monitors, agents) with status, title and git info. Filter by project_id."),
        m::<IdParams, Session>("session.get").d("Get one terminal by id: kind, agent, cwd, command, title, status {state, reason, since, exit_code}, git info and, for agents, agent_info (conversation id, running version, update available, background work, subagents, scheduled wakeups, queued restart, and Claude's plan usage `rate_limits` {five_hour, seven_day: {used_percentage, resets_at}, observed_at}). `midna explain <id>` says why it has its status."),
        m::<SessionOpenParams, Session>("session.open").mutating().d(
            "Open a new terminal. kind=shell runs the login shell (or `command`), kind=monitor runs `command` for the human to watch, \
             kind=agent launches Claude or Codex (agent=claude|codex) with midna's hooks and an optional initial `prompt`; \
             `resume` reopens a conversation by id and `agent_args` are the agent's own arguments (Claude `--settings` / \
             `--append-system-prompt`, Codex `-c developer_instructions` / `-c notify` are merged with midna's own, so its hooks still run). \
             Use this to delegate durable work to a new agent the human can see. background=true opens it in the sidebar's \
             folded Background group instead (a dev server, a watcher): still listed, readable and able to raise needs-you."),
        m::<SessionSetBackgroundParams, Session>("session.set_background").mutating().d(
            "Move a terminal to the sidebar's folded Background group (background=true) or back to its project (false). \
             Its process, status and needs-you items are unchanged."),
        m::<SessionCloseParams, OkResult>("session.close").mutating().d(
            "Close a terminal and kill its process. Closing a working terminal requires force=true, which is policy-checked (`close --force`): \
             it asks the human unless they turned on agents.may_force_close (or a rule allows it). Closing another idle terminal asks unless \
             agents.may_close_idle is on. An approval still pending when its terminal closes is withdrawn."),
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
        m::<IdParams, SessionPromptsResult>("session.prompts").d(
            "The prompts the human sent an agent terminal, oldest first and numbered from 1, with where the agent's view \
             is: `here` is the prompt the top of the screen belongs to and `scrolled` says it is scrolled back. \
             Prompts with `on_screen: false` were sent before a /clear or a fresh start."),
        m::<SessionJumpPromptParams, JumpPromptResult>("session.jump_prompt").mutating().d(
            "Scroll an agent terminal to one of the human's prompts (`n` from session.prompts) so it sits at the top of \
             the screen, or `to: prev|next|latest|live`. Claude Code and Codex scroll their own full-screen view, so \
             midnad presses page keys and reads the screen until the prompt shows: it can take a second or two. \
             Logged as session.input_by_agent when an agent calls it."),
        m::<SessionRestartParams, Session>("session.restart").mutating().d(
            "Restart a terminal's command in place (same id, same tab, fresh process). Agent terminals resume the same \
             conversation by default (`resume`, with the model and permission mode they had). `when: idle` queues it until the \
             agent is idle with no background shells, subagents or scheduled wakeups in flight and an empty input box; \
             `when: now` refuses while background work is in flight unless `force`. Policy-checked as `restart <id>`; the \
             default policy asks the human."),
        m::<IdParams, Session>("session.restart_cancel").mutating().d("Cancel a terminal's queued restart (`session.restart` with `when: idle`)."),
        m::<IdParams, Session>("session.update_decline").mutating().d(
            "Answer \"Not now\" to an agent terminal's update prompt (`agent_info.update_available`): it stays hidden for \
             that update (`agent_info.update_declined`). A newer update asks again; a restart still picks it up."),
        m::<LinksListParams, LinksListResult>("links.list").d(
            "The links, pull requests, artifacts and files that came up in an agent terminal's conversation, collected \
             from the agent's transcript as it runs: URLs the human pasted or the agent wrote or fetched, PRs and artifacts \
             that tools printed, and files the agent created or edited. Each has where it came up first (`source`, `via`), \
             how often, whether it is pinned, and the prompts it came up in (`turn` = the latest, `turns` = all, numbered \
             as in session.prompts). `turn: N | \"last\"` keeps only what came up in that turn, e.g. {kind: file, turn: \
             last} = the files edited since the human's last prompt. Pinned links come first. Defaults to your own terminal."),
        m::<LinksPinParams, Link>("links.pin").mutating().d(
            "Pin (or with `pinned: false` unpin) a session link by id or exact URL/path, so it stays at the top of the \
             terminal's links list in the header. Pin what the human will want to come back to: the PR, the design, the doc \
             you were sent. Defaults to your own terminal."),
        m::<LinksRemoveParams, Link>("links.remove").mutating().d(
            "Remove a link from a terminal's links (by id or exact URL/path) and keep it out: the transcript mentioning \
             it again doesn't bring it back, only links.add does. `restore: true` puts a removed link back as it was (undo). Returns the link. Defaults to \
             your own terminal."),
        m::<LinksAddParams, Link>("links.add").mutating().d(
            "Add a link (http(s) URL or absolute file path) to a terminal's links on purpose, pinned by default, with an \
             optional title and a note on why it matters. Use it for something important the transcript would not show as \
             a link, or to give one a better name. Adding a target that is already there updates its title/note and pins it. \
             Defaults to your own terminal."),
        m::<QueueListParams, QueueListResult>("queue.list").d(
            "A terminal's queued messages, in the order midnad will type them, and whether the queue is paused. The first \
             waiting one says what it is waiting for (`waiting_for`). Defaults to your own terminal."),
        m::<QueueAddParams, QueuedMessage>("queue.add").mutating().d(
            "Queue a message for a terminal instead of typing it now: midnad types it (and presses Enter, unless \
             `enter: false`) once it is first in line, its `when` holds and the agent is ready for input (not working \
             or waiting on the human, no dialog on screen, nothing typed in its input box), then the next one. `when`: \
             {kind: idle} (default), {kind: idle_for, minutes}, {kind: at, at: RFC 3339}, or {kind: after, session} \
             (once that terminal is idle with an empty queue). The human sees the queue on the terminal and can edit, \
             reorder or remove it. Defaults to your own terminal, which is how you leave yourself a follow-up \
             (e.g. `/compact`, then the next step)."),
        m::<QueueUpdateParams, QueuedMessage>("queue.update").mutating().d(
            "Change a queued message's text, enter or when; `retry: true` puts a failed one back to waiting."),
        m::<QueueItemParams, OkResult>("queue.remove").mutating().d("Remove a queued message without sending it."),
        m::<QueueListParams, QueueListResult>("queue.clear").mutating().d("Remove every message from a terminal's queue."),
        m::<QueueMoveParams, QueueListResult>("queue.move").mutating().d("Move a queued message to another position (0 = next)."),
        m::<QueueItemParams, OkResult>("queue.send_now").mutating().d(
            "Type a queued message right away, ignoring its `when` and whether the agent is busy (Claude Code queues \
             input it gets mid-turn itself). It leaves the queue. Refused while a permission prompt is on screen."),
        m::<QueuePauseParams, QueueListResult>("queue.pause").mutating().d(
            "Pause a terminal's queue (`paused: false` resumes): nothing is typed while it is paused."),
        m::<NotifyListParams, NotifyListResult>("notify.list").d(
            "Which macOS notifications midna posts: every category with its global setting (`notify.<key>`), the \
             terminal's override and what applies to it. Defaults to your own terminal; `global: true` for the global \
             settings only. On by default: approvals and questions, attention, failures, agent finished (long turns) and \
             notifications agents send."),
        m::<NotifySetParams, NotifyListResult>("notify.set").mutating().d(
            "Turn a notification category on or off for one terminal (default: your own), or mute it with `key: \"enabled\", \
             value: false`; `value: null` drops the override. `global: true` changes the global setting instead. Change \
             notifications when the human asks (\"tell me when every turn ends here\", \"stop pinging me about this one\"); \
             don't quietly turn off what they rely on."),
        m::<NotifySendParams, NotifySendResult>("notify.send").mutating().d(
            "Send the human a macOS notification (category `agent`): use it when they asked to be told about something \
             (\"ping me when the build is green\") or when a result needs them and they may be away. Clicking it selects \
             your terminal, or opens `open` (a URL) instead; it works from outside midna terminals too. `id` names it: \
             sending again with the same id replaces the one showing (a repeating alert), and notify.withdraw removes it. \
             `actions` (up to 4 buttons, e.g. [\"Snooze 15 min\", \"Snooze 1 hour\"]) report the human's pick as \
             `response` {kind: action|clicked|dismissed, action?}: wait for it with `wait_secs`, ask later with \
             notify.response, or react to the `notify.responded` event (a local trigger on it). Buttons and clicks need the \
             app running (`via: app`). Not for routine progress: approvals, failures and finished turns already notify. \
             Duplicates within a few seconds are dropped and an agent can send at most 6 a minute per terminal."),
        m::<NotifyWithdrawParams, NotifyWithdrawResult>("notify.withdraw").mutating().d(
            "Take a notification sent with notify.send away by its `id` (Notification Center, the in-app card and the \
             floating badge), e.g. once the alert it was about has cleared. It stays in notify.history. With no app \
             connected nothing changes (`delivered: false`)."),
        m::<NotifyResponseParams, NotifyResponseResult>("notify.response").d(
            "What the human did with a notification sent with notify.send: `response` {kind: action (a button, `action` \
             = its label), clicked or dismissed}, or none yet. `wait_secs` (max 600) waits for one. midnad remembers \
             responses until it restarts."),
        m::<NotifyRespondParams, NotifyResponseResult>("notify.respond").mutating().human().d(
            "Internal: the GUI reports the human's click, button or dismissal on a notify.send notification (emits \
             `notify.responded`)."),
        m::<NoParams, NotifyKindsList>("notify.kinds.list").d(
            "The notification kinds the human added, beside the built-in ones (notify.list): each with its label, \
             whether it's on, how long it stays on screen (`stay`, 0 = until handled), its color and sound. Send to one \
             with notify.send `category`, or from a trigger's notify action `category`. Their settings are `notify.<field>.<key>` \
             like a built-in kind's (settings.set)."),
        m::<NotifyKindsAddParams, NotifyKindInfo>("notify.kinds.add").mutating().d(
            "Add a notification kind (\"deploys\", \"ci\"), so notifications about it get their own sound, color, \
             duration and switch: {key, label, description?, settings?: {stay, color, sound, push, …}}. Add one when \
             the human asks for it; same key = error unless replace=true."),
        m::<NotifyKindsRemoveParams, OkResult>("notify.kinds.remove").mutating().d(
            "Remove a notification kind the human added, and its settings. Built-in kinds can only be turned off \
             (`notify.<key>`)."),
        m::<NotifyMediaParams, NotifyMediaResult>("notify.media").d(
            "The sounds and images notifications can use: the macOS sounds (Glass, Ping, …) and what the human imported, \
             each with the `name` a setting takes and which settings use it. A category's sound, volume and image are the \
             settings `notify.sound.<category>`, `notify.volume.<category>` (0-100, scaled by `notify.volume`) and \
             `notify.image.<category>` (empty = `notify.image`, the image every notification uses)."),
        m::<NotifyImportParams, NotifyMedia>("notify.import").mutating().d(
            "Copy a sound (aiff, wav, mp3, m4a, caf) or image (png, jpg, gif) into midna so notifications can use it, and \
             optionally set it right away (`use_for: [\"notify.sound.approval\"]`). Its `name` is what the settings take. \
             Import what the human points you at (\"use ~/Downloads/ding.mp3 for approvals\"); don't pick sounds for them."),
        m::<NotifyRemoveParams, NotifyRemoveResult>("notify.remove").mutating().d(
            "Delete an imported sound or image. Settings that used it go back to their defaults."),
        m::<NotifyClearParams, NotifyClearResult>("notify.clear").mutating().d(
            "Remove midna's notifications from Notification Center: every one, or only one terminal's with `session`. \
             The app removes them; with no app connected nothing changes (`delivered: false`). midna already removes a \
             terminal's notifications when the human opens it. Use it when the human asks."),
        m::<NotifyHistoryParams, NotifyHistoryResult>("notify.history").d(
            "What midna recorded as notifications, newest first: each one's category, title, text, terminal, whether it \
             was a banner (`push`) and whether the human has seen it (`unread`), plus how many are unread in all. \
             Recorded kinds that aren't pushed (failures, finished turns, …) only show up here and in the app's \
             Notifications screen."),
        m::<NotifyReadParams, NotifyReadResult>("notify.read").mutating().d(
            "Mark notifications read (all so far, or up to `seq`). The app does this when the human opens its \
             Notifications screen; only do it when the human asks."),
        m::<NotifyTestParams, NotifySendResult>("notify.test").mutating().d(
            "Show a test notification with a category's sound, volume and image (default `approval`), even when that \
             category is off, so the human can hear and see their choice."),
        m::<NotifyPlayParams, NotifyPlayResult>("notify.play").mutating().d(
            "Play a sound now, with no notification: a kind's sound at its volume (a notification category, or a \
             sound effect: approved, denied, queue_sent, image_added, closed, switched, command_bar, copied) or a \
             sound by name (Glass, Pop, an imported file). Use it when the human asks for one (\"play a sound when \
             the deploy is done\"), never as decoration. `notify.sounds` off silences it; an agent can play at most \
             6 a minute."),
        m::<IdParams, Vec<ProcessInfo>>("session.processes").d(
            "Every OS process under a terminal (its process tree: pid, ppid, pgid, depth, command), with the agent's \
             background task id when a process belongs to one. Subagents run inside the agent process and appear in \
             `session.get` -> agent_info.subagents / background instead."),
        m::<SubagentLogParams, SubagentLog>("session.subagent_log").d(
            "Read one of a terminal's subagents (Claude) from its own transcript: what it was asked, what it said, \
             its tool calls and the first line of each result. Read from `from` = 0, then pass the last `next` to \
             follow it while `running`."),
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
        m::<NeedsYouGetParams, NeedsYouGetResult>("needs_you.get").d(
            "Where one needs-you item stands: open (with the item), resolved (resolution, by whom), withdrawn (its terminal or its \
             asker went away) or timeout. wait_secs blocks until it is answered (max 600). For a call made with caller.no_wait, \
             also the result or error that call ended with. Items closed before midnad last restarted are not found."),
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
            "Check an action and, if a rule says ask, raise an approval for the human and block until they answer or timeout_secs passes \
             (default setting policy.request_timeout_secs). With caller.no_wait it answers at once with error 6 and data.needs_you_id \
             instead; poll needs_you.get. The approval is withdrawn (decision deny, source withdrawn) if the caller disconnects or its \
             terminal closes first."),
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
            "List triggers: webhook ones (GitHub/Bitbucket event -> start an agent, run a command, or raise attention) and local \
             ones (an agent hook, a midna event or an idle terminal -> type into that terminal, put a custom status on it, …). \
             `state`: needs_secret (a human must paste the secret), draft (ready to enable), active, paused. `builtin` marks the \
             ones midna ships (they can be edited, paused or removed)."),
        m::<TriggerAddParams, Trigger>("trigger.add").mutating().d(
            "Create a webhook trigger. It starts as needs_secret and never fires until a human sets its secret and enables it; \
             agents can draft freely. event: GitHub `pull_request.opened`/`push`/`check_run.completed` (event[.action]), \
             Bitbucket `pullrequest:created`. filter: repo (owner/name), branch, action, label (globs). action: \
             {kind:start_agent, project_id, agent, prompt_template} | {kind:run_command, project_id, command, background?, headless?, timeout_secs?} | {kind:attention, message}. \
             Templates: {{pr.number}} {{pr.title}} {{repo}} {{branch}} {{sender}} {{url}} {{action}} or any payload path like {{pull_request.head.ref}}. \
             In run_command each value is shell-quoted; it opens a monitor terminal as a tab, or with background:true in the sidebar's \
             Background group, or with headless:true runs with no terminal (exit code and output tail go on the delivery's command_runs; \
             stopped after timeout_secs, default 1800). Set github_hook_id to enable missed-delivery recovery. \
             LOCAL triggers (source: local) need no secret; pass enabled: true to turn one on at once (agents may, when the human \
             asked for it). event: `hook.<HookEvent>` (hook.Stop, hook.UserPromptSubmit, hook.Notification, hook.PreCompact; \
             data = the hook payload), any midna event kind (agent.prompt_blocked {hook, message, prompt}, agent.turn_ended, \
             session.status, needs_you.raised; data = the event data), `idle` (filter.idle_minutes: no prompt or turn for that \
             long; fires once per idle stretch), or `schedule` (filter.cron: `min hour dom month dow` in local time, names and \
             @hourly/@daily/@weekly/@monthly/@yearly ok, `@every 55m` for an interval that doesn't divide the hour; data {cron, scheduled_for, local_time}; without a session/project/agent \
             filter it fires once about no terminal, with one it acts on every running terminal that matches). \
             filter: session, project, agent, idle_minutes, cron, match {dotted.path: glob} \
             (case-insensitive, every entry must match). Extra actions, acting on the terminal that fired: \
             {kind:send_to_session, steps:[{text, enter=true}]} (typed in order; each step waits until the agent is ready), \
             {kind:set_status, label, color (red|orange|amber|yellow|green|teal|blue|purple|pink|gray|#rrggbb), icon?, \
             base (idle|working|needs_you|done|failed: still drives sorting, notifications, Needs You), clear_on (prompt|turn|status|never)}, \
             {kind:clear_status}. Any trigger may also {kind:notify, title, body?, sound=true, category?, open?, id?} (a macOS notification, \
             category from_trigger; clicking it selects the terminal that fired, or opens `open`, a URL (a webhook's defaults \
             to what it's about); a firing with the same `id` replaces the one still showing). Local templates: {{last_prompt}} (the terminal's latest prompt, in full), {{event}}, \
             {{session.id|name|project_id|agent|status}}, {{data.<path>}} or {{<path>}}. cooldown_secs (default 60) spaces \
             firings per terminal; what a trigger causes never fires triggers. Example (compact, then resend): \
             {source:local, event:agent.prompt_blocked, filter:{match:{message:\"*Compact first*\"}}, \
             action:{kind:send_to_session, steps:[{text:\"/compact\"},{text:\"{{last_prompt}}\"}]}, enabled:true}."),
        m::<TriggerUpdateParams, Trigger>("trigger.update").mutating().d(
            "Edit a trigger (name, event, filter, action, github_hook_id, session_name_template, cooldown_secs). When an agent \
             changes the action or source of an enabled webhook trigger, it goes back to draft and a human must re-enable it \
             (local triggers stay enabled)."),
        m::<TriggerSetEnabledParams, Trigger>("trigger.set_enabled").mutating().d(
            "Enable or pause a trigger. Anyone may pause, and agents may enable local triggers. Enabling a webhook trigger is \
             human only and needs the secret set: an agent asking to enable raises a needs-you confirmation (or a secret_needed \
             item if the secret is missing)."),
        m::<TriggerSetSecretParams, OkResult>("trigger.set_secret").mutating().human().d(
            "Set a trigger's webhook signing secret (stored in the macOS Keychain; never returned or logged). Human only: \
             an agent calling this raises a secret_needed needs-you item and the secret it sent is discarded."),
        m::<IdParams, OkResult>("trigger.remove").mutating().d(
            "Remove a trigger and its secret. Agents may remove triggers that are not enabled; removing an enabled trigger \
             asks the human."),
        m::<TriggerDeliveriesParams, Vec<Delivery>>("trigger.deliveries").d(
            "List received webhook deliveries and local firings (source local), oldest first (default last 50). verdict: verified (fired), bad_signature, \
             filtered (a trigger listens but filters/state said no), no_trigger, replayed, recovered (missed, then fetched from GitHub). \
             command_runs: headless run_command runs {command, started_at, finished_at (none while running), exit_code, signal, \
             timed_out, output (last 50 lines), error}; `trigger.command_finished` is emitted when one ends."),
        m::<TriggerReplayParams, Delivery>("trigger.replay").mutating().d(
            "Run a past delivery through the triggers again (same filters, no signature check since it was verified when it \
             arrived). Deliveries that failed signature checks can't be replayed."),
        m::<TriggerTestParams, Delivery>("trigger.test").d(
            "Dry-run one trigger against a sample payload: shows how the event and filters evaluate and the rendered \
             prompt/command. Local triggers: payload is the hook payload or event data, event defaults to the trigger's, and \
             session names the terminal (filters, {{session.*}}, {{last_prompt}}). Nothing runs and nothing is recorded."),
        // secrets
        m::<SecretListParams, Vec<Secret>>("secret.list").d(
            "Stored secrets (names and metadata only; values never leave midnad). `[secret:NAME]` in a prompt is one of \
             these: the human pasted a secret, or an agent saved one, and midna keeps the value out of your context. \
             added_by is who stored it (absent = the human); exposed = an agent sent the value itself, so a model saw it. Use it with \
             `midna secret exec NAME -- <command>` ($NAME is set in that command's environment and its output is scrubbed) or \
             `midna secret write NAME <file>` (.env style). Never ask the human to paste the value."),
        m::<SecretSetParams, Secret>("secret.set").mutating().d(
            "Store a secret so it can be used as [secret:NAME] without its value passing through you again. Agents: pipe \
             the value in with `<command> | midna secret save NAME` (e.g. `gh auth token | midna secret save GH_TOKEN`) so it \
             never enters your context. A value you pass here yourself already has, so the secret is marked exposed (the \
             human may rotate it). An agent's secret belongs to its terminal's project unless global. Replacing a secret the \
             human stored asks the human (needs-you); new names and agent-stored ones are saved at once."),
        m::<SecretReplaceParams, Secret>("secret.replace").mutating().human().d(
            "Internal: approving an agent's replacement of a secret the human stored runs this (the agent's value waits in \
             the Keychain until then). Agents call secret.set / `midna secret save` instead."),
        m::<SecretNameParams, OkResult>("secret.remove").mutating().human().d(
            "Delete a stored secret. Human only: an agent's call becomes a needs-you confirmation."),
        m::<SecretWriteParams, SecretWriteResult>("secret.write").mutating().d(
            "Write a secret into a .env-style file as KEY=value (replacing an existing KEY line; a new file is created 0600) \
             without the value passing through you. Don't read the file back afterwards; that would put the value in your context."),
        m::<SecretExecEnvParams, SecretExecEnvResult>("secret.exec_env").mutating().d(
            "Internal: `midna secret exec` resolves values here to set them in the command's environment. Answers only the \
             midna CLI's own exec; every other caller is refused. Use `midna secret exec`."),
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
        m::<InsightsDetailParams, InsightsDetail>("insights.detail").d(
            "The Insights widgets' data for a range (today|yesterday|week|month), computed from the event log: agents \
             working at once (samples per 10 min or hour, peak, time with two or more), turn lengths (bins, median, \
             longest), needs-you waits, per-terminal working / blocked-on-you / idle-after-a-turn time, approvals by \
             what was approved, denials and interrupted turns per day, a 7-day × 24-hour heatmap of agent work and \
             your activity, working time per project, spend per model, and records over the whole log."),
        m::<InsightsActivityParams, Vec<Event>>("insights.activity").d(
            "Recent meaningful activity (status changes, turns, approvals, rules, needs-you), newest first. Use events.list for the raw log."),
        // usage
        m::<NoParams, UsageGetResult>("usage.get").d(
            "Claude's plan usage limits (account-wide): the 5-hour and weekly windows {used_percentage 0-100, resets_at}, \
             the latest any Claude terminal's status line reported, with when (`observed_at`) and which terminal. \
             `limited` = a window is at 100% and has not reset (new turns fail until `limited_until`); a window whose \
             resets_at has passed is marked `expired` (its percentage is out of date). A usage.limit_reached event fires \
             once when a window hits 100%. Each terminal's own last report is in session.get agent_info.rate_limits. \
             Empty until a Claude terminal has drawn its status line (setting agents.claude.statusline; API-key accounts \
             never report limits)."),
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
             Human only: an agent's call becomes a needs-you approval (tell the human why the update matters), unless the \
             human turned on agents.may_install_updates."),
        m::<UpdatesReportParams, OkResult>("updates.report").mutating().human().d(
            "Internal: the GUI reports its updater state here (agents call updates.status instead)."),
        // themes
        m::<NoParams, ThemesListResult>("themes.list").d(
            "List the color themes: the eight built-ins and the human's custom themes ($MIDNA_HOME/themes/<id>.json), \
             each with its UI colors and 16 terminal colors, plus files that failed to load, the `theme` / `theme.dark` / \
             `theme.light` settings and the theme the app shows now. To switch: settings.set theme <id> (or system). To \
             tweak one color: settings.set theme.colors [\"accent = #FF79C6\"]. To make a theme: write a file in `dir` \
             in the shape `format` describes; the app picks it up live."),
        m::<ThemesReportParams, OkResult>("themes.report").mutating().human().d(
            "Internal: the GUI reports the theme it shows and its terminal colors, which midnad applies to every terminal \
             (agents switch themes with settings.set theme)."),
        // permissions
        m::<NoParams, PermissionsStatus>("permissions.status").d(
            "macOS permissions midna uses (accessibility, notifications, login-items) as far as the daemon can see them, \
             each with the `midna permissions open <name>` command that opens the right System Settings pane. Only the \
             human can grant them; use needs_you.raise to ask."),
        // adopted agents (`midna shim`)
        m::<SessionAdoptParams, SessionAdoptResult>("session.adopt").mutating().d(
            "Internal (`midna shim`): a `claude` or `codex` typed into a shell terminal asks to run under midna. When \
             it's an interactive session and agents.adopt_typed is on, the reply is the command to run (midna's hooks \
             and MCP added) and the terminal works like an agent terminal until it exits; otherwise it runs as typed."),
        m::<SessionAdoptEndParams, SessionAdoptResult>("session.adopt_end").mutating().d(
            "Internal (`midna shim`): the adopted agent exited. The reply's `run` is the next command when a restart asked \
             for one (same conversation, same shell); otherwise the terminal is a plain shell again."),
        // agent hooks
        m::<AgentHookParams, AgentHookResult>("agent.hook").mutating().d(
            "Report an agent hook event (called by `midna hook claude|codex`). Drives terminal status, turns, prompt counts and cost."),
        m::<NoParams, HooksStatus>("hooks.status").d(
            "Whether midna's hooks are in Claude Code's and Codex's global config, so agents started by hand in a midna terminal \
             report status too. state: not_installed | current | stale (reinstall) | unavailable | error. Agents midna starts \
             itself always report (per_session = midna adds its hooks to them)."),
        m::<HooksPreviewParams, HooksPreview>("hooks.preview").d(
            "The exact change hooks.install (or, with uninstall, hooks.uninstall) would make to each global config file, as diff lines."),
        m::<HooksTargetParams, HooksStatus>("hooks.install").mutating().human().d(
            "Add (or refresh) midna's hooks in the global config of Claude Code (~/.claude/settings.json) and Codex \
             (~/.codex/config.toml; an existing notify keeps running). The hooks do nothing outside midna terminals. Human only."),
        m::<HooksTargetParams, HooksStatus>("hooks.uninstall").mutating().human().d(
            "Remove midna's hooks from the global config (Codex's previous notify is restored). Human only."),
        // scripts
        m::<ScriptRunParams, ScriptRunResult>("script.run").d(
            "Run the configured header, row or status script for a terminal and return its segments (built-in parts joined with +: \
             github, agent, worktree, branch, sync, diff, files, pr; or a custom executable printing JSON segments; \
             `midna explain scripts`). With slot status, `script` runs one of the script paths in ui.status.items; with \
             slot button, one in ui.header.buttons."),
        m::<ScriptClickParams, ScriptRunResult>("script.click").mutating().human().d(
            "Click a custom header button: run its script (listed in ui.header.buttons) with MIDNA_CLICK=1 and return the \
             button's new segments. Human only (the midna app calls it)."),
    ]
}
