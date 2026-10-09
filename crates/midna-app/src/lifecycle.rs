//! App lifecycle: first-launch install, the login item, the CLI link and auto-update, run on
//! one background thread ("midna-lifecycle") and mirrored into the [`Lifecycle`] global that
//! the status bar and Settings render. See `install.rs` / `updater.rs` for the mechanics and
//! docs/RELEASING.md for the release side.
//!
//! Debug / test env:
//! - `MIDNA_UPDATE_FEED_URL` overrides the `updates.feed_url` setting.
//! - `MIDNA_UPDATE_CHECK_SECS` overrides the 6h interval (first check is ~5s after launch).
//! - `MIDNA_DEBUG_UPDATE=apply` installs an update (and relaunches) as soon as it's ready.
//!
//! A busy Mac (midnad answers daemon.upgrade with BUSY) postpones moving the daemon onto the
//! installed build: the first time per launch (or when asked from Settings) a system window
//! recommends waiting (`ui/system_window.rs`); the upgrade is retried quietly every
//! BUSY_RETRY and goes through once the load drops, or at once with Update anyway.
use crate::backend::Backend;
use crate::install::{self, CliLink, LoginItem, Mode};
use crate::updater::{self, FeedEntry};
use gpui_kit::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How often a postponed daemon upgrade is tried again.
const BUSY_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub enum UpdateState {
    Off(String),
    Idle,
    Checking,
    UpToDate { at: Instant },
    Downloading { version: String },
    Ready { version: String, notes: String, app: PathBuf },
    Applying { version: String },
    Failed { error: String, at: Instant },
}

#[derive(Clone, Debug)]
pub enum Cmd {
    CheckNow,
    /// Swap in the staged update and relaunch.
    Apply,
    Register,
    OpenLoginItems,
    /// Settings: link the CLI even if the dir isn't on PATH yet.
    InstallCli,
    /// Settings: move the running daemon onto the installed build (shells survive).
    UpgradeDaemon,
    /// The postponed-upgrade window's Update anyway: upgrade even though the Mac is busy.
    UpgradeDaemonNow,
}

enum Msg {
    Login(LoginItem),
    Cli(CliLink),
    Install(Option<String>),
    Update(UpdateState),
    /// The last daemon upgrade's error (None once it worked).
    DaemonUpgrade(Option<String>),
    /// The daemon upgrade waits for a calmer Mac (None: it isn't waiting any more). `ask`
    /// opens the system window; otherwise an open one just gets the new numbers.
    DaemonPostponed { load: Option<midna_proto::SystemLoad>, ask: bool },
    Quit,
}

/// What ensure_daemon_current did.
enum Upgrade {
    Done,
    /// The Mac is busy; try again later.
    Postponed(midna_proto::SystemLoad),
    Failed,
}

/// What the UI shows. Mirrored in a static so views without an `App` (Settings' row builder)
/// can read it; windows are refreshed whenever it changes.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub mode: Mode,
    pub login: LoginItem,
    pub cli: CliLink,
    pub install_error: Option<String>,
    pub update: UpdateState,
    /// Why moving the daemon onto the installed build failed, if it did.
    pub daemon_upgrade_error: Option<String>,
    /// The daemon upgrade is waiting for the Mac to calm down (its last load reading).
    pub daemon_postponed: Option<midna_proto::SystemLoad>,
}

static SNAP: std::sync::Mutex<Option<Snapshot>> = std::sync::Mutex::new(None);

pub fn snapshot() -> Option<Snapshot> {
    SNAP.lock().ok()?.clone()
}

fn edit(f: impl FnOnce(&mut Snapshot)) {
    if let Ok(mut g) = SNAP.lock()
        && let Some(s) = g.as_mut()
    {
        f(s);
    }
}

/// The command channel to the lifecycle thread.
pub struct Lifecycle {
    tx: Option<mpsc::Sender<Cmd>>,
}

impl Global for Lifecycle {}

/// Run a command from UI code (status bar / Settings).
pub fn command(cmd: Cmd, cx: &mut App) {
    if let Some(tx) = cx.try_global::<Lifecycle>().and_then(|l| l.tx.as_ref()) {
        let _ = tx.send(cmd);
    }
}

/// Start the lifecycle thread. Call once from `main`, before the window opens.
pub fn start(backend: Arc<dyn Backend>, cx: &mut App) {
    let mode = install::mode();
    let fake = std::env::var("MIDNA_BACKEND").as_deref() == Ok("fake");
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    let (msg_tx, msg_rx) = async_channel::unbounded::<Msg>();
    let installed = matches!(mode, Mode::Installed { .. });
    *SNAP.lock().unwrap() = Some(Snapshot {
        mode: mode.clone(),
        login: if installed { LoginItem::Checking } else { LoginItem::Dev },
        cli: CliLink::Dev,
        install_error: None,
        update: if installed { UpdateState::Idle } else { UpdateState::Off("development build".into()) },
        daemon_upgrade_error: None,
        daemon_postponed: None,
    });
    cx.set_global(Lifecycle { tx: Some(cmd_tx) });
    let reporter = backend.clone();
    cx.spawn(async move |cx| {
        while let Ok(msg) = msg_rx.recv().await {
            let quit = matches!(msg, Msg::Quit);
            cx.update(|cx| {
                if quit {
                    log("quitting for the update");
                    cx.quit();
                    return;
                }
                let is_update = matches!(msg, Msg::Update(_));
                if let Msg::DaemonPostponed { load, ask } = &msg {
                    use crate::ui::system_window::{self as sw, Kind};
                    match load {
                        Some(l) if *ask => sw::show(Kind::UpgradePostponed, l.clone(), reporter.clone(), cx),
                        Some(l) => sw::refresh(Kind::UpgradePostponed, l.clone(), cx),
                        None => sw::close(Kind::UpgradePostponed, cx),
                    }
                }
                edit(|l| match msg {
                    Msg::Login(s) => l.login = s,
                    Msg::Cli(s) => l.cli = s,
                    Msg::Install(e) => l.install_error = e,
                    Msg::Update(s) => l.update = s,
                    Msg::DaemonUpgrade(e) => l.daemon_upgrade_error = e,
                    Msg::DaemonPostponed { load, .. } => l.daemon_postponed = load,
                    Msg::Quit => {}
                });
                if is_update && !fake {
                    report_updates(reporter.clone());
                }
                cx.refresh_windows();
            });
        }
    })
    .detach();
    cx.on_app_quit(|cx| {
        apply_on_quit(cx);
        async {}
    })
    .detach();
    if fake {
        return;
    }
    std::thread::Builder::new()
        .name("midna-lifecycle".into())
        .spawn(move || {
            let w = Worker { backend, tx: msg_tx, rx: cmd_rx };
            match mode {
                Mode::Dev => w.dev(),
                Mode::Installed { bundle } => w.installed(&bundle),
            }
        })
        .expect("spawn lifecycle thread");
}

/// The updater state in the daemon's `updates.status` shape (agents read it there).
pub fn updates_status() -> Value {
    let Some(l) = snapshot() else {
        return json!({ "state": "unknown" });
    };
    let ago = |at: &Instant| midna_proto::time::format_unix(midna_proto::time::now_unix() - at.elapsed().as_secs() as i64);
    let mut v = json!({ "state": "idle", "current_version": midna_proto::VERSION });
    match &l.update {
        UpdateState::Off(why) => {
            v["state"] = json!("disabled");
            v["error"] = json!(why);
        }
        UpdateState::Idle => {}
        UpdateState::Checking => v["state"] = json!("checking"),
        UpdateState::UpToDate { at } => {
            v["state"] = json!("up_to_date");
            v["last_checked_at"] = json!(ago(at));
        }
        UpdateState::Downloading { version } => {
            v["state"] = json!("downloading");
            v["available_version"] = json!(version);
        }
        UpdateState::Ready { version, notes, .. } => {
            v["state"] = json!("ready");
            v["available_version"] = json!(version);
            v["notes"] = json!(notes);
        }
        UpdateState::Applying { version } => {
            v["state"] = json!("installing");
            v["available_version"] = json!(version);
        }
        UpdateState::Failed { error, at } => {
            v["state"] = json!("error");
            v["error"] = json!(error);
            v["last_checked_at"] = json!(ago(at));
        }
    }
    v
}

/// Tell midnad the updater state (`updates.report`), off the UI thread. Also called on every
/// (re)connect, since the daemon keeps the report in memory only.
pub fn report_updates(backend: Arc<dyn Backend>) {
    let status = updates_status();
    std::thread::spawn(move || {
        let _ = backend.call("updates.report", json!({ "status": status }));
    });
}

/// `updates.command{action}` from the daemon (an agent's `updates.check`, or an approved
/// `updates.install`). Returns a message for the human when nothing can happen.
pub fn on_daemon_command(action: &str, cx: &mut App) -> Option<String> {
    let l = snapshot()?;
    match (action, &l.update) {
        (_, UpdateState::Off(why)) => Some(format!("Updates are off: {why}")),
        ("check", _) => {
            command(Cmd::CheckNow, cx);
            None
        }
        ("install", UpdateState::Ready { .. }) => {
            command(Cmd::Apply, cx);
            None
        }
        ("install", _) => Some("No update is ready to install yet.".into()),
        _ => None,
    }
}

/// Quitting with an update ready installs it (no relaunch). Called from the Quit paths.
pub fn apply_on_quit(_cx: &mut App) {
    let Some(l) = snapshot() else { return };
    if let (Mode::Installed { bundle }, UpdateState::Ready { app, version, .. }) = (&l.mode, &l.update) {
        match updater::apply(app, bundle) {
            Ok(()) => eprintln!("midna-app: installed {version} on quit"),
            Err(e) => eprintln!("midna-app: update on quit failed: {e}"),
        }
    }
}

struct Worker {
    backend: Arc<dyn Backend>,
    tx: async_channel::Sender<Msg>,
    rx: mpsc::Receiver<Cmd>,
}

pub fn log(msg: &str) {
    eprintln!("midna-app: {msg}");
    let home = midna_proto::paths::midna_home();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(home.join("app.log")) {
        let _ = std::io::Write::write_all(&mut f, format!("{} v{} pid {} {msg}\n", midna_proto::time::now_rfc3339(), midna_proto::VERSION, std::process::id()).as_bytes());
    }
}

impl Worker {
    fn send(&self, m: Msg) {
        let _ = self.tx.send_blocking(m);
    }

    fn dev(self) {
        let socket = crate::backend::daemon::socket_path();
        if std::env::var_os("MIDNA_NO_SPAWN").is_none() && !install::socket_alive(&socket) {
            match install::spawn_dev_daemon() {
                Ok(()) => log("dev: started midnad"),
                Err(e) => log(&format!("dev: midnad not started: {e}")),
            }
        }
        // Commands still work in dev (Settings buttons), minus install/update.
        while let Ok(cmd) = self.rx.recv() {
            match cmd {
                Cmd::OpenLoginItems => install::open_login_items(),
                Cmd::UpgradeDaemon => self.send(Msg::DaemonUpgrade(Some("a development build doesn't upgrade midnad; restart it yourself".into()))),
                _ => {}
            }
        }
    }

    fn installed(self, bundle: &Path) {
        let home = midna_proto::paths::midna_home();
        let _ = std::fs::create_dir_all(&home);
        log(&format!("launch from {} (home {})", bundle.display(), home.display()));
        // 1. the stable daemon binary
        match install::install_daemon(bundle, &home) {
            Ok(_) => self.send(Msg::Install(None)),
            Err(e) => {
                log(&format!("install: {e}"));
                self.send(Msg::Install(Some(e)));
            }
        }
        // 2. the login item
        let mut login = install::register(bundle);
        log(&format!("login item: {login:?}"));
        self.send(Msg::Login(login.clone()));
        // 3. the CLI for agents (a symlink; shell files are never edited)
        let cli = self.link_cli(&home, false);
        self.send(Msg::Cli(cli));
        // 4. the daemon: running, and running the installed build
        let socket = crate::backend::daemon::socket_path();
        let mut daemon_checked = false;
        let mut next_check = Instant::now() + Duration::from_secs(5);
        let interval = crate::dev::var("MIDNA_UPDATE_CHECK_SECS").ok().and_then(|s| s.parse().ok()).map(Duration::from_secs).unwrap_or(updater::CHECK_EVERY);
        let mut update = UpdateState::Idle;
        if updater::PUBKEY.is_none() {
            update = UpdateState::Off("this build has no update key".into());
            self.send(Msg::Update(update.clone()));
        }
        let started = Instant::now();
        let mut kicked = false;
        // A postponed daemon upgrade: when to try again. The window asks once per launch.
        let mut retry_upgrade: Option<Instant> = None;
        let mut asked = false;
        let upgrade = |this: &Self, force: bool, ask: bool, retry: &mut Option<Instant>, asked: &mut bool| {
            *retry = None;
            match this.ensure_daemon_current(&home, force) {
                Upgrade::Postponed(load) => {
                    let ask = ask || !*asked;
                    *asked = true;
                    *retry = Some(Instant::now() + BUSY_RETRY);
                    this.send(Msg::DaemonPostponed { load: Some(load), ask });
                }
                Upgrade::Done | Upgrade::Failed => this.send(Msg::DaemonPostponed { load: None, ask: false }),
            }
        };
        loop {
            if !daemon_checked {
                if login == LoginItem::RequiresApproval || matches!(login, LoginItem::Checking | LoginItem::NotRegistered) {
                    let now = install::login_status(bundle);
                    if now != login {
                        login = now;
                        log(&format!("login item: {login:?}"));
                        self.send(Msg::Login(login.clone()));
                    }
                }
                if install::socket_alive(&socket) {
                    daemon_checked = true;
                    upgrade(&self, false, false, &mut retry_upgrade, &mut asked);
                } else if !kicked && matches!(login, LoginItem::Enabled | LoginItem::Legacy) && started.elapsed() > Duration::from_secs(4) {
                    kicked = true;
                    log("daemon not answering; launchctl kickstart");
                    install::kickstart(bundle);
                }
            }
            if Instant::now() >= next_check && !matches!(update, UpdateState::Off(_) | UpdateState::Ready { .. }) {
                next_check = Instant::now() + interval;
                update = self.check(bundle, &home);
            }
            let until = retry_upgrade.map_or(next_check, |r| r.min(next_check));
            let wait = if daemon_checked { until.saturating_duration_since(Instant::now()).max(Duration::from_millis(50)) } else { Duration::from_millis(500) };
            match self.rx.recv_timeout(wait) {
                Ok(Cmd::CheckNow) => {
                    if !matches!(update, UpdateState::Off(_)) {
                        update = self.check(bundle, &home);
                        next_check = Instant::now() + interval;
                    }
                }
                Ok(Cmd::Apply) => {
                    if let UpdateState::Ready { app, version, .. } = update.clone() {
                        update = self.apply(bundle, &app, &version);
                    }
                }
                Ok(Cmd::Register) => {
                    login = install::register(bundle);
                    self.send(Msg::Login(login.clone()));
                }
                Ok(Cmd::OpenLoginItems) => install::open_login_items(),
                Ok(Cmd::InstallCli) => {
                    let cli = self.link_cli(&home, true);
                    self.send(Msg::Cli(cli));
                }
                Ok(Cmd::UpgradeDaemon) => upgrade(&self, false, true, &mut retry_upgrade, &mut asked),
                Ok(Cmd::UpgradeDaemonNow) => upgrade(&self, true, false, &mut retry_upgrade, &mut asked),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if retry_upgrade.is_some_and(|r| Instant::now() >= r) {
                        upgrade(&self, false, false, &mut retry_upgrade, &mut asked);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if let UpdateState::Ready { app, version, .. } = update.clone()
                && crate::dev::var("MIDNA_DEBUG_UPDATE").as_deref() == Ok("apply")
            {
                update = self.apply(bundle, &app, &version);
            }
        }
    }

    fn link_cli(&self, home: &Path, force: bool) -> CliLink {
        // Already ours and current: no need to ask the login shell for PATH.
        let link = install::cli_link_dir().join("midna");
        if !force && std::fs::read_link(&link).is_ok_and(|t| t == home.join("bin/current/midna")) {
            return CliLink::Linked { link };
        }
        let shell_path = if install::cli_link_dir_pinned() { None } else { install::login_shell_path() };
        let r = install::link_cli(home, shell_path.as_deref(), force);
        log(&format!("cli: {r:?}"));
        r
    }

    /// The running daemon must be `bin/current/midnad`; if not (the app was just updated),
    /// upgrade it in place. Terminals survive (same-PID execv with the PTYs handed over). A busy
    /// Mac postpones it unless `force`.
    fn ensure_daemon_current(&self, home: &Path, force: bool) -> Upgrade {
        let want = install::current_daemon(home);
        let fail = |e: String| {
            log(&e);
            self.send(Msg::DaemonUpgrade(Some(e)));
            Upgrade::Failed
        };
        let Ok(want_canon) = std::fs::canonicalize(&want) else {
            return fail(format!("no installed daemon at {}", want.display()));
        };
        let info = match self.backend.call("daemon.info", json!({})) {
            Ok(v) => v,
            Err(e) => return fail(format!("daemon.info: {e}")),
        };
        let running = info.get("binary").and_then(Value::as_str).and_then(|b| std::fs::canonicalize(b).ok());
        let version = info.get("version").and_then(Value::as_str).unwrap_or("?");
        // Same binary and our version: nothing to do. (Daemons before 0.1.1 reported the
        // unresolved bin/current path, so the version check catches those.)
        if running.as_ref() == Some(&want_canon) && version == midna_proto::VERSION {
            self.send(Msg::DaemonUpgrade(None));
            log(&format!("daemon {version} is current ({})", want_canon.display()));
            return Upgrade::Done;
        }
        log(&format!("daemon {version} runs {:?}; upgrading in place to {}{}", running, want_canon.display(), if force { " (forced)" } else { "" }));
        match self.backend.call("daemon.upgrade", json!({ "binary_path": want, "force": force })) {
            Ok(v) => {
                log(&format!("daemon.upgrade: {v}"));
                self.send(Msg::DaemonUpgrade(None));
                Upgrade::Done
            }
            Err(e) => match e.downcast_ref::<midna_proto::RpcError>().filter(|r| r.code == midna_proto::error::BUSY) {
                Some(r) => {
                    let load = r.data.clone().and_then(|d| serde_json::from_value(d).ok()).unwrap_or_default();
                    log(&format!("daemon.upgrade postponed: {}", r.message));
                    self.send(Msg::DaemonUpgrade(None));
                    Upgrade::Postponed(load)
                }
                None => fail(format!("daemon.upgrade failed: {e}")),
            },
        }
    }

    fn setting(&self, key: &str) -> Option<String> {
        self.backend.call("settings.get", json!({ "key": key })).ok().and_then(|v| v.get("value").and_then(Value::as_str).map(str::to_string))
    }

    fn check(&self, bundle: &Path, home: &Path) -> UpdateState {
        let Some(pubkey) = updater::PUBKEY else {
            return UpdateState::Off("this build has no update key".into());
        };
        self.send(Msg::Update(UpdateState::Checking));
        let channel = self.setting("updates.channel").unwrap_or_else(|| "stable".into());
        let template = crate::dev::var("MIDNA_UPDATE_FEED_URL")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| self.setting("updates.feed_url"))
            .unwrap_or_else(|| midna_proto::settings::DEFAULT_FEED_URL.into());
        let url = updater::feed_url(&template, &channel);
        let r = (|| -> Result<UpdateState, String> {
            let e: FeedEntry = updater::fetch_feed(&url)?;
            if !updater::check_entry(&e, midna_proto::VERSION, pubkey)? {
                return Ok(UpdateState::UpToDate { at: Instant::now() });
            }
            log(&format!("update {} available ({url})", e.version));
            self.send(Msg::Update(UpdateState::Downloading { version: e.version.clone() }));
            let dir = home.join("updates").join(&e.version);
            let archive = updater::download(&e, &dir)?;
            let app = updater::stage(&archive, &dir.join("staged"), &e.version, bundle)?;
            let _ = std::fs::remove_file(&archive);
            log(&format!("update {} verified and staged at {}", e.version, app.display()));
            Ok(UpdateState::Ready { version: e.version, notes: e.notes, app })
        })();
        let state = r.unwrap_or_else(|e| {
            log(&format!("update check: {e}"));
            UpdateState::Failed { error: e, at: Instant::now() }
        });
        self.send(Msg::Update(state.clone()));
        state
    }

    fn apply(&self, bundle: &Path, app: &Path, version: &str) -> UpdateState {
        // Returns only on failure (success ends the process).
        self.send(Msg::Update(UpdateState::Applying { version: version.into() }));
        log(&format!("applying {version}: swapping {}", bundle.display()));
        if let Err(e) = updater::apply(app, bundle) {
            log(&format!("apply failed: {e}"));
            let s = UpdateState::Failed { error: e, at: Instant::now() };
            self.send(Msg::Update(s.clone()));
            return s;
        }
        // The new app upgrades the daemon in place when it starts (ensure_daemon_current).
        match install::relaunch_after_exit(bundle) {
            Ok(()) => log("relaunching"),
            Err(e) => log(&format!("relaunch helper failed: {e}; restart midna by hand")),
        }
        self.send(Msg::Quit);
        // The UI quits on Msg::Quit; if it hasn't within 5s (a hung main thread), exit anyway:
        // the new bundle is in place and the relaunch helper is waiting for this pid.
        std::thread::sleep(Duration::from_secs(5));
        log("still running 5s after quitting for the update; exiting");
        std::process::exit(0);
    }
}

/// Headless subcommands of the bundle executable (no window):
/// - `--login-item-status`: print the SMAppService state.
/// - `--uninstall`: stop midnad (`daemon.stop`, which hangs up every terminal), unregister the
///   login item and remove our `midna` CLI link. MIDNA_HOME's data is left in place.
///
/// Returns the exit code when one of them ran.
pub fn headless(args: &[String]) -> Option<i32> {
    let cmd = args.get(1)?.as_str();
    if !matches!(cmd, "--login-item-status" | "--uninstall") {
        return None;
    }
    let Mode::Installed { bundle } = install::mode() else {
        eprintln!("midna-app {cmd}: only works from inside Midna.app");
        return Some(2);
    };
    if cmd == "--login-item-status" {
        println!("{:?}", install::login_status(&bundle));
        return Some(0);
    }
    let socket = crate::backend::daemon::socket_path();
    if let Ok(mut c) = midna_proto::Client::connect(&socket) {
        c.set_caller(None);
        match c.call_value("daemon.stop", json!({})) {
            Ok(_) => println!("midnad stopping"),
            Err(e) => eprintln!("daemon.stop: {e}"),
        }
        let t0 = Instant::now();
        while install::socket_alive(&socket) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    let code = match install::unregister(&bundle) {
        Ok(()) => {
            println!("login item unregistered");
            0
        }
        Err(e) => {
            eprintln!("unregister: {e}");
            1
        }
    };
    let link = install::cli_link_dir().join("midna");
    if std::fs::read_link(&link).is_ok_and(|t| t.ends_with("bin/current/midna")) && std::fs::remove_file(&link).is_ok() {
        println!("removed {}", link.display());
    }
    Some(code)
}

/// Short status-bar text for an update, if there's anything worth showing.
pub fn status_text(u: &UpdateState) -> Option<String> {
    match u {
        UpdateState::Ready { version, .. } => Some(format!("Update ready ({version}) · restart to apply")),
        UpdateState::Applying { version } => Some(format!("Installing {version}…")),
        _ => None,
    }
}
