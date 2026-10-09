//! session.* handlers, plus the engine/reaper callbacks (`on_title`, `on_exit`).
use super::{Ctx, R, ok};
use crate::agent_state::{TitleHint, agent_title_hint, screen_waits_on_human, title_text};
use crate::daemon::Daemon;
use crate::state::hex_id;
use crate::term::{self, EngineMsg, Launch};
use midna_proto::*;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;

fn not_found(id: &str) -> RpcError {
    RpcError::not_found(format!("no session {id}"))
}

/// Session with live fields (pid, last activity) filled from the runtime.
fn live(d: &Daemon, s: &Session) -> Session {
    let mut s = s.clone();
    if let Some(rt) = d.core().rt.get(&s.id) {
        let t = rt.activity.load(Ordering::Relaxed);
        if time::parse_rfc3339(&s.last_activity_at).is_none_or(|old| t > old) {
            s.last_activity_at = time::format_unix(t);
        }
    }
    s
}

pub fn list(d: &Daemon, p: SessionListParams) -> R {
    let project = match p.project_id.as_deref() {
        Some(id) if id == ROOT_PROJECT_ID => Some(ROOT_PROJECT_ID.to_string()),
        Some(id) => Some(super::project::resolve(d, id)?.id),
        None => None,
    };
    let sessions: Vec<Session> = d.core().state.sessions.iter().filter(|s| project.is_none() || project.as_ref() == Some(&s.project_id)).cloned().collect();
    ok(sessions.iter().map(|s| live(d, s)).collect::<Vec<_>>())
}

pub fn get(d: &Daemon, p: IdParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    ok(live(d, &s))
}

fn login_shell() -> String {
    std::env::var("SHELL").ok().filter(|s| s.starts_with('/')).unwrap_or_else(|| "/bin/zsh".into())
}

/// Real argv to exec. Absolute programs run directly; bare names and one-string commands go
/// through the login shell so the user's PATH (nvm, homebrew, …) applies.
pub fn exec_argv(command: &[String]) -> Vec<String> {
    let shell = login_shell();
    match command {
        [one] if one.contains(char::is_whitespace) => vec![shell, "-l".into(), "-c".into(), one.clone()],
        [prog, ..] if prog.starts_with('/') => command.to_vec(),
        _ => {
            let mut v = vec![shell, "-l".into(), "-c".into(), "exec \"$0\" \"$@\"".into()];
            v.extend(command.iter().cloned());
            v
        }
    }
}

/// The logical command for an agent (what Session.command shows). Everything midna adds is
/// per-invocation (flags / `-c` overrides), never the user's global config:
/// - Claude: `--mcp-config <hooks/mcp.json>` (setting agents.mcp; the flag is variadic, so it
///   comes first and `--settings` ends it), `--settings <hooks/claude-settings.json>`,
///   `--append-system-prompt <hint>` (setting agents.system_hint).
/// - Codex: `-c notify=[...]`, `-c mcp_servers.midna.*` (agents.mcp; Codex starts MCP servers
///   with a minimal env, so the session ids go in `env`), `-c developer_instructions=<hint>`.
///
/// `supervised` (agents a webhook trigger starts, setting `triggers.agent_mode`): force the
/// agent's own permission prompts on, whatever the user's global config says, because the
/// prompt carries untrusted webhook text.
pub(crate) fn agent_command(d: &Daemon, agent: AgentKind, env: &[(String, String)], supervised: bool) -> Vec<String> {
    let (mcp, hint) = {
        let core = d.core();
        (core.state.setting_bool("agents.mcp"), core.state.setting_bool("agents.system_hint"))
    };
    let mut v = match agent {
        AgentKind::Claude => {
            let mut v = vec!["claude".to_string()];
            if mcp {
                v.push("--mcp-config".into());
                v.push(crate::hooks::mcp_config_path(&d.cfg.home).to_string_lossy().into_owned());
            }
            // While the global install reports for Claude, its hooks would fire twice.
            let settings = if crate::global_hooks::covers(d, agent) { crate::hooks::claude_base_settings_path(&d.cfg.home) } else { crate::hooks::claude_settings_path(&d.cfg.home) };
            v.push("--settings".into());
            v.push(settings.to_string_lossy().into_owned());
            if supervised {
                v.push("--permission-mode".into());
                v.push("default".into());
            }
            if hint {
                v.push("--append-system-prompt".into());
                v.push(crate::hooks::system_hint(mcp));
            }
            v
        }
        AgentKind::Codex => {
            let mut v = vec!["codex".to_string()];
            // `-c notify` replaces the global one (midna's wrapper and whatever it chains), so
            // only when the global install doesn't cover Codex.
            if !crate::global_hooks::covers(d, agent) {
                v.extend(["-c".into(), crate::hooks::codex_notify_arg(&d.cfg.cli_path)]);
            }
            if supervised {
                v.extend(["-c".into(), "approval_policy=\"on-request\"".into(), "-c".into(), "sandbox_mode=\"workspace-write\"".into()]);
            }
            if mcp {
                v.extend(crate::hooks::codex_mcp_args(&d.cfg.cli_path, env));
            }
            if hint {
                v.push("-c".into());
                v.push(format!("developer_instructions={}", serde_json::to_string(&crate::hooks::system_hint(mcp)).unwrap_or_default()));
            }
            v
        }
    };
    if let Some(bin) = &d.cfg.agent_bin {
        v[0] = bin.clone(); // dev/test override (MIDNA_AGENT_BIN)
    }
    v
}

/// An agent midna starts: `agent_command`, then the caller's own arguments (merged where both
/// set an option, see `agent_args`), then the conversation to reopen and the first prompt.
/// After caller arguments the prompt follows `--`, so an option that takes several words
/// (`--add-dir a b`) can't swallow it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn launch_command(
    d: &Daemon, agent: AgentKind, args: &[String], resume: Option<&str>, prompt: Option<&str>, env: &[(String, String)], supervised: bool, cwd: &str,
) -> Vec<String> {
    let base = agent_command(d, agent, env, supervised);
    let mut v = crate::agent_args::combine(d, agent, base, args, &midna_proto::agent_cli::Spec::builtin(agent), std::path::Path::new(cwd));
    if let Some(id) = resume.filter(|r| !r.is_empty()) {
        match agent {
            AgentKind::Claude => v.extend(["--resume".to_string(), id.to_string()]),
            AgentKind::Codex => {
                v.insert(1.min(v.len()), "resume".into());
                v.push(id.to_string());
            }
        }
    }
    if let Some(p) = prompt.filter(|p| !p.is_empty()) {
        if !args.is_empty() && !args.iter().any(|a| a == "--") {
            v.push("--".into());
        }
        v.push(p.to_string());
    }
    v
}

/// `MIDNA_HOOKS_INJECTED=1` when `command` carries midna's per-launch hooks, so global ones
/// (`midna hook … --global`) stand down instead of reporting a second time.
pub(crate) fn mark_injected(d: &Daemon, command: &[String], env: &mut Vec<(String, String)>) {
    let full = crate::hooks::claude_settings_path(&d.cfg.home).to_string_lossy().into_owned();
    let notify = crate::hooks::codex_notify_arg(&d.cfg.cli_path);
    let chained = |a: &str| a.starts_with("notify=") && a.contains(&format!("\"{}\"", crate::agent_args::CODEX_LAUNCH_WRAPPER));
    if command.iter().any(|a| *a == full || *a == notify || chained(a) || crate::agent_args::is_merged_with_hooks(&d.cfg.home, a)) {
        env.push(("MIDNA_HOOKS_INJECTED".into(), "1".into()));
    }
}

pub(crate) fn session_env(d: &Daemon, sid: &str, project: &str) -> Vec<(String, String)> {
    let cli_dir = std::path::Path::new(&d.cfg.cli_path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    let path = if cli_dir.is_empty() { path } else { format!("{cli_dir}:{path}") };
    vec![
        ("MIDNA_SESSION".into(), sid.into()),
        ("MIDNA_PROJECT".into(), project.into()),
        ("MIDNA_SOCKET".into(), d.cfg.socket.to_string_lossy().into_owned()),
        ("MIDNA_HOME".into(), d.cfg.home.to_string_lossy().into_owned()),
        ("MIDNA_SKILL".into(), crate::hooks::skill_path(&d.cfg.home).to_string_lossy().into_owned()),
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("TERM_PROGRAM".into(), "midna".into()),
        ("TERM_PROGRAM_VERSION".into(), midna_proto::VERSION.into()),
        ("PATH".into(), path),
    ]
}

pub fn open(d: &Arc<Daemon>, ctx: &Ctx, p: SessionOpenParams) -> R {
    ok(open_session(d, ctx, p)?)
}

fn open_session(d: &Arc<Daemon>, ctx: &Ctx, p: SessionOpenParams) -> Result<Session, RpcError> {
    // Resolve the project: explicit, the caller's, the one containing cwd, or a new one for cwd.
    // With none of those the terminal opens at root: no project, starting in $HOME.
    let root = || Project { id: ROOT_PROJECT_ID.into(), name: "root".into(), path: home_dir(), icon: None, order: 0, commands: vec![], last_opened_at: None, auto_created: false, pinned: false };
    let mut p = p;
    p.cwd = p.cwd.take().map(|c| absolute_cwd(d, ctx, &c)).transpose()?;
    let project = match (&p.project_id, &p.cwd) {
        (Some(id), _) if id == ROOT_PROJECT_ID => root(),
        (Some(id), _) => super::project::resolve(d, id)?,
        (None, Some(cwd)) => match super::project::containing(d, cwd) {
            Some(pr) => pr,
            // `/` is never a project (it would cover every path): open there at root.
            None if cwd == "/" => root(),
            None => super::project::find_or_add_for_open(d, ctx.actor(), cwd)?,
        },
        (None, None) => match super::session_project(d, ctx.session.as_deref()) {
            Some(id) if id == ROOT_PROJECT_ID => root(),
            Some(id) => d.core().state.project(&id).cloned().ok_or_else(|| RpcError::not_found("caller project gone"))?,
            None => root(),
        },
    };
    let cwd = p.cwd.clone().unwrap_or_else(|| project.path.clone());
    let sid = hex_id(8);
    let mut env = session_env(d, &sid, &project.id);
    let (command, agent) = match p.kind {
        SessionKind::Agent => {
            let a = p.agent.ok_or_else(|| RpcError::bad_params("kind=agent needs agent: claude|codex"))?;
            let by_trigger = ctx.as_actor.as_ref().is_some_and(|a| a.kind == ActorKind::Trigger);
            let supervised = by_trigger && d.core().state.setting_str("triggers.agent_mode") != "inherit";
            // A supervised agent's permission prompts are forced on; its own arguments could undo that.
            if supervised && !p.agent_args.is_empty() {
                return Err(RpcError::bad_params("agent_args can't be passed to an agent a trigger starts in supervised mode"));
            }
            (launch_command(d, a, &p.agent_args, p.resume.as_deref(), p.prompt.as_deref(), &env, supervised, &cwd), Some(a))
        }
        _ => (p.command.clone().filter(|c| !c.is_empty()).unwrap_or_else(|| vec![login_shell(), "-l".into()]), None),
    };
    let argv = if p.kind == SessionKind::Shell { crate::adopt::shell_env(d, &command, &mut env) } else { None };
    mark_injected(d, &command, &mut env);
    let name = p.name.clone().unwrap_or_else(|| match agent {
        Some(a) => a.as_str().to_string(),
        None => command[0].rsplit('/').next().unwrap_or("shell").to_string(),
    });
    let now = time::now_rfc3339();
    let (cols, rows) = (p.cols.unwrap_or(100).max(2), p.rows.unwrap_or(30).max(2));
    let launch = Launch { sid: sid.clone(), argv: argv.unwrap_or_else(|| exec_argv(&command)), cwd: cwd.clone(), env, cols, rows };
    let rt = term::start(d, launch).map_err(|e| RpcError::bad_params(format!("could not start {}: {e}", command[0])))?;
    let session = Session {
        id: sid.clone(),
        project_id: project.id.clone(),
        name,
        kind: p.kind,
        agent,
        cwd,
        command,
        agent_args: if agent.is_some() { p.agent_args.clone() } else { vec![] },
        pid: Some(rt.pid),
        title: String::new(),
        status: Status { state: StatusState::Idle, reason: Some("started".into()), exit_code: None, since: now.clone() },
        created_at: now.clone(),
        last_activity_at: now,
        keep_on_top: false,
        background: p.background,
        git: None,
        agent_info: None,
        notify: Default::default(),
        custom_status: None,
        queue: vec![],
        queue_paused: false,
        adopted: None,
        close_on_exit: p.close_on_exit,
        auto_name: None,
    };
    {
        let mut core = d.core();
        core.rt.insert(sid.clone(), rt);
        core.state.sessions.push(session.clone());
        if let Some(pr) = core.state.projects.iter_mut().find(|x| x.id == project.id) {
            pr.last_opened_at = Some(session.created_at.clone());
        }
        if agent.is_some() {
            core.agents.insert(sid.clone(), Default::default());
        }
    }
    d.mark_dirty();
    d.emit(kinds::SESSION_OPENED, ctx.actor(), Some(project.id), Some(sid.clone()), serde_json::to_value(&session).unwrap_or_default());
    crate::git::refresh_session_async(d, &sid);
    // Outside git, nothing refreshes: name it from its folder now (`terminal.auto_name` = context).
    crate::auto_name::on_context(d, &sid);
    Ok(session)
}

pub fn close(d: &Arc<Daemon>, ctx: &Ctx, p: SessionCloseParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    let busy = matches!(s.status.state, StatusState::Working | StatusState::NeedsYou);
    if !ctx.is_human() {
        if busy && !p.force {
            return Err(RpcError::conflict(format!("session {} is {}; pass force to close it anyway", s.id, s.status.state.as_str())));
        }
        let own = ctx.session.as_deref() == Some(s.id.as_str());
        let (may_close_idle, may_force_close) = {
            let core = d.core();
            (core.state.setting_bool("agents.may_close_idle"), core.state.setting_bool("agents.may_force_close"))
        };
        let value = if p.force { "close --force" } else { "close" };
        // agents.may_force_close: no default ask at all (`close --force`, a working terminal,
        // someone else's); a rule on `close …` still decides. Otherwise closing someone else's
        // terminal without agents.may_close_idle is treated as an ask.
        let default = if may_force_close { Some(Effect::Allow) } else { (!own && !may_close_idle).then_some(Effect::Ask) };
        super::policy::gate_with(d, ctx, ActionKind::Cli, &format!("{value} {}", s.id), Some(&s), default)?;
    }
    let closing = crate::cleanup::capture(d, &p.id, p.cleanup);
    close_inner(d, ctx, &p.id, p.force, if p.force { "force_close" } else { "close" });
    if let Some(c) = closing {
        crate::cleanup::after_close(d, c);
    }
    ok(OkResult { ok: true })
}

/// `session.replace`: ⌘T then ⌘W in one step. A fresh terminal of the same kind opens in the
/// same project and directory, takes the old one's place in the sidebar, and the old one closes.
/// Unlike a restart it's a new terminal: new id, name, links and prompts, and an agent starts a
/// new conversation without the first prompt it was launched with.
pub fn replace(d: &Arc<Daemon>, ctx: &Ctx, p: SessionReplaceParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    if !ctx.is_human() {
        let busy = matches!(s.status.state, StatusState::Working | StatusState::NeedsYou);
        if busy && !p.force {
            return Err(RpcError::conflict(format!("session {} is {}; pass force to replace it anyway", s.id, s.status.state.as_str())));
        }
        super::policy::gate(d, ctx, ActionKind::Cli, &format!("replace {}", s.id), Some(&s), false)?;
    }
    // An agent typed into a shell (adopted) comes back as the agent, launched by midna.
    let (kind, agent, args) = match &s.adopted {
        Some(a) => (SessionKind::Agent, Some(a.agent), a.args.clone()),
        None => (s.kind, s.agent, s.agent_args.clone()),
    };
    let agent_args = agent.map(|a| midna_proto::agent_cli::resume_args(a, &midna_proto::agent_cli::Spec::builtin(a), &args)).unwrap_or_default();
    let (cols, rows) = d.rt(&s.id).and_then(|rt| rt.read(true)).map(|(_, c, r)| (c, r)).unwrap_or((100, 30));
    let fresh = open_session(
        d,
        ctx,
        SessionOpenParams {
            project_id: Some(s.project_id.clone()),
            kind,
            agent,
            name: None,
            cwd: Some(s.cwd.clone()),
            command: (kind != SessionKind::Agent).then(|| s.command.clone()),
            prompt: None,
            agent_args,
            resume: None,
            cols: Some(cols),
            rows: Some(rows),
            background: s.background,
            close_on_exit: s.close_on_exit,
        },
    )?;
    {
        let mut core = d.core();
        let sessions = &mut core.state.sessions;
        if let Some(new) = sessions.iter().position(|x| x.id == fresh.id) {
            let mut x = sessions.remove(new);
            x.keep_on_top = s.keep_on_top;
            x.notify = s.notify.clone();
            let at = sessions.iter().position(|o| o.id == s.id).unwrap_or(sessions.len());
            sessions.insert(at, x);
        }
    }
    close_inner(d, ctx, &s.id, true, "replace");
    let fresh = d.core().state.session(&fresh.id).cloned().ok_or_else(|| not_found(&fresh.id))?;
    ok(live(d, &fresh))
}

/// Kill, stop the engine, drop the session and its open needs-you items. `how` says why
/// (`session.closed`, and `agent.session_ended` once the dying agent's hook arrives).
pub fn close_inner(d: &Daemon, ctx: &Ctx, sid: &str, force: bool, how: &'static str) {
    let (rt, project) = {
        let mut core = d.core();
        let rt = core.rt.remove(sid);
        core.agents.remove(sid);
        let project = core.state.session(sid).map(|s| s.project_id.clone());
        core.state.sessions.retain(|s| s.id != sid);
        (rt, project)
    };
    // Before the kill: the agent's SessionEnd can arrive as soon as it gets the SIGHUP.
    if let Some(p) = &project {
        d.ends.record(sid, how, ctx.actor(), p.clone(), true);
    }
    if let Some(rt) = rt {
        rt.kill(force);
        let _ = rt.tx.send(EngineMsg::Stop);
    }
    // Its own items go with it. Approvals are withdrawn, waking whoever is blocked on them:
    // the ones it asked for, and other terminals' asks about it (`close --force <sid>`).
    let open: Vec<(Id, bool)> = d
        .core()
        .state
        .needs_you
        .iter()
        .filter_map(|n| {
            let target = n.approval.as_ref().and_then(|a| a.target_session.as_deref()) == Some(sid);
            (target || n.session_id.as_deref() == Some(sid)).then(|| (n.id.clone(), n.kind == NeedsYouKind::Approval))
        })
        .collect();
    for (id, approval) in open {
        if approval {
            d.withdraw_needs_you(&id, &format!("terminal {sid} closed"));
        } else {
            d.close_needs_you(&id, json!({ "kind": "dismiss", "reason": "session closed" }), Actor::system());
        }
    }
    d.links.forget(&d.cfg.home, sid);
    d.mark_dirty();
    d.emit(kinds::SESSION_CLOSED, ctx.actor(), project, Some(sid.to_string()), json!({ "force": force, "how": how }));
}

pub fn rename(d: &Daemon, ctx: &Ctx, p: SessionRenameParams) -> R {
    let mut core = d.core();
    let s = core.state.session_mut(&p.id).ok_or_else(|| not_found(&p.id))?;
    let old = std::mem::replace(&mut s.name, p.name.clone());
    // A name someone chose stays: midna stops naming this terminal by itself.
    s.auto_name = None;
    let out = s.clone();
    d.mark_dirty();
    d.emit(kinds::SESSION_RENAMED, ctx.actor(), Some(out.project_id.clone()), Some(out.id.clone()), json!({ "name": p.name, "old": old }));
    ok(out)
}

pub fn set_background(d: &Daemon, ctx: &Ctx, p: SessionSetBackgroundParams) -> R {
    let mut core = d.core();
    let s = core.state.session_mut(&p.id).ok_or_else(|| not_found(&p.id))?;
    if s.background == p.background {
        return ok(s.clone());
    }
    s.background = p.background;
    let out = s.clone();
    d.mark_dirty();
    d.emit(kinds::SESSION_BACKGROUND, ctx.actor(), Some(out.project_id.clone()), Some(out.id.clone()), json!({ "background": p.background }));
    ok(out)
}

/// Gap between typed text and the Enter that submits it (see `input`).
const ENTER_DELAY: std::time::Duration = std::time::Duration::from_millis(150);
/// Gap after an image paste: the agent reads the file and swaps in `[Image #N]` before the
/// next input arrives (the app's image sheet uses the same beat).
const PASTE_GAP: std::time::Duration = std::time::Duration::from_millis(250);

/// Agents may not answer a permission prompt by typing into the terminal that shows it: that
/// would approve a tool call without the human. They resolve the needs-you item instead,
/// which only the human can do.
fn refuse_prompt_answer(d: &Daemon, ctx: &Ctx, sid: &str, rt: &term::RtHandle) -> Result<(), RpcError> {
    if ctx.is_human() {
        return Ok(());
    }
    let (is_agent, pending) = {
        let core = d.core();
        let is_agent = core.state.session(sid).is_some_and(|s| s.agent.is_some());
        let pending = core.state.needs_you.iter().find(|n| n.session_id.as_deref() == Some(sid) && n.kind == NeedsYouKind::PermissionPrompt).map(|n| n.id.clone());
        (is_agent, pending)
    };
    let on_screen = is_agent && rt.read(true).is_some_and(|(l, _, _)| screen_waits_on_human(&l));
    if pending.is_some() || on_screen {
        let what = pending.map(|n| format!(" (needs-you {n})")).unwrap_or_default();
        return Err(RpcError::human_only(format!(
            "session {sid} is showing a permission prompt{what}; only the human may answer it. Use needs_you.raise if you are blocked on it."
        )));
    }
    Ok(())
}

pub fn input(d: &Daemon, ctx: &Ctx, p: SessionInputParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    refuse_prompt_answer(d, ctx, &p.id, &rt)?;
    // Copy every image before typing anything, so a bad path leaves the terminal untouched.
    let dir = d.cfg.home.join("images");
    let images = p.images.iter().map(|src| crate::images::prepare(&dir, src)).collect::<Result<Vec<_>, _>>().map_err(RpcError::bad_params)?;
    let bytes = p.text.clone().into_bytes();
    let gone = || RpcError::conflict(format!("session {} is not accepting input (process gone?)", p.id));
    // Each image is its own paste, a beat apart, so the agent sees one path per paste.
    for (i, img) in images.iter().enumerate() {
        if i > 0 {
            std::thread::sleep(PASTE_GAP);
        }
        if !rt.client(midna_proto::frame::ClientMsg::Paste(img.display().to_string())) {
            return Err(gone());
        }
    }
    if !images.is_empty() && !bytes.is_empty() {
        std::thread::sleep(PASTE_GAP);
    }
    if !bytes.is_empty() && !rt.write(&bytes) {
        return Err(gone());
    }
    if p.enter {
        // Enter goes separately: TUIs that detect pastes (Claude Code) treat text+CR arriving
        // in one read as a pasted newline, not a submit. It is encoded for the app's keyboard
        // mode (CSI 13 u under the kitty protocol).
        if !bytes.is_empty() || !images.is_empty() {
            std::thread::sleep(if bytes.is_empty() { PASTE_GAP } else { ENTER_DELAY });
        }
        if !rt.key(term::Key::Enter) {
            return Err(gone());
        }
    }
    if !ctx.is_human() {
        let project = d.core().state.session(&p.id).map(|s| s.project_id.clone());
        let preview: String = p.text.chars().take(120).collect();
        let images: Vec<String> = images.iter().map(|i| i.display().to_string()).collect();
        d.emit(kinds::SESSION_INPUT_BY_AGENT, ctx.actor(), project, Some(p.id.clone()), json!({ "text": preview, "enter": p.enter, "bytes": bytes.len(), "images": images }));
    }
    ok(OkResult { ok: true })
}

pub fn read(d: &Daemon, p: SessionReadParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let (mut lines, cols, rows) = rt.read(p.screen).ok_or_else(|| RpcError::internal("engine did not answer"))?;
    if !p.screen {
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let n = p.lines.unwrap_or(50) as usize;
        if lines.len() > n {
            lines.drain(..lines.len() - n);
        }
    }
    ok(SessionReadResult { text: lines.join("\n"), cols, rows })
}

/// `ctrl-shift-up` -> (mods, key). A trailing `-` is the minus key (`ctrl--`).
pub fn parse_keystroke(s: &str) -> Option<(u8, String)> {
    use midna_proto::frame::{MOD_ALT, MOD_CTRL, MOD_SHIFT, MOD_SUPER};
    let s = s.trim();
    let (mods_part, key) = match s.strip_suffix("--") {
        Some(rest) => (rest, "-"),
        None if s == "-" => ("", "-"),
        None => match s.rsplit_once('-') {
            Some((m, k)) => (m, k),
            None => ("", s),
        },
    };
    let mut mods = 0;
    for m in mods_part.split('-').filter(|m| !m.is_empty()) {
        mods |= match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => MOD_CTRL,
            "alt" | "option" | "opt" | "meta" | "m" => MOD_ALT,
            "shift" | "s" => MOD_SHIFT,
            "super" | "cmd" | "command" => MOD_SUPER,
            _ => return None,
        };
    }
    let key = match key.to_ascii_lowercase().as_str() {
        "esc" => "escape".to_string(),
        "return" | "cr" => "enter".to_string(),
        "bs" => "backspace".to_string(),
        "del" => "delete".to_string(),
        "pgup" => "pageup".to_string(),
        "pgdn" => "pagedown".to_string(),
        k if k.chars().count() == 1 => key.to_string(),
        k => k.to_string(),
    };
    crate::engine::key_from_name(&key)?;
    Some((mods, key))
}

pub fn key(d: &Daemon, ctx: &Ctx, p: SessionKeyParams) -> R {
    use midna_proto::frame::{ClientMsg, KeyAction, KeyMsg};
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    refuse_prompt_answer(d, ctx, &p.id, &rt)?;
    let (mods, key) = parse_keystroke(&p.key).ok_or_else(|| RpcError::bad_params(format!("unknown key {:?}", p.key)))?;
    let action = match p.action.as_deref() {
        None | Some("press") => KeyAction::Press,
        Some("repeat") => KeyAction::Repeat,
        Some("release") => KeyAction::Release,
        Some(a) => return Err(RpcError::bad_params(format!("action must be press, repeat or release, not {a}"))),
    };
    if !rt.client(ClientMsg::Key(KeyMsg { action, mods, key, text: String::new() })) {
        return Err(RpcError::conflict(format!("session {} is not accepting input", p.id)));
    }
    if !ctx.is_human() {
        let project = d.core().state.session(&p.id).map(|s| s.project_id.clone());
        d.emit(kinds::SESSION_INPUT_BY_AGENT, ctx.actor(), project, Some(p.id.clone()), json!({ "key": p.key }));
    }
    ok(OkResult { ok: true })
}

/// `session.clear`: drop the scrollback and ask the app to redraw (ctrl-l), like a terminal's
/// "Clear" menu item. On the alternate screen (vim, less, a TUI) only ctrl-l is sent.
pub fn clear(d: &Daemon, ctx: &Ctx, p: IdParams) -> R {
    use midna_proto::frame::{ClientMsg, KeyAction, KeyMsg};
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let cleared = rt.with(|e| e.clear_scrollback()).unwrap_or(false);
    rt.client(ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods: midna_proto::frame::MOD_CTRL, key: "l".into(), text: String::new() }));
    if !ctx.is_human() {
        let project = d.core().state.session(&p.id).map(|s| s.project_id.clone());
        d.emit(kinds::SESSION_INPUT_BY_AGENT, ctx.actor(), project, Some(p.id.clone()), json!({ "key": "ctrl-l", "clear": true }));
    }
    ok(json!({ "ok": true, "scrollback_cleared": cleared }))
}

pub fn scroll(d: &Daemon, p: SessionScrollParams) -> R {
    use midna_proto::frame::{ClientMsg, ScrollKind, ScrollMsg};
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let (kind, amount) = match (p.to.as_deref(), p.lines, p.pages) {
        (Some("top"), _, _) => (ScrollKind::Top, 0),
        (Some("bottom"), _, _) => (ScrollKind::Bottom, 0),
        (Some(t), _, _) => return Err(RpcError::bad_params(format!("to must be top or bottom, not {t}"))),
        (None, Some(n), _) => (ScrollKind::Lines, n),
        (None, None, Some(n)) => (ScrollKind::Pages, n),
        _ => return Err(RpcError::bad_params("give to, lines or pages")),
    };
    rt.client(ClientMsg::Scroll(ScrollMsg { kind, amount, x: 0.0, y: 0.0, mods: 0 }));
    ok(OkResult { ok: true })
}

pub fn selection(d: &Daemon, p: IdParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let text = rt.with(|e| e.selection_text()).ok_or_else(|| RpcError::internal("engine did not answer"))?;
    ok(SessionSelectionResult { text })
}

pub fn select_all(d: &Daemon, p: IdParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    rt.with(|e| e.select_all()).ok_or_else(|| RpcError::internal("engine did not answer"))?;
    ok(OkResult { ok: true })
}

pub fn link_at(d: &Daemon, p: SessionLinkAtParams) -> R {
    use crate::engine::Link;
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let (cwd, tabs, patterns) = {
        let core = d.core();
        let session = core.state.session(&p.id);
        let cwd = session.map(|s| s.cwd.clone()).unwrap_or_default();
        // The link patterns that apply here: by project (name, id or folder) and terminal kind.
        let project = session.and_then(|s| core.state.projects.iter().find(|p| p.id == s.project_id));
        let names: Vec<&str> = project.map(|p| vec![p.name.as_str(), p.id.as_str(), p.path.as_str()]).unwrap_or_default();
        let agent = session.is_some_and(|s| s.kind == SessionKind::Agent);
        let patterns: Vec<_> = link_patterns(&core.state.setting("terminal.link_patterns")).iter().filter(|r| r.applies(agent, &names)).cloned().collect();
        // Terminals a word can name: by id, and agents by their conversation id.
        let mut tabs = Vec::new();
        for s in &core.state.sessions {
            tabs.push((s.id.clone(), s.id.clone()));
            if let Some(c) = s.agent_info.as_ref().and_then(|i| i.conversation_id.clone()) {
                tabs.push((c, s.id.clone()));
            }
        }
        (cwd, tabs, patterns)
    };
    let (col, row) = (p.col, p.row);
    let link = rt.with(move |e| e.link_at(col, row, &cwd, &tabs, &patterns)).ok_or_else(|| RpcError::internal("engine did not answer"))?;
    ok(match link {
        Some(Link::Url(u)) => LinkAtResult { kind: "url".into(), target: Some(u), ..Default::default() },
        Some(Link::App { url, app }) => LinkAtResult { kind: "url".into(), target: Some(url), app: Some(app), ..Default::default() },
        Some(Link::File { path, line, column }) => LinkAtResult { kind: "file".into(), target: Some(path), line, column, app: None },
        Some(Link::Session(id)) => LinkAtResult { kind: "session".into(), target: Some(id), ..Default::default() },
        None => LinkAtResult { kind: "none".into(), ..Default::default() },
    })
}

/// `terminal.link_patterns`, compiled once per change of the setting.
fn link_patterns(v: &serde_json::Value) -> Arc<Vec<midna_proto::link_patterns::LinkPattern>> {
    use std::sync::{LazyLock, Mutex};
    type Cache = Option<(serde_json::Value, Arc<Vec<midna_proto::link_patterns::LinkPattern>>)>;
    static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(Default::default);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, pats)) = c.as_ref().filter(|(k, _)| k == v) {
        return pats.clone();
    }
    let rules: Vec<String> = v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let pats = Arc::new(midna_proto::link_patterns::parse_all(&rules));
    *c = Some((v.clone(), pats.clone()));
    pats
}

pub fn find(d: &Daemon, p: SessionFindParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let (q, back) = (p.query.clone(), p.backwards);
    let h = rt.with(move |e| e.find(&q, back)).ok_or_else(|| RpcError::internal("engine did not answer"))?;
    ok(FindResult { total: h.total, index: h.index })
}

pub fn prompts(d: &Daemon, p: IdParams) -> R {
    ok(crate::prompts::locate(d, &p.id).ok_or_else(|| not_found(&p.id))?)
}

/// `session.jump_prompt`: scrolls on a thread of its own (it presses keys and waits for the
/// screen, a second or two), so a client that doesn't wait isn't held up.
pub fn jump_prompt(d: &Arc<Daemon>, ctx: &Ctx, p: SessionJumpPromptParams) -> R {
    use crate::prompts::{To, jump};
    d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    let to = To::parse(p.n, p.to.as_deref()).map_err(RpcError::bad_params)?;
    if !ctx.is_human() {
        let project = d.core().state.session(&p.id).map(|s| s.project_id.clone());
        d.emit(kinds::SESSION_INPUT_BY_AGENT, ctx.actor(), project, Some(p.id.clone()), json!({ "jump_prompt": p.n, "to": p.to }));
    }
    let (d2, id) = (d.clone(), p.id.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("jump-prompt".into())
        .spawn(move || {
            let _ = tx.send(jump(&d2, &id, to));
        })
        .map_err(|e| RpcError::internal(e.to_string()))?;
    if p.wait == Some(false) {
        return ok(JumpPromptResult { ok: true, n: p.n, found: false, reason: None });
    }
    ok(rx.recv().map_err(|_| RpcError::internal("the jump stopped"))?)
}

pub fn resize(d: &Daemon, p: SessionResizeParams) -> R {
    let rt = d.rt(&p.id).ok_or_else(|| not_found(&p.id))?;
    if p.cols == 0 || p.rows == 0 {
        return Err(RpcError::bad_params("cols and rows must be > 0"));
    }
    rt.resize(p.cols, p.rows, p.cell_w.unwrap_or(0), p.cell_h.unwrap_or(0));
    ok(OkResult { ok: true })
}

/// `session.restart`: now (refused while the agent has background work in flight, unless
/// `force`) or queued until idle (see `crate::restart`). Agent terminals resume their
/// conversation unless `resume: false`.
pub fn restart(d: &Arc<Daemon>, ctx: &Ctx, p: SessionRestartParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    if !ctx.is_human() {
        super::policy::gate(d, ctx, ActionKind::Cli, &format!("restart {}", s.id), Some(&s), false)?;
    }
    if d.core().agents.get(&s.id).is_some_and(|a| a.restarting) {
        return Err(RpcError::conflict(format!("session {} is restarting right now (a queued restart)", s.id)));
    }
    let info = s.agent_info.clone().unwrap_or_default();
    let can_resume = s.agent.is_some() && info.conversation_id.is_some();
    if p.resume == Some(true) && !can_resume {
        return Err(RpcError::conflict(format!(
            "session {} has no conversation to resume yet (its agent has not reported one); restart with resume=false for a fresh start",
            s.id
        )));
    }
    let resume = p.resume.unwrap_or(can_resume);
    match p.when.as_deref().unwrap_or("now") {
        "idle" => {
            if s.agent.is_none() {
                return Err(RpcError::bad_params("when=idle needs an agent terminal (only agents report what they have in flight)"));
            }
            let reason = p.reason.clone().unwrap_or_else(|| "requested".into());
            crate::restart::queue(d, &s.id, &reason, ctx.actor());
            get(d, IdParams { id: s.id })
        }
        "now" => {
            let in_flight = if s.pid.is_some() { info.in_flight() } else { vec![] };
            if !in_flight.is_empty() && !p.force {
                return Err(RpcError::conflict(format!(
                    "restarting {} now would stop: {}. Queue it with when=idle, or pass force",
                    s.id,
                    in_flight.join("; ")
                )));
            }
            let reason = p.reason.clone().unwrap_or_else(|| "requested".into());
            ok(restart_now(d, &s.id, resume, &reason, ctx.actor())?)
        }
        w => Err(RpcError::bad_params(format!("when must be now or idle, not {w}"))),
    }
}

pub fn restart_cancel(d: &Daemon, ctx: &Ctx, p: IdParams) -> R {
    if d.core().state.session(&p.id).is_none() {
        return Err(not_found(&p.id));
    }
    crate::restart::cancel(d, &p.id, ctx.actor());
    get(d, p)
}

pub fn update_decline(d: &Daemon, p: IdParams) -> R {
    if d.core().state.session(&p.id).is_none() {
        return Err(not_found(&p.id));
    }
    crate::restart::decline_update(d, &p.id);
    get(d, p)
}

pub fn processes(d: &Daemon, p: IdParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    let Some(pid) = d.rt(&p.id).map(|rt| rt.pid).filter(|&p| p > 1) else { return ok(Vec::<ProcessInfo>::new()) };
    let mut procs = crate::procs::tree(pid);
    if let Some(info) = &s.agent_info {
        crate::procs::tag_tasks(&mut procs, &info.background);
    }
    ok(procs)
}

pub fn subagent_log(d: &Daemon, p: SubagentLogParams) -> R {
    let s = d.core().state.session(&p.id).cloned().ok_or_else(|| not_found(&p.id))?;
    let info = s.agent_info.unwrap_or_default();
    let running = info.subagents.iter().find(|a| a.id == p.agent);
    let agent = running.or_else(|| info.finished_subagents.iter().rev().find(|a| a.id == p.agent)).cloned();
    // A background agent between wakes is only in the Stop snapshot.
    let waiting = info.background.iter().any(|t| t.id == p.agent && t.kind == "subagent");
    let transcript = info.transcript_path.as_deref().ok_or_else(|| RpcError::conflict(format!("session {} has no agent transcript", p.id)))?;
    let path = crate::subagent_log::path(transcript, &p.agent).ok_or_else(|| RpcError::bad_params(format!("not a subagent id: {}", p.agent)))?;
    let (entries, next, model) = crate::subagent_log::read(&path, p.from);
    ok(SubagentLog { agent, running: running.is_some() || waiting, entries, next, model })
}

/// Supervised agents (webhook triggers) were launched with permission prompts forced on; a
/// rebuilt command keeps that.
fn was_supervised(command: &[String]) -> bool {
    command.windows(2).any(|w| w[0] == "--permission-mode" && w[1] == "default") || command.iter().any(|a| a == "approval_policy=\"on-request\"")
}

/// Replace the terminal's process in place: same id, same tab, same size. With `resume`, the
/// old process is stopped first (two agents must never write one conversation) and the agent
/// reopens its conversation with the model and permission mode it had; the launch command is
/// rebuilt without its initial prompt, so current midna settings (hooks, MCP) apply.
pub fn restart_now(d: &Arc<Daemon>, sid: &str, resume: bool, reason: &str, actor: Actor) -> Result<Session, RpcError> {
    let s = d.core().state.session(sid).cloned().ok_or_else(|| not_found(sid))?;
    // The old agent's SessionEnd arrives after the restart: it was this, not the agent.
    d.ends.record(sid, "restart", actor.clone(), s.project_id.clone(), false);
    // A conversation nothing was said in yet isn't saved: start fresh instead.
    let resume = resume && s.agent_info.as_ref().is_none_or(crate::agent_work::has_transcript);
    // A `claude` typed into this shell: relaunch it inside the shell, which keeps running.
    if s.adopted.is_some() {
        return crate::adopt::restart(d, &s, resume, reason, actor);
    }
    let info = s.agent_info.clone().unwrap_or_default();
    let (cols, rows) = d.rt(sid).and_then(|rt| rt.read(true)).map(|(_, c, r)| (c, r)).unwrap_or((100, 30));
    let mut env = session_env(d, &s.id, &s.project_id);
    let command = match s.agent {
        Some(agent) if resume => {
            // The caller's own arguments stay, minus the first prompt and conversation options.
            let args = midna_proto::agent_cli::resume_args(agent, &midna_proto::agent_cli::Spec::builtin(agent), &s.agent_args);
            let base = launch_command(d, agent, &args, None, None, &env, was_supervised(&s.command), &s.cwd);
            crate::agent_work::resume_command(agent, &base, &info).ok_or_else(|| RpcError::conflict(format!("session {sid} has no conversation to resume")))?
        }
        _ => s.command.clone(),
    };
    let argv = if s.kind == SessionKind::Shell { crate::adopt::shell_env(d, &command, &mut env) } else { None };
    mark_injected(d, &command, &mut env);
    // Take the old runtime out first, so its exit is ignored (no runtime for this session).
    let old = d.core().rt.remove(sid);
    if let Some(old) = old {
        // The agent's background processes, as they are now: Claude stops them when it exits
        // on SIGHUP, but if it has to be killed they would live on, orphaned.
        let mut procs = crate::procs::tree(old.pid);
        crate::procs::tag_tasks(&mut procs, &info.background);
        old.kill(false);
        if resume || procs.iter().any(|p| p.task_id.is_some()) {
            wait_gone(old.pid, std::time::Duration::from_secs(5));
        }
        let _ = old.tx.send(EngineMsg::Stop);
        stop_orphans(&procs);
    }
    let launch = Launch { sid: s.id.clone(), argv: argv.unwrap_or_else(|| exec_argv(&command)), cwd: s.cwd.clone(), env, cols, rows };
    let new_rt = term::start(d, launch).map_err(|e| {
        d.set_status(sid, StatusState::Failed, Some(format!("restart failed: {e}")), None, Actor::system());
        RpcError::bad_params(format!("could not restart: {e}"))
    })?;
    {
        let mut core = d.core();
        let pid = new_rt.pid;
        core.rt.insert(s.id.clone(), new_rt);
        if let Some(sess) = core.state.session_mut(&s.id) {
            sess.pid = Some(pid);
        }
    }
    after_restart(d, &s, &info, resume, reason, actor)
}

/// The agent bookkeeping of a restart, once the new process runs: what was in flight went with
/// the old one, the cost keeps counting from the conversation's total, and `session.restarted`.
pub(crate) fn after_restart(d: &Daemon, s: &Session, info: &AgentInfo, resume: bool, reason: &str, actor: Actor) -> Result<Session, RpcError> {
    {
        let mut core = d.core();
        if let Some(sess) = core.state.session_mut(&s.id) {
            sess.title.clear();
            if let Some(i) = sess.agent_info.as_mut() {
                i.background.clear();
                i.subagents.clear();
                if !resume {
                    i.crons.clear();
                }
                i.restart = None;
                i.update_available = None;
                i.update_declined = None;
            }
        }
        if s.agent.is_some() {
            // A resumed Claude reports the conversation's running total cost: keep counting
            // from where it was, or the whole spend is counted again.
            let last_cost = if resume { core.agents.get(&s.id).map(|a| a.last_cost).unwrap_or(0.0) } else { 0.0 };
            core.agents.insert(s.id.clone(), crate::daemon::AgentRt { last_cost, ..Default::default() });
        }
    }
    d.mark_dirty();
    emit_agent_info(d, &s.id);
    d.clear_session_needs_you(&s.id, NeedsYouKind::Failed);
    let resumed = resume && s.agent.is_some();
    d.emit(
        kinds::SESSION_RESTARTED,
        actor.clone(),
        Some(s.project_id.clone()),
        Some(s.id.clone()),
        json!({ "resume": resumed, "conversation_id": info.conversation_id, "reason": reason, "from_version": info.version }),
    );
    let status = if resumed { "restarted (resumed)" } else { "restarted" };
    d.set_status(&s.id, StatusState::Idle, Some(status.into()), None, actor);
    let s = d.core().state.session(&s.id).cloned().ok_or_else(|| not_found(&s.id))?;
    Ok(live(d, &s))
}

/// SIGHUP what is left of an exited agent's background tasks (re-parented to launchd). Only
/// processes tagged with a background task: anything else the agent left running on purpose
/// (a daemon it started) is not ours to stop.
pub(crate) fn stop_orphans(procs: &[ProcessInfo]) {
    for p in procs.iter().filter(|p| p.task_id.is_some() && p.depth > 0) {
        if crate::procs::parent(p.pid) == Some(1) {
            unsafe {
                if p.pgid == p.pid {
                    libc::kill(-p.pid, libc::SIGHUP);
                }
                libc::kill(p.pid, libc::SIGHUP);
            }
        }
    }
}

/// `session.agent` with the session's current AgentInfo (after the daemon itself changed it).
pub(crate) fn emit_agent_info(d: &Daemon, sid: &str) {
    let Some((project, info)) = d.core().state.session(sid).and_then(|s| Some((s.project_id.clone(), s.agent_info.clone()?))) else { return };
    d.emit(kinds::SESSION_AGENT, Actor::system(), Some(project), Some(sid.into()), serde_json::to_value(&info).unwrap_or_default());
}

/// Wait for a process we signalled to be reaped (the reaper thread waitpids it).
pub(crate) fn wait_gone(pid: i32, max: std::time::Duration) {
    let t0 = std::time::Instant::now();
    while t0.elapsed() < max && unsafe { libc::kill(pid, 0) } == 0 {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

// ------------------------------------------------------------------ runtime callbacks

/// The process exited. Ignored if the session was closed or restarted since.
pub fn on_exit(d: &Arc<Daemon>, sid: &str, generation: u64, code: Option<i32>, signal: Option<i32>) {
    let (kind, project, close_on_exit) = {
        let mut core = d.core();
        if core.rt.get(sid).map(|r| r.generation) != Some(generation) {
            return;
        }
        let Some(s) = core.state.session_mut(sid) else { return };
        s.pid = None;
        let close_on_exit = s.close_on_exit;
        // An adopted agent that was the terminal's own process (`exec claude`) went with it:
        // released here, or the reaper would mark the exited terminal idle later.
        let adopted = s.adopted.take().is_some();
        if adopted {
            s.kind = SessionKind::Shell;
            s.agent = None;
            s.agent_info = None;
        }
        if let Some(i) = s.agent_info.as_mut() {
            // Its background work went with it (crons come back with a resume); an exit is
            // not a cue to restart.
            i.background.clear();
            i.subagents.clear();
            i.restart = None;
        }
        let out = (s.kind, s.project_id.clone(), close_on_exit);
        if adopted {
            core.agents.remove(sid);
        }
        out
    };
    emit_agent_info(d, sid);
    let (state, reason) = match (code, signal) {
        (Some(0), _) => (StatusState::Exited, "exited".to_string()),
        // Quitting with ctrl-c is a normal way out of a TUI: Codex 0.160.0 exits by re-raising
        // SIGINT after the second ctrl-c, and a shell reports that as 130. SIGHUP is what
        // `session.close` sends. Neither is a failure worth a needs-you item.
        (Some(130), _) | (None, Some(libc::SIGINT)) => (StatusState::Exited, "exited (interrupted)".to_string()),
        (None, Some(libc::SIGHUP)) => (StatusState::Exited, "exited (hung up)".to_string()),
        (Some(c), _) => (StatusState::Failed, format!("exited with code {c}")),
        (None, Some(sig)) => (StatusState::Failed, format!("killed by signal {sig}")),
        _ => (StatusState::Exited, "exited".to_string()),
    };
    d.emit(kinds::SESSION_EXITED, Actor::system(), Some(project), Some(sid.to_string()), json!({ "exit_code": code, "signal": signal, "state": state, "reason": reason }));
    crate::rpc::agent::end_turn_if_open(d, sid, "process exited");
    d.set_status(sid, state, Some(reason.clone()), code, Actor::system());
    d.clear_session_needs_you(sid, NeedsYouKind::PermissionPrompt);
    if close_on_exit && state == StatusState::Exited {
        let closing = crate::cleanup::capture(d, sid, None);
        close_inner(d, &Ctx::internal_system(), sid, false, "exited");
        if let Some(c) = closing {
            crate::cleanup::after_close(d, c);
        }
        return;
    }
    if state == StatusState::Failed && kind != SessionKind::Shell {
        let mut item = d.new_needs_you(NeedsYouKind::Failed, format!("Terminal failed: {reason}"), Actor::system(), Some(sid.to_string()));
        item.bulk_safe = true;
        item.screen_excerpt = d.rt(sid).and_then(|rt| rt.read(true)).map(|(l, _, _)| tail_nonempty(l, 8));
        d.raise_needs_you(item);
    }
    // Hung up: the terminal (or midnad) is going away, so no shell to drop into.
    let hung_up = signal == Some(libc::SIGHUP) || d.shutting_down.load(Ordering::Relaxed);
    if kind == SessionKind::Agent && !close_on_exit && !hung_up && d.core().state.setting_bool("agents.shell_on_exit") {
        if let Err(e) = swap_to_shell(d, sid, generation, &reason) {
            eprintln!("midnad: session {sid}: no shell after the agent exited: {e}");
        }
    }
}

/// An agent midna started exited (`agents.shell_on_exit`): the terminal becomes a login shell
/// in the same tab, on top of the agent's last screen and scrollback, the way a `claude` typed
/// into a shell ends (`adopt::release`). The agent ran as the terminal's own process (`exec`),
/// so the shell needs a new PTY.
fn swap_to_shell(d: &Arc<Daemon>, sid: &str, generation: u64, reason: &str) -> Result<(), String> {
    let old = {
        let mut core = d.core();
        if core.rt.get(sid).map(|r| r.generation) != Some(generation) {
            return Ok(()); // closed or restarted meanwhile
        }
        core.rt.remove(sid)
    };
    let Some(old) = old else { return Ok(()) };
    let (tx, rx) = std::sync::mpsc::channel();
    let snap = old.tx.send(EngineMsg::Snapshot(tx)).ok().and_then(|_| rx.recv_timeout(std::time::Duration::from_secs(5)).ok());
    let _ = old.tx.send(EngineMsg::Stop);
    let Some(s) = d.core().state.session(sid).cloned() else { return Ok(()) };
    // Only the primary screen carries over: the shell starts out of any alt screen, with its
    // own title and nothing half-parsed.
    let snap = snap.map(|mut t| {
        t.alt_active = false;
        t.alt_vt = None;
        t.alt_cursor = None;
        t.title.clear();
        t.pending.clear();
        t
    });
    let (cols, rows) = snap.as_ref().map_or((100, 30), |t| (t.cols, t.rows));
    let command = vec![login_shell(), "-l".into()];
    let mut env = session_env(d, sid, &s.project_id);
    let argv = crate::adopt::shell_env(d, &command, &mut env);
    let launch = Launch { sid: sid.to_string(), argv: argv.unwrap_or_else(|| exec_argv(&command)), cwd: s.cwd.clone(), env, cols, rows };
    let rt = term::start_on(d, launch, snap).map_err(|e| e.to_string())?;
    // Whatever the agent left on (mouse reporting, kitty keys) would reach the shell.
    rt.with(|e| e.reset_app_modes());
    let mut core = d.core();
    if core.state.session(sid).is_none() {
        drop(core);
        rt.kill(true);
        let _ = rt.tx.send(EngineMsg::Stop);
        return Ok(());
    }
    if let Some(sess) = core.state.session_mut(sid) {
        sess.kind = SessionKind::Shell;
        sess.agent = None;
        sess.agent_info = None;
        sess.agent_args.clear();
        sess.command = command;
        sess.pid = Some(rt.pid);
        sess.title.clear();
    }
    core.agents.remove(sid);
    core.rt.insert(sid.to_string(), rt);
    drop(core);
    d.mark_dirty();
    let agent = s.agent.map_or("agent", |a| a.as_str());
    d.set_status(sid, StatusState::Idle, Some(format!("{agent} {reason}")), None, Actor::system());
    Ok(())
}

pub fn tail_nonempty(mut lines: Vec<String>, n: usize) -> Vec<String> {
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let start = lines.len().saturating_sub(n);
    lines.split_off(start)
}

/// Claude Code flips its title to `✳` a few hundred ms *before* it draws its permission dialog
/// and before `PermissionRequest` arrives (seen live on 2.1.288), so a stopped title only counts
/// once it has held this long and the screen check still finds no prompt.
const TITLE_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

/// OSC 0/2 title changed (called on the engine thread; `screen` reads the visible screen).
pub fn on_title(d: &Arc<Daemon>, sid: &str, generation: u64, title: &str, screen: &mut dyn FnMut() -> Vec<String>) {
    let (agent, state, project, changed_text) = {
        let mut core = d.core();
        if core.rt.get(sid).map(|r| r.generation) != Some(generation) {
            return;
        }
        let Some(s) = core.state.session_mut(sid) else { return };
        let changed_text = title_text(&s.title) != title_text(title);
        s.title = title.to_string();
        (s.agent, s.status.state, s.project_id.clone(), changed_text)
    };
    if changed_text {
        d.mark_dirty();
        d.emit(kinds::SESSION_TITLE, Actor::system(), Some(project.clone()), Some(sid.to_string()), json!({ "title": title }));
        crate::auto_name::on_title(d, sid);
    }
    let Some(agent) = agent else { return };
    // Gaps no hook covers: approval answered, Esc during a tool, Esc/ctrl-c at a prompt, and
    // (for Codex, which has no prompt hook without trusting hooks) the start of every turn.
    match (agent_title_hint(agent, title), state) {
        (TitleHint::Working, StatusState::NeedsYou) => {
            // A midna approval blocks inside the PreToolUse hook while the title keeps spinning;
            // only an on-screen prompt can be answered behind midna's back.
            let midna_approval = d.core().state.needs_you.iter().any(|n| n.kind == NeedsYouKind::Approval && n.session_id.as_deref() == Some(sid));
            if !midna_approval && !screen_waits_on_human(&screen()) {
                d.clear_session_needs_you(sid, NeedsYouKind::PermissionPrompt);
                d.set_status(sid, StatusState::Working, Some("prompt answered (title)".into()), None, Actor::system());
            }
        }
        (TitleHint::Working, StatusState::Idle | StatusState::Done) if agent == AgentKind::Codex => {
            let actor = Actor { kind: ActorKind::Agent, session: Some(sid.to_string()), name: Some(agent.as_str().into()) };
            crate::rpc::agent::start_turn(d, sid, &project, &actor);
            d.set_status(sid, StatusState::Working, Some("working (title)".into()), None, Actor::system());
        }
        (TitleHint::Stopped, StatusState::Working | StatusState::NeedsYou) => settle_stopped(d, sid, generation),
        _ => {}
    }
}

/// Re-check a stopped title after `TITLE_SETTLE` (off the engine thread) and only then treat it
/// as an interrupt or a dismissed prompt. While a prompt stays on screen it keeps watching:
/// Esc on Claude's permission dialog fires no hook and leaves the title at `✳`, so only the
/// screen shows that the prompt went away.
fn settle_stopped(d: &Arc<Daemon>, sid: &str, generation: u64) {
    {
        let mut core = d.core();
        let Some(a) = core.agents.get_mut(sid) else { return };
        if std::mem::replace(&mut a.settle_pending, true) {
            return;
        }
    }
    let (d, sid) = (d.clone(), sid.to_string());
    let _ = std::thread::Builder::new().name("title-settle".into()).spawn(move || {
        settle_loop(&d, &sid, generation);
        if let Some(a) = d.core().agents.get_mut(&sid) {
            a.settle_pending = false;
        }
    });
}

fn settle_loop(d: &Arc<Daemon>, sid: &str, generation: u64) {
    let mut wait = TITLE_SETTLE;
    loop {
        std::thread::sleep(wait);
        wait = std::time::Duration::from_secs(1);
        let (rt, agent, title, state) = {
            let core = d.core();
            let Some(rt) = core.rt.get(sid).filter(|r| r.generation == generation).cloned() else { return };
            let Some(s) = core.state.session(sid) else { return };
            let Some(agent) = s.agent else { return };
            (rt, agent, s.title.clone(), s.status.state)
        };
        if agent_title_hint(agent, &title) != TitleHint::Stopped || !matches!(state, StatusState::Working | StatusState::NeedsYou) {
            return;
        }
        let just_raised = d.core().agents.get(sid).and_then(|a| a.prompt_raised_at).is_some_and(|t| t.elapsed() < TITLE_SETTLE);
        if state == StatusState::NeedsYou && just_raised {
            continue;
        }
        let Some((screen, _, _)) = rt.read(true) else { return };
        if screen_waits_on_human(&screen) {
            continue;
        }
        if state == StatusState::Working {
            if crate::rpc::agent::end_turn_if_open(d, sid, "stopped (title)")
                && let Some(a) = d.core().agents.get_mut(sid)
            {
                a.title_ended_at = Some(std::time::Instant::now());
            }
            d.set_status(sid, StatusState::Idle, Some("stopped (title)".into()), None, Actor::system());
        } else {
            d.clear_session_needs_you(sid, NeedsYouKind::PermissionPrompt);
            crate::rpc::agent::end_turn_if_open(d, sid, "prompt dismissed");
            d.set_status(sid, StatusState::Idle, Some("prompt dismissed (screen)".into()), None, Actor::system());
        }
        return;
    }
}

#[cfg(test)]
mod keystroke_tests {
    use super::parse_keystroke;
    use midna_proto::frame::{MOD_ALT, MOD_CTRL, MOD_SHIFT};

    #[test]
    fn keystrokes() {
        assert_eq!(parse_keystroke("ctrl-c"), Some((MOD_CTRL, "c".into())));
        assert_eq!(parse_keystroke("ctrl-shift-up"), Some((MOD_CTRL | MOD_SHIFT, "up".into())));
        assert_eq!(parse_keystroke("alt--"), Some((MOD_ALT, "-".into())));
        assert_eq!(parse_keystroke("esc"), Some((0, "escape".into())));
        assert_eq!(parse_keystroke("Enter"), Some((0, "enter".into())));
        assert_eq!(parse_keystroke("hyper-x"), None);
        assert_eq!(parse_keystroke("ctrl-nope"), None);
    }
}

/// Where root terminals start: `$HOME`, else `/`.
/// `cwd` made absolute. A relative one is relative to the caller's terminal (its agent's
/// folder, else where it started), never to the daemon's own working directory, which is `/`.
fn absolute_cwd(d: &Daemon, ctx: &Ctx, cwd: &str) -> Result<String, RpcError> {
    let path = match cwd.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{}{rest}", home_dir()),
        _ => cwd.to_string(),
    };
    if std::path::Path::new(&path).is_absolute() {
        return Ok(path);
    }
    let core = d.core();
    let base = ctx.session.as_deref().and_then(|sid| core.state.session(sid)).map(|s| s.agent_info.as_ref().and_then(|a| a.cwd.clone()).unwrap_or_else(|| s.cwd.clone()));
    let base = base.ok_or_else(|| RpcError::bad_params(format!("cwd {cwd:?} is relative; pass an absolute path")))?;
    let joined = std::path::Path::new(&base).join(&path);
    Ok(std::fs::canonicalize(&joined).unwrap_or(joined).to_string_lossy().into_owned())
}

pub fn home_dir() -> String {
    std::env::var("HOME").ok().filter(|h| std::path::Path::new(h).is_dir()).unwrap_or_else(|| "/".into())
}
