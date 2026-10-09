//! Settings ▸ Appearance ▸ Status looks: `ui.status.looks` as one row per built-in status
//! (idle … exited), each with color swatches (built in, the named status colors, a custom
//! #rrggbb), an icon menu and a label field, all saved on change. The per-agent rules
//! (`claude.working = …`) stay CLI-only but are listed under the rows, each with ×.
use super::*;
use midna_proto::settings::{STATUS_STATES, StatusLook, status_look};

const KEY: &str = "ui.status.looks";
/// How long the label field waits after the last keystroke before it saves.
const LABEL_SAVE: Duration = Duration::from_millis(450);
/// The icon menu's choices, after "Agent icon" (no icon set).
const ICONS: &[&str] = &["check", "cross", "bell", "bell-off", "lock", "bolt", "play", "pin", "restart", "queue", "link", "globe", "file", "search", "branch", "pr", "shell", "rules", "keyboard", "orbit", "claude", "codex"];

pub(super) struct State {
    /// Each status's label field (in `STATUS_STATES` order) and the value it was last filled from.
    labels: Vec<(LineInput, String)>,
    /// The custom color popover's field (one popover is open at a time).
    hex: LineInput,
    /// A label waiting out `LABEL_SAVE`: the status's index and its timer.
    pending: Option<(usize, Task<()>)>,
    _subs: Vec<Subscription>,
}

impl State {
    pub fn new(cx: &mut Context<SettingsWindow>) -> State {
        let labels: Vec<(LineInput, String)> = STATUS_STATES.iter().map(|_| (LineInput::new(cx, false, "Label"), String::new())).collect();
        let hex = LineInput::new(cx, false, "#rrggbb");
        let mut subs: Vec<Subscription> = labels
            .iter()
            .enumerate()
            .map(|(i, (input, _))| cx.subscribe(&input.field, move |s, _, _: &crate::ui::text_input::FieldChanged, cx| s.label_typed(i, cx)))
            .collect();
        subs.push(cx.subscribe(&hex.field, |s, _, _: &crate::ui::text_input::FieldChanged, cx| s.hex_typed(cx)));
        State { labels, hex, pending: None, _subs: subs }
    }
}

/// The rules as stored.
fn rules_of(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// A rule's match (`needs_you`, `claude.working`).
fn rule_match(r: &str) -> &str {
    r.split_once('=').map_or(r, |(m, _)| m.trim())
}

/// A look as a rule's value: `<color> icon:<name> label:<text>` (label last: it takes the rest).
fn look_text(l: &StatusLook) -> String {
    let mut parts: Vec<String> = vec![];
    parts.extend(l.color.clone());
    parts.extend(l.icon.as_ref().map(|i| format!("icon:{i}")));
    parts.extend(l.label.as_ref().map(|t| format!("label:{t}")));
    parts.join(" ")
}

/// The rules with `state`'s plain rules replaced by one for `look` (where the first one was,
/// else at the end), or dropped when the look keeps everything built in. Per-agent rules stay.
fn with_look(rules: &[String], state: &str, look: &StatusLook) -> Vec<String> {
    let at = rules.iter().position(|r| rule_match(r) == state);
    let mut out: Vec<String> = rules.iter().filter(|r| rule_match(r) != state).cloned().collect();
    let text = look_text(look);
    if !text.is_empty() {
        out.insert(at.unwrap_or(out.len()).min(out.len()), format!("{state} = {text}"));
    }
    out
}

/// A status as a row name.
fn state_name(state: &str) -> &'static str {
    match state {
        "idle" => "Idle",
        "working" => "Working",
        "needs_you" => "Needs you",
        "done" => "Done",
        "failed" => "Failed",
        _ => "Exited",
    }
}

fn state_of(state: &str) -> crate::model::StatusState {
    use crate::model::StatusState as S;
    match state {
        "working" => S::Working,
        "needs_you" => S::NeedsYou,
        "done" => S::Done,
        "failed" => S::Failed,
        "exited" => S::Exited,
        _ => S::Idle,
    }
}

/// A per-agent rule's ×.
fn remove_x(t: &Theme, id: SharedString, label: String) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(20.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(5.))
        .cursor_pointer()
        .hover(|s| s.bg(t.line))
        .tooltip(crate::ui::header::tip(label))
        .child(Icon::Cross.el(10., t.dim))
}

fn hex_ok(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit())
}

impl SettingsWindow {
    fn looks_rules(&self) -> Vec<String> {
        rules_of(&self.value(KEY))
    }

    /// `state`'s look from its plain rules only (what its row edits).
    fn plain_look(&self, state: &str) -> StatusLook {
        // no terminal kind is called "", so only the `<state>` rules apply
        status_look(&self.looks_rules(), "", state)
    }

    /// Change `state`'s look and save it, if that changes anything.
    fn edit_look(&mut self, state: &str, f: impl FnOnce(&mut StatusLook), cx: &mut Context<Self>) {
        let rules = self.looks_rules();
        let mut look = self.plain_look(state);
        f(&mut look);
        let next = with_look(&rules, state, &look);
        if next != rules {
            self.set(KEY, json!(next), cx);
        }
    }

    /// Fill the label fields from the setting, leaving alone one you've typed in since.
    pub(super) fn sync_looks(&mut self, cx: &mut Context<Self>) {
        let wants: Vec<String> = STATUS_STATES.iter().map(|s| self.plain_look(s).label.unwrap_or_default()).collect();
        for (i, want) in wants.into_iter().enumerate() {
            if self.looks.pending.as_ref().is_some_and(|(p, _)| *p == i) {
                continue;
            }
            let (input, last) = &mut self.looks.labels[i];
            if input.text(cx) == *last && *last != want {
                input.set_text(&want, cx);
            }
            *last = want;
        }
    }

    /// A label field changed: save it once typing pauses.
    fn label_typed(&mut self, i: usize, cx: &mut Context<Self>) {
        let (input, last) = &self.looks.labels[i];
        if input.text(cx) == *last {
            return;
        }
        if let Some((p, _)) = self.looks.pending.take()
            && p != i
        {
            self.save_label(p, cx);
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LABEL_SAVE).await;
            let _ = this.update(cx, |s, cx| {
                s.looks.pending = None;
                s.save_label(i, cx);
            });
        });
        self.looks.pending = Some((i, task));
    }

    fn save_label(&mut self, i: usize, cx: &mut Context<Self>) {
        let (input, last) = &mut self.looks.labels[i];
        let text = input.text(cx).trim().to_string();
        *last = input.text(cx);
        self.edit_look(STATUS_STATES[i], |l| l.label = Some(text).filter(|t| !t.is_empty()), cx);
    }

    /// The custom color field changed: save it once it's a whole #rrggbb.
    fn hex_typed(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.picker.as_deref().and_then(|p| p.strip_prefix("looks-hex:")).map(str::to_string) else { return };
        let text = self.looks.hex.text(cx).trim().to_lowercase();
        let text = if text.starts_with('#') { text } else { format!("#{text}") };
        if hex_ok(&text) {
            self.edit_look(&state, |l| l.color = Some(text), cx);
        }
    }

    /// The whole editor: a row per status, then the per-agent rules.
    pub(super) fn looks_control(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut col = div().flex().flex_col().gap(px(6.)).w_full();
        for (i, state) in STATUS_STATES.iter().enumerate() {
            col = col.child(self.look_row(t, i, state, window, cx));
        }
        let rules = self.looks_rules();
        let agents: Vec<&String> = rules.iter().filter(|r| !STATUS_STATES.contains(&rule_match(r))).collect();
        if !agents.is_empty() {
            col = col.child(div().pt(px(6.)).text_size(px(11.5)).text_color(t.dim).child("For one kind of terminal (change these with midna settings set):"));
            for (i, r) in agents.into_iter().enumerate() {
                let rule = r.clone();
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().flex_1().min_w_0().truncate().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.fg).child(r.clone()))
                        .child(remove_x(t, SharedString::from(format!("looks-agent-x-{i}")), "Remove this rule".into()).on_click(cx.listener(move |s, _, _, cx| {
                            let next: Vec<String> = s.looks_rules().into_iter().filter(|x| *x != rule).collect();
                            s.set(KEY, json!(next), cx);
                        }))),
                );
            }
        }
        col.into_any_element()
    }

    fn look_row(&self, t: &Theme, i: usize, state: &'static str, window: &Window, cx: &mut Context<Self>) -> Div {
        let look = self.plain_look(state);
        let color = look.color.as_deref().map(|c| t.status_color(c));
        let dot = match color {
            Some(c) => div().size(px(9.)).flex_none().rounded_full().bg(c),
            None => crate::ui::status_dot(t, state_of(state), 9.),
        };
        let swatch = |id: String, on: bool, tip: String, inner: Div| {
            div()
                .id(SharedString::from(id))
                .size(px(20.))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_2()
                .border_color(if on { t.fg } else { gpui_kit::transparent_black() })
                .cursor_pointer()
                .tooltip(crate::ui::header::tip(tip))
                .child(inner)
        };
        let mut swatches = div().flex().flex_none().items_center().gap(px(1.));
        let builtin = look.color.is_none();
        swatches = swatches.child(
            swatch(format!("looks-c-{state}-builtin"), builtin, "Built in".into(), div().size(px(12.)).flex().items_center().justify_center().child(crate::ui::status_dot(t, state_of(state), 9.)))
                .on_click(cx.listener(move |s, _, _, cx| s.edit_look(state, |l| l.color = None, cx))),
        );
        for c in midna_proto::types::STATUS_COLORS {
            let on = look.color.as_deref() == Some(*c);
            swatches = swatches.child(
                swatch(format!("looks-c-{state}-{c}"), on, c.to_string(), div().size(px(12.)).rounded(px(4.)).bg(t.status_color(c)))
                    .on_click(cx.listener(move |s, _, _, cx| s.edit_look(state, |l| l.color = Some(c.to_string()), cx))),
            );
        }
        let custom = look.color.clone().filter(|c| hex_ok(c));
        let hex_key = format!("looks-hex:{state}");
        let hex_open = self.picker.as_deref() == Some(hex_key.as_str());
        let inner = match &custom {
            Some(c) => div().size(px(12.)).rounded(px(4.)).bg(t.status_color(c)),
            None => div().size(px(12.)).rounded(px(4.)).border_1().border_color(t.dim).flex().items_center().justify_center().text_size(px(9.)).text_color(t.dim).child("#"),
        };
        let current_hex = custom.clone().unwrap_or_default();
        swatches = swatches.child(
            div()
                .relative()
                .child(
                    swatch(format!("looks-c-{state}-custom"), custom.is_some() || hex_open, custom.clone().unwrap_or_else(|| "Custom color".into()), inner)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |s, _, window, cx| {
                            if s.picker.as_deref() == Some(hex_key.as_str()) {
                                s.picker = None;
                            } else {
                                s.picker = Some(hex_key.clone());
                                s.looks.hex.set_text(&current_hex, cx);
                                s.looks.hex.focus.focus(window, cx);
                            }
                            cx.notify();
                        })),
                )
                .children(hex_open.then(|| self.hex_popover(t, window, cx))),
        );
        let (field, _) = &self.looks.labels[i];
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().flex().flex_none().items_center().gap(px(7.)).w(px(80.)).child(dot).child(div().text_size(px(12.5)).text_color(t.fg).child(state_name(state))))
            .child(swatches)
            .child(self.icon_menu(t, state, look.icon.clone(), color.unwrap_or(t.fg), cx))
            .child(
                field
                    .render(t, SharedString::from(format!("looks-label-{state}")), window)
                    .h(px(26.))
                    .flex_1()
                    .min_w(px(80.))
                    .on_key_down(cx.listener(move |s, ev: &KeyDownEvent, _, cx| {
                        let (input, last) = &mut s.looks.labels[i];
                        match input.on_key(ev, cx) {
                            KeyOutcome::Submit => {
                                cx.stop_propagation();
                                s.looks.pending = None;
                                s.save_label(i, cx);
                            }
                            KeyOutcome::Cancel => {
                                // back to what's saved
                                cx.stop_propagation();
                                s.looks.pending = None;
                                let saved = last.clone();
                                input.set_text(&saved, cx);
                            }
                            KeyOutcome::Ignored => {}
                        }
                    })),
            )
    }

    /// The custom color popover under the # swatch: a field that saves once it holds a whole #rrggbb.
    fn hex_popover(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        deferred(
            anchored().offset(point(px(-60.), px(26.))).snap_to_window_with_margin(px(8.)).child(
                crate::ui::sidebar::menu_box(t)
                    .w(px(170.))
                    .p(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(div().text_size(px(11.)).text_color(t.dim).child("Custom color"))
                    .child(self.looks.hex.render(t, "looks-hex", window).h(px(26.)).on_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| {
                        if matches!(s.looks.hex.on_key(ev, cx), KeyOutcome::Submit | KeyOutcome::Cancel) {
                            cx.stop_propagation();
                            s.picker = None;
                            cx.notify();
                        }
                    }))),
            ),
        )
        .with_priority(2)
    }

    /// The icon menu: "Agent icon" (none set) or one of `ICONS`, drawn in the status's color.
    fn icon_menu(&self, t: &Theme, state: &'static str, current: Option<String>, color: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let key = format!("looks-icon:{state}");
        let open = self.picker.as_deref() == Some(key.as_str());
        let k = key.clone();
        let shown = current.as_deref().and_then(Icon::from_name);
        let button = div()
            .id(SharedString::from(format!("pick-{key}")))
            .w(px(96.))
            .flex_none()
            .h(px(26.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .rounded(px(7.))
            .border_1()
            .border_color(if open { t.accent } else { t.line })
            .bg(t.panel)
            .cursor_pointer()
            .hover(|s| s.bg(t.raised))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |s, _, _, cx| {
                s.picker = if s.picker.as_deref() == Some(k.as_str()) { None } else { Some(k.clone()) };
                cx.notify();
            }))
            .children(shown.map(|i| i.el(12., color)))
            .child(div().flex_1().min_w_0().truncate().text_size(px(12.)).text_color(if current.is_some() { t.fg } else { t.dim }).child(current.clone().unwrap_or_else(|| "Agent icon".into())))
            .child(Icon::Chevron.el(10., t.dim));
        let menu = open.then(|| {
            let mut list = div().id(SharedString::from(format!("choices-{key}"))).max_h(px(300.)).overflow_y_scroll().flex().flex_col();
            let choices = std::iter::once(None).chain(ICONS.iter().map(|i| Some(*i)));
            for (n, choice) in choices.enumerate() {
                let on = current.as_deref() == choice;
                list = list.child(
                    div()
                        .id(SharedString::from(format!("choice-{key}-{n}")))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .px(px(8.))
                        .py(px(4.))
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(t.accent_soft))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.picker = None;
                            s.edit_look(state, |l| l.icon = choice.map(str::to_string), cx);
                            cx.notify();
                        }))
                        .child(div().w(px(12.)).flex_none().when(on, |d| d.child(Icon::Check.el(11., t.accent))))
                        .child(div().w(px(14.)).flex_none().children(choice.and_then(Icon::from_name).map(|i| i.el(12., color))))
                        .child(div().flex_1().min_w_0().truncate().child(choice.unwrap_or("Agent icon"))),
                );
            }
            deferred(
                anchored().offset(point(px(0.), px(30.))).snap_to_window_with_margin(px(8.)).child(
                    crate::ui::sidebar::menu_box(t).min_w(px(170.)).p(px(4.)).text_size(px(12.5)).on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(list),
                ),
            )
            .with_priority(2)
        });
        div().relative().child(button).children(menu)
    }
}

#[cfg(test)]
mod tests {
    use super::{KEY, StatusLook, status_look, with_look};
    use serde_json::json;

    fn rules(r: &[&str]) -> Vec<String> {
        r.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_look_round_trips_through_its_rule() {
        let l = StatusLook { color: Some("pink".into()), icon: Some("bell".into()), label: Some("Your turn".into()) };
        let r = with_look(&[], "needs_you", &l);
        assert_eq!(r, rules(&["needs_you = pink icon:bell label:Your turn"]));
        assert_eq!(status_look(&r, "", "needs_you"), l);
        assert!(midna_proto::settings::setting(KEY).unwrap().coerce(&json!(r)).is_ok());
    }

    #[test]
    fn editing_a_status_keeps_its_place_and_the_agent_rules() {
        let before = rules(&["idle = gray", "needs_you = pink", "claude.needs_you = icon:lock", "needs_you = label:Hi", "done = green"]);
        let l = StatusLook { color: Some("#ff8800".into()), ..Default::default() };
        assert_eq!(with_look(&before, "needs_you", &l), rules(&["idle = gray", "needs_you = #ff8800", "claude.needs_you = icon:lock", "done = green"]));
    }

    #[test]
    fn a_built_in_look_drops_the_rule() {
        let before = rules(&["working = blue", "codex.working = label:Thinking"]);
        assert_eq!(with_look(&before, "working", &StatusLook::default()), rules(&["codex.working = label:Thinking"]));
        assert_eq!(with_look(&before, "done", &StatusLook::default()), before);
    }
}
