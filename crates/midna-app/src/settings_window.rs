//! Settings: its own standard macOS window. A sidebar of sections (General … System) beside
//! the selected section's rows, grouped in cards. A row is the setting's name, what it does
//! and its control; a lock marks the ones only the human can change. "Agent commands" adds
//! under every row the exact CLI an agent would run (copyable). Data is `settings.list` + the
//! `midna_proto::settings::SETTINGS` catalog, live-updated on `settings.changed` (agents
//! change settings at any time). `LAYOUT` says where every setting lives.
//!
//! The search field filters both panes: the sidebar keeps the sections with matches (with
//! counts) and the main pane lists every match as Section › Group, the matched text marked,
//! with a line saying why when the match isn't in the name or description. ↩ opens an agent
//! with the text as its prompt. The GUI is the human, so human-only settings are editable here.
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
    let w: f32 = std::env::var("MIDNA_SETTINGS_W").ok().and_then(|v| v.parse().ok()).unwrap_or(940.);
    let h: f32 = std::env::var("MIDNA_SETTINGS_H").ok().and_then(|v| v.parse().ok()).unwrap_or(680.);
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
        titlebar: Some(TitlebarOptions { title: Some("Settings".into()), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(17.))) }),
        window_min_size: Some(size(px(760.), px(440.))),
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

/// The sidebar's sections, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sec {
    General,
    Appearance,
    Terminal,
    Shortcuts,
    Agents,
    Limits,
    Notifications,
    Sounds,
    Webhooks,
    System,
}

const SECS: [Sec; 10] = [Sec::General, Sec::Appearance, Sec::Terminal, Sec::Shortcuts, Sec::Agents, Sec::Limits, Sec::Notifications, Sec::Sounds, Sec::Webhooks, Sec::System];

impl Sec {
    fn label(self) -> &'static str {
        match self {
            Sec::General => "General",
            Sec::Appearance => "Appearance",
            Sec::Terminal => "Terminal",
            Sec::Shortcuts => "Shortcuts",
            Sec::Agents => "Agents",
            Sec::Limits => "Agent limits",
            Sec::Notifications => "Notifications",
            Sec::Sounds => "Sounds",
            Sec::Webhooks => "Webhooks",
            Sec::System => "System",
        }
    }

    fn about(self) -> &'static str {
        match self {
            Sec::General => "Updates, the ⌘K Ask box, and where projects open.",
            Sec::Appearance => "Theme, density and what the bars show.",
            Sec::Terminal => "How terminals behave under your hands.",
            Sec::Shortcuts => "Click a shortcut's keys, then press new ones. Right-click for more.",
            Sec::Agents => "How Claude and Codex report to midna.",
            Sec::Limits => "What agents may do without asking. Only you can change these.",
            Sec::Notifications => "What tells you, and how. A terminal can override any of these.",
            Sec::Sounds => "What each notification and action sounds like.",
            Sec::Webhooks => "How GitHub and Bitbucket events reach midna to start triggers.",
            Sec::System => "macOS permissions, the daemon, and starting over.",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Sec::General => Icon::Settings,
            Sec::Appearance => Icon::Moon,
            Sec::Terminal => Icon::Shell,
            Sec::Shortcuts => Icon::Keyboard,
            Sec::Agents => Icon::Orbit,
            Sec::Limits => Icon::Rules,
            Sec::Notifications => Icon::Bell,
            Sec::Sounds => Icon::Play,
            Sec::Webhooks => Icon::Globe,
            Sec::System => Icon::Screen,
        }
    }

    /// A gap above it in the sidebar: the agent sections, then the system ones.
    fn gap(self) -> bool {
        matches!(self, Sec::Agents | Sec::Webhooks)
    }

    /// For `MIDNA_SETTINGS_VIEW`.
    fn id(self) -> String {
        self.label().to_lowercase().replace(' ', "_")
    }
}

/// Where every setting lives: section, group heading ("" for none), then its rows in order.
/// `@name` is a row that isn't one setting (a status, a button, or one row per notification
/// kind); see `special_rows`. A test checks every catalog setting has a place.
const LAYOUT: &[(Sec, &str, &[&str])] = &[
    (Sec::General, "Updates", &["@update", "updates.channel", "updates.feed_url"]),
    (Sec::General, "⌘K Ask an agent", &["ui.ask.agent", "ui.ask.scope"]),
    (Sec::General, "Projects and windows", &["projects.roots", "ide.app", "ide.rules", "windows.close_with_terminals", "finder.quick_action"]),
    (Sec::Appearance, "Theme", &["theme", "theme.dark", "theme.light", "theme.colors", "density", "ui.haptics"]),
    (Sec::Appearance, "Bars", &["ui.header.script", "ui.header.buttons", "ui.row.script", "ui.status.script", "ui.status.items", "ui.status.looks", "git.refresh_secs"]),
    (Sec::Appearance, "Sidebar footer", &["sidebar.footer.stats", "sidebar.footer.range", "sidebar.footer.buttons", "sidebar.footer.compact"]),
    (Sec::Appearance, "Insights", &["insights.layout", "insights.layouts", "insights.colors.agents", "insights.colors.you", "insights.colors.waiting"]),
    (Sec::Terminal, "Names", &["terminal.auto_name", "terminal.auto_name_updates"]),
    (Sec::Terminal, "Links and paths", &["terminal.link_preview", "terminal.preview_path_click"]),
    (Sec::Terminal, "Pasting", &["terminal.image_paste"]),
    (Sec::Terminal, "Keyboard", &["terminal.option_as_meta"]),
    (Sec::Terminal, "Agents", &["terminal.prompt_bar"]),
    (Sec::Shortcuts, "", &["@shortcuts"]),
    (Sec::Shortcuts, "Built in", &["@built_in"]),
    (Sec::Agents, "Connection", &["@cli", "@hooks.claude", "@hooks.codex", "agents.mcp", "agents.claude.statusline", "agents.system_hint"]),
    (Sec::Agents, "Lifecycle", &["agents.restart_on_update", "agents.restart_idle_secs", "agents.adopt_typed", "agents.resume_after_sleep", "agents.resume_after_network", "agents.resume_after_sleep_prompt"]),
    (Sec::Agents, "Kass dictation", &["@kass", "kass.auto_send"]),
    (
        Sec::Limits,
        "Without asking, agents may",
        &["agents.may_move_windows", "agents.may_close_idle", "agents.may_force_close", "agents.may_install_updates", "approve.from_cli", "agents.trust_folders"],
    ),
    (Sec::Limits, "Policy", &["policy.default", "policy.request_timeout_secs"]),
    (Sec::Notifications, "", &["notify.enabled", "@kinds", "notify.turn_done_min_secs", "notify.when_app_closed"]),
    (Sec::Notifications, "Floating badge", &["notify.badge", "notify.badge.corner", "notify.badge.snap", "notify.badge.inset_x", "notify.badge.inset_y", "notify.badge.sharing"]),
    (Sec::Notifications, "Cleaning up what needs you", &["needs_you.replace", "needs_you.clear_notes_on_open", "needs_you.clear_failed_on_run", "needs_you.withdraw_orphans", "needs_you.expire_hours", "notify.badge.clear_on_open"]),
    (Sec::Notifications, "How long each kind stays on screen", &["@stay"]),
    (Sec::Notifications, "Colors", &["@colors"]),
    (Sec::Notifications, "Counted on the bell", &["@bell"]),
    (Sec::Notifications, "Your kinds", &["@custom_kinds"]),
    (Sec::Notifications, "Banners for other terminals", &["@banners"]),
    (Sec::Notifications, "Banners for the terminal in front of you", &["@banners_focused"]),
    (Sec::Notifications, "Images", &["@images"]),
    (Sec::Notifications, "Text", &["@texts"]),
    (Sec::Sounds, "", &["notify.sounds", "notify.sounds_in_app", "@sounds"]),
    (Sec::Sounds, "Sound effects", &["@effects"]),
    (Sec::Webhooks, "Delivery", &["webhooks.path", "webhooks.port", "webhooks.relay_url"]),
    (Sec::Webhooks, "Triggers", &["triggers.agent_mode"]),
    (Sec::System, "macOS", &["@accessibility", "@notifications", "@login"]),
    (Sec::System, "midna", &["@version", "@daemon"]),
    (Sec::System, "Reset", &["@reset_settings", "@reset_midna"]),
];

/// What the main pane shows (a search shows its results over either).
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Section(Sec),
    /// The read-only, annotated `settings.json` (the sidebar's footer link).
    Json,
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
    /// `notify.kinds.list`: the notification kinds you added.
    custom: Vec<midna_proto::NotifyKindInfo>,
    /// The text fields of editable rows (a kind's title and text, a kind's name), by row key,
    /// with the value each was last filled from (so a reload doesn't clobber typing).
    edits: std::collections::HashMap<String, (LineInput, String)>,
    /// "Add a kind": its name.
    new_kind: LineInput,
    /// The sound/image picker that's open (its setting key).
    picker: Option<String>,
    error: Option<String>,
    view: View,
    /// The search field (↩ asks an agent instead).
    ask: LineInput,
    /// While searching: the one section the results are narrowed to (None = all).
    scope: Option<Sec>,
    /// "Agent commands": show each row's CLI.
    cli: bool,
    /// Shortcuts: the key detector while it's on, and the keys it caught.
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
        let ask = LineInput::new(cx, false, "Search, or ask an agent");
        if let Ok(q) = crate::dev::var("MIDNA_SETTINGS_QUERY") {
            // dev (screenshots): start with this search
            ask.set_text(&q, cx);
        }
        let subs = vec![
            cx.subscribe(&ask.field, |s, _, _: &crate::ui::text_input::FieldChanged, cx| {
                // a new search starts over all sections, at the top
                s.scope = None;
                s.scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            }),
            cx.observe_global::<Theme>(|_, cx| cx.notify()),
        ];
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let view = match crate::dev::var("MIDNA_SETTINGS_VIEW").as_deref() {
            Ok("json") => View::Json,
            Ok(v) => View::Section(SECS.into_iter().find(|s| s.id() == v).unwrap_or(Sec::General)),
            _ => View::Section(Sec::General),
        };
        let mut s = SettingsWindow {
            backend,
            entries: vec![],
            info: Value::Null,
            webhooks: Value::Null,
            hooks: Value::Null,
            media: Value::Null,
            custom: vec![],
            edits: Default::default(),
            new_kind: LineInput::new(cx, false, "Name, e.g. Deploys"),
            picker: None,
            error: None,
            view,
            ask,
            scope: crate::dev::var("MIDNA_SETTINGS_SCOPE").ok().and_then(|v| SECS.into_iter().find(|s| s.id() == v)),
            cli: crate::dev::var("MIDNA_SETTINGS_CLI").is_ok(),
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
            s.shortcut_menu = Some((sc.setting, point(px(560.), px(250.))));
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
            BackendEvent::Event(e) if e.kind.starts_with("webhooks.") || e.kind == "notify.media" || e.kind == "notify.kinds_changed" => self.load(cx),
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
                    let kinds = backend.call("notify.kinds.list", json!({})).ok().and_then(|v| serde_json::from_value::<midna_proto::NotifyKindsList>(v).ok());
                    (list, info, webhooks, media, hooks, kinds)
                })
                .await;
            let _ = this.update(cx, |s, cx| {
                s.custom = r.5.map(|k| k.kinds).unwrap_or_default();
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
                s.sync_edits(cx);
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
        self.entries.iter().find(|e| e.key == key).map(|e| e.value.clone()).or_else(|| self.spec_of(key).map(|s| s.default.to_json())).unwrap_or(Value::Null)
    }

    /// A setting's spec: the catalog's, or for a kind you added the pattern spec of its field.
    fn spec_of(&self, key: &str) -> Option<&'static midna_proto::settings::SettingSpec> {
        midna_proto::settings::setting(key).or_else(|| {
            let (field, kind) = midna_proto::notify::split_kind_key(key)?;
            self.custom.iter().any(|k| k.key == kind).then(|| midna_proto::settings::custom_kind_spec(field)).flatten()
        })
    }

    /// Every notification kind, built in then yours: (key, name).
    fn kinds(&self) -> Vec<(String, String)> {
        let builtin = midna_proto::notify::CATEGORIES.iter().map(|c| (c.key.to_string(), c.label.to_string()));
        builtin.chain(self.custom.iter().map(|k| (k.key.clone(), k.label.clone()))).collect()
    }

    /// Fill the editable rows' fields from the settings (each kind's title and text, the name of
    /// a kind you added), leaving alone a field you've typed in since it was last filled.
    fn sync_edits(&mut self, cx: &mut Context<Self>) {
        use midna_proto::notify::{body_key, title_key};
        let mut want: Vec<(String, String, &str)> = vec![];
        for (k, _) in self.kinds() {
            want.push((title_key(&k), self.value(&title_key(&k)).as_str().unwrap_or("").to_string(), "midna's own (the terminal · its project)"));
            want.push((body_key(&k), self.value(&body_key(&k)).as_str().unwrap_or("").to_string(), "midna's own"));
        }
        for k in &self.custom {
            want.push((format!("label:{}", k.key), k.label.clone(), "Name"));
        }
        self.edits.retain(|key, _| want.iter().any(|(k, ..)| k == key));
        for (key, value, placeholder) in want {
            match self.edits.get_mut(&key) {
                Some((input, last)) => {
                    if input.text(cx) == *last && *last != value {
                        input.set_text(&value, cx);
                    }
                    *last = value;
                }
                None => {
                    let input = LineInput::new(cx, false, placeholder);
                    input.set_text(&value, cx);
                    self.edits.insert(key, (input, value));
                }
            }
        }
    }

    /// An editable row's ↩: save its text (a setting, or a kind's name).
    fn commit_edit(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some((input, _)) = self.edits.get(key) else { return };
        let text = input.text(cx).trim().to_string();
        match key.strip_prefix("label:") {
            Some(kind) if !text.is_empty() => self.kind_call("notify.kinds.add", json!({ "key": kind, "label": text, "replace": true }), format!("midna notify kinds update {kind} --label \"{text}\""), cx),
            Some(_) => {}
            None => self.set(key, json!(text), cx),
        }
    }

    /// A `notify.kinds.*` call from this window, reported in the footer.
    fn kind_call(&mut self, method: &'static str, params: Value, cmd: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
            let _ = this.update(cx, |s, cx| {
                s.last = Some(Last {
                    cmd,
                    ok: r.is_ok(),
                    result: match &r {
                        Ok(_) => "✓ applied".into(),
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

    /// "Add a kind": its key from the name (`Deploys` -> `deploys`).
    fn add_kind(&mut self, cx: &mut Context<Self>) {
        let name = self.new_kind.text(cx).trim().to_string();
        let mut key: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
        key = key.trim_matches('_').split('_').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("_");
        key = key.trim_start_matches(|c: char| c.is_ascii_digit() || c == '_').chars().take(32).collect();
        if key.is_empty() {
            self.last = Some(Last { cmd: "midna notify kinds add <key>".into(), ok: false, result: "✗ give it a name with a letter in it".into(), who: "you, from this window".into(), at: Instant::now() });
            cx.notify();
            return;
        }
        self.new_kind.clear(cx);
        self.kind_call("notify.kinds.add", json!({ "key": key, "label": name }), format!("midna notify kinds add {key} --label \"{name}\""), cx);
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

    /// The search field's ↩: open an agent (`ui.ask.agent`) at the root with the request as its
    /// prompt, then bring the main window to that terminal.
    fn submit_ask(&mut self, cx: &mut Context<Self>) {
        let text = self.ask.text(cx).trim().to_string();
        if text.is_empty() {
            return;
        }
        self.ask.clear(cx);
        let prompt = format!("{text}\n\n(This is a midna settings request. `midna settings list` shows every setting; `midna settings set <key> <value>` changes one.)");
        let agent = if self.value("ui.ask.agent") == json!("codex") { "codex" } else { "claude" };
        let backend = self.backend.clone();
        let shown = text.clone();
        cx.spawn(async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    let v = backend.call("session.open", json!({"kind": "agent", "agent": agent, "name": "settings", "prompt": prompt}))?;
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
                        cmd: format!("midna open --agent {agent} -- \"{shown}\""),
                        ok: true,
                        result: format!("✓ agent {id} started"),
                        who: "you, from this window".into(),
                        at: Instant::now(),
                    },
                    Err(e) => Last { cmd: format!("midna open --agent {agent}"), ok: false, result: format!("✗ {e:#}"), who: "you, from this window".into(), at: Instant::now() },
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
    Sound { cat: String },
    Volume { key: String },
    /// An image picker: `notify.image` (cat None) or a kind's `notify.image.<kind>`.
    Image { key: String, cat: Option<String> },
    /// A kind's `notify.color.<kind>`: theme colors, a few more, and a custom one if set.
    Color { key: String, current: String },
    /// A text field (`edits`), saved on ↩: a setting, or `label:<kind>` (a kind's name), with
    /// buttons after it.
    Edit { key: String, actions: Vec<(String, Act)> },
    /// "Add a kind": a name field and Add.
    AddKind,
    /// `theme` / `theme.dark` / `theme.light`: swatch chips for every theme (customs too).
    Theme { key: String, current: String },
    /// A shortcut's keys: click to rebind (settings_shortcuts.rs). No setting = built in.
    Keys { setting: Option<&'static str>, keys: String },
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
    /// Remove a notification kind you added (and its settings).
    RemoveKind(String),
    /// Show a test notification of a kind.
    TestKind(String),
}

struct RowSpec {
    label: String,
    note: Option<(String, Hsla)>,
    control: Control,
    cli: String,
    who: Who,
    warn: bool,
}

impl RowSpec {
    /// The setting this row changes, from its command (None for a status or a button).
    fn key(&self) -> Option<&str> {
        self.cli.strip_prefix("midna settings set ").and_then(|r| r.split_whitespace().next())
    }

    /// What a search looks at besides the name and description, in the order a match is
    /// explained ("Option: Restart when idle").
    fn fields(&self) -> Vec<(&'static str, String)> {
        let mut out = vec![];
        match &self.control {
            Control::Seg { options, .. } => out.extend(options.iter().map(|(_, l)| ("Option", l.clone()))),
            Control::Text { text, action, .. } => {
                out.push(("Value", text.clone()));
                if let Some((label, ..)) = action {
                    out.push(("Button", label.clone()));
                }
            }
            Control::Theme { current, .. } => out.push(("Value", current.clone())),
            Control::Keys { keys, .. } => out.push(("Keys", keys.clone())),
            _ => {}
        }
        if let Some(k) = self.key() {
            out.push(("Key", k.to_string()));
        }
        let kw = related(self.key().unwrap_or(&self.label));
        if !kw.is_empty() {
            out.push(("Related", kw.to_string()));
        }
        if !self.cli.is_empty() {
            out.push(("Command", self.cli.clone()));
        }
        out
    }
}

/// Words people search for that a row doesn't say ("sound" finds the volume).
fn related(key_or_label: &str) -> &'static str {
    match key_or_label {
        "notify.sounds" | "notify.sounds_in_app" | "notify.volume" => "audio sound",
        "density" => "spacing compact",
        "theme" => "dark mode light mode colors",
        "theme.colors" => "colours",
        "terminal.option_as_meta" => "alt key",
        "terminal.image_paste" => "screenshot clipboard inline",
        "terminal.auto_name" | "terminal.auto_name_updates" => "rename tab title summary",
        "ui.haptics" => "trackpad vibration",
        "terminal.prompt_bar" => "prompt nav fast travel header",
        "agents.mcp" => "tools",
        "Accessibility" => "permission dictation",
        "Notifications" => "permission banners",
        "Login item" => "startup launch at login",
        "Update" => "upgrade version",
        "midna CLI" => "command line path",
        _ => "",
    }
}

struct Group {
    name: &'static str,
    /// A line beside the heading ("Built in": why they can't be changed).
    note: Option<&'static str>,
    rows: Vec<RowSpec>,
}

/// A row as shown: why a search found it when it isn't in its name or description.
struct Hit {
    row: RowSpec,
    via: Option<(&'static str, String)>,
}

/// A card in the main pane: a group, with its section when it's a search result.
struct Shown {
    sec: Sec,
    name: &'static str,
    note: Option<&'static str>,
    rows: Vec<Hit>,
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
        "agents.may_install_updates" => "Agents may install updates",
        "approve.from_cli" => "Agents may approve their own requests",
        "agents.trust_folders" => "Trusted folders",
        "agents.claude.statusline" => "Claude status line (cost)",
        "agents.mcp" => "Give agents the midna MCP tools",
        "agents.system_hint" => "Tell agents they're in midna",
        "policy.default" => "When no rule matches",
        "policy.request_timeout_secs" => "Approval timeout (seconds)",
        "git.refresh_secs" => "Git refresh (seconds)",
        "kass.auto_send" => "Send dictation when Kass finishes",
        "windows.close_with_terminals" => "Closing a window with terminals",
        "ide.app" => "Open in IDE",
        "ide.rules" => "IDE per folder or file",
        "finder.quick_action" => "Finder “Open in Midna”",
        "ui.haptics" => "Haptics",
        "ui.header.buttons" => "Terminal header buttons",
        "sidebar.footer.stats" => "Footer stats",
        "sidebar.footer.range" => "Footer stats count",
        "sidebar.footer.buttons" => "Footer buttons",
        "sidebar.footer.compact" => "Compact footer",
        "insights.layouts" => "Insights layouts",
        "insights.layout" => "Insights layout shown",
        "insights.colors.agents" => "Agent work color",
        "insights.colors.you" => "Your activity color",
        "insights.colors.waiting" => "Blocked on you color",
        "ui.status.looks" => "Status looks",
        "terminal.option_as_meta" => "Option as Meta",
        "agents.restart_on_update" => "After an agent update",
        "agents.restart_idle_secs" => "Idle before a restart (seconds)",
        "agents.adopt_typed" => "Adopt agents typed in a shell",
        "agents.resume_after_sleep" => "Resume after sleep",
        "agents.resume_after_network" => "Resume after network loss",
        "agents.resume_after_sleep_prompt" => "Resume message",
        "triggers.agent_mode" => "Agents started by triggers",
        "projects.roots" => "Project folders",
        "terminal.auto_name" => "Name terminals automatically",
        "terminal.auto_name_updates" => "As the work changes",
        "terminal.link_preview" => "Link previews",
        "terminal.prompt_bar" => "Prompt bar",
        "terminal.preview_path_click" => "Clicking a preview's path",
        "terminal.image_paste" => "⌘V of an image",
        "notify.enabled" => "Show notifications",
        "notify.turn_done_min_secs" => "“Agent finished” after (seconds)",
        "notify.volume" => "All sounds",
        "notify.sounds" => "Play sounds",
        "notify.sounds_in_app" => "While you're using midna",
        "notify.image" => "Every notification",
        "notify.when_app_closed" => "When the app isn't running",
        "notify.badge" => "Show the badge",
        "needs_you.replace" => "A terminal's newest replaces its older ones",
        "needs_you.clear_notes_on_open" => "Opening a terminal marks its notes done",
        "needs_you.clear_failed_on_run" => "A terminal working again clears its failures",
        "needs_you.withdraw_orphans" => "Take back approvals nobody can act on",
        "notify.badge.clear_on_open" => "Opening a terminal clears its badge notifications",
        "needs_you.expire_hours" => "Take back unanswered items after (hours, 0 = never)",
        "notify.badge.corner" => "Corner",
        "notify.badge.snap" => "When you drop it",
        "notify.badge.inset_x" => "From the side (points)",
        "notify.badge.inset_y" => "From the top or bottom (points)",
        "notify.badge.sharing" => "While you share your screen",
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

/// The durations a kind's `notify.stay.<kind>` offers, in seconds (0 = until handled).
const STAYS: [i64; 7] = [0, 3, 5, 8, 15, 30, 60];

fn stay_label(secs: i64) -> String {
    match secs {
        0 => "Until handled".into(),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

/// The colors a kind can take besides the theme's (`notify.color.<kind>`).
const MORE_COLORS: [&str; 4] = ["#e879b9", "#5fc9d8", "#a3d977", "#f0884a"];

fn option_label(key: &str, v: &str) -> String {
    match (key, v) {
        ("webhooks.path", "tailscale_funnel") => "Funnel".into(),
        ("webhooks.path", "self_relay") => "Self-hosted".into(),
        ("webhooks.path", "midna_relay") => "midna relay".into(),
        ("webhooks.path", "off") => "Off".into(),
        ("ui.header.script", "github+agent") => "github + agent".into(),
        ("terminal.auto_name", "agent") => "Agent's summary".into(),
        ("terminal.auto_name", "prompt") => "From the prompt".into(),
        ("terminal.auto_name", "context") => "Branch or folder".into(),
        ("terminal.auto_name_updates", "follow") => "Keep renaming".into(),
        ("terminal.auto_name_updates", "first") => "Name once".into(),
        ("terminal.link_preview", "hover") => "On hover".into(),
        ("terminal.link_preview", "cmd") => "On ⌘-hover".into(),
        ("terminal.prompt_bar", "scrolled") => "When scrolled back".into(),
        ("terminal.prompt_bar", "always") => "Always".into(),
        ("terminal.preview_path_click", "reveal") => "Reveal in Finder".into(),
        ("terminal.preview_path_click", "ide") => "Open in IDE".into(),
        ("terminal.preview_path_click", "copy") => "Copy path".into(),
        ("terminal.image_paste", "sheet") => "Open the image sheet".into(),
        ("terminal.image_paste", "inline") => "Paste it inline".into(),
        ("notify.badge", "background") => "Unless another midna is in front".into(),
        ("notify.badge", "always") => "Always".into(),
        ("notify.badge", "off") => "Off".into(),
        ("notify.badge.snap", "corner") => "Snap to a corner".into(),
        ("notify.badge.snap", "free") => "Stay where it lands".into(),
        ("notify.badge.sharing", "hide") => "Hide the badge".into(),
        ("notify.badge.sharing", "count") => "Only the number".into(),
        ("notify.badge.sharing", "show") => "Show as usual".into(),
        // script names are names: keep them as typed
        ("ui.ask.agent" | "ui.ask.scope", v) => capitalize(v),
        (k, v) if k.starts_with("ui.") => v.to_string(),
        (_, v) => capitalize(&v.replace('_', " ")),
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
        let spec = self.spec_of(key)?;
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
    /// button that installs straight away, or opens the main window's sheet to confirm a removal.
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
                + " Until reinstalled, midna adds its hooks to the agents it starts.", Some(("Reinstall", false))),
            Some("not_installed") => (t.fg, "Per terminal".into(), t.fg, per_terminal.into(), Some(("Install", false))),
            Some("error") => (t.err, format!("Can't read {}", path.rsplit('/').next().unwrap_or(path)), t.err, detail.unwrap_or_else(|| path.into()), Some(("Reinstall", false))),
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

    /// One setting's row, with notes that depend on other settings.
    fn key_row(&self, t: &Theme, key: &str) -> Option<RowSpec> {
        // theme.dark / theme.light only matter while the theme follows macOS
        if matches!(key, "theme.dark" | "theme.light") && self.value("theme") != json!("system") {
            return None;
        }
        let mut r = self.spec_row(key)?;
        let off = |what: &str| Some((format!("No effect while {what}"), t.dim));
        match key {
            "notify.turn_done_min_secs" | "notify.when_app_closed" if self.notify_off() => r.note = off("notifications are off"),
            "terminal.preview_path_click" if self.value("terminal.link_preview") == json!("off") => r.note = off("link previews are off"),
            "terminal.auto_name_updates" if self.value("terminal.auto_name") == json!("off") => r.note = off("automatic names are off"),
            "agents.may_move_windows" if !accessibility_trusted() => r.note = Some(("No effect until Accessibility is granted".into(), t.need)),
            "webhooks.path" => {
                let url = self.webhooks.get("public_url").and_then(Value::as_str).map(|u| format!("Public URL {u}"));
                if let Some(n) = url.or_else(|| self.webhooks.get("message").and_then(Value::as_str).map(str::to_string)) {
                    r.note = Some((n, Hsla::default()));
                }
            }
            _ => {}
        }
        Some(r)
    }

    fn notify_off(&self) -> bool {
        self.value("notify.enabled") == Value::Bool(false)
    }

    /// The rows for a `@name` in `LAYOUT`.
    fn special_rows(&self, t: &Theme, name: &str) -> Vec<RowSpec> {
        use midna_proto::notify::{bell_key, body_key, color_key, push_focused_key, push_key, setting_key, stay_key, title_key};
        let status = |label: &str, dot: Hsla, value: String, color: Hsla, note: Option<String>, cli: &str| RowSpec {
            label: label.into(),
            note: note.map(|n| (n, Hsla::default())),
            control: Control::Text { dot: Some(dot), text: value, color, action: None },
            cli: cli.into(),
            who: Who::ReadOnly,
            warn: false,
        };
        // Notifications off: say so on every row it silences.
        let silenced = |mut rows: Vec<RowSpec>| {
            if self.notify_off() {
                for r in &mut rows {
                    r.note = Some(("No effect while notifications are off".into(), t.dim));
                }
            }
            rows
        };
        // One row per kind (built in, then yours), named after the kind.
        let kinds = self.kinds();
        let per_kind = |key: fn(&str) -> String| {
            kinds
                .iter()
                .filter_map(|(k, label)| {
                    let mut r = self.spec_row(&key(k))?;
                    r.label = label.clone();
                    Some(r)
                })
                .collect::<Vec<_>>()
        };
        match name {
            "@update" => vec![lifecycle_rows(t).update, changelog_row(t)],
            "@cli" => vec![lifecycle_rows(t).cli],
            "@login" => vec![lifecycle_rows(t).login],
            "@hooks.claude" => vec![self.hooks_row(t, "claude")],
            "@hooks.codex" => vec![self.hooks_row(t, "codex")],
            "@kass" => {
                let seen = crate::kass::handshake_detected();
                let mut r = status(
                    "Kass",
                    if seen { t.ok } else { t.dim },
                    if seen { "Handshake detected" } else { "Handshake not detected" }.into(),
                    if seen { t.fg } else { t.dim },
                    Some(if seen {
                        "Dictation opens the composer under the terminal; Kass inserts into it directly.".into()
                    } else {
                        "Dictation composer: needs the Kass handshake (com.mrgnhnt.kass.dictationWillBegin). Detected once Kass sends one.".into()
                    }),
                    "midna info",
                );
                r.who = Who::ReadOnly;
                vec![r]
            }
            "@accessibility" => {
                let ax = accessibility_trusted();
                vec![self.permission(t, "Accessibility", Some(ax), if ax { "Granted" } else { "Missing" }, "Lets Kass read and edit terminal input, and lets agents move windows.", PANE_AX)]
            }
            "@notifications" => {
                use crate::notify::Permission as NP;
                let (ok, state, why) = match crate::notify::permission() {
                    NP::Allowed => (Some(true), "Allowed", "midna shows approvals, failures and finished turns as macOS notifications."),
                    NP::Denied => (Some(false), "Off in System Settings", "Turn midna's notifications on to get approvals, failures and finished turns."),
                    NP::NotAsked => (None, "Not requested yet", "macOS asks the first time midna has something to tell you."),
                    NP::Unknown => (None, "Checking…", "Approvals, failures and finished turns show as macOS notifications."),
                    NP::Dev => (None, "Dev build", "Not running from Midna.app: notifications go through osascript (shown as Script Editor)."),
                };
                vec![self.permission(t, "Notifications", ok, state, why, PANE_NOTIF)]
            }
            "@version" => {
                let daemon = self.info.get("version").and_then(Value::as_str);
                let (dot, note) = match daemon {
                    None => (t.dim, None),
                    Some(v) if v == midna_proto::VERSION => (t.ok, None),
                    Some(_) => (t.err, Some("Doesn't match the app. The daemon picks up the new version when it restarts.".into())),
                };
                vec![
                    status("App version", t.ok, midna_proto::VERSION.into(), t.fg, None, "midna info"),
                    status("Daemon version", dot, daemon.unwrap_or("not connected").into(), if daemon.is_some() { t.fg } else { t.dim }, note, "midna info"),
                ]
            }
            "@daemon" => {
                let uptime = self.info.get("uptime_secs").and_then(Value::as_u64).map(|s| crate::ui::charts::duration(s as f64)).unwrap_or_default();
                let text = if uptime.is_empty() { "not running".into() } else { format!("up {uptime} · pid {}", self.info.get("pid").and_then(Value::as_u64).unwrap_or(0)) };
                vec![RowSpec { control: Control::Text { dot: None, text, color: t.fg, action: None }, ..status("midnad", t.ok, String::new(), t.fg, Some("Runs every terminal, so they survive the app quitting.".into()), "midna info") }]
            }
            "@reset_settings" => {
                let changed = self.entries.iter().filter(|e| e.value != e.default).count();
                vec![RowSpec {
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
                }]
            }
            "@reset_midna" => vec![RowSpec {
                label: "Reset midna".into(),
                note: Some(("Closes every terminal, removes projects, triggers and needs-you items, and resets settings. Rules and the event log are kept.".into(), Hsla::default())),
                control: Control::Text {
                    dot: Some(t.err),
                    text: if self.armed_daemon_reset { "Closes every terminal now. Click again to confirm.".into() } else { "Start over".into() },
                    color: if self.armed_daemon_reset { t.err } else { t.dim },
                    action: Some((if self.armed_daemon_reset { "Confirm".into() } else { "Reset…".into() }, Act::ResetDaemon, true)),
                },
                cli: "midna daemon reset".into(),
                who: Who::Human,
                warn: false,
            }],
            // Notifications: each kind on or off. A terminal overrides any of these with
            // `midna notify set` (or mutes itself from its … menu).
            "@kinds" => silenced(per_kind(setting_key)),
            // Which kinds become macOS banners: for other terminals, and for the one in front of you.
            "@banners" => silenced(per_kind(push_key)),
            "@banners_focused" => silenced(per_kind(push_focused_key)),
            // Which kinds count toward the unread number on the status bar's bell.
            "@bell" => per_kind(bell_key),
            "@images" => silenced(self.notify_image_rows(t)),
            // Each kind's title and text: `notify.title.<kind>` / `notify.body.<kind>` templates,
            // typed here (↩ saves; empty = midna's own, `none` = no text).
            "@texts" => {
                let mut texts = vec![];
                for (k, label) in &kinds {
                    for (key, part) in [(title_key(k), "title"), (body_key(k), "text")] {
                        let Some(mut r) = self.spec_row(&key) else { continue };
                        r.label = format!("{label}: {part}");
                        r.control = Control::Edit { key: key.clone(), actions: vec![] };
                        texts.push(r);
                    }
                }
                silenced(texts)
            }
            // How long each kind stays on screen (the floating badge and the in-app card).
            "@stay" => {
                let mut rows = vec![];
                for (k, label) in &kinds {
                    let key = stay_key(k);
                    let Some(mut r) = self.spec_row(&key) else { continue };
                    let current = self.value(&key).as_i64().unwrap_or(0).to_string();
                    let mut options: Vec<(String, String)> = STAYS.iter().map(|s| (s.to_string(), stay_label(*s))).collect();
                    if !options.iter().any(|(v, _)| *v == current) {
                        options.push((current.clone(), stay_label(current.parse().unwrap_or(0))));
                    }
                    r.label = label.clone();
                    r.note = None;
                    r.control = Control::Seg { key, options, current };
                    rows.push(r);
                }
                silenced(rows)
            }
            "@colors" => {
                let mut rows = vec![];
                for (k, label) in &kinds {
                    let key = color_key(k);
                    let Some(mut r) = self.spec_row(&key) else { continue };
                    r.label = label.clone();
                    r.note = None;
                    r.control = Control::Color { current: self.value(&key).as_str().unwrap_or("").to_string(), key };
                    rows.push(r);
                }
                rows
            }
            // Kinds you added: rename, test or remove each; then add one.
            "@custom_kinds" => {
                let mut rows: Vec<RowSpec> = self
                    .custom
                    .iter()
                    .map(|k| RowSpec {
                        label: k.label.clone(),
                        note: Some((
                            format!("Agents send it with “midna notify send --kind {}”. ↩ saves a new name; Remove drops its settings too.", k.key),
                            Hsla::default(),
                        )),
                        control: Control::Edit { key: format!("label:{}", k.key), actions: vec![("Test".into(), Act::TestKind(k.key.clone())), ("Remove".into(), Act::RemoveKind(k.key.clone()))] },
                        cli: format!("midna notify kinds update {} --label \"{}\"", k.key, k.label),
                        who: Who::Agents,
                        warn: false,
                    })
                    .collect();
                rows.push(RowSpec {
                    label: "Add a kind".into(),
                    note: Some(("Its own sound, color, duration and switch, for notifications agents or triggers send as it (\"Deploys\", \"CI\").".into(), Hsla::default())),
                    control: Control::AddKind,
                    cli: "midna notify kinds add <key> --label <name>".into(),
                    who: Who::Agents,
                    warn: false,
                });
                rows
            }
            // The volume, then every kind's sound (the volume isn't silenced by notifications).
            "@sounds" => {
                let mut rows = self.notify_sound_rows(t);
                if self.notify_off() {
                    for r in rows.iter_mut().skip(1) {
                        r.note = Some(("No effect while notifications are off".into(), t.dim));
                    }
                }
                rows
            }
            "@effects" => self.sound_effect_rows(t),
            "@shortcuts" => self.shortcut_specs(t, false),
            "@built_in" => self.shortcut_specs(t, true),
            _ => vec![],
        }
    }

    /// A macOS grant: agents can't change it; the button opens its System Settings pane.
    fn permission(&self, t: &Theme, name: &str, ok: Option<bool>, state: &str, why: &str, pane: &str) -> RowSpec {
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
    }

    /// Every section's groups, in sidebar order (empty groups dropped).
    fn sections(&self, t: &Theme) -> Vec<(Sec, Vec<Group>)> {
        SECS.into_iter()
            .map(|sec| {
                let groups = LAYOUT
                    .iter()
                    .filter(|(s, ..)| *s == sec)
                    .map(|&(_, name, items)| Group {
                        name,
                        note: match name {
                            "Built in" => Some("Keys a screen handles itself. Not settings, so they can't be changed."),
                            "Floating badge" => Some("Counts what needs you in a corner of the screen. Drag it to move it."),
                            "How long each kind stays on screen" => Some("Until handled: until you answer, open or dismiss it."),
                            "Colors" => Some("In the badge, the in-app card and the notifications list."),
                            "Your kinds" => Some("Kinds agents and triggers can send as."),
                            _ => None,
                        },
                        rows: items.iter().flat_map(|i| if i.starts_with('@') { self.special_rows(t, i) } else { self.key_row(t, i).into_iter().collect() }).collect(),
                    })
                    .filter(|g| !g.rows.is_empty())
                    .collect();
                (sec, groups)
            })
            .collect()
    }
}

/// The search: every word must appear somewhere in a row (its name, description, options,
/// value, key, related words, command, group or section). Only sections with matches are
/// returned, each with its matching groups.
fn search(sections: Vec<(Sec, Vec<Group>)>, query: &str) -> Vec<(Sec, Vec<Shown>)> {
    let words = words(query);
    if words.is_empty() {
        return vec![];
    }
    let hits = |s: &str| {
        let s = s.to_lowercase();
        words.iter().any(|w| s.contains(w.as_str()))
    };
    let mut out = vec![];
    for (sec, groups) in sections {
        let mut shown = vec![];
        for g in groups {
            let mut rows = vec![];
            for r in g.rows {
                let fields = r.fields();
                let note = r.note.as_ref().map(|(n, _)| n.as_str()).unwrap_or("");
                let hay = format!("{} {note} {} {} {}", r.label, g.name, sec.label(), fields.iter().map(|(_, f)| f.as_str()).collect::<Vec<_>>().join(" ")).to_lowercase();
                if !words.iter().all(|w| hay.contains(w.as_str())) {
                    continue;
                }
                let via = if hits(&r.label) || hits(note) { None } else { fields.into_iter().find(|(_, f)| hits(f)) };
                rows.push(Hit { row: r, via });
            }
            if !rows.is_empty() {
                shown.push(Shown { sec, name: g.name, note: g.note, rows });
            }
        }
        if !shown.is_empty() {
            out.push((sec, shown));
        }
    }
    out
}

/// A description split into sentences ("e.g. ~/Development" doesn't end one).
fn sentences(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let (mut start, mut from) = (0, 0);
    while let Some(i) = text[from..].find(". ") {
        let end = from + i + 1;
        if !["e.g.", "i.e.", "etc."].iter().any(|a| text[..end].ends_with(a)) {
            out.push(&text[start..end]);
            start = end + 1;
        }
        from = end;
    }
    out.push(&text[start..]);
    out
}

/// The sentence of a description a search matched, when it isn't the first.
fn excerpt(note: &str, words: &[String]) -> String {
    let all = sentences(note);
    match all.iter().position(|s| words.iter().any(|w| s.to_lowercase().contains(w.as_str()))) {
        Some(0) | None => note.to_string(),
        Some(i) => format!("… {}", all[i]),
    }
}

/// A description's first sentence (and the "(default: …)" a changed setting ends with).
fn brief(note: &str) -> String {
    let (body, default) = match note.rfind(" (default: ") {
        Some(i) if note.ends_with(')') => note.split_at(i),
        _ => (note, ""),
    };
    let first = sentences(body)[0];
    // a long one stops at its first clause: "Script that renders the header line: …"
    if first.len() > 90
        && let Some(i) = [": ", "; ", " ("].iter().filter_map(|sep| first.find(sep)).filter(|&i| i >= 20).min()
    {
        return format!("{}.{default}", &first[..i]);
    }
    format!("{first}{default}")
}

fn words(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

/// `text` with every search word marked (case-insensitive).
fn marked(t: &Theme, text: String, words: &[String]) -> StyledText {
    let lower = text.to_lowercase();
    let mut ranges: Vec<std::ops::Range<usize>> = vec![];
    // byte offsets only line up when lowercasing kept the length
    if lower.len() == text.len() {
        for w in words.iter().filter(|w| !w.is_empty()) {
            let mut from = 0;
            while let Some(i) = lower[from..].find(w.as_str()) {
                let r = from + i..from + i + w.len();
                if text.is_char_boundary(r.start) && text.is_char_boundary(r.end) {
                    ranges.push(r.clone());
                }
                from = r.end;
            }
        }
    }
    ranges.sort_by_key(|r| r.start);
    let mut merged: Vec<std::ops::Range<usize>> = vec![];
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => merged.push(r),
        }
    }
    let style = HighlightStyle { background_color: Some(t.accent.opacity(0.32)), color: Some(t.fg), ..Default::default() };
    StyledText::new(SharedString::from(text)).with_highlights(merged.into_iter().map(|r| (r, style)))
}

/// What's new in each release, on the website (the site renders CHANGELOG.md).
fn changelog_row(t: &Theme) -> RowSpec {
    RowSpec {
        label: "Changelog".into(),
        note: Some(("What's new in each release, on midna.mrgnhnt.com.".into(), Hsla::default())),
        control: Control::Text { dot: None, text: String::new(), color: t.fg, action: Some(("Open".into(), Act::Url(CHANGELOG_URL.into()), false)) },
        cli: String::new(),
        who: Who::ReadOnly,
        warn: false,
    }
}

const CHANGELOG_URL: &str = "https://midna.mrgnhnt.com/changelog/";

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
        Some(UpdateState::Ready { version, .. }) => row(
            "Update",
            t.accent,
            format!("{version} ready"),
            t.fg,
            Some(("Restart to apply".into(), Act::Life(Cmd::Apply), true)),
            "Downloaded and verified. Terminals keep running through the restart.".into(),
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

const SIDEBAR_W: f32 = 228.;

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        let query = self.ask.text(cx);
        let words = words(&query);
        let sections = self.sections(&t);
        // the sidebar's amber badges: rows that need you
        let badges: Vec<(Sec, usize)> = sections.iter().map(|(s, gs)| (*s, gs.iter().flat_map(|g| &g.rows).filter(|r| r.warn).count())).collect();
        let (sidebar, head, body): (AnyElement, AnyElement, AnyElement);
        if !words.is_empty() {
            let found = search(sections, &query);
            let counts: Vec<(Sec, usize)> = found.iter().map(|(s, gs)| (*s, gs.iter().map(|g| g.rows.len()).sum())).collect();
            let total: usize = counts.iter().map(|(_, n)| n).sum();
            let scope = self.scope.filter(|s| counts.iter().any(|(c, _)| c == s));
            let q = query.trim().to_string();
            let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
            let (title, sub) = match scope {
                _ if total == 0 => ("No results".to_string(), "No setting matches. Press ↩ to ask an agent instead.".to_string()),
                None => (format!("Results for “{q}”"), format!("{} in {}.", count(total, "setting", "settings"), count(counts.len(), "section", "sections"))),
                Some(s) => {
                    let n = counts.iter().find(|(c, _)| *c == s).map_or(0, |(_, n)| *n);
                    (s.label().to_string(), format!("{} for “{q}” · {} in other sections.", count(n, "match", "matches"), total - n))
                }
            };
            let shown: Vec<Shown> = found.into_iter().filter(|(s, _)| scope.is_none_or(|x| x == *s)).flat_map(|(_, gs)| gs).collect();
            sidebar = self.sidebar(&t, Some(Found { counts: &counts, total, scope }), &badges, window, cx).into_any_element();
            head = self.page_header(&t, title, sub, None, cx).into_any_element();
            body = self.cards(&t, shown, &words, Some((q, total)), window, cx).into_any_element();
        } else {
            sidebar = self.sidebar(&t, None, &badges, window, cx).into_any_element();
            match self.view {
                View::Json => {
                    let sub = "What `midna settings list --json` returns. Read-only here: change a setting in its section, from the CLI, or by asking.";
                    head = self.page_header(&t, "settings.json".into(), sub.into(), None, cx).into_any_element();
                    body = self.json(&t).into_any_element();
                }
                View::Section(sec) => {
                    let sub = if sec == Sec::Shortcuts && !self.recorded.is_empty() {
                        format!("Shortcuts on {}. Press other keys to search again; ⌥⌘K stops.", crate::actions::pretty(&self.recorded.join(" ")))
                    } else {
                        sec.about().to_string()
                    };
                    head = self.page_header(&t, sec.label().into(), sub, Some(sec), cx).into_any_element();
                    let groups = sections.into_iter().find(|(s, _)| *s == sec).map(|(_, g)| g).unwrap_or_default();
                    let shown = groups.into_iter().map(|g| Shown { sec, name: g.name, note: g.note, rows: g.rows.into_iter().map(|row| Hit { row, via: None }).collect() }).collect();
                    body = self.cards(&t, shown, &[], None, window, cx).into_any_element();
                }
            }
        }
        div()
            .id("settings-root")
            .track_focus(&self.focus)
            .key_context("MidnaSettings")
            .on_action(|_: &crate::actions::CloseWindow, window, _| window.remove_window())
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .bg(t.bg)
            .text_color(t.fg)
            .font_family(t.ui_font.clone())
            .text_size(px(13.))
            .line_height(px(13. * 1.45))
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(head)
                    .when_some(self.error.clone(), |d, e| {
                        d.child(div().mx(px(28.)).mb(px(10.)).px(px(12.)).py(px(8.)).rounded(px(8.)).bg(t.need_soft).text_color(t.need).text_size(px(12.)).child(format!("midnad: {e} — showing the catalog defaults.")))
                    })
                    .child(body)
                    .child(self.footer(&t)),
            )
            .children(self.shortcut_menu_el(&t, cx))
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

/// What a search found, for the sidebar: matches per section, in all, and the section the
/// results are narrowed to.
struct Found<'a> {
    counts: &'a [(Sec, usize)],
    total: usize,
    scope: Option<Sec>,
}

/// Drags the window from empty space; a double click zooms, like a title bar.
fn drag_window(ev: &MouseDownEvent, window: &mut Window, _: &mut App) {
    if ev.click_count >= 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

impl SettingsWindow {
    /// An enum with many options: a button showing the current one that opens the list.
    fn choice_menu(&self, t: &Theme, key: String, options: Vec<(String, String)>, current: String, cx: &mut Context<Self>) -> AnyElement {
        let label = options.iter().find(|(v, _)| *v == current).map_or(current.clone(), |(_, l)| l.clone());
        let open = self.picker.as_deref() == Some(key.as_str());
        let menu = open.then(|| {
            let mut list = div().id(SharedString::from(format!("choices-{key}"))).max_h(px(320.)).overflow_y_scroll().flex().flex_col();
            for (i, (v, l)) in options.into_iter().enumerate() {
                let on = v == current;
                let k = key.clone();
                list = list.child(
                    div()
                        .id(SharedString::from(format!("choice-{key}-{i}")))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.accent_soft))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.picker = None;
                            if !on {
                                s.set(&k, json!(v), cx);
                            }
                            cx.notify();
                        }))
                        .child(div().w(px(12.)).flex_none().when(on, |d| d.child(Icon::Check.el(11., t.accent))))
                        .child(div().flex_1().min_w_0().truncate().child(l)),
                );
            }
            deferred(
                anchored().offset(point(px(0.), px(30.))).snap_to_window_with_margin(px(8.)).child(
                    crate::ui::sidebar::menu_box(t).min_w(px(200.)).p(px(4.)).text_size(px(12.5)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(list),
                ),
            )
            .with_priority(2)
        });
        div().relative().child(self.picker_button(t, &key, label, 170., cx)).children(menu).into_any_element()
    }

    fn go(&mut self, v: View, cx: &mut Context<Self>) {
        if self.view != v {
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.view = v;
        if v != View::Section(Sec::Shortcuts) {
            self.recorder = None;
            self.editing = None;
            self.shortcut_menu = None;
        }
        cx.notify();
    }

    /// Leave the search for a section (a result's crumb, or ⌘[ / ⌘] while searching).
    fn open_section(&mut self, sec: Sec, cx: &mut Context<Self>) {
        self.ask.clear(cx);
        self.scope = None;
        self.go(View::Section(sec), cx);
    }

    /// The window's own keys (listed under Built in on Shortcuts). The key detector takes
    /// keys before this while it's on.
    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        if !m.platform || m.control || m.shift {
            return;
        }
        match (ks.key.as_str(), m.alt) {
            ("k" | "f", false) => {
                self.ask.focus.focus(window, cx);
                self.ask.field.update(cx, |f, cx| f.select_all(cx));
                cx.notify();
            }
            ("[" | "]", false) => {
                let at = match self.view {
                    View::Section(s) => SECS.iter().position(|x| *x == s).unwrap_or(0),
                    View::Json => SECS.len() - 1,
                };
                let next = if ks.key == "]" { (at + 1) % SECS.len() } else { (at + SECS.len() - 1) % SECS.len() };
                self.open_section(SECS[next], cx);
            }
            ("k", true) => self.toggle_recording(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The search field, the sections (or, while searching, the sections with matches), and
    /// the live dot with the settings.json link.
    fn sidebar(&self, t: &Theme, found: Option<Found>, badges: &[(Sec, usize)], window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.ask.focus.is_focused(window);
        let searching = found.is_some();
        let field = div()
            .id("search")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(30.))
            .mx(px(12.))
            .mb(px(12.))
            .pl(px(10.))
            .pr(px(6.))
            .rounded(px(8.))
            .border_1()
            .border_color(if focused || searching { t.accent } else { t.line })
            .when(focused, |d| d.shadow(vec![BoxShadow { color: t.accent_soft, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }]))
            .bg(t.bg)
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
                    if s.ask.is_empty(cx) {
                        s.focus.focus(window, cx);
                    }
                    s.ask.clear(cx);
                    cx.notify();
                }
                KeyOutcome::Ignored => cx.propagate(),
            }))
            .child(Icon::Search.el(13., t.dim))
            .child(div().flex_1().min_w_0().flex().items_center().overflow_hidden().text_color(t.fg).child(self.ask.field.clone()))
            .when(searching, |d| {
                d.child(
                    div()
                        .id("search-clear")
                        .size(px(18.))
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(t.line)
                        .cursor_pointer()
                        .tooltip(crate::ui::header::tip_fixed("Clear the search", "esc"))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(|s, _, window, cx| {
                            s.ask.clear(cx);
                            s.ask.focus.focus(window, cx);
                            cx.notify();
                        }))
                        .child(Icon::Cross.el(8., t.fg)),
                )
            });
        let item = |id: SharedString, icon: Icon, label: &str, on: bool, gap: bool| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(9.))
                .h(px(30.))
                .px(px(9.))
                .when(gap, |d| d.mt(px(12.)))
                .rounded(px(7.))
                .cursor_pointer()
                .when(on, |d| d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::SEMIBOLD))
                .when(!on, |d| d.text_color(t.fg).hover(|s| s.bg(t.raised.opacity(0.6))))
                .child(icon.el(15., if on { t.accent } else { t.dim }))
                .child(div().flex_1().min_w_0().truncate().child(label.to_string()))
        };
        let count = |n: usize, amber: bool| {
            div()
                .flex_none()
                .min_w(px(18.))
                .h(px(18.))
                .px(px(5.))
                .rounded(px(9.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .map(|d| if amber { d.bg(t.need).text_color(t.badge_fg) } else { d.bg(t.line.opacity(0.7)).text_color(t.dim) })
                .child(n.to_string())
        };
        let mut list = div().id("sections").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().gap(px(1.)).px(px(10.));
        match found {
            Some(Found { counts, total, scope }) => {
                if total > 0 {
                    list = list.child(item("sec-all".into(), Icon::Search, "All results", scope.is_none(), false).child(count(total, false)).on_click(cx.listener(|s, _, _, cx| {
                        s.scope = None;
                        s.scroll.set_offset(point(px(0.), px(0.)));
                        cx.notify();
                    })));
                }
                for (i, &(sec, n)) in counts.iter().enumerate() {
                    list = list.child(item(SharedString::from(format!("sec-{}", sec.id())), sec.icon(), sec.label(), scope == Some(sec), i == 0).child(count(n, false)).on_click(cx.listener(
                        move |s, _, _, cx| {
                            s.scope = Some(sec);
                            s.scroll.set_offset(point(px(0.), px(0.)));
                            cx.notify();
                        },
                    )));
                }
                let hidden = SECS.len() - counts.len();
                if total > 0 && hidden > 0 {
                    list = list.child(div().px(px(9.)).pt(px(10.)).text_size(px(11.5)).line_height(px(15.)).text_color(t.dim).child(format!(
                        "{hidden} section{} with no matches hidden",
                        if hidden == 1 { "" } else { "s" }
                    )));
                }
            }
            None => {
                for sec in SECS {
                    let n = badges.iter().find(|(s, _)| *s == sec).map_or(0, |(_, n)| *n);
                    list = list.child(
                        item(SharedString::from(format!("sec-{}", sec.id())), sec.icon(), sec.label(), self.view == View::Section(sec), sec.gap())
                            .when(n > 0, |d| d.child(count(n, true)))
                            .on_click(cx.listener(move |s, _, _, cx| s.go(View::Section(sec), cx))),
                    );
                }
            }
        }
        let live = self.error.is_none() && !self.entries.is_empty();
        let json_on = self.view == View::Json && !searching;
        div()
            .w(px(SIDEBAR_W))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.panel)
            .border_r_1()
            .border_color(t.line)
            // room for the traffic lights; drags the window like a title bar
            .child(div().id("sidebar-drag").h(px(48.)).flex_none().on_mouse_down(MouseButton::Left, drag_window))
            .child(field)
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(7.))
                    .px(px(19.))
                    .py(px(12.))
                    .text_size(px(12.))
                    .text_color(t.dim)
                    .child(div().size(px(7.)).rounded_full().bg(if live { t.ok } else { t.need }))
                    .child(div().flex_1().child(if live { "Live" } else { "Not connected" }))
                    .child(
                        div()
                            .id("open-json")
                            .cursor_pointer()
                            .text_color(t.accent)
                            .when(json_on, |d| d.font_weight(FontWeight::BOLD))
                            .hover(|s| s.underline())
                            .tooltip(crate::ui::header::tip("Every setting as `midna settings list --json` returns it"))
                            .on_click(cx.listener(|s, _, _, cx| {
                                s.ask.clear(cx);
                                s.go(View::Json, cx);
                            }))
                            .child("settings.json"),
                    ),
            )
    }

    /// The page's title and what it's for; "Agent commands" (and, on Shortcuts, the key detector).
    fn page_header(&self, t: &Theme, title: String, sub: String, sec: Option<Sec>, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(7.))
                .border_1()
                .text_size(px(12.))
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .map(|d| if on { d.border_color(t.accent).bg(t.accent_soft).text_color(t.fg) } else { d.border_color(t.line).bg(t.panel).text_color(t.dim).hover(|s| s.text_color(t.fg)) })
        };
        let json = self.view == View::Json && self.ask.is_empty(cx);
        div()
            .id("page-header")
            .flex()
            .flex_none()
            .items_end()
            .gap(px(16.))
            .px(px(28.))
            .pt(px(22.))
            .pb(px(14.))
            .on_mouse_down(MouseButton::Left, drag_window)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(div().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight::BOLD).truncate().child(title))
                    .child(div().text_size(px(12.5)).line_height(px(17.)).text_color(t.dim).child(sub)),
            )
            .when(sec == Some(Sec::Shortcuts), |d| d.child(self.record_button(t, cx)))
            .when(!json, |d| {
                d.child(
                    button("agent-commands", self.cli)
                        .tooltip(crate::ui::header::tip("Show the command an agent would run for each setting"))
                        .on_click(cx.listener(|s, _, _, cx| {
                            s.cli = !s.cli;
                            cx.notify();
                        }))
                        .child(Icon::Code.el(13., if self.cli { t.accent } else { t.dim }))
                        .child("Agent commands"),
                )
            })
    }

    /// The groups as cards. While searching, each heading starts with its section (a link to
    /// it), and the last card asks an agent.
    fn cards(&self, t: &Theme, shown: Vec<Shown>, words: &[String], search: Option<(String, usize)>, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().id("settings-rows").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.scroll).flex().flex_col().gap(px(20.)).px(px(28.)).pt(px(4.)).pb(px(28.));
        let mut id = 0;
        for g in shown {
            let crumb = search.is_some();
            let heading = (crumb || !g.name.is_empty()).then(|| {
                let sec = g.sec;
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(4.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.dim)
                    .when(crumb, |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("crumb-{}-{id}", sec.id())))
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .text_color(t.fg)
                                .cursor_pointer()
                                .hover(|s| s.text_color(t.accent))
                                .tooltip(crate::ui::header::tip(format!("Open {}", sec.label())))
                                .on_click(cx.listener(move |s, _, _, cx| s.open_section(sec, cx)))
                                .child(sec.icon().el(12., t.dim))
                                .child(sec.label()),
                        )
                        .when(!g.name.is_empty(), |d| d.child(div().text_color(t.line).child("›")))
                    })
                    .child(g.name)
                    .when_some(g.note, |d, n| d.child(div().font_weight(FontWeight::NORMAL).text_size(px(11.5)).child(format!("· {n}"))))
            });
            let mut card = div().flex().flex_col().rounded(px(10.)).border_1().border_color(t.line).bg(t.panel).overflow_hidden();
            for (i, hit) in g.rows.into_iter().enumerate() {
                card = card.child(self.row(t, hit, words, i == 0, id, window, cx));
                id += 1;
            }
            list = list.child(div().flex().flex_col().gap(px(8.)).children(heading).child(card));
        }
        if let Some((q, total)) = search {
            let agent = if self.value("ui.ask.agent") == json!("codex") { "Codex" } else { "Claude" };
            if total == 0 {
                list = list.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.))
                        .pt(px(56.))
                        .child(Icon::Search.el(26., t.line))
                        .child(div().pt(px(6.)).text_size(px(14.)).font_weight(FontWeight::BOLD).child(format!("No setting matches “{q}”")))
                        .child(div().text_size(px(12.)).text_color(t.dim).child("Search looks at names, descriptions, options, values and keys.")),
                );
            }
            list = list.child(
                div()
                    .id("ask-agent")
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(14.))
                    .py(px(12.))
                    .rounded(px(10.))
                    .border_1()
                    .border_dashed()
                    .border_color(t.line)
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.accent).bg(t.panel))
                    .on_click(cx.listener(|s, _, _, cx| s.submit_ask(cx)))
                    .child(
                        div()
                            .size(px(28.))
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .bg(t.accent_soft)
                            .child(if agent == "Codex" { Icon::Codex } else { Icon::Claude }.el(15., t.accent)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(format!("Ask {agent}: “{q}”")))
                            .child(div().text_size(px(12.)).text_color(t.dim).child(if total == 0 {
                                "An agent can find it, or change it for you."
                            } else {
                                "Not what you meant? An agent can find or change it for you."
                            })),
                    )
                    .child(crate::ui::header::key_chip(t, "↩".into()).text_size(px(12.)).text_color(t.dim)),
            );
        }
        list
    }

    fn row(&self, t: &Theme, hit: Hit, words: &[String], first: bool, id: usize, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Hit { row: r, via } = hit;
        let note = r.note.map(|(n, c)| (n, if c == Hsla::default() { t.dim } else { c }));
        let lock = r.who == Who::Human && r.cli.starts_with("midna settings set ");
        let label_hit = words.iter().any(|w| r.label.to_lowercase().contains(w.as_str()));
        let keys_setting = match &r.control {
            Control::Keys { setting, .. } => *setting,
            _ => None,
        };
        let active = keys_setting.is_some_and(|k| self.editing.as_ref().is_some_and(|e| e.setting() == k) || self.shortcut_menu.is_some_and(|(s, _)| s == k));
        let wide = matches!(r.control, Control::Theme { .. });
        let control = self.control(t, r.control, words, id, window, cx);
        let (inline, below) = if wide { (None, Some(control)) } else { (Some(control), None) };
        let cli = r.cli;
        let copied = self.copied.as_ref().is_some_and(|(c, _)| *c == cli);
        let cli_line = (self.cli && !cli.is_empty()).then(|| {
            let text = cli.clone();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .mt(px(8.))
                .pl(px(10.))
                .pr(px(4.))
                .py(px(3.))
                .rounded(px(6.))
                .bg(t.term)
                .font_family(t.mono_font.clone())
                .text_size(px(11.5))
                .child(div().flex_none().text_color(t.accent).child("$"))
                .child(div().flex_1().min_w_0().truncate().text_color(t.fg).child(cli.clone()))
                .child(
                    div()
                        .id(SharedString::from(format!("copy-{id}")))
                        .flex_none()
                        .h(px(20.))
                        .px(px(6.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .font_family(t.ui_font.clone())
                        .text_size(px(11.))
                        .text_color(if copied { t.ok } else { t.dim })
                        .cursor_pointer()
                        .hover(|s| s.bg(t.raised).text_color(t.fg))
                        .on_click(cx.listener(move |s, _, _, cx| s.copy(text.clone(), cx)))
                        .child(if copied { "Copied" } else { "Copy" }),
                )
        });
        div()
            .id(SharedString::from(format!("row-{id}")))
            .flex()
            .flex_col()
            .px(px(16.))
            .py(px(9.))
            .when(!first, |d| d.border_t_1().border_color(t.line))
            .when(r.warn, |d| d.bg(t.need_soft))
            .when(active, |d| d.bg(t.raised))
            .when_some(keys_setting, |d, setting| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |s, ev: &MouseDownEvent, _, cx| {
                        s.shortcut_menu = Some((setting, ev.position));
                        cx.notify();
                    }),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .min_h(px(28.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(150.))
                            .flex()
                            .flex_col()
                            .child(div().flex().items_center().gap(px(6.)).child(marked(t, r.label, words)).when(lock, |d| d.child(Icon::Lock.el(11., t.dim))))
                            .when_some(note, |d, (n, c)| {
                                // the catalog's descriptions are written for agents: the first
                                // sentence here, all of it on hover (and when only the rest matched)
                                let first = brief(&n);
                                let seen = |s: &str| words.iter().any(|w| s.to_lowercase().contains(w.as_str()));
                                let short = if words.is_empty() || seen(&first) || label_hit { first } else { excerpt(&n, words) };
                                let cut = short.len() < n.len();
                                d.child(
                                    div()
                                        .id(SharedString::from(format!("note-{id}")))
                                        .text_size(px(11.5))
                                        .line_height(px(15.))
                                        .text_color(c)
                                        .when(cut, |d| d.tooltip(crate::ui::header::tip(n)))
                                        .child(marked(t, short, words)),
                                )
                            })
                            .when_some(via, |d, (what, text)| {
                                d.child(
                                    div()
                                        .flex()
                                        .gap(px(5.))
                                        .mt(px(2.))
                                        .text_size(px(11.))
                                        .line_height(px(15.))
                                        .text_color(t.dim)
                                        .child(div().flex_none().child(what))
                                        .child(div().min_w_0().truncate().font_family(t.mono_font.clone()).child(marked(t, text, words))),
                                )
                            }),
                    )
                    .children(inline.map(|c| div().flex().justify_end().min_w_0().max_w(relative(0.62)).child(c))),
            )
            .children(below.map(|c| div().pt(px(8.)).child(c)))
            .children(cli_line)
    }

    /// A row button's action.
    fn run(&mut self, act: &Act, cx: &mut Context<Self>) {
        match act {
            Act::Url(u) => cx.open_url(u),
            Act::ResetSettings => {
                if self.armed_reset {
                    self.reset_all(cx);
                } else {
                    self.armed_reset = true;
                    self.armed_daemon_reset = false;
                }
                cx.notify();
            }
            Act::ResetDaemon => {
                if self.armed_daemon_reset {
                    self.reset_daemon(cx);
                } else {
                    self.armed_daemon_reset = true;
                    self.armed_reset = false;
                }
                cx.notify();
            }
            Act::Life(c) => crate::lifecycle::command(c.clone(), cx),
            &Act::Hooks { uninstall } => crate::windows::with_active(cx, |m, window, cx| {
                if uninstall {
                    window.activate_window();
                    crate::ui::hooks::open(m, true, window, cx);
                } else {
                    crate::ui::hooks::install(m, cx);
                }
            }),
            Act::RemoveKind(k) => self.kind_call("notify.kinds.remove", json!({ "key": k }), format!("midna notify kinds rm {k}"), cx),
            Act::TestKind(k) => self.test_notification(k.clone(), cx),
        }
    }

    /// A text field row (`Control::Edit`): ↩ saves, esc puts back what's saved.
    fn edit_control(&self, t: &Theme, key: String, actions: Vec<(String, Act)>, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let Some((input, saved)) = self.edits.get(&key) else { return div().into_any_element() };
        let changed = input.text(cx) != *saved;
        let k = key.clone();
        let mut row = div().flex().items_center().gap(px(6.)).w(px(320.)).child(
            input
                .render(t, SharedString::from(format!("edit-{key}")), window)
                .h(px(28.))
                .flex_1()
                .min_w_0()
                .on_key_down(cx.listener(move |s, ev: &KeyDownEvent, _, cx| {
                    let Some((input, saved)) = s.edits.get_mut(&k) else { return };
                    match input.on_key(ev, cx) {
                        KeyOutcome::Submit => {
                            cx.stop_propagation();
                            s.commit_edit(&k, cx);
                        }
                        KeyOutcome::Cancel => {
                            cx.stop_propagation();
                            let saved = saved.clone();
                            input.set_text(&saved, cx);
                        }
                        _ => {}
                    }
                    cx.notify();
                })),
        );
        if changed {
            let k = key.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("edit-save-{key}")))
                    .flex_none()
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .bg(t.accent)
                    .text_color(t.accent_fg)
                    .cursor_pointer()
                    .on_click(cx.listener(move |s, _, _, cx| s.commit_edit(&k, cx)))
                    .child("Save"),
            );
        }
        for (i, (label, act)) in actions.into_iter().enumerate() {
            row = row.child(
                div()
                    .id(SharedString::from(format!("edit-act-{key}-{i}")))
                    .flex_none()
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .text_color(if matches!(act, Act::RemoveKind(_)) { t.err } else { t.fg })
                    .cursor_pointer()
                    .hover(|s| s.border_color(t.dim))
                    .on_click(cx.listener(move |s, _, _, cx| s.run(&act, cx)))
                    .child(label),
            );
        }
        row.into_any_element()
    }

    /// A kind's color: the theme's six, a few more, and the one it has if it's none of these.
    fn color_control(&self, t: &Theme, key: String, current: String, cx: &mut Context<Self>) -> AnyElement {
        let mut values: Vec<String> = midna_proto::notify::COLOR_TOKENS.iter().chain(MORE_COLORS.iter()).map(|c| c.to_string()).collect();
        if !current.is_empty() && !values.contains(&current) {
            values.push(current.clone());
        }
        let mut row = div().flex().flex_wrap().justify_end().gap(px(4.));
        for (i, v) in values.into_iter().enumerate() {
            let on = v == current;
            let color = crate::ui::notifications::color_of(t, &v).unwrap_or(t.dim);
            let k = key.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("color-{key}-{i}")))
                    .size(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(6.))
                    .border_2()
                    .border_color(if on { t.fg } else { gpui_kit::transparent_black() })
                    .cursor_pointer()
                    .tooltip(crate::ui::header::tip(v.clone()))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        if !on {
                            s.set(&k, json!(v), cx);
                        }
                    }))
                    .child(div().size(px(14.)).rounded(px(4.)).bg(color)),
            );
        }
        row.into_any_element()
    }

    fn add_kind_control(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .w(px(320.))
            .child(
                self.new_kind
                    .render(t, "new-kind", window)
                    .h(px(28.))
                    .flex_1()
                    .min_w_0()
                    .on_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| match s.new_kind.on_key(ev, cx) {
                        KeyOutcome::Submit => {
                            cx.stop_propagation();
                            s.add_kind(cx);
                        }
                        KeyOutcome::Cancel => {
                            cx.stop_propagation();
                            s.new_kind.clear(cx);
                        }
                        _ => {}
                    })),
            )
            .child(
                div()
                    .id("new-kind-add")
                    .flex_none()
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .bg(t.accent)
                    .text_color(t.accent_fg)
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| s.add_kind(cx)))
                    .child(Icon::Plus.el(11., t.accent_fg))
                    .child("Add"),
            )
            .into_any_element()
    }

    fn control(&self, t: &Theme, control: Control, words: &[String], id: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        match control {
            Control::Color { key, current } => self.color_control(t, key, current, cx),
            Control::Edit { key, actions } => self.edit_control(t, key, actions, window, cx),
            Control::AddKind => self.add_kind_control(t, window, cx),
            // many choices: a menu instead of a row of buttons
            Control::Seg { key, options, current } if options.len() > 5 => self.choice_menu(t, key, options, current, cx),
            Control::Seg { key, options, current } => {
                let mut seg = div().flex().flex_wrap().justify_end().p(px(2.)).gap(px(1.)).rounded(px(7.)).border_1().border_color(t.line).bg(t.bg);
                for (i, (v, label)) in options.into_iter().enumerate() {
                    let on = v == current;
                    let soon = key == "webhooks.path" && crate::ui::triggers::SOON.contains(&v.as_str());
                    let found = !words.is_empty() && words.iter().any(|w| label.to_lowercase().contains(w.as_str()));
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
                            .gap(px(6.))
                            .when(!soon, |d| d.cursor_pointer())
                            .when(on, |d| {
                                d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::BOLD).shadow(vec![BoxShadow {
                                    color: hsla(0., 0., 0., 0.2),
                                    offset: point(px(0.), px(1.)),
                                    blur_radius: px(2.),
                                    spread_radius: px(0.),
                                    inset: false,
                                }])
                            })
                            .when(found, |d| d.border_1().border_color(t.accent))
                            .when(!on && !soon, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                            .when(soon && !on, |d| d.text_color(t.dim).opacity(0.6))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if !on && !soon {
                                    s.set(&k, json!(v), cx);
                                }
                            }))
                            .child(label)
                            .when(soon, |d| d.child(crate::ui::triggers::soon_pill(t))),
                    );
                }
                seg.into_any_element()
            }
            Control::Switch { key, on, on_text, off_text } => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().text_size(px(12.)).text_color(t.dim).child(if on { on_text } else { off_text }))
                .child(
                    div()
                        .id(SharedString::from(format!("sw-{key}")))
                        .relative()
                        .flex_none()
                        .w(px(36.))
                        .h(px(20.))
                        .rounded(px(10.))
                        .bg(if on { t.accent } else { t.line })
                        .cursor_pointer()
                        .on_click(cx.listener(move |s, _, _, cx| s.set(&key, json!(!on), cx)))
                        .child(div().absolute().top(px(2.)).left(px(if on { 18. } else { 2. })).size(px(16.)).rounded_full().bg(gpui_kit::white())),
                )
                .into_any_element(),
            Control::Sound { cat } => self.sound_control(t, cat, cx),
            Control::Theme { key, current } => self.theme_control(t, key, current, cx).into_any_element(),
            Control::Volume { key } => self.volume_stepper(t, &key, None, cx).into_any_element(),
            Control::Image { key, cat } => self.image_control(t, &key, cat, cx),
            Control::Keys { setting, keys } => self.keys_control(t, setting, keys, id, cx),
            Control::Text { dot, text, color, action } => {
                let color = if color == Hsla::default() { t.fg } else { color };
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .when_some(dot, |d, c| d.child(div().size(px(7.)).rounded_full().flex_none().bg(c)))
                    .child(div().min_w_0().text_color(color).truncate().child(text))
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
                                        b.bg(if color == t.need { t.need } else { t.accent }).text_color(if color == t.need { t.badge_fg } else { t.accent_fg })
                                    } else {
                                        b.border_1().border_color(t.line).bg(t.raised).text_color(t.fg).hover(|s| s.border_color(t.dim))
                                    }
                                })
                                .on_click(cx.listener(move |s, _, _, cx| s.run(&act, cx)))
                                .child(label),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

impl SettingsWindow {
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

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{Control, Group, Hsla, LAYOUT, RowSpec, Sec, Shown, Who, brief, excerpt, search};
    use midna_proto::notify::{CATEGORIES, EFFECTS};
    use midna_proto::settings::{SETTINGS, setting};

    fn row(label: &str, note: &str, cli: &str) -> RowSpec {
        let text = Control::Text { dot: None, text: String::new(), color: Hsla::default(), action: None };
        RowSpec { label: label.into(), note: Some((note.into(), Hsla::default())), control: text, cli: cli.into(), who: Who::Agents, warn: false }
    }

    fn seg(label: &str, key: &str, options: &[&str]) -> RowSpec {
        let options = options.iter().map(|o| (o.to_lowercase(), o.to_string())).collect();
        RowSpec { control: Control::Seg { key: key.into(), options, current: String::new() }, ..row(label, "", &format!("midna settings set {key} x")) }
    }

    fn group(name: &'static str, rows: Vec<RowSpec>) -> Group {
        Group { name, note: None, rows }
    }

    fn sample() -> Vec<(Sec, Vec<Group>)> {
        vec![
            (Sec::Appearance, vec![group("Theme", vec![row("Theme", "", "midna settings set theme dusk"), seg("Sidebar density", "density", &["Comfortable", "Compact"])])]),
            (Sec::Agents, vec![group("Lifecycle", vec![seg("After an agent update", "agents.restart_on_update", &["Ask", "Restart when idle", "Never"])])]),
            (Sec::Sounds, vec![group("", vec![row("Needs you", "Plays when an agent waits", "midna settings set notify.sound.need ping")])]),
        ]
    }

    /// A row's name, and why it matched when that isn't its name or description.
    type Match = (String, Option<String>);

    /// Section → rows.
    fn found(query: &str) -> Vec<(Sec, Vec<Match>)> {
        let names = |gs: Vec<Shown>| gs.into_iter().flat_map(|g| g.rows).map(|h| (h.row.label, h.via.map(|(what, text)| format!("{what}: {text}")))).collect();
        search(sample(), query).into_iter().map(|(s, gs)| (s, names(gs))).collect()
    }

    fn row_names(query: &str) -> Vec<(Sec, Vec<String>)> {
        found(query).into_iter().map(|(s, rows)| (s, rows.into_iter().map(|(l, _)| l).collect())).collect()
    }

    #[test]
    fn search_keeps_only_sections_and_rows_that_match() {
        assert!(found("  ").is_empty());
        assert_eq!(row_names("THEME"), vec![(Sec::Appearance, vec!["Theme".into(), "Sidebar density".into()])]);
        assert_eq!(row_names("agent waits"), vec![(Sec::Sounds, vec!["Needs you".into()])]);
        // every word, anywhere in the row
        assert_eq!(row_names("agent idle"), vec![(Sec::Agents, vec!["After an agent update".into()])]);
        assert!(found("make ⌘T open Claude").is_empty());
        // a section's name finds all of it
        assert_eq!(row_names("sounds"), vec![(Sec::Sounds, vec!["Needs you".into()])]);
    }

    #[test]
    fn search_says_why_a_row_matched() {
        // in the name or description: nothing to explain
        assert_eq!(found("update"), vec![(Sec::Agents, vec![("After an agent update".into(), None)])]);
        // an option, then the key, then related words
        assert_eq!(found("restart"), vec![(Sec::Agents, vec![("After an agent update".into(), Some("Option: Restart when idle".into()))])]);
        assert_eq!(found("notify.sound"), vec![(Sec::Sounds, vec![("Needs you".into(), Some("Key: notify.sound.need".into()))])]);
        assert_eq!(found("spacing"), vec![(Sec::Appearance, vec![("Sidebar density".into(), Some("Related: spacing compact".into()))])]);
    }

    #[test]
    fn notes_show_their_first_sentence() {
        assert_eq!(brief("Spacing of rows. Compact fits more."), "Spacing of rows.");
        assert_eq!(brief("Folders (e.g. ~/Dev). More here."), "Folders (e.g. ~/Dev).");
        assert_eq!(brief("One sentence only."), "One sentence only.");
        assert_eq!(brief("Which channel. Beta gets builds first. (default: stable)"), "Which channel. (default: stable)");
        let long = "Script that renders the terminal header line: built-in parts joined with + (github, agent, worktree) or a path.";
        assert_eq!(brief(long), "Script that renders the terminal header line.");
        // a search shows the sentence that matched
        assert_eq!(excerpt("Spacing of rows. Compact fits more. Done.", &["fits".into()]), "… Compact fits more.");
    }

    #[test]
    fn every_setting_has_a_place() {
        use midna_proto::notify::{bell_key, body_key, color_key, image_key, push_focused_key, push_key, setting_key, sound_key, stay_key, title_key, volume_key};
        let listed: Vec<&str> = LAYOUT.iter().flat_map(|(_, _, items)| items.iter().copied()).filter(|i| !i.starts_with('@')).collect();
        for k in &listed {
            assert!(setting(k).is_some(), "LAYOUT lists {k}, which isn't a setting");
        }
        // the per-kind rows (`@kinds`, `@sounds`, `@images`, …) and `@shortcuts`
        let kinds: [fn(&str) -> String; 11] = [setting_key, push_key, push_focused_key, sound_key, volume_key, image_key, title_key, body_key, stay_key, color_key, bell_key];
        let per_kind = |k: &str| {
            CATEGORIES.iter().any(|c| kinds.iter().any(|f| f(c.key) == k))
                || EFFECTS.iter().any(|e| sound_key(e.key) == k || volume_key(e.key) == k)
                || ["notify.volume", "notify.image"].contains(&k)
        };
        for s in SETTINGS {
            assert!(listed.contains(&s.key) || s.key.starts_with("keys.") || per_kind(s.key), "{} has no place in Settings (add it to LAYOUT)", s.key);
        }
    }

    #[test]
    fn every_special_row_is_built() {
        let known = [
            "@update", "@cli", "@login", "@hooks.claude", "@hooks.codex", "@kass", "@accessibility", "@notifications", "@version", "@daemon", "@reset_settings", "@reset_midna",
            "@kinds", "@banners", "@banners_focused", "@images", "@texts", "@sounds", "@effects", "@shortcuts", "@built_in", "@stay", "@colors",
            "@custom_kinds", "@bell",
        ];
        for (_, _, items) in LAYOUT {
            for i in items.iter().filter(|i| i.starts_with('@')) {
                assert!(known.contains(i), "{i} isn't handled by special_rows");
            }
        }
    }
}
