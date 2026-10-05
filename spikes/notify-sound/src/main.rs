//! Does macOS play a notification's custom sound for a signed, non-sandboxed app, and from where?
//! Posts three notifications, a few seconds apart; listen for each sound as its banner appears.
//!   1. `soundNamed("midna-spike-chime.wav")`, the file in ~/Library/Sounds (loud)
//!   2. `soundNamed("midna-spike-chime-quiet.wav")`, the same chime at 25% baked in
//!   3. `soundNamed(<absolute path>)`, a low tone outside ~/Library/Sounds
//! Writes what it did to the file given as its first argument (or stderr).
use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_user_notifications::*;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A 16-bit mono 44.1 kHz WAV of `tones` (Hz, seconds) at `gain` (0–1), with short fades.
fn wav(path: &Path, tones: &[(f32, f32)], gain: f32) {
    let rate = 44_100u32;
    let mut samples: Vec<i16> = Vec::new();
    for &(hz, secs) in tones {
        let n = (secs * rate as f32) as usize;
        for i in 0..n {
            let t = i as f32 / rate as f32;
            let fade = (i.min(n - i) as f32 / (0.01 * rate as f32)).min(1.0);
            let decay = (-3.0 * t / secs).exp();
            samples.push(((2.0 * std::f32::consts::PI * hz * t).sin() * gain * fade * decay * i16::MAX as f32) as i16);
        }
    }
    let data = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

fn main() {
    let start = Instant::now();
    let mut log: Box<dyn Write> = match std::env::args().nth(1) {
        Some(p) => Box::new(std::fs::File::create(p).unwrap()),
        None => Box::new(std::io::stderr()),
    };
    let mut say = move |s: String| {
        let _ = writeln!(log, "{:>6.3}s {s}", start.elapsed().as_secs_f32());
        let _ = log.flush();
    };

    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    let sounds = home.join("Library/Sounds");
    std::fs::create_dir_all(&sounds).unwrap();
    let chime = [(880.0, 0.18), (1320.0, 0.45)];
    let loud = sounds.join("midna-spike-chime.wav");
    let quiet = sounds.join("midna-spike-chime-quiet.wav");
    let outside = std::env::temp_dir().join("midna-spike-low.wav");
    wav(&loud, &chime, 0.9);
    wav(&quiet, &chime, 0.9 * 0.25);
    wav(&outside, &[(330.0, 0.6)], 0.9);
    say(format!("wrote {}, {}, {}", loud.display(), quiet.display(), outside.display()));

    let center = UNUserNotificationCenter::currentNotificationCenter();
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |granted: Bool, err: *mut NSError| {
        let err = unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = tx.send((granted.as_bool(), err));
    });
    let options = UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound;
    center.requestAuthorizationWithOptions_completionHandler(options, &done);
    // Up to a minute for the human to answer the permission prompt.
    let (granted, err) = rx.recv_timeout(Duration::from_secs(60)).unwrap_or((false, Some("no answer in 60s".into())));
    say(format!("authorization granted={granted} error={err:?}"));

    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |s: NonNull<UNNotificationSettings>| {
        let s = unsafe { s.as_ref() };
        let _ = tx.send(format!("status={} sound={} alertStyle={}", s.authorizationStatus().0, s.soundSetting().0, s.alertStyle().0));
    });
    center.getNotificationSettingsWithCompletionHandler(&done);
    say(format!("settings {}", rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()));
    if !granted {
        say("not allowed; stopping".into());
        return;
    }

    let tests = [
        ("1 of 3: chime from ~/Library/Sounds", "Loud two-note chime, starting with this banner.", loud.file_name().unwrap().to_string_lossy().into_owned()),
        ("2 of 3: the same chime, quieter", "Same chime at a quarter of the volume.", quiet.file_name().unwrap().to_string_lossy().into_owned()),
        ("3 of 3: low tone, absolute path", "A single low tone, if a path outside ~/Library/Sounds works.", outside.to_string_lossy().into_owned()),
    ];
    for (i, (title, body, sound)) in tests.iter().enumerate() {
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&format!("Sound test {title}")));
        content.setBody(&NSString::from_str(body));
        content.setSound(Some(&UNNotificationSound::soundNamed(&NSString::from_str(sound))));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&format!("spike-{i}")), &content, None);
        let (tx, rx) = mpsc::channel();
        let done = RcBlock::new(move |err: *mut NSError| {
            let _ = tx.send(unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string()));
        });
        say(format!("post {title} (sound {sound})"));
        center.addNotificationRequest_withCompletionHandler(&request, Some(&done));
        say(format!("added, error={:?}", rx.recv_timeout(Duration::from_secs(5)).ok().flatten()));
        std::thread::sleep(Duration::from_secs(7));
    }
    for f in [&loud, &quiet, &outside] {
        let _ = std::fs::remove_file(f);
    }
    say("done; removed the test sounds".into());
}
