//! Main window state and behavior. Rendering lives in `ui/`.
use crate::actions::*;
use crate::backend::{Backend, BackendEvent, ConnState};
use crate::model::*;
use crate::terminal::TerminalView;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::ROOT_PROJECT_ID;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long any connection drop reads as "midnad restarting…" before "not running".
pub const RESTART_GRACE: Duration = Duration::from_secs(3);

/// What fills the area right of the sidebar. Rules / Triggers / Insights replace the
/// terminal pane (the sidebar stays). Next agents: swap the placeholder views in
/// `ui::screens` for the real ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Terminal,
    Rules,
    Triggers,
    Insights,
}

/// Modal layers over the main window (CommandBar-A, NeedsYou-C, the image sheet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
    None,
    CommandBar,
    NeedsYou,
    /// Image annotations (`annotate.rs`).
    Annotate,
}

/// Small popover menus the main window can show (one at a time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Menu {
    None,
    Approve,
    Project(String),
    More,
    /// The header's session links popover (`ui/links.rs`).
    Links,
    /// The header's subagents popover (`ui/subagents.rs`).
    Subagents,
    /// The header's Open in IDE menu (`ide.rs`).
    Ide,
    /// Right-click menu on the sidebar's Background heading (`ui/sidebar.rs`).
    Background,
    /// Right-click menu on the status bar: show/hide its items (`ui/statusbar.rs`).
    StatusBar,
    /// Right-click menu on the header toolbar: show/hide its buttons (`ui/header.rs`).
    HeaderButtons,
}

/// What to re-fetch after an event. Coalesced and run together.
pub mod refresh {
    pub const PROJECTS: u32 = 1;
    pub const SESSIONS: u32 = 2;
    pub const NEEDS: u32 = 4;
    pub const SETTINGS: u32 = 8;
    pub const INSIGHTS: u32 = 16;
    pub const RULES: u32 = 32;
    pub const WEBHOOKS: u32 = 64;
    pub const HEADER: u32 = 128;
    pub const ROWS: u32 = 256;
    pub const HOOKS: u32 = 512;
    pub const ALL: u32 = 0x3ff;
}

pub struct MainWindow {
    pub backend: Arc<dyn Backend>,
    pub conn: ConnState,
    pub projects: Vec<Project>,
    /// Folders under `projects.roots` (`project.discover`), refreshed with projects and settings.
    pub discovered: Vec<ProjectCandidate>,
    pub sessions: Vec<Session>,
    pub needs: Vec<NeedsYou>,
    pub settings: HashMap<String, Value>,
    pub today: Today,
    pub rules_count: usize,
    pub webhooks: Value,
    /// `hooks.status`: midna's hooks in the agents' global config (`ui/hooks.rs`).
    pub hooks: Value,
    pub hooks_sheet: Option<crate::ui::hooks::HooksSheet>,
    /// The Accessibility card Kass's first dictation raises (`ui/ax_prompt.rs`).
    pub ax_prompt: bool,
    pub onboarding: crate::ui::onboarding::Onboarding,
    /// The opening (`ui/twilight.rs`): which play, its phase, whether it runs with Reduce motion
    /// and over the setup screen, when it started, how long it runs (design ms), and the window
    /// drawing its tiles over the main window.
    pub twilight_seq: u64,
    pub twilight_phase: crate::ui::twilight::Phase,
    pub twilight_reduced: bool,
    pub twilight_setup: bool,
    pub twilight_clock: std::rc::Rc<crate::ui::twilight::Clock>,
    /// When (design ms) every cell shows the window.
    pub twilight_revealed: f32,
    pub twilight_total: f32,
    pub twilight_overlay: Option<AnyWindowHandle>,
    /// Whether the traffic lights are showing (hidden during the opening and on the setup screen).
    pub twilight_lights: bool,
    /// The opening hasn't revealed the whole window yet (see-through, masked, no chrome).
    pub twilight_masked: bool,
    pub triggers_count: usize,
    /// Inline rename in progress (double-click a terminal's name).
    pub renaming: Option<crate::ui::rename::Rename>,
    /// A busy terminal ⌘W was pressed on; a second ⌘W within 2 s closes it.
    close_armed: Option<(String, Instant)>,
    /// ⌘Q is being held since then (see `ui::quit_hold`).
    pub quit_hold: Option<Instant>,
    /// Status bar items the user has clicked at least once (`$MIDNA_HOME/app-state.json`).
    pub seen: std::collections::HashSet<String>,
    /// Sidebar project groups folded to their heading (project ids; same file as `seen`).
    pub collapsed: std::collections::HashSet<String>,
    /// The sidebar is collapsed to its rail (`ui::sidebar::rail`; same file as `seen`).
    pub sidebar_collapsed: bool,
    /// The sidebar's Background group is unfolded (same file as `seen`; folded by default).
    pub background_open: bool,
    /// The Background group is left out of the sidebar and rail entirely (same file as `seen`).
    /// ⌘K "Show background terminals in the sidebar" brings it back.
    pub background_hidden: bool,
    /// Terminal ids in the order the user dragged them to in the sidebar (same file as `seen`).
    /// Terminals not listed keep daemon order, after the listed ones.
    pub order: Vec<String>,
    /// When each group heading was last clicked, to animate the fold (`ui::sidebar`).
    pub fold_anim: HashMap<String, Instant>,
    /// Natural height of each foldable run of sidebar rows, measured at prepaint.
    pub fold_heights: std::rc::Rc<std::cell::RefCell<HashMap<String, f32>>>,
    pub selected: Option<String>,
    /// This window's id in `windows` (which terminals it shows).
    pub id: EntityId,
    pub windows: crate::windows::Shared,
    /// A sidebar row drag in progress: the rows moving and where the cursor is (window coords),
    /// to move them to another window when released outside this one.
    pub drag_out: Option<(Vec<String>, Point<Pixels>)>,
    /// Closing this window with terminals in it: close them or move them? (`ui::close_window`)
    pub close_ask: Option<crate::ui::close_window::CloseAsk>,
    /// Sidebar multi-selection (⌘-click toggles a row, ⌘⇧-click selects a range): every
    /// selected terminal id, `selected` included, or empty when just `selected` is.
    pub marked: Vec<String>,
    /// Where a ⌘⇧-click range starts: the last row clicked without ⇧.
    pub mark_anchor: Option<String>,
    pub screen: Screen,
    pub overlay: Overlay,
    pub menu: Menu,
    pub header_segments: HashMap<String, Vec<Segment>>,
    pub row_segments: HashMap<String, Vec<Segment>>,
    /// The status bar's script items, per terminal they ran for: `script` (`ui.status.script`)
    /// and each script path in `ui.status.items`.
    pub status_segments: HashMap<String, HashMap<String, Vec<Segment>>>,
    /// Custom header buttons' looks (`ui.header.buttons`), per terminal: script path → segments.
    pub header_buttons: HashMap<String, HashMap<String, Vec<Segment>>>,
    /// Where the status bar or header toolbar was right-clicked (its menu opens there).
    pub status_menu_at: Point<Pixels>,
    pub terminal: Option<Entity<TerminalView>>,
    /// The next terminal while it attaches; `terminal` stays on screen until this has a frame.
    pending_terminal: Option<(Entity<TerminalView>, Subscription)>,
    /// Second terminal pane (`ui/split.rs`), next to or under `terminal`.
    pub split: Option<crate::ui::split::Split>,
    pub focus: FocusHandle,
    pub overlay_focus: FocusHandle,
    pub toast: Option<(String, Instant)>,
    /// Rules-B / Triggers-A screens, created the first time they are shown.
    pub rules_view: Option<Entity<crate::ui::rules::RulesView>>,
    pub triggers_view: Option<Entity<crate::ui::triggers::TriggersView>>,
    /// Insights (graphs), created the first time it is shown.
    pub insights: Option<Entity<crate::ui::insights::InsightsView>>,
    /// ⌘K command bar (CommandBar-A) and needs-you card stack (NeedsYou-C) state.
    pub palette: crate::ui::command_bar::Palette,
    pub stack: crate::ui::needs_you::Stack,
    /// Session links popover (⌘L) and each terminal's links.
    pub links: crate::ui::links::LinksPanel,
    pub subagents: crate::ui::subagents::SubagentsPanel,
    /// The queued-messages panel (⌘U).
    pub queue: Entity<crate::ui::queue::QueueView>,
    /// Installed IDEs and the header's Open in IDE menu.
    pub ide: crate::ide::IdePanel,
    /// Native composer for Kass dictation and long prompts (`composer.rs`).
    pub composer: crate::composer::Composer,
    /// The image sheet (`annotate.rs`); drafts are in the `Drafts` global.
    pub annot: Entity<crate::annotate::AnnotateView>,
    /// When the daemon connection last dropped (None while connected). The status bar says
    /// "midnad restarting…" for the first seconds of any drop, or until reconnect when a
    /// restart/upgrade was seen in the log just before.
    pub dropped_at: Option<Instant>,
    pub restart_expected: bool,
    pending_refresh: u32,
    refresh_scheduled: bool,
    loaded: bool,
    _tasks: Vec<Task<()>>,
}

impl MainWindow {
    pub fn new(backend: Arc<dyn Backend>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let seen = crate::ui::statusbar::load_state(&backend, "seen");
        let collapsed = crate::ui::statusbar::load_state(&backend, "collapsed");
        let order = crate::ui::statusbar::load_state(&backend, "order");
        let sidebar_collapsed = crate::ui::statusbar::load_state(&backend, "sidebar_collapsed");
        let background_open = crate::ui::statusbar::load_state(&backend, "background_open");
        let background_hidden = crate::ui::statusbar::load_state(&backend, "background_hidden");
        let onboarding = crate::ui::onboarding::load(&backend);
        // Main windows open see-through; only the launch opening keeps it that way for a moment.
        let intro = crate::ui::twilight::wanted();
        if !intro {
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
            crate::ui::twilight::native_bg(window, cx.global::<Theme>().bg);
        }
        let id = cx.entity_id();
        let windows = crate::windows::register(cx.weak_entity(), id, window.window_handle(), cx);
        cx.on_release(|m: &mut MainWindow, cx| crate::windows::closed(m.id, cx)).detach();
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |window, cx| weak.update(cx, |m, cx| crate::ui::close_window::should_close(m, window, cx)).unwrap_or(true));
        cx.observe_window_bounds(window, |_, _, cx| crate::windows::save_soon(cx)).detach();
        let (tx, rx) = async_channel::unbounded::<BackendEvent>();
        backend.subscribe(tx);
        let mut tasks = vec![];
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this.update_in(cx, |m, window, cx| m.on_backend_event(ev, window, cx)).is_err() {
                    break;
                }
            }
        }));
        // Periodic refresh of script segments (git data changes outside of events).
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(15)).await;
                if this.update(cx, |m, cx| m.request_refresh(refresh::HEADER | refresh::ROWS | refresh::INSIGHTS, cx)).is_err() {
                    break;
                }
            }
        }));
        cx.observe_window_appearance(window, |m, window, cx| m.apply_theme(window, cx)).detach();
        // Custom theme files ($MIDNA_HOME/themes) are edited by hand or by agents: follow them live.
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let gone = this.update_in(cx, |m, window, cx| {
                    if crate::theme::themes_changed(&m.home()) {
                        m.apply_theme(window, cx);
                    }
                });
                if gone.is_err() {
                    break;
                }
            }
        }));
        cx.observe_window_activation(window, |m, window, cx| {
            if window.is_window_active() {
                crate::windows::focused(m.id, cx);
            } else {
                crate::ui::quit_hold::cancel(m, "window inactive", cx);
            }
        })
        .detach();
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        let palette = crate::ui::command_bar::Palette::new(cx);
        crate::ui::command_bar::wire(&palette, cx);
        let links = crate::ui::links::LinksPanel::new(cx);
        if !cx.has_global::<crate::ui::queue::QueueStore>() {
            crate::ui::queue::init(cx);
        }
        let queue = crate::ui::queue::new_for_main(backend.clone(), window, cx);
        if !cx.has_global::<crate::annotate::Drafts>() {
            crate::annotate::init(cx);
        }
        let annot = crate::annotate::new_for_main(window, cx);
        crate::ui::links::wire(&links, cx);
        crate::ide::detect(cx);
        let mut this = MainWindow {
            backend,
            conn: ConnState::Connecting,
            projects: vec![],
            discovered: vec![],
            sessions: vec![],
            needs: vec![],
            settings: HashMap::new(),
            today: Today::default(),
            rules_count: 0,
            triggers_count: 0,
            renaming: None,
            close_armed: None,
            quit_hold: None,
            seen,
            collapsed,
            sidebar_collapsed,
            background_open,
            background_hidden,
            order,
            fold_anim: HashMap::new(),
            fold_heights: Default::default(),
            webhooks: Value::Null,
            hooks: Value::Null,
            hooks_sheet: None,
            ax_prompt: false,
            onboarding,
            twilight_seq: 0,
            twilight_phase: crate::ui::twilight::Phase::Off,
            twilight_reduced: false,
            twilight_setup: false,
            twilight_clock: Default::default(),
            twilight_revealed: 0.,
            twilight_total: 0.,
            twilight_overlay: None,
            twilight_lights: true,
            twilight_masked: false,
            selected: crate::dev::var("MIDNA_SELECT").ok(),
            id,
            windows,
            drag_out: None,
            close_ask: None,
            marked: vec![],
            mark_anchor: None,
            screen: Screen::Terminal,
            overlay: Overlay::None,
            menu: Menu::None,
            header_segments: HashMap::new(),
            row_segments: HashMap::new(),
            status_segments: HashMap::new(),
            status_menu_at: Point::default(),
            header_buttons: HashMap::new(),
            terminal: None,
            pending_terminal: None,
            split: None,
            focus,
            overlay_focus: cx.focus_handle(),
            toast: None,
            rules_view: None,
            triggers_view: None,
            insights: None,
            palette,
            stack: crate::ui::needs_you::Stack::new(cx),
            links,
            subagents: crate::ui::subagents::SubagentsPanel::new(cx),
            queue,
            composer: crate::composer::Composer::new(window, cx),
            annot,
            dropped_at: None,
            ide: crate::ide::IdePanel::new(cx),
            restart_expected: false,
            pending_refresh: 0,
            refresh_scheduled: false,
            loaded: false,
            _tasks: tasks,
        };
        if intro {
            crate::ui::twilight::start(&mut this, window, cx);
        }
        this
    }

    // ------------------------------------------------------------------ settings

    pub fn setting_str(&self, key: &str) -> Option<String> {
        self.settings.get(key).and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        })
    }

    pub fn key_label(&self, key: &str) -> String {
        let k = self.setting_str(key).unwrap_or_else(|| crate::actions::default_key(key).to_string());
        pretty(&k)
    }

    pub fn compact(&self) -> bool {
        self.setting_str("density").as_deref() == Some("compact")
    }

    /// The theme the settings pick right now (`theme`, or `theme.dark` / `theme.light` by the
    /// window's appearance), with `theme.colors` applied.
    pub fn resolve_theme(&self, window: &Window) -> midna_proto::themes::ThemeDef {
        let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        let s = |k: &str| self.setting_str(k).unwrap_or_default();
        let id = midna_proto::themes::choose(&s("theme"), &s("theme.dark"), &s("theme.light"), system_dark);
        self.theme_def(&id, system_dark)
    }

    /// Theme `id` (built-in or custom; unknown falls back by appearance) with `theme.colors` applied.
    pub fn theme_def(&self, id: &str, system_dark: bool) -> midna_proto::themes::ThemeDef {
        let rules: Vec<String> = match self.settings.get("theme.colors") {
            Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
            _ => vec![],
        };
        midna_proto::themes::resolve(&crate::theme::cached_themes(), id, system_dark, &rules)
    }

    /// `$MIDNA_HOME` (where the daemon's socket lives).
    pub fn home(&self) -> std::path::PathBuf {
        self.backend.socket_path().parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(midna_proto::paths::midna_home)
    }

    pub fn apply_theme(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Re-reads custom theme files when they changed (`theme_def` then uses the cache).
        crate::theme::all_themes(&self.home());
        let def = self.resolve_theme(window);
        self.show_theme(&def, window, cx);
    }

    fn show_theme(&mut self, def: &midna_proto::themes::ThemeDef, window: &mut Window, cx: &mut Context<Self>) {
        let old = cx.global::<Theme>().clone();
        let mut t = Theme::from_def(def, old.ui_font.clone(), old.mono_font.clone());
        t.compact = self.compact();
        let bg = t.bg;
        cx.set_global(t);
        report_terminal_colors(&self.backend, def);
        crate::theme::cache_def(&self.backend, def);
        if !self.twilight_masked {
            crate::ui::twilight::native_bg(window, bg);
        }
        window.refresh();
        cx.notify();
    }

    fn rebind(&self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        bind_keys(cx, |k| {
            settings.get(k).map(|v| match v {
                Value::String(s) => s.clone(),
                _ => String::new(),
            })
        });
    }

    // ------------------------------------------------------------------ backend

    fn on_backend_event(&mut self, ev: BackendEvent, window: &mut Window, cx: &mut Context<Self>) {
        if crate::dev::var("MIDNA_DEBUG").is_ok() {
            eprintln!("midna-app: backend event {ev:?}");
        }
        crate::ui::rules::on_backend_event(self, &ev, cx);
        crate::ui::triggers::on_backend_event(self, &ev, cx);
        match ev {
            BackendEvent::Conn(st) => {
                let was_connected = self.conn == ConnState::Connected;
                self.conn = st.clone();
                match st {
                    ConnState::Connected => {
                        self.request_refresh(refresh::ALL, cx);
                        self.dropped_at = None;
                        self.restart_expected = false;
                        crate::ui::command_bar::reload_user_commands(self, cx);
                        if !was_connected {
                            // re-attach after a reconnect (the split pane and pop-outs
                            // re-attach themselves, see TerminalView::watch_stream)
                            self.terminal = None;
                            self.pending_terminal = None;
                            self.ensure_terminal(window, cx);
                            if self.backend.label() == "midnad" && self.is_home() {
                                crate::lifecycle::report_updates(self.backend.clone());
                            }
                        }
                    }
                    ConnState::NotRunning { .. } => {
                        self.terminal = None;
                        self.pending_terminal = None;
                        if was_connected || self.dropped_at.is_none() {
                            self.dropped_at = Some(Instant::now());
                            // Re-render when the "restarting" grace period runs out.
                            cx.spawn(async move |this, cx| {
                                cx.background_executor().timer(RESTART_GRACE + Duration::from_millis(100)).await;
                                let _ = this.update(cx, |_, cx| cx.notify());
                            })
                            .detach();
                        }
                    }
                    ConnState::Connecting => {}
                }
                cx.notify();
            }
            BackendEvent::Event(e) => {
                if let Some(v) = self.insights.clone() {
                    v.update(cx, |v, cx| v.on_event(&e, cx));
                }
                let k = e.kind.as_str();
                let mut what = 0;
                if k.starts_with("project.") {
                    what |= refresh::PROJECTS;
                }
                if k == midna_proto::kinds::SESSION_CUSTOM_STATUS
                    && let Some(sess) = e.session_id.as_ref().and_then(|id| self.sessions.iter_mut().find(|s| &s.id == id))
                {
                    // Show it now; the session.list refetch below confirms it.
                    sess.custom_status = e.data.get("custom_status").cloned().and_then(|v| serde_json::from_value(v).ok());
                }
                if k.starts_with("session.") {
                    what |= refresh::SESSIONS | refresh::ROWS;
                    if e.session_id.is_some() && e.session_id == self.selected {
                        what |= refresh::HEADER;
                    }
                }
                if k.starts_with("needs_you.") {
                    what |= refresh::NEEDS | refresh::SESSIONS;
                }
                if k == "settings.changed" {
                    what |= refresh::SETTINGS;
                    let key = e.data.get("key").and_then(|v| v.as_str()).unwrap_or("");
                    if key.starts_with("ui.") {
                        what |= refresh::HEADER | refresh::ROWS;
                    }
                    if key.starts_with("webhooks.") {
                        what |= refresh::WEBHOOKS;
                    }
                    if key.starts_with("policy.") {
                        what |= refresh::RULES;
                    }
                }
                if k.starts_with("agent.") || k.starts_with("trigger.") {
                    what |= refresh::INSIGHTS;
                }
                if k.starts_with("trigger.") {
                    what |= refresh::WEBHOOKS;
                }
                if k.starts_with("rule.") {
                    what |= refresh::RULES;
                }
                if k == midna_proto::kinds::HOOKS_CHANGED {
                    self.hooks = e.data.clone();
                }
                if k == "window.command" {
                    self.on_window_command(e.data.clone(), window, cx);
                }
                if k == midna_proto::kinds::SESSION_QUEUE {
                    crate::ui::queue::on_event(self, &e, cx);
                    if self.is_home() && e.data.get("action").and_then(|a| a.as_str()) == Some("sent") {
                        crate::sounds::play("queue_sent");
                    }
                }
                if k == "links.changed"
                    && let Some(sid) = e.session_id.clone().filter(|s| self.selected.as_ref() == Some(s))
                {
                    crate::ui::links::fetch(self, sid, cx);
                }
                if k == midna_proto::kinds::NOTIFY_POSTED && self.is_home() {
                    self.on_notification(&e, window, cx);
                }
                if k == midna_proto::kinds::NOTIFY_SOUND && self.is_home() {
                    let fresh = midna_proto::time::parse_rfc3339(&e.at).is_some_and(|t| midna_proto::time::now_unix() - t < 30);
                    if let Ok(p) = serde_json::from_value::<midna_proto::notify::Played>(e.data.clone())
                        && fresh
                        && p.via == "app"
                    {
                        crate::sounds::play_file(&p.file, p.volume);
                    }
                }
                if k == "ui.commands_changed" {
                    crate::ui::command_bar::reload_user_commands(self, cx);
                }
                // An accepted restart/upgrade: the drop that follows is expected.
                if k == "audit"
                    && matches!(e.data.get("method").and_then(|m| m.as_str()), Some("daemon.restart" | "daemon.upgrade"))
                    && e.data.get("outcome").and_then(|o| o.as_str()) == Some("ok")
                {
                    self.restart_expected = true;
                }
                if what != 0 {
                    self.request_refresh(what, cx);
                }
            }
            BackendEvent::WindowCommand(v) if self.is_home() => {
                // A command about one terminal goes to the window it shows in.
                let target = v.get("target").and_then(|t| t.as_str()).and_then(|t| self.windows.borrow().place(t)).filter(|w| *w != self.id);
                match target {
                    Some(w) => cx.defer(move |cx| crate::windows::command(w, v, cx)),
                    None => self.on_window_command(v, window, cx),
                }
            }
            BackendEvent::WindowCommand(_) => {}
            BackendEvent::Notification { method, params } => {
                if method == "updates.command" && self.is_home() {
                    let action = params.get("action").and_then(|a| a.as_str()).unwrap_or("").to_string();
                    if let Some(msg) = crate::lifecycle::on_daemon_command(&action, cx) {
                        self.toast(msg, cx);
                    }
                }
            }
        }
    }

    /// The main window focused last (see `windows`).
    fn is_home(&self) -> bool {
        self.windows.borrow().is_home(self.id)
    }

    /// Whether `session` shows in this window (not another main window).
    pub fn shows(&self, session: &str) -> bool {
        self.windows.borrow().shows(session, self.id)
    }

    /// midnad's `notify.posted`: show it unless you're looking at that terminal (then only its
    /// sound plays, see `crate::sounds`). Old ones (replayed after a reconnect) are dropped.
    fn on_notification(&self, e: &Event, window: &Window, cx: &App) {
        let Ok(p) = serde_json::from_value::<midna_proto::notify::Posted>(e.data.clone()) else { return };
        let fresh = midna_proto::time::parse_rfc3339(&e.at).is_some_and(|t| midna_proto::time::now_unix() - t < 30);
        if p.via != "app" || !fresh {
            return;
        }
        let when_focused = self.settings.get("notify.when_focused").and_then(Value::as_bool).unwrap_or(false);
        let on_screen = e.session_id.as_deref().is_some_and(|s| crate::windows::on_screen(s, window, self.id, self.selected.as_deref(), cx));
        if !p.test && crate::notify::looking_at(on_screen, e.session_id.as_deref(), e.session_id.as_deref(), when_focused) {
            if let (true, Some(file)) = (p.sound, p.sound_file.as_deref()) {
                crate::sounds::play_file(file, p.volume.unwrap_or(100));
            }
            return;
        }
        let mut p = p;
        // notify.sounds_in_app off: a banner shown while midna is frontmost stays quiet.
        if !p.test && !crate::sounds::allowed_now() {
            p.sound = false;
            p.notification_sound = None;
        }
        crate::notify::post(&p, e.session_id.as_deref());
    }

    /// "midnad restarting…" rather than "not running": during a restart we saw coming, or the
    /// first seconds of any drop (an upgrade by another client, launchd's restart).
    pub fn daemon_restarting(&self) -> bool {
        self.dropped_at.is_some_and(|t| self.restart_expected && t.elapsed() < Duration::from_secs(30) || t.elapsed() < RESTART_GRACE)
    }

    /// `window.command{action, target?, value?}` forwarded by the daemon.
    pub fn on_window_command(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let target = v.get("target").and_then(|a| a.as_str()).map(str::to_string);
        let value = v.get("value").and_then(|a| a.as_str()).unwrap_or("");
        match action {
            "front" => {
                // Like a clicked notification: midna becomes the frontmost app, showing the terminal.
                cx.activate(true);
                window.activate_window();
                if let Some(t) = target.filter(|t| self.sessions.iter().any(|s| &s.id == t)) {
                    if self.screen != Screen::Terminal {
                        self.set_screen(Screen::Terminal, window, cx);
                    }
                    self.select(t, window, cx);
                }
            }
            "open_screen" => {
                let screen = match value {
                    "rules" => Screen::Rules,
                    "triggers" => Screen::Triggers,
                    "insights" => Screen::Insights,
                    _ => Screen::Terminal,
                };
                // Opening is idempotent (set_screen toggles a screen that is already showing).
                if self.screen != screen {
                    self.set_screen(screen, window, cx);
                }
                if value == "settings" {
                    crate::ui::settings::open(self.backend.clone(), cx);
                }
            }
            "split" => match value {
                "close" => crate::ui::split::close(self, window, cx),
                _ => {
                    if let Some(t) = target.filter(|t| self.sessions.iter().any(|s| &s.id == t)) {
                        if self.selected.as_deref() == Some(t.as_str()) {
                            self.toast("That terminal is already in the main pane.", cx);
                        } else {
                            crate::ui::split::show(self, t, window, cx);
                            if let Some(s) = self.split.as_mut() {
                                s.stacked = value == "stacked";
                            }
                            self.screen = Screen::Terminal;
                        }
                    }
                }
            },
            "pop_out" | "keep_on_top" => {
                if let Some(t) = target.or(self.selected.clone()) {
                    crate::ui::popout::open(self, t, window, cx);
                }
            }
            _ => {}
        }
    }

    pub fn request_refresh(&mut self, what: u32, cx: &mut Context<Self>) {
        self.pending_refresh |= what;
        if self.refresh_scheduled {
            return;
        }
        self.refresh_scheduled = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(40)).await;
            let _ = this.update(cx, |m, cx| {
                m.refresh_scheduled = false;
                let what = std::mem::take(&mut m.pending_refresh);
                m.run_refresh(what, cx);
            });
        })
        .detach();
    }

    fn run_refresh(&mut self, what: u32, cx: &mut Context<Self>) {
        if self.conn != ConnState::Connected {
            return;
        }
        let backend = self.backend.clone();
        let header_for = self.selected.clone();
        let row_ids: Vec<String> = self.sessions.iter().map(|s| s.id.clone()).collect();
        let row_script_on = self.setting_str("ui.row.script").map(|s| s != "none" && !s.is_empty()).unwrap_or(false);
        let buttons_now = crate::ui::header::custom_buttons(self);
        let status_scripts = crate::ui::statusbar::scripts_wanted(self.settings.get("ui.status.items"), self.settings.get("ui.status.script"));
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    let mut r = RefreshResult::default();
                    let call = |m: &str, p: Value| match backend.call(m, p) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            eprintln!("midna-app: {m}: {e:#}");
                            None
                        }
                    };
                    if what & refresh::SETTINGS != 0 {
                        r.settings = call("settings.list", json!({})).map(|v| parse_list::<SettingEntry>(&v));
                    }
                    if what & refresh::PROJECTS != 0 {
                        r.projects = call("project.list", json!({})).map(|v| parse_list(&v));
                    }
                    if what & (refresh::PROJECTS | refresh::SETTINGS) != 0 {
                        r.discovered = call("project.discover", json!({})).map(|v| parse_list(&v));
                    }
                    if what & refresh::SESSIONS != 0 {
                        r.sessions = call("session.list", json!({})).map(|v| parse_list(&v));
                    }
                    if what & refresh::NEEDS != 0 {
                        r.needs = call("needs_you.list", json!({})).map(|v| parse_list(&v));
                    }
                    if what & refresh::INSIGHTS != 0 {
                        r.today = call("insights.summary", json!({"range": "today"})).map(|v| Today::from_value(&v));
                    }
                    if what & refresh::RULES != 0 {
                        r.rules = call("rule.list", json!({})).map(|v| parse_list::<Value>(&v).len());
                    }
                    if what & refresh::WEBHOOKS != 0 {
                        r.webhooks = call("webhooks.status", json!({}));
                        r.triggers = call("trigger.list", json!({})).map(|v| parse_list::<Value>(&v).len());
                    }
                    if what & refresh::HOOKS != 0 {
                        r.hooks = call("hooks.status", json!({}));
                    }
                    if what & refresh::HEADER != 0
                        && let Some(sid) = &header_for
                    {
                        let segs = call("script.run", json!({"session_id": sid, "slot": "header"})).map(|v| parse_list::<Segment>(&v)).unwrap_or_default();
                        r.header = Some((sid.clone(), segs));
                        let get = |k: &str| r.settings.as_ref().and_then(|s| s.iter().find(|e| e.key == k).map(|e| e.value.clone()));
                        let scripts = match (get("ui.status.items"), get("ui.status.script")) {
                            (None, None) => status_scripts,
                            (items, script) => crate::ui::statusbar::scripts_wanted(items.as_ref(), script.as_ref()),
                        };
                        let mut status = HashMap::new();
                        for item in scripts {
                            let p = if item == "script" { json!({"session_id": sid, "slot": "status"}) } else { json!({"session_id": sid, "slot": "status", "script": item}) };
                            status.insert(item, call("script.run", p).map(|v| parse_list::<Segment>(&v)).unwrap_or_default());
                        }
                        r.status = Some((sid.clone(), status));
                        let buttons: Vec<String> = get("ui.header.buttons").and_then(|v| serde_json::from_value(v).ok()).unwrap_or(buttons_now);
                        let mut looks = HashMap::new();
                        for path in buttons {
                            let p = json!({"session_id": sid, "slot": "button", "script": path});
                            looks.insert(path, call("script.run", p).map(|v| parse_list::<Segment>(&v)).unwrap_or_default());
                        }
                        r.buttons = Some((sid.clone(), looks));
                    }
                    if what & refresh::ROWS != 0 {
                        let ids = r.sessions.as_ref().map(|s| s.iter().map(|s| s.id.clone()).collect()).unwrap_or(row_ids);
                        let on = r
                            .settings
                            .as_ref()
                            .and_then(|s| s.iter().find(|e| e.key == "ui.row.script").map(|e| e.value.as_str().map(|v| v != "none" && !v.is_empty()).unwrap_or(false)))
                            .unwrap_or(row_script_on);
                        let mut rows = HashMap::new();
                        if on {
                            for id in ids {
                                if let Some(v) = call("script.run", json!({"session_id": id, "slot": "row"})) {
                                    rows.insert(id, parse_list::<Segment>(&v));
                                }
                            }
                        }
                        r.rows = Some(rows);
                    }
                    r
                })
                .await;
            let _ = this.update_in(cx, |m, window, cx| m.apply_refresh(res, window, cx));
        })
        .detach();
    }

    fn apply_refresh(&mut self, r: RefreshResult, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(s) = r.settings {
            let new: HashMap<String, Value> = s.into_iter().map(|e| (e.key, e.value)).collect();
            if new != self.settings {
                self.settings = new;
                crate::sounds::sync(&self.settings, self.backend.socket_path().parent());
                self.apply_theme(window, cx);
                self.rebind(cx);
                crate::terminal::set_option_as_meta(self.settings.get("terminal.option_as_meta").and_then(Value::as_bool).unwrap_or(true));
                crate::finder::sync(self.settings.get("finder.quick_action").and_then(Value::as_bool).unwrap_or(true));
                crate::haptics::sync(self.settings.get("ui.haptics").and_then(Value::as_bool).unwrap_or(true));
            }
        }
        if let Some(mut p) = r.projects {
            p.sort_by_key(|p| p.order);
            self.projects = p;
        }
        if let Some(d) = r.discovered {
            self.discovered = d;
        }
        crate::terminal::share_preview_env(self, cx);
        // The selected terminal closing (⌘K, the header menu, its shell exiting): its neighbour
        // in the sidebar we had, so focus stays in its project.
        let mut closed_neighbour = None;
        if let Some(s) = r.sessions {
            if let Some(sel) = self.selected.clone().filter(|id| self.sessions.iter().any(|x| &x.id == id) && !s.iter().any(|x| &x.id == id)) {
                let rows = self.sidebar_rows(Some(&sel), cx);
                closed_neighbour = neighbour(&rows, &sel, |id| s.iter().any(|x| x.id == id));
            }
            self.sessions = s;
            // New terminals (from an agent, a trigger, the CLI) land in the home window.
            if self.is_home() {
                let mut w = self.windows.borrow_mut();
                let new: Vec<&Session> = self.sessions.iter().filter(|s| !w.owned(&s.id)).collect();
                for s in &new {
                    w.claim(&s.id, self.id);
                }
                if !new.is_empty() {
                    drop(w);
                    crate::windows::save_soon(cx);
                }
            }
            let live = &self.sessions;
            self.marked.retain(|id| live.iter().any(|s| &s.id == id));
            if self.marked.len() < 2 {
                self.marked.clear();
            }
            crate::ui::queue::sync(&self.sessions, cx);
        }
        if let Some(n) = r.needs {
            self.needs = n;
        }
        if let Some(t) = r.today {
            self.today = t;
        }
        if let Some(n) = r.rules {
            self.rules_count = n;
        }
        if let Some(n) = r.triggers {
            self.triggers_count = n;
        }
        if let Some(w) = r.webhooks {
            self.webhooks = w;
        }
        if let Some(h) = r.hooks {
            self.hooks = h;
        }
        if let Some((sid, segs)) = r.header {
            if !self.links.by_session.contains_key(&sid) {
                crate::ui::links::fetch(self, sid.clone(), cx);
            }
            self.header_segments.insert(sid, segs);
        }
        if let Some((sid, segs)) = r.status {
            self.status_segments.insert(sid, segs);
        }
        if let Some((sid, looks)) = r.buttons {
            self.header_buttons.insert(sid, looks);
        }
        if let Some(rows) = r.rows {
            self.row_segments = rows;
        }
        // keep a valid selection
        // (a popped-out terminal lives in its own window, never in the main pane)
        let valid = self.selected.as_ref().is_some_and(|id| self.sessions.iter().any(|s| &s.id == id) && self.shows(id) && !crate::ui::popout::is_popped(id, cx));
        let docked: Vec<&Session> = self.ordered_sessions().into_iter().filter(|s| !crate::ui::popout::is_popped(&s.id, cx)).collect();
        let pick = (!valid)
            .then(|| {
                closed_neighbour
                    .filter(|id| docked.iter().any(|s| &s.id == id))
                    .or_else(|| docked.iter().find(|s| self.need_for_session(&s.id).is_some_and(|n| n.is_approval())).map(|s| s.id.clone()))
                    .or_else(|| docked.iter().find(|s| self.effective_state(s) == StatusState::NeedsYou).map(|s| s.id.clone()))
                    .or_else(|| docked.first().map(|s| s.id.clone()))
            })
            .flatten();
        if let Some(id) = pick {
            self.select(id, window, cx);
        } else if !valid {
            self.selected = None;
            self.terminal = None;
            self.pending_terminal = None;
        } else {
            self.ensure_terminal(window, cx);
        }
        if !self.loaded && !self.sessions.is_empty() {
            self.loaded = true;
            if crate::dev::var("MIDNA_DEBUG_APPROVE_MENU").is_ok() {
                self.menu = Menu::Approve;
            }
            if let Ok(keys) = crate::dev::var("MIDNA_DEBUG_KEYS") {
                // Dev: comma-separated keystrokes sent through GPUI's own dispatch (bindings,
                // key handlers, then the input handler), to exercise shortcuts and typing.
                cx.spawn_in(window, async move |_, cx| {
                    cx.background_executor().timer(Duration::from_millis(1200)).await;
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
            if let Ok(steps) = crate::dev::var("MIDNA_DEBUG_TERM") {
                crate::term_debug::start(steps, window, cx);
            }
            if let Ok(s) = crate::dev::var("MIDNA_DEBUG_SCREEN") {
                match s.as_str() {
                    "twilight" => crate::ui::twilight::replay(self, window, cx),
                    "rules" => self.screen = Screen::Rules,
                    "triggers" => self.screen = Screen::Triggers,
                    "insights" => self.screen = Screen::Insights,
                    "commands" => self.set_overlay(Overlay::CommandBar, window, cx),
                    "needs" => self.set_overlay(Overlay::NeedsYou, window, cx),
                    "annotate" | "annotate-tray" => crate::annotate::debug(self, &s, window, cx),
                    "queue" | "queue-sent" => crate::ui::queue::debug(self, &s, window, cx),
                    "background-menu" => self.menu = Menu::Background,
                    "close-window" => {
                        self.close_ask = Some(Default::default());
                    }
                    "hooks" => crate::ui::hooks::open(self, false, window, cx),
                    "ax" => self.ax_prompt = true,
                    "onboarding" => crate::ui::onboarding::reopen(self, cx),
                    "popout-queue" => {
                        if let Some(sid) = self.selected.clone() {
                            crate::ui::popout::debug_queue(self, sid, window, cx);
                        }
                    }
                    "popout-annotate" => {
                        if let Some(sid) = self.selected.clone() {
                            crate::ui::popout::debug_annotate(self, sid, window, cx);
                        }
                    }
                    _ => {}
                }
            }
        }
        cx.notify();
    }

    // ------------------------------------------------------------------ selection

    /// Sessions in sidebar order: grouped by project order, then sessions in dragged order
    /// (`order`), then daemon order. Background terminals come last, and only while their
    /// group is unfolded and not hidden (the selected one always), so ⌘]/⌘[ and auto-select
    /// skip them.
    pub fn ordered_sessions(&self) -> Vec<&Session> {
        let mut out = vec![];
        for g in self.groups() {
            out.extend(g.sessions);
        }
        let shown = |s: &&Session| (self.background_open && !self.background_hidden) || self.selected.as_deref() == Some(&s.id);
        out.extend(self.background_sessions().into_iter().filter(shown));
        out
    }

    /// The docked terminals in sidebar order (plus `keep`, even when popped out), each with
    /// the group it sits in, for [`neighbour`].
    fn sidebar_rows(&self, keep: Option<&str>, cx: &App) -> Vec<(String, RowGroup)> {
        self.ordered_sessions()
            .into_iter()
            .filter(|s| keep == Some(s.id.as_str()) || !crate::ui::popout::is_popped(&s.id, cx))
            .map(|s| (s.id.clone(), if s.background { RowGroup::Background } else { RowGroup::Project(s.project_id.clone()) }))
            .collect()
    }

    /// This window's background terminals (`Session::background`), in dragged order. They sit
    /// in the sidebar's Background group, not under their projects.
    pub fn background_sessions(&self) -> Vec<&Session> {
        let mut out: Vec<&Session> = self.sessions.iter().filter(|s| s.background && self.shows(&s.id)).collect();
        out.sort_by_key(|s| self.order.iter().position(|id| id == &s.id).unwrap_or(usize::MAX));
        out
    }

    pub fn groups(&self) -> Vec<Group<'_>> {
        let mut groups: Vec<Group> = self.projects.iter().map(|p| Group { project: Some(p), name: p.name.clone(), sessions: vec![] }).collect();
        let mut root = Group { project: None, name: "root".into(), sessions: vec![] };
        for s in self.sessions.iter().filter(|s| !s.background && self.shows(&s.id)) {
            match s.project_id.as_ref().and_then(|pid| groups.iter_mut().find(|g| g.project.is_some_and(|p| &p.id == pid))) {
                Some(g) => g.sessions.push(s),
                None => root.sessions.push(s),
            }
        }
        // A project with no terminals is closed: it leaves the sidebar but keeps its rules
        // and commands, and ⌘O / ⌘K "Go to project" reopen it.
        groups.retain(|g| !g.sessions.is_empty());
        let rank = |s: &Session| self.order.iter().position(|id| id == &s.id).unwrap_or(usize::MAX);
        for g in groups.iter_mut().chain(std::iter::once(&mut root)) {
            g.sessions.sort_by_key(|s| rank(s));
        }
        // Root terminals (no project) come first and are drawn without a heading.
        if !root.sessions.is_empty() {
            groups.insert(0, root);
        }
        groups
    }

    pub fn selected_session(&self) -> Option<&Session> {
        let id = self.selected.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id)
    }

    pub fn current_project_id(&self) -> Option<String> {
        self.selected_session().and_then(|s| s.project_id.clone()).or_else(|| self.groups().iter().find_map(|g| g.project).map(|p| p.id.clone()))
    }

    /// The status to show: a pending needs-you item for the session means "needs you" even
    /// when the session's own status (e.g. a shell) hasn't changed.
    pub fn effective_state(&self, s: &Session) -> StatusState {
        if s.status.state != StatusState::Failed && self.need_for_session(&s.id).is_some() { StatusState::NeedsYou } else { s.status.state }
    }

    pub fn need_for_session(&self, sid: &str) -> Option<&NeedsYou> {
        self.needs.iter().find(|n| n.session_id.as_deref() == Some(sid))
    }

    pub fn approval_for_selected(&self) -> Option<&NeedsYou> {
        let sid = self.selected.as_ref()?;
        self.needs.iter().find(|n| n.session_id.as_deref() == Some(sid) && n.is_approval())
    }

    pub fn needs_in_project(&self, pid: Option<&str>) -> usize {
        self.needs
            .iter()
            .filter(|n| {
                let p = n.project_id.clone().or_else(|| n.session_id.as_ref().and_then(|sid| self.sessions.iter().find(|s| &s.id == sid)).and_then(|s| s.project_id.clone()));
                p.as_deref() == pid
            })
            .count()
    }

    pub fn select(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        // Another main window has it: show it there. Nobody has it yet: it's this window's.
        if self.windows.borrow().owned(&id) {
            if !self.shows(&id) {
                cx.defer(move |cx| crate::windows::reveal(id, cx));
                return;
            }
        } else {
            self.windows.borrow_mut().claim(&id, self.id);
        }
        if crate::ui::popout::is_popped(&id, cx) {
            crate::ui::popout::open(self, id, window, cx);
            return;
        }
        // Showing a terminal outside the multi-selection ends it.
        if !self.marked.contains(&id) {
            self.marked.clear();
        }
        if self.selected.as_deref() != Some(&id) {
            crate::windows::save_soon(cx);
            self.selected = Some(id.clone());
            self.menu = Menu::None;
            self.request_refresh(refresh::HEADER, cx);
            crate::ui::links::fetch(self, id, cx);
        }
        if self.screen != Screen::Terminal {
            self.screen = Screen::Terminal;
        }
        self.ensure_terminal(window, cx);
        cx.notify();
    }

    /// Whether `id` is part of the sidebar selection.
    pub fn is_marked(&self, id: &str) -> bool {
        self.marked.iter().any(|m| m == id) || (self.marked.is_empty() && self.selected.as_deref() == Some(id))
    }

    /// The selected terminals in sidebar order: the multi-selection, else just `selected`.
    pub fn marked_ids(&self) -> Vec<String> {
        self.ordered_sessions().iter().map(|s| s.id.clone()).filter(|id| self.is_marked(id)).collect()
    }

    /// ⌘-click: add `id` to the selection and show it, or take it out (showing another
    /// selected terminal when it was the one on screen). The last one can't be taken out.
    pub fn toggle_mark(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if crate::ui::popout::is_popped(&id, cx) {
            self.select(id, window, cx);
            return;
        }
        if self.marked.is_empty() {
            self.marked.extend(self.selected.clone().filter(|s| !crate::ui::popout::is_popped(s, cx)));
        }
        self.mark_anchor = Some(id.clone());
        if let Some(i) = self.marked.iter().position(|m| m == &id) {
            if self.marked.len() < 2 {
                return;
            }
            self.marked.remove(i);
            let show = if self.selected.as_deref() == Some(&id) { self.marked.last().cloned() } else { self.selected.clone() };
            if self.marked.len() < 2 {
                self.marked.clear();
            }
            if let Some(show) = show {
                self.select(show, window, cx);
            }
        } else {
            self.marked.push(id.clone());
            if self.marked.len() < 2 {
                self.marked.clear();
            }
            self.select(id, window, cx);
        }
        cx.notify();
    }

    /// ⌘⇧-click: select every row from the anchor (the last row clicked without ⇧) to `id`,
    /// in sidebar order, skipping popped-out terminals and rows hidden in folded projects.
    pub fn mark_range(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let rows: Vec<String> = self
            .ordered_sessions()
            .into_iter()
            .filter(|s| !crate::ui::popout::is_popped(&s.id, cx))
            .filter(|s| self.selected.as_deref() == Some(&s.id) || !s.project_id.as_ref().is_some_and(|p| self.collapsed.contains(p)))
            .map(|s| s.id.clone())
            .collect();
        let anchor = self.mark_anchor.clone().or_else(|| self.selected.clone());
        let (Some(a), Some(b)) = (anchor.and_then(|a| rows.iter().position(|r| *r == a)), rows.iter().position(|r| *r == id)) else {
            self.toggle_mark(id, window, cx);
            return;
        };
        self.marked = rows[a.min(b)..=a.max(b)].to_vec();
        if self.marked.len() < 2 {
            self.marked.clear();
        }
        self.select(id, window, cx);
    }

    /// A plain click: just this terminal, which also becomes the anchor for ⌘⇧-click.
    pub fn select_only(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.as_deref() != Some(&id) {
            crate::sounds::play("switched");
        }
        self.marked.clear();
        self.mark_anchor = Some(id.clone());
        self.select(id, window, cx);
    }

    fn ensure_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.conn != ConnState::Connected {
            return;
        }
        let Some(id) = self.selected.clone() else {
            return;
        };
        if self.terminal.as_ref().is_some_and(|t| t.read(cx).session_id == id) {
            self.pending_terminal = None;
            return;
        }
        if self.pending_terminal.as_ref().is_some_and(|(t, _)| t.read(cx).session_id == id) {
            return;
        }
        let backend = self.backend.clone();
        let size = self.terminal.as_ref().map(|t| t.read(cx).size()).unwrap_or((0, 0));
        let view = cx.new(|cx| TerminalView::new_sized(id, backend, size, window, cx));
        if self.terminal.is_none() {
            self.show_terminal(view, window, cx);
            return;
        }
        // Switching: keep the current screen up until the next one has drawn, so the pane
        // doesn't flash blank. Keys go nowhere meanwhile rather than to the old terminal.
        self.focus.focus(window, cx);
        let sub = cx.observe_in(&view, window, |m, v, window, cx| m.promote_terminal(&v, false, window, cx));
        self.pending_terminal = Some((view.clone(), sub));
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let _ = this.update_in(cx, |m, window, cx| m.promote_terminal(&view, true, window, cx));
        })
        .detach();
    }

    fn promote_terminal(&mut self, view: &Entity<TerminalView>, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.pending_terminal.as_ref().is_some_and(|(p, _)| p == view) || !(force || view.read(cx).has_frame()) {
            return;
        }
        self.pending_terminal = None;
        self.show_terminal(view.clone(), window, cx);
        cx.notify();
    }

    fn show_terminal(&mut self, view: Entity<TerminalView>, window: &mut Window, cx: &mut Context<Self>) {
        // An inline rename started by the double-click that selected this terminal keeps focus.
        if self.overlay == Overlay::None && self.screen == Screen::Terminal && self.renaming.is_none() {
            let fh = view.read(cx).focus_handle().clone();
            fh.focus(window, cx);
        }
        self.terminal = Some(view);
    }

    pub fn focus_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(t) = &self.terminal {
            let fh = t.read(cx).focus_handle().clone();
            fh.focus(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
    }

    pub fn set_screen(&mut self, s: Screen, window: &mut Window, cx: &mut Context<Self>) {
        self.screen = if self.screen == s { Screen::Terminal } else { s };
        self.menu = Menu::None;
        crate::ui::queue::hide(self, cx);
        if self.screen == Screen::Terminal {
            self.focus_terminal(window, cx);
        } else {
            self.overlay_focus.focus(window, cx);
        }
        cx.notify();
    }

    pub fn set_overlay(&mut self, o: Overlay, window: &mut Window, cx: &mut Context<Self>) {
        let was = self.overlay;
        self.overlay = if self.overlay == o { Overlay::None } else { o };
        if was == Overlay::Annotate && self.overlay != Overlay::Annotate {
            self.annot.update(cx, |v, cx| v.hide(window, cx));
        }
        self.menu = Menu::None;
        crate::ui::queue::hide(self, cx);
        match self.overlay {
            Overlay::None => self.focus_terminal(window, cx),
            Overlay::CommandBar => {
                if was != Overlay::CommandBar {
                    crate::sounds::play("command_bar");
                    crate::ui::command_bar::on_open(self, cx);
                }
                self.palette.focus.focus(window, cx);
            }
            Overlay::NeedsYou => {
                if was != Overlay::NeedsYou {
                    crate::ui::needs_you::on_open(self);
                }
                self.stack.focus.focus(window, cx);
            }
            Overlay::Annotate => {
                let fh = self.annot.read(cx).focus.clone();
                fh.focus(window, cx);
            }
        }
        cx.notify();
    }

    pub fn toast(&mut self, msg: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some((msg.into(), Instant::now()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(4200)).await;
            let _ = this.update(cx, |m, cx| {
                if m.toast.as_ref().is_some_and(|(_, t)| t.elapsed() >= Duration::from_secs(4)) {
                    m.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Fire-and-forget RPC with error toast; `then` runs on success on the UI thread.
    pub fn rpc(&mut self, method: &'static str, params: Value, cx: &mut Context<Self>, then: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + 'static) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
            let _ = this.update_in(cx, |m, window, cx| match res {
                Ok(v) => then(m, v, window, cx),
                Err(e) => m.toast(format!("{method} failed: {e:#}"), cx),
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------ actions

    pub fn resolve(&mut self, need_id: String, res: Resolution, cx: &mut Context<Self>) {
        self.menu = Menu::None;
        // Optimistic: hide the item now; the event-driven refresh confirms it.
        self.needs.retain(|n| n.id != need_id);
        match res {
            Resolution::Approve { .. } | Resolution::Done | Resolution::Restart => crate::sounds::play("approved"),
            Resolution::Deny => crate::sounds::play("denied"),
            Resolution::Dismiss => {}
        }
        let params = json!({"id": need_id, "resolution": res});
        self.rpc("needs_you.resolve", params, cx, |m, _, _, cx| m.request_refresh(refresh::NEEDS | refresh::SESSIONS | refresh::RULES, cx));
        cx.notify();
    }

    fn approve_once(&mut self, _: &ApproveOnce, _w: &mut Window, cx: &mut Context<Self>) {
        match self.approval_for_selected().map(|n| n.id.clone()) {
            Some(id) => self.resolve(id, Resolution::Approve { scope: ApprovalScope::Once }, cx),
            None => cx.propagate(),
        }
    }

    fn deny(&mut self, _: &Deny, _w: &mut Window, cx: &mut Context<Self>) {
        match self.approval_for_selected().map(|n| n.id.clone()) {
            Some(id) => self.resolve(id, Resolution::Deny, cx),
            None => cx.propagate(),
        }
    }

    /// ⌘J / the sidebar "N need you" button: open the needs-you card stack (NeedsYou-C).
    /// Inside the stack ⌘J skips to the next card (handled by the stack).
    pub fn next_needs_you(&mut self, _: &NextNeedsYou, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay == Overlay::NeedsYou {
            crate::ui::needs_you::skip(self, cx);
        } else {
            self.set_overlay(Overlay::NeedsYou, window, cx);
        }
    }

    pub fn open_session(&mut self, project: Option<String>, agent: Option<AgentKind>, cx: &mut Context<Self>) {
        let mut p = json!({"kind": if agent.is_some() { "agent" } else { "shell" }});
        if let Some(pid) = project {
            p["project_id"] = json!(pid);
        }
        if let Some(a) = agent {
            p["agent"] = json!(a);
        }
        self.rpc("session.open", p, cx, |m, v, window, cx| {
            let id = v.get("id").and_then(|x| x.as_str()).or_else(|| v.get("session").and_then(|s| s.get("id")).and_then(|x| x.as_str())).map(str::to_string);
            m.request_refresh(refresh::SESSIONS, cx);
            if let Some(id) = id {
                m.windows.borrow_mut().claim(&id, m.id);
                m.selected = Some(id);
                m.terminal = None;
                m.pending_terminal = None;
                m.ensure_terminal(window, cx);
            }
        });
    }

    /// The agent most recently started in a project (Claude when none).
    fn last_agent(&self, project: Option<&str>) -> AgentKind {
        self.sessions
            .iter()
            .filter(|s| s.project_id.as_deref() == project && s.agent.is_some())
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .and_then(|s| s.agent)
            .unwrap_or(AgentKind::Claude)
    }

    fn new_terminal(&mut self, _: &NewTerminal, _w: &mut Window, cx: &mut Context<Self>) {
        let p = self.current_project_id();
        self.open_session(p, None, cx);
    }
    fn new_agent(&mut self, _: &NewAgent, _w: &mut Window, cx: &mut Context<Self>) {
        let p = self.current_project_id();
        let a = self.last_agent(p.as_deref());
        self.open_session(p, Some(a), cx);
    }
    fn new_terminal_root(&mut self, _: &NewTerminalRoot, _w: &mut Window, cx: &mut Context<Self>) {
        self.open_session(Some(ROOT_PROJECT_ID.into()), None, cx);
    }
    fn new_agent_root(&mut self, _: &NewAgentRoot, _w: &mut Window, cx: &mut Context<Self>) {
        let a = self.last_agent(Some(ROOT_PROJECT_ID));
        self.open_session(Some(ROOT_PROJECT_ID.into()), Some(a), cx);
    }

    fn open_project(&mut self, _: &OpenProject, _w: &mut Window, cx: &mut Context<Self>) {
        self.pick_project(cx);
    }

    /// Folder picker, then [`Self::add_project`].
    pub fn pick_project(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open Project".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |m, window, cx| m.add_project(path.to_string_lossy().into_owned(), window, cx));
        })
        .detach();
    }

    /// Add `path` as a project (a no-op if it already is one), then go to it: its first
    /// terminal outside the Background group, or a new shell there when it has none.
    pub fn add_project(&mut self, path: String, _window: &mut Window, cx: &mut Context<Self>) {
        self.rpc("project.add", json!({ "path": path }), cx, |m, v, window, cx| {
            let Some(pid) = v.get("id").and_then(|x| x.as_str()).map(str::to_string) else {
                return;
            };
            m.request_refresh(refresh::PROJECTS, cx);
            match m.sessions.iter().find(|s| !s.background && s.project_id.as_deref() == Some(pid.as_str())).map(|s| s.id.clone()) {
                Some(id) => m.select(id, window, cx),
                None => m.open_session(Some(pid), None, cx),
            }
        });
    }

    fn select_project(&mut self, a: &SelectProject, window: &mut Window, cx: &mut Context<Self>) {
        // ⌘1–9 count projects only; root terminals have no number.
        let groups: Vec<Group> = self.groups().into_iter().filter(|g| g.project.is_some()).collect();
        let Some(g) = groups.get(a.0) else { return };
        let pick = g.sessions.iter().find(|s| s.status.state == StatusState::NeedsYou).or(g.sessions.first()).map(|s| s.id.clone());
        if let Some(id) = pick {
            self.select(id, window, cx);
        }
    }

    pub fn restart_selected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.rpc("session.restart", json!({"id": id}), cx, |m, _, window, cx| {
            m.terminal = None;
            m.pending_terminal = None;
            m.ensure_terminal(window, cx);
            m.request_refresh(refresh::SESSIONS, cx);
        });
    }

    /// ⌘W in the main window: close the selected terminal and select its neighbour in the
    /// sidebar. A busy one (working, needs you) asks for a second ⌘W first. With no terminal
    /// selected, close the window.
    fn close_tab(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        // A menu, overlay or screen (Rules, Triggers, Insights) in front closes first, like esc.
        if self.menu != Menu::None || self.overlay != Overlay::None || self.screen != Screen::Terminal {
            window.dispatch_action(Box::new(Dismiss), cx);
            return;
        }
        if self.marked.len() > 1 {
            self.close_marked(window, cx);
            return;
        }
        let Some(s) = self.selected_session() else {
            crate::ui::close_window::request(self, window, cx);
            return;
        };
        let (id, name) = (s.id.clone(), s.name.clone());
        let busy = matches!(self.effective_state(s), StatusState::Working | StatusState::NeedsYou);
        let confirmed = self.close_armed.as_ref().is_some_and(|(a, t)| *a == id && t.elapsed() < Duration::from_secs(2));
        if busy && !confirmed {
            self.close_armed = Some((id, Instant::now()));
            self.toast(format!("{name} is still running. Press ⌘W again to close it."), cx);
            return;
        }
        self.close_armed = None;
        crate::sounds::play("closed");
        self.select_neighbour(&id, window, cx);
        self.sessions.retain(|s| s.id != id);
        self.menu = Menu::None;
        self.rpc("session.close", json!({ "id": id, "force": true }), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS | refresh::NEEDS, cx));
        cx.notify();
    }

    /// ⌘W with several terminals selected: close them all and select the nearest one left
    /// below (else above). If any is busy, a second ⌘W within 2 s is needed first.
    fn close_marked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.marked_ids();
        let busy = self.sessions.iter().filter(|s| ids.contains(&s.id) && matches!(self.effective_state(s), StatusState::Working | StatusState::NeedsYou)).count();
        let key = ids.join(",");
        let confirmed = self.close_armed.as_ref().is_some_and(|(a, t)| *a == key && t.elapsed() < Duration::from_secs(2));
        if busy > 0 && !confirmed {
            self.close_armed = Some((key, Instant::now()));
            let still = if busy == 1 { "1 is still running".to_string() } else { format!("{busy} are still running") };
            self.toast(format!("Closing {} terminals: {still}. Press ⌘W again to close them.", ids.len()), cx);
            return;
        }
        self.close_armed = None;
        let order: Vec<String> = self.ordered_sessions().iter().map(|s| s.id.clone()).filter(|s| !crate::ui::popout::is_popped(s, cx)).collect();
        let last = order.iter().rposition(|x| ids.contains(x)).unwrap_or(0);
        let next = order[last..].iter().chain(order[..last].iter().rev()).find(|x| !ids.contains(x)).cloned();
        self.marked.clear();
        self.mark_anchor = None;
        match next {
            Some(n) => self.select(n, window, cx),
            None => {
                self.selected = None;
                self.terminal = None;
                self.pending_terminal = None;
            }
        }
        self.sessions.retain(|s| !ids.contains(&s.id));
        self.menu = Menu::None;
        crate::sounds::play("closed");
        for id in ids {
            self.rpc("session.close", json!({ "id": id, "force": true }), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS | refresh::NEEDS, cx));
        }
        cx.notify();
    }

    /// Select the terminal to show once `id` leaves the main pane ([`neighbour`]), or nothing.
    fn select_neighbour(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.sidebar_rows(Some(id), cx);
        match neighbour(&rows, id, |_| true) {
            Some(n) => self.select(n, window, cx),
            None => {
                self.selected = None;
                self.terminal = None;
                self.pending_terminal = None;
            }
        }
    }

    /// `id` is moving to its own window (`ui::popout`): take it out of the main and split panes.
    pub fn release(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.split.as_ref().is_some_and(|s| s.session_id(cx) == id) {
            crate::ui::split::close(self, window, cx);
        }
        if self.selected.as_deref() == Some(id) {
            self.select_neighbour(id, window, cx);
        }
        cx.notify();
    }

    /// `ids` are moving to another main window (a sidebar drag): take them out of the main
    /// and split panes, showing the nearest terminal that stays.
    pub fn hand_off(&mut self, ids: &[String], window: &mut Window, cx: &mut Context<Self>) {
        if self.split.as_ref().is_some_and(|s| ids.contains(&s.session_id(cx))) {
            crate::ui::split::close(self, window, cx);
        }
        self.marked.retain(|x| !ids.contains(x));
        if self.marked.len() < 2 {
            self.marked.clear();
        }
        if let Some(sel) = self.selected.clone().filter(|s| ids.contains(s)) {
            let order: Vec<String> = self.ordered_sessions().iter().map(|s| s.id.clone()).filter(|s| *s == sel || !crate::ui::popout::is_popped(s, cx)).collect();
            let at = order.iter().position(|x| *x == sel).unwrap_or(0);
            let next = order.iter().skip(at + 1).chain(order.iter().take(at).rev()).find(|x| !ids.contains(x)).cloned();
            match next {
                Some(n) => self.select(n, window, cx),
                None => {
                    self.selected = None;
                    self.terminal = None;
                    self.pending_terminal = None;
                }
            }
        }
        cx.notify();
    }

    pub fn close_selected(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        self.menu = Menu::None;
        crate::sounds::play("closed");
        self.rpc("session.close", json!({"id": id}), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS | refresh::NEEDS, cx));
    }

    pub fn copy_session_id(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected.clone() {
            cx.write_to_clipboard(ClipboardItem::new_string(id));
            crate::sounds::play("copied");
            self.toast("Copied the session id.", cx);
        }
        self.menu = Menu::None;
        cx.notify();
    }

    /// Mute the selected terminal's notifications, or drop the override (back to the global settings).
    pub fn toggle_mute(&mut self, cx: &mut Context<Self>) {
        if let Some(s) = self.selected_session() {
            let value = if s.notify_muted() { Value::Null } else { json!(false) };
            self.rpc("notify.set", json!({ "session": s.id, "key": "enabled", "value": value }), cx, |m, _, _, cx| m.request_refresh(refresh::SESSIONS, cx));
        }
        self.menu = Menu::None;
        cx.notify();
    }

    /// Select the terminal `step` rows away in sidebar order (wrapping), skipping popped-out ones.
    fn select_step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let order: Vec<String> = self.ordered_sessions().iter().map(|s| s.id.clone()).filter(|s| !crate::ui::popout::is_popped(s, cx)).collect();
        if order.is_empty() {
            return;
        }
        let n = order.len() as isize;
        let next = match self.selected.as_ref().and_then(|id| order.iter().position(|x| x == id)) {
            Some(i) => (i as isize + step).rem_euclid(n),
            None if step > 0 => 0,
            None => n - 1,
        };
        if self.selected.as_deref() != Some(order[next as usize].as_str()) {
            crate::sounds::play("switched");
        }
        self.select(order[next as usize].clone(), window, cx);
    }

    pub fn register_actions<E: InteractiveElement>(el: E, cx: &mut Context<Self>) -> E {
        el.on_action(cx.listener(Self::approve_once))
            .on_action(cx.listener(Self::deny))
            .on_action(cx.listener(Self::next_needs_you))
            .on_action(cx.listener(Self::new_terminal))
            .on_action(cx.listener(Self::new_agent))
            .on_action(cx.listener(Self::new_terminal_root))
            .on_action(cx.listener(Self::new_agent_root))
            .on_action(cx.listener(Self::open_project))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(|m, _: &HoldToQuit, window, cx| crate::ui::quit_hold::start(m, window, cx)))
            .on_action(cx.listener(Self::select_project))
            .on_action(cx.listener(|m, _: &ToggleCommandBar, w, cx| m.set_overlay(Overlay::CommandBar, w, cx)))
            .on_action(cx.listener(|m, _: &OpenPrompts, w, cx| crate::ui::command_bar::open_prompts(m, w, cx)))
            .on_action(cx.listener(|m, _: &OpenNeedsYou, w, cx| m.set_overlay(Overlay::NeedsYou, w, cx)))
            .on_action(cx.listener(|m, _: &crate::annotate::AddImage, w, cx| {
                crate::annotate::open(m, None, w, cx);
            }))
            .on_action(cx.listener(|m, _: &crate::ui::links::ToggleLinks, w, cx| crate::ui::links::toggle(m, w, cx)))
            .on_action(cx.listener(|m, _: &crate::ui::subagents::ToggleSubagents, w, cx| crate::ui::subagents::toggle(m, w, cx)))
            .on_action(cx.listener(|m, _: &crate::ui::queue::ToggleQueue, w, cx| crate::ui::queue::toggle(m, w, cx)))
            .on_action(cx.listener(|m, _: &crate::annotate::PasteImage, w, cx| {
                if crate::annotate::open(m, None, w, cx) {
                    let s = crate::annotate::clipboard_sources(cx);
                    m.annot.update(cx, |v, cx| v.add(s, w, cx));
                }
            }))
            .on_action(cx.listener(|m, a: &crate::annotate::DropImages, w, cx| crate::annotate::drop_images(m, a.session.clone(), a.paths.clone(), w, cx)))
            .on_action(cx.listener(|m, _: &crate::annotate::EditAttachment, w, cx| {
                if let Some(id) = crate::annotate::pane_session(m, w, cx).filter(|id| crate::annotate::is_attached(id, cx)) {
                    crate::annotate::open(m, Some(id), w, cx);
                }
            }))
            .on_action(cx.listener(|m, _: &OpenRules, w, cx| m.set_screen(Screen::Rules, w, cx)))
            .on_action(cx.listener(|m, _: &crate::ide::OpenInIde, _, cx| crate::ide::open_default(m, cx)))
            .on_action(cx.listener(|m, _: &crate::ide::ChooseIde, w, cx| crate::ide::toggle(m, w, cx)))
            .on_action(cx.listener(|m, _: &OpenTriggers, w, cx| m.set_screen(Screen::Triggers, w, cx)))
            .on_action(cx.listener(|m, _: &OpenInsights, w, cx| m.set_screen(Screen::Insights, w, cx)))
            .on_action(cx.listener(|m, _: &OpenSettings, _w, cx| crate::ui::settings::open(m.backend.clone(), cx)))
            .on_action(cx.listener(|m, _: &Dismiss, w, cx| {
                if m.menu != Menu::None {
                    if matches!(m.menu, Menu::Links | Menu::Subagents | Menu::Ide) {
                        m.focus_terminal(w, cx);
                    }
                    m.menu = Menu::None;
                    cx.notify();
                } else if m.queue.read(cx).is_open() {
                    m.queue.update(cx, |v, cx| v.close(cx));
                } else if m.overlay != Overlay::None {
                    m.set_overlay(Overlay::None, w, cx);
                } else if m.screen != Screen::Terminal {
                    m.set_screen(Screen::Terminal, w, cx);
                } else if !m.marked.is_empty() {
                    m.marked.clear();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|m, _: &SplitRight, window, cx| crate::ui::split::toggle(m, window, cx)))
            .on_action(cx.listener(|m, _: &PopOut, w, cx| {
                if let Some(id) = m.selected.clone() {
                    crate::ui::popout::open(m, id, w, cx);
                }
            }))
            .on_action(cx.listener(|m, _: &RestartSession, _w, cx| m.restart_selected(cx)))
            .on_action(cx.listener(|m, _: &ApproveOptions, _w, cx| {
                if m.overlay == Overlay::NeedsYou {
                    m.stack.menu = !m.stack.menu;
                } else if m.approval_for_selected().is_some() {
                    m.menu = if m.menu == Menu::Approve { Menu::None } else { Menu::Approve };
                }
                cx.notify();
            }))
            .on_action(cx.listener(|m, _: &NextTerminal, w, cx| m.select_step(1, w, cx)))
            .on_action(cx.listener(|m, _: &PrevTerminal, w, cx| m.select_step(-1, w, cx)))
            .on_action(cx.listener(|m, _: &RenameSession, w, cx| {
                if let Some(id) = m.selected.clone() {
                    crate::ui::rename::start(m, &id, crate::ui::rename::At::Header, w, cx);
                }
            }))
            .on_action(cx.listener(|m, _: &ToggleTerminalMenu, _w, cx| {
                if m.selected.is_some() {
                    m.menu = if m.menu == Menu::More { Menu::None } else { Menu::More };
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|m, _: &ToggleProjectMenu, _w, cx| {
                if let Some(p) = m.selected_session().and_then(|s| s.project_id.clone()).filter(|p| p != ROOT_PROJECT_ID) {
                    let k = Menu::Project(p);
                    m.menu = if m.menu == k { Menu::None } else { k };
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|m, _: &FoldProject, _w, cx| {
                if let Some(p) = m.selected_session().and_then(|s| s.project_id.clone()).filter(|p| p != ROOT_PROJECT_ID) {
                    crate::ui::sidebar::toggle_fold(m, p, cx);
                }
            }))
            .on_action(cx.listener(|m, _: &ToggleSidebar, _w, cx| crate::ui::sidebar::toggle_collapsed(m, cx)))
            .on_action(cx.listener(|m, _: &CopySessionId, _w, cx| m.copy_session_id(cx)))
            .on_action(cx.listener(|m, _: &ToggleMute, _w, cx| m.toggle_mute(cx)))
            .on_action(cx.listener(|m, _: &ToggleSplitOrientation, _w, cx| crate::ui::split::flip(m, cx)))
            .on_action(cx.listener(|m, _: &SplitToMain, w, cx| crate::ui::split::to_main(m, w, cx)))
            .on_action(cx.listener(|m, _: &FocusOtherPane, w, cx| crate::ui::split::focus_other(m, w, cx)))
    }
}

#[derive(Default)]
struct RefreshResult {
    settings: Option<Vec<SettingEntry>>,
    projects: Option<Vec<Project>>,
    discovered: Option<Vec<ProjectCandidate>>,
    sessions: Option<Vec<Session>>,
    needs: Option<Vec<NeedsYou>>,
    today: Option<Today>,
    rules: Option<usize>,
    webhooks: Option<Value>,
    hooks: Option<Value>,
    triggers: Option<usize>,
    header: Option<(String, Vec<Segment>)>,
    status: Option<(String, HashMap<String, Vec<Segment>>)>,
    buttons: Option<(String, HashMap<String, Vec<Segment>>)>,
    rows: Option<HashMap<String, Vec<Segment>>>,
}

pub struct Group<'a> {
    pub project: Option<&'a Project>,
    pub name: String,
    pub sessions: Vec<&'a Session>,
}

/// Tell midnad the terminal colors of the theme on screen (`themes.report`), so every engine
/// uses them as its defaults. Only when they change, off the UI thread.
fn report_terminal_colors(backend: &Arc<dyn Backend>, def: &midna_proto::themes::ThemeDef) {
    use midna_proto::themes::to_hex;
    static LAST: std::sync::Mutex<Option<Value>> = std::sync::Mutex::new(None);
    let colors = json!({
        "theme": def.id,
        "foreground": to_hex(def.get("fg").unwrap_or([255; 3])),
        "background": to_hex(def.get("term").unwrap_or([0; 3])),
        "ansi": def.ansi.iter().map(|c| to_hex(*c)).collect::<Vec<_>>(),
    });
    {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&colors) {
            return;
        }
        *last = Some(colors.clone());
    }
    let backend = backend.clone();
    std::thread::spawn(move || {
        if let Err(e) = backend.call("themes.report", json!({ "colors": colors })) {
            eprintln!("midna-app: themes.report: {e:?}");
        }
    });
}

/// Which sidebar group a row sits in: its project (`None` for root terminals), or Background.
#[derive(Clone, PartialEq, Debug)]
enum RowGroup {
    Project(Option<String>),
    Background,
}

/// The terminal to show when `id` leaves: the nearest one in its own group, above first, so
/// closing a terminal keeps you in its project; with none left there, the next row down,
/// else up. Only rows `alive` says still exist count.
fn neighbour(rows: &[(String, RowGroup)], id: &str, alive: impl Fn(&str) -> bool) -> Option<String> {
    let at = rows.iter().position(|(x, _)| x == id)?;
    let group = &rows[at].1;
    let ok = |(x, _): &&(String, RowGroup)| x != id && alive(x);
    let (above, below) = (&rows[..at], &rows[at + 1..]);
    above.iter().rev().filter(|r| &r.1 == group).find(ok)
        .or_else(|| below.iter().filter(|r| &r.1 == group).find(ok))
        .or_else(|| below.iter().find(ok))
        .or_else(|| above.iter().rev().find(ok))
        .map(|(x, _)| x.clone())
}

#[cfg(test)]
mod tests {
    use super::{neighbour, RowGroup};

    fn rows(spec: &[(&str, Option<&str>)]) -> Vec<(String, RowGroup)> {
        spec.iter().map(|(id, p)| (id.to_string(), match *p { Some("bg") => RowGroup::Background, p => RowGroup::Project(p.map(str::to_string)) })).collect()
    }

    #[test]
    fn closing_stays_in_the_project_going_up_first() {
        let r = rows(&[("a1", Some("a")), ("a2", Some("a")), ("b1", Some("b")), ("b2", Some("b")), ("b3", Some("b")), ("c1", Some("c"))]);
        let all = |_: &str| true;
        assert_eq!(neighbour(&r, "b2", all).as_deref(), Some("b1"));
        // the top of its project: down, still in the project
        assert_eq!(neighbour(&r, "b1", all).as_deref(), Some("b2"));
        // the bottom of its project: up, not into the next project
        assert_eq!(neighbour(&r, "b3", all).as_deref(), Some("b2"));
        assert_eq!(neighbour(&r, "a2", all).as_deref(), Some("a1"));
        // the project's last terminal: the next row, else the one above
        assert_eq!(neighbour(&r, "c1", all).as_deref(), Some("b3"));
        assert_eq!(neighbour(&rows(&[("a1", Some("a")), ("b1", Some("b"))]), "a1", all).as_deref(), Some("b1"));
        assert_eq!(neighbour(&rows(&[("a1", Some("a"))]), "a1", all), None);
    }

    #[test]
    fn neighbour_skips_gone_rows_and_other_groups() {
        let r = rows(&[("r1", None), ("a1", Some("a")), ("a2", Some("a")), ("a3", Some("a")), ("g1", Some("bg"))]);
        // a2 went too (closed together): a1
        assert_eq!(neighbour(&r, "a3", |id| id != "a2").as_deref(), Some("a1"));
        // a background terminal of the same project isn't "in" the project
        let r = rows(&[("a1", Some("a")), ("b1", Some("b")), ("g1", Some("bg"))]);
        assert_eq!(neighbour(&r, "g1", |_| true).as_deref(), Some("b1"));
        assert_eq!(neighbour(&r, "missing", |_| true), None);
    }
}
