//! Reading the clipboard without stalling the window.
//!
//! What another device copied (Universal Clipboard: "Copying from iPhone…") is fetched when an
//! app first asks for it, and `NSPasteboard` blocks the asking thread until it arrives, which
//! can take seconds. GPUI reads on the main thread, so ⌘V froze the whole app and the keys
//! typed meanwhile all landed at once. Here the read runs on a background thread, and paste
//! reads the clipboard once per ⌘V rather than once per question it asks of it.

use gpui_kit::{App, ClipboardItem, Task};
use std::time::Duration;

/// A paste's placeholder waits this long to show, so a paste from this Mac doesn't flash it.
pub const SHOW_AFTER: Duration = Duration::from_millis(150);
/// From here the placeholder counts the seconds, so a slow copy doesn't look stuck.
pub const SLOW: Duration = Duration::from_secs(2);
/// A copy this slow isn't coming: stop waiting, and keep what was typed meanwhile.
pub const GIVE_UP: Duration = Duration::from_secs(20);

/// The clipboard's contents, read off the main thread: files (with their names as text), else
/// text, else an image, as `cx.read_from_clipboard()` gives them.
pub fn read(cx: &App) -> Task<Option<ClipboardItem>> {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        // Dev: `MIDNA_DEBUG_SLOW_PASTE=<ms>` makes every read that slow, as another device's can be.
        let slow = crate::dev::var("MIDNA_DEBUG_SLOW_PASTE").ok().and_then(|v| v.parse().ok()).map(std::time::Duration::from_millis);
        cx.background_executor().spawn(async move {
            if let Some(d) = slow {
                std::thread::sleep(d);
            }
            objc2::rc::autoreleasepool(|_| mac::read())
        })
    }
    // Tests use GPUI's own clipboard.
    #[cfg(not(all(target_os = "macos", not(test))))]
    {
        Task::ready(cx.read_from_clipboard())
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod mac {
    use gpui_kit::{ClipboardEntry, ClipboardItem, ClipboardString, ExternalPaths, Image, ImageFormat};
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString};
    use objc2_foundation::{NSString, NSURL};
    use std::path::PathBuf;

    /// Image types in GPUI's order (most likely pasted first), by pasteboard type.
    const IMAGES: [(ImageFormat, &str); 8] = [
        (ImageFormat::Png, "public.png"),
        (ImageFormat::Jpeg, "public.jpeg"),
        (ImageFormat::Webp, "org.webmproject.webp"),
        (ImageFormat::Gif, "com.compuserve.gif"),
        (ImageFormat::Svg, "public.svg-image"),
        (ImageFormat::Bmp, "com.microsoft.bmp"),
        (ImageFormat::Tiff, "public.tiff"),
        (ImageFormat::Ico, "com.microsoft.ico"),
    ];

    pub fn read() -> Option<ClipboardItem> {
        let pb = NSPasteboard::generalPasteboard();
        let types: Vec<String> = pb.types().map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        let has = |t: &str| types.iter().any(|x| x == t);
        let text = || unsafe { pb.stringForType(NSPasteboardTypeString) }.map(|s| ClipboardEntry::String(ClipboardString::new(s.to_string())));

        // Copied files: one file URL per pasteboard item.
        if has(&unsafe { NSPasteboardTypeFileURL }.to_string()) {
            let paths: Vec<PathBuf> = pb
                .pasteboardItems()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| unsafe { item.stringForType(NSPasteboardTypeFileURL) })
                        .filter_map(|s| NSURL::URLWithString(&s))
                        .filter_map(|u| u.path())
                        .map(|p| PathBuf::from(p.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            if !paths.is_empty() {
                let mut entries = vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))];
                entries.extend(text());
                return Some(ClipboardItem { entries });
            }
        }
        if has(&unsafe { NSPasteboardTypeString }.to_string()) {
            if let Some(t) = text() {
                return Some(ClipboardItem { entries: vec![t] });
            }
        }
        for (format, uti) in IMAGES {
            if !has(uti) {
                continue;
            }
            if let Some(data) = pb.dataForType(&NSString::from_str(uti)) {
                let bytes = data.to_vec();
                return Some(ClipboardItem { entries: vec![ClipboardEntry::Image(Image::from_bytes(format, bytes))] });
            }
        }
        None
    }
}
