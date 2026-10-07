//! Native macOS notifications for midnad's `notify.posted` events (`midna_proto::notify`).
//!
//! midnad decides *what* to notify; the app decides only whether you're already looking at it
//! (the selected terminal while midna is frontmost) and shows banners, as each kind's
//! `notify.push.*` / `notify.push_focused.*` says, through `UNUserNotificationCenter`. Each
//! notification's thread identifier is its terminal id, so Notification Center groups them per
//! terminal and a click selects it.
//!
//! `UNUserNotificationCenter` needs a real app bundle (it raises without one), so a dev build
//! run from `target/` posts through `osascript` instead (shown as Script Editor, no click).
//!
//! Authorization is asked for on the first notification, not at launch, so the macOS prompt
//! shows up next to something that explains it.
//!
//! Sounds: `UNNotificationSound` has no volume, so midnad renders each sound at its volume
//! into `~/Library/Sounds` (`Posted::notification_sound`) and the notification names that file.
//! macOS plays it with the banner, so Focus, "Deliver quietly" and midna's sound switch in System
//! Settings all apply. `soundNamed` needs a name it can find there: given a full path it plays
//! the default sound. Only when there's no file, and in dev builds, does midna play
//! `sound_file` itself with NSSound.
//!
//! Images: `UNNotificationAttachment` moves the file it's given into its own store, so each
//! notification attaches a fresh copy of the imported image.
//!
//! `notify.send` extras (`Posted::id`, `open`, `actions`): the request identifier is `id:<id>`,
//! so a later one with the same id replaces it and `withdraw` finds it; the id and URL ride in
//! `userInfo`; buttons come from a `UNNotificationCategory` registered for that set of labels
//! (with a custom dismiss action, so a dismissal is reported too). A click, button or dismissal
//! comes back as `Clicked::response`, which the app reports with `notify.respond`.
use block2::{DynBlock, RcBlock};
use midna_proto::notify::{Posted, Response, ResponseKind};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, define_class, msg_send};
use objc2_app_kit::NSSound;
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError, NSSet, NSString, NSURL, NSUserDefaults};
use objc2_user_notifications::*;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{Mutex, OnceLock};

/// A notification was clicked: select this terminal (empty = none), and show this needs-you
/// item if it was about one. A `notify.send` one may open a URL instead, and reports what the
/// human did (`response`) under its `id`.
pub struct Clicked {
    pub session: String,
    pub needs_you: Option<String>,
    pub id: Option<String>,
    pub open: Option<String>,
    /// None for midna's own notifications (nothing to report).
    pub response: Option<Response>,
}

/// What a click on a notification does, wherever it shows (banner, in-app card, badge,
/// Notifications screen): open its URL, else select its terminal (`reveal`). Then report it.
pub fn on_click(backend: &std::sync::Arc<dyn crate::backend::Backend>, c: Clicked, reveal: impl FnOnce(String, Option<String>)) {
    let opens = c.open.is_some() && c.response.as_ref().is_none_or(|r| r.kind == ResponseKind::Clicked);
    match &c.open {
        Some(url) if opens => open_url(url),
        _ if c.response.as_ref().is_none_or(|r| r.kind == ResponseKind::Clicked) => reveal(c.session, c.needs_you),
        _ => {}
    }
    if let (Some(id), Some(r)) = (c.id, c.response) {
        respond(backend, &id, r);
    }
}

/// Tell midnad what the human did with notification `id` (`notify.respond`), off the main thread.
pub fn respond(backend: &std::sync::Arc<dyn crate::backend::Backend>, id: &str, r: Response) {
    let (b, params) = (backend.clone(), serde_json::json!({ "id": id, "response": r }));
    std::thread::spawn(move || {
        if let Err(e) = b.call("notify.respond", params) {
            eprintln!("notify.respond: {e}");
        }
    });
}

/// Open a notification's URL with whatever handles it (the browser, Finder, another app).
pub fn open_url(url: &str) {
    let url = url.to_string();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("/usr/bin/open").arg(&url).stdin(std::process::Stdio::null()).output();
    });
}

/// The request identifier for a `notify.send` id (so the same id replaces it).
fn request_id(id: &str) -> String {
    format!("id:{id}")
}

/// Button `label`'s action identifier.
const ACTION_PREFIX: &str = "action:";
/// The `userInfo` key holding `{id, open}` as JSON.
const INFO_KEY: &str = "midna";
/// Categories registered for sets of buttons, oldest first (at most CATEGORIES_MAX).
static CATEGORIES: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());
const CATEGORIES_MAX: usize = 64;

static CLICKS: OnceLock<async_channel::Sender<Clicked>> = OnceLock::new();
/// `UNAuthorizationStatus` as last seen (-1 = not checked yet).
static STATUS: AtomicI8 = AtomicI8::new(-1);
/// `UNNotificationSetting` for sounds as last seen (1 = the human turned midna's sounds off).
static SOUNDS: AtomicI8 = AtomicI8::new(-1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Permission {
    /// A dev build: notifications go through osascript.
    Dev,
    Unknown,
    NotAsked,
    Denied,
    Allowed,
}

/// What macOS says about midna's notification permission (Settings ▸ Permissions).
pub fn permission() -> Permission {
    if !native() {
        return Permission::Dev;
    }
    match STATUS.load(Ordering::Relaxed) {
        -1 => Permission::Unknown,
        0 => Permission::NotAsked,
        1 => Permission::Denied,
        _ => Permission::Allowed,
    }
}

/// Running from Midna.app with a bundle id (the only case `UNUserNotificationCenter` works).
fn native() -> bool {
    static NATIVE: OnceLock<bool> = OnceLock::new();
    *NATIVE.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        crate::install::bundle_of(&exe).is_some() && NSBundle::mainBundle().bundleIdentifier().is_some()
    })
}

/// Install the click delegate and read the current permission. Call once, at launch.
pub fn start(clicks: async_channel::Sender<Clicked>) {
    let _ = CLICKS.set(clicks);
    if !native() {
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(), init] };
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // The center holds its delegate weakly; this one lives as long as the app.
    std::mem::forget(delegate);
    refresh_permission();
}

/// Re-read the permission (the human may have changed it in System Settings).
pub fn refresh_permission() {
    if !native() {
        return;
    }
    let done = RcBlock::new(|settings: NonNull<UNNotificationSettings>| {
        let settings = unsafe { settings.as_ref() };
        STATUS.store(settings.authorizationStatus().0 as i8, Ordering::Relaxed);
        SOUNDS.store(settings.soundSetting().0 as i8, Ordering::Relaxed);
    });
    UNUserNotificationCenter::currentNotificationCenter().getNotificationSettingsWithCompletionHandler(&done);
}

/// Ask macOS for permission now (onboarding), without posting anything. Asking again once
/// decided returns at once without a prompt.
pub fn request_permission() {
    if !native() {
        return;
    }
    let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge;
    let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        STATUS.store(if granted.as_bool() { 2 } else { 1 }, Ordering::Relaxed);
    });
    UNUserNotificationCenter::currentNotificationCenter().requestAuthorizationWithOptions_completionHandler(options, &done);
}

/// Show one notification about `session` (None: about midna in general).
pub fn post(p: &Posted, session: Option<&str>) {
    if !native() {
        return post_osascript(p);
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(&p.title));
    content.setBody(&NSString::from_str(&p.body));
    // A daemon from before custom sounds sends no file: the system's default sound.
    let named = p.notification_sound.as_deref().filter(|_| p.sound);
    if let Some(file) = named {
        content.setSound(Some(&UNNotificationSound::soundNamed(&NSString::from_str(file))));
    } else if p.sound && p.sound_file.is_none() {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    if let Some(a) = p.image.as_deref().and_then(attachment) {
        content.setAttachments(&NSArray::from_retained_slice(&[a]));
    }
    if let Some(s) = session {
        content.setThreadIdentifier(&NSString::from_str(s));
    }
    // The needs-you item it's about, so a click opens that card rather than the oldest one.
    if let Some(n) = p.needs_you_id.as_deref() {
        content.setTargetContentIdentifier(Some(&NSString::from_str(n)));
    }
    if let Some(sent) = p.id.as_deref() {
        let info = serde_json::json!({ "id": sent, "open": p.open }).to_string();
        let dict = NSDictionary::from_slices::<NSString>(&[&*NSString::from_str(INFO_KEY)], &[&*NSString::from_str(&info)]);
        // SAFETY: an NSDictionary of NSStrings is an NSDictionary of objects.
        unsafe { content.setUserInfo(&Retained::cast_unchecked::<NSDictionary>(dict)) };
        content.setCategoryIdentifier(&NSString::from_str(&category_for(&p.actions)));
    }
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = match p.id.as_deref() {
        Some(sent) => request_id(sent),
        None => format!("{}-{}-{}", session.unwrap_or("midna"), midna_proto::time::now_unix(), N.fetch_add(1, Ordering::Relaxed)),
    };
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None);
    // Asking again once decided returns at once without a prompt.
    let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound | UNAuthorizationOptions::Badge;
    let done = RcBlock::new(move |granted: Bool, _err: *mut NSError| {
        STATUS.store(if granted.as_bool() { 2 } else { 1 }, Ordering::Relaxed);
        if granted.as_bool() {
            UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
        }
    });
    UNUserNotificationCenter::currentNotificationCenter().requestAuthorizationWithOptions_completionHandler(options, &done);
    // No rendered file: play it here, but not when macOS has midna's notifications or their
    // sounds off.
    if named.is_none() && STATUS.load(Ordering::Relaxed) != 1 && SOUNDS.load(Ordering::Relaxed) != 1 {
        play_posted(p);
    }
}

/// The category for a set of buttons (none: only the custom dismiss action), registered with
/// the center the first time it's used.
fn category_for(actions: &[String]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    actions.hash(&mut h);
    let key = format!("midna.respond.{:016x}", h.finish());
    let mut cats = CATEGORIES.lock().unwrap_or_else(|e| e.into_inner());
    if cats.iter().any(|(k, _)| *k == key) {
        return key;
    }
    if cats.len() >= CATEGORIES_MAX {
        cats.remove(0);
    }
    cats.push((key.clone(), actions.to_vec()));
    let all: Vec<Retained<UNNotificationCategory>> = cats
        .iter()
        .map(|(k, labels)| {
            let buttons: Vec<Retained<UNNotificationAction>> = labels
                .iter()
                .map(|l| UNNotificationAction::actionWithIdentifier_title_options(&NSString::from_str(&format!("{ACTION_PREFIX}{l}")), &NSString::from_str(l), UNNotificationActionOptions::empty()))
                .collect();
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(k),
                &NSArray::from_retained_slice(&buttons),
                &NSArray::new(),
                UNNotificationCategoryOptions::CustomDismissAction,
            )
        })
        .collect();
    UNUserNotificationCenter::currentNotificationCenter().setNotificationCategories(&NSSet::from_retained_slice(&all));
    key
}

/// Take a `notify.send` notification out of Notification Center (`notify.withdraw`).
pub fn withdraw(id: &str) {
    if !native() {
        return;
    }
    let ids = NSArray::from_retained_slice(&[NSString::from_str(&request_id(id))]);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.removeDeliveredNotificationsWithIdentifiers(&ids);
    center.removePendingNotificationRequestsWithIdentifiers(&ids);
}

/// Take every midna notification out of Notification Center (`notify.clear`, ⌘K).
pub fn clear_all() {
    if native() {
        UNUserNotificationCenter::currentNotificationCenter().removeAllDeliveredNotifications();
    }
}

/// Take `session`'s notifications out of Notification Center: you're looking at its terminal
/// now (selected it, or brought midna forward on it). Matches the thread identifier `post` sets.
pub fn clear(session: &str) {
    if !native() || STATUS.load(Ordering::Relaxed) < 2 {
        return;
    }
    let session = session.to_string();
    let done = RcBlock::new(move |delivered: NonNull<NSArray<UNNotification>>| {
        let delivered = unsafe { delivered.as_ref() };
        let ids: Vec<Retained<NSString>> = delivered
            .iter()
            .filter(|n| n.request().content().threadIdentifier().to_string() == session)
            .map(|n| n.request().identifier())
            .collect();
        if !ids.is_empty() {
            UNUserNotificationCenter::currentNotificationCenter().removeDeliveredNotificationsWithIdentifiers(&NSArray::from_retained_slice(&ids));
        }
    });
    UNUserNotificationCenter::currentNotificationCenter().getDeliveredNotificationsWithCompletionHandler(&done);
}

fn play_posted(p: &Posted) {
    if let (true, Some(file)) = (p.sound, p.sound_file.as_deref()) {
        play(file, p.volume.unwrap_or(100));
    }
}

thread_local! {
    /// Sounds still playing (an NSSound dropped mid-play may stop).
    static PLAYING: std::cell::RefCell<Vec<Retained<NSSound>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Play a sound file at `volume` (0–100) as loud as a notification would: scaled by the Mac's
/// alert volume (System Settings ▸ Sound), which notification sounds follow. Also Settings'
/// preview. Main thread.
pub fn play(file: &str, volume: u8) {
    let Some(sound) = NSSound::initWithContentsOfFile_byReference(NSSound::alloc(), &NSString::from_str(file), true) else {
        return;
    };
    sound.setVolume(volume.min(100) as f32 / 100. * alert_volume());
    sound.play();
    PLAYING.with_borrow_mut(|p| {
        p.retain(|s| s.isPlaying());
        p.push(sound);
    });
}

/// The alert volume, 0–1 (unset until the human first moves the slider: full).
fn alert_volume() -> f32 {
    let defaults = NSUserDefaults::standardUserDefaults();
    let key = NSString::from_str("com.apple.sound.beep.volume");
    if defaults.objectForKey(&key).is_none() { 1. } else { defaults.floatForKey(&key).clamp(0., 1.) }
}

/// A copy of an imported image to attach (the system takes the file it's given).
fn attachment(image: &str) -> Option<Retained<UNNotificationAttachment>> {
    let src = std::path::Path::new(image);
    let dir = std::env::temp_dir().join("midna-notify");
    std::fs::create_dir_all(&dir).ok()?;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ext = src.extension()?.to_str()?;
    let copy = dir.join(format!("{}-{}.{ext}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    std::fs::copy(src, &copy).ok()?;
    let url = NSURL::fileURLWithPath(&NSString::from_str(&copy.to_string_lossy()));
    let a = unsafe { UNNotificationAttachment::attachmentWithIdentifier_URL_options_error(&NSString::from_str("image"), &url, None) };
    if a.is_err() {
        let _ = std::fs::remove_file(&copy);
    }
    a.ok()
}

/// Dev builds: osascript shows no image; the sound plays here, at its volume.
fn post_osascript(p: &Posted) {
    let q = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut script = format!("display notification \"{}\" with title \"{}\"", q(&p.body), q(&p.title));
    if p.sound && p.sound_file.is_none() {
        script.push_str(" sound name \"Glass\"");
    }
    play_posted(p);
    std::thread::spawn(move || {
        let _ = std::process::Command::new("/usr/bin/osascript").args(["-e", &script]).stdin(std::process::Stdio::null()).output();
    });
}

define_class!(
    /// `UNUserNotificationCenterDelegate`: show banners while midna is frontmost too (the
    /// app already skipped the terminal you're looking at), and turn a click into `Clicked`.
    #[unsafe(super(NSObject))]
    #[name = "MidnaNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(&self, _center: &UNUserNotificationCenter, _n: &UNNotification, done: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>) {
            done.call((UNNotificationPresentationOptions::Banner | UNNotificationPresentationOptions::List | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, done: &DynBlock<dyn Fn()>) {
            let content = response.notification().request().content();
            let session = content.threadIdentifier().to_string();
            let needs_you = content.targetContentIdentifier().map(|n| n.to_string()).filter(|n| !n.is_empty());
            let info = content.userInfo().objectForKey(&NSString::from_str(INFO_KEY)).and_then(|o| o.downcast::<NSString>().ok());
            let info: serde_json::Value = info.and_then(|s| serde_json::from_str(&s.to_string()).ok()).unwrap_or_default();
            let id = info["id"].as_str().map(str::to_string);
            let open = info["open"].as_str().map(str::to_string);
            let action = response.actionIdentifier().to_string();
            let response = if action == unsafe { UNNotificationDismissActionIdentifier }.to_string() {
                Response { kind: ResponseKind::Dismissed, action: None }
            } else if let Some(label) = action.strip_prefix(ACTION_PREFIX) {
                Response { kind: ResponseKind::Action, action: Some(label.to_string()) }
            } else {
                Response { kind: ResponseKind::Clicked, action: None }
            };
            if let Some(tx) = CLICKS.get() {
                let response = id.is_some().then_some(response);
                let _ = tx.try_send(Clicked { session, needs_you, id, open, response });
            }
            done.call(());
        }
    }
);

/// Whether you're looking at a notification's terminal.
pub fn looking_at(window_active: bool, selected: Option<&str>, session: Option<&str>) -> bool {
    window_active && session.is_some() && selected == session
}

/// What the app does with a posted notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    Banner,
    SoundOnly,
    Nothing,
}

/// `push` covers terminals you aren't looking at, `push_focused` the one you are; looking at it
/// without `push_focused`, a kind that would push plays only its sound. Tests always show.
pub fn show(p: &Posted, looking: bool) -> Show {
    match (p.test, looking) {
        (true, _) => Show::Banner,
        (false, false) if p.push => Show::Banner,
        (false, true) if p.push_focused => Show::Banner,
        (false, true) if p.push => Show::SoundOnly,
        _ => Show::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_only_at_the_terminal_in_front_of_you() {
        assert!(looking_at(true, Some("a"), Some("a")));
        assert!(!looking_at(false, Some("a"), Some("a")));
        assert!(!looking_at(true, Some("a"), Some("b")));
        assert!(!looking_at(true, None, None));
    }

    #[test]
    fn push_settings_pick_banner_sound_or_nothing() {
        let p = |push, push_focused| Posted { push, push_focused, ..serde_json::from_value(serde_json::json!({ "category": "approval", "title": "t", "via": "app" })).unwrap() };
        // approval/attention out of the box: banner elsewhere, sound only in front of you.
        assert_eq!((show(&p(true, false), false), show(&p(true, false), true)), (Show::Banner, Show::SoundOnly));
        // Recorded only (failed, turn_done, … out of the box): nothing either way.
        assert_eq!((show(&p(false, false), false), show(&p(false, false), true)), (Show::Nothing, Show::Nothing));
        assert_eq!(show(&p(false, true), true), Show::Banner);
        assert_eq!(show(&Posted { test: true, ..p(false, false) }, true), Show::Banner);
        // A daemon from before push pushed everything.
        assert!(serde_json::from_value::<Posted>(serde_json::json!({ "category": "x", "title": "t", "via": "app" })).unwrap().push);
    }
}
