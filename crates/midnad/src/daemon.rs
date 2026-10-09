//! The daemon: shared state (`Core` behind one mutex), the event log, and background loops.
//!
//! Lock order: `core` before the event log. Never hold `core` while waiting on an engine
//! thread (engine threads lock `core` to report titles and exits).
use crate::conn::{Out, OutTx};
use crate::eventlog::EventLog;
use crate::state::{State, hex_id};
use crate::term::RtHandle;
use midna_proto::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct Config {
    pub home: PathBuf,
    pub socket: PathBuf,
    /// Executable treated as the human GUI (MIDNA_APP_PATH, honoured in debug builds only;
    /// tests set it directly). None = the code-signing requirement or the name rule (peer.rs).
    pub app_path: Option<String>,
    /// Code-signing requirement for the GUI, derived from this daemon's own signature (signed
    /// builds only). See peer.rs and docs/SECURITY.md.
    pub gui_requirement: Option<String>,
    /// The `midna` CLI binary (hooks call it; its dir is prepended to terminals' PATH).
    pub cli_path: String,
    /// Webhook receiver / secrets / tailscale knobs (MIDNA_SECRETS, MIDNA_WEBHOOKS_PORT, MIDNA_TAILSCALE).
    pub webhooks: crate::webhooks::WebhookConfig,
    /// Dev/test override for the agent executable (MIDNA_AGENT_BIN), e.g. `/bin/echo`.
    pub agent_bin: Option<String>,
    /// This daemon owns its process (the `midnad` binary), so `daemon.upgrade`/`restart` may
    /// execv and `daemon.stop` may exit. False for in-process daemons (tests), where both
    /// would take the host process down.
    pub owns_process: bool,
    /// Claude Code's and Codex's config folders, where `hooks.install` writes midna's global
    /// hooks (`CLAUDE_CONFIG_DIR` / `CODEX_HOME`, else `~/.claude` / `~/.codex`). Tests point
    /// them inside their own home so they never touch the real ones.
    pub claude_dir: PathBuf,
    pub codex_dir: PathBuf,
}

impl Config {
    /// Resolve from the environment (MIDNA_HOME, MIDNA_SOCKET, MIDNA_APP_PATH, MIDNA_CLI_PATH).
    pub fn from_env() -> Config {
        let home = paths::midna_home();
        let socket = paths::socket_path();
        Config {
            home,
            socket,
            app_path: app_path_from_env(),
            gui_requirement: crate::peer::codesign::gui_requirement(),
            cli_path: default_cli_path(),
            webhooks: crate::webhooks::WebhookConfig::from_env(),
            agent_bin: std::env::var("MIDNA_AGENT_BIN").ok().filter(|s| !s.is_empty()),
            owns_process: false,
            claude_dir: agent_dir("CLAUDE_CONFIG_DIR", ".claude"),
            codex_dir: agent_dir("CODEX_HOME", ".codex"),
        }
    }

    pub fn for_home(home: PathBuf) -> Config {
        Config {
            socket: home.join("midnad.sock"),
            app_path: None,
            gui_requirement: None,
            cli_path: default_cli_path(),
            webhooks: crate::webhooks::WebhookConfig::from_env(),
            agent_bin: std::env::var("MIDNA_AGENT_BIN").ok().filter(|s| !s.is_empty()),
            owns_process: false,
            claude_dir: home.join("agent-config/claude"),
            codex_dir: home.join("agent-config/codex"),
            home,
        }
    }
}

fn agent_dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(default))
}

/// `MIDNA_APP_PATH` makes any executable the human GUI. That's a development convenience only:
/// a release daemon ignores it, or an agent could start midnad with it pointed at its own CLI.
fn app_path_from_env() -> Option<String> {
    let v = std::env::var("MIDNA_APP_PATH").ok().filter(|s| !s.is_empty());
    if cfg!(debug_assertions) {
        v
    } else {
        if v.is_some() {
            eprintln!("midnad: ignoring MIDNA_APP_PATH (debug builds only)");
        }
        None
    }
}

/// `MIDNA_CLI_PATH`, else a `midna` next to this executable (or one dir up, for test binaries
/// in target/*/deps), else plain `midna` from PATH.
pub fn default_cli_path() -> String {
    if let Ok(p) = std::env::var("MIDNA_CLI_PATH") {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1).take(2) {
            let c = dir.join("midna");
            if c.is_file() {
                return c.to_string_lossy().into_owned();
            }
        }
    }
    "midna".into()
}

/// Per-session agent bookkeeping (not persisted).
#[derive(Default)]
pub struct AgentRt {
    pub last_cost: f64,
    pub in_turn: bool,
    /// When the title heuristic last ended a turn. Codex's notify for the same turn can land
    /// after the title already stopped spinning; it must not open a second turn.
    pub title_ended_at: Option<Instant>,
    /// When the agent's prompt item was last raised. Claude draws the dialog after the hook,
    /// so the screen check must not dismiss the item before it could have appeared.
    pub prompt_raised_at: Option<Instant>,
    /// A stopped-title re-check is already scheduled.
    pub settle_pending: bool,
    /// A queued restart is running (see `restart::tick`).
    pub restarting: bool,
}

/// What a caller blocked on an approval (`waiters`) is woken with.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    Resolved(Resolution),
    /// The item was taken back before anyone answered (why, for the caller's error).
    Withdrawn(String),
}

pub struct Core {
    pub state: State,
    pub rt: HashMap<Id, RtHandle>,
    pub agents: HashMap<Id, AgentRt>,
}

pub struct Daemon {
    pub cfg: Config,
    pub started: Instant,
    pub started_at: Timestamp,
    core: Mutex<Core>,
    pub log: EventLog,
    dirty: AtomicBool,
    pub shutting_down: AtomicBool,
    /// Human connections that subscribed (receive `window.command`).
    gui: Mutex<Vec<(u64, OutTx)>>,
    /// policy.request callers blocked on a needs-you id.
    pub waiters: Mutex<HashMap<Id, Sender<Answer>>>,
    /// How recently closed needs-you items ended (`needs_you.get`). Not persisted.
    pub answered: Mutex<crate::rpc::needs_you::Answered>,
    /// Folder-trust dialogs being answered (`trust.rs`). Not persisted.
    pub trust: Mutex<crate::trust::Memory>,
    next_conn: AtomicU64,
    pub next_gen: AtomicU64,
    /// Webhook receiver, secrets and delivery-path runtime state.
    pub webhooks: crate::webhooks::Runtime,
    /// Values of the human's stored secrets (`secret.*`); metadata is in `state.secrets`.
    pub vault: crate::webhooks::secrets::Store,
    /// A handoff (upgrade/restart) is in progress: mutating calls are refused meanwhile.
    pub upgrading: AtomicBool,
    /// The listening socket's fd (inherited across a same-PID upgrade).
    pub listener_fd: std::sync::atomic::AtomicI32,
    /// Last seen mtime of commands.json (external edits emit `ui.commands_changed`).
    pub commands_mtime: Mutex<Option<std::time::SystemTime>>,
    /// The app's updater state as the GUI last reported it (`updates.report`).
    pub updates: Mutex<UpdatesStatus>,
    /// Session links (`links.rs`), loaded per terminal on first use.
    pub links: crate::links::Links,
    /// The notifier's book-keeping (`notify.rs`). Lock after `core`, never before it.
    notify: Mutex<crate::notify::State>,
    /// Local triggers' worker and book-keeping (`local.rs`). Lock after `core`, never before it.
    pub local: crate::local::Runtime,
    /// The queued-message sender's book-keeping (`queue.rs`). Never held together with `core`.
    pub queue: crate::queue::Runtime,
    /// Agents watched after a wake (`resume.rs`). Lock after `core`, never before it.
    pub resume: crate::resume::Runtime,
    /// When the Mac slept (`clock.rs`), so idle and expiry checks count awake time only.
    pub clock: crate::clock::Clock,
    /// The keep-awake power assertion (`keep_awake.rs`). Lock after `core`, never before it.
    pub keep_awake: crate::keep_awake::Runtime,
}

impl Daemon {
    pub fn new(cfg: Config) -> std::io::Result<Arc<Daemon>> {
        Daemon::new_keeping(cfg, &HashMap::new())
    }

    /// Like `new`, but the sessions in `kept` (id -> live pid, from an upgrade handoff) keep
    /// their status instead of being marked exited.
    pub fn new_keeping(cfg: Config, kept: &HashMap<Id, Option<i32>>) -> std::io::Result<Arc<Daemon>> {
        std::fs::create_dir_all(&cfg.home)?;
        let _ = std::fs::set_permissions(&cfg.home, std::os::unix::fs::PermissionsExt::from_mode(0o700));
        crate::notify_media::install_twilight(&cfg.home);
        crate::rpc::themes::load(&cfg.home);
        let log = EventLog::open(&paths::events_path(&cfg.home))?;
        let mut state = State::load(&paths::state_path(&cfg.home));
        // PTYs don't survive a daemon restart (yet): mark leftover sessions exited.
        let now = time::now_rfc3339();
        for s in &mut state.sessions {
            if let Some(pid) = kept.get(&s.id) {
                s.pid = *pid;
                continue;
            }
            s.pid = None;
            if !s.status.state.is_terminal() {
                s.status = Status { state: StatusState::Exited, reason: Some("daemon restarted".into()), exit_code: None, since: now.clone() };
            }
        }
        Ok(Arc::new(Daemon {
            started: Instant::now(),
            started_at: now,
            core: Mutex::new(Core { state, rt: HashMap::new(), agents: HashMap::new() }),
            log,
            dirty: AtomicBool::new(true),
            shutting_down: AtomicBool::new(false),
            gui: Mutex::new(vec![]),
            waiters: Mutex::new(HashMap::new()),
            answered: Default::default(),
            trust: Default::default(),
            next_conn: AtomicU64::new(1),
            next_gen: AtomicU64::new(1),
            webhooks: crate::webhooks::Runtime::new(&cfg),
            vault: crate::webhooks::secrets::Store::vault(cfg.webhooks.secrets, &cfg.home),
            upgrading: AtomicBool::new(false),
            listener_fd: std::sync::atomic::AtomicI32::new(-1),
            commands_mtime: Mutex::new(std::fs::metadata(cfg.home.join("commands.json")).and_then(|m| m.modified()).ok()),
            updates: Mutex::new(UpdatesStatus::default()),
            links: Default::default(),
            notify: Mutex::new(Default::default()),
            local: Default::default(),
            queue: Default::default(),
            resume: Default::default(),
            clock: crate::clock::Clock::open(Some(&cfg.home)),
            keep_awake: Default::default(),
            cfg,
        }))
    }

    pub fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn notify(&self) -> MutexGuard<'_, crate::notify::State> {
        self.notify.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn conn_id(&self) -> u64 {
        self.next_conn.fetch_add(1, Ordering::Relaxed)
    }

    /// Mark state as changed; the saver thread writes it shortly.
    pub fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    pub fn save_now(&self) {
        let snapshot = self.core().state.clone();
        if let Err(e) = snapshot.save(&paths::state_path(&self.cfg.home)) {
            eprintln!("midnad: saving state failed: {e}");
        }
    }

    pub fn save_if_dirty(&self) {
        if self.dirty.swap(false, Ordering::AcqRel) {
            self.save_now();
        }
    }

    pub fn emit(&self, kind: &str, actor: Actor, project: Option<Id>, session: Option<Id>, data: Value) -> Event {
        self.log.append(kind, actor, project, session, data)
    }

    pub fn setting(&self, key: &str) -> Value {
        self.core().state.setting(key)
    }

    /// (session id, shell pid) of every live terminal (caller checks, see peer.rs).
    pub fn terminal_pids(&self) -> Vec<(Id, i32)> {
        self.core().rt.iter().filter(|(_, rt)| rt.pid > 1).map(|(id, rt)| (id.clone(), rt.pid)).collect()
    }

    pub fn rt(&self, sid: &str) -> Option<RtHandle> {
        self.core().rt.get(sid).cloned()
    }

    // ---------------------------------------------------------------- GUI fan-out

    pub fn register_gui(&self, conn: u64, tx: OutTx) {
        let mut g = self.gui.lock().unwrap_or_else(|e| e.into_inner());
        if !g.iter().any(|(c, _)| *c == conn) {
            g.push((conn, tx));
        }
    }

    pub fn forget_conn(&self, conn: u64) {
        self.gui.lock().unwrap_or_else(|e| e.into_inner()).retain(|(c, _)| *c != conn);
    }

    /// Send a notification line to every live GUI connection; returns how many got it.
    pub fn gui_send(&self, method: &str, params: Value) -> u32 {
        let line = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
        let mut g = self.gui.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|(_, tx)| tx.send(Out::Line(line.clone())).is_ok());
        g.len() as u32
    }

    pub fn gui_connected(&self) -> bool {
        let mut g = self.gui.lock().unwrap_or_else(|e| e.into_inner());
        // Probe with a no-op: a dead writer drops its receiver.
        g.retain(|(_, tx)| tx.send(Out::Line(String::new())).is_ok());
        !g.is_empty()
    }

    // ---------------------------------------------------------------- status

    /// Change a session's status (no-op when unchanged). Emits `session.status`.
    pub fn set_status(&self, sid: &str, state: StatusState, reason: Option<String>, exit_code: Option<i32>, actor: Actor) {
        let mut core = self.core();
        let clear_failed_on_run = core.state.setting_bool("needs_you.clear_failed_on_run");
        let Some(s) = core.state.session_mut(sid) else { return };
        if s.status.state == state && s.status.reason == reason && s.status.exit_code == exit_code {
            return;
        }
        let from = s.status.state;
        s.status = Status { state, reason: reason.clone(), exit_code, since: time::now_rfc3339() };
        let project = s.project_id.clone();
        // `needs_you.clear_failed_on_run`: working again, its failures are behind it.
        let clear_failed = state == StatusState::Working && from != StatusState::Working && clear_failed_on_run;
        self.mark_dirty();
        self.emit(
            kinds::SESSION_STATUS,
            actor,
            Some(project),
            Some(sid.to_string()),
            json!({ "state": state, "from": from, "reason": reason, "exit_code": exit_code }),
        );
        drop(core);
        if clear_failed {
            self.clear_session_needs_you(sid, NeedsYouKind::Failed);
        }
    }

    // ---------------------------------------------------------------- needs-you

    pub fn raise_needs_you(&self, item: NeedsYou) -> NeedsYou {
        let mut core = self.core();
        core.state.needs_you.push(item.clone());
        self.mark_dirty();
        self.emit(
            kinds::NEEDS_YOU_RAISED,
            item.asked_by.clone(),
            item.project_id.clone(),
            item.session_id.clone(),
            serde_json::to_value(&item).unwrap_or_default(),
        );
        item
    }

    pub fn new_needs_you(&self, kind: NeedsYouKind, title: String, asked_by: Actor, session: Option<Id>) -> NeedsYou {
        let project = session.as_deref().and_then(|s| self.core().state.session(s).map(|s| s.project_id.clone()));
        NeedsYou {
            id: format!("n_{}", hex_id(6)),
            session_id: session,
            project_id: project,
            kind,
            title,
            detail: String::new(),
            screen_excerpt: None,
            asked_by,
            created_at: time::now_rfc3339(),
            bulk_safe: false,
            approval: None,
            trigger_id: None,
            question: None,
        }
    }

    /// Remove a needs-you item and emit `needs_you.resolved`. Returns the item if it existed.
    pub fn close_needs_you(&self, id: &str, resolution: Value, by: Actor) -> Option<NeedsYou> {
        let mut core = self.core();
        let idx = core.state.needs_you.iter().position(|n| n.id == id)?;
        let item = core.state.needs_you.remove(idx);
        let deferred = core.state.deferred.remove(id);
        drop(core);
        if let Some(def) = &deferred {
            crate::rpc::secret::drop_pending(self, def);
        }
        self.answered.lock().unwrap_or_else(|e| e.into_inner()).record(&item.id, &resolution, &by);
        crate::trust::closed(self, &item, &resolution);
        self.mark_dirty();
        self.emit(
            kinds::NEEDS_YOU_RESOLVED,
            by,
            item.project_id.clone(),
            item.session_id.clone(),
            json!({ "id": item.id, "kind": item.kind, "resolution": resolution }),
        );
        Some(item)
    }

    /// Take back an open item nobody answered (its terminal closed, its asker went away): close
    /// it as `{"kind":"withdrawn","reason":…}` and wake a caller blocked on it with the reason.
    pub fn withdraw_needs_you(&self, id: &str, reason: &str) -> Option<NeedsYou> {
        let item = self.close_needs_you(id, json!({ "kind": "withdrawn", "reason": reason }), Actor::system());
        if let Some(tx) = self.waiters.lock().unwrap_or_else(|e| e.into_inner()).remove(id) {
            let _ = tx.send(Answer::Withdrawn(reason.to_string()));
        }
        item
    }

    /// Auto-resolve open items of `kind` for a session (e.g. a permission prompt the agent moved past).
    pub fn clear_session_needs_you(&self, sid: &str, kind: NeedsYouKind) {
        let ids: Vec<Id> = self
            .core()
            .state
            .needs_you
            .iter()
            .filter(|n| n.kind == kind && n.session_id.as_deref() == Some(sid))
            .map(|n| n.id.clone())
            .collect();
        for id in ids {
            self.close_needs_you(&id, json!({ "kind": "done", "auto": true }), Actor::system());
        }
    }

    /// `sid` raised a new blocked or note item (`needs_you.replace`): the open ones it raised
    /// before are stale. A custom status's item keeps its own clear rule.
    pub fn clear_raised(&self, sid: &str) {
        let ids: Vec<Id> = {
            let core = self.core();
            let custom = core.state.session(sid).and_then(|s| s.custom_status.as_ref()).and_then(|c| c.needs_you_id.clone());
            core.state
                .needs_you
                .iter()
                .filter(|n| matches!(n.kind, NeedsYouKind::Blocked | NeedsYouKind::Note) && n.session_id.as_deref() == Some(sid))
                .filter(|n| n.asked_by.session.as_deref() == Some(sid))
                .filter(|n| custom.as_deref() != Some(n.id.as_str()))
                .map(|n| n.id.clone())
                .collect()
        };
        for id in ids {
            self.close_needs_you(&id, json!({ "kind": "done", "auto": true, "reason": "replaced" }), Actor::system());
        }
    }

    /// The agent in `sid` got a new prompt: the `blocked` items it raised itself (`midna
    /// attention`) are stale, since it was answered or moved on. It raises a fresh one if it is
    /// still stuck. A custom status's item keeps its own clear rule.
    pub fn clear_agent_blocked(&self, sid: &str) {
        let ids: Vec<Id> = {
            let core = self.core();
            let custom = core.state.session(sid).and_then(|s| s.custom_status.as_ref()).and_then(|c| c.needs_you_id.clone());
            core.state
                .needs_you
                .iter()
                .filter(|n| n.kind == NeedsYouKind::Blocked && n.session_id.as_deref() == Some(sid))
                .filter(|n| n.asked_by.kind == ActorKind::Agent && n.asked_by.session.as_deref() == Some(sid))
                .filter(|n| custom.as_deref() != Some(n.id.as_str()))
                .map(|n| n.id.clone())
                .collect()
        };
        for id in ids {
            self.close_needs_you(&id, json!({ "kind": "done", "auto": true, "reason": "prompt" }), Actor::system());
        }
    }
}
