//! ⌘V in the composer, with the clipboard read off the main thread (`crate::clipboard`). Until
//! it arrives, a tinted " Pasting… " run holds the paste's spot in the field, tagged with
//! [`KEY`] so it can be found wherever typing moved it. Typing goes on around it: the run's
//! look never becomes the typing look (`plain_typing`). ↩ meanwhile sends once the paste is
//! in; Esc, or erasing the run, cancels it.
use super::{MainWindow, Native, native, ns_color, submit};
use crate::clipboard::GIVE_UP;
use crate::theme::Theme;
use gpui_kit::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_app_kit::{NSBackgroundColorAttributeName, NSForegroundColorAttributeName};
use objc2_foundation::{NSAttributedStringKey, NSDictionary, NSMutableAttributedString, NSNumber, NSRange, NSString};
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// The attribute that marks the placeholder run; its value is the paste's number.
const KEY: &str = "MidnaPendingPaste";
const CHIP: &str = " Pasting… ";

#[derive(Default)]
pub struct Pending {
    count: u64,
    started: Option<Instant>,
    /// ↩ was pressed while it waited: send once it's in.
    submit: bool,
}

impl Pending {
    /// How long the clipboard has been on its way, while a paste waits for it.
    pub fn waiting(&self) -> Option<Duration> {
        self.started.map(|s| s.elapsed())
    }

    pub fn sends_after(&self) -> bool {
        self.submit
    }
}

thread_local! {
    /// The typing look from before the placeholder went in.
    static PLAIN: RefCell<Option<Retained<NSDictionary<NSAttributedStringKey, AnyObject>>>> = const { RefCell::new(None) };
}

fn key() -> Retained<NSString> {
    NSString::from_str(KEY)
}

/// ⌘V: put the placeholder in and read the clipboard. One at a time.
pub(super) fn start(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.composer.paste.started.is_some() {
        return;
    }
    m.composer.paste.count += 1;
    let id = m.composer.paste.count;
    let t = crate::theme::of(window, cx).clone();
    if !native(m, |n| insert_chip(n, id, &t)).unwrap_or(false) {
        return;
    }
    m.composer.paste.started = Some(Instant::now());
    m.composer.paste.submit = false;
    native(m, |n| { replace(n, n.tv.selectedRange(), " can you fix it?"); });
    let read = crate::clipboard::read(cx);
    cx.spawn_in(window, async move |this, cx| {
        let item = read.await;
        let _ = this.update_in(cx, |m, window, cx| landed(m, id, item, window, cx));
    })
    .detach();
    // Tick the hint's seconds, and give up on a copy that isn't coming.
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let going = this.update(cx, |m, cx| {
                if m.composer.paste.count != id || m.composer.paste.started.is_none() {
                    return false;
                }
                if m.composer.paste.waiting().is_some_and(|w| w >= GIVE_UP) {
                    cancel(m, cx);
                    m.toast(format!("Nothing arrived from the clipboard in {}s, so nothing was pasted.", GIVE_UP.as_secs()), cx);
                    return false;
                }
                cx.notify();
                true
            });
            if !going.unwrap_or(false) {
                return;
            }
        }
    })
    .detach();
    cx.notify();
}

/// ↩ while a paste waits: send once it's in. False when none waits.
pub(super) fn submit_later(m: &mut MainWindow, cx: &mut Context<MainWindow>) -> bool {
    if m.composer.paste.started.is_none() {
        return false;
    }
    m.composer.paste.submit = true;
    cx.notify();
    true
}

/// Esc, or the field is put away: no paste, and the placeholder comes out. False when none waits.
pub(super) fn cancel(m: &mut MainWindow, cx: &mut Context<MainWindow>) -> bool {
    if m.composer.paste.started.take().is_none() {
        return false;
    }
    m.composer.paste.submit = false;
    let id = m.composer.paste.count;
    native(m, |n| {
        if let Some(r) = find(n, id) {
            let sel = n.tv.selectedRange();
            replace(n, r, "");
            n.tv.setSelectedRange(moved(sel, r, 0));
        }
    });
    cx.notify();
    true
}

/// The clipboard is in: it replaces the placeholder, as ⌘V would have pasted it there.
fn landed(m: &mut MainWindow, id: u64, item: Option<ClipboardItem>, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.composer.paste.count != id || m.composer.paste.started.is_none() {
        return; // cancelled
    }
    m.composer.paste.started = None;
    let send = std::mem::take(&mut m.composer.paste.submit);
    cx.notify();
    // Erased by hand: that's a cancel too.
    let Some((r, sel)) = native(m, |n| find(n, id).map(|r| (r, n.tv.selectedRange()))).flatten() else { return };
    // Out with the placeholder first: an image or a secret opens a sheet, and what that sheet
    // sends back goes in at the cursor, which is now the paste's spot.
    native(m, |n| replace(n, r, ""));
    let Some(item) = item else { return restore(m, sel, r, 0) };
    if super::paste_image(m, &item, window, cx) {
        return;
    }
    let Some(text) = item.text() else { return restore(m, sel, r, 0) };
    if super::paste_secret(m, &text, window, cx) {
        return;
    }
    native(m, |n| replace(n, NSRange::new(r.location, 0), &text));
    restore(m, sel, r, text.encode_utf16().count());
    if send {
        let text = super::text(m);
        submit(m, text, window, cx);
    }
}

fn restore(m: &MainWindow, sel: NSRange, chip: NSRange, added: usize) {
    native(m, |n| n.tv.setSelectedRange(moved(sel, chip, added)));
}

/// Where a selection goes when `chip` becomes `added` UTF-16 units of text.
fn moved(sel: NSRange, chip: NSRange, added: usize) -> NSRange {
    let end = chip.location + chip.length;
    let location = if sel.location >= end {
        sel.location - chip.length + added
    } else if sel.location > chip.location {
        chip.location + added
    } else {
        sel.location
    };
    NSRange::new(location, sel.length)
}

/// The placeholder at the cursor, as one undoable edit, with the cursor after it.
fn insert_chip(n: &Native, id: u64, t: &Theme) -> bool {
    let tv = &n.tv;
    let Some(storage) = (unsafe { tv.textStorage() }) else { return false };
    let at = tv.selectedRange();
    let plain = tv.typingAttributes();
    PLAIN.with(|p| *p.borrow_mut() = Some(plain.clone()));
    let text = NSString::from_str(CHIP);
    let chip = unsafe { NSMutableAttributedString::initWithString_attributes(NSMutableAttributedString::alloc(), &text, Some(&plain)) };
    let all = NSRange::new(0, chip.length());
    let fg = ns_color(t.accent);
    let bg = ns_color(t.accent.opacity(0.16));
    let tag = NSNumber::new_u64(id);
    unsafe {
        chip.addAttribute_value_range(NSForegroundColorAttributeName, &fg, all);
        chip.addAttribute_value_range(NSBackgroundColorAttributeName, &bg, all);
        chip.addAttribute_value_range(&key(), &tag, all);
    }
    if !tv.shouldChangeTextInRange_replacementString(at, Some(&text)) {
        return false;
    }
    storage.replaceCharactersInRange_withAttributedString(at, &chip);
    tv.didChangeText();
    tv.setSelectedRange(NSRange::new(at.location + all.length, 0));
    unsafe { tv.setTypingAttributes(&plain) };
    true
}

/// Paste `id`'s placeholder in the field: its " Pasting… " when that's whole, else what's left
/// of the run (erased in part).
fn find(n: &Native, id: u64) -> Option<NSRange> {
    let storage = unsafe { n.tv.textStorage() }?;
    let k = key();
    let len = storage.length();
    let mut i = 0;
    while i < len {
        let mut r = NSRange::new(0, 0);
        let v = unsafe { storage.attribute_atIndex_effectiveRange(&k, i, &mut r) };
        if v.and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|v| v.as_u64() == id) {
            let s = storage.string().to_string();
            let run: String = String::from_utf16_lossy(&s.encode_utf16().skip(r.location).take(r.length).collect::<Vec<_>>());
            return Some(match run.find(CHIP) {
                Some(b) => NSRange::new(r.location + run[..b].encode_utf16().count(), CHIP.encode_utf16().count()),
                None => r,
            });
        }
        i = (r.location + r.length).max(i + 1);
    }
    None
}

/// `range` becomes `text`, undoably, with the cursor after it.
fn replace(n: &Native, range: NSRange, text: &str) {
    let s = NSString::from_str(text);
    let _: () = unsafe { msg_send![&*n.tv, insertText: &*s, replacementRange: range] };
}

/// The typing look to use instead of `attrs` when they carry the placeholder's (typing right
/// after it would otherwise type in its colors, and into its run).
pub(super) fn plain_typing(attrs: &NSDictionary<NSAttributedStringKey, AnyObject>) -> Option<Retained<NSDictionary<NSAttributedStringKey, AnyObject>>> {
    attrs.objectForKey(&key())?;
    PLAIN.with(|p| p.borrow().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;

    #[test]
    fn the_cursor_keeps_its_place_around_the_paste() {
        let chip = NSRange::new(10, 10);
        // Typed on after the placeholder: it moves with the text the paste adds.
        assert_eq!(moved(NSRange::new(26, 0), chip, 40).location, 56);
        // Before it: it stays.
        assert_eq!(moved(NSRange::new(4, 0), chip, 40).location, 4);
        // Inside it: after the pasted text.
        assert_eq!(moved(NSRange::new(13, 0), chip, 40).location, 50);
        // Cancelled: the placeholder's room closes up.
        assert_eq!(moved(NSRange::new(26, 2), chip, 0), NSRange::new(16, 2));
    }
}
