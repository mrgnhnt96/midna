//! Spike: can an agent drive window management (own GPUI windows + other apps via AX)?
//! `spike-win app` runs the GPUI app (main window + control socket).
//! `spike-win <cmd...>` sends a command over the socket and prints the reply.
use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSRunningApplication, NSScreen, NSWindow, NSWorkspace};
use objc2_foundation::{NSArray, NSNumber, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::cell::RefCell;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::Duration;

fn sock_path() -> String {
    std::env::var("SPIKE_WIN_SOCK").unwrap_or_else(|_| format!("{}/spike-win.sock", std::env::temp_dir().display()))
}

type Req = (String, mpsc::Sender<String>);

thread_local! {
    static MAIN: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
    static SAT: RefCell<Option<(AnyWindowHandle, Retained<NSWindow>)>> = const { RefCell::new(None) };
    static EXTRA: RefCell<Vec<(AnyWindowHandle, Retained<NSWindow>)>> = const { RefCell::new(Vec::new()) };
    static PIN: RefCell<Option<Pin>> = const { RefCell::new(None) };
}

struct Pin { observer: *mut c_void, app: *mut c_void, win: *mut c_void, raises: usize }

fn ns_window(window: &Window) -> Retained<NSWindow> {
    let RawWindowHandle::AppKit(h) = HasWindowHandle::window_handle(window).unwrap().as_raw() else { panic!() };
    let view: &objc2_app_kit::NSView = unsafe { h.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
    view.window().expect("window")
}

struct Label(String);
impl Render for Label {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().p_2().bg(rgb(0x14141c)).text_color(rgb(0x9fef9f)).font_family("Menlo").text_sm().child(self.0.clone())
    }
}

// ---------------- AppKit helpers ----------------
fn r(rect: NSRect) -> String {
    format!("({:.0},{:.0} {:.0}x{:.0})", rect.origin.x, rect.origin.y, rect.size.width, rect.size.height)
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect { NSRect::new(NSPoint::new(x, y), NSSize::new(w, h)) }

fn screens() -> Vec<Retained<NSScreen>> {
    let mtm = MainThreadMarker::new().unwrap();
    NSScreen::screens(mtm).to_vec()
}

fn win_desc(name: &str, w: &NSWindow) -> String {
    let level: isize = unsafe { msg_send![w, level] };
    let cb: usize = unsafe { msg_send![w, collectionBehavior] };
    let on_space: bool = unsafe { msg_send![w, isOnActiveSpace] };
    let occl: usize = unsafe { msg_send![w, occlusionState] };
    let scr = w.screen().map(|s| r(s.frame())).unwrap_or("-".into());
    let cls = w.class().name().to_string_lossy().into_owned();
    format!(
        "{name}: id={} class={cls} frame={} level={level} cb=0x{cb:x} key={} main={} visible={} onActiveSpace={on_space} occlusionVisible={} screen={scr} scale={}",
        w.windowNumber(), r(w.frame()), w.isKeyWindow(), w.isMainWindow(), w.isVisible(), occl & 2 != 0, w.backingScaleFactor()
    )
}

fn frontmost() -> String {
    let ws = NSWorkspace::sharedWorkspace();
    ws.frontmostApplication()
        .map(|a| format!("{} pid={}", a.localizedName().map(|n| n.to_string()).unwrap_or_default(), a.processIdentifier()))
        .unwrap_or("-".into())
}

fn status() -> String {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let mut out = vec![format!(
        "pid={} bundle={:?} NSApp.isActive={} frontmost=[{}] AXIsProcessTrusted={}",
        std::process::id(),
        objc2_foundation::NSBundle::mainBundle().bundleIdentifier().map(|s| s.to_string()),
        app.isActive(),
        frontmost(),
        unsafe { AXIsProcessTrusted() }
    )];
    for (i, s) in screens().iter().enumerate() {
        out.push(format!("screen[{i}] frame={} visibleFrame={} scale={}", r(s.frame()), r(s.visibleFrame()), s.backingScaleFactor()));
    }
    MAIN.with(|m| if let Some(w) = &*m.borrow() { out.push(win_desc("main", w)) });
    SAT.with(|m| if let Some((_, w)) = &*m.borrow() { out.push(win_desc("satellite", w)) });
    EXTRA.with(|e| for (i, (_, w)) in e.borrow().iter().enumerate() { out.push(win_desc(&format!("extra[{i}]"), w)) });
    out.join("\n")
}

/// Front-to-back on-screen windows (owner + layer + bounds only, no titles).
fn zorder(n: usize) -> String {
    unsafe {
        let arr = CGWindowListCopyWindowInfo(1 | (1 << 4), 0) as *mut NSArray<AnyObject>;
        let arr: Retained<NSArray<AnyObject>> = Retained::from_raw(arr).unwrap();
        let mut out = vec![];
        let mut i = 0;
        for d in arr.iter() {
            let get = |k: &str| -> Option<Retained<AnyObject>> { msg_send![&*d, objectForKey: &*NSString::from_str(k)] };
            let layer: isize = get("kCGWindowLayer").map(|n| msg_send![&*n, integerValue]).unwrap_or(0);
            let num: isize = get("kCGWindowNumber").map(|n| msg_send![&*n, integerValue]).unwrap_or(0);
            let pid: isize = get("kCGWindowOwnerPID").map(|n| msg_send![&*n, integerValue]).unwrap_or(0);
            let owner: String = get("kCGWindowOwnerName").map(|n| { let s: Retained<NSString> = msg_send![&*n, description]; s.to_string() }).unwrap_or_default();
            if layer >= 24 && owner != "spike-win" { continue; } // skip menubar/dock/system overlays
            out.push(format!("  #{i} id={num} pid={pid} owner={owner} layer={layer}"));
            i += 1;
            if i >= n { break; }
        }
        out.join("\n")
    }
}

fn target(name: &str) -> Option<Retained<NSWindow>> {
    match name {
        "sat" | "satellite" => SAT.with(|s| s.borrow().as_ref().map(|(_, w)| w.clone())),
        "extra" => EXTRA.with(|e| e.borrow().last().map(|(_, w)| w.clone())),
        _ => MAIN.with(|m| m.borrow().clone()),
    }
}

fn set_frame(w: &NSWindow, f: NSRect, animate: bool) -> String {
    let before = w.frame();
    w.setFrame_display_animate(f, true, animate);
    let after = w.frame();
    format!("requested={} before={} actual={} match={}", r(f), r(before), r(after), (after.origin.x - f.origin.x).abs() < 1.0 && (after.origin.y - f.origin.y).abs() < 1.0 && (after.size.width - f.size.width).abs() < 1.0 && (after.size.height - f.size.height).abs() < 1.0)
}

fn snap(w: &NSWindow, how: &str) -> String {
    let scr = w.screen().or_else(|| screens().into_iter().next()).unwrap();
    let v = scr.visibleFrame();
    let (x, y, wd, h) = (v.origin.x, v.origin.y, v.size.width, v.size.height);
    let f = match how {
        "left" => rect(x, y, wd / 2.0, h),
        "right" => rect(x + wd / 2.0, y, wd / 2.0, h),
        "left-third" => rect(x, y, wd / 3.0, h),
        "center-third" => rect(x + wd / 3.0, y, wd / 3.0, h),
        "right-third" => rect(x + 2.0 * wd / 3.0, y, wd / 3.0, h),
        "top" => rect(x, y + h / 2.0, wd, h / 2.0),
        "max" => v,
        "center" => { let f = w.frame(); rect(x + (wd - f.size.width) / 2.0, y + (h - f.size.height) / 2.0, f.size.width, f.size.height) }
        _ => return format!("unknown snap {how}"),
    };
    format!("snap {how} visibleFrame={} {}", r(v), set_frame(w, f, false))
}

fn handle(cmd: &str, cx: &mut App) -> String {
    let mtm = MainThreadMarker::new().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    // optional trailing "@sat"/"@extra" picks a target window
    let (parts, tname) = match parts.last() { Some(p) if p.starts_with('@') => (parts[..parts.len() - 1].to_vec(), &p[1..]), _ => (parts, "main") };
    let w = target(tname);
    let Some(w) = w else { return format!("no window {tname}") };
    match parts.as_slice() {
        ["status"] => status(),
        ["z"] | ["zorder"] => zorder(12),
        ["z", n] => zorder(n.parse().unwrap_or(12)),
        ["float"] => { w.setLevel(3); format!("level -> {}", w.level()) } // NSFloatingWindowLevel
        ["float", lvl] => {
            let l: isize = match *lvl { "modal" => 8, "status" => 25, "popup" => 101, "screensaver" => 1000, n => n.parse().unwrap_or(3) };
            w.setLevel(l); format!("level -> {}", w.level())
        }
        ["unfloat"] => { w.setLevel(0); format!("level -> {}", w.level()) }
        ["nonactivating", v] => {
            let m: usize = unsafe { msg_send![&*w, styleMask] };
            let m2 = if *v == "on" { m | (1 << 7) } else { m & !(1 << 7) };
            let _: () = unsafe { msg_send![&*w, setStyleMask: m2] };
            let back: usize = unsafe { msg_send![&*w, styleMask] };
            format!("styleMask 0x{m:x} -> 0x{back:x}")
        }
        ["hides-on-deactivate", v] => { w.setHidesOnDeactivate(*v == "on"); format!("hidesOnDeactivate={}", w.hidesOnDeactivate()) }
        ["snap", how] => snap(&w, how),
        ["frame", x, y, wd, h] => set_frame(&w, rect(x.parse().unwrap(), y.parse().unwrap(), wd.parse().unwrap(), h.parse().unwrap()), false),
        ["frame-anim", x, y, wd, h] => set_frame(&w, rect(x.parse().unwrap(), y.parse().unwrap(), wd.parse().unwrap(), h.parse().unwrap()), true),
        ["move-display", n] => {
            let all = screens();
            let Some(s) = all.get(n.parse::<usize>().unwrap_or(99)) else { return format!("no display {n}; NSScreen.screens count={}", all.len()) };
            let v = s.visibleFrame(); let f = w.frame();
            set_frame(&w, rect(v.origin.x + (v.size.width - f.size.width) / 2.0, v.origin.y + (v.size.height - f.size.height) / 2.0, f.size.width.min(v.size.width), f.size.height.min(v.size.height)), false)
        }
        // ---- activation variants ----
        ["front"] => { app.activate(); w.makeKeyAndOrderFront(None); "NSApp.activate() + makeKeyAndOrderFront (check status in ~300ms)".into() }
        ["front-legacy"] => { #[allow(deprecated)] app.activateIgnoringOtherApps(true); w.makeKeyAndOrderFront(None); "activateIgnoringOtherApps(YES) + makeKeyAndOrderFront".into() }
        ["front-running"] => {
            let ok = NSRunningApplication::currentApplication().activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::ActivateAllWindows);
            w.makeKeyAndOrderFront(None);
            format!("NSRunningApplication.activateWithOptions(AllWindows) -> {ok}")
        }
        ["front-running-ignoring"] => {
            #[allow(deprecated)]
            let ok = NSRunningApplication::currentApplication().activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps);
            w.makeKeyAndOrderFront(None);
            format!("NSRunningApplication.activateWithOptions(IgnoringOtherApps) -> {ok}")
        }
        ["front-ax"] => {
            // activate ourselves through the AX API (AXFrontmost on our own app element)
            let app = unsafe { AXUIElementCreateApplication(std::process::id() as i32) };
            let e = ax_set_bool(app, "AXFrontmost", true);
            unsafe { CFRelease(app) };
            w.makeKeyAndOrderFront(None);
            format!("AXFrontmost(self)=true -> {e}")
        }
        ["order-front"] => { w.orderFrontRegardless(); "orderFrontRegardless (no activation)".into() }
        ["order-front-key"] => { w.makeKeyAndOrderFront(None); "makeKeyAndOrderFront only".into() }
        ["gpui-activate"] => { cx.activate(true); "gpui cx.activate(true)".into() }
        ["hide"] => { w.orderOut(None); "orderOut".into() }
        ["miniaturize"] => { w.miniaturize(None); "miniaturize".into() }
        ["deminiaturize"] => { w.deminiaturize(None); "deminiaturize".into() }
        ["spaces", mode] => {
            let cb: usize = match *mode { "all" => 1, "default" => 0, "move" => 2, "aux" => 1 << 8, "all+aux" => 1 | (1 << 8), "all+aux+stationary" => 1 | (1 << 8) | (1 << 4), "all-apps" => 1 << 18, m => usize::from_str_radix(m.trim_start_matches("0x"), 16).unwrap_or(0) };
            let _: () = unsafe { msg_send![&*w, setCollectionBehavior: cb] };
            let back: usize = unsafe { msg_send![&*w, collectionBehavior] };
            format!("collectionBehavior requested=0x{cb:x} actual=0x{back:x}")
        }
        // ---- GPUI windows ----
        ["open"] | ["open", _] => {
            let floating = parts.get(1) == Some(&"floating");
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(120.), px(120.)), size(px(420.), px(240.))))),
                kind: if floating { WindowKind::Floating } else { WindowKind::Normal },
                ..Default::default()
            };
            let mut nsw = None;
            let h = cx.open_window(opts, |window, cx| { nsw = Some(ns_window(window)); cx.new(|_| Label("extra window".into())) }).unwrap();
            let nsw = nsw.unwrap();
            let d = win_desc("extra", &nsw);
            EXTRA.with(|e| e.borrow_mut().push((h.into(), nsw)));
            format!("opened {d}")
        }
        ["close"] => {
            let Some((h, _)) = EXTRA.with(|e| e.borrow_mut().pop()) else { return "no extra window".into() };
            let _ = h.update(cx, |_, window, _| window.remove_window());
            "closed extra window".into()
        }
        ["sat", "show"] | ["sat", "show", _] => {
            if SAT.with(|s| s.borrow().is_some()) { return "already".into() }
            let kind = match parts.get(2) { Some(&"floating") => WindowKind::Floating, _ => WindowKind::PopUp };
            let was_active = app.isActive();
            let front_before = frontmost();
            let scr = screens().into_iter().next().unwrap().visibleFrame();
            // GPUI bounds are top-left origin within the display
            let opts = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px((scr.size.width - 300.) as f32), px(40.)), size(px(280.), px(90.))))),
                kind, focus: false, show: true, titlebar: None, is_movable: true,
                ..Default::default()
            };
            let mut nsw = None;
            let h = cx.open_window(opts, |window, cx| { nsw = Some(ns_window(window)); cx.new(|_| Label("satellite: 3 agents running".into())) }).unwrap();
            let nsw = nsw.unwrap();
            let d = win_desc("satellite", &nsw);
            SAT.with(|s| *s.borrow_mut() = Some((h.into(), nsw)));
            format!("{d}\nwasActive={was_active} isActiveNow={} frontBefore=[{front_before}] frontNow=[{}]", app.isActive(), frontmost())
        }
        ["sat", "hide"] => {
            let Some((h, _)) = SAT.with(|s| s.borrow_mut().take()) else { return "no sat".into() };
            let _ = h.update(cx, |_, window, _| window.remove_window());
            "satellite closed".into()
        }
        // ---- Accessibility on other apps ----
        ["ax", rest @ ..] => ax_cmd(rest),
        _ => format!("unknown: {cmd}"),
    }
}

// ---------------- Accessibility FFI ----------------
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementCopyAttributeValue(el: *mut c_void, attr: *const c_void, out: *mut *mut c_void) -> i32;
    fn AXUIElementSetAttributeValue(el: *mut c_void, attr: *const c_void, v: *const c_void) -> i32;
    fn AXUIElementPerformAction(el: *mut c_void, action: *const c_void) -> i32;
    fn AXValueCreate(t: u32, v: *const c_void) -> *mut c_void;
    fn AXValueGetValue(v: *const c_void, t: u32, out: *mut c_void) -> bool;
    fn AXObserverCreate(pid: i32, cb: extern "C" fn(*mut c_void, *mut c_void, *const c_void, *mut c_void), out: *mut *mut c_void) -> i32;
    fn AXObserverAddNotification(o: *mut c_void, el: *mut c_void, n: *const c_void, refcon: *mut c_void) -> i32;
    fn AXObserverGetRunLoopSource(o: *mut c_void) -> *mut c_void;
    fn CGWindowListCopyWindowInfo(opt: u32, rel: u32) -> *mut c_void;
    fn _AXUIElementGetWindow(el: *mut c_void, out: *mut u32) -> i32; // private SPI, read-only
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(p: *const c_void);
    fn CFRetain(p: *const c_void) -> *const c_void;
    fn CFRunLoopGetMain() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
    fn CFRunLoopRemoveSource(rl: *mut c_void, src: *mut c_void, mode: *const c_void);
    static kCFRunLoopDefaultMode: *const c_void;
}

fn cfs(s: &str) -> Retained<NSString> { NSString::from_str(s) }
fn p(s: &NSString) -> *const c_void { s as *const NSString as *const c_void }

fn ax_get(el: *mut c_void, attr: &str) -> Result<*mut c_void, i32> {
    let mut out = std::ptr::null_mut();
    let e = unsafe { AXUIElementCopyAttributeValue(el, p(&cfs(attr)), &mut out) };
    if e == 0 { Ok(out) } else { Err(e) }
}
fn ax_point(el: *mut c_void) -> Option<(f64, f64)> {
    let v = ax_get(el, "AXPosition").ok()?;
    let mut pt = [0f64; 2];
    unsafe { AXValueGetValue(v, 1, pt.as_mut_ptr() as _); CFRelease(v) };
    Some((pt[0], pt[1]))
}
fn ax_size(el: *mut c_void) -> Option<(f64, f64)> {
    let v = ax_get(el, "AXSize").ok()?;
    let mut s = [0f64; 2];
    unsafe { AXValueGetValue(v, 2, s.as_mut_ptr() as _); CFRelease(v) };
    Some((s[0], s[1]))
}
fn ax_bool(el: *mut c_void, attr: &str) -> String {
    match ax_get(el, attr) { Ok(v) => { let b: bool = unsafe { msg_send![&*(v as *mut AnyObject), boolValue] }; unsafe { CFRelease(v) }; b.to_string() } Err(e) => format!("err{e}") }
}
fn ax_str(el: *mut c_void, attr: &str) -> String {
    match ax_get(el, attr) { Ok(v) => { let s: Retained<NSString> = unsafe { msg_send![&*(v as *mut AnyObject), description] }; unsafe { CFRelease(v) }; s.to_string() } Err(e) => format!("err{e}") }
}
fn ax_set_bool(el: *mut c_void, attr: &str, b: bool) -> i32 {
    let n = NSNumber::new_bool(b);
    unsafe { AXUIElementSetAttributeValue(el, p(&cfs(attr)), &*n as *const NSNumber as _) }
}
fn ax_windows(pid: i32) -> (*mut c_void, Vec<*mut c_void>) {
    let app = unsafe { AXUIElementCreateApplication(pid) };
    let mut v = vec![];
    if let Ok(arr) = ax_get(app, "AXWindows") {
        let a: &NSArray<AnyObject> = unsafe { &*(arr as *const NSArray<AnyObject>) };
        for w in a.iter() { v.push(unsafe { CFRetain(&*w as *const AnyObject as _) } as *mut c_void); }
        unsafe { CFRelease(arr) };
    }
    (app, v)
}
fn ax_win_desc(i: usize, w: *mut c_void) -> String {
    let mut wid = 0u32;
    let e = unsafe { _AXUIElementGetWindow(w, &mut wid) };
    format!("  win[{i}] cgid={} role={} subrole={} pos={:?} size={:?} minimized={} main={} focused={} fullscreen={}",
        if e == 0 { wid.to_string() } else { "?".into() }, ax_str(w, "AXRole"), ax_str(w, "AXSubrole"), ax_point(w), ax_size(w),
        ax_bool(w, "AXMinimized"), ax_bool(w, "AXMain"), ax_bool(w, "AXFocused"), ax_bool(w, "AXFullScreen"))
}

extern "C" fn pin_cb(_o: *mut c_void, _el: *mut c_void, n: *const c_void, _r: *mut c_void) {
    let name: &NSString = unsafe { &*(n as *const NSString) };
    PIN.with(|pin| if let Some(pin) = &mut *pin.borrow_mut() {
        // Re-raise: AXRaise the window, and make its app frontmost so it sits above other apps.
        let e1 = unsafe { AXUIElementPerformAction(pin.win, p(&cfs("AXRaise"))) };
        let e2 = ax_set_bool(pin.app, "AXFrontmost", true);
        pin.raises += 1;
        eprintln!("pin: {name} -> AXRaise={e1} AXFrontmost={e2} (raises={})", pin.raises);
    });
}

fn ax_cmd(args: &[&str]) -> String {
    let trusted = unsafe { AXIsProcessTrusted() };
    if args.first() == Some(&"trusted") { return format!("AXIsProcessTrusted={trusted}"); }
    if args.first() == Some(&"unpin") {
        return PIN.with(|pin| match pin.borrow_mut().take() {
            Some(pn) => unsafe {
                let src = AXObserverGetRunLoopSource(pn.observer);
                CFRunLoopRemoveSource(CFRunLoopGetMain(), src, kCFRunLoopDefaultMode);
                CFRelease(pn.observer); CFRelease(pn.app); CFRelease(pn.win);
                format!("unpinned after {} raises", pn.raises)
            },
            None => "not pinned".into(),
        });
    }
    let Some(pid) = args.get(1).and_then(|s| s.parse::<i32>().ok()) else { return "usage: ax <list|move|resize|raise|min|unmin|close|fullscreen|pin|unpin|focus-app> <pid> [idx] [args]".into() };
    let idx: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let (app, wins) = ax_windows(pid);
    let res = (|| -> String {
        if args[0] == "list" {
            let mut out = vec![format!("trusted={trusted} pid={pid} frontmost={} windows={}", ax_bool(app, "AXFrontmost"), wins.len())];
            for (i, w) in wins.iter().enumerate() { out.push(ax_win_desc(i, *w)); }
            return out.join("\n");
        }
        if args[0] == "focus-app" { return format!("AXFrontmost=true -> {}", ax_set_bool(app, "AXFrontmost", true)); }
        let Some(&w) = wins.get(idx) else { return format!("no window idx {idx} (count {})", wins.len()) };
        let num = |i: usize| -> f64 { args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0) };
        let before = ax_win_desc(idx, w);
        let e = match args[0] {
            "move" => { let pt = [num(3), num(4)]; unsafe { let v = AXValueCreate(1, pt.as_ptr() as _); let e = AXUIElementSetAttributeValue(w, p(&cfs("AXPosition")), v); CFRelease(v); e } }
            "resize" => { let s = [num(3), num(4)]; unsafe { let v = AXValueCreate(2, s.as_ptr() as _); let e = AXUIElementSetAttributeValue(w, p(&cfs("AXSize")), v); CFRelease(v); e } }
            "frame" => unsafe {
                // set size, position, size (common trick to survive screen-edge clamping)
                let s = [num(5), num(6)]; let pt = [num(3), num(4)];
                let vs = AXValueCreate(2, s.as_ptr() as _); let vp = AXValueCreate(1, pt.as_ptr() as _);
                let a = AXUIElementSetAttributeValue(w, p(&cfs("AXSize")), vs);
                let b = AXUIElementSetAttributeValue(w, p(&cfs("AXPosition")), vp);
                let c = AXUIElementSetAttributeValue(w, p(&cfs("AXSize")), vs);
                CFRelease(vs); CFRelease(vp); a | b | c
            },
            "raise" => unsafe { AXUIElementPerformAction(w, p(&cfs("AXRaise"))) },
            "min" => ax_set_bool(w, "AXMinimized", true),
            "unmin" => ax_set_bool(w, "AXMinimized", false),
            "fullscreen" => ax_set_bool(w, "AXFullScreen", args.get(3) != Some(&"off")),
            "close" => match ax_get(w, "AXCloseButton") {
                Ok(b) => unsafe { let e = AXUIElementPerformAction(b, p(&cfs("AXPress"))); CFRelease(b); e },
                Err(e) => e,
            },
            "pin" => unsafe {
                let mut obs = std::ptr::null_mut();
                let e = AXObserverCreate(pid, pin_cb, &mut obs);
                if e != 0 { return format!("AXObserverCreate err {e}"); }
                let mut errs = vec![];
                for n in ["AXApplicationDeactivated", "AXFocusedWindowChanged", "AXMainWindowChanged"] {
                    errs.push((n, AXObserverAddNotification(obs, if n == "AXFocusedWindowChanged" || n == "AXMainWindowChanged" { app } else { app }, p(&cfs(n)), std::ptr::null_mut())));
                }
                CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(obs), kCFRunLoopDefaultMode);
                CFRetain(app); CFRetain(w);
                PIN.with(|pin| *pin.borrow_mut() = Some(Pin { observer: obs, app, win: w, raises: 0 }));
                return format!("pinned; addNotification results {errs:?}");
            },
            other => return format!("unknown ax {other}"),
        };
        std::thread::sleep(Duration::from_millis(250));
        format!("ax {} err={e}\n before {}\n after  {}", args[0], before.trim(), ax_win_desc(idx, w).trim())
    })();
    unsafe { for w in wins { CFRelease(w) }; CFRelease(app) };
    res
}

// ---------------- main ----------------
fn client(args: &[String]) {
    let mut s = UnixStream::connect(sock_path()).expect("connect (is `spike-win app` running?)");
    writeln!(s, "{}", args.join(" ")).unwrap();
    let mut out = String::new();
    std::io::Read::read_to_string(&mut s, &mut out).unwrap();
    println!("{out}");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|a| a != "app").unwrap_or(false) { return client(&args); }

    let (tx, rx) = mpsc::channel::<Req>();
    let path = sock_path();
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind");
    eprintln!("listening on {path}");
    std::thread::spawn(move || for s in listener.incoming().flatten() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut line = String::new();
            if BufReader::new(&s).read_line(&mut line).is_err() { return; }
            let (rtx, rrx) = mpsc::channel();
            let _ = tx.send((line.trim().to_string(), rtx));
            let reply = rrx.recv_timeout(Duration::from_secs(10)).unwrap_or("timeout".into());
            let mut s = s; let _ = s.write_all(reply.as_bytes());
        });
    });

    gpui_kit::application().run(move |cx| {
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(200.), px(200.)), size(px(640.), px(400.))))),
            ..Default::default()
        };
        cx.open_window(opts, |window, cx| {
            MAIN.with(|m| *m.borrow_mut() = Some(ns_window(window)));
            cx.new(|_| Label(format!("midna window spike  pid={}", std::process::id())))
        }).unwrap();
        cx.activate(true);
        cx.spawn(async move |cx| loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            while let Ok((cmd, reply)) = rx.try_recv() {
                let t = std::time::Instant::now();
                let out = cx.update(|cx| handle(&cmd, cx));
                eprintln!("> {cmd} ({:?})\n{out}", t.elapsed());
                let _ = reply.send(out);
            }
        }).detach();
    });
}
