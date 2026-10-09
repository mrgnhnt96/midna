//! An agent's ask to change a human-only setting, shown as the setting itself: its Settings row
//! with the value the agent asked for, which you can change before saving (board
//! docs/design/SettingChange-B.dc.html). Lists and scripts are Settings' toggle chips, marked
//! with what the ask turns on or off; a switch or a choice is clickable; anything else shows
//! read-only. Save sends `needs_you.resolve` with your value when it isn't the agent's.
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::model::*;
use crate::settings_window::{self as sw, CHIP_KEYS};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::settings::{SCRIPT_PARTS, SettingKind, setting};
use serde_json::{Value, json};

/// The value on screen for item `id`: your edit, else what the agent asked for.
fn shown(m: &MainWindow, id: &str, c: &SettingChange) -> Value {
    m.stack.edits.get(id).cloned().unwrap_or_else(|| c.to.clone())
}

/// What Save should send as `value`: your edit, only when it isn't what the agent asked for.
pub fn edited(m: &MainWindow, n: &NeedsYou) -> Option<Value> {
    let c = n.setting.as_ref()?;
    m.stack.edits.get(&n.id).filter(|v| **v != c.to).cloned()
}

/// The item's name in lists and notifications: "Change “Status bar items”".
pub fn title(n: &NeedsYou) -> Option<String> {
    n.setting.as_ref().map(|c| format!("Change “{}”", sw::label_for(&c.key)))
}

/// One line on what the ask changes, for the badge and toasts: "Adds ci-status.sh · Removes
/// awake", "Turns it on", "Sets it to 30".
pub fn summary(n: &NeedsYou) -> Option<String> {
    let c = n.setting.as_ref()?;
    let key = c.key.as_str();
    if CHIP_KEYS.contains(&key) {
        let (was, to) = (items_of(key, &c.from), items_of(key, &c.to));
        let names = |v: Vec<&String>| v.into_iter().map(|i| sw::chip_label(i)).collect::<Vec<_>>().join(", ");
        let added = names(to.iter().filter(|i| !was.contains(i)).collect());
        let removed = names(was.iter().filter(|i| !to.contains(i)).collect());
        let parts: Vec<String> = [("Adds", added), ("Removes", removed)].into_iter().filter(|(_, s)| !s.is_empty()).map(|(w, s)| format!("{w} {s}")).collect();
        return Some(if parts.is_empty() { "Reorders it".into() } else { parts.join(" · ") });
    }
    Some(match &c.to {
        Value::Bool(true) => "Turns it on".into(),
        Value::Bool(false) => "Turns it off".into(),
        v => {
            let text = tilde(&sw::value_text(v));
            if text.chars().count() > 60 { format!("Sets it to {}…", text.chars().take(60).collect::<String>()) } else { format!("Sets it to {text}") }
        }
    })
}

/// A chip list's items, in order.
fn items_of(key: &str, v: &Value) -> Vec<String> {
    if sw::is_script(key) {
        return sw::script_items(v.as_str().unwrap_or(""));
    }
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn value_of(key: &str, items: &[String]) -> Value {
    if sw::is_script(key) { json!(sw::script_value(items)) } else { json!(items) }
}

/// `~/…` for a path under the home folder.
fn tilde(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&format!("{h}/")) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// A chip being dragged: (item id, chip).
struct ChipDrag(String, String);

struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// Puts `v` on screen as your edit of item `id`.
fn edit(m: &mut MainWindow, id: &str, v: Value, cx: &mut Context<MainWindow>) {
    m.stack.edits.insert(id.to_string(), v);
    cx.notify();
}

/// The setting box: a strip saying whether it shows the ask or today's value (and the button
/// that switches), then the setting's name, line and control.
pub fn panel(m: &MainWindow, t: &Theme, n: &NeedsYou, cx: &mut Context<MainWindow>) -> AnyElement {
    let Some(c) = n.setting.clone() else { return div().into_any_element() };
    let current = m.stack.current.as_deref() == Some(n.id.as_str());
    let value = if current { c.from.clone() } else { shown(m, &n.id, &c) };
    let id = n.id.clone();
    let strip = div()
        .flex()
        .items_center()
        .gap(px(8.))
        .px(px(14.))
        .py(px(8.))
        .border_b_1()
        .border_color(t.line)
        .bg(if current { t.raised } else { t.need.opacity(0.08) })
        .text_size(px(12.))
        .child(div().font_weight(FontWeight::SEMIBOLD).text_color(if current { t.dim } else { t.need }).child(if current { "Current" } else { "Proposed" }))
        .child(div().text_color(t.dim).child(format!("· {}", sw::place_of(&c.key))))
        .child(div().flex_1())
        .child(
            div()
                .id("setting-show-current")
                .text_color(t.dim)
                .cursor_pointer()
                .hover(|s| s.text_color(t.fg))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.stack.current = if m.stack.current.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) };
                    cx.notify();
                }))
                .child(if current { "Show proposed" } else { "Show current" }),
        );
    let control = control(t, n, &c, &value, current, cx);
    let paths: Vec<String> = if CHIP_KEYS.contains(&c.key.as_str()) { items_of(&c.key, &value).into_iter().filter(|i| i.starts_with('/')).collect() } else { vec![] };
    let mut body = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .px(px(16.))
        .py(px(14.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(3.))
                .child(div().font_weight(FontWeight::SEMIBOLD).text_size(px(13.5)).child(sw::label_for(&c.key)))
                .child(div().text_size(px(12.)).text_color(t.dim).child(sw::note_for(&c.key))),
        )
        .child(control);
    for p in paths {
        body = body.child(div().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(format!("{} = {}", sw::chip_label(&p), tilde(&p))));
    }
    div()
        .flex()
        .flex_col()
        .rounded(px(10.))
        .border_1()
        .border_color(if current { t.line } else { t.need })
        .bg(t.panel)
        .overflow_hidden()
        .child(strip)
        .child(body)
        .into_any_element()
}

/// The setting's control showing `value`; read-only while showing the current value.
fn control(t: &Theme, n: &NeedsYou, c: &SettingChange, value: &Value, current: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let key = c.key.as_str();
    let spec = setting(key);
    if CHIP_KEYS.contains(&key) {
        return chips(t, n, c, value, current, cx);
    }
    match spec.map(|s| s.ty) {
        Some(SettingKind::Bool) => {
            let on = value.as_bool().unwrap_or(false);
            let allow = key.starts_with("agents.may") || key == "approve.from_cli";
            let text = match (on, allow) {
                (true, true) => "allowed",
                (false, true) => "ask first",
                (true, false) => "on",
                (false, false) => "off",
            };
            let id = n.id.clone();
            div()
                .id("setting-switch")
                .flex()
                .items_center()
                .gap(px(10.))
                .when(!current, |d| d.cursor_pointer().on_click(cx.listener(move |m, _, _, cx| edit(m, &id, json!(!on), cx))))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .w(px(34.))
                        .h(px(20.))
                        .px(px(2.))
                        .rounded(px(10.))
                        .when(on, |d| d.justify_end().bg(t.accent))
                        .when(!on, |d| d.bg(t.raised).border_1().border_color(t.line))
                        .child(div().size(px(16.)).rounded_full().bg(if on { t.accent_fg } else { t.dim })),
                )
                .child(div().text_color(if on { t.fg } else { t.dim }).child(text))
                .into_any_element()
        }
        Some(SettingKind::Enum { options, .. }) => {
            let now = value.as_str().unwrap_or("").to_string();
            let mut opts: Vec<String> = options.iter().map(|o| o.to_string()).collect();
            if !now.is_empty() && !opts.contains(&now) {
                opts.push(now.clone());
            }
            let mut row = div().flex().flex_wrap().gap(px(6.));
            for (i, o) in opts.into_iter().enumerate() {
                let on = o == now;
                let label = if o.starts_with('/') { sw::chip_label(&o) } else { sw::option_label(key, &o) };
                let id = n.id.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("setting-opt-{i}")))
                        .h(px(26.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .rounded(px(7.))
                        .border_1()
                        .text_size(px(12.))
                        .map(|d| if on { d.bg(t.accent).border_color(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD) } else { d.bg(t.raised).border_color(t.line).text_color(t.dim) })
                        .when(!current && !on, |d| d.cursor_pointer().hover(|s| s.text_color(t.fg)).on_click(cx.listener(move |m, _, _, cx| edit(m, &id, json!(o), cx))))
                        .child(label),
                );
            }
            row.into_any_element()
        }
        _ => {
            // a list one item a line, anything else as typed
            let lines: Vec<String> = match value {
                Value::Array(a) => a.iter().map(sw::value_text).collect(),
                v => vec![sw::value_text(v)],
            };
            let mut block = div().flex().flex_col().px(px(10.)).py(px(8.)).rounded(px(7.)).bg(t.term).font_family(t.mono_font.clone()).text_size(px(12.));
            for l in lines {
                block = block.child(div().child(if l.is_empty() { "(empty)".to_string() } else { tilde(&l) }));
            }
            block.into_any_element()
        }
    }
}

/// Settings' toggle chips, on ones first. Next to today's value, a chip the ask turns on is
/// green ("new") and one it turns off is red ("was on"). Click turns one on or off; drag an
/// on one to move it.
fn chips(t: &Theme, n: &NeedsYou, c: &SettingChange, value: &Value, current: bool, cx: &mut Context<MainWindow>) -> AnyElement {
    let key = c.key.clone();
    let script = sw::is_script(&key);
    let on = items_of(&key, value);
    let was = items_of(&key, &c.from);
    let options: &[&str] = match setting(&key).map(|s| s.ty) {
        _ if script => SCRIPT_PARTS,
        Some(SettingKind::ItemList { options, .. }) => options,
        _ => &[],
    };
    // on ones, then what today has that this turns off, then every other option, plus any path
    // either value has, so a dropped one can go back on
    let mut all: Vec<String> = on.clone();
    let paths = was.iter().chain(items_of(&key, &c.to).iter()).filter(|i| i.starts_with('/')).cloned().collect::<Vec<_>>();
    for i in was.iter().cloned().chain(options.iter().filter(|o| **o != "none").map(|o| o.to_string())).chain(paths) {
        if !all.contains(&i) {
            all.push(i);
        }
    }
    let mut row = div().id("setting-chips").flex().flex_wrap().gap(px(6.)).w_full();
    for (i, item) in all.into_iter().enumerate() {
        let is_on = on.contains(&item);
        let added = !current && is_on && !was.contains(&item);
        let removed = !current && !is_on && was.contains(&item);
        let path = item.starts_with('/');
        let color = match () {
            _ if added => t.ok,
            _ if removed => t.err,
            _ if is_on => t.accent_fg,
            _ => t.dim,
        };
        let (id, k, it) = (n.id.clone(), key.clone(), item.clone());
        let next = value_of(&key, &sw::toggled(&on, &item, script));
        let mut chip = div()
            .id(SharedString::from(format!("setting-chip-{i}")))
            .h(px(26.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(5.))
            .rounded(px(7.))
            .border_1()
            .text_size(px(12.))
            .whitespace_nowrap()
            .text_color(color)
            .map(|d| match () {
                _ if added => d.bg(t.ok.opacity(0.14)).border_color(t.ok).font_weight(FontWeight::BOLD),
                _ if removed => d.bg(t.err.opacity(0.08)).border_color(t.err.opacity(0.5)),
                _ if is_on => d.bg(t.accent).border_color(t.accent).font_weight(FontWeight::BOLD),
                _ => d.bg(t.raised).border_color(t.line),
            })
            .when(path, |d| d.child(Icon::Code.el(11., color)))
            .child(sw::chip_label(&item))
            .when(added, |d| d.child(div().text_size(px(11.)).font_weight(FontWeight::NORMAL).text_color(t.dim).child(if path { "new script" } else { "new" })))
            .when(removed, |d| d.child(div().text_size(px(11.)).text_color(t.dim).child("was on")));
        if !current {
            chip = chip.cursor_pointer().hover(|s| s.border_color(t.dim)).on_click(cx.listener(move |m, _, _, cx| edit(m, &id, next.clone(), cx)));
            if is_on && on.len() > 1 {
                let (id, to) = (n.id.clone(), c.to.clone());
                chip = chip
                    .cursor_grab()
                    .on_drag(ChipDrag(n.id.clone(), item.clone()), |_, _, _, cx| cx.new(|_| NoGhost))
                    .on_drag_move(cx.listener(move |m, ev: &DragMoveEvent<ChipDrag>, _, cx| {
                        let ChipDrag(did, dragged) = ev.drag(cx);
                        if *did == id && *dragged != it && ev.bounds.contains(&ev.event.position) {
                            let now = items_of(&k, &m.stack.edits.get(&id).cloned().unwrap_or_else(|| to.clone()));
                            let next = value_of(&k, &sw::moved(&now, dragged, &it));
                            edit(m, &id, next, cx);
                        }
                    }));
            }
        }
        row = row.child(chip);
    }
    row.on_drop(cx.listener(|_, _: &ChipDrag, _, cx| cx.notify())).into_any_element()
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{items_of, summary, title, value_of};
    use crate::model::{NeedsYou, SettingChange};
    use serde_json::{Value, json};

    fn ask(key: &str, from: Value, to: Value) -> NeedsYou {
        NeedsYou { setting: Some(SettingChange { key: key.into(), from, to }), ..Default::default() }
    }

    #[test]
    fn titles_name_the_setting() {
        assert_eq!(title(&ask("ui.status.items", json!([]), json!([]))).as_deref(), Some("Change “Status bar items”"));
        assert_eq!(title(&NeedsYou::default()), None);
    }

    #[test]
    fn summaries_say_what_changes() {
        let n = ask("ui.status.items", json!(["daemon", "awake", "keys"]), json!(["daemon", "/x/ci.sh", "keys"]));
        assert_eq!(summary(&n).as_deref(), Some("Adds ci.sh · Removes Awake"));
        assert_eq!(summary(&ask("ui.status.items", json!(["a", "b"]), json!(["b", "a"]))).as_deref(), Some("Reorders it"));
        assert_eq!(summary(&ask("approve.from_cli", json!(false), json!(true))).as_deref(), Some("Turns it on"));
        assert_eq!(summary(&ask("needs_you.expire_hours", json!(0), json!(12))).as_deref(), Some("Sets it to 12"));
    }

    #[test]
    fn scripts_round_trip_as_parts() {
        assert_eq!(items_of("ui.row.script", &json!("worktree+branch")), vec!["worktree", "branch"]);
        assert_eq!(value_of("ui.row.script", &[]), json!("none"));
        assert_eq!(value_of("ui.status.items", &["daemon".to_string()]), json!(["daemon"]));
    }
}
