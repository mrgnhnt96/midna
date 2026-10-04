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
}

impl Config {
    /// Resolve from the environment (MIDNA_HOME, MIDNA_SOCKET, MIDNA_APP_PATH, MIDNA_CLI_PATH).
    pub fn from_env() -> Config {
        let home = paths::midna_home();
        let socket = std::env::var_os("MIDNA_SOCKET").map(PathBuf::from).unwrap_or_else(|| home.join("midnad.sock"));
        Config {
            home,
            socket,
            app_path: app_path_from_env(),
            gui_requirement: crate::peer::codesign::gui_requirement(),
            cli_path: default_cli_path(),
            webhooks: crate::webhooks::WebhookConfig::from_env(),
            agent_bin: std::env::var("MIDNA_AGENT_BIN").ok().filter(|s| !s.is_empty()),
            owns_process: false,
        }
    }

    pub fn for_home(home: PathBuf) -> Config {
        Config {
            socket: home.join("midnad.sock"),
            home,
            app_path: None,
            gui_requirement: None,
            cli_path: default_cli_path(),
            webhooks: crate::webhooks::WebhookConfig::from_env(),
            agent_bin: std::env::var("MIDNA_AGENT_BIN").ok().filter(|s| !s.is_empty()),
            owns_process: false,
        }
    }
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
    /// A stopped-title re-check is already scheduled.
    pub settle_pending: bool,
    /// A queued restart is running (see `restart::tick`).
    pub restarting: bool,
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
    pub waiters: Mutex<HashMap<Id, Sender<Resolution>>>,
    next_conn: AtomicU64,
    pub next_gen: AtomicU64,
    /// Webhook receiver, secrets and delivery-path runtime state.
    pub webhooks: crate::webhooks::Runtime,
    /// A handoff (upgrade/restart) is in progress: mutating calls are refused meanwhile.
    pub upgrading: AtomicBool,
    /// The listening socket's fd (inherited across a same-PID upgrade).
    pub listener_fd: std::sync::atomic::AtomicI32,
    /// Last seen mtime of commands.json (external edits emit `ui.commands_changed`).
    pub commands_mtime: Mutex<Option<std::time::SystemTime>>,
    /// The app's updater state as the GUI last reported it (`updates.report`).
    pub updates: Mutex<UpdatesStatus>,
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
            next_conn: AtomicU64::new(1),
            next_gen: AtomicU64::new(1),
            webhooks: crate::webhooks::Runtime::new(&cfg),
            upgrading: AtomicBool::new(false),
            listener_fd: std::sync::atomic::AtomicI32::new(-1),
            commands_mtime: Mutex::new(std::fs::metadata(cfg.home.join("commands.json")).and_then(|m| m.modified()).ok()),
            updates: Mutex::new(UpdatesStatus::default()),
            cfg,
        }))
    }

    pub fn core(&self) -> MutexGuard<'_, Core> {
        self.core.lock().unwrap_or_else(|e| e.into_inner())
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
        let Some(s) = core.state.session_mut(sid) else { return };
        if s.status.state == state && s.status.reason == reason && s.status.exit_code == exit_code {
            return;
        }
        let from = s.status.state;
        s.status = Status { state, reason: reason.clone(), exit_code, since: time::now_rfc3339() };
        let project = s.project_id.clone();
        self.mark_dirty();
        self.emit(
            kinds::SESSION_STATUS,
            actor,
            Some(project),
            Some(sid.to_string()),
            json!({ "state": state, "from": from, "reason": reason, "exit_code": exit_code }),
        );
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
        }
    }

    /// Remove a needs-you item and emit `needs_you.resolved`. Returns the item if it existed.
    pub fn close_needs_you(&self, id: &str, resolution: Value, by: Actor) -> Option<NeedsYou> {
        let mut core = self.core();
        let idx = core.state.needs_you.iter().position(|n| n.id == id)?;
        let item = core.state.needs_you.remove(idx);
        core.state.deferred.remove(id);
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
}
