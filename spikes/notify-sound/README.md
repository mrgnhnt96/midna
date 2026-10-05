# notify-sound

Does macOS play a notification's custom sound for a signed, non-sandboxed app, and where may the
file live? `UNNotificationSound` has no volume, so midna wants to name a copy rendered at the
chosen volume.

Build, bundle and sign (Developer ID, hardened runtime, no sandbox, like Midna.app), then run:

    cargo build --offline --release
    # bundle target/release/notify-sound as SoundSpike.app (com.mrgnhnt.midna.soundspike), sign
    open -W SoundSpike.app --args /path/to/log

It posts three notifications 7 s apart: a chime from `~/Library/Sounds`, the same chime at 25%
baked in, and a low tone named by an absolute path in `$TMPDIR`; then deletes the files.

Result (2026-10-05, macOS 26): all three played with their banners, the second quieter. With a
Work Focus on, all three were delivered muted (NotificationCenter logs "muted by DND
suppression" / "Not playing sound"). So `soundNamed` takes an absolute path, and midna renders
sounds into `MIDNA_HOME/notify/cache` instead of `~/Library/Sounds` (which System Settings lists
as alert sounds).
