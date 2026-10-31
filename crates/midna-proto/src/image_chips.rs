//! Pasting an image path into Claude Code attaches the image: Claude reads the file and swaps
//! the path for an `[Image #N]` chip in its input box. Reading a big file takes a while (a
//! 25 MB photo took 12 s), and input that arrives meanwhile isn't safe: a second path pasted
//! then is dropped, and a ↩ sends the message without the image, which then turns up as a
//! message of its own. So whoever pastes images (the app's image sheet, `session.input`)
//! waits for each one's chip before the next step.
//!
//! Claude numbers images up the conversation and never reuses a number, so a chip that's new
//! on the screen is the image just pasted, wherever older ones still show in the transcript.
use std::collections::BTreeSet;
use std::time::Duration;

/// The longest one send waits for its images' chips, all of them together, before going on
/// anyway. Inside the app's 30 s RPC timeout, as `session.input` waits before it answers.
pub const WAIT: Duration = Duration::from_secs(20);
/// How often to look at the screen while waiting.
pub const POLL: Duration = Duration::from_millis(100);

/// The image numbers in every `[Image #N]` on these rows.
pub fn on_screen<S: AsRef<str>>(rows: &[S]) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for row in rows {
        let mut rest = row.as_ref();
        while let Some(i) = rest.find("[Image #") {
            rest = &rest[i + "[Image #".len()..];
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 && rest[digits..].starts_with(']') && let Ok(n) = rest[..digits].parse() {
                out.insert(n);
            }
        }
    }
    out
}

/// Whether `now` shows a chip `before` didn't: the pasted image has been attached.
pub fn arrived(before: &BTreeSet<u32>, now: &BTreeSet<u32>) -> bool {
    now.iter().any(|n| !before.contains(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_chip_numbers() {
        let rows = ["❯ look at [Image #3] and [Image #12]", "[Image #] [Image #4x] [image #5]", "  [Image #7]"];
        assert_eq!(on_screen(&rows).into_iter().collect::<Vec<_>>(), vec![3, 7, 12]);
        assert!(on_screen::<&str>(&[]).is_empty());
    }

    #[test]
    fn a_new_number_is_an_arrival() {
        let before = on_screen(&["❯ [Image #5]"]);
        assert!(!arrived(&before, &on_screen(&["❯ [Image #5]", "❯ /var/x.png"])));
        assert!(arrived(&before, &on_screen(&["❯ [Image #5]", "❯ [Image #6]"])));
        // The old chip scrolled away; the new one still counts.
        assert!(arrived(&before, &on_screen(&["❯ [Image #6]"])));
    }
}
