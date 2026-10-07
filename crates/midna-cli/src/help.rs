//! Help text: one entry per verb. `midna help` is generated from this table and
//! `midna <verb> --help` prints the entry, so the two can't drift apart. Each entry names the
//! RPC methods it calls, so `--help` also says which steps are human only.
use midna_proto::catalog;

pub struct Verb {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    /// Usage lines (first line is the short form shown in `midna help`).
    pub usage: &'static str,
    pub summary: &'static str,
    pub details: &'static str,
    /// Catalog methods this verb calls.
    pub methods: &'static [&'static str],
}

pub static VERBS: &[Verb] = &[
    // ---------------------------------------------------------------- orientation
    Verb {
        name: "capabilities",
        aliases: &["caps"],
        usage: "capabilities [--json]",
        summary: "what midna can do for you, and what only the human can do",
        details: "A short overview for agents: the verbs and MCP tools by area, the human-only boundary and\n\
                  what to do instead. Works without a daemon. `--json` lists every method with its flags.",
        methods: &[],
    },
    Verb {
        name: "skill",
        aliases: &["guide"],
        usage: "skill [--path]",
        summary: "print the bundled agent guide (SKILL.md)",
        details: "Prints the guide: when to use which verb, attention etiquette, approvals, rules, triggers,\n\
                  settings, windows, and never routing around a denial. In a midna terminal the same file\n\
                  is at $MIDNA_SKILL. `--path` prints that path instead.",
        methods: &[],
    },
    Verb {
        name: "explain",
        aliases: &["why"],
        usage: "explain <id|method|setting|topic>\n       explain <command|tool|path|cli|window> <value...>",
        summary: "explain a terminal's status, a rule, a trigger, a needs-you item, a method or a setting",
        details: "Ids: terminal (8 hex) = why it has its status, recent status events, open needs-you items;\n\
                  r_… rule = what it matches, who added it, when it fired, how to get it removed;\n\
                  t_… trigger = what it does, what it is waiting for, recent deliveries;\n\
                  n_… needs-you item = what it asks and who can answer it; p_… project; d_… delivery.\n\
                  Also a method (`session.open` or `session_open`), a setting key (`theme`), or a topic:\n\
                  status, rules, approvals, triggers, needs-you, settings, scripts, themes, windows, human-only, mcp.\n\
                  With a kind and a value it explains which rule decides that action and why\n\
                  (e.g. `midna explain command -- git push --force`).",
        methods: &["session.get", "rule.list", "trigger.list", "needs_you.list", "events.list", "policy.check", "settings.get"],
    },
    Verb {
        name: "info",
        aliases: &[],
        usage: "info",
        summary: "daemon info and your role (human or agent)",
        details: "Version, pid, uptime, MIDNA_HOME, socket, and the role midnad gave this connection.",
        methods: &["daemon.info"],
    },
    Verb {
        name: "schema",
        aliases: &[],
        usage: "schema [method] | schema --list",
        summary: "the API: the whole OpenRPC document, one method, or a list of methods",
        details: "No argument prints the OpenRPC document for every method. With a method (dotted or the\n\
                  MCP tool name, e.g. `rule.add` or `rule_add`) it prints that method's description, flags\n\
                  and a self-contained JSON Schema for its params and result. `--list` prints one line per\n\
                  method. Works without a daemon.",
        methods: &["rpc.discover"],
    },
    Verb {
        name: "daemon",
        aliases: &[],
        usage: "daemon info | restart | stop | upgrade <path> | reset [--drop-rules]",
        summary: "manage midnad itself",
        details: "restart  graceful restart; terminals keep running (anyone may)\n\
                  stop     stop midnad and hang up every terminal (human only: asks the human)\n\
                  upgrade  replace midnad in place with another binary (human only: asks the human)\n\
                  reset    close every terminal, remove projects, triggers and needs-you items, reset settings;\n\
                  \x20        keeps the event log and rules (--drop-rules removes rules too). Human only: asks the human",
        methods: &["daemon.info", "daemon.restart", "daemon.stop", "daemon.upgrade", "daemon.reset"],
    },
    // ---------------------------------------------------------------- projects / terminals
    Verb {
        name: "projects",
        aliases: &["project"],
        usage: "projects [list]\n       projects discover\n       projects add <path> [--name N]\n       projects update <id> [--name N] [--icon I]\n       \
                projects add-command <id> --name N [--pinned] -- <command line>\n       projects remove-command <id> <name>\n       \
                projects remove <id>",
        summary: "list and edit projects (directories that group terminals)",
        details: "Saved commands show up in the human's command bar as \"Run <name>\" (pinned ones are\n\
                  suggested). `discover` lists folders under the projects.roots setting\n\
                  that can be opened. `remove` is human only (it asks the human), except for a project midna\n\
                  created for an agent's `open --cwd` (auto_created) once none of its terminals runs.",
        methods: &["project.list", "project.discover", "project.add", "project.update", "project.remove"],
    },
    Verb {
        name: "list",
        aliases: &["ls"],
        usage: "list [--project P]",
        summary: "list terminals with status",
        details: "Status: idle, working, needs_you, done (finished, not seen yet), failed, exited.\n\
                  `midna explain <id>` says why a terminal has its status.",
        methods: &["session.list"],
    },
    Verb {
        name: "get",
        aliases: &["show"],
        usage: "get <id>",
        summary: "one terminal: status, cwd, command, agent info",
        details: "Everything session.get returns: kind, agent, cwd, command, title, status {state, reason, since},\n\
                  repo info and, for agents, agent_info (conversation, version, background work, subagents,\n\
                  queued restart). `--json` for the full object; `midna explain <id>` says why it has its status.",
        methods: &["session.get"],
    },
    Verb {
        name: "open",
        aliases: &["new"],
        usage: "open [--agent claude|codex [--prompt TEXT] [--resume ID]] [--monitor CMD] [--name N]\n            [--project P] [--cwd DIR] [--background] [--close-on-exit] [-- argv... | -- agent args...]",
        summary: "open a terminal: a shell (default), a monitor, or an agent",
        details: "Shell: the login shell, or `-- argv...`. Monitor: `--monitor CMD` runs a command the human\n\
                  should watch; a failure raises a needs-you item. Agent: `--agent claude|codex` starts a\n\
                  new agent with midna's hooks and MCP server, optionally with `--prompt`; `--resume ID`\n\
                  reopens a conversation, and words after `--` go to the agent itself\n\
                  (`-- --append-system-prompt \"…\" --settings board.json --model opus`). A `--settings`\n\
                  or `--append-system-prompt` is merged with midna's own (Claude reads only one); Codex\n\
                  `-c developer_instructions=…` / `-c notify=…` likewise. Without\n\
                  --project the terminal goes in the project containing --cwd (default: this directory).\n\
                  --background puts it in the sidebar's folded Background group instead of under its\n\
                  project (a dev server, a watcher); it is still listed, readable and can raise needs-you.\n\
                  --close-on-exit closes the terminal when its agent or command exits normally (exit 0,\n\
                  ctrl-c, hang-up); a failed exit stays open with its needs-you item.\n\
                  Prints the new terminal's id.",
        methods: &["session.open"],
    },
    Verb {
        name: "close",
        aliases: &[],
        usage: "close <id> [--force]",
        summary: "close a terminal and kill its process",
        details: "A working terminal needs --force. `close --force` and closing another terminal may ask the\n\
                  human (settings agents.may_close_idle and agents.may_force_close, rules on `close …`); add\n\
                  --no-wait to get the needs-you id back at once instead of waiting (see `midna needs`).\n\
                  An approval still open when its terminal closes is withdrawn.",
        methods: &["session.close"],
    },
    Verb {
        name: "rename",
        aliases: &[],
        usage: "rename <id> <name...>",
        summary: "rename a terminal",
        details: "Changes the sidebar label.",
        methods: &["session.rename"],
    },
    Verb {
        name: "background",
        aliases: &["bg"],
        usage: "background <id> [--off]",
        summary: "move a terminal to the sidebar's Background group (or back with --off)",
        details: "Background terminals sit folded and dimmed at the bottom of the sidebar instead of under\n\
                  their project. Nothing else changes: the process keeps running and needs-you items still show.",
        methods: &["session.set_background"],
    },
    Verb {
        name: "restart",
        aliases: &[],
        usage: "restart <id> [--idle] [--fresh] [--force] [--reason TEXT] | restart <id> --cancel | --decline",
        summary: "restart a terminal's command in place (same id, same tab)",
        details: "Agent terminals reopen the same conversation (claude --resume / codex resume) unless --fresh. \
                  --idle queues it until the agent is idle with no background work, subagents or scheduled wakeups in flight; \
                  without it, a restart that would kill background work is refused unless --force. --cancel drops a queued restart. \
                  --decline answers \"Not now\" to an agent update prompt (hidden until a newer update). \
                  Policy-checked as `restart <id>`; the default policy asks the human.",
        methods: &["session.restart", "session.restart_cancel", "session.update_decline"],
    },
    Verb {
        name: "procs",
        aliases: &["processes"],
        usage: "procs <id>",
        summary: "list the OS processes under a terminal, and what its agent has in flight",
        details: "The terminal's process tree (pid, pgid, command), each process tagged with the agent's background task \
                  it belongs to, then the agent's background tasks, subagents and scheduled wakeups as its hooks report them.",
        methods: &["session.processes"],
    },
    Verb {
        name: "subagent",
        aliases: &["subagents"],
        usage: "subagent <terminal-id> <agent-id>",
        summary: "read one of an agent terminal's subagents: its prompt, tool calls and what it said",
        details: "From the subagent's own transcript (Claude). Ids are in `midna procs <terminal-id>` (running and\n\
                  finished this turn). The human opens the same thing from the header's subagents chip (⌥⌘A).",
        methods: &["session.subagent_log"],
    },
    Verb {
        name: "links",
        aliases: &[],
        usage: "links [--session <id>] [--kind pr|artifact|web|file] [--pinned] [--turn N|last]\n       \
                links pin|unpin <link-id|url|path> [--session <id>]\n       \
                links remove <link-id|url|path> [--session <id>]\n       \
                links add <url|path> [--title T] [--why <note>] [--no-pin] [--session <id>]",
        summary: "the links, PRs, artifacts and files that came up in an agent terminal's conversation",
        details: "midna collects them from the agent's transcript as it works: URLs the human pasted or you wrote or\n\
                  fetched, PRs and artifacts your tools printed, files you created or edited. The human sees them\n\
                  under the header's links button (⌘L). Pin what they will want to come back to (the PR, the\n\
                  design, the doc they sent); `add` puts in something the transcript wouldn't show as a link, pinned\n\
                  unless --no-pin, with an optional title and --why it matters (shown under it); `remove` drops one\n\
                  for good (only `add` brings it back). Each link knows the prompts it came up in (numbered as in\n\
                  `midna prompts`); --turn keeps one, e.g.\n\
                  `midna links --kind file --turn last` = the files edited since the last prompt. Defaults to your own terminal.",
        methods: &["links.list", "links.pin", "links.remove", "links.add"],
    },
    Verb {
        name: "queue",
        aliases: &[],
        usage: "queue [list] [--session <id>]\n       \
                queue add <text…|-> [--idle <dur> | --at <time> | --after <id> | --when-idle] [--no-enter]\n            \
                [--image PATH]… [--first | --position N] [--session <id>]\n       \
                queue edit <q_id> [--text T|-] [--idle <dur> | --at <time> | --after <id> | --when-idle]\n            \
                [--enter | --no-enter] [--retry] [--session <id>]\n       \
                queue rm <q_id> | mv <q_id> <to> | send-now <q_id> | clear | pause | resume   [--session <id>]",
        summary: "queue messages midna types into a terminal once its agent is ready (follow-ups, sequencing)",
        details: "A queued message is typed (and Enter pressed, unless --no-enter) once it is first in line, its\n\
                  `when` holds and the agent is ready for input: not working or waiting on the human, no dialog\n\
                  on screen, nothing typed in its input box. Then the next one goes. Every verb defaults to your\n\
                  own terminal; --session <id> targets another. The human sees the queue on the terminal and can\n\
                  edit, reorder or remove it.\n\
                  Use it to leave yourself a follow-up you can't send mid-turn (`queue add /compact`, then\n\
                  `queue add \"continue with step 3\"`), to hand another terminal its next step without\n\
                  interrupting it, or to sequence work across terminals: `queue add --session B --after A \"…\"`\n\
                  waits until terminal A is idle with an empty queue. Don't use it for something to send now\n\
                  (`midna send`) or for a recurring reaction (a local trigger).\n\
                  when: default as soon as the agent is ready (--when-idle sets that back on `edit`);\n\
                  --idle 10m|1h|90 once the terminal has been idle that long (bare number = minutes);\n\
                  --at 18:00 (today, or tomorrow if that time has passed; local) or RFC 3339;\n\
                  --after <id> once that terminal has finished.\n\
                  Text: the words after `add` joined with spaces, everything after `--`, or `-` to read stdin.\n\
                  Positions are 1-based here (1 = next to go); the RPC's `position`/`to` are 0-based.\n\
                  `list` shows each message's state (a failed one shows its error and holds the queue until\n\
                  `edit --retry`, `send-now` or `rm`), its when, who queued it, and what the first one is waiting for.\n\
                  `send-now` types one at once, even mid-turn (refused while a permission prompt is up).\n\
                  `pause` holds the whole queue until `resume`.",
        methods: &["queue.list", "queue.add", "queue.update", "queue.remove", "queue.clear", "queue.move", "queue.send_now", "queue.pause"],
    },
    Verb {
        name: "notify",
        aliases: &["notifications"],
        usage: "notify [--session <id> | --global]\n       \
                notify set <key> on|off|default [--session <id> | --global]\n       \
                notify mute|unmute [--session <id>]\n       \
                notify send \"<title>\" [--detail \"<body>\"] [--sound] [--kind <kind>] [--session <id>] [--id <id>] [--open <url>]\n       \
                            [--action <label>]… [--wait <secs>]\n       \
                notify withdraw <id> | response <id> [--wait <secs>]\n       \
                notify kinds [list | add|update <key> [--label L] [--description D] [--color C] [--stay S] [--set field=value]… | rm <key>]\n       \
                notify media | import <file> [--for <kind>|all] | remove <name> | test [<kind>]\n       \
                notify play <kind>|<sound> [--volume 0-100] [--session <id>]\n       \
                notify clear [--session <id>]",
        summary: "which macOS notifications midna posts, per terminal or globally; send the human one",
        details: "Lists each kind (approval, attention, failed, turn_done, agent, requests, background, pr_checks,\n\
                  exited, triggers, restarted), whether it's on globally and for the terminal (default: yours).\n\
                  `set` overrides a kind for one terminal (default drops the override); --global changes the\n\
                  `notify.<key>` setting. `mute` silences a terminal. Change these when the human asks (\"tell me\n\
                  when every turn ends here\"). `send` notifies the human on purpose: when they asked to be told, or\n\
                  a result needs them while they may be away. Not for routine progress; approvals, failures and long\n\
                  turns already notify. At most 6 a minute (--sound plays the human's sound for agent notifications).\n\
                  It works from outside midna terminals too (a background server, a cron job). --open <url> makes a\n\
                  click open that URL instead of selecting the terminal. --id <id> names it: sending again with the\n\
                  same id replaces the one still showing (a repeating alert), and `withdraw <id>` takes it away once\n\
                  it no longer applies. --action <label> (up to 4) adds buttons: --wait 10m blocks until the human\n\
                  picks one, clicks or dismisses it and prints the label, `clicked`, `dismissed` or `no response`;\n\
                  without --wait, `response <id>` asks later, and every response is a `notify.responded` event\n\
                  (`midna trigger add --event notify.responded --match action='Snooze*' …` reacts to it). Buttons\n\
                  and clicks need the app running.\n\
                  `kinds` lists the kinds the human added (deploys, ci, …); `send --kind <key>` sends as one, with its\n\
                  own sound, color and switch. `kinds add` makes one when the human asks (--stay 0 = it stays on\n\
                  screen until handled; --color a theme color need|ok|err|work|accent|dim or #rrggbb; --set\n\
                  sound=Glass, push=false, …); `kinds rm` removes it and its settings.\n\
                  Every kind also has notify.stay.<kind> (seconds on screen in the floating badge and the in-app\n\
                  card, 0 = until handled) and notify.color.<kind>. notify.badge (background|always|off) shows or\n\
                  hides the floating badge; notify.badge.corner says which corner it sits in.\n\
                  Each kind has a sound, a volume and an image: settings notify.sound.<kind> (none, one of midna's\n\
                  Twilight sounds, the defaults: Portal, Call, Uh-oh, Strum, Hm, Rise, Nn-nn, Whoosh, Fwip, Close,\n\
                  Tick, Thump, Tick-tick; a macOS sound like Glass; or an imported file), notify.volume.<kind>\n\
                  (0-100, times the master notify.volume) and notify.image.<kind> (empty = notify.image, every\n\
                  notification's image). `import` copies a sound\n\
                  (aiff, wav, mp3, m4a, caf) or image (png, jpg, gif) into midna, --for sets it for those kinds;\n\
                  `media` lists what's there; `test` shows a kind's notification even when it's off. Pick sounds\n\
                  and images only when the human asks.\n\
                  Sound effects play in the app with no banner and use the same settings: approved, denied,\n\
                  queue_sent, image_added, closed (what the human does) and switched, command_bar, copied (UI cues).\n\
                  notify.sounds turns every sound off; notify.sounds_in_app off silences sounds while midna is the\n\
                  frontmost app. `play`\n\
                  plays a kind's sound or a sound by name now (at most 6 a minute), when the human asks for one.\n\
                  `clear` removes every midna notification from Notification Center (--session: only that\n\
                  terminal's); opening a terminal already removes its own.",
        methods: &["notify.list", "notify.set", "notify.send", "notify.kinds.list", "notify.kinds.add", "notify.kinds.remove", "notify.media", "notify.import", "notify.remove", "notify.test", "notify.play", "notify.clear", "notify.withdraw", "notify.response", "notify.history", "notify.read"],
    },
    Verb {
        name: "read",
        aliases: &[],
        usage: "read <id> [--lines N] [--screen]",
        summary: "read a terminal's text",
        details: "The last N lines of scrollback + screen (default 50), or exactly the visible screen.",
        methods: &["session.read"],
    },
    Verb {
        name: "prompts",
        aliases: &[],
        usage: "prompts <id> [--jump N|prev|next|latest|live]",
        summary: "list the human's prompts in an agent terminal, or scroll the agent to one",
        details: "Numbered from 1, oldest first; ▸ marks the prompt the agent's view is in. Prompts sent before a\n\
                  /clear are listed but the agent no longer shows them. --jump scrolls the agent's own view (it\n\
                  presses page keys until the prompt shows, a second or two).",
        methods: &["session.prompts", "session.jump_prompt"],
    },
    Verb {
        name: "send",
        aliases: &["type"],
        usage: "send <id> [--image PATH]... <text...> [--no-enter]",
        summary: "type text into a terminal (or message its agent), then Enter",
        details: "Free text with flags goes after `--`: `midna send ab12cd34 -- ls --all`. Works for Claude Code\n\
                  and Codex: multi-line text stays one message. --image (repeatable) attaches a PNG, JPEG, GIF or\n\
                  WebP (other formats are converted) ahead of the text; the agent shows it as [Image #N].\n\
                  Never answer another agent's permission prompt unless `midna read <id> --screen` shows it.",
        methods: &["session.input"],
    },
    Verb {
        name: "key",
        aliases: &[],
        usage: "key <id> <key>...",
        summary: "press keys in a terminal (ctrl-c, escape, up, shift-tab, enter…)",
        details: "Each key is encoded the way the app in the terminal expects (legacy, modifyOtherKeys or\n\
                  kitty), like a real keypress. Several keys are pressed in order.",
        methods: &["session.key"],
    },
    Verb {
        name: "focus",
        aliases: &[],
        usage: "focus <id>",
        summary: "bring a terminal to the front in the GUI",
        details: "Always allowed. Prints a note when the GUI isn't running.",
        methods: &["session.focus"],
    },
    // ---------------------------------------------------------------- the human
    Verb {
        name: "attention",
        aliases: &[],
        usage: "attention <message...> [--note] [--detail D]",
        summary: "get the human's attention (blocked, or --note for FYI)",
        details: "Default = blocked: you cannot continue without the human. --note = FYI. Keep the\n\
                  message to one line; put more in --detail. Check `midna needs` first to avoid duplicates.",
        methods: &["needs_you.raise"],
    },
    Verb {
        name: "needs",
        aliases: &["needs-you"],
        usage: "needs\n       needs get <n_id>\n       needs wait <n_id> [--timeout S]",
        summary: "list what is waiting on the human; follow one item",
        details: "Approvals, permission prompts (also an agent's \"trust this folder?\" dialog), blocked agents,\n\
                  notes, failures, rule-removal requests, missing webhook secrets. `midna explain <n_id>` explains one.\n\
                  get   where one item stands: open, resolved (how, by whom), withdrawn (its terminal or its asker\n\
                  \x20     went away) or timeout; for a --no-wait call also what that call ended with.\n\
                  wait  block until it is answered (default 300 s), then print the same. Exit 4 while still open.",
        methods: &["needs_you.list", "needs_you.get"],
    },
    Verb {
        name: "approve",
        aliases: &["resolve"],
        usage: "approve <needs-you-id> [--scope once|session|always|<N>m] [--deny]\n       approve <needs-you-id> --done | --restart",
        summary: "answer a needs-you item (usually the human's job)",
        details: "Agents may approve only their own session's requests, and only when the human turned on\n\
                  approve.from_cli. --done marks a blocked item handled; --restart restarts a failed terminal.\n\
                  Approvals of human-only actions can only be answered by the human.",
        methods: &["needs_you.resolve"],
    },
    // ---------------------------------------------------------------- policy
    Verb {
        name: "check",
        aliases: &[],
        usage: "check <command|tool|path|cli|window> <value...>",
        summary: "test an action against the rules (no side effects)",
        details: "Prints the decision, the winning rule and the trace. Use `--` for values with flags:\n\
                  `midna check command -- git push --force`.",
        methods: &["policy.check"],
    },
    Verb {
        name: "rules",
        aliases: &["rule"],
        usage: "rules [list]\n       rules add <allow|ask|deny> <command|tool|path|cli|window> <pattern...>\n                 [--scope global|project:ID|session:ID] [--expires SECS]\n       \
                rules request-removal <rule-id> --reason R\n       rules remove <rule-id>   (human only)\n       \
                rules restore <rule-id>  (human only: undo a removal, same id)",
        summary: "list, add, and request removal of policy rules",
        details: "Agents may add rules; only the human removes them. To get one removed, run\n\
                  `midna rules request-removal <id> --reason \"…\"`: the human sees it and decides.\n\
                  Patterns are globs: `git push --force*`, `Bash(rm -rf*)`.",
        methods: &["rule.list", "rule.add", "rule.request_removal", "rule.remove", "rule.restore"],
    },
    // ---------------------------------------------------------------- triggers / webhooks
    Verb {
        name: "triggers",
        aliases: &["trigger"],
        usage: "triggers list | show <id> | deliveries [--trigger ID] [--limit N] | replay <delivery-id>\n       \
                triggers add --name N --event E [--source github|bitbucket] [--repo R] [--branch B] [--action A]\n            \
                [--label L] (--agent claude|codex --prompt TEMPLATE | --run CMD [--background | --headless [--timeout 30m]]\n            \
                | --attention MSG)\n            \
                [--project P] [--hook-id N] [--session-name TEMPLATE]\n       \
                triggers add --name N --event hook.<Hook>|<midna event kind>|idle|schedule [--source local]\n            \
                [--session ID] [--in-project P] [--for-agent claude|codex] [--idle-for 55m] [--cron '0 9 * * mon-fri']\n            \
                [--between 13:00-17:00] [--starts '2026-10-06 13:00'] [--ends 2026-10-31] [--max-runs N]\n            \
                [--match path=glob]...\n            \
                (--send TEXT [--send TEXT | --send-no-enter TEXT]... | --set-status LABEL --color C --base B\n             \
                [--clear-on prompt|turn|status|never] [--icon I] | --clear-status | --notify TITLE [--notify-body B] [--notify-kind K] [--notify-open URL] [--notify-id ID] [--silent]\n             \
                | --attention MSG | --run CMD --project P [--background | --headless [--timeout 30m]])\n            \
                [--cooldown 60s] [--enable]\n       \
                triggers add|update … [--action-json '<TriggerAction>'] [--filter-json '<TriggerFilter>']\n       \
                triggers update <id> [same flags] | enable <id> | disable <id> | remove <id>\n       \
                triggers set-secret <id>   (human only; reads stdin)\n       \
                triggers test <id> [--payload FILE|-|JSON] [--event E] [--session ID]",
        summary: "triggers: GitHub/Bitbucket webhooks, or local agent hooks / midna events / idle terminals / schedules, that act",
        details: "Webhook triggers: agents draft freely; they start as needs_secret. Only the human pastes the signing\n\
                  secret and enables one (`enable` from an agent asks the human). Anyone may pause.\n\
                  Templates: {{pr.number}} {{pr.title}} {{repo}} {{branch}} {{sender}} {{url}} or any payload path.\n\
                  --run opens a monitor terminal as a tab; --background opens it in the sidebar's Background group;\n\
                  --headless runs it with no terminal: `deliveries` / `replay` show its exit code and output tail\n\
                  (stopped after --timeout, default 30m). On update, --background/--headless alone change the current\n\
                  command; --run CMD alone makes it a tab again.\n\
                  \n\
                  Local triggers (source local; inferred for hook.*, agent.*, session.*, needs_you.*, idle, schedule, or any\n\
                  local-only flag) fire on this Mac and act on the terminal that fired. Agents may add, enable\n\
                  (--enable) and disable them when the human asked for one. --send steps run in order, each waiting\n\
                  until the agent is ready. --match path=glob (repeatable) matches the hook payload / event data,\n\
                  case-insensitive; an empty glob removes the key on update; an empty value clears a filter flag.\n\
                  Durations: 90s, 55m, 1h30m; a bare number is minutes for --idle-for, seconds for --cooldown.\n\
                  --cron (implies --event schedule): minute hour day-of-month month day-of-week in local time, or\n\
                  @hourly/@daily/@weekly/@monthly/@yearly, or @every 55m for an interval cron can't say (*/55 is\n\
                  :00 and :55; @every counts from --starts, or runs one interval after it's added). Without\n\
                  --session/--in-project/--for-agent it fires once about no terminal (notify, attention, run, agent);\n\
                  with one it acts on each running match.\n\
                  Schedules also take --between HH:MM-HH:MM (local, end not included; may wrap midnight), --starts\n\
                  and --ends (local `2026-10-06 13:00` or a date), --max-runs N (1 = once; a new limit counts again).\n\
                  Templates: {{last_prompt}} {{event}} {{session.id|name|project_id|agent|status}} {{data.<path>}} or {{<path>}}.\n\
                  Example: triggers add --name \"Auto-compact\" --event agent.prompt_blocked \\\n\
                  \x20 --match 'message=*Compact first*' --send /compact --send '{{last_prompt}}' --enable\n\
                  `midna explain triggers` has the full rules.",
        methods: &[
            "trigger.list", "trigger.add", "trigger.update", "trigger.set_enabled", "trigger.set_secret", "trigger.remove",
            "trigger.deliveries", "trigger.replay", "trigger.test",
        ],
    },
    Verb {
        name: "webhooks",
        aliases: &["webhook"],
        usage: "webhooks status | reconcile\n       webhooks configure <tailscale_funnel|self_relay|midna_relay|off> [--port N] [--relay-url U]   (human only)",
        summary: "how webhooks reach this Mac",
        details: "status shows the path, health and the public URL to paste into GitHub. reconcile asks GitHub\n\
                  (via `gh`) for deliveries midnad missed. configure is human only: it asks the human.",
        methods: &["webhooks.status", "webhooks.reconcile", "webhooks.configure"],
    },
    Verb {
        name: "secret",
        aliases: &["secrets"],
        usage: "secret [list] [--all]\n       <command> | secret save <NAME> [--label L] [--global]\n       secret exec <NAME>[,<VAR=NAME>…] -- <command…>\n       secret write <NAME> <file> [--as KEY]\n       secret rm <NAME>   (human only)",
        summary: "save and use secrets without seeing them",
        details: "When the human pastes a secret into an agent terminal, midna stores it and the agent sees\n\
                  [secret:NAME] instead. `exec` runs a command with $NAME set (VAR=NAME sets $VAR) and masks\n\
                  the value in its output as ‹NAME›. `write` sets NAME (or --as KEY) in a .env-style file;\n\
                  don't read that file back. Values are never printed. `save` stores a token a command\n\
                  produced, read from stdin (`gh auth token | midna secret save GH_TOKEN`), so it never\n\
                  enters your context; replacing a secret the human stored asks them. `rm` asks the human.",
        methods: &["secret.list", "secret.set", "secret.replace", "secret.remove", "secret.write", "secret.exec_env"],
    },
    // ---------------------------------------------------------------- settings / windows / data
    Verb {
        name: "settings",
        aliases: &["setting", "config"],
        usage: "settings [list] | get <key> | set <key> <value> | reset <key>",
        summary: "read and change settings",
        details: "`list` marks human-only keys; agents can read them, and setting one asks the human.\n\
                  Values are JSON or bare strings (`true`, `42`, `dark`).",
        methods: &["settings.list", "settings.get", "settings.set", "settings.reset"],
    },
    Verb {
        name: "window",
        aliases: &[],
        usage: "window <front|keep_on_top|pop_out|snap|close|open_screen> [target] [value]\n       \
                window split <terminal-id> [side|stacked] | window split close\n       window list",
        summary: "ask the GUI to act on a window",
        details: "front, open_screen (rules|triggers|insights|notifications|settings|needs_you) and split (show a terminal\n\
                  beside the selected one) are always allowed; the rest need the human-only setting\n\
                  agents.may_move_windows. Rules can still deny `window` actions (e.g. `split*`).",
        methods: &["window.list", "window.command"],
    },
    Verb {
        name: "events",
        aliases: &["log"],
        usage: "events [--since N] [--limit N] [--kind PREFIX[,PREFIX]] [--follow]",
        summary: "the event log (every state change and every acting call)",
        details: "--follow streams live events. Kinds: session.*, needs_you.*, rule.*, trigger.*, settings.changed,\n\
                  agent.*, window.command, daemon.*, audit.",
        methods: &["events.list", "events.subscribe"],
    },
    Verb {
        name: "insights",
        aliases: &[],
        usage: "insights [--range today|yesterday|week|month] [--by project|agent|terminal|day]\n       \
                insights series <turns|messages|spend|working|waiting|approvals|triggers> [--range R] [--bucket hour|day] [--by B]\n       \
                insights detail [--range R]\n       \
                insights activity [--limit N]",
        summary: "totals, time series, widget detail and recent activity, computed from the event log",
        details: "Turns, human messages, spend, working and waiting time, approvals and triggers fired. `detail`: agents\n\
                  working at once, turn lengths, waits on you, idle time, approvals, corrections, heatmap and records.",
        methods: &["insights.summary", "insights.series", "insights.detail", "insights.activity"],
    },
    Verb {
        name: "usage",
        aliases: &[],
        usage: "usage",
        summary: "Claude's plan usage limits: the 5-hour and weekly windows, and whether you are limited",
        details: "The latest any Claude terminal's status line reported (account-wide), with when and where it was\n\
                  seen. LIMITED = a window is at 100% until it resets; new turns fail until then. A window whose\n\
                  reset time passed is marked out of date. Each terminal's own last report: `midna call session.get`\n\
                  (agent_info.rate_limits). Event usage.limit_reached fires once when a window hits 100%.",
        methods: &["usage.get"],
    },
    Verb {
        name: "commands",
        aliases: &["palette"],
        usage: "commands [list]\n       \
                commands add --title T (--rpc METHOD [--params JSON] | --screen S | --prefill TEXT | --focus ID)\n            \
                [--keywords K] [--sub S] [--icon I] [--featured HEADING] [--danger WHY] [--id ID] [--replace]\n       \
                commands remove <id>",
        summary: "user commands in the human's ⌘K palette ($MIDNA_HOME/commands.json)",
        details: "Add commands for workflows the human repeats; they show up in the palette at once. --rpc runs\n\
                  a daemon method as the human when they pick it (human-only methods always ask twice).\n\
                  Per-project scripts belong in `midna projects add-command` instead.",
        methods: &["ui.commands.list", "ui.commands.add", "ui.commands.remove"],
    },
    Verb {
        name: "themes",
        aliases: &["theme"],
        usage: "themes [list] | use <id|system> | format",
        summary: "color themes: the eight built-ins and custom ones in $MIDNA_HOME/themes",
        details: "list shows every theme (* = showing now) and custom theme files that failed to load. use <id>\n\
                  switches (settings theme; system follows macOS with theme.dark / theme.light). format prints\n\
                  the custom theme file shape. One color on top of any theme: setting theme.colors\n\
                  (`midna settings set theme.colors \"accent = #FF79C6\"`).",
        methods: &["themes.list"],
    },
    Verb {
        name: "updates",
        aliases: &["update"],
        usage: "updates [status] | check | install   (install: human only)",
        summary: "the midna app's auto-updater",
        details: "status shows the running and available version as the app reported it. check asks the app to\n\
                  check its feed now (harmless). install applies a downloaded update and relaunches the app;\n\
                  it is human only, so from an agent it asks the human. Terminals keep running either way.",
        methods: &["updates.status", "updates.check", "updates.install"],
    },
    Verb {
        name: "permissions",
        aliases: &["permission"],
        usage: "permissions [status] | open <accessibility|notifications|login-items>",
        summary: "macOS permissions midna uses, and the System Settings pane for each",
        details: "status says what midnad can see. open opens the pane in System Settings; only the human can\n\
                  grant anything there (from an agent, raise `midna attention` to ask).",
        methods: &["permissions.status"],
    },
    Verb {
        name: "hooks",
        aliases: &[],
        usage: "hooks [status] | preview [claude|codex] [--uninstall] | install [claude|codex] | uninstall [claude|codex]   (install/uninstall: human only)",
        summary: "midna's hooks in Claude Code's and Codex's global config",
        details: "Agents midna starts always report status. Installing the hooks globally makes a claude or codex\n\
                  typed into a midna terminal report too; outside midna terminals they do nothing, and Codex's\n\
                  existing notify keeps running. status: not_installed, current, stale (reinstall), unavailable.\n\
                  preview prints the exact change. install/uninstall are human only (from an agent they ask).",
        methods: &["hooks.status", "hooks.preview", "hooks.install", "hooks.uninstall"],
    },
    // ---------------------------------------------------------------- plumbing
    Verb {
        name: "mcp",
        aliases: &[],
        usage: "mcp",
        summary: "stdio MCP server exposing every method as a tool",
        details: "Speaks MCP (JSON-RPC over stdin/stdout). Tool names are method names with `.` → `_`\n\
                  (session_open, rule_add…), plus capabilities, explain and guide. Agents midna launches get it\n\
                  automatically (setting agents.mcp).",
        methods: &[],
    },
    Verb {
        name: "call",
        aliases: &[],
        usage: "call <method> [json-params]",
        summary: "call any method directly",
        details: "Example: `midna call session.scroll '{\"id\":\"ab12cd34\",\"to\":\"top\"}'`. `midna schema <method>`\n\
                  shows the params.",
        methods: &[],
    },
    Verb {
        name: "hook",
        aliases: &[],
        usage: "hook <claude|codex> [event]",
        summary: "agent hook bridge (used by midna's own agent wiring)",
        details: "Reads Claude's hook JSON on stdin and reports it; for PreToolUse it asks midna's policy.\n\
                  You don't need to call this yourself.",
        methods: &["agent.hook", "policy.request"],
    },
    Verb {
        name: "shim",
        aliases: &[],
        usage: "shim claude|codex [args…]",
        summary: "what `claude` / `codex` run in a midna shell terminal (setting agents.adopt_typed)",
        details: "Runs the agent under midna when it's an interactive session, so the terminal works like an agent\n\
                  terminal (and restarts into the same conversation inside the shell); anything else runs as typed.\n\
                  You don't need to call this yourself.",
        methods: &["session.adopt", "session.adopt_end"],
    },
    Verb {
        name: "help",
        aliases: &[],
        usage: "help [verb]",
        summary: "this list, or one verb's help (same as `midna <verb> --help`)",
        details: "",
        methods: &[],
    },
];

pub fn verb(name: &str) -> Option<&'static Verb> {
    VERBS.iter().find(|v| v.name == name || v.aliases.contains(&name))
}

/// `midna <verb> --help`.
pub fn verb_help(v: &Verb) -> String {
    // Usage lines: a new form starts at column 7 and gets `midna `; deeper lines continue one.
    let usage: Vec<String> = v
        .usage
        .lines()
        .enumerate()
        .map(|(i, l)| match (i, l.strip_prefix("       ")) {
            (0, _) => format!("usage: midna {l}"),
            (_, Some(rest)) if !rest.starts_with(' ') => format!("       midna {rest}"),
            _ => format!("      {l}"),
        })
        .collect();
    let mut out = format!("midna {} — {}\n\n{}\n", v.name, v.summary, usage.join("\n"));
    if !v.details.is_empty() {
        out.push('\n');
        out.push_str(v.details);
        out.push('\n');
    }
    if !v.aliases.is_empty() {
        out.push_str(&format!("\naliases: {}\n", v.aliases.join(", ")));
    }
    if !v.methods.is_empty() {
        out.push_str("\nmethods (MCP tool in brackets):\n");
        for m in v.methods {
            let flag = match catalog::method(m) {
                Some(s) if s.human_only => "  human only: an agent's call asks the human",
                Some(s) if !s.mutating => "  read only",
                _ => "",
            };
            out.push_str(&format!("  {m} [{}]{flag}\n", m.replace('.', "_")));
        }
    }
    out.push_str("\nAdd --json for machine-readable output. Exit codes: 0 ok, 1 refused or failed, 2 bad arguments, 3 daemon unreachable.");
    out
}

/// `midna help`.
pub fn usage() -> String {
    let mut out = String::from(
        "midna — drive the midna terminal from the shell, agents, or MCP\n\n\
         usage: midna <verb> [args] [--json]      midna <verb> --help for details\n\n",
    );
    let w = VERBS.iter().map(|v| v.usage.lines().next().unwrap_or("").len()).max().unwrap_or(0).min(46);
    for v in VERBS {
        let first = v.usage.lines().next().unwrap_or("");
        if first.len() > w {
            out.push_str(&format!("  {first}\n  {:w$}  {}\n", "", v.summary));
        } else {
            out.push_str(&format!("  {first:w$}  {}\n", v.summary));
        }
    }
    out.push_str(
        "\nNew here? `midna capabilities` (overview), `midna skill` (guide), `midna explain <id|topic>`.\n\
         Free text with flags goes after `--`, e.g. `midna check command -- git push --force`.\n\
         --no-wait (any verb): don't block on an approval; print its needs-you id and exit 4 (the call\n\
         carries on when the human answers; follow it with `midna needs get|wait <id>`).\n\
         Exit codes: 0 ok, 1 refused or failed, 2 bad arguments, 3 daemon unreachable, 4 waiting on the human.\n\
         Socket: $MIDNA_SOCKET, else $MIDNA_HOME/midnad.sock.",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_in_help_exists_and_every_reachable_method_has_a_verb() {
        let mut covered = std::collections::HashSet::new();
        for v in VERBS {
            for m in v.methods {
                assert!(catalog::method(m).is_some(), "{} lists unknown method {m}", v.name);
                covered.insert(*m);
            }
        }
        // Reached only through `midna call` (or the GUI's frame stream); listed so a new
        // method has to be placed deliberately.
        let call_only = [
            "session.resize", "session.scroll", "session.selection", "session.select_all", "session.link_at", "session.find",
            "stream.attach", "script.run", "script.click", "updates.report", "session.clear", "themes.report", "notify.respond",
        ];
        for m in catalog() {
            assert!(covered.contains(m.name) || call_only.contains(&m.name), "no verb covers {}", m.name);
        }
    }

    #[test]
    fn names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for v in VERBS {
            for n in std::iter::once(&v.name).chain(v.aliases) {
                assert!(seen.insert(*n), "duplicate verb name {n}");
            }
        }
    }
}
