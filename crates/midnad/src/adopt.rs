//! A `claude` or `codex` typed into a midna shell terminal, run under midna ("adopted";
//! setting `agents.adopt_typed`). The shell stays: the terminal works like an agent terminal
//! while the agent runs (hooks, status, update prompt, restart into the same conversation)
//! and is a plain shell again when it exits.
//!
//! - **Shims.** `$MIDNA_HOME/shims/{claude,codex}` come first on PATH in shell terminals. Each
//!   runs `midna shim <agent> …`, which asks `session.adopt` and runs what the reply says as
//!   its child; outside midna, or without midna, it is the next one on PATH. A user's own
//!   `claude()` wrapper still runs: its `command claude` reaches the shim.
//! - **Staying first on PATH.** Startup files often rebuild PATH, so zsh (macOS' default)
//!   starts with `ZDOTDIR` pointing at `$MIDNA_HOME/shell/zsh`, whose `.zshenv` puts the
//!   user's `ZDOTDIR` back, runs their `.zshenv` (zsh then reads their other startup files as
//!   usual) and adds a `precmd` hook that moves the shims dir back to the front. Other shells
//!   get the shims dir on PATH and keep it unless their startup files drop it.
//! - **Restart.** midna sets the next command (`AdoptedAgent.next`) and SIGHUPs the agent;
//!   the shim sees it exit, asks `session.adopt_end` and runs the next command in the same
//!   shell (Claude: `--resume <id>`; Codex: `codex resume … <thread-id>`).
//! - **Kind.** While adopted the terminal reports `kind: agent` (with `agent`, `agent_info` and
//!   `adopted`); released, it is `shell` again. `adopted` is what says a shell is underneath.
//! - **`exec claude`** replaces the shell with the shim, which then is the terminal's own
//!   process: adopted the same way. When the agent exits the terminal's process ends, and the
//!   terminal is released with it (`session::on_exit`).
//! - **Not adopted** (the shim runs it exactly as typed): anything but an interactive session
//!   (`agent_cli::Parsed::interactive`: subcommands, `--print`, `exec`, …), no tty, a terminal
//!   that already runs an agent (Claude's own Bash tool calling `claude`), or anything going
//!   wrong on the way.
use crate::daemon::Daemon;
use midna_proto::agent_cli::{Spec, resume_args};
use midna_proto::*;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

pub fn shims_dir(home: &Path) -> PathBuf {
    home.join("shims")
}

fn zsh_dir(home: &Path) -> PathBuf {
    home.join("shell").join("zsh")
}

/// zsh's first startup file in midna shells (`ZDOTDIR` points at its dir).
pub const ZSHENV: &str = include_str!("shell/zshenv.zsh");
/// bash's `--init-file` in midna shells.
pub const BASH_INIT: &str = include_str!("shell/init.bash");
/// fish's `vendor_conf.d` file in midna shells (its data dir is first in `XDG_DATA_DIRS`).
pub const FISH_CONF: &str = include_str!("shell/midna.fish");

/// The agents a shell terminal hands to midna.
pub const AGENTS: [AgentKind; 2] = [AgentKind::Claude, AgentKind::Codex];

pub fn shim_script(cli: &str, agent: &str) -> String {
    include_str!("shell/shim.sh").replace("@CLI@", &crate::hooks::sh_quote(cli)).replace("@AGENT@", agent)
}

fn bash_init(home: &Path) -> PathBuf {
    home.join("shell").join("bash").join("init.bash")
}

fn fish_data(home: &Path) -> PathBuf {
    home.join("shell").join("fish")
}

/// (Re)write the shims and the shells' startup files (daemon start).
pub fn write_files(d: &Daemon) {
    let write = |path: PathBuf, body: &str, mode: u32| {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::read_to_string(&path).ok().as_deref() != Some(body) {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(mode));
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    };
    let home = &d.cfg.home;
    let cli = crate::global_hooks::global_cli(d);
    for a in AGENTS {
        write(shims_dir(home).join(a.as_str()), &shim_script(&cli, a.as_str()), 0o755);
    }
    write(zsh_dir(home).join(".zshenv"), ZSHENV, 0o644);
    write(bash_init(home), BASH_INIT, 0o644);
    write(fish_data(home).join("fish").join("vendor_conf.d").join("midna.fish"), FISH_CONF, 0o644);
}

/// A shell terminal's launch: the shims dir first on PATH, and the shell's startup files
/// routed through midna's (see the module docs). Returns the argv to run instead of the
/// usual one (bash, which only takes an init file when it isn't a login shell). Nothing when
/// `agents.adopt_typed` is off.
pub fn shell_env(d: &Daemon, command: &[String], env: &mut Vec<(String, String)>) -> Option<Vec<String>> {
    if !d.core().state.setting_bool("agents.adopt_typed") {
        return None;
    }
    let home = &d.cfg.home;
    let shims = shims_dir(home).to_string_lossy().into_owned();
    match env.iter_mut().find(|(k, _)| k == "PATH") {
        Some((_, v)) => *v = format!("{shims}:{v}"),
        None => env.push(("PATH".into(), shims.clone())),
    }
    env.push(("MIDNA_SHIMS".into(), shims));
    let argv = crate::rpc::session::exec_argv(command);
    let shell = argv.first().cloned().unwrap_or_default();
    match shell.rsplit('/').next().unwrap_or("") {
        "zsh" => {
            if let Ok(z) = std::env::var("ZDOTDIR") {
                env.push(("MIDNA_ZDOTDIR".into(), z));
            }
            env.push(("ZDOTDIR".into(), zsh_dir(home).to_string_lossy().into_owned()));
            None
        }
        // Only the plain interactive shell midna starts; a command of the user's runs as given.
        "bash" if matches!(argv[1..].iter().map(String::as_str).collect::<Vec<_>>()[..], [] | ["-l"] | ["--login"] | ["-i"]) => {
            if argv.len() > 1 && argv[1] != "-i" {
                env.push(("MIDNA_BASH_LOGIN".into(), "1".into()));
            }
            Some(vec![shell, "--init-file".into(), bash_init(home).to_string_lossy().into_owned(), "-i".into()])
        }
        "fish" => {
            let old = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty());
            if let Some(o) = &old {
                env.push(("MIDNA_XDG_DATA_DIRS".into(), o.clone()));
            }
            // Unset means these two; keeping them keeps fish's own vendor files too.
            let rest = old.unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            env.push(("XDG_DATA_DIRS".into(), format!("{}:{rest}", fish_data(home).display())));
            None
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ the agents' option tables

/// `<agent> --help` read per binary (path, mtime, size): an update changes it.
static SPECS: LazyLock<Mutex<HashMap<(PathBuf, u64, u64), Arc<Spec>>>> = LazyLock::new(Default::default);

fn spec_key(bin: &str) -> Option<(PathBuf, u64, u64)> {
    let path = std::fs::canonicalize(bin).ok()?;
    let m = std::fs::metadata(&path).ok()?;
    let mtime = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    Some((path, mtime, m.len()))
}

fn read_spec(agent: AgentKind, bin: &str) -> Option<Spec> {
    let out = run_with_timeout(bin, &["--help"], Duration::from_secs(10))?;
    Some(Spec::from_help(&out)).filter(|s| s.is_usable(agent))
}

/// The option table of `bin`. `wait: false` (the shim is waiting for its reply) answers from
/// the cache or the built-in table at once and reads the binary's help in the background.
pub fn spec_for(agent: AgentKind, bin: &str, wait: bool) -> Arc<Spec> {
    let Some(key) = spec_key(bin) else { return Arc::new(Spec::builtin(agent)) };
    if let Some(s) = SPECS.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return s.clone();
    }
    let fill = {
        let (bin, key) = (bin.to_string(), key.clone());
        move || {
            let s = Arc::new(read_spec(agent, &bin).unwrap_or_else(|| Spec::builtin(agent)));
            SPECS.lock().unwrap_or_else(|e| e.into_inner()).insert(key, s.clone());
            s
        }
    };
    if wait {
        return fill();
    }
    let _ = std::thread::Builder::new().name("claude-help".into()).spawn(move || {
        fill();
    });
    Arc::new(Spec::builtin(agent))
}

fn run_with_timeout(bin: &str, args: &[&str], max: Duration) -> Option<String> {
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Read on a thread: a long help text would fill the pipe and block the child.
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut s).ok().map(|_| s)
    });
    let t0 = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if t0.elapsed() < max => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    reader.join().ok().flatten()
}

// ------------------------------------------------------------------ adopt / end / restart

fn alive(pid: i32) -> bool {
    pid > 1 && unsafe { libc::kill(pid, 0) } == 0
}

/// The command midna runs for `args` typed to `bin` in `cwd`: midna's flags first (Claude's
/// `--mcp-config` takes every word up to the next option, and `--settings` ends it; Codex's
/// are `-c` overrides, which must come before a subcommand), then the user's, with a typed
/// `--settings` / `--append-system-prompt` merged into midna's (see `agent_args`).
fn command(d: &Daemon, s: &Session, agent: AgentKind, bin: &str, args: &[String], cwd: Option<String>) -> (Vec<String>, BTreeMap<String, String>) {
    let env = crate::rpc::session::session_env(d, &s.id, &s.project_id);
    let mut v = crate::rpc::session::agent_command(d, agent, &env, false);
    v[0] = bin.to_string();
    let cwd = cwd.unwrap_or_else(|| s.cwd.clone());
    let v = crate::agent_args::combine(d, agent, v, args, &spec_for(agent, bin, false), Path::new(&cwd));
    let mut extra = vec![];
    crate::rpc::session::mark_injected(d, &v, &mut extra);
    (v, extra.into_iter().collect())
}

fn sid_of(ctx: &crate::rpc::Ctx, session: &Option<Id>) -> Result<Id, RpcError> {
    session.clone().or(ctx.session.clone()).ok_or_else(|| RpcError::bad_params("no session: run inside a midna terminal or pass session"))
}

pub fn adopt(d: &Arc<Daemon>, ctx: &crate::rpc::Ctx, p: SessionAdoptParams) -> Result<SessionAdoptResult, RpcError> {
    let sid = sid_of(ctx, &p.session)?;
    let none = Ok(SessionAdoptResult::default());
    let s = d.core().state.session(&sid).cloned().ok_or_else(|| RpcError::not_found(format!("no session {sid}")))?;
    // A shell terminal, or one an adopted agent holds (kind `agent` until it is released).
    if (s.kind != SessionKind::Shell && s.adopted.is_none()) || !d.core().state.setting_bool("agents.adopt_typed") {
        return none;
    }
    // An agent already runs here (say, Claude's Bash tool running `claude`): leave this one be.
    if s.agent.is_some() && s.adopted.as_ref().is_none_or(|a| alive(a.pid)) {
        return none;
    }
    // Only a process in this terminal can hand its agent to midna: the shell's child, or the
    // terminal's own process when the shell `exec`ed the agent.
    let Some(rt) = d.rt(&sid) else { return none };
    if !crate::procs::tree(rt.pid).iter().any(|x| x.pid == p.pid) {
        return none;
    }
    if !spec_for(p.agent, &p.bin, false).parse(&p.args).interactive(p.agent) {
        return none;
    }
    let (run, env) = command(d, &s, p.agent, &p.bin, &p.args, crate::procs::cwd(p.pid));
    {
        let mut core = d.core();
        let Some(sess) = core.state.session_mut(&sid) else { return none };
        sess.adopted = Some(AdoptedAgent { agent: p.agent, pid: p.pid, bin: p.bin, args: p.args, since: time::now_rfc3339(), next: None, next_cwd: None });
        sess.kind = SessionKind::Agent;
        sess.agent = Some(p.agent);
        sess.agent_info = Some(AgentInfo::default());
        core.agents.insert(sid.clone(), Default::default());
    }
    d.mark_dirty();
    crate::rpc::session::emit_agent_info(d, &sid);
    Ok(SessionAdoptResult { run: Some(run), env, cwd: None })
}

pub fn adopt_end(d: &Arc<Daemon>, ctx: &crate::rpc::Ctx, p: SessionAdoptEndParams) -> Result<SessionAdoptResult, RpcError> {
    let sid = sid_of(ctx, &p.session)?;
    let s = d.core().state.session(&sid).cloned().ok_or_else(|| RpcError::not_found(format!("no session {sid}")))?;
    let Some(a) = s.adopted.as_ref().filter(|a| a.pid == p.pid) else { return Ok(SessionAdoptResult::default()) };
    if let Some(next) = &a.next {
        let (_, env) = command(d, &s, a.agent, &a.bin, &[], None);
        if let Some(sess) = d.core().state.session_mut(&sid)
            && let Some(a) = sess.adopted.as_mut()
        {
            a.next = None;
            a.next_cwd = None;
        }
        d.mark_dirty();
        return Ok(SessionAdoptResult { run: Some(next.clone()), env, cwd: a.next_cwd.clone() });
    }
    release(d, &sid, &format!("{} exited", a.agent.as_str()));
    Ok(SessionAdoptResult::default())
}

/// The adopted agent is gone: the terminal is a plain shell again.
fn release(d: &Daemon, sid: &str, why: &str) {
    {
        let mut core = d.core();
        let Some(s) = core.state.session_mut(sid) else { return };
        if s.adopted.take().is_none() {
            return;
        }
        s.kind = SessionKind::Shell;
        s.agent = None;
        s.agent_info = None;
        core.agents.remove(sid);
    }
    d.mark_dirty();
    if let Some(rt) = d.rt(sid) {
        rt.with(|e| e.reset_app_modes());
    }
    d.clear_session_needs_you(sid, NeedsYouKind::PermissionPrompt);
    d.set_status(sid, StatusState::Idle, Some(why.into()), None, Actor::system());
}

/// Every couple of seconds: an adopted agent whose shim died without saying so (killed) is
/// released.
pub fn reap(d: &Daemon) {
    let gone: Vec<(Id, AgentKind)> =
        d.core().state.sessions.iter().filter_map(|s| s.adopted.as_ref().filter(|a| !alive(a.pid)).map(|a| (s.id.clone(), a.agent))).collect();
    for (sid, agent) in gone {
        release(d, &sid, &format!("{} exited", agent.as_str()));
    }
}

/// `session.restart` of an adopted agent: the shim relaunches it in the same shell.
pub fn restart(d: &Arc<Daemon>, s: &Session, resume: bool, reason: &str, actor: Actor) -> Result<Session, RpcError> {
    let a = s.adopted.clone().ok_or_else(|| RpcError::conflict("not adopted"))?;
    let info = s.agent_info.clone().unwrap_or_default();
    let args = resume_args(a.agent, &spec_for(a.agent, &a.bin, true), &a.args);
    let (base, _) = command(d, s, a.agent, &a.bin, &args, crate::procs::cwd(a.pid));
    let next = if resume {
        crate::agent_work::resume_command(a.agent, &base, &info).ok_or_else(|| RpcError::conflict(format!("session {} has no conversation to resume", s.id)))?
    } else {
        base
    };
    let mut procs = crate::procs::tree(a.pid);
    crate::procs::tag_tasks(&mut procs, &info.background);
    let Some(child) = procs.iter().find(|p| p.ppid == a.pid).map(|p| p.pid) else {
        return Err(RpcError::conflict(format!("session {}: {} isn't running", s.id, a.agent.as_str())));
    };
    // An agent that moved into a worktree (`codex --worktree`, `claude -w`) comes back there:
    // the resume drops `--worktree`, and Codex would otherwise ask which directory to use.
    // Codex keeps its process in the shell's directory, so its hooks say where it works.
    let shell_cwd = crate::procs::cwd(a.pid);
    let next_cwd = info.cwd.clone().filter(|c| resume && Path::new(c).is_dir()).or_else(|| crate::procs::cwd(child)).filter(|c| shell_cwd.as_ref() != Some(c));
    if let Some(sess) = d.core().state.session_mut(&s.id)
        && let Some(x) = sess.adopted.as_mut()
    {
        x.next = Some(next);
        x.next_cwd = next_cwd;
    }
    // The agent exits on SIGHUP (Claude stops its background shells itself); killed, they'd
    // live on.
    unsafe { libc::kill(child, libc::SIGHUP) };
    crate::rpc::session::wait_gone(child, Duration::from_secs(3));
    if alive(child) {
        unsafe { libc::kill(child, libc::SIGKILL) };
        crate::rpc::session::wait_gone(child, Duration::from_secs(2));
    }
    crate::rpc::session::stop_orphans(&procs);
    crate::rpc::session::after_restart(d, s, &info, resume, reason, actor)
}
