//! Hold ⌘Q to quit: pressing ⌘Q starts a 2.5 s hold. The window dims while a ring fills around
//! a ⌘Q badge; letting go of ⌘ (or Q, or switching apps) cancels. The menu's "Quit midna"
//! still quits at once. Terminals keep running in midnad either way.
//!
//! macOS often swallows Q's key-up while ⌘ is down, so releasing ⌘ is the reliable cancel. It
//! is polled from the keyboard state every tick as well as caught as an event.
use crate::app::MainWindow;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::f32::consts::PI;
use std::time::{Duration, Instant};

pub const HOLD: Duration = Duration::from_millis(2500);

/// ⌘Q pressed (key repeats arrive as more presses and are ignored).
pub fn start(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.quit_hold.is_some() {
        return;
    }
    m.quit_hold = Some(Instant::now());
    crate::lifecycle::log(&format!("⌘Q hold started (first responder: {})", crate::composer::first_responder()));
    cx.spawn_in(window, async move |this, cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(16)).await;
            let done = this.update(cx, |m, cx| {
                if !cmd_down() {
                    cancel(m, "⌘ up (polled)", cx);
                }
                cx.notify();
                match m.quit_hold {
                    None => Some(false),
                    Some(t) if t.elapsed() >= HOLD => Some(true),
                    Some(_) => None,
                }
            });
            match done {
                Ok(None) => continue,
                Ok(Some(true)) => {
                    crate::lifecycle::log("⌘Q held 2.5 s: quitting");
                    // Quit directly rather than dispatching the action through whichever
                    // window AppKit considers active.
                    let _ = this.update(cx, |m, _| crate::backend::fake::shutdown(&*m.backend));
                    let _ = cx.update(|_, cx| {
                        crate::lifecycle::log("quit: hold complete, terminating");
                        cx.quit();
                    });
                    break;
                }
                _ => break,
            }
        }
    })
    .detach();
}

/// ⌘ is held right now, read from the keyboard state rather than events: the release event
/// can miss GPUI (another responder holding focus), and a missed release must not quit.
fn cmd_down() -> bool {
    objc2_app_kit::NSEvent::modifierFlags_class().contains(objc2_app_kit::NSEventModifierFlags::Command)
}

pub fn cancel(m: &mut MainWindow, why: &str, cx: &mut Context<MainWindow>) {
    if m.quit_hold.take().is_some() {
        crate::lifecycle::log(&format!("⌘Q hold cancelled: {why}"));
        cx.notify();
    }
}

fn ease(x: f32) -> f32 {
    // ease-in-out cubic
    if x < 0.5 { 4. * x * x * x } else { 1. - (-2. * x + 2.).powi(3) / 2. }
}

/// The overlay while ⌘Q is held.
pub fn render(m: &MainWindow, t: &Theme) -> Option<AnyElement> {
    let started = m.quit_hold?;
    let raw = (started.elapsed().as_secs_f32() / HOLD.as_secs_f32()).clamp(0., 1.);
    let p = ease(raw);
    // Fade the card in over the first 150 ms so a tap doesn't flash it.
    let appear = (started.elapsed().as_secs_f32() / 0.15).clamp(0., 1.);
    let (accent, track, fg, dim) = (t.accent, t.line, t.fg, t.dim);
    const SIZE: f32 = 132.;
    let ring = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let c = b.center();
            let r = SIZE / 2. - 8.;
            let at = |a: f32, r: f32| point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
            let circle = |from: f32, to: f32, width: f32, color: Hsla, window: &mut Window| {
                let steps = ((to - from).abs() / (2. * PI) * 120.).ceil().max(2.) as usize;
                let mut path = PathBuilder::stroke(px(width));
                path.move_to(at(from, r));
                for i in 1..=steps {
                    path.line_to(at(from + (to - from) * i as f32 / steps as f32, r));
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            circle(0., 2. * PI, 6., track, window);
            if p > 0.001 {
                let start = -PI / 2.;
                let end = start + 2. * PI * p;
                // soft glow under the arc, then the arc and a bright head
                circle(start, end, 14., accent.opacity(0.18), window);
                circle(start, end, 6., accent, window);
                // round cap where the arc begins (strokes end square)
                let tail = at(start, r);
                let tail_glow = gpui_kit::point(tail.x - px(7.), tail.y);
                window.paint_quad(fill(Bounds::new(tail_glow - point(px(0.), px(7.)), size(px(7.), px(14.))), accent.opacity(0.18)).corner_radii(Corners {
                    top_left: px(7.),
                    bottom_left: px(7.),
                    ..Default::default()
                }));
                window.paint_quad(fill(Bounds::centered_at(tail, size(px(6.), px(6.))), accent).corner_radii(px(3.)));
                let head = at(end, r);
                let glow = Bounds::centered_at(head, size(px(18.), px(18.)));
                window.paint_quad(fill(glow, accent.opacity(0.35)).corner_radii(px(9.)));
                let dot = Bounds::centered_at(head, size(px(9.), px(9.)));
                window.paint_quad(fill(dot, gpui_kit::white()).corner_radii(px(4.5)));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();
    let badge = div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(div().font_family(t.mono_font.clone()).text_size(px(22. + 4. * p)).font_weight(FontWeight::BOLD).text_color(fg.opacity(0.7 + 0.3 * p)).child("⌘Q"));
    Some(
        div()
            .id("quit-hold")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.62 * p.max(0.12 * appear)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(14.))
                    .px(px(36.))
                    .py(px(28.))
                    .rounded(px(18.))
                    .bg(t.panel.opacity(0.92 * appear))
                    .border_1()
                    .border_color(t.line.opacity(appear))
                    .shadow(vec![BoxShadow { color: accent.opacity(0.25 * p), offset: point(px(0.), px(0.)), blur_radius: px(48.), spread_radius: px(2.), inset: false }])
                    .opacity(appear)
                    .child(div().relative().size(px(SIZE)).child(ring).child(badge))
                    .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(fg).child(if raw >= 1. { "Quitting midna…" } else { "Keep holding to quit" }))
                    .child(div().text_size(px(12.)).text_color(dim).child("Let go to cancel · your terminals keep running")),
            )
            .into_any_element(),
    )
}
