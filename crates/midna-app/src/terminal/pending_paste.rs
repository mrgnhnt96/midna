//! ⌘V while the clipboard is still on its way (`crate::clipboard`): what another device copied
//! can take seconds to arrive, and the window keeps going meanwhile. A "Pasting…" chip holds the
//! paste's spot at the cursor. Keys typed after it are held here and drawn after the chip, and
//! go to the terminal once the paste has (the shell owns the line, so they can't go first).
//! Esc, ⌫ with nothing typed after the chip, or the chip's ✕ cancels the paste; the held keys
//! go anyway. After `SLOW` the chip counts the seconds, and after `GIVE_UP` it stops waiting.
use super::{FONT_SIZE, LINE_H, TerminalView};
use crate::clipboard::{GIVE_UP, SHOW_AFTER, SLOW};
use crate::frame::ClientMsg;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::time::{Duration, Instant};

pub struct PendingPaste {
    id: u64,
    /// The keys typed since ⌘V, in order.
    held: Vec<ClientMsg>,
    /// `SHOW_AFTER` has passed.
    show: bool,
    started: Instant,
}

/// What to do with the clipboard once it's read.
pub(super) type Then = fn(&mut TerminalView, Option<ClipboardItem>, &mut Window, &mut Context<TerminalView>);

impl TerminalView {
    /// Read the clipboard off the main thread, then `then` with it. One at a time: another ⌘V
    /// while one is on its way does nothing.
    pub(super) fn with_clipboard(&mut self, then: Then, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_paste.is_some() {
            return;
        }
        self.paste_gen += 1;
        let id = self.paste_gen;
        self.pending_paste = Some(PendingPaste { id, held: vec![], show: false, started: Instant::now() });
        let read = crate::clipboard::read(cx);
        // Show the chip, tick its seconds, and give up: each wakes the chip until the paste ends.
        cx.spawn(async move |this, cx| {
            let mut wait = SHOW_AFTER;
            loop {
                cx.background_executor().timer(wait).await;
                wait = Duration::from_secs(1);
                let going = this.update(cx, |t, cx| {
                    let Some(p) = t.pending_paste.as_mut().filter(|p| p.id == id) else { return false };
                    p.show = true;
                    if p.started.elapsed() >= GIVE_UP {
                        t.cancel_paste(cx);
                        t.say(format!("Nothing arrived from the clipboard in {}s, so nothing was pasted.", GIVE_UP.as_secs()), cx);
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
        cx.spawn_in(window, async move |this, cx| {
            let item = read.await;
            let _ = this.update_in(cx, |t, window, cx| t.finish_paste(id, item, then, window, cx));
        })
        .detach();
    }

    fn finish_paste(&mut self, id: u64, item: Option<ClipboardItem>, then: Then, window: &mut Window, cx: &mut Context<Self>) {
        // Cancelled (or a newer one): the held keys already went.
        if !self.pending_paste.as_ref().is_some_and(|p| p.id == id) {
            return;
        }
        let p = self.pending_paste.take().expect("checked above");
        self.after_paste = p.held;
        then(self, item, window, cx);
        self.release_after_paste();
        cx.notify();
    }

    /// The keys typed during a paste, now that it's in. Not while the secret sheet asks about
    /// it: they go when it's answered or cancelled.
    pub(super) fn release_after_paste(&mut self) {
        if self.secret_sheet.is_some() {
            return;
        }
        for m in std::mem::take(&mut self.after_paste) {
            self.deliver(m);
        }
    }

    /// Esc, ⌫ or ✕: no paste. What was typed after it goes now.
    pub(super) fn cancel_paste(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.pending_paste.take() {
            for m in p.held {
                self.deliver(m);
            }
            cx.notify();
        }
    }

    /// Input that waits for the paste on its way, or `m` back when there's none.
    pub(super) fn hold(&mut self, m: ClientMsg) -> Option<ClientMsg> {
        match self.pending_paste.as_mut() {
            Some(p) => {
                p.held.push(m);
                None
            }
            None => Some(m),
        }
    }

    /// A key while a paste is on its way. ⌘ keys aren't asked about (they're actions).
    pub(super) fn pending_paste_key(&mut self, ks: &Keystroke, is_held: bool, cx: &mut Context<Self>) -> bool {
        let Some(p) = self.pending_paste.as_mut() else { return false };
        let plain = !ks.modifiers.modified();
        match ks.key.as_str() {
            "escape" if plain => self.cancel_paste(cx),
            "backspace" if plain => {
                if !unhold(&mut p.held) {
                    self.cancel_paste(cx);
                }
            }
            _ => match super::key_msg_now(ks, is_held) {
                Some(k) => p.held.push(ClientMsg::Key(k)),
                None => return false,
            },
        }
        cx.notify();
        true
    }

    /// The chip and the keys held after it, at the cursor.
    pub(super) fn render_pending_paste(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.pending_paste.as_ref()?;
        let held = shown(&p.held);
        if !p.show && held.is_empty() {
            return None;
        }
        let at = self.cursor_bounds()?.origin;
        let close = div()
            .id("paste-cancel")
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.))
            .h(px(LINE_H - 6.))
            .rounded(px(4.))
            .cursor_pointer()
            .hover(|s| s.bg(theme.accent.opacity(0.22)))
            .child(crate::icons::Icon::Cross.el(9., theme.dim))
            .on_click(cx.listener(|t, _, _, cx| t.cancel_paste(cx)));
        let chip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(LINE_H - 2.))
            .mx(px(2.))
            .pl(px(7.))
            .pr(px(2.))
            .rounded(px(5.))
            .border_1()
            .border_color(theme.accent.opacity(0.6))
            .bg(theme.accent.opacity(0.14))
            .text_color(theme.accent)
            .text_size(px(12.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(crate::icons::spinner("paste-spin", 10., theme.accent))
            .child("Pasting…")
            .when(p.started.elapsed() >= SLOW, |d| d.child(div().text_color(theme.dim).font_weight(FontWeight::MEDIUM).child(format!("{}s", p.started.elapsed().as_secs()))))
            .child(div().ml(px(2.)).text_color(theme.dim).text_size(px(11.)).child("esc"))
            .child(close);
        let typed = div().font_family(self.font_family.clone()).text_size(px(FONT_SIZE)).text_color(theme.fg).whitespace_nowrap().child(held);
        let caret = div().flex_none().w(px(self.cell_w)).h(px(LINE_H - 4.)).bg(theme.fg.opacity(0.55));
        let row = div().id("pending-paste").occlude().flex().items_center().h(px(LINE_H)).bg(theme.term).child(chip).child(typed).child(caret);
        Some(deferred(anchored().position(at).child(row)).with_priority(1).into_any_element())
    }
}

/// Drop the last key held, as ⌫ would have erased it. False when nothing is held.
fn unhold(held: &mut Vec<ClientMsg>) -> bool {
    match held.last_mut() {
        None => false,
        Some(ClientMsg::Input(b)) => {
            let mut s = String::from_utf8_lossy(b).into_owned();
            s.pop();
            if s.is_empty() {
                held.pop();
            } else {
                *b = s.into_bytes();
            }
            true
        }
        Some(_) => {
            held.pop();
            true
        }
    }
}

/// What the held keys type, for the chip's line: text as is, ↩ and ⇥ as their symbols.
fn shown(held: &[ClientMsg]) -> String {
    held.iter()
        .map(|m| match m {
            ClientMsg::Input(b) => String::from_utf8_lossy(b).into_owned(),
            ClientMsg::Paste(t) => t.clone(),
            ClientMsg::Key(k) if k.key == "enter" => "↩".into(),
            ClientMsg::Key(k) if k.key == "tab" => "⇥".into(),
            ClientMsg::Key(k) => k.text.clone(),
            _ => String::new(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::core::prelude::v1::test;
    use crate::frame::{KeyAction, KeyMsg};

    fn key(name: &str) -> ClientMsg {
        ClientMsg::Key(KeyMsg { action: KeyAction::Press, mods: 0, key: name.into(), text: String::new() })
    }

    #[test]
    fn held_keys_show_as_typed() {
        let held = vec![ClientMsg::Input(b"\" && git push".to_vec()), key("enter")];
        assert_eq!(shown(&held), "\" && git push↩");
    }

    #[test]
    fn backspace_takes_back_the_last_key_held() {
        let mut held = vec![ClientMsg::Input(b"ab".to_vec()), key("enter")];
        assert!(unhold(&mut held));
        assert_eq!(shown(&held), "ab");
        assert!(unhold(&mut held));
        assert_eq!(shown(&held), "a");
        assert!(unhold(&mut held));
        assert!(held.is_empty());
        // Nothing after the chip: ⌫ erases the chip itself (the caller cancels).
        assert!(!unhold(&mut held));
    }
}
