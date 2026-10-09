//! Settings ▸ Appearance: list settings as toggle chips. Every option is a chip: the ones that
//! are on come first in their saved order (drag one to move it), then the ones that are off.
//! A click turns a chip on or off and saves at once.
//!
//! Lists (`sidebar.footer.stats`, `sidebar.footer.buttons`, `ui.header.buttons`,
//! `ui.status.items`) save as arrays; scripts (`ui.row.script`, `ui.status.script`) save their
//! built-in parts joined with `+`. A script path a user added shows as one chip; turning it off
//! keeps it here until the window closes, so it can go back on, but adding a new path stays a
//! `midna settings set` from the CLI (human only).
use super::*;
use midna_proto::settings::{SCRIPT_PARTS, is_builtin_script};

/// The settings shown as chips.
pub(super) const KEYS: &[&str] = &["sidebar.footer.stats", "sidebar.footer.buttons", "ui.header.buttons", "ui.status.items", "ui.row.script", "ui.status.script"];

#[derive(Default)]
pub(super) struct State {
    /// A setting's chips while one is being dragged; saved on drop.
    live: Option<(String, Vec<String>)>,
    /// Script paths turned off in this window, by setting, so they can go back on.
    gone: Vec<(String, String)>,
}

/// A chip being dragged: (setting, item).
struct ChipDrag(String, String);

struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

/// `ui.row.script` / `ui.status.script`: built-in parts joined with `+`, or one path.
fn is_script(key: &str) -> bool {
    key.ends_with(".script")
}

/// The most chips a setting may have on.
fn max_on(key: &str) -> Option<usize> {
    (key == "sidebar.footer.buttons").then_some(4)
}

/// A script setting's value as chips: its parts (`none` = nothing), or the path as one.
fn script_items(v: &str) -> Vec<String> {
    if !is_builtin_script(v) {
        return vec![v.trim().to_string()];
    }
    let mut out: Vec<String> = vec![];
    for p in v.split('+').map(str::trim).filter(|p| !p.is_empty() && *p != "none") {
        if !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// The chips on, back to a script setting's value.
fn script_value(items: &[String]) -> String {
    if items.is_empty() { "none".into() } else { items.join("+") }
}

/// `items` with `item` turned on (at the end) or off. A script path can't be joined with parts,
/// so in a script setting turning one on replaces everything else.
fn toggled(items: &[String], item: &str, script: bool) -> Vec<String> {
    if items.iter().any(|i| i == item) {
        return items.iter().filter(|i| *i != item).cloned().collect();
    }
    let path = item.starts_with('/');
    if script && (path || items.iter().any(|i| i.starts_with('/'))) {
        return vec![item.to_string()];
    }
    let mut out = items.to_vec();
    out.push(item.to_string());
    out
}

/// `items` with `dragged` moved to `target`'s place.
fn moved(items: &[String], dragged: &str, target: &str) -> Vec<String> {
    let mut out = items.to_vec();
    let (Some(from), Some(to)) = (out.iter().position(|i| i == dragged), out.iter().position(|i| i == target)) else { return out };
    let d = out.remove(from);
    out.insert(to, d);
    out
}

/// A chip's name: a path's file name, else the option with spaces.
fn chip_label(item: &str) -> String {
    if item.starts_with('/') {
        return item.rsplit('/').next().unwrap_or(item).to_string();
    }
    match item {
        "ide" => "IDE".into(),
        "popout" => "Pop out".into(),
        "git-diff-stats" => "Diff stats".into(),
        "pr" => "PR".into(),
        "github" => "GitHub".into(),
        _ => {
            let s = item.replace(['_', '-'], " ");
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
    }
}

impl SettingsWindow {
    /// The chips of a setting that are on, in order (while dragging, the dragged order).
    fn chips_on(&self, key: &str) -> Vec<String> {
        if let Some((k, items)) = &self.chips.live
            && k == key
        {
            return items.clone();
        }
        let v = self.value(key);
        if is_script(key) {
            return script_items(v.as_str().unwrap_or(""));
        }
        v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    /// Every chip of a setting, on ones first: `(item, label, on)`.
    pub(super) fn chips(&self, key: &str) -> Vec<(String, String, bool)> {
        let on = self.chips_on(key);
        let options: &[&str] = match self.spec_of(key).map(|s| s.ty) {
            _ if is_script(key) => SCRIPT_PARTS,
            Some(SettingKind::ItemList { options, .. }) => options,
            _ => &[],
        };
        let off = options.iter().filter(|o| **o != "none").map(|o| o.to_string());
        let gone = self.chips.gone.iter().filter(|(k, _)| k == key).map(|(_, p)| p.clone());
        let mut out: Vec<(String, String, bool)> = on.iter().map(|i| (i.clone(), chip_label(i), true)).collect();
        for i in off.chain(gone) {
            if !out.iter().any(|(o, ..)| *o == i) {
                out.push((i.clone(), chip_label(&i), false));
            }
        }
        out
    }

    fn save_chips(&mut self, key: &str, items: Vec<String>, cx: &mut Context<Self>) {
        self.chips.live = None;
        let value = if is_script(key) { json!(script_value(&items)) } else { json!(items) };
        self.set(key, value, cx);
    }

    fn toggle_chip(&mut self, key: &str, item: &str, cx: &mut Context<Self>) {
        let on = self.chips_on(key);
        let next = toggled(&on, item, is_script(key));
        // a path that goes off stays as a chip, so it can go back on
        for p in on.iter().filter(|p| p.starts_with('/') && !next.contains(p)) {
            if !self.chips.gone.iter().any(|(k, g)| k == key && g == p) {
                self.chips.gone.push((key.to_string(), p.clone()));
            }
        }
        self.save_chips(key, next, cx);
    }

    /// The chips, wrapping under the row's name. Click one to turn it on or off; drag one
    /// that's on to move it.
    pub(super) fn chips_control(&self, t: &Theme, key: String, chips: Vec<(String, String, bool)>, words: &[String], cx: &mut Context<Self>) -> AnyElement {
        let count = chips.iter().filter(|c| c.2).count();
        let full = max_on(&key).is_some_and(|m| count >= m);
        let script = is_script(&key);
        let mut row = div().id(SharedString::from(format!("chips-{key}"))).flex().flex_wrap().gap(px(6.)).w_full();
        for (n, (item, label, on)) in chips.into_iter().enumerate() {
            let path = item.starts_with('/');
            let blocked = !on && full;
            let found = !words.is_empty() && words.iter().any(|w| label.to_lowercase().contains(w.as_str()));
            let tip = match () {
                _ if blocked => "At most four; turn one off first".to_string(),
                _ if path && script => format!("Your script: {item}. Turning it on replaces the built-in parts"),
                _ if path => format!("Your script: {item}"),
                _ if on => "Click to turn off, drag to move".to_string(),
                _ => "Click to turn on".to_string(),
            };
            let (k, i) = (key.clone(), item.clone());
            let mut chip = div()
                .id(SharedString::from(format!("chip-{key}-{n}")))
                .h(px(26.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(7.))
                .text_size(px(12.))
                .whitespace_nowrap()
                .border_1()
                .map(|c| {
                    if on {
                        c.bg(t.accent).border_color(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD)
                    } else {
                        c.bg(t.raised).border_color(t.line).text_color(t.dim).when(!blocked, |c| c.hover(|s| s.text_color(t.fg).border_color(t.dim)))
                    }
                })
                .when(found && !on, |c| c.border_color(t.accent))
                .when(blocked, |c| c.opacity(0.5))
                .when(!blocked, |c| c.cursor_pointer())
                .tooltip(crate::ui::header::tip(tip))
                .on_click(cx.listener(move |s, _, _, cx| {
                    if !blocked {
                        s.toggle_chip(&k, &i, cx);
                    }
                }))
                .when(path, |c| c.child(Icon::Code.el(11., if on { t.accent_fg } else { t.dim })))
                .child(label);
            if on && count > 1 {
                let (k, target) = (key.clone(), item.clone());
                chip = chip
                    .cursor_grab()
                    .on_drag(ChipDrag(key.clone(), item.clone()), |_, _, _, cx| cx.new(|_| NoGhost))
                    .on_drag_move(cx.listener(move |s, ev: &DragMoveEvent<ChipDrag>, _, cx| {
                        let ChipDrag(dk, dragged) = ev.drag(cx);
                        if *dk == k && *dragged != target && ev.bounds.contains(&ev.event.position) {
                            let next = moved(&s.chips_on(&k), dragged, &target);
                            s.chips.live = Some((k.clone(), next));
                            cx.notify();
                        }
                    }));
            }
            row = row.child(chip);
        }
        // dropped anywhere on the row (or a chip) saves the new order
        row.on_drop(cx.listener(move |s, ChipDrag(k, _): &ChipDrag, _, cx| {
            let items = s.chips_on(k);
            s.save_chips(k, items, cx);
        }))
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{chip_label, moved, script_items, script_value, toggled};

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn scripts_read_and_write_parts() {
        assert_eq!(script_items("worktree+branch"), v(&["worktree", "branch"]));
        assert_eq!(script_items("none"), Vec::<String>::new());
        assert_eq!(script_items(""), Vec::<String>::new());
        assert_eq!(script_items("/Users/me/bin/row.sh"), v(&["/Users/me/bin/row.sh"]));
        assert_eq!(script_value(&v(&["diff", "worktree"])), "diff+worktree");
        assert_eq!(script_value(&[]), "none");
    }

    #[test]
    fn toggles_append_and_paths_stand_alone() {
        assert_eq!(toggled(&v(&["a", "b"]), "c", false), v(&["a", "b", "c"]));
        assert_eq!(toggled(&v(&["a", "b"]), "a", false), v(&["b"]));
        // a list keeps paths beside built-ins
        assert_eq!(toggled(&v(&["daemon"]), "/x.sh", false), v(&["daemon", "/x.sh"]));
        // a script is parts or one path
        assert_eq!(toggled(&v(&["diff"]), "/x.sh", true), v(&["/x.sh"]));
        assert_eq!(toggled(&v(&["/x.sh"]), "diff", true), v(&["diff"]));
        assert_eq!(toggled(&v(&["diff"]), "branch", true), v(&["diff", "branch"]));
    }

    #[test]
    fn drags_move_to_the_target() {
        assert_eq!(moved(&v(&["a", "b", "c"]), "c", "a"), v(&["c", "a", "b"]));
        assert_eq!(moved(&v(&["a", "b", "c"]), "a", "c"), v(&["b", "c", "a"]));
        assert_eq!(moved(&v(&["a", "b"]), "z", "a"), v(&["a", "b"]));
    }

    #[test]
    fn labels_read_well() {
        assert_eq!(chip_label("needs_you"), "Needs you");
        assert_eq!(chip_label("ide"), "IDE");
        assert_eq!(chip_label("/Users/me/bin/clock.sh"), "clock.sh");
    }

    #[test]
    fn every_chip_setting_is_a_list_or_script() {
        use midna_proto::settings::{SettingKind, setting};
        for k in super::KEYS {
            let s = setting(k).unwrap_or_else(|| panic!("{k} is not a setting"));
            assert!(matches!(s.ty, SettingKind::ItemList { .. }) || super::is_script(k), "{k}");
        }
    }
}
