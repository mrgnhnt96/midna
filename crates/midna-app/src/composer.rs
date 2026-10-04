//! The composer: a native `NSTextView` under the terminal that appears only while dictating
//! (Kass) or when opened by hand (`keys.composer`, default ⇧⌘D). Proven in
//! `spikes/kass-composer`: gpui-kit's own input fails Kass's Accessibility checks, a real
//! `NSTextView` passes them (role AXTextArea, settable AXSelectedText, readable value and
//! selection, AXStringForRange).
//!
//! Layout follows Main.dc.html (`mode=dictating`): a "Kass · listening" pill, then an
//! accent-bordered box with `❯`, the field and "↩ send · esc cancel". GPUI draws the box; the
//! `NSTextView` (inside an `NSScrollView`) is a sibling *above* the GPUI view, moved onto the
//! box's field area every paint (`place`).
//!
//! Keys: ↩ sends the text to the target terminal with `session.input{enter:true}` (multi-line
//! text goes as one bracketed paste when the terminal asked for it) and closes; ⇧↩ / ⌥↩ insert
//! a newline; Esc cancels (clears and hides). The target is the terminal selected when the
//! composer opened.
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
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSAutoresizingMaskOptions, NSBorderType, NSColor, NSEventModifierFlags, NSFont, NSResponder, NSScrollView, NSText, NSTextView, NSView};
use objc2_foundation::{NSObject, NSPoint, NSRange, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::json;
use std::cell::RefCell;
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
}

static EVENTS: OnceLock<async_channel::Sender<ComposerEvent>> = OnceLock::new();

fn emit(e: ComposerEvent) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.try_send(e);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Manual,
    Kass,
}

pub struct Composer {
    pub open: bool,
    pub source: Source,
    /// Between Kass's `dictationWillBegin` and `dictationDidEnd`.
    pub listening: bool,
    /// The terminal the text goes to (selected when the composer opened).
    pub target: Option<String>,
    pub lines: usize,
    pub focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl Composer {
    pub fn new(window: &mut Window, cx: &mut Context<MainWindow>) -> Self {
        let focus = cx.focus_handle();
        let mut tasks = vec![];
        let (tx, rx) = async_channel::unbounded::<ComposerEvent>();
        let _ = EVENTS.set(tx);
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(ev) = rx.recv().await {
                if this.update_in(cx, |m, window, cx| on_event(m, ev, window, cx)).is_err() {
                    break;
                }
            }
        }));
        let (ktx, krx) = async_channel::unbounded::<KassEvent>();
        kass::start(move |ev| {
            let _ = ktx.try_send(ev);
        });
        tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Ok(ev) = krx.recv().await {
                if this.update_in(cx, |m, window, cx| on_kass(m, ev, window, cx)).is_err() {
                    break;
                }
            }
        }));
        if let Ok(text) = crate::dev::var("MIDNA_DEBUG_COMPOSER") {
            // Dev: open the composer with `text` once terminals have loaded (screenshots).
            tasks.push(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(1500)).await;
                let _ = this.update_in(cx, |m, window, cx| {
                    open(m, Source::Manual, window, cx);
                    native(|n| n.tv.setString(&NSString::from_str(&text.replace("\\n", "\n"))));
                    m.composer.listening = crate::dev::var("MIDNA_DEBUG_COMPOSER_LISTENING").is_ok();
                    on_event(m, ComposerEvent::Changed, window, cx);
                });
            }));
        }
        let subs = vec![cx.on_blur(&focus, window, |_, _, _| release_native_focus())];
        Composer { open: false, source: Source::Manual, listening: false, target: None, lines: 1, focus, _tasks: tasks, _subs: subs }
    }
}

// ------------------------------------------------------------------ main-window logic

/// Can the composer be shown right now (a connected terminal pane, nothing over it)?
fn can_show(m: &MainWindow) -> bool {
    m.conn == ConnState::Connected && m.screen == Screen::Terminal && m.overlay == Overlay::None && m.selected.is_some()
}

fn open(m: &mut MainWindow, source: Source, window: &mut Window, cx: &mut Context<MainWindow>) -> bool {
    if !m.composer.open {
        m.composer.open = true;
        m.composer.source = source;
        m.composer.target = m.selected.clone();
        m.composer.lines = 1;
        native(|n| n.tv.setString(&NSString::from_str("")));
    } else if source == Source::Kass {
        m.composer.source = Source::Kass;
    }
    let t = cx.global::<Theme>().clone();
    let ok = show_native(window, &t);
    m.composer.focus.focus(window, cx);
    cx.notify();
    ok
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.composer.open = false;
    m.composer.listening = false;
    m.composer.target = None;
    // Hand focus back before hiding: hiding the first responder makes AppKit give it to the
    // bare window, and plain keys (and ⌘'s release) would then go nowhere.
    release_native_focus();
    native(|n| {
        n.tv.setString(&NSString::from_str(""));
        n.scroll.setHidden(true);
    });
    m.focus_terminal(window, cx);
    cx.notify();
}

/// `keys.composer`: open by hand, or bring focus back to an open composer.
fn open_manual(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if !can_show(m) {
        m.toast("The composer opens over a terminal.", cx);
        return;
    }
    open(m, Source::Manual, window, cx);
}

fn on_kass(m: &mut MainWindow, ev: KassEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    // Settings shows "handshake detected"; it reads kass::handshake_detected() on render.
    cx.refresh_windows();
    let debug = crate::dev::var("MIDNA_DEBUG").is_ok();
    match ev {
        KassEvent::WillBegin { mode } => {
            let t0 = Instant::now();
            let active = window.is_window_active() || crate::dev::var("MIDNA_KASS_ANY_WINDOW").is_ok();
            if !active || !can_show(m) {
                // Kass only asks the frontmost app; if the main window isn't the key window
                // or there's no terminal pane, stay out of the way: no reply, Kass carries on.
                if debug {
                    eprintln!("midna-app: kass willBegin ({mode}) ignored: active={} can_show={}", window.is_window_active(), can_show(m));
                }
                return;
            }
            let ok = open(m, Source::Kass, window, cx);
            m.composer.listening = true;
            if ok {
                kass::post_ready();
            }
            if debug {
                eprintln!("midna-app: kass willBegin ({mode}): composer first responder={ok}, ready posted in {:?}", t0.elapsed());
            }
        }
        KassEvent::DidEnd { outcome } => {
            m.composer.listening = false;
            if !m.composer.open {
                return;
            }
            let text = text();
            if debug {
                eprintln!("midna-app: kass didEnd ({outcome:?}): {} chars", text.chars().count());
            }
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
            let lines = native(|n| n.lines()).unwrap_or(1);
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
    }
}

fn submit(m: &mut MainWindow, text: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(id) = m.composer.target.clone().or_else(|| m.selected.clone()) else {
        close(m, window, cx);
        return;
    };
    if text.trim().is_empty() {
        close(m, window, cx);
        return;
    }
    let bpaste = m.terminal.as_ref().filter(|t| t.read(cx).session_id == id).map(|t| t.read(cx).bracketed_paste()).unwrap_or(true);
    let data = encode(&text, bpaste);
    close(m, window, cx);
    m.rpc("session.input", json!({"id": id, "text": data, "enter": true}), cx, |_, _, _, _| {});
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
pub fn text() -> String {
    native(|n| n.tv.string().to_string()).unwrap_or_default()
}

// ------------------------------------------------------------------ rendering

/// Keep the native view's visibility in step with the layout (called every render).
pub fn sync(m: &MainWindow) {
    let visible = m.composer.open && can_show(m);
    native(|n| {
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
    if !m.composer.open || !can_show(m) {
        return None;
    }
    let line_h = native(|n| n.line_h).unwrap_or(16.) as f32;
    let field_h = line_h * m.composer.lines.clamp(1, MAX_LINES) as f32;
    let target = m.composer.target.as_ref().and_then(|id| m.sessions.iter().find(|s| &s.id == id)).map(|s| s.name.clone()).unwrap_or_default();
    let theme = t.clone();
    let field = canvas(|b, _, _| b, move |_, b, _, _| place(b, &theme)).flex_1().min_w_0().h(px(field_h));
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
                    .child(format!("{}▏", text())),
            )
            .into_any_element()
    } else {
        field.into_any_element()
    };
    let pill = m.composer.listening.then(|| {
        let bar = |h: f32| div().w(px(2.)).h(px(h)).bg(t.accent);
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(12.))
            .py(px(4.))
            .rounded_full()
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .text_size(px(12.))
            .child(div().flex().items_center().gap(px(2.)).h(px(12.)).child(bar(5.)).child(bar(11.)).child(bar(7.)).child(bar(12.)).child(bar(4.)))
            .child("Kass · listening")
    });
    let hint = if target.is_empty() { "↩ send · esc cancel".to_string() } else { format!("↩ send to {target} · esc cancel") };
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
                native(|n| {
                    if let Some(w) = n.tv.window() {
                        w.makeFirstResponder(Some(&n.tv));
                    }
                });
            }),
        )
        .child(div().font_family(t.mono_font.clone()).text_size(px(FONT_SIZE as f32)).line_height(px(line_h)).text_color(t.accent).child("❯"))
        .child(field)
        .child(div().flex_none().text_size(px(11.)).line_height(px(line_h)).text_color(t.dim).whitespace_nowrap().child(hint));
    Some(
        div()
            .id("composer")
            .key_context(CTX_COMPOSER)
            .track_focus(&m.composer.focus)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(8.))
            .px(px(16.))
            .pt(px(8.))
            .pb(px(16.))
            .on_mouse_down_out(cx.listener(|_, _, _, _| release_native_focus()))
            .children(pill)
            .child(boxed)
            .into_any_element(),
    )
}

pub fn register<E: InteractiveElement>(el: E, cx: &mut Context<MainWindow>) -> E {
    let fwd = |s: Sel| move |_: &mut MainWindow, _: &mut Window, _: &mut Context<MainWindow>| send_to_text_view(s);
    let paste = fwd(sel!(paste:));
    let copy = fwd(sel!(copy:));
    let cut = fwd(sel!(cut:));
    let all = fwd(sel!(selectAll:));
    el.on_action(cx.listener(|m, _: &OpenComposer, w, cx| open_manual(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerPaste, w, cx| paste(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerCopy, w, cx| copy(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerCut, w, cx| cut(m, w, cx)))
        .on_action(cx.listener(move |m, _: &ComposerSelectAll, w, cx| all(m, w, cx)))
        .on_action(cx.listener(|_, _: &ComposerUndo, _, _| undo(false)))
        .on_action(cx.listener(|_, _: &ComposerRedo, _, _| undo(true)))
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

        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            let _: () = unsafe { msg_send![super(self), didChangeText] };
            emit(ComposerEvent::Changed);
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, cmd: Sel) {
            if cmd == sel!(insertNewline:) && !newline_modifier(self.mtm()) {
                emit(ComposerEvent::Submit(self.string().to_string()));
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

thread_local! {
    static NATIVE: RefCell<Option<Native>> = const { RefCell::new(None) };
}

fn native<R>(f: impl FnOnce(&Native) -> R) -> Option<R> {
    NATIVE.with(|n| n.borrow().as_ref().map(f))
}

/// Add the (hidden) scroll view + text view above the GPUI view, once.
fn install(window: &Window) {
    if NATIVE.with(|n| n.borrow().is_some()) {
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
    NATIVE.with(|n| *n.borrow_mut() = Some(Native { scroll, tv, gpui, line_h, font, styled_for: RefCell::new(None) }));
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
fn show_native(window: &Window, t: &Theme) -> bool {
    install(window);
    native(|n| {
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
fn release_native_focus() {
    native(release_native_focus_inner);
}

/// Who holds AppKit's first responder in the main window, for logs.
pub fn first_responder() -> String {
    native(|n| {
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
fn place(b: Bounds<Pixels>, t: &Theme) {
    native(|n| {
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
    });
}

fn send_to_text_view(s: Sel) {
    native(|n| unsafe {
        let _: () = msg_send![&*n.tv, performSelector: s, withObject: std::ptr::null::<AnyObject>()];
    });
}

fn undo(redo: bool) {
    native(|n| {
        if let Some(um) = n.tv.undoManager() {
            if redo { um.redo() } else { um.undo() }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::encode;
    use ::core::prelude::v1::test;

    #[test]
    fn encode_single_and_multi_line() {
        assert_eq!(encode("fix the tests", true), "fix the tests");
        assert_eq!(encode("fix the tests\n\n", true), "fix the tests");
        assert_eq!(encode("one\r\ntwo\n", true), "\x1b[200~one\rtwo\x1b[201~");
        assert_eq!(encode("one\ntwo", false), "one\rtwo");
        assert_eq!(encode("a\n\x1b[201~b", true), "\x1b[200~a\rb\x1b[201~");
    }
}
