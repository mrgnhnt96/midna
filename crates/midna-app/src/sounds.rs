//! Sound effects (`midna_proto::notify::EFFECTS`): the app plays a kind's sound itself, with no
//! banner, when you do something (approve, close a terminal, …) or move around (switch
//! terminals, ⌘K). They share the notification sounds' library and settings
//! (`notify.sound.<kind>`, `notify.volume.<kind>` times `notify.volume`).
//!
//! Also here: a notification's sound when its banner is skipped because you're looking at that
//! terminal, and the sounds agents play on purpose (`notify.play`, event `notify.sound`).
//!
//! `notify.sounds` off silences everything; `notify.sounds_in_app` off silences sounds while
//! midna is the frontmost app (so you hear only what happens while you're elsewhere).
//!
//! The settings live in a main-thread copy (`sync`, from every settings refresh), so any view
//! can call `play` without a handle to the main window. Main thread only (NSSound).
use midna_proto::notify::{self, sound_key, volume_key};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Config {
    settings: HashMap<String, Value>,
    /// `MIDNA_HOME`, where imported sounds live (the socket's directory).
    home: Option<PathBuf>,
}

thread_local! {
    static CONFIG: RefCell<Config> = RefCell::new(Config::default());
}

/// The settings changed (or the app connected).
pub fn sync(settings: &HashMap<String, Value>, home: Option<&Path>) {
    CONFIG.with_borrow_mut(|c| {
        c.settings = settings.clone();
        c.home = home.map(Path::to_path_buf);
    });
}

/// Play a kind's sound: a sound effect (`approved`, `switched`, …) or a notification category.
pub fn play(kind: &str) {
    let active = app_active();
    let found = CONFIG.with_borrow(|c| resolve(&|k| get(&c.settings, k), c.home.as_deref()?, kind, active));
    if let Some((file, volume)) = found {
        crate::notify::play(&file.to_string_lossy(), volume);
    }
}

/// Play a file the daemon already resolved and scaled (a skipped banner's sound, `notify.play`).
pub fn play_file(file: &str, volume: u8) {
    let active = app_active();
    if CONFIG.with_borrow(|c| allowed(&|k| get(&c.settings, k), active)) {
        crate::notify::play(file, volume);
    }
}

/// Whether sounds play right now (`notify.sounds`, and `notify.sounds_in_app` while frontmost).
pub fn allowed_now() -> bool {
    let active = app_active();
    CONFIG.with_borrow(|c| allowed(&|k| get(&c.settings, k), active))
}

/// A setting's value, or its catalog default before the first refresh.
fn get(settings: &HashMap<String, Value>, key: &str) -> Value {
    settings.get(key).cloned().or_else(|| midna_proto::settings::setting(key).map(|s| s.default.to_json())).unwrap_or(Value::Null)
}

fn allowed(setting: &dyn Fn(&str) -> Value, active: bool) -> bool {
    setting("notify.sounds") != Value::Bool(false) && !(active && setting("notify.sounds_in_app") == Value::Bool(false))
}

/// The file and volume (1–100) `kind` plays now, if any.
fn resolve(setting: &dyn Fn(&str) -> Value, home: &Path, kind: &str, active: bool) -> Option<(PathBuf, u8)> {
    if !notify::has_sound(kind) || !allowed(setting, active) {
        return None;
    }
    let int = |k: &str| setting(k).as_i64().unwrap_or(100).clamp(0, 100);
    let volume = int("notify.volume") * int(&volume_key(kind)) / 100;
    let file = notify::sound_file(home, setting(&sound_key(kind)).as_str()?)?;
    (volume > 0).then_some((file, volume as u8))
}

fn app_active() -> bool {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return false };
    objc2_app_kit::NSApplication::sharedApplication(mtm).isActive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with(pairs: &[(&str, Value)]) -> impl Fn(&str) -> Value + use<> {
        let map: HashMap<String, Value> = pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        move |k| get(&map, k)
    }

    /// A MIDNA_HOME holding the Twilight sounds (copied from the repo, as midnad installs them).
    fn home() -> PathBuf {
        let home = std::env::temp_dir().join(format!("midna-app-sounds-{}", std::process::id()));
        let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../midnad/assets/sounds/twilight");
        for name in notify::TWILIGHT {
            let to = notify::twilight_path(&home, name).unwrap();
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(assets.join(format!("{name}.wav")), to).unwrap();
        }
        home
    }

    #[test]
    fn kinds_use_their_sound_scaled_by_the_master_volume() {
        let home = home();
        let s = with(&[("notify.volume", json!(50)), ("notify.volume.switched", json!(40))]);
        // Out of the box: every kind plays its Twilight sound.
        assert_eq!(resolve(&s, &home, "approved", true), Some((home.join("notify/twilight/Rise.wav"), 50)));
        assert_eq!(resolve(&s, &home, "switched", true), Some((home.join("notify/twilight/Tick.wav"), 20)));
        assert_eq!(resolve(&s, &home, "failed", true), Some((home.join("notify/twilight/Uh-oh.wav"), 50)));
        assert_eq!(resolve(&s, &home, "nope", true), None);
        // A macOS sound, and none.
        let mac = with(&[("notify.sound.approved", json!("Tink")), ("notify.sound.copied", json!("none"))]);
        assert_eq!(resolve(&mac, &home, "approved", true), Some(("/System/Library/Sounds/Tink.aiff".into(), 100)));
        assert_eq!(resolve(&mac, &home, "copied", true), None);
        // Before midnad installed them, Twilight sounds play nothing.
        assert_eq!(resolve(&s, Path::new("/nonexistent"), "approved", true), None);
    }

    #[test]
    fn switches_silence_sounds() {
        let home = home();
        assert_eq!(resolve(&with(&[("notify.sounds", json!(false))]), &home, "approved", false), None);
        assert_eq!(resolve(&with(&[("notify.volume.approved", json!(0))]), &home, "approved", true), None);
        let elsewhere = with(&[("notify.sounds_in_app", json!(false))]);
        assert_eq!(resolve(&elsewhere, &home, "queue_sent", true), None, "midna is frontmost");
        assert!(resolve(&elsewhere, &home, "queue_sent", false).is_some(), "you're in another app");
        // An imported sound that isn't there plays nothing.
        assert_eq!(resolve(&with(&[("notify.sound.approved", json!("gone.mp3"))]), &home, "approved", true), None);
    }
}
