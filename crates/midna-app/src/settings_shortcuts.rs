//! Settings ▸ Shortcuts: every shortcut in one list (the `keys.*` settings, then the built-in
//! keys screens handle themselves), searchable by name or by keys. The keyboard button (⌥⌘K)
//! is a key detector, like VS Code's "Record Keys": while it's on, the next keystroke is
//! captured before any binding fires (so ⌘W or ⌘Q are searched, not run) and the list
//! shows only the shortcuts on those keys.
//!
//! Clicking a shortcut's keys rebinds it the same way: press the new keys, ↩ saves, ⎋
//! cancels. × on the row (or Remove in its right-click menu) unbinds it. Keys another shortcut uses move: saving unbinds the other one.
use super::*;
use crate::actions::{FIXED, SHORTCUTS, keys_match, keystroke_text, pretty, setting_keys};
use crate::ui::sidebar::{menu_box, menu_item as item};

/// A shortcut being rebound: the keys pressed so far, and the hook that catches them.
pub(super) struct Editing {
    setting: &'static str,
    keys: Option<String>,
    _capture: Subscription,
}

/// Keystrokes in `window` go to `f` before any binding sees them (a lone modifier is skipped).
fn capture(window: &Window, cx: &mut Context<SettingsWindow>, f: fn(&mut SettingsWindow, &Keystroke, &mut Context<SettingsWindow>)) -> Subscription {
    let me = cx.entity().downgrade();
    let here = window.window_handle();
    cx.intercept_keystrokes(move |ev, window, cx| {
        if window.window_handle() != here {
            return;
        }
        let ks = &ev.keystroke;
        // a lone modifier press and release arrives as a keystroke named after it
        if matches!(ks.key.as_str(), "shift" | "control" | "alt" | "platform" | "function") {
            return;
        }
        let _ = me.update(cx, |s, cx| {
            f(s, ks, cx);
            cx.notify();
        });
        cx.stop_propagation();
    })
}

/// One row of the list.
struct Row {
    title: String,
    place: &'static str,
    /// Pretty keys ("" when unbound).
    keys: String,
    /// Every keystroke it's on, as written in settings (what a key search matches).
    raw: Vec<String>,
    /// The setting to rebind it, or None for a built-in key.
    setting: Option<&'static str>,
    cli: String,
}

impl SettingsWindow {
    fn shortcut_rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = SHORTCUTS
            .iter()
            .map(|s| {
                let v = value_text(&self.value(s.setting));
                let desc = midna_proto::settings::setting(s.setting).map(|x| x.description).unwrap_or("");
                Row {
                    title: desc.trim_end_matches('.').to_string(),
                    place: "",
                    keys: pretty(&v),
                    raw: vec![v.clone()],
                    setting: Some(s.setting),
                    cli: format!("midna settings set {} {}", s.setting, if v.is_empty() { "<keys>".into() } else { v }),
                }
            })
            .collect();
        rows.extend(FIXED.iter().map(|f| Row {
            title: f.title.to_string(),
            place: f.place,
            keys: f.label.map(str::to_string).unwrap_or_else(|| f.keys.iter().map(|k| pretty(k)).collect::<Vec<_>>().join(" · ")),
            raw: f.keys.iter().map(|k| k.to_string()).collect(),
            setting: None,
            cli: String::new(),
        }));
        rows
    }

    /// Rows matching the recorded keys, or else the search text.
    fn matching(&self, cx: &App) -> Vec<Row> {
        let q = self.search.text(cx).trim().to_lowercase();
        let rec = &self.recorded;
        self.shortcut_rows()
            .into_iter()
            .filter(|r| {
                if !rec.is_empty() {
                    return r.raw.iter().any(|k| keys_match(k, rec));
                }
                q.is_empty() || [r.title.as_str(), r.place, r.keys.as_str(), r.setting.unwrap_or("")].iter().any(|h| h.to_lowercase().contains(&q))
            })
            .collect()
    }

    /// The key detector: capture keystrokes in this window before bindings see them.
    pub(super) fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.recorder.take().is_some() {
            cx.notify();
            return;
        }
        self.view = View::Shortcuts;
        self.editing = None;
        self.search.clear(cx);
        self.recorded.clear();
        self.focus.focus(window, cx);
        self.recorder = Some(capture(window, cx, |s, ks, _| {
            let text = keystroke_text(ks);
            if text == "alt-cmd-k" {
                s.recorder = None;
            } else {
                s.recorded = vec![text];
            }
        }));
        cx.notify();
    }

    /// Start rebinding `setting`: the next keys pressed in this window are its new keys.
    pub(super) fn start_edit(&mut self, setting: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.recorder = None;
        self.shortcut_menu = None;
        self.focus.focus(window, cx);
        let capture = capture(window, cx, |s, ks, cx| {
            let plain = !ks.modifiers.modified();
            match ks.key.as_str() {
                "enter" if plain => s.save_edit(cx),
                "escape" if plain => s.editing = None,
                _ => {
                    if let Some(e) = s.editing.as_mut() {
                        e.keys = Some(setting_keys(ks));
                    }
                }
            }
        });
        self.editing = Some(Editing { setting, keys: None, _capture: capture });
        cx.notify();
    }

    /// Bind the pressed keys, unbinding any other shortcut that had them.
    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(Editing { setting, keys: Some(keys), .. }) = self.editing.take() else {
            return;
        };
        for other in self.taken_by(setting, &keys) {
            self.set(other, json!(""), cx);
        }
        self.set(setting, json!(keys), cx);
    }

    /// Back to the catalog default (taking the keys from any shortcut that has them now).
    fn reset_shortcut(&mut self, setting: &'static str, cx: &mut Context<Self>) {
        let default = crate::actions::default_key(setting);
        for other in self.taken_by(setting, default) {
            self.set(other, json!(""), cx);
        }
        self.set(setting, json!(default), cx);
    }

    /// The right-click menu on a shortcut row: change, remove, reset, copy the command.
    fn shortcut_menu_el(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (setting, pos) = self.shortcut_menu?;
        let current = value_text(&self.value(setting));
        let default = crate::actions::default_key(setting);
        let changed = crate::actions::normalize_keys(&current) != crate::actions::normalize_keys(default);
        let close = |s: &mut Self, cx: &mut Context<Self>| {
            s.shortcut_menu = None;
            cx.notify();
        };
        let mut menu = menu_box(t)
            .on_mouse_down_out(cx.listener(move |s: &mut Self, _: &MouseDownEvent, _, cx| close(s, cx)))
            .child(item(t, "sc-change", if current.is_empty() { "Set keys…" } else { "Change keys…" }, "", cx.listener(move |s: &mut Self, _: &ClickEvent, w, cx| s.start_edit(setting, w, cx))));
        if !current.is_empty() {
            menu = menu.child(item(t, "sc-remove", "Remove keys", &pretty(&current), cx.listener(move |s: &mut Self, _: &ClickEvent, _, cx| {
                s.set(setting, json!(""), cx);
                close(s, cx);
            })));
        }
        if changed {
            let hint = if default.is_empty() { "unbound".to_string() } else { pretty(default) };
            menu = menu.child(item(t, "sc-reset", "Reset to default", &hint, cx.listener(move |s: &mut Self, _: &ClickEvent, _, cx| {
                s.reset_shortcut(setting, cx);
                close(s, cx);
            })));
        }
        let cli = format!("midna settings set {setting} {}", if current.is_empty() { "\"\"".to_string() } else { current });
        menu = menu.child(item(t, "sc-copy", "Copy command", "", cx.listener(move |s: &mut Self, _: &ClickEvent, _, cx| {
            s.copy(cli.clone(), cx);
            close(s, cx);
        })));
        Some(deferred(anchored().position(pos).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }

    /// Other `keys.*` settings bound to `keys` (or to a chord starting with it).
    fn taken_by(&self, setting: &str, keys: &str) -> Vec<&'static str> {
        SHORTCUTS.iter().map(|s| s.setting).filter(|s| *s != setting && keys_match(&value_text(&self.value(s)), &[keys.to_string()])).collect()
    }

    /// What saving `keys` would collide with, in words ("" when nothing).
    fn conflict_note(&self, setting: &str, keys: &str) -> String {
        let title = |k: &str| midna_proto::settings::setting(k).map(|s| s.description.trim_end_matches('.')).unwrap_or(k).to_string();
        let mut notes: Vec<String> = self.taken_by(setting, keys).iter().map(|k| format!("Used by “{}”: saving unbinds it.", title(k))).collect();
        notes.extend(FIXED.iter().filter(|f| f.keys.iter().any(|k| keys_match(k, &[keys.to_string()]))).map(|f| format!("Also used in {}: {}.", f.place, f.title.to_lowercase())));
        notes.join(" ")
    }

    pub(super) fn shortcuts(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let recording = self.recorder.is_some();
        let focused = self.search.focus.is_focused(window);
        let field: AnyElement = if recording || !self.recorded.is_empty() {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.))
                .children(self.recorded.iter().map(|k| crate::ui::header::key_chip(t, pretty(k).into()).text_size(px(12.)).text_color(t.fg)))
                .when(self.recorded.is_empty(), |d| d.child(div().text_color(t.dim).child("Press a shortcut…")))
                .when(recording && !self.recorded.is_empty(), |d| d.child(div().text_color(t.dim).text_size(px(11.5)).child("press another to change it")))
                .into_any_element()
        } else {
            div().flex_1().min_w_0().flex().items_center().overflow_hidden().text_color(t.fg).child(self.search.field.clone()).into_any_element()
        };
        let clear = (!self.recorded.is_empty() || !self.search.is_empty(cx)).then(|| {
            div()
                .id("shortcut-search-clear")
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|s| s.bg(t.panel))
                .tooltip(crate::ui::header::tip("Clear"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|s, _, window, cx| {
                    s.recorded.clear();
                    s.recorder = None;
                    s.search.clear(cx);
                    s.search.focus.focus(window, cx);
                    cx.notify();
                }))
                .child(Icon::Cross.el(11., t.dim))
        });
        let detector = div()
            .id("shortcut-record")
            .size(px(26.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .when(recording, |d| d.bg(t.accent_soft).border_1().border_color(t.accent))
            .when(!recording, |d| d.hover(|s| s.bg(t.panel)))
            .tooltip(crate::ui::header::tip_fixed(if recording { "Stop recording keys" } else { "Record keys: search by pressing a shortcut" }, "⌥⌘K"))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|s, _, window, cx| s.toggle_recording(window, cx)))
            .child(Icon::Keyboard.el(15., if recording { t.accent } else { t.dim }));
        let bar = div()
            .id("shortcut-search")
            .flex_1()
            .flex()
            .items_center()
            .gap(px(8.))
            .h(px(34.))
            .pl(px(12.))
            .pr(px(4.))
            .rounded(px(9.))
            .border_1()
            .border_color(if recording { t.accent } else { t.line })
            .when(focused || recording, |d| d.shadow(vec![BoxShadow { color: t.accent_soft, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(3.), inset: false }]))
            .bg(t.raised)
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|s, _, window, cx| {
                    if s.recorder.is_none() {
                        s.recorded.clear();
                        s.search.focus.focus(window, cx);
                    }
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(|s, ev: &KeyDownEvent, window, cx| match s.search.on_key(ev, cx) {
                KeyOutcome::Cancel => {
                    s.search.clear(cx);
                    s.focus.focus(window, cx);
                    cx.notify();
                }
                KeyOutcome::Submit | KeyOutcome::Ignored => cx.propagate(),
            }))
            .child(Icon::Search.el(14., t.dim))
            .child(field)
            .children(clear)
            .child(detector);
        let rows = self.matching(cx);
        let n = rows.len();
        let head = div()
            .flex()
            .flex_none()
            .gap(px(16.))
            .px(px(18.))
            .py(px(8.))
            .bg(t.bg)
            .border_b_1()
            .border_color(t.line)
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .text_color(t.dim)
            .child(div().w(px(COL_SETTING + 80.)).flex_none().child("COMMAND"))
            .child(div().w(px(KEYS_W)).flex_none().child("KEYS · CLICK TO CHANGE"))
            .child(div().flex_1().min_w_0().child("SAME THING, FOR AGENTS"))
            .child(div().w(px(COL_WHO)).flex_none().child("WHO CAN SET"));
        let mut list = div().id("shortcut-rows").flex_1().min_h_0().overflow_y_scroll().flex().flex_col().pb(px(16.));
        let mut built_in = false;
        for (i, r) in rows.into_iter().enumerate() {
            if r.setting.is_none() && !built_in {
                built_in = true;
                list = list.child(
                    div()
                        .px(px(18.))
                        .pt(px(16.))
                        .pb(px(4.))
                        .flex()
                        .gap(px(8.))
                        .items_baseline()
                        .child(div().font_weight(FontWeight::BOLD).child("Built in"))
                        .child(div().text_size(px(11.5)).text_color(t.dim).child("Keys a screen handles itself. Not settings, so they can't be changed.")),
                );
            }
            list = list.child(self.shortcut_row(t, r, i, cx));
        }
        if n == 0 {
            list = list.child(div().px(px(18.)).py(px(24.)).text_color(t.dim).child(if self.recorded.is_empty() {
                "No shortcut matches.".to_string()
            } else {
                format!("Nothing uses {}. Ask above to bind it to something.", pretty(&self.recorded.join(" ")))
            }));
        }
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .px(px(16.))
                    .py(px(10.))
                    .border_b_1()
                    .border_color(t.line)
                    .child(bar)
                    .child(div().flex_none().w(px(90.)).text_size(px(12.)).text_color(t.dim).child(format!("{n} shortcut{}", if n == 1 { "" } else { "s" }))),
            )
            .child(head)
            .child(list)
            .children(self.shortcut_menu_el(t, cx))
    }

    fn shortcut_row(&self, t: &Theme, r: Row, i: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let cli = r.cli.clone();
        let copied = !cli.is_empty() && self.copied.as_ref().is_some_and(|(c, _)| *c == cli);
        let (who_text, who_color) = if r.setting.is_some() { ("agents too", t.dim) } else { ("built in", t.dim) };
        let editing = self.editing.as_ref().filter(|e| Some(e.setting) == r.setting);
        let note = editing.and_then(|e| e.keys.as_ref().map(|k| self.conflict_note(e.setting, k))).filter(|n| !n.is_empty());
        let keys: AnyElement = match (r.setting, editing) {
            (Some(_), Some(e)) => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(6.))
                .h(px(28.))
                .rounded(px(6.))
                .border_1()
                .border_color(t.accent)
                .bg(t.accent_soft)
                .child(match &e.keys {
                    Some(k) => crate::ui::header::key_chip(t, pretty(k).into()).text_size(px(12.)).text_color(t.fg).into_any_element(),
                    None => div().text_size(px(12.)).text_color(t.accent).child("Press keys…").into_any_element(),
                })
                .child(div().text_size(px(11.)).text_color(t.dim).whitespace_nowrap().child(if e.keys.is_some() { "↩ save · ⎋ cancel" } else { "⎋ cancel" }))
                .into_any_element(),
            (Some(setting), None) => {
                let current = value_text(&self.value(setting));
                let default = crate::actions::default_key(setting);
                let changed = crate::actions::normalize_keys(&current) != crate::actions::normalize_keys(default);
                let small = |id: &'static str| {
                    div()
                        .id((id, i))
                        .px(px(5.))
                        .h(px(22.))
                        .flex()
                        .items_center()
                        .rounded(px(5.))
                        .text_size(px(11.))
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover(|s| s.bg(t.raised).text_color(t.fg))
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        div()
                            .id(("shortcut-keys", i))
                            .cursor_pointer()
                            .rounded(px(5.))
                            .tooltip(crate::ui::header::tip("Click, then press the new keys"))
                            .on_click(cx.listener(move |s, _, window, cx| s.start_edit(setting, window, cx)))
                            .child(if r.keys.is_empty() {
                                div().px(px(6.)).h(px(22.)).flex().items_center().rounded(px(5.)).border_1().border_dashed().border_color(t.line).text_size(px(11.5)).text_color(t.dim).hover(|s| s.border_color(t.accent).text_color(t.fg)).child("Set keys")
                            } else {
                                crate::ui::header::key_chip(t, r.keys.clone().into()).text_size(px(12.)).text_color(t.fg).hover(|s| s.border_color(t.accent))
                            }),
                    )
                    .when(!r.keys.is_empty(), |d| {
                        d.child(small("shortcut-unbind").tooltip(crate::ui::header::tip("Remove keys")).on_click(cx.listener(move |s, _, _, cx| s.set(setting, json!(""), cx))).child(Icon::Cross.el(10., t.dim)))
                    })
                    .when(changed, |d| {
                        d.child(
                            small("shortcut-reset")
                                .tooltip(crate::ui::header::tip(if default.is_empty() { "Back to the default: unbound".to_string() } else { format!("Back to the default: {}", pretty(default)) }))
                                .on_click(cx.listener(move |s, _, _, cx| s.reset_shortcut(setting, cx)))
                                .child("Reset"),
                        )
                    })
                    .into_any_element()
            }
            (None, _) => crate::ui::header::key_chip(t, r.keys.into()).text_size(px(12.)).text_color(t.fg).into_any_element(),
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(16.))
            .min_h(px(38.))
            .px(px(18.))
            .py(px(5.))
            .border_b_1()
            .border_color(t.line)
            .when(editing.is_some() || self.shortcut_menu.is_some_and(|(s, _)| Some(s) == r.setting), |d| d.bg(t.raised))
            .when_some(r.setting, |d, setting| {
                d.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |s, ev: &MouseDownEvent, _, cx| {
                        s.shortcut_menu = Some((setting, ev.position));
                        cx.notify();
                    }),
                )
            })
            .child(
                div()
                    .w(px(COL_SETTING + 80.))
                    .flex_none()
                    .flex()
                    .flex_col()
                    .child(div().child(r.title))
                    .when(!r.place.is_empty(), |d| d.child(div().text_size(px(11.5)).text_color(t.dim).child(r.place)))
                    .when_some(note, |d, n| d.child(div().text_size(px(11.5)).line_height(px(15.)).text_color(t.need).child(n))),
            )
            .child(div().w(px(KEYS_W)).flex_none().flex().child(keys))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .when(!cli.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(t.mono_font.clone())
                                .text_size(px(11.5))
                                .text_color(t.dim)
                                .flex()
                                .gap(px(6.))
                                .child(div().flex_none().text_color(t.accent).child("$"))
                                .child(div().flex_1().min_w_0().child(cli.clone())),
                        )
                        .child(
                            div()
                                .id(("shortcut-copy", i))
                                .flex_none()
                                .h(px(22.))
                                .px(px(6.))
                                .flex()
                                .items_center()
                                .rounded(px(5.))
                                .text_size(px(11.))
                                .text_color(if copied { t.ok } else { t.dim })
                                .cursor_pointer()
                                .hover(|s| s.bg(t.raised).text_color(t.fg))
                                .on_click(cx.listener(move |s, _, _, cx| s.copy(cli.clone(), cx)))
                                .child(if copied { "Copied" } else { "Copy" }),
                        )
                    }),
            )
            .child(div().w(px(COL_WHO)).flex_none().text_size(px(11.5)).text_color(who_color).child(who_text))
    }
}

const KEYS_W: f32 = 210.;

#[cfg(test)]
mod tests {
    use crate::actions::{SHORTCUTS, keys_match, normalize_keys};
    use midna_proto::settings::{SETTINGS, SettingKind};

    #[test]
    fn every_keys_setting_is_bound_and_every_binding_has_a_setting() {
        let settings: Vec<&str> = SETTINGS.iter().filter(|s| matches!(s.ty, SettingKind::Keybinding)).map(|s| s.key).collect();
        let bound: Vec<&str> = SHORTCUTS.iter().map(|s| s.setting).collect();
        for k in &settings {
            assert!(bound.contains(k), "{k} has no binding in actions::SHORTCUTS");
        }
        for k in &bound {
            assert!(settings.contains(k), "{k} is bound but not in the settings catalog");
        }
    }

    #[test]
    fn defaults_parse_and_dont_collide() {
        let mut seen: Vec<(String, &str)> = vec![];
        for s in SHORTCUTS {
            let k = normalize_keys(crate::actions::default_key(s.setting));
            if k.is_empty() {
                continue; // unbound by default
            }
            assert!(gpui_kit::Keystroke::parse(&k).is_ok(), "{}: {k:?}", s.setting);
            if let Some((_, other)) = seen.iter().find(|(x, _)| keys_match(x, &[k.clone()])) {
                panic!("{} and {other} both default to {k}", s.setting);
            }
            seen.push((k, s.setting));
        }
    }

    #[test]
    fn recorded_keys_match_however_the_binding_is_written() {
        assert!(keys_match("cmd-shift-t", &["shift-cmd-t".into()]));
        assert!(keys_match("cmd-comma", &["cmd-,".into()]));
        assert!(keys_match("cmd-alt-up", &["alt-cmd-up".into()]));
        assert!(!keys_match("cmd-t", &["cmd-shift-t".into()]));
        assert!(!keys_match("", &["cmd-t".into()]));
        // a chord is found by its first keystroke
        assert!(keys_match("cmd-k cmd-t", &["cmd-k".into()]));
    }
}
