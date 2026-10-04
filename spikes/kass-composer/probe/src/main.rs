//! Mirrors Kass's text_insert.rs AX logic against a target pid.
//! usage: kass-probe <pid> <cmd> [args]
//!   dump | insert <text> | select <loc> <len> | type <text> | key <keycode> | tree
#![allow(non_upper_case_globals)]
use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_foundation_sys::base::{CFGetTypeID, CFIndex, CFRange, CFRelease};
use std::ffi::c_void;
use std::ptr;

type AXUIElementRef = *const c_void;
type AXError = i32;
const kAXValueTypeCFRange: u32 = 4;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementGetPid(e: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXUIElementGetTypeID() -> usize;
    fn AXUIElementSetMessagingTimeout(e: AXUIElementRef, t: f32) -> AXError;
    fn AXUIElementCopyAttributeValue(e: AXUIElementRef, a: CFStringRef, out: *mut CFTypeRef) -> AXError;
    fn AXUIElementCopyAttributeNames(e: AXUIElementRef, out: *mut CFTypeRef) -> AXError;
    fn AXUIElementCopyParameterizedAttributeNames(e: AXUIElementRef, out: *mut CFTypeRef) -> AXError;
    fn AXUIElementIsAttributeSettable(e: AXUIElementRef, a: CFStringRef, out: *mut u8) -> AXError;
    fn AXUIElementSetAttributeValue(e: AXUIElementRef, a: CFStringRef, v: CFTypeRef) -> AXError;
    fn AXUIElementCopyParameterizedAttributeValue(e: AXUIElementRef, a: CFStringRef, p: CFTypeRef, out: *mut CFTypeRef) -> AXError;
    fn AXValueCreate(t: u32, v: *const c_void) -> CFTypeRef;
    fn AXValueGetTypeID() -> usize;
    fn AXValueGetValue(v: CFTypeRef, t: u32, out: *mut c_void) -> bool;
}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateKeyboardEvent(src: *const c_void, key: u16, down: bool) -> CFTypeRef;
    fn CGEventKeyboardSetUnicodeString(e: CFTypeRef, len: usize, s: *const u16);
    fn CGEventPostToPid(pid: i32, e: CFTypeRef);
}

struct El(AXUIElementRef);
impl Drop for El {
    fn drop(&mut self) { unsafe { CFRelease(self.0) } }
}

fn attr(e: AXUIElementRef, name: &str) -> Result<CFType, AXError> {
    let k = CFString::new(name);
    let mut out: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(e, k.as_concrete_TypeRef(), &mut out) };
    if err != 0 || out.is_null() { return Err(err); }
    Ok(unsafe { CFType::wrap_under_create_rule(out) })
}
fn settable(e: AXUIElementRef, name: &str) -> String {
    let k = CFString::new(name);
    let mut s = 0u8;
    let err = unsafe { AXUIElementIsAttributeSettable(e, k.as_concrete_TypeRef(), &mut s) };
    if err != 0 { format!("err {err}") } else { (s != 0).to_string() }
}
fn as_range(v: &CFType) -> Option<(i64, i64)> {
    if unsafe { CFGetTypeID(v.as_CFTypeRef()) } != unsafe { AXValueGetTypeID() } { return None; }
    let mut r = CFRange { location: 0, length: 0 };
    let ok = unsafe { AXValueGetValue(v.as_CFTypeRef(), kAXValueTypeCFRange, &mut r as *mut _ as *mut c_void) };
    ok.then_some((r.location as i64, r.length as i64))
}
fn show(v: &Result<CFType, AXError>) -> String {
    match v {
        Err(e) => format!("<AXError {e}>"),
        Ok(v) => {
            if let Some(s) = v.downcast::<CFString>() { return format!("{:?}", s.to_string()); }
            if let Some(n) = v.downcast::<CFNumber>() { return format!("{:?}", n.to_i64()); }
            if let Some(b) = v.downcast::<CFBoolean>() { return format!("{}", bool::from(b)); }
            if let Some(r) = as_range(v) { return format!("range{r:?}"); }
            if let Some(a) = v.downcast::<CFArray>() { return format!("<array len {}>", a.len()); }
            if unsafe { CFGetTypeID(v.as_CFTypeRef()) } == unsafe { AXUIElementGetTypeID() } { return "<AXUIElement>".into(); }
            "<other>".into()
        }
    }
}
fn s_attr(e: AXUIElementRef, n: &str) -> Option<String> {
    attr(e, n).ok().and_then(|v| v.downcast::<CFString>().map(|s| s.to_string()))
}
fn range_val(loc: i64, len: i64) -> CFType {
    let r = CFRange { location: loc as CFIndex, length: len as CFIndex };
    unsafe { CFType::wrap_under_create_rule(AXValueCreate(kAXValueTypeCFRange, &r as *const _ as *const c_void)) }
}
fn string_for_range(e: AXUIElementRef, loc: i64, len: i64) -> Result<String, AXError> {
    let k = CFString::new("AXStringForRange");
    let p = range_val(loc, len);
    let mut out: CFTypeRef = ptr::null();
    let err = unsafe { AXUIElementCopyParameterizedAttributeValue(e, k.as_concrete_TypeRef(), p.as_CFTypeRef(), &mut out) };
    if err != 0 || out.is_null() { return Err(err); }
    let v = unsafe { CFType::wrap_under_create_rule(out) };
    v.downcast::<CFString>().map(|s| s.to_string()).ok_or(-1)
}
fn observe(e: AXUIElementRef) -> (Option<(i64, i64)>, Option<i64>) {
    let sel = attr(e, "AXSelectedTextRange").ok().as_ref().and_then(as_range);
    let n = attr(e, "AXNumberOfCharacters").ok().and_then(|v| v.downcast::<CFNumber>().and_then(|n| n.to_i64()))
        .or_else(|| s_attr(e, "AXValue").map(|s| s.encode_utf16().count() as i64));
    (sel, n)
}
fn names(e: AXUIElementRef, param: bool) -> Vec<String> {
    let mut out: CFTypeRef = ptr::null();
    let err = unsafe { if param { AXUIElementCopyParameterizedAttributeNames(e, &mut out) } else { AXUIElementCopyAttributeNames(e, &mut out) } };
    if err != 0 || out.is_null() { return vec![format!("<err {err}>")]; }
    let a: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(out as _) };
    a.iter().map(|s| s.to_string()).collect()
}

fn focused(pid: i32) -> Option<El> {
    let app = El(unsafe { AXUIElementCreateApplication(pid) });
    unsafe { AXUIElementSetMessagingTimeout(app.0, 1.0) };
    let v = match attr(app.0, "AXFocusedUIElement") { Ok(v) => v, Err(e) => { println!("AXFocusedUIElement: AXError {e}"); return None; } };
    let r = v.as_CFTypeRef();
    if unsafe { CFGetTypeID(r) } != unsafe { AXUIElementGetTypeID() } { return None; }
    unsafe { core_foundation_sys::base::CFRetain(r) };
    Some(El(r))
}

const TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox"];

fn dump(e: AXUIElementRef) {
    for a in ["AXRole", "AXSubrole", "AXRoleDescription", "AXIdentifier", "AXDescription", "AXValue", "AXSelectedText", "AXSelectedTextRange", "AXNumberOfCharacters", "AXInsertionPointLineNumber", "AXFocused", "AXEnabled"] {
        println!("  {a:28} = {}", show(&attr(e, a)));
    }
    for a in ["AXSelectedText", "AXValue", "AXSelectedTextRange"] {
        println!("  settable({a}) = {}", settable(e, a));
    }
    println!("  attrs: {:?}", names(e, false));
    println!("  param attrs: {:?}", names(e, true));
    if let Ok(p) = attr(e, "AXParent") {
        let pe = p.as_CFTypeRef();
        println!("  parent role = {:?}", s_attr(pe, "AXRole"));
    }
}

fn kass_verdict(e: AXUIElementRef) {
    let role = s_attr(e, "AXRole");
    let sub = s_attr(e, "AXSubrole");
    let (sel, n) = observe(e);
    let set = settable(e, "AXSelectedText") == "true";
    let secure = [&role, &sub].iter().any(|r| r.as_deref() == Some("AXSecureTextField"));
    let v = if secure { "Clipboard(SecureField)" }
        else if !role.as_deref().is_some_and(|r| TEXT_ROLES.contains(&r)) { "Clipboard(NotATextRole)" }
        else if !set { "Clipboard(NotSettable)" }
        else if sel.is_none() || n.is_none() { "Clipboard(Unverifiable)" }
        else { "Accessibility" };
    println!("KASS choose_strategy (ignoring bundle list) => {v}  [role={role:?} settable={set} sel={sel:?} count={n:?}]");
    if let (Some((loc, len)), Some(count)) = (sel, n) {
        let start = (loc - 16).max(0);
        let end = loc + len;
        println!("  context before [{start}..{loc}] = {:?}", string_for_range(e, start, loc - start));
        println!("  context after  [{end}..{}] = {:?}", (end + 4).min(count), string_for_range(e, end, (end + 4).min(count) - end));
    }
}

fn insert(e: AXUIElementRef, text: &str) {
    let (sel0, n0) = observe(e);
    println!("before: sel={sel0:?} count={n0:?} value={:?}", s_attr(e, "AXValue"));
    let k = CFString::new("AXSelectedText");
    let v = CFString::new(text);
    let err = unsafe { AXUIElementSetAttributeValue(e, k.as_concrete_TypeRef(), v.as_CFTypeRef()) };
    println!("set AXSelectedText -> AXError {err}");
    let ins = text.encode_utf16().count() as i64;
    for i in 0..4 {
        std::thread::sleep(std::time::Duration::from_millis(15));
        let (sel, n) = observe(e);
        if let (Some((l0, len0)), Some(c0)) = (sel0, n0) {
            let exp_sel = (l0 + ins, 0);
            let exp_n = c0 - len0 + ins;
            let text_ok = string_for_range(e, l0, ins);
            let inserted = sel == Some(exp_sel) && n == Some(exp_n);
            println!("poll {i}: sel={sel:?} (exp {exp_sel:?}) count={n:?} (exp {exp_n}) text@range={text_ok:?} => {}",
                if inserted { if text_ok.as_deref() == Ok(text) { "Inserted{exact:true}" } else { "Inserted{exact:false}" } }
                else if sel == sel0 && n == n0 { "Unchanged" } else { "ChangedUnexpectedly/Unknown" });
            if inserted { break; }
        }
    }
    println!("after value={:?}", s_attr(e, "AXValue"));
}

fn tree(e: AXUIElementRef, depth: usize) {
    if depth > 8 { return; }
    println!("{}{} sub={:?} title={:?} val={:?}", "  ".repeat(depth), s_attr(e, "AXRole").unwrap_or_default(), s_attr(e, "AXSubrole"), s_attr(e, "AXTitle"), s_attr(e, "AXValue").map(|s| s.chars().take(30).collect::<String>()));
    if std::env::var("DUMP_TEXT").is_ok() && matches!(s_attr(e, "AXRole").as_deref(), Some("AXTextField" | "AXTextArea")) { dump(e); kass_verdict(e); if let Ok(t) = std::env::var("INSERT") { insert(e, &t); } }
    if let Ok(c) = attr(e, "AXChildren") {
        if let Some(a) = c.downcast::<CFArray>() {
            for i in 0..a.len() {
                let child = *a.get(i).unwrap() as AXUIElementRef;
                tree(child, depth + 1);
            }
        }
    }
}

fn post_unicode(pid: i32, text: &str) {
    for ch in text.encode_utf16() {
        for down in [true, false] {
            let ev = unsafe { CGEventCreateKeyboardEvent(ptr::null(), 0, down) };
            unsafe { CGEventKeyboardSetUnicodeString(ev, 1, &ch) };
            unsafe { CGEventPostToPid(pid, ev); CFRelease(ev) };
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
    }
}
fn post_key(pid: i32, code: u16) {
    for down in [true, false] {
        let ev = unsafe { CGEventCreateKeyboardEvent(ptr::null(), code, down) };
        unsafe { CGEventPostToPid(pid, ev); CFRelease(ev) };
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    println!("AXIsProcessTrusted = {}", unsafe { AXIsProcessTrusted() });
    let pid: i32 = args[1].parse().unwrap();
    let cmd = args.get(2).map(String::as_str).unwrap_or("dump");
    match cmd {
        "type" => return post_unicode(pid, &args[3]),
        "key" => return post_key(pid, args[3].parse().unwrap()),
        "kass-sim" => {
            let n: usize = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(1);
            let no_insert = std::env::var("NO_INSERT").is_ok();
            for i in 0..n { kass_sim(pid, i, no_insert); }
            return;
        }
        "tree" => {
            let app = El(unsafe { AXUIElementCreateApplication(pid) });
            return tree(app.0, 0);
        }
        _ => {}
    }
    let Some(f) = focused(pid) else { println!("no focused element"); return };
    match cmd {
        "dump" => { println!("focused element:"); dump(f.0); kass_verdict(f.0); }
        "insert" => insert(f.0, &args[3]),
        "select" => {
            let r = range_val(args[3].parse().unwrap(), args[4].parse().unwrap());
            let k = CFString::new("AXSelectedTextRange");
            let err = unsafe { AXUIElementSetAttributeValue(f.0, k.as_concrete_TypeRef(), r.as_CFTypeRef()) };
            println!("set AXSelectedTextRange -> AXError {err}");
            let (sel, _) = observe(f.0);
            println!("Command Mode read_selection: sel={sel:?} AXStringForRange={:?} AXSelectedText={}",
                sel.map(|(l, n)| string_for_range(f.0, l, n)), show(&attr(f.0, "AXSelectedText")));
        }
        _ => println!("unknown cmd"),
    }
}

// ---------------- handshake simulation ----------------
use core_foundation::dictionary::CFDictionary;
use core_foundation_sys::notification_center::*;
use core_foundation_sys::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use std::sync::Mutex;
use std::time::Instant;

static READY_AT: Mutex<Option<(Instant, i64)>> = Mutex::new(None);

extern "C" fn on_ready(_c: CFNotificationCenterRef, _o: *mut c_void, _n: CFStringRef, _obj: *const c_void, info: core_foundation_sys::dictionary::CFDictionaryRef) {
    let mut pid = -1;
    if !info.is_null() {
        let d: CFDictionary<CFString, CFType> = unsafe { CFDictionary::wrap_under_get_rule(info) };
        if let Some(v) = d.find(CFString::new("pid")) { pid = v.downcast::<CFNumber>().and_then(|n| n.to_i64()).unwrap_or(-1); }
    }
    *READY_AT.lock().unwrap() = Some((Instant::now(), pid));
}

fn ensure_observer() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        CFNotificationCenterAddObserver(CFNotificationCenterGetDistributedCenter(), 1 as *const c_void, on_ready,
            CFString::new("com.mrgnhnt.kass.dictationReady").as_concrete_TypeRef(), ptr::null(),
            CFNotificationSuspensionBehaviorDeliverImmediately);
    });
}
fn post(name: &str, pid: i32, mode: Option<&str>) {
    let mut pairs = vec![(CFString::new("pid"), CFNumber::from(pid).as_CFType())];
    if let Some(m) = mode { pairs.push((CFString::new("mode"), CFString::new(m).as_CFType())); }
    let d = CFDictionary::from_CFType_pairs(&pairs);
    unsafe { CFNotificationCenterPostNotification(CFNotificationCenterGetDistributedCenter(), CFString::new(name).as_concrete_TypeRef(), ptr::null(), d.as_concrete_TypeRef(), 1) };
}
/// Kass capture_focus: system-wide first; on error fall back to the app (Kass's
/// insert path then uses FocusedElement::of_app(pid) anyway).
fn system_focus_or_app(pid: i32) -> Option<(El, i32, Option<String>)> {
    match system_focus() {
        Some(x) => { println!("        capture: system-wide ok"); Some(x) }
        None => { println!("        capture: system-wide FAILED ({:?}), Kass falls back to frontmost app; using app-level focus", { let sw = El(unsafe { AXUIElementCreateSystemWide() }); attr(sw.0, "AXFocusedUIElement").err() }); let e = focused(pid)?; let role = s_attr(e.0, "AXRole"); Some((e, pid, role)) }
    }
}
fn system_focus() -> Option<(El, i32, Option<String>)> {
    let sw = El(unsafe { AXUIElementCreateSystemWide() });
    let v = attr(sw.0, "AXFocusedUIElement").ok()?;
    let r = v.as_CFTypeRef();
    unsafe { core_foundation_sys::base::CFRetain(r) };
    let e = El(r);
    let mut pid = 0;
    unsafe { AXUIElementGetPid(e.0, &mut pid) };
    let role = s_attr(e.0, "AXRole");
    Some((e, pid, role))
}

fn kass_sim(pid: i32, i: usize, no_insert: bool) {
    ensure_observer();
    *READY_AT.lock().unwrap() = None;
    let timeout = std::time::Duration::from_millis(150);
    let t0 = Instant::now();
    post("com.mrgnhnt.kass.dictationWillBegin", pid, Some("dictate"));
    let ready = loop {
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.001, 1) };
        if let Some((t, p)) = *READY_AT.lock().unwrap() { break Some(((t - t0).as_secs_f64() * 1000.0, p)); }
        if t0.elapsed() >= timeout { break None; }
    };
    let waited = t0.elapsed().as_secs_f64() * 1000.0;
    let (fe, fpid, role) = match system_focus_or_app(pid) { Some(x) => x, None => { println!("RUN {i}: ready={ready:?} waited={waited:.1}ms focus=NONE"); return; } };
    let (sel, n) = observe(fe.0);
    let ctx = sel.map(|(loc, len)| {
        let st = (loc - 16).max(0);
        let end = loc + len;
        (string_for_range(fe.0, st, loc - st).ok(), n.and_then(|c| string_for_range(fe.0, end, (end + 4).min(c) - end).ok()))
    });
    let mut ins = String::from("skipped");
    if !no_insert && fpid == pid && role.as_deref() == Some("AXTextArea") && settable(fe.0, "AXSelectedText") == "true" {
        let text = " hello from kass";
        let k = CFString::new("AXSelectedText");
        let err = unsafe { AXUIElementSetAttributeValue(fe.0, k.as_concrete_TypeRef(), CFString::new(text).as_CFTypeRef()) };
        std::thread::sleep(std::time::Duration::from_millis(15));
        let (sel2, n2) = observe(fe.0);
        let (l0, len0) = sel.unwrap();
        let ok = sel2 == Some((l0 + 16, 0)) && n2 == Some(n.unwrap() - len0 + 16) && string_for_range(fe.0, l0, 16).as_deref() == Ok(text);
        ins = format!("err={err} {}", if ok { "Inserted{exact:true}" } else { "MISMATCH" });
    }
    println!("RUN {i:2}: ready={} rtt={} waited={waited:.1}ms focused_pid={fpid} role={role:?} sel={sel:?} n={n:?} ctx={ctx:?} insert={ins}",
        ready.is_some(), ready.map(|r| format!("{:.2}ms", r.0)).unwrap_or("-".into()));
    drop(fe);
    if !no_insert {
        post("com.mrgnhnt.kass.dictationDidEnd", pid, None);
        std::thread::sleep(std::time::Duration::from_millis(30));
        // alternate: Enter (send) / Esc (hide)
        post_key(pid, if i % 2 == 0 { 36 } else { 53 });
        std::thread::sleep(std::time::Duration::from_millis(80));
        let after = system_focus_or_app(pid).map(|(_, p, r)| (p, r));
        println!("        after {}: focus={after:?}", if i % 2 == 0 { "Enter" } else { "Esc" });
    }
}
