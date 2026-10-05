//! ⌘K command bar (docs/design/CommandBar-A.dc.html): a centered palette over a dimmed
//! window. Rows come from the registry in `crate::commands`; free text that matches
//! nothing hands the text to a new agent ("Ask an agent").
use super::border_w;
use crate::app::{MainWindow, Overlay, Screen, refresh};
use crate::commands::{self, CmdIcon, Command, Run};
use crate::icons::Icon;
use crate::model::*;
use crate::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Max rows shown for a non-empty query.
const LIMIT: usize = 12;

pub struct Palette {
    /// The query field (shared IME text field); `focus` is its focus handle.
    pub input: Entity<super::text_input::TextField>,
    pub focus: FocusHandle,
    pub query: String,
    pub sel: usize,
    /// Id of the destructive command armed by the first ↩.
    pub armed: Option<String>,
    scroll: ScrollHandle,
    /// `policy.check` decisions for project command lines, fetched when the bar opens.
    policy: HashMap<String, String>,
    user: Vec<Command>,
    /// `session.prompts` of the terminal the prompt list (⌘P) was opened for.
    prompts: Option<(String, Value)>,
}

impl Palette {
    pub fn new(cx: &mut App) -> Palette {
        let input = cx.new(|cx| super::text_input::TextField::new(cx, false, "Type a command, or ask an agent…"));
        let focus = input.read(cx).focus.clone();
        Palette { input, focus, query: String::new(), sel: 0, armed: None, scroll: ScrollHandle::new(), policy: HashMap::new(), user: vec![], prompts: None }
    }
}

/// Called when the bar opens: fresh query, script commands reloaded, and the policy
/// decisions for project commands refreshed in the background.
pub fn on_open(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    set_query(m, String::new(), cx);
    reload_user_commands(m, cx);
    // folders under projects.roots may have come and gone
    m.request_refresh(crate::app::refresh::PROJECTS, cx);
    let checks: Vec<(String, String)> = m.projects.iter().flat_map(|p| p.commands.iter().map(move |c| (p.id.clone(), c.run.clone()))).collect();
    if checks.is_empty() {
        return;
    }
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let res = cx
            .background_executor()
            .spawn(async move {
                let mut out = HashMap::new();
                for (pid, run) in checks {
                    let params = json!({"action": {"kind": "command", "value": run, "project": pid}});
                    if let Ok(v) = backend.call("policy.check", params) {
                        // Only an explicit rule counts; the defaults table has no opinion on commands.
                        if v.get("source").and_then(|s| s.as_str()) == Some("rule")
                            && let Some(d) = v.get("decision").and_then(|d| d.as_str())
                        {
                            out.insert(run, d.to_string());
                        }
                    }
                }
                out
            })
            .await;
        let _ = this.update(cx, |m, cx| {
            m.palette.policy = res;
            cx.notify();
        });
    })
    .detach();
}

/// User commands from `ui.commands.list` (the daemon validates `$MIDNA_HOME/commands.json`);
/// the file directly when the daemon is older or the backend is the fake. Called when the bar
/// opens, on connect and on `ui.commands_changed`.
pub fn reload_user_commands(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let res = cx
            .background_executor()
            .spawn(async move {
                match backend.call("ui.commands.list", json!({})) {
                    Ok(v) => serde_json::to_string(&v["commands"]).map(|t| commands::parse_user_commands(&t)).unwrap_or_default(),
                    Err(_) => backend.socket_path().parent().map(commands::load_user_commands).unwrap_or_default(),
                }
            })
            .await;
        let _ = this.update(cx, |m, cx| {
            if m.palette.user != res {
                m.palette.user = res;
                cx.notify();
            }
        });
    })
    .detach();
}

/// "Ask an agent" agent: setting `ui.ask.agent` (tab toggles it and saves it there).
fn current_agent(m: &MainWindow) -> AgentKind {
    match m.setting_str("ui.ask.agent").as_deref() {
        Some("codex") => AgentKind::Codex,
        Some(_) => AgentKind::Claude,
        // An older daemon without the setting: the agent last started in this project.
        None => last_agent(m, m.current_project_id().as_deref()),
    }
}

/// "Ask an agent" scope: setting `ui.ask.scope` (shift-tab toggles it).
/// Ask at root: chosen in the toggle, or forced when there is no current project.
fn at_root(m: &MainWindow) -> bool {
    m.setting_str("ui.ask.scope").as_deref() == Some("root") || current_project(m).is_none()
}

/// Save an ask toggle (optimistic locally; `settings.changed` confirms).
fn set_ask(m: &mut MainWindow, key: &'static str, value: &str, cx: &mut Context<MainWindow>) {
    m.settings.insert(key.into(), json!(value));
    m.rpc("settings.set", json!({ "key": key, "value": value }), cx, |_, _, _, _| {});
    cx.notify();
}

fn set_agent(m: &mut MainWindow, a: AgentKind, cx: &mut Context<MainWindow>) {
    set_ask(m, "ui.ask.agent", if a == AgentKind::Codex { "codex" } else { "claude" }, cx);
}

fn set_root(m: &mut MainWindow, root: bool, cx: &mut Context<MainWindow>) {
    set_ask(m, "ui.ask.scope", if root { "root" } else { "project" }, cx);
}

/// The agent most recently started in a project (Claude when none), as "New agent" uses.
fn last_agent(m: &MainWindow, project: Option<&str>) -> AgentKind {
    m.sessions
        .iter()
        .filter(|s| s.project_id.as_deref() == project && s.agent.is_some())
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
        .and_then(|s| s.agent)
        .filter(|a| *a != AgentKind::Other)
        .unwrap_or(AgentKind::Claude)
}

fn current_project(m: &MainWindow) -> Option<&Project> {
    let pid = m.current_project_id()?;
    m.projects.iter().find(|p| p.id == pid)
}

/// The full registry for the current state (built-ins plus `$MIDNA_HOME/commands.json`).
pub fn registry(m: &MainWindow) -> Vec<Command> {
    let key = |k: &str| {
        let v = m.setting_str(k).unwrap_or_else(|| crate::actions::default_keys().get(k).copied().unwrap_or("").to_string());
        if v.trim().is_empty() { String::new() } else { crate::actions::pretty(&v) }
    };
    let setting = |k: &str| m.setting_str(k);
    let policy = |run: &str| m.palette.policy.get(run).cloned();
    let snap = commands::Snapshot {
        projects: &m.projects,
        discovered: &m.discovered,
        sessions: m.ordered_sessions(),
        needs: &m.needs,
        selected: m.selected_session(),
        current_project: current_project(m),
        key: &key,
        setting: &setting,
        rules_count: m.rules_count,
        last_agent: last_agent(m, m.current_project_id().as_deref()),
        policy: &policy,
    };
    let mut cmds = commands::build(&snap);
    cmds.extend(m.palette.user.iter().cloned());
    cmds
}

struct Row {
    cmd: Command,
    hits: Vec<usize>,
    head: Option<String>,
}

fn rows(m: &MainWindow) -> Vec<Row> {
    if prompt_mode(m) {
        return prompt_rows(m);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    // "Add project folder…": a path search instead of commands
    let roots: Vec<String> = m.settings.get("projects.roots").and_then(Value::as_array).into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    if let Some(cmds) = commands::folder_rows(&m.palette.query, &home, &roots) {
        return cmds.into_iter().map(|cmd| Row { cmd, hits: vec![], head: None }).collect();
    }
    let cmds = registry(m);
    // A typed folder path ("~/Development/kass") is offered first, as "Open … as a project".
    let path = commands::path_command(&m.palette.query, &home, &m.projects).map(|cmd| Row { cmd, hits: vec![], head: None });
    path.into_iter().chain(commands::search(&cmds, &m.palette.query, LIMIT).into_iter().map(|h| Row { cmd: h.cmd.clone(), hits: h.hits, head: h.head })).collect()
}

// ------------------------------------------------------------------ behavior

/// Keep `query` in sync with the field: call once from MainWindow::new.
pub fn wire(p: &Palette, cx: &mut Context<MainWindow>) {
    cx.subscribe(&p.input, |m, field, _: &super::text_input::FieldChanged, cx| {
        let text = field.read(cx).text().to_string();
        if text != m.palette.query {
            set_query(m, text, cx);
        }
    })
    .detach();
}

fn folder_mode(m: &MainWindow) -> bool {
    m.palette.query.starts_with(commands::FOLDER_PREFIX)
}

fn prompt_mode(m: &MainWindow) -> bool {
    m.palette.query.starts_with(commands::PROMPT_PREFIX)
}

/// The selected terminal's prompts, newest first, filtered by the text after `>`.
fn prompt_rows(m: &MainWindow) -> Vec<Row> {
    let Some(s) = m.selected_session() else { return vec![] };
    let Some((_, v)) = m.palette.prompts.as_ref().filter(|(id, _)| *id == s.id) else { return vec![] };
    let cmds = commands::prompt_rows(&s.id, s.agent, v);
    let q = m.palette.query[commands::PROMPT_PREFIX.len()..].trim();
    if !q.is_empty() {
        return commands::search(&cmds, q, cmds.len()).into_iter().map(|h| Row { cmd: h.cmd.clone(), hits: h.hits, head: None }).collect();
    }
    let mut earlier = false;
    cmds.into_iter()
        .enumerate()
        .map(|(i, cmd)| {
            let head = if i == 0 {
                Some(format!("Your prompts in {}", s.name))
            } else if cmd.run.is_none() && !earlier {
                earlier = true;
                Some("Before /clear".to_string())
            } else {
                None
            };
            Row { cmd, hits: vec![], head }
        })
        .collect()
}

/// ⌘P (or the title of a terminal's pinned prompt bar): the bar listing the selected agent
/// terminal's prompts.
pub fn open_prompts(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    open_with(m, commands::PROMPT_PREFIX.to_string(), window, cx);
}

/// Fetch the selected terminal's prompts for the list.
fn fetch_prompts(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(id) = m.selected_session().map(|s| s.id.clone()) else { return };
    m.palette.prompts = Some((id.clone(), json!({})));
    m.rpc("session.prompts", json!({ "id": id }), cx, move |m, v, _, cx| {
        if m.palette.prompts.as_ref().is_some_and(|(x, _)| *x == id) {
            m.palette.prompts = Some((id, v));
            cx.notify();
        }
    });
}

/// Open the bar on a typed query, e.g. "Add project folder…" from the empty pane.
pub fn open_with(m: &mut MainWindow, query: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.overlay != Overlay::CommandBar {
        m.set_overlay(Overlay::CommandBar, window, cx);
    }
    set_query(m, query, cx);
}

fn set_query(m: &mut MainWindow, q: String, cx: &mut Context<MainWindow>) {
    if m.palette.input.read(cx).text() != q {
        m.palette.input.update(cx, |f, cx| f.set_text(&q, cx));
    }
    let entering = q.starts_with(commands::PROMPT_PREFIX) && !m.palette.query.starts_with(commands::PROMPT_PREFIX);
    m.palette.query = q;
    if entering {
        fetch_prompts(m, cx);
    }
    m.palette.sel = 0;
    m.palette.armed = None;
    m.palette.scroll.scroll_to_item(0);
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.overlay == Overlay::CommandBar {
        m.set_overlay(Overlay::None, window, cx);
    }
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let md = &ks.modifiers;
    let rows = rows(m);
    let n = rows.len();
    match ks.key.as_str() {
        "up" | "down" if !md.platform => {
            if n > 0 {
                let cur = m.palette.sel.min(n - 1);
                m.palette.sel = if ks.key == "down" { (cur + 1) % n } else { (cur + n - 1) % n };
                if m.palette.armed.as_deref() != Some(rows[m.palette.sel].cmd.id.as_str()) {
                    m.palette.armed = None;
                }
                m.palette.scroll.scroll_to_item(m.palette.sel);
            }
        }
        "enter" if !md.platform => {
            let q = m.palette.query.trim().to_string();
            if (folder_mode(m) || prompt_mode(m)) && (md.shift || n == 0) {
                // nothing to hand to an agent here
            } else if md.shift || (n == 0 && !q.is_empty()) {
                ask(m, window, cx);
            } else if let Some(r) = rows.get(m.palette.sel.min(n.saturating_sub(1))) {
                let cmd = r.cmd.clone();
                activate(m, &cmd, window, cx);
            }
        }
        "tab" if prompt_mode(m) => {}
        "tab" if folder_mode(m) => {
            // complete to the selected folder
            if let Some(Run::Prefill { text }) = rows.get(m.palette.sel.min(n.saturating_sub(1))).and_then(|r| r.cmd.run.clone()) {
                set_query(m, text, cx);
            }
        }
        "tab" => {
            if md.shift {
                let root = !at_root(m);
                set_root(m, root, cx);
            } else {
                let next = if current_agent(m) == AgentKind::Codex { AgentKind::Claude } else { AgentKind::Codex };
                set_agent(m, next, cx);
            }
        }
        // Everything else is the field's: editing keys, and typing through the IME.
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

/// ↩ on a row: arm destructive commands first, run on the second ↩.
fn activate(m: &mut MainWindow, cmd: &Command, window: &mut Window, cx: &mut Context<MainWindow>) {
    if cmd.danger.is_some() && m.palette.armed.as_deref() != Some(cmd.id.as_str()) {
        m.palette.armed = Some(cmd.id.clone());
        cx.notify();
        return;
    }
    execute(m, cmd, window, cx);
}

/// Run a command. Public so other views (and future agents' code) can run registry rows.
pub fn execute(m: &mut MainWindow, cmd: &Command, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(run) = cmd.run.clone() else { return };
    m.palette.armed = None;
    if let Run::Prefill { text } = &run {
        set_query(m, text.clone(), cx);
        return;
    }
    close(m, window, cx);
    let title = cmd.title.clone();
    match run {
        Run::Rpc { method, params } => call(m, method, params, title, cx),
        Run::Focus { session } => {
            if m.sessions.iter().any(|s| s.id == session) {
                m.select(session, window, cx);
            }
        }
        Run::Project { index } => window.dispatch_action(Box::new(crate::actions::SelectProject(index)), cx),
        Run::Screen { screen } => match screen.as_str() {
            "settings" => crate::ui::settings::open(m.backend.clone(), cx),
            "needs_you" => m.set_overlay(Overlay::NeedsYou, window, cx),
            s => {
                let target = match s {
                    "rules" => Screen::Rules,
                    "triggers" => Screen::Triggers,
                    "insights" => Screen::Insights,
                    _ => Screen::Terminal,
                };
                if m.screen != target {
                    m.set_screen(target, window, cx);
                }
            }
        },
        Run::PopOut { session } => crate::ui::popout::open(m, session, window, cx),
        Run::OpenProject { path: Some(path) } => m.add_project(path, window, cx),
        Run::OpenProject { path: None } => m.pick_project(cx),
        Run::Resolve { need, resolution } => {
            m.resolve(need, resolution, cx);
            m.toast(format!("✓ {title}"), cx);
        }
        Run::Prefill { .. } => {}
        Run::JumpPrompt { session, n } => {
            if m.selected.as_deref() != Some(session.as_str()) && m.sessions.iter().any(|s| s.id == session) {
                m.select(session.clone(), window, cx);
            }
            m.rpc("session.jump_prompt", json!({ "id": session, "n": n, "wait": false }), cx, |_, _, _, _| {});
        }
    }
}

/// Any RPC by name. A `session.open` result becomes the selection.
fn call(m: &mut MainWindow, method: String, params: Value, title: String, cx: &mut Context<MainWindow>) {
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let mcall = method.clone();
        let res = cx.background_executor().spawn(async move { backend.call(&mcall, params) }).await;
        let _ = this.update_in(cx, |m, window, cx| match res {
            Ok(v) => {
                m.request_refresh(refresh::SESSIONS | refresh::NEEDS | refresh::SETTINGS | refresh::RULES, cx);
                if method == "session.open" {
                    let id = v.get("id").and_then(|x| x.as_str()).or_else(|| v.get("session").and_then(|s| s.get("id")).and_then(|x| x.as_str()));
                    if let Some(id) = id {
                        m.select(id.to_string(), window, cx);
                    }
                }
                m.toast(format!("✓ {title}"), cx);
            }
            Err(e) => m.toast(format!("{method} failed: {e:#}"), cx),
        });
    })
    .detach();
}

/// "Ask an agent": a new agent session with the query as its prompt.
fn ask(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let q = m.palette.query.trim().to_string();
    if q.is_empty() {
        return;
    }
    let agent = current_agent(m);
    let project = if at_root(m) { None } else { m.current_project_id() };
    let params = commands::ask_params(agent, project.as_deref(), &q);
    let where_ = project.as_deref().and_then(|p| m.projects.iter().find(|x| x.id == p)).map(|p| p.name.clone()).unwrap_or_else(|| "root".into());
    close(m, window, cx);
    call(m, "session.open".into(), params, format!("Started {} in {where_}: “{}”", commands::agent_name(agent), commands::short(&q, 40)), cx);
}

/// ⌘↩ inside the bar: approve the selected approval once, else run the row.
fn approve_key(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    let rows = rows(m);
    if let Some(r) = rows.get(m.palette.sel.min(rows.len().saturating_sub(1))) {
        let cmd = r.cmd.clone();
        activate(m, &cmd, window, cx);
    }
}

fn approve_with(m: &mut MainWindow, cmd: &Command, scope: ApprovalScope, window: &mut Window, cx: &mut Context<MainWindow>) {
    if let Some(Run::Resolve { need, .. }) = &cmd.run {
        let c = Command { run: Some(Run::Resolve { need: need.clone(), resolution: Resolution::Approve { scope } }), ..cmd.clone() };
        execute(m, &c, window, cx);
    }
}

// ------------------------------------------------------------------ view

fn kbd(t: &Theme, s: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(6.))
        .rounded(px(5.))
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
        .text_color(t.dim)
        .font_family(t.mono_font.clone())
        .text_size(px(11.))
        .whitespace_nowrap()
        .child(s.into())
}

fn badge(t: &Theme, text: &str, color: Hsla, filled: bool) -> Div {
    let d = div().flex().flex_none().items_center().h(px(18.)).px(px(6.)).rounded(px(5.)).text_size(px(10.5)).font_weight(FontWeight::BOLD).whitespace_nowrap().text_color(color);
    let d = if filled { d.bg(t.need_soft) } else { d.border_1().border_color(color) };
    d.child(text.to_string())
}

fn icon_of(i: CmdIcon) -> Icon {
    match i {
        CmdIcon::Claude => Icon::Claude,
        CmdIcon::Codex => Icon::Codex,
        CmdIcon::Monitor => Icon::Monitor,
        CmdIcon::Shell => Icon::Shell,
        CmdIcon::Project => Icon::Project,
        CmdIcon::Run => Icon::Play,
        CmdIcon::Screen => Icon::Screen,
        CmdIcon::Approve => Icon::Check,
        CmdIcon::Deny => Icon::Cross,
        CmdIcon::Rule => Icon::Rules,
        CmdIcon::Pin => Icon::PopOut,
        CmdIcon::New => Icon::Plus,
        CmdIcon::Trigger => Icon::Triggers,
        CmdIcon::Restart => Icon::Restart,
    }
}

/// Title with matched characters in accent + bold.
fn highlighted(t: &Theme, title: &str, hits: &[usize]) -> StyledText {
    let mut ranges = vec![];
    let mut byte_of = title.char_indices().map(|(b, _)| b).collect::<Vec<_>>();
    byte_of.push(title.len());
    let mut i = 0;
    while i < hits.len() {
        let start = hits[i];
        let mut end = start;
        while i + 1 < hits.len() && hits[i + 1] == end + 1 {
            i += 1;
            end += 1;
        }
        if end + 1 < byte_of.len() {
            ranges.push((byte_of[start]..byte_of[end + 1], HighlightStyle { color: Some(t.accent), font_weight: Some(FontWeight::BOLD), ..Default::default() }));
        }
        i += 1;
    }
    StyledText::new(SharedString::from(title.to_string())).with_highlights(ranges)
}

pub fn render(m: &MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let rows = rows(m);
    let q = m.palette.query.trim().to_string();
    let empty = q.is_empty();
    let none = !empty && rows.is_empty();
    let sel = m.palette.sel.min(rows.len().saturating_sub(1));
    let scrim = if t.mode == ThemeMode::Dark { hsla(228. / 360., 0.33, 0.03, 0.62) } else { hsla(228. / 360., 0.23, 0.15, 0.32) };

    // ---- input row
    let context = match m.selected_session() {
        Some(s) => format!("{} › {}", current_project(m).map(|p| p.name.as_str()).unwrap_or("root"), s.name),
        None => current_project(m).map(|p| p.name.clone()).unwrap_or_else(|| "root".into()),
    };
    let input = div()
        .flex()
        .items_center()
        .gap(px(12.))
        .px(px(16.))
        .py(px(14.))
        .border_b_1()
        .border_color(t.line)
        .text_color(t.dim)
        .child(Icon::Search.el(18., t.dim))
        .child(div().flex().flex_1().min_w_0().items_center().text_size(px(17.)).text_color(t.fg).overflow_hidden().child(m.palette.input.clone()))
        .child(div().flex().flex_none().items_center().h(px(24.)).px(px(9.)).rounded(px(6.)).bg(t.panel).text_color(t.fg).text_size(px(12.)).whitespace_nowrap().child(context))
        .child(kbd(t, "esc"));

    // ---- list
    let mut list = div().id("cmd-list").flex().flex_col().max_h(px(470.)).overflow_y_scroll().track_scroll(&m.palette.scroll).p(px(6.));
    if empty {
        list = list.child(div().px(px(10.)).pt(px(4.)).pb(px(2.)).text_size(px(12.)).text_color(t.dim).child("Run a command, or type anything else to hand it to an agent."));
    }
    for (n, r) in rows.iter().enumerate() {
        list = list.child(row(m, t, r, n, n == sel, cx));
    }
    let folders = folder_mode(m);
    let prompts_mode = prompt_mode(m);
    if prompts_mode && rows.is_empty() {
        let why = match m.selected_session() {
            Some(s) if s.agent.is_none() => "Prompts are listed for agent terminals.",
            Some(_) if q.len() > commands::PROMPT_PREFIX.len() => "No prompt matches.",
            Some(_) => "No prompts in this terminal yet.",
            None => "Select an agent terminal first.",
        };
        list = list.child(div().px(px(12.)).py(px(14.)).text_color(t.dim).child(why));
    } else if none && folders {
        list = list.child(div().px(px(12.)).py(px(14.)).text_color(t.dim).child("No folder here."));
    } else if none {
        list = list.child(div().px(px(12.)).py(px(14.)).text_color(t.dim).child(format!("No command matches “{q}”. Press ↩ to hand it to an agent.")));
    }

    // ---- ask an agent
    let agent = current_agent(m);
    let root = at_root(m);
    let project = if root { None } else { m.current_project_id() };
    let project_name = current_project(m).map(|p| p.name.clone()).unwrap_or_else(|| "root".into());
    let scope_name = if root { "root".to_string() } else { project_name.clone() };
    let ask_box = (!empty && !folders && !prompts_mode).then(|| {
        let seg = |id: &'static str, on: bool| {
            let d = div().id(id).flex().items_center().gap(px(6.)).h(px(24.)).px(px(10.)).rounded(px(6.)).text_size(px(12.)).cursor_pointer();
            if on { d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD) } else { d.text_color(t.dim) }
        };
        let group = || div().flex().p(px(2.)).border_1().border_color(t.line).rounded(px(8.)).bg(t.panel);
        let agent_opts = group()
            .child(
                seg("ask-claude", agent == AgentKind::Claude)
                    .child(Icon::Claude.el(13., if agent == AgentKind::Claude { t.accent_fg } else { t.dim }))
                    .child("Claude")
                    .on_click(cx.listener(|m, _, _, cx| set_agent(m, AgentKind::Claude, cx))),
            )
            .child(
                seg("ask-codex", agent == AgentKind::Codex)
                    .child(Icon::Codex.el(13., if agent == AgentKind::Codex { t.accent_fg } else { t.dim }))
                    .child("Codex")
                    .on_click(cx.listener(|m, _, _, cx| set_agent(m, AgentKind::Codex, cx))),
            );
        let scope_opts = group()
            .when(current_project(m).is_some(), |g| g.child(seg("ask-project", !root).child(project_name.clone()).on_click(cx.listener(|m, _, _, cx| set_root(m, false, cx)))))
            .child(seg("ask-root", root).child("Root").on_click(cx.listener(|m, _, _, cx| set_root(m, true, cx))));
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .pt(px(10.))
            .pb(px(12.))
            .border_t_1()
            .border_color(t.line)
            .bg(if none { t.accent_soft } else { t.raised })
            .child(div().px(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(if none { "ASK AN AGENT · ↩" } else { "OR ASK AN AGENT" }))
            .child(
                div()
                    .id("ask-run")
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(10.))
                    .py(px(9.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(if none { t.accent } else { t.line })
                    .bg(t.raised)
                    .cursor_pointer()
                    .on_click(cx.listener(|m, _, w, cx| ask(m, w, cx)))
                    .child((if agent == AgentKind::Codex { Icon::Codex } else { Icon::Claude }).el(16., t.accent))
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(5.))
                            .text_size(px(14.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(div().flex_none().font_weight(FontWeight::BOLD).child(format!("New {} tab in {scope_name}", commands::agent_name(agent))))
                            .child(div().flex_none().text_color(t.dim).child("with"))
                            .child(div().min_w_0().truncate().child(format!("“{q}”"))),
                    )
                    .child(kbd(t, if none { "↩" } else { "⇧↩" })),
            )
            .child(div().flex().items_center().gap(px(10.)).px(px(2.)).child(agent_opts).child(scope_opts).child(
                div().flex_1().min_w_0().truncate().font_family(t.mono_font.clone()).text_size(px(11.)).text_color(t.dim).child(commands::ask_cli(agent, project.as_deref(), &q)),
            ))
    });

    // ---- "Try asking" chips (empty query)
    let prompts = empty.then(|| {
        let mut ideas: Vec<String> = vec![];
        if let Some(f) = m.sessions.iter().find(|s| s.status.state == StatusState::Failed) {
            ideas.push(format!("why did {} fail?", f.name));
        }
        ideas.push(format!("start Claude on every PR opened in {project_name}"));
        ideas.push("add a rule: ask before any rm -rf".into());
        let mut d = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .px(px(14.))
            .pt(px(10.))
            .pb(px(12.))
            .border_t_1()
            .border_color(t.line)
            .child(div().mr(px(4.)).text_size(px(12.)).text_color(t.dim).child("Try asking"));
        for (i, idea) in ideas.into_iter().take(3).enumerate() {
            let text = idea.clone();
            d = d.child(
                div()
                    .id(("try", i))
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(13.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.panel)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.accent))
                    .on_click(cx.listener(move |m, _, _, cx| set_query(m, text.clone(), cx)))
                    .child(idea),
            );
        }
        d
    });

    let sel_danger = rows.get(sel).is_some_and(|r| r.cmd.danger.is_some());
    let enter_label = if prompts_mode {
        "jump"
    } else if none {
        "ask agent"
    } else if sel_danger {
        "arm, ↩ again to run"
    } else {
        "run"
    };
    let legend = |c: Hsla, label: &str| div().flex().items_center().gap(px(4.)).child(div().size(px(8.)).bg(c)).child(label.to_string());
    let footer = div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(14.))
        .px(px(14.))
        .py(px(8.))
        .border_t_1()
        .border_color(t.line)
        .bg(t.panel)
        .text_size(px(11.5))
        .text_color(t.dim)
        .child("↑↓ select")
        .child(format!("↩ {enter_label}"))
        .when(prompts_mode, |d| d.child("prompts sent before a /clear can't be jumped to"))
        .when(!prompts_mode, |d| {
            d.child("⇧↩ ask instead")
                .child("⇥ agent · ⇧⇥ scope")
                .child(div().flex_1())
                .child(legend(t.err, "destructive"))
                .child(legend(t.need, "needs approval"))
                .child(legend(t.accent, "you only"))
        });

    let dialog = div()
        .id("command-bar")
        .key_context("MidnaOverlay")
        .on_key_down(cx.listener(on_key))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| {
            if m.palette.armed.take().is_some() {
                cx.notify();
            } else {
                close(m, w, cx);
            }
        }))
        .on_action(cx.listener(|m, _: &crate::actions::ApproveOnce, w, cx| approve_key(m, w, cx)))
        .on_action(cx.listener(|m, _: &crate::actions::Deny, _w, cx| set_query(m, String::new(), cx)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .absolute()
        .top(px(92.))
        .w(px(720.))
        .flex()
        .flex_col()
        .rounded(px(14.))
        .border_1()
        .border_color(t.line)
        .bg(t.raised)
        .overflow_hidden()
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.5), offset: point(px(0.), px(28.)), blur_radius: px(80.), spread_radius: px(0.), inset: false }])
        .child(input)
        .child(list)
        .children(ask_box)
        .children(prompts)
        .child(footer);

    deferred(
        div()
            .id("command-bar-scrim")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(scrim)
            .flex()
            .justify_center()
            .on_mouse_down(MouseButton::Left, cx.listener(|m, _, w, cx| close(m, w, cx)))
            .child(dialog),
    )
    .with_priority(2)
}

fn row(m: &MainWindow, t: &Theme, r: &Row, n: usize, on: bool, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let c = &r.cmd;
    let armed = m.palette.armed.as_deref() == Some(c.id.as_str());
    let edge = if c.danger.is_some() { t.err } else { t.accent };
    let icon_color = if c.danger.is_some() {
        t.err
    } else if c.icon == CmdIcon::Approve {
        t.ok
    } else if c.pending {
        t.need
    } else {
        t.dim
    };
    let mut badges = div().flex().flex_none().items_center().gap(px(6.));
    if c.pending {
        badges = badges.child(badge(t, "waiting on you", t.need, true));
    }
    if c.approval.is_some() {
        badges = badges.child(badge(t, "needs approval", t.need, false));
    }
    if c.danger.is_some() {
        badges = badges.child(badge(t, "destructive", t.err, false));
    }
    if c.human_only {
        badges = badges.child(badge(t, "you only", t.accent, false));
    }
    if let Some(s) = &c.state {
        badges = badges.child(badge(t, s, t.dim, false).border_color(t.line));
    }
    if let Some(k) = &c.keys {
        badges = badges.child(kbd(t, k.clone()));
    }
    let cmd = c.clone();
    let line = div()
        .id(("cmd-row", n))
        .flex()
        .flex_col()
        .gap(px(1.))
        .w_full()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(8.))
        .cursor_pointer()
        .when(on, |d| d.bg(t.accent_soft).shadow(vec![BoxShadow { color: edge, offset: point(px(2.), px(0.)), blur_radius: px(0.), spread_radius: px(0.), inset: true }]))
        .when(!on, |d| d.hover(|s| s.bg(t.panel)))
        .on_click(cx.listener(move |m, _, w, cx| {
            m.palette.sel = n;
            activate(m, &cmd, w, cx);
        }))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().w(px(20.)).flex().flex_none().justify_center().child(icon_of(c.icon).el(16., icon_color)))
                .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_size(px(14.)).child(highlighted(t, &c.title, &r.hits)))
                .child(badges),
        )
        .when(!c.sub.is_empty(), |d| d.child(div().pl(px(32.)).min_w_0().truncate().text_size(px(12.)).text_color(t.dim).child(c.sub.clone())));

    let chips = (on && c.approve_menu && !armed).then(|| {
        let session_name = match &c.run {
            Some(Run::Resolve { need, .. }) => {
                m.needs.iter().find(|x| &x.id == need).and_then(|x| x.session_id.as_ref()).and_then(|sid| m.sessions.iter().find(|s| &s.id == sid)).map(|s| s.name.clone())
            }
            _ => None,
        }
        .unwrap_or_else(|| "it".into());
        let opts: Vec<(&str, String, ApprovalScope)> = vec![
            ("Once", m.key_label("keys.approve"), ApprovalScope::Once),
            ("15 min", "this command".into(), ApprovalScope::Minutes { minutes: 15 }),
            ("1 hour", "this command".into(), ApprovalScope::Minutes { minutes: 60 }),
            ("This session", format!("until {session_name} exits"), ApprovalScope::Session),
            ("Always", "adds a rule".into(), ApprovalScope::Always),
        ];
        let mut d = div().flex().flex_wrap().gap(px(6.)).pl(px(42.)).pr(px(10.)).pt(px(4.)).pb(px(8.));
        for (i, (label, hint, scope)) in opts.into_iter().enumerate() {
            let cmd = c.clone();
            d = d.child(
                div()
                    .id(("approve-chip", i))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(26.))
                    .px(px(10.))
                    .rounded(px(13.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.panel)
                    .text_size(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.accent))
                    .on_click(cx.listener(move |m, _, w, cx| approve_with(m, &cmd, scope.clone(), w, cx)))
                    .child(label.to_string())
                    .child(div().text_color(t.dim).child(format!("· {hint}"))),
            );
        }
        d
    });

    let confirm = armed.then(|| {
        let cmd = c.clone();
        border_w(div(), 1.)
            .flex()
            .items_center()
            .gap(px(10.))
            .ml(px(40.))
            .mr(px(6.))
            .mt(px(2.))
            .mb(px(8.))
            .px(px(12.))
            .py(px(9.))
            .rounded(px(8.))
            .border_color(t.err)
            .bg(t.panel)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_wrap()
                    .gap(px(4.))
                    .child(div().font_weight(FontWeight::BOLD).text_color(t.err).child("Destructive."))
                    .child(c.danger.clone().unwrap_or_default())
                    .child(div().text_color(t.dim).child("↩ again to confirm.")),
            )
            .child(
                div()
                    .id("confirm-run")
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px(px(12.))
                    .rounded(px(7.))
                    .bg(t.err)
                    .text_color(gpui_kit::white())
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(move |m, _, w, cx| execute(m, &cmd, w, cx)))
                    .child("Confirm ↩"),
            )
            .child(
                div()
                    .id("confirm-cancel")
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(7.))
                    .border_1()
                    .border_color(t.line)
                    .cursor_pointer()
                    .on_click(cx.listener(|m, _, _, cx| {
                        m.palette.armed = None;
                        cx.notify();
                    }))
                    .child("Cancel esc"),
            )
    });

    let head = r.head.clone().map(|h| {
        let color = if h == "Needs you" { t.need } else { t.dim };
        div().px(px(10.)).pt(px(10.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(h.to_uppercase())
    });
    div().flex().flex_col().children(head).child(line).children(chips).children(confirm)
}
