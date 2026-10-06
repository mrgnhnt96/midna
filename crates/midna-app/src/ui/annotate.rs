//! The image sheet (design: "D · Sheet + image strip") and the "added, not sent yet" tray
//! under the terminal. State and behavior live in `crate::annotate`; this draws `AnnotateView`.
use crate::annotate::{self as an, AnnotateView, Mark, Outbox, Tool};
use crate::icons::Icon;
use crate::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;

const STRIP_W: f32 = 128.;
const THUMB_W: f32 = 104.;
const THUMB_H: f32 = 65.;
const RAIL_W: f32 = 300.;
const RAIL_W_NARROW: f32 = 220.;
/// Below this window width (a pop-out) the sheet tightens: less margin, a narrower notes rail,
/// tool buttons without their keys.
const NARROW: f32 = 960.;

fn narrow(window: &Window) -> bool {
    f32::from(window.viewport_size().width) < NARROW
}

fn kbd(t: &Theme, s: impl Into<SharedString>, on_accent: bool) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .font_family(t.mono_font.clone())
        .text_size(px(11.))
        .text_color(if on_accent { t.accent_fg } else { t.dim })
        .child(s.into())
}

fn button(t: &Theme, id: &'static str, label: &str, primary: bool) -> Stateful<Div> {
    let raised = t.raised;
    let d = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(8.))
        .h(px(34.))
        .px(px(14.))
        .rounded(px(8.))
        .cursor_pointer()
        .whitespace_nowrap()
        .child(label.to_string());
    if primary {
        d.bg(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD).hover(|s| s.opacity(0.92))
    } else {
        d.border_1().border_color(t.line).hover(move |s| s.bg(raised))
    }
}

/// Numbered note badge.
fn badge(t: &Theme, n: usize, size: f32) -> Div {
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(t.accent)
        .text_color(t.accent_fg)
        .text_size(px(if size > 23. { 13. } else { 12. }))
        .font_weight(FontWeight::BOLD)
        .child(n.to_string())
}

impl Render for AnnotateView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.global::<Theme>().clone();
        sheet(self, &t, window, cx)
    }
}

fn sheet(a: &AnnotateView, t: &Theme, window: &mut Window, cx: &mut Context<AnnotateView>) -> impl IntoElement + use<> {
    let scrim = if t.mode == ThemeMode::Dark { hsla(228. / 360., 0.33, 0.03, 0.62) } else { hsla(228. / 360., 0.23, 0.15, 0.32) };
    let has_images = a.draft(cx).is_some_and(|d| !d.shots.is_empty());
    let attach_key = crate::actions::label(cx, "keys.approve");

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(12.))
        .h(px(64.))
        .pl(px(22.))
        .pr(px(16.))
        .border_b_1()
        .border_color(t.line)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(div().text_size(px(16.)).font_weight(FontWeight::BOLD).child("Add image"))
                .child(div().text_size(px(12.)).text_color(t.dim).child("Click to pin a note, drag to box an area.")),
        )
        .child(button(t, "annot-close", "Close", false).child(kbd(t, "esc", false)).on_click(cx.listener(|v, _, w, cx| v.close(w, cx))))
        .child(
            button(t, "annot-attach", "Add to chat", true)
                .when(!has_images, |d| d.opacity(0.5))
                .child(kbd(t, attach_key, true))
                .on_click(cx.listener(|v, _, w, cx| v.attach(w, cx))),
        );

    let body: AnyElement = if has_images {
        div().flex().flex_1().min_h_0().child(strip(a, t, cx)).child(stage(a, t, window, cx)).child(rail(a, t, narrow(window), cx)).into_any_element()
    } else {
        empty(a, t, cx).into_any_element()
    };

    let dialog = div()
        .id("annotate")
        .key_context("MidnaOverlay")
        .track_focus(&a.focus)
        .capture_key_down(cx.listener(AnnotateView::capture_key))
        .on_key_down(cx.listener(AnnotateView::on_key))
        .on_action(cx.listener(|v, _: &crate::actions::Dismiss, w, cx| {
            if v.editing.is_some() {
                v.commit(w, cx);
            } else {
                v.close(w, cx);
            }
        }))
        .on_action(cx.listener(|v, _: &crate::actions::ApproveOnce, w, cx| v.attach(w, cx)))
        .on_action(cx.listener(|_, _: &an::AddImage, _, _| {}))
        .on_drop(cx.listener(|v, paths: &ExternalPaths, w, cx| v.add(paths.paths().iter().cloned().map(an::Source::Path).collect(), w, cx)))
        // A click anywhere but a note card (they stop the event) finishes the note being edited.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|v, _, w, cx| {
                cx.stop_propagation();
                v.commit(w, cx);
            }),
        )
        .size_full()
        .max_w(px(1240.))
        .flex()
        .flex_col()
        .rounded(px(14.))
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
        .overflow_hidden()
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.45), offset: point(px(0.), px(24.)), blur_radius: px(80.), spread_radius: px(0.), inset: false }])
        .child(header)
        .child(body);

    div()
        .id("annotate-scrim")
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .justify_center()
        .p(px(if narrow(window) { 12. } else { 36. }))
        .bg(scrim)
        // the terminal under it isn't hovered, so a file dragged over the sheet stays here
        .occlude()
        .on_mouse_down(MouseButton::Left, cx.listener(|v, _, w, cx| v.close(w, cx)))
        .child(dialog)
}

fn empty(a: &AnnotateView, t: &Theme, cx: &mut Context<AnnotateView>) -> impl IntoElement + use<> {
    let loading = a.loading > 0;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(14.))
        .bg(t.term)
        .child(Icon::Image.el(56., t.accent))
        .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).child(if loading { "Reading image…" } else { "Paste, choose, or drop an image" }))
        .child(
            div()
                .flex()
                .gap(px(10.))
                .child(button(t, "annot-paste", "Paste image", false).child(kbd(t, "⌘V", false)).on_click(cx.listener(|v, _, w, cx| v.paste(w, cx))))
                .child(button(t, "annot-choose", "Choose file…", false).on_click(cx.listener(|v, _, w, cx| v.pick_files(w, cx)))),
        )
        .child(div().text_size(px(12.)).text_color(t.dim).child("PNG, JPEG, HEIC, TIFF, and other macOS image formats"))
}

// ------------------------------------------------------------------ left: the image strip

fn strip(a: &AnnotateView, t: &Theme, cx: &mut Context<AnnotateView>) -> impl IntoElement + use<> {
    let shots = a.draft(cx).map(|d| d.shots.clone()).unwrap_or_default();
    let mut col = div().id("annot-strip").flex().flex_col().flex_none().w(px(STRIP_W)).h_full().gap(px(14.)).px(px(12.)).py(px(16.)).border_r_1().border_color(t.line).overflow_y_scroll();
    for (i, s) in shots.iter().enumerate() {
        let on = i == a.cur;
        let count = s.notes.iter().filter(|n| !n.text.trim().is_empty()).count();
        col = col.child(
            div()
                .id(("annot-thumb", i))
                .relative()
                .flex()
                .flex_col()
                .flex_none()
                .gap(px(4.))
                .cursor_pointer()
                .on_click(cx.listener(move |v, _, w, cx| v.select_image(i, w, cx)))
                .child(
                    div()
                        .w(px(THUMB_W))
                        .h(px(THUMB_H))
                        .rounded(px(6.))
                        .overflow_hidden()
                        .bg(t.term)
                        .border_2()
                        .border_color(if on { t.accent } else { t.line })
                        .child(img(s.image.clone()).size_full().object_fit(ObjectFit::Cover)),
                )
                .child(div().text_size(px(11.)).text_color(if on { t.fg } else { t.dim }).overflow_hidden().text_ellipsis().whitespace_nowrap().child(s.name.clone()))
                .when(count > 0, |d| {
                    d.child(
                        div()
                            .absolute()
                            .top(px(-6.))
                            .right(px(-4.))
                            .min_w(px(18.))
                            .h(px(18.))
                            .px(px(4.))
                            .rounded(px(9.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .map(|d| if on { d.bg(t.accent).text_color(t.accent_fg) } else { d.bg(t.raised).border_1().border_color(t.line) })
                            .child(count.to_string()),
                    )
                })
                .when(on, |d| {
                    d.child(
                        div()
                            .id(("annot-thumb-remove", i))
                            .absolute()
                            .top(px(4.))
                            .left(px(4.))
                            .size(px(20.))
                            .rounded_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(hsla(0., 0., 0., 0.6))
                            .cursor_pointer()
                            .tooltip(crate::ui::header::tip("Remove image"))
                            .on_click(cx.listener(move |v, _, w, cx| {
                                cx.stop_propagation();
                                v.remove_image(i, w, cx);
                            }))
                            .child(Icon::Cross.el(11., white())),
                    )
                }),
        );
    }
    let raised = t.raised;
    col.child(
        div()
            .id("annot-add")
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(2.))
            .w(px(THUMB_W))
            .h(px(THUMB_H))
            .rounded(px(6.))
            .border_1()
            .border_dashed()
            .border_color(t.line)
            .text_size(px(11.))
            .text_color(t.dim)
            .cursor_pointer()
            .hover(move |s| s.bg(raised))
            .on_click(cx.listener(|v, _, w, cx| v.pick_files(w, cx)))
            .child(Icon::Plus.el(14., t.dim))
            .child(if a.loading > 0 { "Reading…" } else { "Paste or drop" })
            .child(kbd(t, "⌘V", false)),
    )
}

// ------------------------------------------------------------------ middle: the image

fn stage(a: &AnnotateView, t: &Theme, window: &mut Window, cx: &mut Context<AnnotateView>) -> impl IntoElement + use<> {
    let narrow = narrow(window);
    let Some(shot) = a.shot(cx).cloned() else {
        return div().flex_1().into_any_element();
    };
    let scale = a.current_scale(cx);
    let (dw, dh) = ((shot.w as f32 * scale).max(1.), (shot.h as f32 * scale).max(1.));

    let tool = |id: &'static str, icon: Icon, label: &'static str, key: &'static str, on: bool, which: Tool| {
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(10.))
            .rounded(px(6.))
            .cursor_pointer()
            .map(|d| if on { d.bg(t.accent.opacity(0.14)).text_color(t.accent).font_weight(FontWeight::BOLD) } else { d.text_color(t.dim) })
            .on_click(cx.listener(move |v, _, _, cx| {
                v.tool = which;
                cx.notify();
            }))
            .child(icon.el(14., if on { t.accent } else { t.dim }))
            .child(label)
            .when(!narrow, |d| d.child(kbd(t, key, false)))
    };
    let zbtn = |id: &'static str, label: &'static str, by: Option<f32>| {
        let raised = t.raised;
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.))
            .min_w(px(30.))
            .px(px(8.))
            .cursor_pointer()
            .hover(move |s| s.bg(raised))
            .on_click(cx.listener(move |v, _, _, cx| v.zoom(by, cx)))
            .child(label)
    };
    let zoom_label = match a.zoom {
        None => "Fit".to_string(),
        Some(z) => format!("{:.0}%", z * 100.),
    };
    let toolbar = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .flex()
                .p(px(2.))
                .rounded(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.bg)
                .child(tool("annot-tool-pin", Icon::Pin, "Pin", "P", a.tool == Tool::Pin, Tool::Pin))
                .child(tool("annot-tool-box", Icon::Area, "Box", "B", a.tool == Tool::Area, Tool::Area)),
        )
        .when(!narrow, |d| d.child(div().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.dim).whitespace_nowrap().overflow_hidden().text_ellipsis().min_w_0().child(format!("{} · {}×{}", shot.name, shot.w, shot.h))))
        .child(div().flex_1())
        .child(
            div()
                .flex()
                .items_center()
                .rounded(px(8.))
                .border_1()
                .border_color(t.line)
                .bg(t.bg)
                .text_color(t.dim)
                .child(zbtn("annot-zoom-out", "−", Some(0.8)))
                .child(
                    div()
                        .border_l_1()
                        .border_r_1()
                        .border_color(t.line)
                        .child(zbtn("annot-zoom-fit", "", None).font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.fg).child(zoom_label)),
                )
                .child(zbtn("annot-zoom-in", "+", Some(1.25))),
        );

    // ---- the image with its marks
    let mut marks: Vec<AnyElement> = vec![];
    for (i, n) in shot.notes.iter().enumerate() {
        let selected = a.sel == Some(i) || a.editing == Some(i);
        if let Mark::Area { x0, y0, x1, y1 } = n.mark {
            marks.push(
                div()
                    .absolute()
                    .left(px(x0 * dw))
                    .top(px(y0 * dh))
                    .w(px((x1 - x0) * dw))
                    .h(px((y1 - y0) * dh))
                    .border_2()
                    .border_dashed()
                    .border_color(t.accent)
                    .rounded(px(3.))
                    .bg(t.accent.opacity(if selected { 0.22 } else { 0.12 }))
                    .into_any_element(),
            );
        }
        let (x, y) = n.mark.anchor();
        let size = if selected { 28. } else { 24. };
        marks.push(
            div()
                .id(("annot-pin", i))
                .absolute()
                .left(px(x * dw - size / 2.))
                .top(px(y * dh - size / 2.))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |v, _, w, cx| {
                        cx.stop_propagation();
                        v.edit(i, w, cx);
                    }),
                )
                .child(badge(t, i + 1, size).border_2().border_color(if selected { t.accent_fg } else { white() }).shadow(vec![drop()]))
                .into_any_element(),
        );
    }
    if a.drag_is_area()
        && let Some((p, q)) = a.drag
        && let Mark::Area { x0, y0, x1, y1 } = Mark::area(p, q)
    {
        marks.push(div().absolute().left(px(x0 * dw)).top(px(y0 * dh)).w(px((x1 - x0) * dw)).h(px((y1 - y0) * dh)).border_2().border_dashed().border_color(t.accent).bg(t.accent.opacity(0.12)).into_any_element());
    }
    let stage_cell = a.stage.clone();
    let image = div()
        .id("annot-image")
        .relative()
        .flex_none()
        .w(px(dw))
        .h(px(dh))
        .cursor_crosshair()
        .shadow(vec![ring(t.line, 1.)])
        .on_mouse_down(MouseButton::Left, cx.listener(|v, ev: &MouseDownEvent, w, cx| v.pointer_down(ev.position, w, cx)))
        .on_mouse_move(cx.listener(|v, ev: &MouseMoveEvent, _, cx| {
            if ev.pressed_button == Some(MouseButton::Left) {
                v.pointer_move(ev.position, cx);
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(|v, ev: &MouseUpEvent, w, cx| v.pointer_up(ev.position, w, cx)))
        .child(img(shot.image.clone()).size_full())
        .child(canvas(move |b, _, _| stage_cell.set(Some(b)), |_, _, _, _| {}).absolute().top_0().left_0().size_full())
        .children(marks);

    // The viewport scrolls when zoomed past the fit; its size drives the fit.
    let vp_cell = a.viewport.clone();
    let viewport = div()
        .relative()
        .flex_1()
        .min_h_0()
        .child(
            canvas(
                move |b, window, _| {
                    if vp_cell.get().map(|o| o.size) != Some(b.size) {
                        vp_cell.set(Some(b));
                        window.refresh();
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
        .child(
            div()
                .id("annot-viewport")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .overflow_scroll()
                .child(div().flex().items_center().justify_center().min_w(relative(1.)).min_h(relative(1.)).p(px(16.)).child(image)),
        );

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .gap(px(12.))
        .pt(px(14.))
        .px(px(18.))
        .pb(px(12.))
        .bg(t.term)
        .child(toolbar)
        .child(viewport)
        .child(div().flex_none().text_center().text_size(px(11.)).text_color(t.dim).child("Scroll to pan · ⌘− ⌘+ ⌘0 zoom · ⌘↑ ⌘↓ switch image"))
        .into_any_element()
}

fn ring(c: Hsla, spread: f32) -> BoxShadow {
    BoxShadow { color: c, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(spread), inset: false }
}

fn drop() -> BoxShadow {
    BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(2.)), blur_radius: px(6.), spread_radius: px(0.), inset: false }
}

// ------------------------------------------------------------------ right: notes

fn rail(a: &AnnotateView, t: &Theme, narrow: bool, cx: &mut Context<AnnotateView>) -> impl IntoElement + use<> {
    let Some(shot) = a.shot(cx).cloned() else {
        return div().into_any_element();
    };
    let mut list = div().id("annot-notes").flex().flex_col().flex_1().min_h_0().gap(px(6.)).px(px(12.)).overflow_y_scroll();
    if shot.notes.is_empty() {
        list = list.child(div().px(px(6.)).py(px(4.)).text_color(t.dim).child("Click the image to pin a note, or drag to box an area."));
    }
    for (i, n) in shot.notes.iter().enumerate() {
        let editing = a.editing == Some(i);
        let selected = editing || a.sel == Some(i);
        let raised = t.raised;
        let text: AnyElement = if editing {
            let nl = crate::actions::label(cx, "keys.note_newline");
            let hint = if nl.is_empty() { "↩ save · esc done".to_string() } else { format!("↩ save · {nl} new line · esc done") };
            div().flex().flex_col().gap(px(6.)).child(div().min_h(px(20.)).child(a.field.clone())).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(11.))
                    .text_color(t.dim)
                    .child(div().flex_1().child(hint))
                    .child(
                        div()
                            .id(("annot-note-remove", i))
                            .cursor_pointer()
                            .tooltip(crate::ui::header::tip_fixed("Remove note", "⌫"))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |v, _, w, cx| {
                                v.editing = None;
                                v.remove_note(i, cx);
                                v.focus.focus(w, cx);
                            }))
                            .child(Icon::Trash.el(14., t.dim)),
                    ),
            )
            .into_any_element()
        } else if n.text.trim().is_empty() {
            div().text_color(t.dim).child("No note yet").into_any_element()
        } else {
            div().child(n.text.clone()).into_any_element()
        };
        let kind = matches!(n.mark, Mark::Area { .. }).then(|| div().text_size(px(11.)).text_color(t.dim).child("Area"));
        list = list.child(
            div()
                .id(("annot-note", i))
                .flex()
                .flex_none()
                .gap(px(10.))
                .p(px(10.))
                .rounded(px(8.))
                .cursor_pointer()
                .map(|d| if selected { d.bg(t.raised).border_1().border_color(t.accent) } else { d.border_1().border_color(transparent_black()).hover(move |s| s.bg(raised)) })
                // Not the sheet's click-away: `edit` finishes the other note and keeps the numbering.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |v, _, w, cx| v.edit(i, w, cx)))
                .child(badge(t, i + 1, 22.))
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(text).children(kind)),
        );
    }

    let d = a.draft(cx).cloned().unwrap_or_default();
    let summary = format!("Sends as · {} image{}, {} note{}", d.shots.len(), if d.shots.len() == 1 { "" } else { "s" }, d.note_count(), if d.note_count() == 1 { "" } else { "s" });
    let open = a.show_text;
    let preview = open.then(|| {
        let shots: Vec<(&str, &[an::Note])> = d.shots.iter().map(|s| (s.name.as_str(), s.notes.as_slice())).collect();
        let mut text = d.shots.iter().map(|s| format!("[{}]", s.name)).collect::<Vec<_>>().join("\n");
        let notes = an::notes_text(&shots);
        if !notes.is_empty() {
            text.push_str("\n\n");
            text.push_str(&notes);
        }
        div()
            .id("annot-sends-text")
            .max_h(px(220.))
            .overflow_y_scroll()
            .px(px(12.))
            .pb(px(12.))
            .font_family(t.mono_font.clone())
            .text_size(px(11.))
            .line_height(px(17.))
            .text_color(t.dim)
            .child(text)
    });
    let sends = div()
        .flex()
        .flex_col()
        .flex_none()
        .m(px(12.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.line)
        .bg(t.bg)
        .child(
            div()
                .id("annot-sends")
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(10.))
                .text_size(px(12.))
                .text_color(t.dim)
                .cursor_pointer()
                .on_click(cx.listener(|v, _, _, cx| {
                    v.show_text = !v.show_text;
                    cx.notify();
                }))
                .child(div().child(if open { "▾" } else { "▸" }))
                .child(summary),
        )
        .children(preview);

    let count = shot.notes.len();
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(if narrow { RAIL_W_NARROW } else { RAIL_W }))
        .h_full()
        .border_l_1()
        .border_color(t.line)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(18.))
                .pt(px(14.))
                .pb(px(8.))
                .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(crate::ui::caps_label(t, &shot.name)))
                .child(crate::ui::caps_label(t, &format!("{count} note{}", if count == 1 { "" } else { "s" }))),
        )
        .child(list)
        .child(sends)
        .into_any_element()
}

// ------------------------------------------------------------------ the tray

/// Under a terminal pane while its terminal has an attachment waiting for ↩. Edit reopens the
/// window's sheet for it; `done` runs after Remove (refocus the terminal).
pub fn tray<V: 'static>(
    id: &str,
    t: &Theme,
    cx: &mut Context<V>,
    edit: impl Fn(&mut V, String, &mut Window, &mut Context<V>) + 'static,
    done: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Option<AnyElement> {
    let out = cx.try_global::<Outbox>()?.0.get(id)?.clone();
    let n = out.paths.len();
    let title = format!("{n} image{} · {} note{}", if n == 1 { "" } else { "s" }, out.notes, if out.notes == 1 { "" } else { "s" });
    let mut thumbs = div().flex().flex_none().gap(px(6.));
    for (i, im) in out.thumbs.iter().take(4).enumerate() {
        thumbs = thumbs.child(div().id(("tray-thumb", i)).w(px(64.)).h(px(40.)).rounded(px(4.)).overflow_hidden().border_1().border_color(t.line).child(img(im.clone()).size_full().object_fit(ObjectFit::Cover)));
    }
    let (eid, rid) = (id.to_string(), id.to_string());
    Some(
        div()
            .id(SharedString::from(format!("tray-{id}")))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(14.))
            .mx(px(14.))
            .mb(px(14.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(10.))
            .bg(t.panel)
            .border_1()
            .border_color(t.accent)
            .child(thumbs)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(div().font_weight(FontWeight::BOLD).child(title))
                    .child(div().text_size(px(12.)).text_color(t.dim).child("Goes in with your next ↩. Nothing has been typed into the terminal yet.")),
            )
            .child(
                div()
                    .id("tray-edit")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(12.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.raised)
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(move |v, _, w, cx| edit(v, eid.clone(), w, cx)))
                    .child("Edit")
                    .child(kbd(t, "⌘E", false)),
            )
            .child(
                div()
                    .id("tray-remove")
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(32.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover(|s| s.text_color(t.fg))
                    .on_click(cx.listener(move |v, _, w, cx| {
                        an::remove(&rid, cx);
                        done(v, w, cx);
                    }))
                    .child("Remove"),
            )
            .into_any_element(),
    )
}
