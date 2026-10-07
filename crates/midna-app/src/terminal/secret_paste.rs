//! Secrets pasted into an agent terminal. Anything typed into Claude Code or Codex goes to the
//! model, so ⌘V into an agent first scans the text (`midna_proto::secrets::scan`). If it
//! finds a token, key or `.env` secret, a sheet asks before anything is sent:
//!
//! - **Store and paste reference** (↩ / ⌘↩): `secret.set` for each checked value, then the
//!   paste goes in with `[secret:NAME]` in their place. The agent uses them through
//!   `midna secret exec NAME -- …` without ever seeing them.
//! - **Paste anyway**: the original text, as typed.
//! - **Cancel** (esc): nothing is sent.
//!
//! ⌥⌘V (Paste as Secret, also in the right-click menu) opens the sheet in any terminal, even
//! when nothing was detected: then the whole clipboard is the secret.
use super::TerminalView;
use crate::frame::ClientMsg;
use crate::theme::Theme;
use crate::ui::screen_kit::{KeyOutcome, LineInput, btn, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::secrets::{self, Found};
use serde_json::json;

pub struct Sheet {
    text: String,
    found: Vec<Found>,
    rows: Vec<Row>,
    /// Store for this terminal's project only (else every project).
    project_only: bool,
    error: Option<String>,
    busy: bool,
    /// Opened with ⌥⌘V rather than detected.
    explicit: bool,
    /// From the Kass composer: send it as the composer would (with ↩), not as a paste.
    submit: bool,
}

struct Row {
    input: LineInput,
    store: bool,
    label: &'static str,
    shown: String,
}

/// `ghp_••••••hJ6`: enough to recognize it, not enough to use it.
fn masked(v: &str) -> String {
    if v.starts_with("-----BEGIN") {
        return v.lines().next().unwrap_or("").to_string();
    }
    let n = v.chars().count();
    if n < 12 {
        return "•".repeat(n.min(8));
    }
    let head: String = v.chars().take(4).collect();
    let tail: String = v.chars().skip(n - 3).collect();
    format!("{head}••••••{tail}")
}

impl TerminalView {
    /// The text the human pasted: scan it first when an agent runs here. True = the sheet
    /// took it (nothing sent yet).
    pub(super) fn paste_checks_secrets(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.is_agent() || self.secret_sheet.is_some() {
            return false;
        }
        let found = secrets::scan(text);
        if found.is_empty() {
            return false;
        }
        self.open_secret_sheet(text.to_string(), found, false, window, cx);
        true
    }

    /// The composer is about to send `text` here (with ↩). True = the sheet took it.
    pub fn submit_checks_secrets(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let took = self.paste_checks_secrets(text.trim_end_matches(['\n', '\r']), window, cx);
        if let Some(s) = self.secret_sheet.as_mut().filter(|_| took) {
            s.submit = true;
        }
        took
    }

    /// ⌥⌘V: the clipboard as a secret, whatever it looks like.
    pub(super) fn paste_as_secret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) else { return };
        let mut found = secrets::scan(&text);
        if found.is_empty() {
            let start = text.len() - text.trim_start().len();
            let end = text.trim_end().len();
            if start >= end {
                return;
            }
            found.push(Found { start, end, label: "Secret", name: "SECRET".into() });
        }
        self.open_secret_sheet(text, found, true, window, cx);
    }

    fn open_secret_sheet(&mut self, text: String, found: Vec<Found>, explicit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut taken: Vec<String> = vec![];
        let rows: Vec<Row> = found
            .iter()
            .map(|f| {
                // Two values that suggest the same name get _2, _3…
                let mut name = f.name.clone();
                let mut n = 2;
                while taken.contains(&name) {
                    name = format!("{}_{n}", f.name);
                    n += 1;
                }
                taken.push(name.clone());
                let input = LineInput::new(cx, false, "NAME");
                input.set_text(&name, cx);
                Row { input, store: true, label: f.label, shown: masked(f.value(&text)) }
            })
            .collect();
        if let Some(r) = rows.first() {
            r.input.field.update(cx, |f, cx| f.select_all(cx));
            r.input.focus.focus(window, cx);
        }
        self.secret_sheet = Some(Sheet { text, found, rows, project_only: self.project_for_secrets().is_some(), error: None, busy: false, explicit, submit: false });
        cx.notify();
    }

    /// The project a secret stored here belongs to (None for root terminals).
    fn project_for_secrets(&self) -> Option<String> {
        self.project_id.clone().filter(|p| p != midna_proto::ROOT_PROJECT_ID)
    }

    pub(super) fn refocus_secret_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.secret_sheet.as_ref().and_then(|s| s.rows.first()) {
            r.input.focus.focus(window, cx);
            cx.notify();
        }
    }

    fn close_secret_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.secret_sheet = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn paste_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.secret_sheet.take() else { return };
        self.send_out(s.text, s.submit, window, cx);
    }

    /// A paste, or (from the composer) what the composer would have sent, then ↩.
    fn send_out(&mut self, text: String, submit: bool, window: &mut Window, cx: &mut Context<Self>) {
        if submit {
            let data = crate::composer::encode(&text, self.bracketed_paste());
            self.call("session.input", json!({ "id": self.session_id, "text": data, "enter": true }), window, cx, |_, _, _, _| {});
        } else {
            self.erase_selection(cx);
            self.deliver(ClientMsg::Paste(text));
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Store the checked values, then paste with references in their place.
    fn store_secrets(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project_for_secrets();
        let Some(s) = self.secret_sheet.as_mut() else { return };
        if s.busy {
            return;
        }
        let mut names: Vec<Option<String>> = vec![];
        let mut calls = vec![];
        for (f, r) in s.found.iter().zip(&s.rows) {
            if !r.store {
                names.push(None);
                continue;
            }
            let name = secrets::normalize_name(&r.input.text(cx));
            if !secrets::valid_name(&name) {
                s.error = Some("Each name needs a letter or _ first, then A–Z, 0–9 or _.".into());
                cx.notify();
                return;
            }
            if names.iter().flatten().any(|n| *n == name) {
                s.error = Some(format!("{name} is used twice."));
                cx.notify();
                return;
            }
            let project_id = if s.project_only { project.clone() } else { None };
            calls.push(json!({ "name": name, "value": f.value(&s.text), "project_id": project_id, "label": f.label }));
            names.push(Some(name));
        }
        let text = secrets::replace(&s.text, &s.found, &names);
        if calls.is_empty() {
            // Nothing checked: that's "paste anyway".
            return self.paste_anyway(window, cx);
        }
        s.busy = true;
        s.error = None;
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let r = cx
                .background_executor()
                .spawn(async move {
                    for c in calls {
                        backend.call("secret.set", c)?;
                    }
                    anyhow::Ok(())
                })
                .await;
            let _ = this.update_in(cx, |t, window, cx| match r {
                Ok(()) => {
                    let submit = t.secret_sheet.take().is_some_and(|s| s.submit);
                    t.send_out(text, submit, window, cx);
                }
                Err(e) => {
                    if let Some(s) = t.secret_sheet.as_mut() {
                        s.busy = false;
                        s.error = Some(format!("Couldn't store it: {e}"));
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn sheet_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let Some(s) = self.secret_sheet.as_mut() else { return };
        let focused = s.rows.iter().position(|r| r.input.focus.is_focused(window));
        if ks.key == "tab" && !ks.modifiers.platform && !s.rows.is_empty() {
            let n = s.rows.len();
            let i = match focused {
                Some(i) if ks.modifiers.shift => (i + n - 1) % n,
                Some(i) => (i + 1) % n,
                None => 0,
            };
            s.rows[i].input.focus.focus(window, cx);
            cx.stop_propagation();
            return;
        }
        if ks.key == "enter" && ks.modifiers.platform {
            self.store_secrets(window, cx);
            cx.stop_propagation();
            return;
        }
        let outcome = match focused {
            Some(i) => s.rows[i].input.on_key(ev, cx),
            None => match ks.key.as_str() {
                "enter" => KeyOutcome::Submit,
                "escape" => KeyOutcome::Cancel,
                _ => KeyOutcome::Ignored,
            },
        };
        match outcome {
            KeyOutcome::Submit => self.store_secrets(window, cx),
            KeyOutcome::Cancel => self.close_secret_sheet(window, cx),
            KeyOutcome::Ignored => {}
        }
        // Nothing typed here reaches the terminal underneath.
        cx.stop_propagation();
    }

    pub(super) fn render_secret_sheet(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.secret_sheet.as_ref()?;
        let n = s.rows.len();
        let title = match (s.explicit, n) {
            (true, 1) => "Paste as a secret".to_string(),
            (_, 1) => format!("This looks like a {}", s.rows[0].label.to_lowercase().replace("api ", "API ").replace("jwt", "JWT").replace("github", "GitHub")),
            _ => format!("This paste has {n} secrets"),
        };
        let who = match self.agent_kind() {
            Some("codex") => "Codex",
            Some(_) => "Claude",
            None => "this terminal",
        };
        let body = if self.is_agent() {
            format!("Anything pasted here is sent to {who}'s model. Store it in your Keychain and paste a reference instead; the agent uses it with `midna secret exec`.")
        } else {
            "Store it in your Keychain and paste a reference. Agents use it with `midna secret exec NAME -- <command>`.".to_string()
        };
        let mono = self.font_family.clone();
        let check = |on: bool| {
            div()
                .size(px(14.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(3.))
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |d| d.bg(t.accent).text_color(t.accent_fg).text_size(px(10.)).child("✓"))
        };
        let rows = s.rows.iter().enumerate().map(|(i, r)| {
            let store = r.store;
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .when(n > 1, |d| {
                    d.child(div().id(("secret-store", i)).cursor_pointer().child(check(store)).on_click(cx.listener(move |t, _, _, cx| {
                        if let Some(s) = t.secret_sheet.as_mut() {
                            s.rows[i].store = !s.rows[i].store;
                        }
                        cx.notify();
                    })))
                })
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(div().text_size(px(11.)).text_color(t.dim).child(r.label))
                        .child(div().font_family(mono.clone()).text_size(px(12.)).text_color(if store { t.fg } else { t.dim }).truncate().child(r.shown.clone())),
                )
                .child(div().text_color(t.dim).child("→"))
                .child(
                    div()
                        .id(("secret-name", i))
                        .w(px(200.))
                        .h(px(26.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .border_1()
                        .border_color(if r.input.focus.is_focused(window) { t.accent } else { t.line })
                        .bg(t.bg)
                        .font_family(mono.clone())
                        .text_size(px(12.))
                        .overflow_hidden()
                        .cursor_text()
                        .when(!store, |d| d.opacity(0.5))
                        .child(r.input.field.clone()),
                )
        });
        let scope = self.project_for_secrets().map(|_| {
            let on = s.project_only;
            div()
                .id("secret-scope")
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .text_size(px(12.))
                .text_color(t.dim)
                .child(check(on))
                .child("Only this project (otherwise every project)")
                .on_click(cx.listener(|t, _, _, cx| {
                    if let Some(s) = t.secret_sheet.as_mut() {
                        s.project_only = !s.project_only;
                    }
                    cx.notify();
                }))
        });
        let store_label = if s.busy { "Storing…".to_string() } else if n == 1 { "Store and paste reference  ↩".into() } else { "Store checked and paste  ↩".into() };
        let card = div()
            .id("secret-sheet-card")
            .occlude()
            .w(px(560.))
            .max_w(relative(0.94))
            .flex()
            .flex_col()
            .gap(px(14.))
            .p(px(20.))
            .rounded(px(12.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .text_color(t.fg)
            .text_size(px(13.))
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title))
            .child(div().text_color(t.dim).child(body))
            .child(div().flex().flex_col().gap(px(10.)).children(rows))
            .children(scope)
            .when_some(s.error.clone(), |d, e| d.child(div().text_color(t.err).text_size(px(12.)).child(e)))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(btn(t, "secret-cancel", "Cancel").on_click(cx.listener(|t, _, w, cx| t.close_secret_sheet(w, cx))))
                    .child(btn(t, "secret-anyway", "Paste anyway").on_click(cx.listener(|t, _, w, cx| t.paste_anyway(w, cx))))
                    .child(btn_primary(t, "secret-store", store_label).on_click(cx.listener(|t, _, w, cx| t.store_secrets(w, cx)))),
            );
        Some(
            div()
                .id("secret-sheet")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(hsla(0., 0., 0., 0.45))
                .on_mouse_down(MouseButton::Left, cx.listener(|t, _, w, cx| {
                    t.close_secret_sheet(w, cx);
                    cx.stop_propagation();
                }))
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .on_key_down(cx.listener(Self::sheet_key))
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::masked;

    #[test]
    fn masks_most_of_it() {
        assert_eq!(masked("ghp_aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6"), "ghp_••••••hJ6");
        assert_eq!(masked("hunter2"), "•••••••");
        assert_eq!(masked("-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----"), "-----BEGIN RSA PRIVATE KEY-----");
    }
}
