//! Settings ▸ Agents ▸ Clean up after a terminal closes (canvas
//! https://claude.ai/artifact/TzgiyN4nhvrRUw71jSsEt6, option A): `cleanup.items` as one list
//! (the built-ins with a switch, your own items with ×, drag a row to reorder, a field and Add
//! underneath), `cleanup.keep` as chips with × and a field, and the last few runs.
use super::*;
use midna_proto::cleanup::{self as cl, CleanupRun, CleanupState};

const RUNS: u32 = 5;

pub(super) struct State {
    new_item: LineInput,
    new_keep: LineInput,
    /// `cleanup.runs`, newest first.
    pub runs: Vec<CleanupRun>,
    /// `cleanup.items` while a row is being dragged; saved on drop.
    live: Option<Vec<String>>,
}

impl State {
    pub fn new(cx: &mut App) -> State {
        State {
            new_item: LineInput::new(cx, false, "Add your own, e.g. “stop the docker compose stack started here”"),
            new_keep: LineInput::new(cx, false, "+ branch or folder"),
            runs: vec![],
            live: None,
        }
    }
}

/// `cleanup.runs` for the list at the bottom of the group.
pub(super) fn fetch_runs(backend: &Arc<dyn Backend>) -> Vec<CleanupRun> {
    backend.call("cleanup.runs", json!({ "limit": RUNS })).ok().and_then(|v| serde_json::from_value::<midna_proto::CleanupRunsResult>(v).ok()).map(|r| r.runs).unwrap_or_default()
}

/// A row of the items list being dragged.
struct ItemDrag(String);

struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

fn small_x(t: &Theme, id: SharedString, label: String) -> Stateful<Div> {
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

fn add_button(t: &Theme, id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .h(px(26.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(4.))
        .rounded(px(7.))
        .text_size(px(12.))
        .font_weight(FontWeight::BOLD)
        .bg(t.accent)
        .text_color(t.accent_fg)
        .cursor_pointer()
        .child(Icon::Plus.el(11., t.accent_fg))
        .child("Add")
}

impl SettingsWindow {
    fn cleanup_items(&self) -> Vec<String> {
        self.cleanup.live.clone().unwrap_or_else(|| strings(&self.value("cleanup.items")))
    }

    fn save_cleanup(&mut self, key: &str, value: Vec<String>, cx: &mut Context<Self>) {
        self.cleanup.live = None;
        // shown at once; the reload that follows the save confirms it
        if let Some(e) = self.entries.iter_mut().find(|e| e.key == key) {
            e.value = json!(value);
        }
        self.set(key, json!(value), cx);
    }

    fn add_cleanup_item(&mut self, cx: &mut Context<Self>) {
        let text = self.cleanup.new_item.text(cx).trim().to_string();
        if text.is_empty() {
            return;
        }
        let mut items = self.cleanup_items();
        items.push(text);
        self.cleanup.new_item.clear(cx);
        self.save_cleanup("cleanup.items", cl::normalize_items(items.iter().map(String::as_str)), cx);
    }

    fn add_keep(&mut self, cx: &mut Context<Self>) {
        let text = self.cleanup.new_keep.text(cx).trim().to_string();
        if text.is_empty() {
            return;
        }
        let mut keep = strings(&self.value("cleanup.keep"));
        keep.extend(text.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty() && !keep.contains(p)).collect::<Vec<_>>());
        self.cleanup.new_keep.clear(cx);
        self.save_cleanup("cleanup.keep", keep, cx);
    }

    /// `cleanup.items`: what's on in its order, then the built-ins that are off. Built-ins have
    /// a switch, your own items an ×; drag an item that's on to move it.
    pub(super) fn cleanup_items_control(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let items = self.cleanup_items();
        let off = cl::BUILTINS.iter().filter(|b| !items.iter().any(|i| i == *b)).map(|b| b.to_string());
        let mut list = div().flex().flex_col().gap(px(6.)).w_full();
        for (n, item) in items.iter().cloned().chain(off).enumerate() {
            let builtin = cl::builtin_label(&item);
            let on = items.contains(&item);
            let mut row = div()
                .id(SharedString::from(format!("cleanup-item-{n}")))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(10.))
                .py(px(6.))
                .rounded(px(7.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_size(px(12.5))
                .when(!on, |d| d.opacity(0.6))
                .child(div().w(px(10.)).flex_none().text_size(px(11.)).text_color(t.dim).when(on, |d| d.child("⋮⋮")))
                .child(div().flex_1().min_w_0().text_color(t.fg).child(builtin.map(str::to_string).unwrap_or_else(|| item.clone())));
            match builtin {
                Some(_) => {
                    let (b, all) = (item.clone(), items.clone());
                    row = row
                        .child(div().flex_none().px(px(5.)).rounded(px(4.)).border_1().border_color(t.line).text_size(px(10.)).text_color(t.dim).child("BUILT IN"))
                        .child(crate::ui::screen_kit::switch(t, SharedString::from(format!("cleanup-sw-{item}")), on).on_click(cx.listener(move |s, _, _, cx| {
                            s.save_cleanup("cleanup.items", cl::set_builtin(&all, &b, !on), cx);
                        })));
                }
                None => {
                    let (gone, all) = (item.clone(), items.clone());
                    row = row.child(small_x(t, SharedString::from(format!("cleanup-x-{n}")), "Remove".into()).on_click(cx.listener(move |s, _, _, cx| {
                        let rest = all.iter().filter(|i| **i != gone).cloned().collect();
                        s.save_cleanup("cleanup.items", rest, cx);
                    })));
                }
            }
            if on {
                let target = item.clone();
                row = row
                    .cursor_grab()
                    .on_drag(ItemDrag(item.clone()), |_, _, _, cx| cx.new(|_| NoGhost))
                    .on_drag_move(cx.listener(move |s, ev: &DragMoveEvent<ItemDrag>, _, cx| {
                        let y = ev.event.position.y;
                        let dragged = ev.drag(cx).0.clone();
                        if dragged != target && ev.bounds.top() <= y && y < ev.bounds.bottom() {
                            s.cleanup.live = Some(cl::moved(&s.cleanup_items(), &dragged, &target));
                            cx.notify();
                        }
                    }))
                    .on_drop(cx.listener(|s, _: &ItemDrag, _, cx| {
                        let items = s.cleanup_items();
                        s.save_cleanup("cleanup.items", items, cx);
                    }));
            }
            list = list.child(row);
        }
        list.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    self.cleanup
                        .new_item
                        .render(t, "cleanup-new-item", window)
                        .h(px(28.))
                        .flex_1()
                        .min_w_0()
                        .on_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| match s.cleanup.new_item.on_key(ev, cx) {
                            KeyOutcome::Submit => {
                                cx.stop_propagation();
                                s.add_cleanup_item(cx);
                            }
                            KeyOutcome::Cancel => {
                                cx.stop_propagation();
                                s.cleanup.new_item.clear(cx);
                            }
                            _ => {}
                        })),
                )
                .child(add_button(t, "cleanup-add-item").on_click(cx.listener(|s, _, _, cx| s.add_cleanup_item(cx)))),
        )
        .into_any_element()
    }

    /// `cleanup.keep` as chips (× removes one) and a field (↩ adds; commas add several).
    pub(super) fn cleanup_keep_control(&self, t: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let keep = strings(&self.value("cleanup.keep"));
        let mut chips = div().flex().flex_wrap().items_center().gap(px(6.)).w_full();
        for (n, k) in keep.iter().enumerate() {
            let (gone, all) = (k.clone(), keep.clone());
            chips = chips.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(9.))
                    .pr(px(3.))
                    .py(px(2.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .child(crate::ui::screen_kit::mono(t, k.clone(), 12.).text_color(t.fg))
                    .child(small_x(t, SharedString::from(format!("cleanup-keep-x-{n}")), format!("Remove {k}")).on_click(cx.listener(move |s, _, _, cx| {
                        let rest = all.iter().filter(|i| **i != gone).cloned().collect();
                        s.save_cleanup("cleanup.keep", rest, cx);
                    }))),
            );
        }
        chips
            .child(
                self.cleanup
                    .new_keep
                    .render(t, "cleanup-new-keep", window)
                    .h(px(26.))
                    .w(px(170.))
                    .text_size(px(12.))
                    .on_key_down(cx.listener(|s, ev: &KeyDownEvent, _, cx| match s.cleanup.new_keep.on_key(ev, cx) {
                        KeyOutcome::Submit => {
                            cx.stop_propagation();
                            s.add_keep(cx);
                        }
                        KeyOutcome::Cancel => {
                            cx.stop_propagation();
                            s.cleanup.new_keep.clear(cx);
                        }
                        _ => {}
                    })),
            )
            .into_any_element()
    }

    /// The last few runs: a mark (✓ all went, ● it kept something, ✕ failed, … running), the
    /// terminal and what it did, then how long ago and what it cost.
    pub(super) fn cleanup_runs_control(&self, t: &Theme) -> AnyElement {
        let mut list = div().flex().flex_col().w_full().text_size(px(12.));
        if self.cleanup.runs.is_empty() {
            return list.child(div().text_color(t.dim).child("None yet. Closing an agent terminal that left a branch or worktree starts one.")).into_any_element();
        }
        for (n, r) in self.cleanup.runs.iter().enumerate() {
            let (mark, color) = match r.state {
                CleanupState::Running => ("…", t.dim),
                CleanupState::Failed => ("✕", t.err),
                CleanupState::Done if !r.kept.is_empty() => ("●", t.need),
                CleanupState::Done => ("✓", t.ok),
            };
            let what = match r.state {
                CleanupState::Running => "cleaning up…".to_string(),
                CleanupState::Failed => format!("failed: {}", r.error.as_deref().unwrap_or("").lines().next().unwrap_or("")),
                CleanupState::Done => {
                    let mut parts = vec![];
                    if !r.removed.is_empty() {
                        parts.push(format!("removed {}", r.removed.join(", ")));
                    }
                    if !r.kept.is_empty() {
                        parts.push(format!("kept {}", r.kept.join("; ")));
                    }
                    if parts.is_empty() { "nothing to do".to_string() } else { parts.join(". ") }
                }
            };
            let dry = if r.dry_run { " (dry run)" } else { "" };
            let mut when = crate::ui::screen_kit::ago(Some(&r.started_at));
            if let Some(c) = r.cost_usd {
                when.push_str(&format!(" · ${c:.3}"));
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("cleanup-run-{n}")))
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .py(px(6.))
                    .when(n > 0, |d| d.border_t_1().border_color(t.line))
                    .tooltip(crate::ui::header::tip(format!("midna cleanup show {}", r.id)))
                    .child(div().w(px(12.)).flex_none().text_color(color).child(mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(t.dim)
                            .child(div().text_color(t.fg).font_weight(FontWeight::BOLD).child(format!("{}{dry}", r.session_name)))
                            .child(what),
                    )
                    .child(div().flex_none().text_color(t.dim).child(when)),
            );
        }
        list.into_any_element()
    }
}
