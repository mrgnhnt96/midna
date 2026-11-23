//! The composer: a native `NSTextView` over the bottom of the terminal that appears only while dictating
//! (Kass) or when opened by hand (`keys.composer`, default ⇧⌘D). Proven in
//! `spikes/kass-composer`: gpui-kit's own input fails Kass's Accessibility checks, a real
//! `NSTextView` passes them (role AXTextArea, settable AXSelectedText, readable value and
//! selection, AXStringForRange).
//!
//! Layout follows Main.dc.html (`mode=dictating`), without its "Kass · listening" pill: an
//! accent-bordered box with `❯`, the field and "↩ send · esc cancel". GPUI draws the box; the
//! `NSTextView` (inside an `NSScrollView`) is a sibling *above* the GPUI view, moved onto the
//! box's field area every paint (`place`). Every main window gets its own text view: one shared
//! view lives in the window that opened it first, and a composer in any other window had no
//! field to type into.
//!
//! Keys: ↩ sends the text to the target terminal with `session.input{enter:true}` (multi-line
//! text goes as one bracketed paste when the terminal asked for it) and closes; ⇧↩ / ⌥↩ insert
//! a newline; Esc cancels (clears and hides). Esc in the terminal also closes the composer when
//! it's empty ([`escape_in_terminal`]). The target is the terminal selected when the
//! composer opened. Images attached to it (`annotate.rs`) go with the text.
//!
//! The composer belongs to its target: select another terminal and it's put away with its
//! text (a draft), come back and it's open again where you left it. It never shows over (or
//! sends to) a terminal it wasn't opened on.
//!
//! Focus has two layers. AppKit's first responder gets the keystrokes; GPUI has its own focus
//! and sees every key *equivalent* first (performKeyEquivalent goes to the GPUI view before
//! the text view). So while the text view is first responder, GPUI focus sits on
//! [`Composer::focus`] (key context `MidnaComposer`, which binds ⌘V/⌘C/⌘X/⌘A/⌘Z to the text
//! view) instead of the terminal, whose key handler would otherwise eat ↩ and Esc. The text
//! view reports becoming/resigning first responder; GPUI blurring the composer (⌘K, a click
//! elsewhere, another screen) hands first responder back to the GPUI view.
use crate::app::{MainWindow, Overlay, Screen};
use crate::backend::ConnState;
use crate::kass::{self, KassEvent};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSAutoresizingMaskOptions, NSBorderType, NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSFont, NSResponder, NSScrollView, NSText, NSTextView, NSView};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::json;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

actions!(
    midna,
    [
        /// Open the composer by hand (`keys.composer`), or focus it when it's open.
        OpenComposer,
        ComposerPaste,
        ComposerCopy,
        ComposerCut,
        ComposerSelectAll,
        ComposerUndo,
        ComposerRedo,
    ]
);

pub const CTX_COMPOSER: &str = "MidnaComposer";
/// The field grows with its text up to this many lines, then scrolls.
const MAX_LINES: usize = 8;
const FONT_SIZE: f64 = 12.5;

/// From the text view (AppKit callbacks, main thread) to the main window.
#[derive(Debug)]
pub enum ComposerEvent {
    Submit(String),
    Cancel,
    Changed,
    Focused(bool),
    /// The secret sheet answered a ⌘V here (`terminal/secret_paste.rs`): type this at the cursor.
    Insert(String),
    /// The secret sheet was cancelled: focus the field again.
    Refocus,
}

static EVENTS: OnceLock<async_channel::Sender<ComposerEvent>> = OnceLock::new();

pub fn emit(e: ComposerEvent) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.try_send(e);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Manual,
    Kass,
}

/// An open composer put away when you selected another terminal.
struct Draft {
    text: String,
    source: Source,
}

pub struct Composer {
    pub open: bool,
    pub source: Source,
    /// The terminal the text goes to (selected when the composer opened).
    pub target: Option<String>,
    pub lines: usize,
    pub focus: FocusHandle,
    /// Composers put away by terminal, until you select that terminal again.
    drafts: HashMap<String, Draft>,
    /// This window's text view, added on first open. Each main window has its own: AppKit
    /// can only give keys to a view inside the key window.
    native: Option<Rc<Native>>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<MainWindow>) -> Self {
        let focus = cx.focus_handle();
        let mut tasks = vec![];
        // The native text view and Kass have one sink each for the whole app: their events
        // go to the main window you're using (there can be several, see `windows`).
        let (tx, rx) = async_channel::unbounded::<ComposerEvent>();
        if EVENTS.set(tx).is_ok() {
            let app: &App = cx;
            app.spawn(async move |cx| {
                while let Ok(ev) = rx.recv().await {
                    cx.update(|cx| crate::windows::with_active(cx, |m, window, cx| on_event(m, ev, window, cx)));
                }
            })
            .detach();
            // Each event carries when it arrived, so the log shows how long it waited for main.
            let (ktx, krx) = async_channel::unbounded::<(KassEvent, Instant)>();
            kass::start(move |ev| {
                let _ = ktx.try_send((ev, Instant::now()));
            });
            app.spawn(async move |cx| {
                while let Ok((ev, at)) = krx.recv().await {
                    cx.update(|cx| crate::windows::with_active(cx, |m, window, cx| on_kass(m, ev, at, window, cx)));
                }
            })
            .detach();
        }
        if let Ok(text) = crate::dev::var("MIDNA_DEBUG_COMPOSER") {
            // Dev: open the composer with `text` once terminals have loaded (screenshots).
            tasks.push(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let _ = this.update_in(cx, |m, window, cx| {
                    open(m, Source::Manual, window, cx);
                    native(m, |n| n.tv.setString(&NSString::from_str(&text.replace("\\n", "\n"))));
                    on_event(m, ComposerEvent::Changed, window, cx);
                });
            }));
        }
        let subs = vec![cx.on_blur(&focus, window, |m, _, _| release_native_focus(m))];
        Composer { open: false, source: Source::Manual, target: None, lines: 1, focus, drafts: HashMap::new(), native: None, _tasks: tasks, _subs: subs }
    }
}

// ------------------------------------------------------------------ main-window logic

/// Can the composer be shown right now (a connected terminal pane, nothing over it)?
fn can_show(m: &MainWindow) -> bool {
    m.conn == ConnState::Connected && m.screen == Screen::Terminal && m.overlay == Overlay::None && m.selected.is_some()
}

/// An open composer shows only over its own terminal.
fn on_target(m: &MainWindow) -> bool {
    m.composer.target.is_none() || m.composer.target == m.selected
}

/// What the selected terminal means for the composer: put the open one away (it belongs to
/// another terminal), and/or bring back the selected terminal's draft.
fn follow(open_on: Option<&str>, selected: Option<&str>, has_draft: impl Fn(&str) -> bool) -> (bool, bool) {
    let park = open_on.is_some() && open_on != selected;
    let restore = (open_on.is_none() || park) && selected.is_some_and(has_draft);
    (park, restore)
}

/// Keep the composer with its terminal (called every render): put it away when another
/// terminal is selected, and reopen a terminal's draft when it's selected again.
fn follow_selection(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.conn == ConnState::Connected {
        let sessions = &m.sessions;
        m.composer.drafts.retain(|id, _| sessions.iter().any(|s| &s.id == id));
    }
    let open_on = m.composer.target.as_deref().filter(|_| m.composer.open);
    let drafts = &m.composer.drafts;
    let (park, restore) = follow(open_on, m.selected.as_deref(), |id| drafts.contains_key(id));
    if park {
        let had_focus = m.composer.focus.contains_focused(window, cx);
        if let Some(id) = m.composer.target.take() {
            let text = text(m);
            if !text.trim().is_empty() {
                m.composer.drafts.insert(id, Draft { text, source: m.composer.source });
            }
        }
        m.composer.open = false;
        release_native_focus(m);
        native(m, |n| {
            n.tv.setString(&NSString::from_str(""));
            n.scroll.setHidden(true);
        });
        if had_focus {
            cx.defer_in(window, |m, window, cx| m.focus_terminal(window, cx));
        }
    }
    if restore && let Some(id) = m.selected.clone() && let Some(d) = m.composer.drafts.remove(&id) {
        m.composer.open = true;
        m.composer.source = d.source;
        m.composer.target = Some(id);
        native(m, |n| n.tv.setString(&NSString::from_str(&d.text)));
        cx.defer_in(window, |m, window, cx| {
            if m.composer.open && on_target(m) && can_show(m) {
                let t = crate::theme::of(window, cx).clone();
                show_native(m, window, &t);
                m.composer.focus.focus(window, cx);
                on_event(m, ComposerEvent::Changed, window, cx);
            }
        });
    }
}

fn open(m: &mut MainWindow, source: Source, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if !m.composer.open {
        m.composer.open = true;
        m.composer.source = source;
        m.composer.target = m.selected.clone();
        m.composer.lines = 1;
        native(m, |n| n.tv.setString(&NSString::from_str("")));
    } else if source == Source::Kass {
        m.composer.source = Source::Kass;
    }
    let t = crate::theme::of(window, cx).clone();
    let ok = show_native(m, window, &t);
    m.composer.focus.focus(window, cx);
    cx.notify();
    ok
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.composer.open = false;
    m.composer.target = None;
    // Hand focus back before hiding: hiding the first responder makes AppKit give it to the
    // bare window, and plain keys (and ⌘'s release) would then go nowhere.
    release_native_focus(m);
    native(m, |n| {
        n.tv.setString(&NSString::from_str(""));
        n.scroll.setHidden(true);
    });
    m.focus_terminal(window, cx);
    cx.notify();
}

/// Esc in the terminal while an empty composer is open over it closes the composer instead of
/// going to the terminal. With text in it, Esc goes to the terminal as usual (only Esc in the
/// field throws text away). True = handled.
pub fn escape_in_terminal(m: &mut MainWindow, ks: &Keystroke, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if ks.key != "escape" || ks.modifiers.modified() || !m.composer.open || !on_target(m) || !can_show(m) {
        return false;
    }
    let in_terminal = m.terminal.as_ref().is_some_and(|t| t.read(cx).focus_handle().contains_focused(window, cx));
    if !in_terminal || !text(m).trim().is_empty() {
        return false;
    }
    close(m, window, cx);
    true
}

/// Give focus back to the composer if it's open over the selected terminal (the image sheet
/// closed over it): its ↩ sends the text and the images together. False = nothing to focus.
pub fn refocus(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if !m.composer.open || !on_target(m) || !can_show(m) {
        return false;
    }
    let t = crate::theme::of(window, cx).clone();
    show_native(m, window, &t);
    m.composer.focus.focus(window, cx);
    cx.notify();
    true
}

/// `keys.composer`: open by hand, or bring focus back to an open composer.
fn open_manual(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if !can_show(m) {
        m.toast("The composer opens over a terminal.", cx);
        return;
    }
    open(m, Source::Manual, window, cx);
}

/// How long Kass waits for `dictationReady` after `willBegin`; then it reads the focused element
/// anyway, and pastes there or types.
const KASS_WAIT: Duration = Duration::from_millis(150);

/// The key window when Kass's `dictationWillBegin` comes in, seen from the main window it was
/// routed to (`windows::with_active`, which goes by AppKit's *main* window).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyWindow {
    /// This main window.
    This,
    /// A midna window you don't type into (the badge, a subagent's log, a system window), or
    /// none while midna is in front: the dictation is for the main window you're on.
    Passive,
    /// A window with text of its own: another main window, Settings, a popped-out terminal, an
    /// AppKit panel or sheet. Kass inserts there as it would anywhere.
    Typing,
    /// midna isn't the app in front.
    Away,
}

impl KeyWindow {
    fn label(self) -> &'static str {
        match self {
            KeyWindow::This => "this window",
            KeyWindow::Passive => "badge or another window without text",
            KeyWindow::Typing => "another midna window or panel with text",
            KeyWindow::Away => "another app",
        }
    }
}

/// Whether a `willBegin` seen with `key` is for this window: `Some(make_key)`, where `make_key`
/// says to make this window key first (the badge or a log window was key). `any_window`
/// (`MIDNA_KASS_ANY_WINDOW`) takes it whatever is key.
fn route(key: KeyWindow, any_window: bool) -> Option<bool> {
    match key {
        KeyWindow::This => Some(false),
        KeyWindow::Passive => Some(true),
        _ if any_window => Some(false),
        KeyWindow::Typing | KeyWindow::Away => None,
    }
}

/// Why a Kass `willBegin` doesn't open the composer: the first gate that fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KassSkip {
    NotKey,
    NotConnected,
    NotTerminalScreen(Screen),
    Overlay(Overlay),
    NoTerminal,
    SecretSheet,
    OtherTerminal,
}

impl KassSkip {
    /// A short app.log reason; `key` says what is key instead (`KeyWindow::label`).
    fn reason(self, key: &str) -> String {
        match self {
            KassSkip::NotKey => format!("window not key (key: {key})"),
            KassSkip::NotConnected => "not connected".into(),
            KassSkip::NotTerminalScreen(s) => format!("screen {s:?}"),
            KassSkip::Overlay(o) => format!("overlay {o:?}"),
            KassSkip::NoTerminal => "no terminal selected".into(),
            KassSkip::SecretSheet => "secret sheet open".into(),
            KassSkip::OtherTerminal => "composer open on another terminal".into(),
        }
    }
}

/// The willBegin gate: the dictation is for this window (`route`) and the composer can show
/// over its own terminal (`can_show`, no secret sheet over it, `on_target`), else why not.
fn kass_skip(active: bool, connected: bool, screen: Screen, overlay: Overlay, selected: bool, sheet: bool, on_target: bool) -> Option<KassSkip> {
    if !active {
        Some(KassSkip::NotKey)
    } else if !connected {
        Some(KassSkip::NotConnected)
    } else if screen != Screen::Terminal {
        Some(KassSkip::NotTerminalScreen(screen))
    } else if overlay != Overlay::None {
        Some(KassSkip::Overlay(overlay))
    } else if !selected {
        Some(KassSkip::NoTerminal)
    } else if sheet {
        Some(KassSkip::SecretSheet)
    } else if !on_target {
        Some(KassSkip::OtherTerminal)
    } else {
        None
    }
}

fn key_window(window: &Window, cx: &mut App) -> KeyWindow {
    if window.is_window_active() {
        return KeyWindow::This;
    }
    let me = window.window_handle();
    let key = cx.windows().into_iter().filter(|h| *h != me).find(|h| h.update(cx, |_, w, _| w.is_window_active()).unwrap_or(false));
    match key {
        Some(h) if h.downcast::<MainWindow>().is_some() || h.downcast::<crate::settings_window::SettingsWindow>().is_some() || h.downcast::<crate::ui::popout::PopOut>().is_some() => KeyWindow::Typing,
        Some(_) => KeyWindow::Passive,
        None => {
            let Some(mtm) = MainThreadMarker::new() else { return KeyWindow::Away };
            let app = NSApplication::sharedApplication(mtm);
            match (app.isActive(), app.keyWindow()) {
                (false, _) => KeyWindow::Away,
                (true, None) => KeyWindow::Passive,
                (true, Some(_)) => KeyWindow::Typing,
            }
        }
    }
}

fn on_kass(m: &mut MainWindow, ev: KassEvent, at: Instant, window: &mut Window, cx: &mut Context<MainWindow>) {
    // Every handshake goes in app.log (never the dictated text): a take that misses the
    // composer is otherwise silent.
    let log = crate::lifecycle::log;
    match ev {
        KassEvent::WillBegin { mode } => {
            // Kass waits only `KASS_WAIT` for the reply: decide, show the field and answer
            // first; everything else after.
            let queued = at.elapsed();
            let key = key_window(window, cx);
            let route = route(key, crate::dev::var("MIDNA_KASS_ANY_WINDOW").is_ok());
            let sheet = sheet_over(m, cx);
            let skip = kass_skip(route.is_some(), m.conn == ConnState::Connected, m.screen, m.overlay, m.selected.is_some(), sheet, on_target(m));
            let ready = match (skip, route) {
                (None, Some(make_key)) => {
                    if make_key {
                        // The badge or a log window is key: the dictation is for this window.
                        // AppKit directly, so it's key before the reply (GPUI's activate is async).
                        install(m, window);
                        native(m, |n| n.tv.window().map(|w| w.makeKeyAndOrderFront(None)));
                    }
                    let ok = open(m, Source::Kass, window, cx);
                    if ok {
                        kass::post_ready();
                    }
                    ok
                }
                _ => {
                    // No reply: Kass reads the focused element and inserts there. Never the
                    // hidden field.
                    if !visible(m, cx) {
                        release_native_focus(m);
                    }
                    false
                }
            };
            let took = at.elapsed();
            crate::ui::ax_prompt::on_kass_begin(m, cx);
            // Settings shows "handshake detected"; it reads kass::handshake_detected() on render.
            cx.refresh_windows();
            let ms = format!("{}ms after the notice, {}ms of it queued", took.as_millis(), queued.as_millis());
            match (skip, route) {
                (Some(skip), _) => log(&format!("kass willBegin ({mode}) ignored: {} ({ms})", skip.reason(key.label()))),
                (None, Some(make_key)) if ready => {
                    let late = if took > KASS_WAIT { ", past Kass's wait" } else { "" };
                    let made = if make_key { " after making the window key" } else { "" };
                    log(&format!("kass willBegin ({mode}): composer first responder{made}, ready posted {ms}{late}"));
                }
                _ => log(&format!("kass willBegin ({mode}): composer not first responder, no ready ({ms})")),
            }
        }
        KassEvent::DidEnd { outcome } => {
            cx.refresh_windows();
            let outcome_s = outcome.as_deref().unwrap_or("-");
            if !m.composer.open {
                log(&format!("kass didEnd ({outcome_s}): composer closed"));
                return;
            }
            let text = text(m);
            log(&format!("kass didEnd ({outcome_s}): {} chars, composer open", text.chars().count()));
            if text.trim().is_empty() {
                close(m, window, cx);
                return;
            }
            let auto = m.setting_str("kass.auto_send").as_deref() == Some("true");
            let settled = matches!(outcome.as_deref(), None | Some("inserted"));
            if auto && settled {
                submit(m, text, window, cx);
            } else {
                on_event(m, ComposerEvent::Changed, window, cx);
            }
        }
    }
}

fn on_event(m: &mut MainWindow, ev: ComposerEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    match ev {
        ComposerEvent::Submit(text) => submit(m, text, window, cx),
        ComposerEvent::Cancel => close(m, window, cx),
        ComposerEvent::Changed => {
            let lines = native(m, |n| n.lines()).unwrap_or(1);
            if lines != m.composer.lines {
                m.composer.lines = lines;
            }
            cx.notify();
        }
        ComposerEvent::Focused(true) => {
            if m.composer.open && !m.composer.focus.is_focused(window) {
                m.composer.focus.focus(window, cx);
            }
        }
        ComposerEvent::Focused(false) => {}
        ComposerEvent::Insert(text) => {
            if refocus(m, window, cx) {
                native(m, |n| unsafe {
                    let s = NSString::from_str(&text);
                    let _: () = msg_send![&*n.tv, insertText: &*s, replacementRange: n.tv.selectedRange()];
                });
                on_event(m, ComposerEvent::Changed, window, cx);
            }
        }
        ComposerEvent::Refocus => {
            refocus(m, window, cx);
        }
    }
}

fn submit(m: &mut MainWindow, text: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    if !m.composer.open || !on_target(m) {
        return;
    }
    let Some(id) = m.composer.target.clone().or_else(|| m.selected.clone()) else {
        close(m, window, cx);
        return;
    };
    if text.trim().is_empty() {
        close(m, window, cx);
        return;
    }
    let bpaste = m.terminal.as_ref().filter(|t| t.read(cx).session_id == id).map(|t| t.read(cx).bracketed_paste()).unwrap_or(true);
    close(m, window, cx);
    // A secret in the text: the terminal asks whether to store it first (`terminal/secret_paste.rs`).
    if let Some(t) = m.terminal.clone().filter(|t| t.read(cx).session_id == id)
        && t.update(cx, |t, cx| t.submit_checks_secrets(&text, window, cx))
    {
        return;
    }
    // Images attached to this terminal (`annotate.rs`) wait for its next ↩, which this send
    // replaces: they go with the text, or one of the two would be left behind.
    let out = crate::annotate::take(&id, cx);
    let data = encode(&with_notes(&text, out.as_ref().map_or("", |o| o.text.as_str())), bpaste);
    let paths: Vec<std::path::PathBuf> = out.map(|o| o.paths).unwrap_or_default();
    if let Some(mac) = m.backend.mac().map(|m| m.name.clone()).filter(|_| !paths.is_empty()) {
        // Another Mac's terminal: these paths mean nothing there. Each image goes there first
        // and that Mac pastes its own path, then the text follows.
        let backend = m.backend.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move {
                    crate::terminal::send_image::send_there(&*backend, &id, &paths, &mac)?;
                    backend.call("session.input", json!({"id": id, "text": data, "enter": true})).map(drop).map_err(|e| format!("session.input failed: {e:#}"))
                })
                .await;
            if let Err(e) = res {
                let _ = this.update(cx, |m, cx| m.toast(e, cx));
            }
        })
        .detach();
        return;
    }
    let images: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    m.rpc("session.input", json!({"id": id, "text": data, "enter": true, "images": images}), cx, |_, _, _, _| {});
}

/// The text, then an attachment's notes on their own line.
fn with_notes(text: &str, notes: &str) -> String {
    if notes.is_empty() {
        return text.to_string();
    }
    format!("{}\n{notes}", text.trim_end_matches(['\n', '\r']))
}

/// What goes to the terminal for `text` (Enter is sent separately by `session.input`).
/// Trailing newlines are dropped. Multi-line text is one bracketed paste when the terminal
/// enabled it (mode 2004), so a shell or agent receives it as a single input; newlines are
/// sent as CR, like a terminal paste.
pub fn encode(text: &str, bracketed_paste: bool) -> String {
    let t = text.replace("\r\n", "\n");
    let t = t.trim_end_matches(['\n', '\r']);
    if !t.contains('\n') {
        return t.to_string();
    }
    let t = t.replace('\n', "\r");
    if bracketed_paste { format!("\x1b[200~{}\x1b[201~", t.replace("\x1b[201~", "")) } else { t }
}

/// Current text of the field.
pub fn text(m: &MainWindow) -> String {
    native(m, |n| n.tv.string().to_string()).unwrap_or_default()
}

// ------------------------------------------------------------------ rendering

/// Keep the composer with its terminal and the native view's visibility in step with the
/// layout (called every render).
pub fn sync(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    // The (hidden) field is there before the first dictation, so answering Kass's first
    // `dictationWillBegin` doesn't wait for AppKit to build it.
    install(m, window);
    follow_selection(m, window, cx);
    let visible = visible(m, cx);
    native(m, |n| {
        // Every render, not just on hide: whatever stranded AppKit's first responder (an AX
        // client poking the hidden field, a hide racing a focus), keys must reach GPUI.
        if !visible {
            release_native_focus_inner(n);
        }
        if n.scroll.isHidden() == visible {
            n.scroll.setHidden(!visible);
        }
    });
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !m.composer.open || !on_target(m) || !can_show(m) || sheet_over(m, cx) {
        return None;
    }
    let line_h = native(m, |n| n.line_h).unwrap_or(16.) as f32;
    let field_h = line_h * m.composer.lines.clamp(1, MAX_LINES) as f32;
    let theme = t.clone();
    let nat = m.composer.native.clone();
    let field = canvas(|b, _, _| b, move |_, b, _, _| place(nat.as_deref(), b, &theme)).flex_1().min_w_0().h(px(field_h));
    // Offscreen snapshots (`--features snapshot`) can't see native views, so draw the field's
    // text with GPUI there. Dev only; the live app always shows the NSTextView.
    let field = if cfg!(feature = "snapshot") && std::env::var("MIDNA_SNAPSHOT").is_ok() {
        div()
            .flex_1()
            .min_w_0()
            .relative()
            .child(field)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .font_family(t.mono_font.clone())
                    .text_size(px(FONT_SIZE as f32))
                    .line_height(px(line_h))
                    .text_color(t.fg)
                    .child(format!("{}▏", text(m))),
            )
            .into_any_element()
    } else {
        field.into_any_element()
    };
    let key = |k: &'static str| crate::ui::header::key_chip(t, k.into()).bg(t.raised);
    let hint = div()
        .flex_none()
        .flex()
        .items_center()
        .gap(px(5.))
        .h(px(line_h))
        .text_size(px(11.))
        .text_color(t.dim)
        .whitespace_nowrap()
        .child(key("↩"))
        .child("send")
        .child(div().w(px(4.)))
        .child(key("esc"))
        .child("cancel");
    let boxed = crate::ui::border_w(div(), 1.5)
        .id("composer-box")
        .w_full()
        .flex()
        .items_start()
        .gap(px(10.))
        .px(px(12.))
        .py(px(10.))
        .border_color(t.accent)
        .rounded(px(9.))
        .bg(t.panel)
        .cursor_text()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|m, _, window, cx| {
                m.composer.focus.focus(window, cx);
                native(m, |n| {
                    if let Some(w) = n.tv.window() {
                        w.makeFirstResponder(Some(&n.tv));
                    }
                });
            }),
        )
        .child(div().font_family(t.mono_font.clone()).text_size(px(FONT_SIZE as f32)).line_height(px(line_h)).text_color(t.accent).child("❯"))
        .child(field)
        .child(hint);
    Some(
        div()
            .id("composer")
            .key_context(CTX_COMPOSER)
            .track_focus(&m.composer.focus)
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .bg(t.term)
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(8.))
            .pb(px(16.))
            .on_mouse_down_out(cx.listener(|m, _, _, _| release_native_focus(m)))
            .child(boxed)
            .into_any_element(),
    )
}

pub fn register<E: InteractiveElement>(el: E, cx: &mut Context<MainWindow>) -> E {
    let fwd = |s: Sel| move |m: &mut MainWindow, _: &mut Window, _: &mut Context<MainWindow>| send_to_text_view(m, s);
    let paste = fwd(sel!(paste:));
    let copy = fwd(sel!(copy:));
    let cut = fwd(sel!(cut:));
    let all = fwd(sel!(selectAll:));
    el.on_action(cx.listener(|m, _: &OpenComposer, w, cx| open_manual(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerPaste, w, cx| {
            if !paste_image(m, w, cx) && !paste_secret(m, w, cx) {
                paste(m, w, cx)
            }
        }))
        .on_action(cx.listener(move |m, _: &ComposerCopy, w, cx| copy(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerCut, w, cx| cut(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerSelectAll, w, cx| all(m, w, cx)))
        .on_action(cx.listener(|m, _: &ComposerUndo, _, _| undo(m, false)))
        .on_action(cx.listener(|m, _: &ComposerRedo, _, _| undo(m, true)))
}

/// ⌘V of an image (a screenshot, or a copied image file), as ⌘V in the terminal does: over an agent, open the image
/// sheet with it (its images go with the composer's text when it's sent, `submit`); with
/// `terminal.image_paste` inline, or any other target, type its file path. False = paste as text.
fn paste_image(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if !crate::annotate::clipboard_is_image(cx) {
        return false;
    }
    let sources = crate::annotate::clipboard_sources(cx);
    match m.composer.target.clone().filter(|id| crate::annotate::is_agent(m, id)) {
        Some(id) if !crate::terminal::image_paste_inline() => {
            release_native_focus(m);
            if crate::annotate::open(m, Some(id), window, cx) {
                m.annot.update(cx, |v, cx| v.add(sources, window, cx));
            }
        }
        // Another Mac's terminal: a path here means nothing there, so the image goes there and
        // that Mac pastes its own path into the terminal (`terminal::send_image`).
        _ if m.remote() => {
            let target = m.composer.target.clone();
            if let Some(t) = m.terminal.clone().filter(|t| target.is_none() || target.as_deref() == Some(t.read(cx).session_id.as_str())) {
                t.update(cx, |t, cx| t.send_images(sources, cx));
            }
        }
        _ => paste_image_paths(sources, cx),
    }
    true
}

/// ⌘V of text with a secret in it over an agent: the terminal's secret sheet asks now, as a
/// paste into the terminal would, instead of when ↩ sends it. Its answer (references, or the
/// text as it was) comes back into the field (`ComposerEvent::Insert`). False = paste as usual.
fn paste_secret(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return false };
    let Some(t) = target_terminal(m, cx) else { return false };
    if !t.update(cx, |t, cx| t.composer_paste_checks_secrets(&text, window, cx)) {
        return false;
    }
    release_native_focus(m);
    cx.notify();
    true
}

/// The composer's target terminal, when it's the one on screen.
fn target_terminal(m: &MainWindow, cx: &App) -> Option<Entity<crate::terminal::TerminalView>> {
    let id = m.composer.target.clone().or_else(|| m.selected.clone())?;
    m.terminal.clone().filter(|t| t.read(cx).session_id == id)
}

/// The composer shows over the selected terminal now.
fn visible(m: &MainWindow, cx: &App) -> bool {
    m.composer.open && on_target(m) && can_show(m) && !sheet_over(m, cx)
}

/// The target terminal's secret sheet is open: the native field hides so it can't cover it.
fn sheet_over(m: &MainWindow, cx: &App) -> bool {
    target_terminal(m, cx).is_some_and(|t| t.read(cx).secret_sheet_open())
}

/// Save the images (off the UI thread, as the sheet does) and type their paths at the cursor,
/// space-separated, as one undoable edit.
fn paste_image_paths(sources: Vec<crate::annotate::Source>, cx: &mut Context<MainWindow>) {
    cx.spawn(async move |this, cx| {
        let saved = cx.background_executor().spawn(async move { sources.into_iter().map(crate::annotate::save).collect::<Vec<_>>() }).await;
        let paths: Vec<String> = saved
            .into_iter()
            .filter_map(|r| r.map_err(|e| eprintln!("midna-app: couldn't paste image: {e}")).ok())
            .map(|p| p.display().to_string())
            .collect();
        if paths.is_empty() {
            return;
        }
        let _ = this.update(cx, |m, _| {
            native(m, |n| unsafe {
                let s = NSString::from_str(&paths.join(" "));
                let _: () = msg_send![&*n.tv, insertText: &*s, replacementRange: n.tv.selectedRange()];
            });
        });
    })
    .detach();
}

/// ⌘-shortcuts for the field (GPUI sees key equivalents before the text view does, and the
/// app has no Edit menu to route them).
pub fn bindings() -> Vec<KeyBinding> {
    let c = Some(CTX_COMPOSER);
    vec![
        KeyBinding::new("cmd-v", ComposerPaste, c),
        KeyBinding::new("cmd-c", ComposerCopy, c),
        KeyBinding::new("cmd-x", ComposerCut, c),
        KeyBinding::new("cmd-a", ComposerSelectAll, c),
        KeyBinding::new("cmd-z", ComposerUndo, c),
        KeyBinding::new("cmd-shift-z", ComposerRedo, c),
    ]
}

// ------------------------------------------------------------------ native view

define_class!(
    /// The composer's text view: an `NSTextView` that reports focus, edits and ↩ / Esc.
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MidnaComposerTextView"]
    struct ComposerTextView;

    impl ComposerTextView {
        /// Hidden, the field never takes keys.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            !self.isHiddenOrHasHiddenAncestor() && unsafe { msg_send![super(self), acceptsFirstResponder] }
        }

        // Hidden, the field is out of the Accessibility tree and refuses writes. Kass keeps the
        // element it saw at chord-down and writes into it when the take ends: a field hidden in
        // between took the text out of sight, and the next open cleared it. Refused, Kass
        // falls back to typing it into what's focused, or keeps it in its Captures.
        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            !ax_hidden(self) && unsafe { msg_send![super(self), isAccessibilityElement] }
        }

        #[unsafe(method(accessibilityIsIgnored))]
        fn accessibility_is_ignored(&self) -> bool {
            ax_hidden(self) || unsafe { msg_send![super(self), accessibilityIsIgnored] }
        }

        #[unsafe(method(isAccessibilityFocused))]
        fn is_accessibility_focused(&self) -> bool {
            !ax_hidden(self) && unsafe { msg_send![super(self), isAccessibilityFocused] }
        }

        #[unsafe(method(isAccessibilitySelectorAllowed:))]
        fn is_accessibility_selector_allowed(&self, s: Sel) -> bool {
            !(ax_hidden(self) && ax_setter(s)) && unsafe { msg_send![super(self), isAccessibilitySelectorAllowed: s] }
        }

        #[unsafe(method(accessibilityIsAttributeSettable:))]
        fn accessibility_is_attribute_settable(&self, attr: &NSString) -> bool {
            !ax_hidden(self) && unsafe { msg_send![super(self), accessibilityIsAttributeSettable: attr] }
        }

        #[unsafe(method(accessibilitySetValue:forAttribute:))]
        fn accessibility_set_value(&self, value: Option<&AnyObject>, attr: &NSString) {
            if !ax_hidden(self) {
                let _: () = unsafe { msg_send![super(self), accessibilitySetValue: value, forAttribute: attr] };
            }
        }

        #[unsafe(method(setAccessibilityValue:))]
        fn set_accessibility_value(&self, value: Option<&AnyObject>) {
            if !ax_hidden(self) {
                let _: () = unsafe { msg_send![super(self), setAccessibilityValue: value] };
            }
        }

        #[unsafe(method(setAccessibilitySelectedText:))]
        fn set_accessibility_selected_text(&self, text: Option<&NSString>) {
            if !ax_hidden(self) {
                let _: () = unsafe { msg_send![super(self), setAccessibilitySelectedText: text] };
            }
        }

        #[unsafe(method(setAccessibilitySelectedTextRange:))]
        fn set_accessibility_selected_text_range(&self, range: NSRange) {
            if !ax_hidden(self) {
                let _: () = unsafe { msg_send![super(self), setAccessibilitySelectedTextRange: range] };
            }
        }

        #[unsafe(method(setAccessibilityFocused:))]
        fn set_accessibility_focused(&self, focused: bool) {
            if !ax_hidden(self) {
                let _: () = unsafe { msg_send![super(self), setAccessibilityFocused: focused] };
            }
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if ok {
                emit(ComposerEvent::Focused(true));
            }
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            if ok {
                emit(ComposerEvent::Focused(false));
            }
            ok
        }

        /// Notes whether the edit removes text, for [`renumber`] (which also runs when a number
        /// is typed into a list item).
        #[unsafe(method(shouldChangeTextInRange:replacementString:))]
        fn should_change_text(&self, range: NSRange, with: Option<&NSString>) -> bool {
            let ok: bool = unsafe { msg_send![super(self), shouldChangeTextInRange: range, replacementString: with] };
            if ok && !RENUMBERING.get() {
                REMOVED.set(with.map_or(0, |s| s.length()) < range.length);
            }
            ok
        }

        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            let _: () = unsafe { msg_send![super(self), didChangeText] };
            if !RENUMBERING.get() && (REMOVED.replace(false) || typed_number(self)) {
                renumber(self);
            }
            emit(ComposerEvent::Changed);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, cmd: Sel) {
            if cmd == sel!(insertNewline:) && !newline_modifier(self.mtm()) {
                emit(ComposerEvent::Submit(self.string().to_string()));
                return;
            }
            if cmd == sel!(insertNewline:) && list_newline(self) {
                return;
            }
            if cmd == sel!(cancelOperation:) {
                emit(ComposerEvent::Cancel);
                return;
            }
            let _: () = unsafe { msg_send![super(self), doCommandBySelector: cmd] };
        }
    }
);

thread_local! {
    /// The edit under way removes text (`shouldChangeTextInRange:` saw it).
    static REMOVED: Cell<bool> = const { Cell::new(false) };
    /// [`renumber`] is changing the text: its own edits don't trigger it again.
    static RENUMBERING: Cell<bool> = const { Cell::new(false) };
}

/// The field is off screen (hidden, or in no window): Accessibility can't see or change it.
fn ax_hidden(tv: &ComposerTextView) -> bool {
    tv.isHiddenOrHasHiddenAncestor() || tv.window().is_none()
}

/// An Accessibility selector that changes the field (`setAccessibility…:`, or the legacy
/// `accessibilitySetValue:forAttribute:`).
fn ax_setter(s: Sel) -> bool {
    let name = s.name().to_bytes();
    name.starts_with(b"setAccessibility") || name.starts_with(b"accessibilitySetValue")
}

/// UTF-16 offset `u` (an `NSRange`'s) in `s` as a byte offset.
fn byte_at(s: &str, u: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= u {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}

fn utf16_at(s: &str, b: usize) -> usize {
    s[..b].encode_utf16().count()
}

/// ⇧↩ on a list item (`list_edit::newline`): the next item's marker after the line break, or
/// an empty item's marker cleared. False for a plain line break (or a selection).
fn list_newline(tv: &ComposerTextView) -> bool {
    let sel = tv.selectedRange();
    if sel.length != 0 {
        return false;
    }
    let text = tv.string().to_string();
    let Some(nl) = crate::list_edit::newline(&text, byte_at(&text, sel.location)) else { return false };
    let (range, insert) = match nl {
        crate::list_edit::Newline::Continue(s) => (sel, s),
        crate::list_edit::Newline::End(r) => (NSRange::new(utf16_at(&text, r.start), utf16_at(&text, r.end) - utf16_at(&text, r.start)), String::new()),
    };
    // `insertText:` goes through `shouldChangeText…`, so ⌘Z undoes it.
    let s = NSString::from_str(&insert);
    let _: () = unsafe { msg_send![tv, insertText: &*s, replacementRange: range] };
    renumber(tv);
    true
}

/// The edit just made left the cursor in a list item's number: a new number was typed there.
fn typed_number(tv: &ComposerTextView) -> bool {
    let sel = tv.selectedRange();
    let text = tv.string().to_string();
    sel.length == 0 && crate::list_edit::in_number(&text, byte_at(&text, sel.location))
}

/// Renumber the numbered list at the cursor (`list_edit::renumber`), keeping the cursor where it
/// was. One undo step.
fn renumber(tv: &ComposerTextView) {
    let sel = tv.selectedRange();
    let text = tv.string().to_string();
    let cursor = byte_at(&text, sel.location);
    let edits = crate::list_edit::renumber(&text, cursor);
    if edits.is_empty() {
        return;
    }
    let Some(storage) = (unsafe { tv.textStorage() }) else { return };
    RENUMBERING.set(true);
    // Back to front, so the earlier ranges still hold.
    for (r, s) in edits.iter().rev() {
        let range = NSRange::new(utf16_at(&text, r.start), utf16_at(&text, r.end) - utf16_at(&text, r.start));
        let s = NSString::from_str(s);
        if tv.shouldChangeTextInRange_replacementString(range, Some(&s)) {
            storage.replaceCharactersInRange_withString(range, &s);
        }
    }
    tv.didChangeText();
    RENUMBERING.set(false);
    let at = utf16_at(&crate::list_edit::apply(&text, &edits), crate::list_edit::shift(cursor, &edits));
    tv.setSelectedRange(NSRange::new(at, 0));
}

/// ⇧↩ / ⌥↩ insert a newline instead of sending.
fn newline_modifier(mtm: MainThreadMarker) -> bool {
    NSApplication::sharedApplication(mtm).currentEvent().map(|e| e.modifierFlags().intersects(NSEventModifierFlags::Shift | NSEventModifierFlags::Option)).unwrap_or(false)
}

struct Native {
    scroll: Retained<NSScrollView>,
    tv: Retained<ComposerTextView>,
    gpui: Retained<NSView>,
    line_h: f64,
    font: Retained<NSFont>,
    styled_for: RefCell<Option<(String, String)>>,
}

impl Native {
    /// Lines the text needs at the current width (1 when empty).
    fn lines(&self) -> usize {
        let used = unsafe {
            match (self.tv.layoutManager(), self.tv.textContainer()) {
                (Some(lm), Some(tc)) => {
                    lm.ensureLayoutForTextContainer(&tc);
                    lm.usedRectForTextContainer(&tc).size.height
                }
                _ => self.line_h,
            }
        };
        ((used / self.line_h).round() as usize).max(1)
    }
}

fn native<R>(m: &MainWindow, f: impl FnOnce(&Native) -> R) -> Option<R> {
    m.composer.native.as_deref().map(f)
}

/// Add the (hidden) scroll view + text view above this window's GPUI view, once.
fn install(m: &mut MainWindow, window: &Window) {
    if m.composer.native.is_some() {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        return;
    };
    let Some(gpui) = (unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }) else {
        return;
    };
    let Some(container) = (unsafe { gpui.superview() }) else {
        return;
    };

    let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(400., 20.));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    let size = scroll.contentSize();
    let tv: Retained<ComposerTextView> = unsafe { msg_send![ComposerTextView::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(0., 0.), size)] };
    let font = NSFont::fontWithName_size(&NSString::from_str(crate::theme::MONO_FONT), FONT_SIZE).unwrap_or_else(|| NSFont::monospacedSystemFontOfSize_weight(FONT_SIZE, 0.0));
    unsafe {
        tv.setRichText(false);
        tv.setImportsGraphics(false);
        tv.setAllowsUndo(true);
        tv.setAutomaticQuoteSubstitutionEnabled(false);
        tv.setAutomaticDashSubstitutionEnabled(false);
        tv.setAutomaticTextReplacementEnabled(false);
        tv.setAutomaticSpellingCorrectionEnabled(false);
        tv.setContinuousSpellCheckingEnabled(false);
        tv.setDrawsBackground(false);
        tv.setFont(Some(&font));
        tv.setTextContainerInset(NSSize::new(0., 0.));
        tv.setMinSize(NSSize::new(0., size.height));
        tv.setMaxSize(NSSize::new(f64::MAX, f64::MAX));
        tv.setVerticallyResizable(true);
        tv.setHorizontallyResizable(false);
        tv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        if let Some(tc) = tv.textContainer() {
            tc.setLineFragmentPadding(0.);
            tc.setWidthTracksTextView(true);
            tc.setContainerSize(NSSize::new(size.width, f64::MAX));
        }
        let _: () = msg_send![&*tv, setAccessibilityLabel: &*NSString::from_str("midna composer")];
    }
    scroll.setDocumentView(Some(&tv));
    scroll.setHidden(true);
    container.addSubview(&scroll);
    let line_h = unsafe { tv.layoutManager() }.map(|lm| lm.defaultLineHeightForFont(&font)).unwrap_or(16.).ceil();
    watch_nav_keys();
    m.composer.native = Some(Rc::new(Native { scroll, tv, gpui, line_h, font, styled_for: RefCell::new(None) }));
}

fn ns_color(c: Hsla) -> Retained<NSColor> {
    let r = c.to_rgb();
    NSColor::colorWithSRGBRed_green_blue_alpha(r.r as f64, r.g as f64, r.b as f64, r.a as f64)
}

fn style(n: &Native, t: &Theme) {
    let key = (format!("{:?}", t.fg), format!("{:?}", t.accent));
    if n.styled_for.borrow().as_ref() == Some(&key) {
        return;
    }
    n.tv.setTextColor(Some(&ns_color(t.fg)));
    n.tv.setInsertionPointColor(Some(&ns_color(t.accent)));
    n.tv.setFont(Some(&n.font));
    *n.styled_for.borrow_mut() = Some(key);
}

/// Unhide the field and make it first responder. True when AppKit accepted it.
fn show_native(m: &mut MainWindow, window: &Window, t: &Theme) -> bool {
    install(m, window);
    native(m, |n| {
        style(n, t);
        n.scroll.setHidden(false);
        let len = n.tv.string().length();
        let sel = n.tv.selectedRange();
        if sel.location > len {
            n.tv.setSelectedRange(NSRange::new(len, 0));
        }
        n.tv.window().map(|w| w.makeFirstResponder(Some(&n.tv))).unwrap_or(false)
    })
    .unwrap_or(false)
}

/// The field, or nothing at all (the window itself, after AppKit dropped a hidden field), has
/// first responder: give it to the GPUI view, which must hold it for keys to reach GPUI.
fn release_native_focus_inner(n: &Native) {
    let Some(w) = n.tv.window() else { return };
    let is = |r: &NSResponder, o: *const AnyObject| std::ptr::eq(r as *const NSResponder as *const AnyObject, o);
    let stranded = match w.firstResponder() {
        None => true,
        Some(r) => is(&r, &**n.tv as *const NSTextView as *const AnyObject) || is(&r, &*w as *const _ as *const AnyObject),
    };
    if stranded && !w.makeFirstResponder(Some(&n.gpui)) {
        w.makeFirstResponder(None);
    }
}

/// Hand AppKit's first responder back to the GPUI view if the field has it.
fn release_native_focus(m: &MainWindow) {
    native(m, release_native_focus_inner);
}

/// Who holds AppKit's first responder in the main window, for logs.
pub fn first_responder(m: &MainWindow) -> String {
    native(m, |n| {
        let Some(w) = n.tv.window() else { return "no window".to_string() };
        let is = |r: &NSResponder, o: *const AnyObject| std::ptr::eq(r as *const NSResponder as *const AnyObject, o);
        let who = match w.firstResponder() {
            None => "none",
            Some(r) if is(&r, &*n.gpui as *const NSView as *const AnyObject) => "gpui",
            Some(r) if is(&r, &**n.tv as *const NSTextView as *const AnyObject) => "composer",
            Some(r) if is(&r, &*w as *const _ as *const AnyObject) => "window",
            Some(_) => "other",
        };
        format!("{who}{}", if w.isKeyWindow() { "" } else { ", window not key" })
    })
    .unwrap_or_else(|| "gpui (composer never shown)".to_string())
}

/// Move the field onto `b` (GPUI window coordinates, top-left origin).
fn place(n: Option<&Native>, b: Bounds<Pixels>, t: &Theme) {
    let Some(n) = n else { return };
    style(n, t);
    let Some(container) = (unsafe { n.gpui.superview() }) else {
        return;
    };
    let (x, y, w, h) = (f64::from(b.origin.x), f64::from(b.origin.y), f64::from(b.size.width), f64::from(b.size.height));
    let vh = n.gpui.bounds().size.height;
    let local = if n.gpui.isFlipped() { NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)) } else { NSRect::new(NSPoint::new(x, vh - y - h), NSSize::new(w, h)) };
    let frame = n.gpui.convertRect_toView(local, Some(&container));
    let cur = n.scroll.frame();
    if (cur.origin.x - frame.origin.x).abs() > 0.5
        || (cur.origin.y - frame.origin.y).abs() > 0.5
        || (cur.size.width - frame.size.width).abs() > 0.5
        || (cur.size.height - frame.size.height).abs() > 0.5
    {
        let width_changed = (cur.size.width - frame.size.width).abs() > 0.5;
        n.scroll.setFrame(frame);
        let len = n.tv.string().length();
        n.tv.scrollRangeToVisible(NSRange::new(len, 0));
        if width_changed {
            emit(ComposerEvent::Changed); // wrapping may have changed the line count
        }
    }
}

/// Arrows and the other navigation keys go straight to the field while it is first responder,
/// from an AppKit local monitor that sees each key before the window does (installed once).
/// AppKit offers these keys as key *equivalents*, to the GPUI view first, and GPUI runs them
/// through its own input context before any GPUI handler sees them: that swallowed the key some
/// of the time, and handing it on to the field from inside that context sometimes typed the
/// arrow's invisible private-use character over the selection. ⌃ (Spaces) and ⌘⌥ (switch
/// terminal) stay GPUI's.
fn watch_nav_keys() {
    thread_local! {
        static WATCHING: Cell<bool> = const { Cell::new(false) };
    }
    if WATCHING.replace(true) {
        return;
    }
    let handler = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit hands the monitor a live event.
        let e = unsafe { event.as_ref() };
        match nav_target(e) {
            Some(tv) => {
                tv.keyDown(e);
                std::ptr::null_mut()
            }
            None => event.as_ptr(),
        }
    });
    // SAFETY: main thread (from `install`); the handler returns the event or nil.
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &handler) };
    std::mem::forget(monitor);
}

/// The composer field that should take this key down itself, if any.
fn nav_target(e: &NSEvent) -> Option<Retained<NSResponder>> {
    let c = e.charactersIgnoringModifiers()?.to_string().chars().next()? as u32;
    // ↑ ↓ ← →, forward delete, home, end, page up, page down (NS*FunctionKey).
    let nav = matches!(c, 0xF700..=0xF703 | 0xF728 | 0xF729 | 0xF72B | 0xF72C | 0xF72D);
    let f = e.modifierFlags();
    if !nav || f.contains(NSEventModifierFlags::Control) || f.contains(NSEventModifierFlags::Command | NSEventModifierFlags::Option) {
        return None;
    }
    let r = e.window(MainThreadMarker::new()?)?.firstResponder()?;
    r.isKindOfClass(ComposerTextView::class()).then_some(r)
}

fn send_to_text_view(m: &MainWindow, s: Sel) {
    native(m, |n| unsafe {
        let _: () = msg_send![&*n.tv, performSelector: s, withObject: std::ptr::null::<AnyObject>()];
    });
}

fn undo(m: &MainWindow, redo: bool) {
    native(m, |n| {
        if let Some(um) = n.tv.undoManager() {
            if redo { um.redo() } else { um.undo() }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{KassSkip, KeyWindow, ax_setter, encode, follow, kass_skip, route, with_notes};
    use crate::app::{Overlay, Screen};
    use ::core::prelude::v1::test;
    use objc2::sel;

    #[test]
    fn kass_dictation_goes_to_the_main_window_youre_on() {
        assert_eq!(route(KeyWindow::This, false), Some(false));
        // The badge (or a subagent's log) is key: this window takes the dictation, and the keys.
        assert_eq!(route(KeyWindow::Passive, false), Some(true));
        // Settings, a popout or another app is key: Kass inserts there.
        assert_eq!(route(KeyWindow::Typing, false), None);
        assert_eq!(route(KeyWindow::Away, false), None);
        assert_eq!(route(KeyWindow::Away, true), Some(false));
    }

    #[test]
    fn only_accessibility_setters_are_refused() {
        assert!(ax_setter(sel!(setAccessibilitySelectedText:)));
        assert!(ax_setter(sel!(setAccessibilityValue:)));
        assert!(ax_setter(sel!(accessibilitySetValue:forAttribute:)));
        assert!(!ax_setter(sel!(accessibilityValue)));
        assert!(!ax_setter(sel!(accessibilitySelectedTextRange)));
    }

    #[test]
    fn kass_will_begin_names_the_first_gate_that_fails() {
        let t = Screen::Terminal;
        let none = Overlay::None;
        assert_eq!(kass_skip(true, true, t, none, true, false, true), None);
        assert_eq!(kass_skip(false, true, t, none, true, false, true), Some(KassSkip::NotKey));
        // Not key wins over everything else (Kass asked the app, not this window).
        assert_eq!(kass_skip(false, false, Screen::Rules, Overlay::CommandBar, false, true, false), Some(KassSkip::NotKey));
        assert_eq!(kass_skip(true, false, t, none, true, false, true), Some(KassSkip::NotConnected));
        assert_eq!(kass_skip(true, true, Screen::Insights, none, true, false, true), Some(KassSkip::NotTerminalScreen(Screen::Insights)));
        assert_eq!(kass_skip(true, true, t, Overlay::NeedsYou, true, false, true), Some(KassSkip::Overlay(Overlay::NeedsYou)));
        assert_eq!(kass_skip(true, true, t, none, false, false, true), Some(KassSkip::NoTerminal));
        assert_eq!(kass_skip(true, true, t, none, true, true, true), Some(KassSkip::SecretSheet));
        assert_eq!(kass_skip(true, true, t, none, true, false, false), Some(KassSkip::OtherTerminal));
    }

    #[test]
    fn kass_skip_reasons_are_short() {
        assert_eq!(KassSkip::NotKey.reason(KeyWindow::Away.label()), "window not key (key: another app)");
        assert_eq!(KassSkip::Overlay(Overlay::CommandBar).reason("-"), "overlay CommandBar");
        assert_eq!(KassSkip::NotTerminalScreen(Screen::Rules).reason("-"), "screen Rules");
        assert_eq!(KassSkip::SecretSheet.reason("-"), "secret sheet open");
        assert_eq!(KassSkip::OtherTerminal.reason("-"), "composer open on another terminal");
    }

    #[test]
    fn notes_follow_the_text_on_their_own_line() {
        assert_eq!(with_notes("look at this\n", ""), "look at this\n");
        assert_eq!(with_notes("look at this\n", "Annotations"), "look at this\nAnnotations");
        assert_eq!(encode(&with_notes("look", "1. fix"), true), "\x1b[200~look\r1. fix\x1b[201~");
    }

    #[test]
    fn the_composer_stays_with_its_terminal() {
        let none = |_: &str| false;
        let b_has = |id: &str| id == "b";
        // Open on the selected terminal, or nothing open and no draft: leave it.
        assert_eq!(follow(Some("a"), Some("a"), none), (false, false));
        assert_eq!(follow(None, Some("a"), none), (false, false));
        // Another terminal selected: put it away; if that one has a draft, bring it back.
        assert_eq!(follow(Some("a"), Some("b"), none), (true, false));
        assert_eq!(follow(Some("a"), Some("b"), b_has), (true, true));
        assert_eq!(follow(Some("a"), None, none), (true, false));
        // Nothing open: the selected terminal's draft comes back.
        assert_eq!(follow(None, Some("b"), b_has), (false, true));
        assert_eq!(follow(Some("b"), Some("b"), b_has), (false, false));
    }

    #[test]
    fn encode_single_and_multi_line() {
        assert_eq!(encode("fix the tests", true), "fix the tests");
        assert_eq!(encode("fix the tests\n\n", true), "fix the tests");
        assert_eq!(encode("one\r\ntwo\n", true), "\x1b[200~one\rtwo\x1b[201~");
        assert_eq!(encode("one\ntwo", false), "one\rtwo");
        assert_eq!(encode("a\n\x1b[201~b", true), "\x1b[200~a\rb\x1b[201~");
    }
}
