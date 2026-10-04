//! Kass dictation handshake (Kass `docs/DICTATION_HANDSHAKE.md` on its main branch, proven
//! in `spikes/kass-composer`).
//!
//! Kass posts distributed notifications around a dictation, to the frontmost app only:
//! - `dictationWillBegin {pid, mode}` at chord-down, before it reads the focused element;
//! - `dictationDidEnd {pid, outcome}` when the take has settled.
//!
//! We reply `dictationReady {pid}` once the composer's `NSTextView` is first responder
//! ([`post_ready`]). Kass waits at most 150 ms for it. We advertise support by posting
//! `handshakeSupported {pid}` at launch and every few seconds, because dev builds aren't
//! bundles. A bundled Midna.app should also set `KassDictationHandshake = true` in Info.plist.
//!
//! **Threads.** The listener has its own thread (`midna-kass`) running its own CFRunLoop
//! (it re-advertises from a run-loop timer there). Distributed notifications, though, are
//! *not* delivered to a background thread's run loop: CFNotificationCenter and the selector
//! form of NSDistributedNotificationCenter both deliver on the **main** run loop whatever
//! thread registered (checked with a Swift probe and with `delivers_off_main` below). The
//! block form with an `NSOperationQueue` is delivered off main, so the observers run on a
//! private serial queue. Either way nothing waits on the GPUI main thread to *notice* a
//! notification; showing the composer must still happen on main, so events go to the UI
//! over a channel.
//!
//! Only notifications whose `pid` is ours are forwarded, but any Kass notification counts as
//! "handshake detected" (shown in Settings).
use core_foundation_sys::base::{CFRelease, CFTypeRef, kCFAllocatorDefault};
use core_foundation_sys::dictionary::{CFDictionaryCreate, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks};
use core_foundation_sys::notification_center::{CFNotificationCenterGetDistributedCenter, CFNotificationCenterPostNotificationWithOptions, kCFNotificationDeliverImmediately};
use core_foundation_sys::number::{CFNumberCreate, kCFNumberSInt64Type};
use core_foundation_sys::runloop::{CFRunLoopAddTimer, CFRunLoopGetCurrent, CFRunLoopRun, CFRunLoopTimerCreate, CFRunLoopTimerRef, kCFRunLoopCommonModes};
use core_foundation_sys::string::{CFStringCreateWithBytes, CFStringRef, kCFStringEncodingUTF8};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNumber, NSOperationQueue, NSString};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub const WILL_BEGIN: &str = "com.mrgnhnt.kass.dictationWillBegin";
pub const READY: &str = "com.mrgnhnt.kass.dictationReady";
pub const DID_END: &str = "com.mrgnhnt.kass.dictationDidEnd";
pub const SUPPORTED: &str = "com.mrgnhnt.kass.handshakeSupported";
/// How often `handshakeSupported` is re-posted, so a Kass started after midna sees it.
const ADVERTISE_EVERY_SECS: f64 = 3.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KassEvent {
    /// Dictation is about to start. `mode` is `dictate` or `command`.
    WillBegin { mode: String },
    /// The take settled. `outcome` is `inserted`, `cancelled` or `failed` (None if absent).
    DidEnd { outcome: Option<String> },
}

/// Turn a notification into an event for this process, if it is one.
pub fn classify(name: &str, pid: Option<i64>, ours: i64, text: Option<String>) -> Option<KassEvent> {
    if pid != Some(ours) {
        return None;
    }
    match name {
        WILL_BEGIN => Some(KassEvent::WillBegin { mode: text.unwrap_or_else(|| "dictate".into()) }),
        DID_END => Some(KassEvent::DidEnd { outcome: text }),
        _ => None,
    }
}

static DETECTED: AtomicBool = AtomicBool::new(false);
static LAST_SEEN: Mutex<Option<Instant>> = Mutex::new(None);
type Sink = Box<dyn Fn(KassEvent) + Send + Sync>;
static SINK: OnceLock<Sink> = OnceLock::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// True once any Kass handshake notification has arrived in this run.
pub fn handshake_detected() -> bool {
    DETECTED.load(Ordering::SeqCst)
}

fn our_pid() -> i64 {
    std::process::id() as i64
}

/// Start the listener (once). `sink` is called off the main thread for every notification
/// meant for us; it must not block (send on a channel).
pub fn start(sink: impl Fn(KassEvent) + Send + Sync + 'static) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = SINK.set(Box::new(sink));
    std::thread::Builder::new()
        .name("midna-kass".into())
        .spawn(|| {
            observe(&[WILL_BEGIN, DID_END], "midna-kass", on_notification);
            unsafe {
                post(SUPPORTED, &[("pid", Val::Int(our_pid()))]);
                let timer = CFRunLoopTimerCreate(
                    kCFAllocatorDefault,
                    core_foundation_sys::date::CFAbsoluteTimeGetCurrent() + ADVERTISE_EVERY_SECS,
                    ADVERTISE_EVERY_SECS,
                    0,
                    0,
                    advertise,
                    std::ptr::null_mut(),
                );
                CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopCommonModes);
                CFRunLoopRun();
            }
        })
        .expect("spawn kass listener");
}

extern "C" fn advertise(_t: CFRunLoopTimerRef, _info: *mut c_void) {
    post(SUPPORTED, &[("pid", Val::Int(our_pid()))]);
}

fn on_notification(name: &str, pid: Option<i64>, text: Option<String>) {
    if name == WILL_BEGIN || name == DID_END {
        DETECTED.store(true, Ordering::SeqCst);
        *LAST_SEEN.lock().unwrap() = Some(Instant::now());
    }
    if crate::dev::var("MIDNA_DEBUG").is_ok() {
        eprintln!("midna-app: kass {name} pid={pid:?} {text:?} main={}", objc2::MainThreadMarker::new().is_some());
    }
    if let Some(ev) = classify(name, pid, our_pid(), text)
        && let Some(sink) = SINK.get()
    {
        sink(ev);
    }
}

/// Reply to `dictationWillBegin`: the composer is first responder. Any thread.
pub fn post_ready() {
    post(READY, &[("pid", Val::Int(our_pid()))]);
}

// ---------------------------------------------------------------- Foundation glue

type Handler = fn(name: &str, pid: Option<i64>, text: Option<String>);

/// Observe `names` on the distributed center; `handler` runs on a private serial
/// `NSOperationQueue` (off the main thread). Observers live for the whole process.
pub(crate) fn observe(names: &[&str], queue_name: &str, handler: Handler) {
    let queue = NSOperationQueue::new();
    queue.setMaxConcurrentOperationCount(1);
    queue.setName(Some(&NSString::from_str(queue_name)));
    let center = NSDistributedNotificationCenter::defaultCenter();
    for name in names {
        let block = block2::RcBlock::new(move |n: NonNull<NSNotification>| {
            let n = unsafe { n.as_ref() };
            let (pid, text) = user_info(n);
            handler(&n.name().to_string(), pid, text);
        });
        let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(&NSString::from_str(name)), None, Some(&queue), &block) };
        std::mem::forget(token);
    }
    std::mem::forget(queue);
}

/// `pid` (NSNumber) and `mode` / `outcome` (strings) from a notification's userInfo.
fn user_info(n: &NSNotification) -> (Option<i64>, Option<String>) {
    let Some(info) = n.userInfo() else {
        return (None, None);
    };
    let get = |k: &str| -> Option<Retained<AnyObject>> { unsafe { objc2::msg_send![&*info, objectForKey: &*NSString::from_str(k)] } };
    let pid = get("pid").and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.as_i64());
    let text = get("mode").or_else(|| get("outcome")).and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string());
    (pid, text)
}

pub(crate) enum Val<'a> {
    Int(i64),
    #[cfg_attr(not(test), allow(dead_code))]
    Text(&'a str),
}

/// Post a distributed notification, delivered immediately even to background apps.
/// CoreFoundation, so it is safe from any thread.
pub(crate) fn post(name: &str, info: &[(&str, Val)]) {
    unsafe {
        let Some(n) = cf_string(name) else { return };
        let mut keys: Vec<CFTypeRef> = vec![];
        let mut vals: Vec<CFTypeRef> = vec![];
        for (k, v) in info {
            let v: CFTypeRef = match v {
                Val::Int(i) => CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt64Type, i as *const i64 as *const c_void) as CFTypeRef,
                Val::Text(s) => match cf_string(s) {
                    Some(s) => s as CFTypeRef,
                    None => continue,
                },
            };
            match cf_string(k) {
                Some(k) => {
                    keys.push(k as CFTypeRef);
                    vals.push(v);
                }
                None => CFRelease(v),
            }
        }
        let d = CFDictionaryCreate(kCFAllocatorDefault, keys.as_ptr(), vals.as_ptr(), keys.len() as isize, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
        CFNotificationCenterPostNotificationWithOptions(CFNotificationCenterGetDistributedCenter(), n, std::ptr::null(), d, kCFNotificationDeliverImmediately);
        for o in keys.into_iter().chain(vals) {
            CFRelease(o);
        }
        if !d.is_null() {
            CFRelease(d as CFTypeRef);
        }
        CFRelease(n as CFTypeRef);
    }
}

unsafe fn cf_string(s: &str) -> Option<CFStringRef> {
    let r = unsafe { CFStringCreateWithBytes(kCFAllocatorDefault, s.as_ptr(), s.len() as isize, kCFStringEncodingUTF8, 0) };
    (!r.is_null()).then_some(r)
}

#[cfg(test)]
mod tests {
    use super::{DID_END, KassEvent, Val, WILL_BEGIN, classify, observe, post};
    use ::core::prelude::v1::test;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    #[test]
    fn classify_filters_by_pid() {
        assert_eq!(classify(WILL_BEGIN, Some(7), 7, Some("command".into())), Some(KassEvent::WillBegin { mode: "command".into() }));
        assert_eq!(classify(WILL_BEGIN, Some(7), 7, None), Some(KassEvent::WillBegin { mode: "dictate".into() }));
        assert_eq!(classify(DID_END, Some(7), 7, Some("inserted".into())), Some(KassEvent::DidEnd { outcome: Some("inserted".into()) }));
        assert_eq!(classify(WILL_BEGIN, Some(8), 7, None), None);
        assert_eq!(classify(WILL_BEGIN, None, 7, None), None);
        assert_eq!(classify("other", Some(7), 7, None), None);
    }

    static GOT: Mutex<Vec<(String, Option<i64>, Option<String>, bool)>> = Mutex::new(Vec::new());

    /// Notifications arrive although no thread here runs the main run loop (the test
    /// harness's main thread is blocked), and not on the main thread. Uses a private name,
    /// so a running Kass never sees it.
    #[test]
    fn delivers_off_main() {
        let name: &'static str = Box::leak(format!("com.mrgnhnt.midna.test.kass.{}", std::process::id()).into_boxed_str());
        observe(&[name], "kass-test", |n, pid, text| {
            GOT.lock().unwrap().push((n.to_string(), pid, text, objc2::MainThreadMarker::new().is_some()));
        });
        let t0 = Instant::now();
        while GOT.lock().unwrap().is_empty() && t0.elapsed() < Duration::from_secs(3) {
            post(name, &[("pid", Val::Int(42)), ("mode", Val::Text("command"))]);
            std::thread::sleep(Duration::from_millis(100));
        }
        let got = GOT.lock().unwrap().first().cloned();
        assert_eq!(got, Some((name.to_string(), Some(42), Some("command".into()), false)));
    }
}
