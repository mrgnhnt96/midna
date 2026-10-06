//! Settings: its own standard macOS window (Settings-C, "settings as code").
//!
//! Every row shows the control, what the setting does, the exact CLI an agent would run
//! (copyable), and who may change it. Data is `settings.list` + the
//! `midna_proto::settings::SETTINGS` catalog, live-updated on `settings.changed` (agents
//! change settings at any time). The title bar toggles Rows ↔ an annotated, read-only
//! `settings.json`. The Ask box opens an agent with the request as its prompt. The GUI is
//! the human, so human-only settings are editable here.
use crate::backend::{Backend, BackendEvent};
use crate::icons::Icon;
use crate::model::{Event, SettingEntry, parse_list};
use crate::theme::Theme;
use crate::ui::screen_kit::{KeyOutcome, LineInput};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::settings::{SETTINGS, SettingKind};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[path = "settings_notify.rs"]
mod notify_rows;
#[path = "settings_shortcuts.rs"]
mod shortcuts;

struct SettingsWindowHandle(Option<WindowHandle<SettingsWindow>>);
impl Global for SettingsWindowHandle {}

/// Open (or bring forward) the Settings window.
pub fn open(backend: Arc<dyn Backend>, cx: &mut App) {
    if let Some(h) = cx.try_global::<SettingsWindowHandle>().and_then(|g| g.0)
        && h.update(cx, |_, w, _| w.activate_window()).is_ok()
    {
        return;
    }
    let w: f32 = std::env::var("MIDNA_SETTINGS_W").ok().and_then(|v| v.parse().ok()).unwrap_or(900.);
    let h: f32 = std::env::var("MIDNA_SETTINGS_H").ok().and_then(|v| v.parse().ok()).unwrap_or(640.);
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
        titlebar: Some(TitlebarOptions { title: Some("Settings".into()), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(17.))) }),
        window_min_size: Some(size(px(720.), px(420.))),
        app_id: Some("com.mrgnhnt.midna".into()),
        focus: std::env::var("MIDNA_NO_ACTIVATE").is_err(),
        ..Default::default()
    };
    let h = cx.open_window(opts, |window, cx| cx.new(|cx| SettingsWindow::new(backend, window, cx)));
    if let Ok(h) = h {
        cx.set_global(SettingsWindowHandle(Some(h)));
        #[cfg(feature = "snapshot")]
        snapshot(h, cx);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Rows,
    Json,
    /// Every shortcut, searchable by name or by pressing keys (settings_shortcuts.rs).
    Shortcuts,
}

struct Last {
    cmd: String,
    ok: bool,
    result: String,
    who: String,
    at: Instant,
}

pub struct SettingsWindow {
    backend: Arc<dyn Backend>,
    entries: Vec<SettingEntry>,
    info: Value,
    webhooks: Value,
    /// `hooks.status`: midna's hooks in Claude Code's and Codex's global config (ui/hooks.rs).
    hooks: Value,
    /// `notify.media`: the sounds and images notifications can use.
    media: Value,
    /// The sound/image picker that's open (its setting key).
    picker: Option<String>,
    error: Option<String>,
    view: View,
    ask: LineInput,
    /// Shortcuts tab: the search field, the key detector while it's on, and the keys it caught.
    search: LineInput,
    recorder: Option<Subscription>,
    recorded: Vec<String>,
    /// The shortcut being rebound by pressing keys.
    editing: Option<shortcuts::Editing>,
    /// A shortcut row's right-click menu: its setting and where it opened.
    shortcut_menu: Option<(&'static str, Point<Pixels>)>,
    last: Option<Last>,
    /// Keys this window changed recently (so the echoed `settings.changed` reads "you").
    mine: Vec<(String, Instant)>,
    armed_reset: bool,
    /// "Reset midna" (daemon.reset) armed by its first click.
    armed_daemon_reset: bool,
    copied: Option<(String, Instant)>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl SettingsWindow {
    fn new(backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded::<BackendEvent>();
        backend.subscribe(tx);
        let task = cx.spawn(async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this.update(cx, |s, cx| s.on_backend_event(ev, cx)).is_err() {
                    break;
                }
            }
        });
        let search = LineInput::new(cx, false, "Search shortcuts");
        let subs = vec![
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
            cx.subscribe(&search.field, |s, _, _: &crate::ui::text_input::FieldChanged, cx| {
                s.recorded.clear();
                cx.notify();
            }),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let mut s = SettingsWindow {
            backend,
            entries: vec![],
            info: Value::Null,
            webhooks: Value::Null,
            hooks: Value::Null,
            media: Value::Null,
            picker: None,
            error: None,
            view: match crate::dev::var("MIDNA_SETTINGS_VIEW").as_deref() {
                Ok("json") => View::Json,
                Ok("shortcuts") => View::Shortcuts,
                _ => View::Rows,
            },
            ask: LineInput::new(cx, false, "Ask: make ⌘T open Claude at the project root"),
            search,
            recorder: None,
            recorded: vec![],
            editing: None,
            shortcut_menu: None,
            last: None,
            mine: vec![],
            armed_reset: false,
            armed_daemon_reset: false,
            copied: None,
            focus,
            scroll: ScrollHandle::new(),
            _subs: subs,
            _tasks: vec![task],
        };
        s.load(cx);
        if let Some(sc) = crate::dev::var("MIDNA_SETTINGS_EDIT").ok().and_then(|k| crate::actions::SHORTCUTS.iter().find(|s| s.setting == k)) {
            // dev (screenshots): start rebinding this shortcut
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let _ = this.update_in(cx, |s, window, cx| s.start_edit(sc.setting, window, cx));
            })
            .detach();
        }
        if let Some(sc) = crate::dev::var("MIDNA_SETTINGS_SHORTCUT_MENU").ok().and_then(|k| crate::actions::SHORTCUTS.iter().find(|s| s.setting == k)) {
            // dev (screenshots): a shortcut row's right-click menu
            s.shortcut_menu = Some((sc.setting, point(px(340.), px(250.))));
        }
        if let Ok(keys) = crate::dev::var("MIDNA_SETTINGS_DEBUG_KEYS") {
            // Dev: comma-separated keystrokes through GPUI's own dispatch (interceptors,
            // bindings, key handlers), e.g. to drive the key detector.
            cx.spawn_in(window, async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(800)).await;
                for k in keys.split(',').filter(|k| !k.is_empty()) {
                    if let Ok(ks) = Keystroke::parse(k) {
                        let _ = cx.update(|window, cx| {
                            window.dispatch_keystroke(ks, cx);
                        });
                    }
                    cx.background_executor().timer(Duration::from_millis(120)).await;
                }
            })
            .detach();
        }
        s
    }

    fn on_backend_event(&mut self, ev: BackendEvent, cx: &mut Context<Self>) {
        match ev {
            BackendEvent::Event(e) if e.kind == "settings.changed" => {
                self.note_change(&e);
                self.load(cx);
            }
            BackendEvent::Event(e) if e.kind.starts_with("webhooks.") || e.kind == "notify.media" => self.load(cx),
            BackendEvent::Event(e) if e.kind == "hooks.changed" => {
                self.hooks = e.data.clone();
                cx.notify();
            }
            BackendEvent::Conn(crate::backend::ConnState::Connected) => self.load(cx),
            _ => {}
        }
    }

    /// Footer line for a change made anywhere (this window, the CLI, an agent).
    fn note_change(&mut self, e: &Event) {
        let key = e.data.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        let value = e.data.get("value").map(cli_text).unwrap_or_default();
        self.mine.retain(|(_, t)| t.elapsed() < Duration::from_secs(5));
        let ours = self.mine.iter().position(|(k, _)| *k == key);
        let who = if let Some(i) = ours {
            self.mine.remove(i);
            "you, from this window".to_string()
        } else {
            match e.actor.kind.as_str() {
                "human" => "you".to_string(),
                "agent" => match (&e.actor.name, &e.actor.session) {
                    (Some(n), Some(s)) => format!("{} in terminal {s}", capitalize(n)),
                    (_, Some(s)) => format!("an agent in terminal {s}"),
                    _ => "an agent (CLI)".to_string(),
                },
                "trigger" => "a trigger".to_string(),
                _ => "midnad".to_string(),
            }
        };
        self.last = Some(Last { cmd: format!("midna settings set {key} {value}"), ok: true, result: "✓ applied".into(), who, at: Instant::now() });
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        // The answer lands before the rows below are fetched (Settings ▸ Permissions).
        crate::notify::refresh_permission();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let list = backend.call("settings.list", json!({}));
                    let info = backend.call("daemon.info", json!({})).unwrap_or(Value::Null);
                    let webhooks = backend.call("webhooks.status", json!({})).unwrap_or(Value::Null);
                    let media = backend.call("notify.media", json!({})).unwrap_or(Value::Null);
                    let hooks = backend.call("hooks.status", json!({})).unwrap_or(Value::Null);
                    (list, info, webhooks, media, hooks)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                match r.0 {
                    Ok(v) => {
                        s.entries = parse_list(&v);
                        s.error = None;
                    }
                    Err(e) => s.error = Some(format!("{e:#}")),
                }
                s.info = r.1;
                s.webhooks = r.2;
                s.media = r.3;
                s.hooks = r.4;
                if let Ok(k) = crate::dev::var("MIDNA_SETTINGS_PICKER") {
                    // dev (screenshots): open this setting's sound/image picker
                    s.picker = Some(k);
                }
                if let Ok(k) = crate::dev::var("MIDNA_SETTINGS_KEYS") {
                    // dev (screenshots): a key-detector search, e.g. "cmd-t"
                    s.recorded = vec![k];
                }
                if let Ok(v) = crate::dev::var("MIDNA_SETTINGS_SCROLL") {
                    // dev (screenshots): scroll the rows down by this many pixels
                    let y: f32 = v.parse().unwrap_or(0.);
                    s.scroll.set_offset(point(px(0.), px(-y)));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn value(&self, key: &str) -> Value {
        self.entries.iter().find(|e| e.key == key).map(|e| e.value.clone()).or_else(|| midna_proto::settings::setting(key).map(|s| s.default.to_json())).unwrap_or(Value::Null)
    }

    fn set(&mut self, key: &str, value: Value, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let k = key.to_string();
        self.mine.push((k.clone(), Instant::now()));
        let cmd = format!("midna settings set {k} {}", cli_text(&value));
        // optimistic
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == k) {
            e.value = value.clone();
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("settings.set", json!({"key": k, "value": value})) }).await;
            let _ = this.update(cx, |s, cx| {
                if let Err(e) = r {
                    s.last = Some(Last { cmd, ok: false, result: format!("✗ {e:#}"), who: "you, from this window".into(), at: Instant::now() });
                    s.load(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reset_all(&mut self, cx: &mut Context<Self>) {
        self.armed_reset = false;
        let keys: Vec<String> = self.entries.iter().filter(|e| e.value != e.default).map(|e| e.key.clone()).collect();
        let backend = self.backend.clone();
        let n = keys.len();
        for k in &keys {
            self.mine.push((k.clone(), Instant::now()));
        }
        cx.spawn(async move |this, cx| {
            let errs = cx.background_executor().spawn(async move { keys.iter().filter(|k| backend.call("settings.reset", json!({"key": k})).is_err()).count() }).await;
            let _ = this.update(cx, |s, cx| {
                s.last = Some(Last {
                    cmd: "midna settings reset <every changed key>".into(),
                    ok: errs == 0,
                    result: if errs == 0 { format!("✓ {n} reset to default") } else { format!("✗ {errs} of {n} failed") },
                    who: "you, from this window".into(),
                    at: Instant::now(),
                });
                s.load(cx);
            });
        })
        .detach();
    }

    fn reset_daemon(&mut self, cx: &mut Context<Self>) {
        self.armed_daemon_reset = false;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("daemon.reset", json!({ "keep_rules": true })) }).await;
            let _ = this.update(cx, |s, cx| {
                s.last = Some(Last {
                    cmd: "midna daemon reset".into(),
                    ok: r.is_ok(),
                    result: match &r {
                        Ok(v) => format!(
                            "✓ closed {} terminal(s), removed {} project(s) and {} trigger(s), reset {} setting(s); rules and the event log kept",
                            v["sessions_closed"], v["projects_removed"], v["triggers_removed"], v["settings_reset"]
                        ),
                        Err(e) => format!("✗ {e:#}"),
                    },
                    who: "you, from this window".into(),
                    at: Instant::now(),
                });
                s.load(cx);
            });
        })
        .detach();
    }

    fn copy(&mut self, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.copied = Some((text, Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1600)).await;
            let _ = this.update(cx, |s, cx| {
                if s.copied.as_ref().is_some_and(|(_, t)| t.elapsed() >= Duration::from_millis(1500)) {
                    s.copied = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Ask box: open a Claude agent at the root with the request as its prompt, then bring the
    /// main window to that terminal.
    fn submit_ask(&mut self, cx: &mut Context<Self>) {
        let text = self.ask.text(cx).trim().to_string();
        if text.is_empty() {
            return;
        }
        self.ask.clear(cx);
        let prompt = format!("{text}\n\n(This is a midna settings request. `midna settings list` shows every setting; `midna settings set <key> <value>` changes one.)");
        let backend = self.backend.clone();
        let shown = text.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let v = backend.call("session.open", json!({"kind": "agent", "agent": "claude", "name": "settings", "prompt": prompt}))?;
                    let id = v.get("id").and_then(Value::as_str).or_else(|| v.pointer("/session/id").and_then(Value::as_str)).unwrap_or("").to_string();
                    if !id.is_empty() {
                        let _ = backend.call("window.command", json!({"action": "front", "target": id}));
                    }
                    anyhow::Ok(id)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.last = Some(match r {
                    Ok(id) => Last {
                        cmd: format!("midna open --agent claude -- \"{shown}\""),
                        ok: true,
                        result: format!("✓ agent {id} started"),
                        who: "you, from this window".into(),
                        at: Instant::now(),
                    },
                    Err(e) => Last { cmd: "midna open --agent claude".into(), ok: false, result: format!("✗ {e:#}"), who: "you, from this window".into(), at: Instant::now() },
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

// ------------------------------------------------------------------ row model

#[derive(Clone, Copy, PartialEq)]
enum Who {
    Agents,
    Human,
    ReadOnly,
}

enum Control {
    Seg { key: String, options: Vec<(String, String)>, current: String },
    Switch { key: String, on: bool, on_text: &'static str, off_text: &'static str },
    Text { dot: Option<Hsla>, text: String, color: Hsla, action: Option<(String, Act, bool)> },
    /// A notification kind's sound picker, volume and preview (settings_notify.rs).
    Sound { cat: &'static str },
    Volume { key: String },
    /// An image picker: `notify.image` (cat None) or a kind's `notify.image.<kind>`.
    Image { key: String, cat: Option<&'static str> },
    /// `theme` / `theme.dark` / `theme.light`: swatch chips for every theme (customs too).
    Theme { key: String, current: String },
}

#[derive(Clone)]
enum Act {
    Url(String),
    ResetSettings,
    /// `daemon.reset`: terminals, projects, triggers, needs-you and settings (rules and the log kept).
    ResetDaemon,
    /// Install / login item / update commands (lifecycle.rs).
    Life(crate::lifecycle::Cmd),
    /// Agent hooks: open the main window's hooks sheet (the diff, then install or remove).
    Hooks { uninstall: bool },
}

struct RowSpec {
    label: String,
    note: Option<(String, Hsla)>,
    control: Control,
    cli: String,
    who: Who,
    warn: bool,
}

struct Group {
    name: &'static str,
    danger: bool,
    badge: usize,
    rows: Vec<RowSpec>,
}

fn label_for(key: &str) -> String {
    match key {
        "theme" => "Theme",
        "theme.dark" => "When macOS is dark",
        "theme.light" => "When macOS is light",
        "theme.colors" => "Color overrides",
        "density" => "Sidebar density",
        "ui.header.script" => "Terminal header line",
        "ui.row.script" => "Sidebar row line",
        "ui.status.script" => "Status bar line",
        "ui.status.items" => "Status bar items",
        "updates.channel" => "Channel",
        "ui.ask.agent" => "⌘K “Ask an agent” uses",
        "ui.ask.scope" => "⌘K “Ask an agent” starts in",
        "updates.feed_url" => "Feed",
        "webhooks.path" => "Delivery path",
        "webhooks.port" => "Local port",
        "webhooks.relay_url" => "Relay URL",
        "agents.may_move_windows" => "Agents may move windows",
        "agents.may_close_idle" => "Agents may close idle terminals",
        "agents.may_force_close" => "Agents may force-close terminals",
        "approve.from_cli" => "Agents may approve their own requests",
        "agents.claude.statusline" => "Claude status line (cost)",
        "agents.mcp" => "Give agents the midna MCP tools",
        "agents.system_hint" => "Tell agents they're in midna",
        "policy.default" => "When no rule matches",
        "policy.request_timeout_secs" => "Approval timeout (seconds)",
        "git.refresh_secs" => "Git refresh (seconds)",
        "kass.auto_send" => "Send dictation when Kass finishes",
        "projects.roots" => "Project folders",
        "terminal.link_preview" => "Link previews",
        "terminal.preview_path_click" => "Clicking a preview's path",
        "notify.enabled" => "Show notifications",
        "notify.turn_done_min_secs" => "“Agent finished” after (seconds)",
        "notify.volume" => "All sounds",
        "notify.sounds" => "Play sounds",
        "notify.sounds_in_app" => "While you're using midna",
        "notify.image" => "Every notification",
        "notify.when_focused" => "Also for the terminal in front of you",
        "notify.when_app_closed" => "When the app isn't running",
        k if k.starts_with("notify.") => {
            let kind = k.rsplit('.').next().unwrap_or(k);
            match (midna_proto::notify::category(kind), midna_proto::notify::effect(kind)) {
                (Some(c), _) => c.label,
                (_, Some(e)) => e.label,
                _ => k,
            }
        }
        k => return k.to_string(),
    }
    .to_string()
}

fn option_label(key: &str, v: &str) -> String {
    match (key, v) {
        ("webhooks.path", "tailscale_funnel") => "Funnel".into(),
        ("webhooks.path", "self_relay") => "Self-hosted".into(),
        ("webhooks.path", "midna_relay") => "midna relay".into(),
        ("webhooks.path", "off") => "Off".into(),
        ("ui.header.script", "github+agent") => "github + agent".into(),
        ("terminal.link_preview", "hover") => "On hover".into(),
        ("terminal.link_preview", "cmd") => "On ⌘-hover".into(),
        ("terminal.preview_path_click", "reveal") => "Reveal in Finder".into(),
        ("terminal.preview_path_click", "ide") => "Open in IDE".into(),
        ("terminal.preview_path_click", "copy") => "Copy path".into(),
        // script names are names: keep them as typed
        ("ui.ask.agent" | "ui.ask.scope", v) => capitalize(v),
        (k, v) if k.starts_with("ui.") => v.to_string(),
        (_, v) => capitalize(v),
    }
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Array(a) if a.iter().all(Value::is_string) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
        v => v.to_string(),
    }
}

/// A value as typed on the command line: "" for an empty string.
fn cli_text(v: &Value) -> String {
    match value_text(v) {
        t if t.is_empty() => "\"\"".into(),
        t => t,
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// AXIsProcessTrusted: whether midna may use Accessibility (Kass input editing, window moves).
pub(crate) fn accessibility_trusted() -> bool {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    unsafe { AXIsProcessTrusted() != 0 }
}

const PANE_AX: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
const PANE_NOTIF: &str = midna_proto::paths::NOTIFICATIONS_PANE;

impl SettingsWindow {
    fn spec_row(&self, key: &str) -> Option<RowSpec> {
        let spec = midna_proto::settings::setting(key)?;
        let entry = self.entries.iter().find(|e| e.key == key);
        let value = self.value(key);
        let who = if spec.human_only { Who::Human } else { Who::Agents };
        let text = value_text(&value);
        let control = match spec.ty {
            SettingKind::Enum { .. } if matches!(key, "theme" | "theme.dark" | "theme.light") => Control::Theme { key: key.into(), current: text.clone() },
            SettingKind::Enum { options, .. } => {
                let mut opts: Vec<(String, String)> = options.iter().map(|o| (o.to_string(), option_label(key, o))).collect();
                if !options.contains(&text.as_str()) {
                    opts.push((text.clone(), text.rsplit('/').next().unwrap_or(&text).to_string()));
                }
                Control::Seg { key: key.into(), options: opts, current: text.clone() }
            }
            SettingKind::Bool => {
                let allow = key.starts_with("agents.may") || key == "approve.from_cli";
                Control::Switch {
                    key: key.into(),
                    on: value.as_bool().unwrap_or(false),
                    on_text: if allow { "allowed" } else { "on" },
                    off_text: if allow { "ask first" } else { "off" },
                }
            }
            _ => Control::Text { dot: None, text: if text.is_empty() { "not set".into() } else { text.clone() }, color: cx_dim_placeholder(), action: None },
        };
        let cli_value = if text.is_empty() {
            "<value>".to_string()
        } else if text.contains(' ') {
            format!("\"{text}\"")
        } else {
            text.clone()
        };
        let changed = entry.is_some_and(|e| e.value != e.default);
        // the who column already says it
        let mut note = spec.description.trim_end_matches(" Human only.").to_string();
        if changed {
            note.push_str(&format!(" (default: {})", value_text(&spec.default.to_json())));
        }
        Some(RowSpec { label: label_for(key), note: Some((note, Hsla::default())), control, cli: format!("midna settings set {key} {cli_value}"), who, warn: false })
    }

    /// Theme chips: a mini swatch (background, panel, accent) and the name, dark themes then
    /// light ones. `theme` also offers "Follow macOS"; `theme.dark` / `theme.light` list one kind.
    fn theme_control(&self, t: &Theme, key: String, current: String, cx: &mut Context<Self>) -> impl IntoElement {
        use midna_proto::themes;
        let current = if key == "theme" && current != "system" { themes::choose(&current, "", "", true) } else { current };
        let all = crate::theme::cached_themes();
        let only = match key.as_str() {
            "theme.dark" => Some(true),
            "theme.light" => Some(false),
            _ => None,
        };
        let chip = |id: String, name: String, swatch: Option<[Hsla; 3]>, on: bool, cx: &mut Context<Self>| {
            let k = key.clone();
            div()
                .id(SharedString::from(format!("theme-{key}-{id}")))
                .flex()
                .items_center()
                .gap(px(7.))
                .h(px(28.))
                .pl(px(5.))
                .pr(px(10.))
                .rounded(px(7.))
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |d| d.bg(t.accent_soft))
                .cursor_pointer()
                .text_size(px(12.))
                .whitespace_nowrap()
                .text_color(if on { t.fg } else { t.dim })
                .when(on, |d| d.font_weight(FontWeight::BOLD))
                .hover(|s| s.text_color(t.fg))
                .children(swatch.map(|[bg, panel, accent]| {
                    div().w(px(26.)).h(px(18.)).rounded(px(4.)).overflow_hidden().flex().border_1().border_color(t.line).child(div().w(px(8.)).h_full().bg(panel)).child(
                        div().flex_1().h_full().bg(bg).flex().items_center().justify_center().child(div().w(px(7.)).h(px(7.)).rounded_full().bg(accent)),
                    )
                }))
                .on_click(cx.listener(move |s, _, _, cx| {
                    if !on {
                        s.set(&k, json!(id), cx);
                    }
                }))
                .child(name)
        };
        let mut col = div().flex().flex_col().gap(px(6.));
        if key == "theme" {
            col = col.child(div().flex().child(chip("system".into(), "Follow macOS".into(), None, current == "system", cx)));
        }
        for dark in [true, false] {
            if only.is_some_and(|o| o != dark) {
                continue;
            }
            let mut row = div().flex().flex_wrap().gap(px(6.));
            for d in all.iter().filter(|d| d.dark == dark) {
                let c = |k: &str| crate::theme::rgb3(d.get(k).unwrap_or([0; 3]), 1.);
                row = row.child(chip(d.id.clone(), d.name.clone(), Some([c("term"), c("panel"), c("accent")]), d.id == current, cx));
            }
            col = col.child(row);
        }
        col
    }

    /// "Claude Code hooks" / "Codex hooks": midna's global install (`hooks.status`), with a
    /// button that opens the main window's hooks sheet (shows the diff before writing).
    fn hooks_row(&self, t: &Theme, agent: &str) -> RowSpec {
        let h = &self.hooks[agent];
        let (label, name) = if agent == "claude" { ("Claude Code hooks", "Claude Code") } else { ("Codex hooks", "Codex") };
        let path = h["path"].as_str().unwrap_or(if agent == "claude" { "~/.claude/settings.json" } else { "~/.codex/config.toml" });
        let detail = h["detail"].as_str().map(str::to_string);
        let per_terminal = if agent == "claude" {
            "midna adds its hooks to the agents it starts. Install globally so a claude you type into a terminal reports too."
        } else {
            "midna adds its notify to the agents it starts. Install globally so a codex you type into a terminal reports too; your own notify keeps running."
        };
        let (dot, text, color, note, action): (Hsla, String, Hsla, String, Option<(&str, bool)>) = match h["state"].as_str() {
            Some("current") => (t.ok, "Global · current".into(), t.fg, format!("In {path}. Does nothing outside midna terminals."), Some(("Remove…", true))),
            Some("stale") => (t.need, "Global · out of date".into(), t.need, detail.unwrap_or_else(|| format!("midna's entries in {path} are out of date."))
                + " Until reinstalled, midna adds its hooks to the agents it starts.", Some(("Reinstall…", false))),
            Some("not_installed") => (t.fg, "Per terminal".into(), t.fg, per_terminal.into(), Some(("Install…", false))),
            Some("error") => (t.err, format!("Can't read {}", path.rsplit('/').next().unwrap_or(path)), t.err, detail.unwrap_or_else(|| path.into()), Some(("Reinstall…", false))),
            Some("unavailable") => (t.dim, format!("{name} not set up"), t.dim, format!("No {name} config on this Mac."), None),
            _ => (t.dim, "Checking…".into(), t.dim, String::new(), None),
        };
        RowSpec {
            label: label.into(),
            note: (!note.is_empty()).then(|| (note, Hsla::default())),
            control: Control::Text {
                dot: Some(dot),
                text,
                color,
                action: action.map(|(l, uninstall)| (l.to_string(), Act::Hooks { uninstall }, !uninstall && h["state"] != "not_installed")),
            },
            cli: "midna hooks status".into(),
            who: Who::Human,
            warn: matches!(h["state"].as_str(), Some("stale" | "error")),
        }
    }

    fn groups(&self, t: &Theme) -> Vec<Group> {
        let row = |k: &str| self.spec_row(k);
        let text = |label: &str, value: String, color: Hsla, dot: Option<Hsla>, cli: &str, who: Who, note: Option<String>| RowSpec {
            label: label.into(),
            note: note.map(|n| (n, Hsla::default())),
            control: Control::Text { dot, text: value, color, action: None },
            cli: cli.into(),
            who,
            warn: false,
        };
        // Updates
        let daemon_v = self.info.get("version").and_then(Value::as_str).unwrap_or("not connected").to_string();
        let uptime = self.info.get("uptime_secs").and_then(Value::as_u64).map(|s| crate::ui::charts::duration(s as f64)).unwrap_or_default();
        let life = lifecycle_rows(t);
        let updates = vec![
            Some(life.update),
            row("updates.channel"),
            row("updates.feed_url"),
            Some(text("Version", format!("midna {} · midnad {daemon_v}", midna_proto::VERSION), t.fg, Some(t.ok), "midna info", Who::ReadOnly, None)),
            Some(text(
                "Daemon",
                if uptime.is_empty() { "not running".into() } else { format!("up {uptime} · pid {}", self.info.get("pid").and_then(Value::as_u64).unwrap_or(0)) },
                t.fg,
                None,
                "midna info",
                Who::ReadOnly,
                None,
            )),
        ];
        // Permissions (OS grants are human only; agents can't change them)
        let ax = accessibility_trusted();
        let perm = |name: &str, ok: Option<bool>, state: &str, why: &str, pane: &str, _key: &str| {
            let (color, dot) = match ok {
                Some(true) => (t.fg, t.ok),
                Some(false) => (t.need, t.need),
                None => (t.dim, t.dim),
            };
            RowSpec {
                label: name.into(),
                note: Some((why.into(), Hsla::default())),
                control: Control::Text {
                    dot: Some(dot),
                    text: state.into(),
                    color,
                    action: (ok != Some(true)).then(|| ((if ok == Some(false) { "Fix" } else { "Open" }).to_string(), Act::Url(pane.into()), ok == Some(false))),
                },
                // OS grants: nothing in midna can change them; this opens the same pane
                cli: format!("midna permissions open {}", if pane == PANE_AX { "accessibility" } else { "notifications" }),
                who: Who::Human,
                warn: ok == Some(false),
            }
        };
        let ax_row = perm(
            "Accessibility",
            Some(ax),
            if ax { "Granted" } else { "Missing" },
            "Lets Kass read and edit terminal input, and lets agents move windows.",
            PANE_AX,
            "permissions.accessibility",
        );
        use crate::notify::Permission as NP;
        let (n_ok, n_state, n_why) = match crate::notify::permission() {
            NP::Allowed => (Some(true), "Allowed", "midna shows approvals, failures and finished turns as macOS notifications."),
            NP::Denied => (Some(false), "Off in System Settings", "Turn midna's notifications on to get approvals, failures and finished turns."),
            NP::NotAsked => (None, "Not requested yet", "macOS asks the first time midna has something to tell you."),
            NP::Unknown => (None, "Checking…", "Approvals, failures and finished turns show as macOS notifications."),
            NP::Dev => (None, "Dev build", "Not running from Midna.app: notifications go through osascript (shown as Script Editor)."),
        };
        let perms = vec![ax_row, perm("Notifications", n_ok, n_state, n_why, PANE_NOTIF, "permissions.notifications"), life.login];
        let missing = perms.iter().filter(|r| r.warn).count();
        // Webhooks
        let wh_note = self
            .webhooks
            .get("public_url")
            .and_then(Value::as_str)
            .map(|u| format!("Public URL {u}"))
            .or_else(|| self.webhooks.get("message").and_then(Value::as_str).map(str::to_string));
        let mut wpath = row("webhooks.path");
        if let (Some(r), Some(n)) = (wpath.as_mut(), wh_note) {
            r.note = Some((n, Hsla::default()));
        }
        let webhooks = vec![wpath, row("webhooks.port"), row("webhooks.relay_url")];
        // Notifications: the master switch, each kind, then how they're shown. A terminal
        // overrides any of these with `midna notify set` (or mutes itself from its … menu).
        let mut notifications = vec![row("notify.enabled")];
        notifications.extend(midna_proto::notify::CATEGORIES.iter().map(|c| row(&midna_proto::notify::setting_key(c.key))));
        notifications.extend(["notify.turn_done_min_secs", "notify.when_focused", "notify.when_app_closed"].map(row));
        let notify_off = self.value("notify.enabled") == Value::Bool(false);
        let mut notifications: Vec<RowSpec> = notifications.into_iter().flatten().collect();
        // Sounds: on/off and in-app first, then every notification kind; effects get their own group.
        let mut sounds: Vec<RowSpec> = ["notify.sounds", "notify.sounds_in_app"].iter().filter_map(|k| self.spec_row(k)).collect();
        let switches = sounds.len();
        sounds.extend(self.notify_sound_rows(t));
        let effects = self.sound_effect_rows(t);
        let mut images = self.notify_image_rows(t);
        if notify_off {
            for r in notifications.iter_mut().skip(1).chain(sounds.iter_mut().skip(switches + 1)).chain(images.iter_mut()) {
                r.note = Some(("No effect while notifications are off".into(), t.dim));
            }
        }
        // Look
        let system = self.value("theme") == json!("system");
        let look = vec![row("theme"), row("theme.dark").filter(|_| system), row("theme.light").filter(|_| system), row("theme.colors"), row("density"), row("ui.header.script"), row("ui.row.script"), row("ui.status.script"), row("ui.status.items")];
        // Terminal: link previews (the card a hovered path or link opens)
        let mut terminal: Vec<RowSpec> = [row("terminal.link_preview"), row("terminal.preview_path_click")].into_iter().flatten().collect();
        if self.value("terminal.link_preview") == json!("off")
            && let Some(r) = terminal.get_mut(1)
        {
            r.note = Some(("No effect while link previews are off".into(), t.dim));
        }
        // Agents
        let agents = vec![
            Some(life.cli),
            Some(self.hooks_row(t, "claude")),
            Some(self.hooks_row(t, "codex")),
            row("agents.claude.statusline"),
            row("agents.mcp"),
            row("agents.system_hint"),
            row("ui.ask.agent"),
            row("ui.ask.scope"),
            row("kass.auto_send"),
            Some(RowSpec {
                label: "Kass".into(),
                note: Some((
                    if crate::kass::handshake_detected() {
                        "Dictation opens the composer under the terminal; Kass inserts into it directly.".into()
                    } else {
                        "Dictation composer: needs the Kass handshake (com.mrgnhnt.kass.dictationWillBegin). Detected once Kass sends one.".into()
                    },
                    Hsla::default(),
                )),
                control: if crate::kass::handshake_detected() {
                    Control::Text { dot: Some(t.ok), text: "Handshake detected".into(), color: t.fg, action: None }
                } else {
                    Control::Text { dot: Some(t.dim), text: "Handshake not detected".into(), color: t.dim, action: None }
                },
                cli: "midna info".into(),
                who: Who::ReadOnly,
                warn: false,
            }),
        ];
        // What agents may change without asking (human-only switches)
        let allow = vec![
            row("agents.may_move_windows"),
            row("agents.may_close_idle"),
            row("agents.may_force_close"),
            row("approve.from_cli"),
            row("policy.default"),
            row("policy.request_timeout_secs"),
            row("git.refresh_secs"),
        ];
        let mut allow: Vec<RowSpec> = allow.into_iter().flatten().collect();
        if !ax && let Some(r) = allow.iter_mut().find(|r| r.label == label_for("agents.may_move_windows")) {
            r.note = Some(("No effect until Accessibility is granted".into(), t.need));
        }
        let changed = self.entries.iter().filter(|e| e.value != e.default).count();
        let danger = vec![
            RowSpec {
                label: "Reset settings".into(),
                note: Some(("Every setting back to its default. Terminals, rules and the event log are kept.".into(), Hsla::default())),
                control: Control::Text {
                    dot: Some(t.err),
                    text: if self.armed_reset {
                        format!("Resets {changed} changed setting{}. Click again to confirm.", if changed == 1 { "" } else { "s" })
                    } else {
                        format!("{changed} changed from default")
                    },
                    color: if self.armed_reset { t.err } else { t.dim },
                    action: Some((if self.armed_reset { "Confirm".into() } else { "Reset…".into() }, Act::ResetSettings, true)),
                },
                cli: "midna settings reset <key>".into(),
                who: Who::Human,
                warn: false,
            },
            RowSpec {
                label: "Reset midna".into(),
                note: Some((
                    "Closes every terminal, removes projects, triggers and needs-you items, and resets settings. Rules and the event log are kept.".into(),
                    Hsla::default(),
                )),
                control: Control::Text {
                    dot: Some(t.err),
                    text: if self.armed_daemon_reset { "Closes every terminal now. Click again to confirm.".into() } else { "Start over".into() },
                    color: if self.armed_daemon_reset { t.err } else { t.dim },
                    action: Some((if self.armed_daemon_reset { "Confirm".into() } else { "Reset…".into() }, Act::ResetDaemon, true)),
                },
                cli: "midna daemon reset".into(),
                who: Who::Human,
                warn: false,
            },
        ];
        vec![
            Group { name: "Updates", danger: false, badge: 0, rows: updates.into_iter().flatten().collect() },
            Group { name: "Permissions", danger: false, badge: missing, rows: perms },
            Group { name: "Webhooks", danger: false, badge: 0, rows: webhooks.into_iter().flatten().collect() },
            Group { name: "Notifications", danger: false, badge: 0, rows: notifications },
            Group { name: "Sounds", danger: false, badge: 0, rows: sounds },
            Group { name: "Sound effects", danger: false, badge: 0, rows: effects },
            Group { name: "Notification images", danger: false, badge: 0, rows: images },
            Group { name: "Look", danger: false, badge: 0, rows: look.into_iter().flatten().collect() },
            Group { name: "Terminal", danger: false, badge: 0, rows: terminal },
            Group { name: "Projects", danger: false, badge: 0, rows: row("projects.roots").into_iter().collect() },
            Group { name: "Agents", danger: false, badge: 0, rows: agents.into_iter().flatten().collect() },
            Group { name: "What agents may do without asking", danger: false, badge: 0, rows: allow },
            Group { name: "Danger zone", danger: true, badge: 0, rows: danger },
        ]
    }
}

struct LifeRows {
    update: RowSpec,
    login: RowSpec,
    cli: RowSpec,
}

/// Rows for the install / update state (lifecycle.rs).
fn lifecycle_rows(t: &Theme) -> LifeRows {
    use crate::install::{CliLink, LoginItem, Mode};
    use crate::lifecycle::{Cmd, UpdateState};
    let snap = crate::lifecycle::snapshot();
    let dev = snap.as_ref().is_none_or(|s| s.mode == Mode::Dev);
    let row = |label: &str, dot: Hsla, text: String, color: Hsla, action: Option<(String, Act, bool)>, note: String, cli: &str, who: Who, warn: bool| RowSpec {
        label: label.into(),
        note: Some((note, Hsla::default())),
        control: Control::Text { dot: Some(dot), text, color, action },
        cli: cli.into(),
        who,
        warn,
    };
    let update = match snap.as_ref().map(|s| &s.update) {
        Some(UpdateState::Ready { version, notes, .. }) => row(
            "Update",
            t.accent,
            format!("{version} ready"),
            t.fg,
            Some(("Restart to apply".into(), Act::Life(Cmd::Apply), true)),
            if notes.is_empty() { "Downloaded and verified. Terminals keep running through the restart.".into() } else { notes.clone() },
            "midna updates install",
            Who::Human,
            false,
        ),
        Some(UpdateState::Applying { version }) => {
            row("Update", t.accent, format!("Installing {version}…"), t.fg, None, "midna restarts in a moment.".into(), "midna updates check", Who::Human, false)
        }
        Some(UpdateState::Downloading { version }) => {
            row("Update", t.dim, format!("Downloading {version}…"), t.dim, None, "Verified before it's offered.".into(), "midna updates check", Who::Human, false)
        }
        Some(UpdateState::Checking) => row("Update", t.dim, "Checking…".into(), t.dim, None, "Checks every 6 hours.".into(), "midna updates check", Who::Human, false),
        Some(UpdateState::UpToDate { at }) => row(
            "Update",
            t.ok,
            format!("Up to date · checked {}", ago(*at)),
            t.fg,
            Some(("Check now".into(), Act::Life(Cmd::CheckNow), false)),
            "Checks every 6 hours.".into(),
            "midna updates check",
            Who::Human,
            false,
        ),
        Some(UpdateState::Failed { error, at }) => row(
            "Update",
            t.need,
            format!("Check failed {}", ago(*at)),
            t.need,
            Some(("Retry".into(), Act::Life(Cmd::CheckNow), false)),
            error.clone(),
            "midna updates check",
            Who::Human,
            false,
        ),
        Some(UpdateState::Off(why)) => row("Update", t.dim, "Off".into(), t.dim, None, capitalize(why), "midna updates check", Who::ReadOnly, false),
        Some(UpdateState::Idle) | None => row(
            "Update",
            t.dim,
            "Not checked yet".into(),
            t.dim,
            Some(("Check now".into(), Act::Life(Cmd::CheckNow), false)),
            "Checks every 6 hours.".into(),
            "midna updates check",
            Who::Human,
            false,
        ),
    };
    let why_login = "Starts midnad at login so shells survive a restart.";
    let login = match snap.as_ref().map(|s| &s.login) {
        _ if dev => row("Login item", t.dim, "Dev build: midnad started directly".into(), t.dim, None, why_login.into(), "midna permissions open login-items", Who::Human, false),
        Some(LoginItem::Enabled) | Some(LoginItem::Legacy) => {
            row("Login item", t.ok, "On".into(), t.fg, None, why_login.into(), "midna permissions open login-items", Who::Human, false)
        }
        Some(LoginItem::RequiresApproval) => row(
            "Login item",
            t.need,
            "Needs your OK in Login Items".into(),
            t.need,
            Some(("Fix".into(), Act::Life(Cmd::OpenLoginItems), true)),
            "Switch midna on under System Settings ▸ General ▸ Login Items ▸ Allow in the Background.".into(),
            "midna permissions open login-items",
            Who::Human,
            true,
        ),
        Some(LoginItem::Checking) | None => row("Login item", t.dim, "Checking…".into(), t.dim, None, why_login.into(), "", Who::Human, false),
        Some(LoginItem::NotRegistered) => {
            row("Login item", t.need, "Not installed".into(), t.need, Some(("Fix".into(), Act::Life(Cmd::Register), true)), why_login.into(), "", Who::Human, true)
        }
        Some(LoginItem::Failed(e)) => {
            row("Login item", t.err, "Couldn't register".into(), t.err, Some(("Retry".into(), Act::Life(Cmd::Register), true)), e.clone(), "", Who::Human, true)
        }
        Some(LoginItem::Dev) => row("Login item", t.dim, "Dev build: midnad started directly".into(), t.dim, None, why_login.into(), "", Who::Human, false),
    };
    let why_cli = "The midna command for agents outside midna (midna terminals already have it on PATH).";
    let cli = match snap.as_ref().map(|s| &s.cli) {
        _ if dev => row("midna CLI", t.dim, "Dev build: target/debug/midna".into(), t.dim, None, why_cli.into(), "midna --help", Who::Human, false),
        Some(CliLink::Linked { link }) => row("midna CLI", t.ok, link.display().to_string(), t.fg, None, why_cli.into(), "midna --help", Who::Human, false),
        Some(CliLink::NotOnPath { dir }) => {
            let installed = std::fs::read_link(dir.join("midna")).is_ok();
            row(
                "midna CLI",
                t.need,
                if installed { format!("In {} (not on PATH)", dir.display()) } else { "Not installed".into() },
                t.need,
                (!installed).then(|| (format!("Install to {}", dir.display()), Act::Life(Cmd::InstallCli), false)),
                format!("{} isn't on your shell's PATH. Add it yourself (midna never edits shell files): export PATH=\"{}:$PATH\"", dir.display(), dir.display()),
                "midna --help",
                Who::Human,
                false,
            )
        }
        Some(CliLink::Conflict { link }) => row(
            "midna CLI",
            t.need,
            format!("{} is something else", link.display()),
            t.need,
            None,
            "Left alone. Remove it to let midna link its CLI there.".into(),
            "",
            Who::Human,
            false,
        ),
        Some(CliLink::Failed(e)) => {
            row("midna CLI", t.err, "Couldn't link".into(), t.err, Some(("Retry".into(), Act::Life(Cmd::InstallCli), false)), e.clone(), "", Who::Human, false)
        }
        Some(CliLink::Dev) | None => row("midna CLI", t.dim, "Checking…".into(), t.dim, None, why_cli.into(), "", Who::Human, false),
    };
    LifeRows { update, login, cli }
}

/// Placeholder color for plain text values (resolved to `t.fg` at render).
fn cx_dim_placeholder() -> Hsla {
    Hsla::default()
}

// ------------------------------------------------------------------ render

const COL_SETTING: f32 = 210.;
const COL_VALUE: f32 = 250.;
const COL_WHO: f32 = 96.;

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let body: AnyElement = match self.view {
            View::Rows => self.rows(&t, cx).into_any_element(),
            View::Json => self.json(&t).into_any_element(),
            View::Shortcuts => self.shortcuts(&t, window, cx).into_any_element(),
        };
        div()
            .id("settings-root")
            .track_focus(&self.focus)
            .key_context("MidnaSettings")
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(13.))
            .line_height(px(13. * 1.45))
            .child(self.title_bar(&t, cx))
            .child(self.ask_bar(&t, window, cx))
            .when_some(self.error.clone(), |d, e| {
                d.child(div().px(px(18.)).py(px(8.)).bg(t.need_soft).text_color(t.need).text_size(px(12.)).child(format!("midnad: {e} — showing the catalog defaults.")))
            })
            .child(body)
            .child(self.footer(&t))
            .when(self.picker.is_some(), |d| {
                // click-away layer under an open sound/image picker
                d.child(
                    deferred(div().id("picker-dismiss").absolute().top_0().left_0().size_full().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|s, _, _, cx| {
                            s.picker = None;
                            cx.notify();
                        }),
                    ))
                    .with_priority(1),
                )
            })
    }
}

impl SettingsWindow {
    fn title_bar(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut tabs = div().flex().p(px(2.)).gap(px(2.)).rounded(px(7.)).border_1().border_color(t.line).bg(t.bg);
        for (i, (v, label)) in [(View::Rows, "Rows"), (View::Json, "settings.json"), (View::Shortcuts, "Shortcuts")].into_iter().enumerate() {
            let on = self.view == v;
            let keys = ["⌘1", "⌘2", "⌘3"][i];
            tabs = tabs.child(
                div()
                    .id(label)
                    .h(px(24.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(12.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::BOLD))
                    .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                    .tooltip(crate::ui::header::tip_fixed(label, keys))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |s, _, _, cx| s.show(v, cx)))
                    .child(label),
            );
        }
        div()
            .id("settings-titlebar")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .h(px(46.))
            .px(px(14.))
            .border_b_1()
            .border_color(t.line)
            .bg(t.panel)
            .on_mouse_down(MouseButton::Left, |ev, window, _| {
                if ev.click_count >= 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(div().absolute().top_0().left_0().size_full().flex().items_center().justify_center().text_size(px(13.)).font_weight(FontWeight::BOLD).child("Settings"))
            .child(div().flex_1())
            .child(tabs)
    }

    fn show(&mut self, v: View, cx: &mut Context<Self>) {
        self.view = v;
        if v != View::Shortcuts {
            self.recorder = None;
            self.editing = None;
            self.shortcut_menu = None;
        }
        cx.notify();
    }

    /// The window's own keys (listed under Built in on the Shortcuts tab). The key detector
    /// takes keys before this while it's on.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if !m.platform || m.control || m.shift {
            return;
        }
        match (ks.key.as_str(), m.alt) {
            ("1", false) => self.show(View::Rows, cx),
            ("2", false) => self.show(View::Json, cx),
            ("3", false) => self.show(View::Shortcuts, cx),
            ("k", false) => {
                self.show(View::Rows, cx);
                self.ask.focus.focus(window, cx);
            }
            ("f", false) => {
                self.show(View::Shortcuts, cx);
                self.recorded.clear();
                self.search.focus.focus(window, cx);
            }
            ("k", true) => self.toggle_recording(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn ask_bar(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.ask.focus.is_focused(window);
        div().flex().flex_none().items_center().gap(px(10.)).px(px(16.)).py(px(10.)).border_b_1().border_color(t.line).child(
            div()
                .id("ask")
                .flex_1()
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(34.))
                .pl(px(12.))
                .pr(px(6.))
                .rounded(px(9.))
                .border_1()
                .border_color(t.accent)
                .when(focused, |d| d.shadow(vec![BoxShadow { color: t.accent_soft, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }]))
                .bg(t.raised)
                .cursor_text()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|s, _, window, cx| {
                        s.ask.focus.focus(window, cx);
                        cx.notify();
                    }),
                )
                .on_key_down(cx.listener(|s, ev: &KeyDownEvent, window, cx| match s.ask.on_key(ev, cx) {
                    KeyOutcome::Submit => s.submit_ask(cx),
                    KeyOutcome::Cancel => {
                        s.ask.clear(cx);
                        s.focus.focus(window, cx);
                        cx.notify();
                    }
                    KeyOutcome::Ignored => cx.propagate(),
                }))
                .child(div().font_family(t.mono_font.clone()).text_color(t.accent).child("❯"))
                .child(div().flex_1().min_w_0().flex().items_center().overflow_hidden().text_color(t.fg).child(self.ask.field.clone()))
                .child(
                    div()
                        .id("ask-go")
                        .h(px(24.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .bg(t.accent)
                        .text_color(t.accent_fg)
                        .text_size(px(12.))
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|s, _, _, cx| s.submit_ask(cx)))
                        .child("Ask ↩"),
                ),
        )
    }

    fn rows(&self, t: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let head = div()
            .flex()
            .flex_none()
            .gap(px(16.))
            .px(px(18.))
            .py(px(8.))
            .bg(t.bg)
            .border_b_1()
            .border_color(t.line)
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .text_color(t.dim)
            .child(div().w(px(COL_SETTING)).flex_none().child("SETTING"))
            .child(div().w(px(COL_VALUE)).flex_none().child("VALUE"))
            .child(div().flex_1().min_w_0().child("SAME THING, FOR AGENTS"))
            .child(div().w(px(COL_WHO)).flex_none().child("WHO CAN SET"));
        let mut list = div().id("settings-rows").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).flex().flex_col().pb(px(16.));
        for (gi, g) in self.groups(t).into_iter().enumerate() {
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(18.))
                    .pt(px(16.))
                    .pb(px(4.))
                    .child(div().font_weight(FontWeight::BOLD).text_color(if g.danger { t.err } else { t.fg }).child(g.name))
                    .when(g.badge > 0, |d| {
                        d.child(
                            div()
                                .min_w(px(18.))
                                .h(px(18.))
                                .px(px(5.))
                                .rounded(px(9.))
                                .bg(t.need)
                                .text_color(t.badge_fg)
                                .text_size(px(11.))
                                .font_weight(FontWeight::BOLD)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(g.badge.to_string()),
                        )
                    }),
            );
            for (ri, r) in g.rows.into_iter().enumerate() {
                list = list.child(self.row(t, r, gi * 100 + ri, cx));
            }
        }
        div().flex_1().min_h_0().flex().flex_col().child(head).child(list)
    }

    fn row(&self, t: &Theme, r: RowSpec, id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let note = r.note.map(|(n, c)| (n, if c == Hsla::default() { t.dim } else { c }));
        let control: AnyElement = match r.control {
            Control::Seg { key, options, current } => {
                let mut seg = div().flex().flex_wrap().p(px(2.)).gap(px(1.)).rounded(px(7.)).border_1().border_color(t.line).bg(t.panel);
                for (i, (v, label)) in options.into_iter().enumerate() {
                    let on = v == current;
                    let k = key.clone();
                    seg = seg.child(
                        div()
                            .id(SharedString::from(format!("seg-{key}-{i}")))
                            .h(px(24.))
                            .px(px(9.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_size(px(12.))
                            .whitespace_nowrap()
                            .cursor_pointer()
                            .when(on, |d| {
                                d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::BOLD).shadow(vec![BoxShadow {
                                    color: hsla(0., 0., 0., 0.2),
                                    offset: point(px(0.), px(1.)),
                                    blur_radius: px(2.),
                                    spread_radius: px(0.),
                                    inset: false,
                                }])
                            })
                            .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if !on {
                                    s.set(&k, json!(v), cx);
                                }
                            }))
                            .child(label),
                    );
                }
                seg.into_any_element()
            }
            Control::Switch { key, on, on_text, off_text } => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    div()
                        .id(SharedString::from(format!("sw-{key}")))
                        .relative()
                        .w(px(36.))
                        .h(px(20.))
                        .rounded(px(10.))
                        .bg(if on { t.accent } else { t.line })
                        .cursor_pointer()
                        .on_click(cx.listener(move |s, _, _, cx| s.set(&key, json!(!on), cx)))
                        .child(div().absolute().top(px(2.)).left(px(if on { 18. } else { 2. })).size(px(16.)).rounded_full().bg(gpui_kit::white())),
                )
                .child(div().text_color(t.dim).child(if on { on_text } else { off_text }))
                .into_any_element(),
            Control::Sound { cat } => self.sound_control(t, cat, cx),
            Control::Theme { key, current } => self.theme_control(t, key, current, cx).into_any_element(),
            Control::Volume { key } => self.volume_stepper(t, &key, None, cx).into_any_element(),
            Control::Image { key, cat } => self.image_control(t, &key, cat, cx),
            Control::Text { dot, text, color, action } => {
                let color = if color == Hsla::default() { t.fg } else { color };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .when_some(dot, |d, c| d.child(div().size(px(7.)).rounded_full().flex_none().bg(c)))
                    .child(div().flex_1().min_w_0().text_color(color).truncate().child(text))
                    .when_some(action, |d, (label, act, strong)| {
                        let danger = matches!(act, Act::ResetSettings | Act::ResetDaemon);
                        let armed = match act {
                            Act::ResetSettings => self.armed_reset,
                            Act::ResetDaemon => self.armed_daemon_reset,
                            _ => false,
                        };
                        d.child(
                            div()
                                .id(SharedString::from(format!("act-{id}")))
                                .flex_none()
                                .h(px(26.))
                                .px(px(10.))
                                .flex()
                                .items_center()
                                .rounded(px(7.))
                                .text_size(px(12.))
                                .font_weight(FontWeight::BOLD)
                                .cursor_pointer()
                                .map(|b| {
                                    if danger {
                                        b.border_1().border_color(t.err).bg(if armed { t.err } else { gpui_kit::transparent_black() }).text_color(if armed {
                                            gpui_kit::white()
                                        } else {
                                            t.err
                                        })
                                    } else if strong {
                                        b.bg(t.accent).text_color(t.accent_fg)
                                    } else {
                                        b.border_1().border_color(t.line).text_color(t.fg).hover(|s| s.bg(t.raised))
                                    }
                                })
                                .on_click(cx.listener(move |s, _, _, cx| match &act {
                                    Act::Url(u) => cx.open_url(u),
                                    Act::ResetSettings => {
                                        if s.armed_reset {
                                            s.reset_all(cx);
                                        } else {
                                            s.armed_reset = true;
                                            s.armed_daemon_reset = false;
                                        }
                                        cx.notify();
                                    }
                                    Act::ResetDaemon => {
                                        if s.armed_daemon_reset {
                                            s.reset_daemon(cx);
                                        } else {
                                            s.armed_daemon_reset = true;
                                            s.armed_reset = false;
                                        }
                                        cx.notify();
                                    }
                                    Act::Life(c) => crate::lifecycle::command(c.clone(), cx),
                                    &Act::Hooks { uninstall } => crate::windows::with_active(cx, |m, window, cx| {
                                        window.activate_window();
                                        crate::ui::hooks::open(m, uninstall, window, cx);
                                    }),
                                }))
                                .child(label),
                        )
                    })
                    .into_any_element()
            }
        };
        let cli = r.cli.clone();
        let copied = self.copied.as_ref().is_some_and(|(c, _)| *c == r.cli);
        let (who_text, who_color) = match r.who {
            Who::Agents => ("agents too", t.dim),
            Who::Human => ("human only", t.fg),
            Who::ReadOnly => ("read-only", t.dim),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .min_h(px(40.))
            .px(px(18.))
            .py(px(5.))
            .border_b_1()
            .border_color(t.line)
            .when(r.warn, |d| d.bg(t.need_soft))
            .child(
                div()
                    .w(px(COL_SETTING))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(div().child(r.label))
                    .when_some(note, |d, (n, c)| d.child(div().text_size(px(11.5)).line_height(px(15.)).text_color(c).child(n))),
            )
            .child(div().w(px(COL_VALUE)).flex_none().min_w_0().flex().items_center().child(control))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(t.mono_font.clone())
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(t.dim)
                            .flex()
                            .gap(px(6.))
                            .child(div().flex_none().text_color(t.accent).child("$"))
                            .child(div().flex_1().min_w_0().child(r.cli.clone())),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("copy-{id}")))
                            .flex_none()
                            .h(px(22.))
                            .px(px(6.))
                            .flex()
                            .items_center()
                            .rounded(px(5.))
                            .text_size(px(11.))
                            .text_color(if copied { t.ok } else { t.dim })
                            .cursor_pointer()
                            .hover(|s| s.bg(t.raised).text_color(t.fg))
                            .on_click(cx.listener(move |s, _, _, cx| s.copy(cli.clone(), cx)))
                            .child(if copied { "Copied" } else { "Copy" }),
                    ),
            )
            .child(
                div()
                    .w(px(COL_WHO))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .text_size(px(11.5))
                    .text_color(who_color)
                    .when(r.who == Who::Human, |d| d.child(Icon::Lock.el(12., who_color)))
                    .child(who_text),
            )
    }

    /// Read-only annotated JSON: what `midna settings list --json` holds, grouped by prefix.
    fn json(&self, t: &Theme) -> impl IntoElement {
        let mut lines: Vec<(String, String, String, Hsla, String, Hsla)> = vec![]; // indent, key, value, value color, comment, comment color
        let mut push =
            |ind: &str, k: &str, v: String, vc: Hsla, c: String, cc: Hsla| lines.push((ind.into(), if k.is_empty() { String::new() } else { format!("\"{k}\": ") }, v, vc, c, cc));
        push("", "", "{".into(), t.fg, format!("// what `midna settings list --json` returns · read-only here · {} settings", SETTINGS.len()), t.dim);
        let mut prefixes: Vec<&str> = vec![];
        for s in SETTINGS {
            let p = s.key.split_once('.').map(|(p, _)| p).unwrap_or("");
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
        }
        for p in prefixes {
            let group: Vec<_> = SETTINGS.iter().filter(|s| s.key.split_once('.').map(|(x, _)| x).unwrap_or("") == p).collect();
            let ind = if p.is_empty() { "  " } else { "    " };
            if !p.is_empty() {
                push("  ", p, "{".into(), t.fg, String::new(), t.dim);
            }
            for s in group {
                let v = self.value(s.key);
                let vs = match &v {
                    Value::String(x) => format!("\"{x}\","),
                    v => format!("{v},"),
                };
                let vc = match v {
                    Value::Bool(_) | Value::Number(_) => t.work,
                    _ => t.ok,
                };
                let opts = match s.ty {
                    SettingKind::Enum { options, allow_other } => format!("{}{}", options.join(" | "), if allow_other { " | <path>" } else { "" }),
                    SettingKind::Bool => "true | false".into(),
                    SettingKind::Int => "integer".into(),
                    SettingKind::Keybinding => "keys".into(),
                    SettingKind::String => "string".into(),
                    SettingKind::PathList => "[paths]".into(),
                    SettingKind::RuleList => "[\"match = value\"]".into(),
                    SettingKind::ItemList { options, allow_paths } => format!("[{}{}]", options.join(" | "), if allow_paths { " | <path>" } else { "" }),
                };
                let (c, cc) = if s.human_only { (format!("// {opts} · human only: agents get a needs-you confirmation"), t.fg) } else { (format!("// {opts}"), t.dim) };
                let k = s.key.split_once('.').map(|(_, r)| r).filter(|_| !p.is_empty()).unwrap_or(s.key);
                push(ind, k, vs, vc, c, cc);
            }
            if !p.is_empty() {
                push("  ", "", "},".into(), t.fg, String::new(), t.dim);
            }
        }
        let ax = accessibility_trusted();
        push("  ", "status", "{".into(), t.fg, "// read-only · from midnad and macOS, not settings".into(), t.fg);
        push("    ", "version", format!("\"{}\",", self.info.get("version").and_then(Value::as_str).unwrap_or("?")), t.ok, String::new(), t.dim);
        push(
            "    ",
            "permissions.accessibility",
            format!("\"{}\",", if ax { "granted" } else { "missing" }),
            if ax { t.ok } else { t.need },
            if ax { String::new() } else { "// fix in System Settings".into() },
            t.need,
        );
        let kass_seen = crate::kass::handshake_detected();
        for agent in ["claude", "codex"] {
            let st = self.hooks[agent]["state"].as_str().unwrap_or("unknown");
            let c = match st {
                "current" => t.ok,
                "stale" => t.need,
                "error" => t.err,
                _ => t.dim,
            };
            push("    ", &format!("hooks.{agent}"), format!("\"{st}\","), c, if st == "stale" || st == "error" { "// Settings ▸ Agents to reinstall".into() } else { String::new() }, t.need);
        }
        push("    ", "kass", if kass_seen { "\"handshake_detected\"" } else { "\"handshake_not_detected\"" }.into(), if kass_seen { t.ok } else { t.dim }, String::new(), t.dim);
        push("  ", "", "}".into(), t.fg, String::new(), t.dim);
        push("", "", "}".into(), t.fg, String::new(), t.dim);
        let mut col = div()
            .id("settings-json")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .bg(t.term)
            .px(px(20.))
            .py(px(14.))
            .font_family(t.mono_font.clone())
            .text_size(px(12.5))
            .line_height(px(20.));
        for (ind, k, v, vc, c, cc) in lines {
            col = col.child(
                div()
                    .flex()
                    .gap(px(24.))
                    .whitespace_nowrap()
                    .child(div().flex().flex_none().child(div().text_color(t.dim).child(ind)).child(div().text_color(t.accent).child(k)).child(div().text_color(vc).child(v)))
                    .child(div().text_color(cc).truncate().child(c)),
            );
        }
        col
    }

    fn footer(&self, t: &Theme) -> impl IntoElement {
        let (cmd, result, color, who) = match &self.last {
            Some(l) => (l.cmd.clone(), l.result.clone(), if l.ok { t.ok } else { t.err }, format!("{} · {}", l.who, ago(l.at))),
            None => (
                "midna settings list".to_string(),
                format!("{} settings", self.entries.len().max(SETTINGS.len())),
                t.dim,
                "live: changes from agents and the CLI show up here".to_string(),
            ),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .min_h(px(40.))
            .px(px(18.))
            .border_t_1()
            .border_color(t.line)
            .bg(t.term)
            .font_family(t.mono_font.clone())
            .text_size(px(12.))
            .child(div().text_color(t.accent).child("$"))
            .child(div().text_color(t.fg).truncate().child(cmd))
            .child(div().flex_none().text_color(color).child(result))
            .child(div().flex_1())
            .child(div().flex_none().font_family(t.ui_font.clone()).text_color(t.dim).child(who))
    }
}

fn ago(at: Instant) -> String {
    let s = at.elapsed().as_secs();
    match s {
        0..=9 => "just now".into(),
        10..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        _ => format!("{}h ago", s / 3600),
    }
}

/// Dev: `MIDNA_SETTINGS_SNAPSHOT=out.png` renders this window offscreen once it has data.
#[cfg(feature = "snapshot")]
fn snapshot(handle: WindowHandle<SettingsWindow>, cx: &mut App) {
    let Ok(path) = std::env::var("MIDNA_SETTINGS_SNAPSHOT") else {
        return;
    };
    let any: AnyWindowHandle = handle.into();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(2000)).await;
        for _ in 0..2 {
            let _ = cx.update_window(any, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            cx.background_executor().timer(Duration::from_millis(300)).await;
        }
        let _ = cx.update_window(any, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            match window.render_to_image() {
                Ok(img) => match img.save(&path) {
                    Ok(()) => eprintln!("midna-app: settings snapshot saved to {path}"),
                    Err(e) => eprintln!("midna-app: settings snapshot save failed: {e}"),
                },
                Err(e) => eprintln!("midna-app: render_to_image failed: {e:#}"),
            }
        });
        let _ = cx.update(|cx| cx.dispatch_action(&crate::actions::Quit));
    })
    .detach();
}
