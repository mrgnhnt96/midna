//! The image sheet (design: "D · Sheet + image strip") and the "added, not sent yet" tray
//! under the terminal. State and behavior live in `crate::annotate`.
use crate::annotate::{self as an, Mark, Outbox, Tool};
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::theme::{Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::*;

const STRIP_W: f32 = 128.;
const THUMB_W: f32 = 104.;
const THUMB_H: f32 = 65.;
const RAIL_W: f32 = 300.;

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

pub fn render(m: &MainWindow, t: &Theme, window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let scrim = if t.mode == ThemeMode::Dark { hsla(228. / 360., 0.33, 0.03, 0.62) } else { hsla(228. / 360., 0.23, 0.15, 0.32) };
    let a = &m.annot;
    let has_images = a.draft().is_some_and(|d| !d.shots.is_empty());
    let attach_key = m.key_label("keys.approve");

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
        .child(button(t, "annot-close", "Close", false).child(kbd(t, "esc", false)).on_click(cx.listener(|m, _, w, cx| an::close(m, w, cx))))
        .child(
            button(t, "annot-attach", "Add to chat", true)
                .when(!has_images, |d| d.opacity(0.5))
                .child(kbd(t, attach_key, true))
                .on_click(cx.listener(|m, _, w, cx| an::attach(m, w, cx))),
        );

    let body: AnyElement = if has_images {
        div().flex().flex_1().min_h_0().child(strip(m, t, cx)).child(stage(m, t, window, cx)).child(rail(m, t, cx)).into_any_element()
    } else {
        empty(m, t, cx).into_any_element()
    };

    let dialog = div()
        .id("annotate")
        .key_context("MidnaOverlay")
        .track_focus(&a.focus)
        .on_key_down(cx.listener(an::on_key))
        .on_action(cx.listener(|m, _: &crate::actions::Dismiss, w, cx| {
            if m.annot.editing.is_some() {
                an::commit(m, w, cx);
            } else {
                an::close(m, w, cx);
            }
        }))
        .on_action(cx.listener(|m, _: &crate::actions::ApproveOnce, w, cx| an::attach(m, w, cx)))
        .on_action(cx.listener(|_, _: &an::AddImage, _, _| {}))
        .on_drop(cx.listener(|m, paths: &ExternalPaths, w, cx| an::add(m, paths.paths().iter().cloned().map(an::Source::Path).collect(), w, cx)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
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
        .p(px(36.))
        .bg(scrim)
        .on_mouse_down(MouseButton::Left, cx.listener(|m, _, w, cx| an::close(m, w, cx)))
        .child(dialog)
}

fn empty(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let loading = m.annot.loading > 0;
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
                .child(button(t, "annot-paste", "Paste image", false).child(kbd(t, "⌘V", false)).on_click(cx.listener(|m, _, w, cx| {
                    let s = an::clipboard_sources(cx);
                    if s.is_empty() {
                        m.toast("No image on the clipboard.", cx);
                    }
                    an::add(m, s, w, cx);
                })))
                .child(button(t, "annot-choose", "Choose file…", false).on_click(cx.listener(|m, _, w, cx| an::pick_files(m, w, cx)))),
        )
        .child(div().text_size(px(12.)).text_color(t.dim).child("PNG, JPEG, HEIC, TIFF, and other macOS image formats"))
}

// ------------------------------------------------------------------ left: the image strip

fn strip(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let a = &m.annot;
    let shots = a.draft().map(|d| d.shots.clone()).unwrap_or_default();
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
                .on_click(cx.listener(move |m, _, w, cx| an::select_image(m, i, w, cx)))
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
                            .tooltip(|_, cx| cx.new(|_| crate::ui::header::Tip("Remove image".into())).into())
                            .on_click(cx.listener(move |m, _, w, cx| {
                                cx.stop_propagation();
                                an::remove_image(m, i, w, cx);
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
            .on_click(cx.listener(|m, _, w, cx| an::pick_files(m, w, cx)))
            .child(Icon::Plus.el(14., t.dim))
            .child(if a.loading > 0 { "Reading…" } else { "Paste or drop" })
            .child(kbd(t, "⌘V", false)),
    )
}

// ------------------------------------------------------------------ middle: the image

fn stage(m: &MainWindow, t: &Theme, _window: &mut Window, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let a = &m.annot;
    let Some(shot) = a.shot().cloned() else {
        return div().flex_1().into_any_element();
    };
    let scale = an::current_scale(m);
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
            .on_click(cx.listener(move |m, _, _, cx| {
                m.annot.tool = which;
                cx.notify();
            }))
            .child(icon.el(14., if on { t.accent } else { t.dim }))
            .child(label)
            .child(kbd(t, key, false))
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
            .on_click(cx.listener(move |m, _, _, cx| an::zoom(m, by, cx)))
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
        .child(div().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.dim).whitespace_nowrap().overflow_hidden().text_ellipsis().min_w_0().child(format!("{} · {}×{}", shot.name, shot.w, shot.h)))
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
                    cx.listener(move |m, _, w, cx| {
                        cx.stop_propagation();
                        an::edit(m, i, w, cx);
                    }),
                )
                .child(badge(t, i + 1, size).border_2().border_color(if selected { t.accent_fg } else { white() }).shadow(vec![drop()]))
                .into_any_element(),
        );
    }
    if an::drag_is_area(m)
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
        .on_mouse_down(MouseButton::Left, cx.listener(|m, ev: &MouseDownEvent, w, cx| an::pointer_down(m, ev.position, w, cx)))
        .on_mouse_move(cx.listener(|m, ev: &MouseMoveEvent, _, cx| {
            if ev.pressed_button == Some(MouseButton::Left) {
                an::pointer_move(m, ev.position, cx);
            }
        }))
        .on_mouse_up(MouseButton::Left, cx.listener(|m, ev: &MouseUpEvent, w, cx| an::pointer_up(m, ev.position, w, cx)))
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

fn rail(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let a = &m.annot;
    let Some(shot) = a.shot().cloned() else {
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
            div().flex().flex_col().gap(px(6.)).child(div().min_h(px(20.)).child(a.field.clone())).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(11.))
                    .text_color(t.dim)
                    .child(div().flex_1().child("↩ save · esc done"))
                    .child(
                        div()
                            .id(("annot-note-remove", i))
                            .cursor_pointer()
                            .tooltip(|_, cx| cx.new(|_| crate::ui::header::Tip("Remove note".into())).into())
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(cx.listener(move |m, _, w, cx| {
                                m.annot.editing = None;
                                an::remove_note(m, i, cx);
                                m.annot.focus.focus(w, cx);
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
                .on_click(cx.listener(move |m, _, w, cx| an::edit(m, i, w, cx)))
                .child(badge(t, i + 1, 22.))
                .child(div().flex().flex_col().flex_1().min_w_0().gap(px(2.)).child(text).children(kind)),
        );
    }

    let d = a.draft().cloned().unwrap_or_default();
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
                .on_click(cx.listener(|m, _, _, cx| {
                    m.annot.show_text = !m.annot.show_text;
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
        .w(px(RAIL_W))
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

/// Under the terminal while the selected terminal has an attachment waiting for ↩.
pub fn tray(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let id = m.selected.clone()?;
    let out = cx.try_global::<Outbox>()?.0.get(&id)?.clone();
    let n = out.paths.len();
    let title = format!("{n} image{} · {} note{}", if n == 1 { "" } else { "s" }, out.notes, if out.notes == 1 { "" } else { "s" });
    let mut thumbs = div().flex().flex_none().gap(px(6.));
    for (i, im) in out.thumbs.iter().take(4).enumerate() {
        thumbs = thumbs.child(div().id(("tray-thumb", i)).w(px(64.)).h(px(40.)).rounded(px(4.)).overflow_hidden().border_1().border_color(t.line).child(img(im.clone()).size_full().object_fit(ObjectFit::Cover)));
    }
    let rid = id.clone();
    Some(
        div()
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
                    .on_click(cx.listener(|m, _, w, cx| an::open(m, w, cx)))
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
                    .on_click(cx.listener(move |m, _, w, cx| {
                        an::remove(m, &rid, cx);
                        m.focus_terminal(w, cx);
                    }))
                    .child("Remove"),
            )
            .into_any_element(),
    )
}
