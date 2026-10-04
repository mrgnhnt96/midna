//! Spike: GPUI window with a "composer" bar that Kass should treat as a text field.
//! MIDNA_MODE=a -> native NSTextView overlay; MIDNA_MODE=b -> gpui-kit Input.
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSColor, NSFont, NSTextDelegate, NSTextView, NSTextViewDelegate, NSView};
use objc2_foundation::{NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNotificationSuspensionBehavior, NSNumber, NSDictionary};
use std::time::Duration;

const BAR_H: f64 = 56.0;
static HANDSHAKE: AtomicBool = AtomicBool::new(false);
static COMPOSER_VISIBLE: AtomicBool = AtomicBool::new(true);
static RESTORE_TERM_FOCUS: AtomicBool = AtomicBool::new(false);
const WILL_BEGIN: &str = "com.mrgnhnt.kass.dictationWillBegin";
const READY: &str = "com.mrgnhnt.kass.dictationReady";
const DID_END: &str = "com.mrgnhnt.kass.dictationDidEnd";

thread_local! {
    static TEXT_VIEW: RefCell<Option<Retained<NSTextView>>> = const { RefCell::new(None) };
    static DELEGATE: RefCell<Option<Retained<ComposerDelegate>>> = const { RefCell::new(None) };
    static SENT: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static OBSERVER: RefCell<Option<Retained<KassObserver>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MidnaComposerDelegate"]
    struct ComposerDelegate;

    unsafe impl NSObjectProtocol for ComposerDelegate {}
    unsafe impl NSTextDelegate for ComposerDelegate {}
    unsafe impl NSTextViewDelegate for ComposerDelegate {
        #[unsafe(method(textView:doCommandBySelector:))]
        fn do_command(&self, tv: &NSTextView, cmd: Sel) -> objc2::runtime::Bool {
            if cmd == sel!(insertNewline:) {
                let s = unsafe { tv.string() }.to_string();
                println!("SENT_TO_PTY(A): {s:?}");
                SENT.with(|v| v.borrow_mut().push(s));
                unsafe { tv.setString(&NSString::from_str("")) };
                if HANDSHAKE.load(Ordering::SeqCst) { hide_composer("enter"); }
                return objc2::runtime::Bool::YES;
            }
            if cmd == sel!(cancelOperation:) && HANDSHAKE.load(Ordering::SeqCst) {
                hide_composer("esc");
                return objc2::runtime::Bool::YES;
            }
            objc2::runtime::Bool::NO
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MidnaKassObserver"]
    struct KassObserver;

    unsafe impl NSObjectProtocol for KassObserver {}
    impl KassObserver {
        #[unsafe(method(willBegin:))]
        fn will_begin(&self, n: &NSNotification) {
            let t0 = std::time::Instant::now();
            let (pid, mode) = unsafe {
                let info: Option<Retained<AnyObject>> = msg_send![n, userInfo];
                let Some(info) = info else { return };
                let pid: Option<Retained<AnyObject>> = msg_send![&*info, objectForKey: &*NSString::from_str("pid")];
                let mode: Option<Retained<AnyObject>> = msg_send![&*info, objectForKey: &*NSString::from_str("mode")];
                let pid: i32 = pid.map(|p| msg_send![&*p, intValue]).unwrap_or(-1);
                let mode: String = mode.map(|m| { let d: Retained<NSString> = msg_send![&*m, description]; d.to_string() }).unwrap_or_default();
                (pid, mode)
            };
            if pid != std::process::id() as i32 { println!("willBegin for pid {pid}: not us, ignoring"); return; }
            // Spike: the focused terminal is always an agent/shell.
            show_composer("existing draft");
            let c = NSDistributedNotificationCenter::defaultCenter();
            let num = NSNumber::new_i32(pid);
            let key = NSString::from_str("pid");
            let info = NSDictionary::<NSString, AnyObject>::from_slices(&[&*key], &[&**num as &AnyObject]);
            unsafe { c.postNotificationName_object_userInfo_deliverImmediately(&NSString::from_str(READY), None, Some(&*info.cast_unchecked::<AnyObject, AnyObject>()), true) };
            println!("willBegin mode={mode}: composer shown + ready posted in {:?}; first responder={}", t0.elapsed(), first_responder_desc());
        }
        #[unsafe(method(didEnd:))]
        fn did_end(&self, _n: &NSNotification) {
            let empty = TEXT_VIEW.with(|t| t.borrow().as_ref().map(|tv| tv.string().length() == 0).unwrap_or(true));
            println!("didEnd: composer empty={empty}");
            if empty { hide_composer("didEnd-empty"); }
        }
    }
);

fn install_observer() {
    let mtm = MainThreadMarker::new().unwrap();
    let obs: Retained<KassObserver> = unsafe { msg_send![KassObserver::alloc(mtm), init] };
    let c = NSDistributedNotificationCenter::defaultCenter();
    unsafe {
        c.addObserver_selector_name_object_suspensionBehavior(&obs, sel!(willBegin:), Some(&NSString::from_str(WILL_BEGIN)), None, NSNotificationSuspensionBehavior::DeliverImmediately);
        c.addObserver_selector_name_object_suspensionBehavior(&obs, sel!(didEnd:), Some(&NSString::from_str(DID_END)), None, NSNotificationSuspensionBehavior::DeliverImmediately);
    }
    OBSERVER.with(|o| *o.borrow_mut() = Some(obs));
    println!("kass observer installed");
}

fn show_composer(text: &str) {
    TEXT_VIEW.with(|t| {
        let b = t.borrow();
        let Some(tv) = b.as_ref() else { return };
        tv.setString(&NSString::from_str(text));
        tv.setHidden(false);
        COMPOSER_VISIBLE.store(true, Ordering::SeqCst);
        let len = tv.string().length();
        tv.setSelectedRange(objc2_foundation::NSRange::new(len, 0));
    });
    focus_native(true);
}

fn hide_composer(why: &str) {
    TEXT_VIEW.with(|t| { if let Some(tv) = t.borrow().as_ref() { tv.setHidden(true); } });
    COMPOSER_VISIBLE.store(false, Ordering::SeqCst);
    focus_native(false);
    RESTORE_TERM_FOCUS.store(true, Ordering::SeqCst);
    println!("composer hidden ({why})");
}

fn install_native_composer(window: &Window) {
    let mtm = MainThreadMarker::new().unwrap();
    let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).unwrap().as_raw() else { panic!() };
    let gpui_view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let container = unsafe { gpui_view.superview() }.expect("gpui view has superview");
    let flipped = gpui_view.isFlipped();
    let cb = container.bounds();
    println!("gpui view flipped={flipped} container flipped={} bounds={:?} backingScale={:?}",
        container.isFlipped(), cb, gpui_view.window().map(|w| w.backingScaleFactor()));
    // Container (window contentView) is unflipped: y=0 is bottom.
    let frame = NSRect::new(NSPoint::new(8.0, 8.0), NSSize::new(cb.size.width - 16.0, BAR_H - 16.0));
    let tv = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
    tv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMaxYMargin);
    unsafe {
        tv.setRichText(false);
        tv.setAutomaticQuoteSubstitutionEnabled(false);
        tv.setAutomaticDashSubstitutionEnabled(false);
        tv.setAutomaticTextReplacementEnabled(false);
        tv.setAutomaticSpellingCorrectionEnabled(false);
        tv.setContinuousSpellCheckingEnabled(false);
        tv.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(13.0, 0.0)));
        tv.setBackgroundColor(&NSColor::colorWithSRGBRed_green_blue_alpha(0.12, 0.12, 0.16, 1.0));
        tv.setTextColor(Some(&NSColor::whiteColor()));
        tv.setInsertionPointColor(Some(&NSColor::whiteColor()));
        tv.setString(&NSString::from_str("git commit -m 'wip'"));
    }
    let del: Retained<ComposerDelegate> = unsafe { msg_send![ComposerDelegate::alloc(mtm), init] };
    tv.setDelegate(Some(ProtocolObject::from_ref(&*del)));
    container.addSubview(&tv); // sibling ABOVE gpui view
    let w = gpui_view.window().unwrap();
    let ok = if HANDSHAKE.load(Ordering::SeqCst) {
        tv.setHidden(true);
        COMPOSER_VISIBLE.store(false, Ordering::SeqCst);
        w.makeFirstResponder(Some(gpui_view))
    } else { w.makeFirstResponder(Some(&tv)) };
    println!("initial makeFirstResponder = {ok}");
    // in-process AX self-check (no TCC needed)
    unsafe {
        let role: Option<Retained<NSString>> = msg_send![&*tv, accessibilityRole];
        let is_el: bool = msg_send![&*tv, isAccessibilityElement];
        let r: objc2_foundation::NSRange = msg_send![&*tv, accessibilitySelectedTextRange];
        let n: isize = msg_send![&*tv, accessibilityNumberOfCharacters];
        let kids: Option<Retained<objc2_foundation::NSArray<AnyObject>>> = msg_send![&*container, accessibilityChildren];
        println!("in-process: role={role:?} isAXElement={is_el} selRange={r:?} nChars={n} contentView.accessibilityChildren={}",
            kids.map(|k| k.count()).unwrap_or(0));
    }
    TEXT_VIEW.with(|t| *t.borrow_mut() = Some(tv));
    DELEGATE.with(|d| *d.borrow_mut() = Some(del));
    if HANDSHAKE.load(Ordering::SeqCst) { install_observer(); }
}

fn first_responder_desc() -> String {
    TEXT_VIEW.with(|t| {
        t.borrow().as_ref().and_then(|tv| tv.window()).and_then(|w| w.firstResponder())
            .map(|r| r.class().name().to_string_lossy().into_owned()).unwrap_or("-".into())
    })
}

struct Spike {
    mode_b: bool,
    input: Option<Entity<InputState>>,
    live: String,
    fr: String,
    log: Vec<String>,
    term_focus: FocusHandle,
    keys_seen_by_gpui: usize,
    live_changed: bool,
    bar_visible: bool,
    log_seen: usize,
}

impl Spike {
    fn new(mode_b: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = mode_b.then(|| {
            let st = cx.new(|cx| InputState::new(window, cx));
            st.update(cx, |s, cx| s.set_value("git commit -m 'wip'", window, cx));
            cx.subscribe_in(&st, window, |this: &mut Self, st, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = ev {
                    let v = st.read(cx).value().to_string();
                    println!("SENT_TO_PTY(B): {v:?}");
                    this.log.push(v);
                    st.update(cx, |s, cx| s.set_value("", window, cx));
                }
            }).detach();
            st.update(cx, |s, cx| s.focus(window, cx));
            st
        });
        let term_focus = cx.focus_handle();
        if !mode_b { install_native_composer(window); }
        if HANDSHAKE.load(Ordering::SeqCst) { term_focus.focus(window, cx); }
        cx.spawn_in(window, async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let r = this.update_in(cx, |this, window, cx| {
                SENT.with(|s| this.log.extend(s.borrow_mut().drain(..)));
                let live = match &this.input {
                    Some(i) => i.read(cx).value().to_string(),
                    None => TEXT_VIEW.with(|t| t.borrow().as_ref().map(|tv| unsafe { tv.string() }.to_string()).unwrap_or_default()),
                };
                let vis = COMPOSER_VISIBLE.load(Ordering::SeqCst);
                this.live_changed = live != this.live || vis != this.bar_visible || !this.log.is_empty() && this.log.len() != this.log_seen;
                this.live = live; this.bar_visible = vis; this.log_seen = this.log.len();
                if let Ok(c) = std::fs::read_to_string(&std::env::var("MIDNA_SPIKE_CMD").unwrap_or_default()) {
                    let _ = std::fs::remove_file(&std::env::var("MIDNA_SPIKE_CMD").unwrap_or_default());
                    match c.trim() {
                        "focus_term" => { focus_native(false); this.term_focus.focus(window, cx); println!("cmd: focus_term"); }
                        "focus_composer" => { focus_native(true); println!("cmd: focus_composer"); }
                        _ => {}
                    }
                }
                if RESTORE_TERM_FOCUS.swap(false, Ordering::SeqCst) { this.term_focus.focus(window, cx); }
                let fr = first_responder_desc();
                if fr != this.fr || this.live_changed { this.fr = fr; cx.notify(); }
            });
            if r.is_err() { break; }
        }).detach();
        Self { mode_b, input, live: String::new(), fr: String::new(), log: vec![], term_focus, keys_seen_by_gpui: 0, live_changed: false, bar_visible: true, log_seen: 0 }
    }
}

impl Render for Spike {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut term = div().id("term").flex_1().w_full().p_2().bg(rgb(0x0b0b0f)).text_color(rgb(0x9fef9f))
            .font_family("Menlo").text_sm().track_focus(&self.term_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                this.keys_seen_by_gpui += 1;
                println!("GPUI terminal key: {:?}", ev.keystroke);
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| {
                this.term_focus.focus(window, cx);
                focus_native(false);
                println!("terminal clicked; gpui focus now terminal");
            }))
            .child(format!("midna spike — mode {}  a11y_active={}", if self.mode_b { "B (gpui Input)" } else { "A (NSTextView)" }, window.is_a11y_active()))
            .child(format!("first responder: {}   gpui keys seen: {}", self.fr, self.keys_seen_by_gpui))
            .child("$ fake shell prompt");
        for l in &self.log { term = term.child(format!("$ {l}   <- sent to PTY")); }
        term = term.child(format!("composer live text: {:?}", self.live));
        let bar_h = if self.input.is_some() || self.bar_visible { BAR_H as f32 } else { 0.0 };
        let bar = div().h(px(bar_h)).w_full().p_2().bg(rgb(0x1e1e28));
        let bar = match &self.input { Some(i) => bar.child(Input::new(i)), None => bar.child("") };
        div().size_full().flex().flex_col().child(term).child(bar)
    }
}

fn main() {
    let mode_b = std::env::var("MIDNA_MODE").map(|m| m == "b").unwrap_or(false);
    HANDSHAKE.store(std::env::var("MIDNA_HANDSHAKE").is_ok(), Ordering::SeqCst);
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(720.), px(420.)), cx))),
            ..Default::default()
        };
        gpui_kit::open_window(opts, cx, |window, cx| cx.new(|cx| Spike::new(mode_b, window, cx))).expect("window");
        cx.activate(true);
    });
}

/// Move AppKit first responder between the GPUI view and the native composer.
fn focus_native(composer: bool) {
    TEXT_VIEW.with(|t| {
        let b = t.borrow();
        let Some(tv) = b.as_ref() else { return };
        let w = tv.window().unwrap();
        let ok = if composer { w.makeFirstResponder(Some(tv)) } else {
            let gpui_view = unsafe { tv.superview().unwrap().subviews() }.objectAtIndex(0);
            w.makeFirstResponder(Some(&gpui_view))
        };
        println!("makeFirstResponder(composer={composer}) = {ok}");
    });
}
