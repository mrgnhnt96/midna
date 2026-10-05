//! Dev only: `MIDNA_DEBUG_TERM="step;step;..."` drives the main terminal pane through GPUI's
//! own event dispatch (hit testing, key bindings, the pane's handlers), so keyboard, wheel,
//! mouse, selection, links, find and split panes can be exercised and screenshotted without
//! sending input to the OS. Cell positions are grid cells of the focused/main pane.
//!
//! Steps: `wait:MS`, `key:KEYSTROKE` (e.g. `key:ctrl-c`, `key:cmd-up`), `text:STRING` (typed
//! key by key), `run:CMD` (text + enter), `paste:TEXT` (a ⌘V of TEXT, `\\n` = newline), `wheel:PIXELS` (negative = toward history), `drag:C0,R0,C1,R1[,alt]`,
//! `click:C,R[,COUNT][,cmd|shift]`, `clickxy:X,Y[,COUNT][,cmd|shift]` and `dragxy:X0,Y0,X1,Y1` and `hoverxy:X,Y` (window points), `release` (all modifiers up), `split`, `stack`, `focus:main|split`, `bench:MS`, `quit`, and with the
//! `snapshot` feature `shot:NAME` (saves `$MIDNA_SHOT_DIR/terminal-NAME.png`, default `.`).
use crate::app::MainWindow;
use crate::terminal::TerminalView;
use gpui_kit::*;
use std::time::Duration;

pub fn start(steps: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(1500)).await;
        for step in steps.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (verb, arg) = step.split_once(':').unwrap_or((step, ""));
            eprintln!("midna-app debug-term: {step}");
            if verb == "wait" {
                cx.background_executor().timer(Duration::from_millis(arg.parse().unwrap_or(300))).await;
                continue;
            }
            if verb == "bench" {
                // Draw at a 120 Hz pace for ARG ms (the screen may be locked, so nothing
                // presents on its own) and report draw cost; MIDNA_FPS=1 adds frames/s.
                let until = std::time::Instant::now() + Duration::from_millis(arg.parse().unwrap_or(3000));
                let (mut n, mut total, mut worst) = (0u32, 0f64, 0f64);
                while std::time::Instant::now() < until {
                    let t0 = std::time::Instant::now();
                    let _ = cx.update(|window, cx| {
                        window.refresh();
                        window.draw(cx).clear(cx);
                    });
                    let ms = t0.elapsed().as_secs_f64() * 1000.;
                    (n, total, worst) = (n + 1, total + ms, worst.max(ms));
                    cx.background_executor().timer(Duration::from_millis(8)).await;
                }
                eprintln!("midna-app debug-term: bench {n} draws, mean {:.2} ms, max {:.2} ms", total / n.max(1) as f64, worst);
                continue;
            }
            let (verb, arg) = (verb.to_string(), arg.to_string());
            let Some(main) = this.upgrade() else { break };
            // Not inside the main window's update: drawing (for hitboxes and shots) renders it.
            let _ = cx.update(|window, cx| run_step(&main, &verb, &arg, window, cx));
            cx.background_executor().timer(Duration::from_millis(90)).await;
        }
        eprintln!("midna-app debug-term: done");
    })
    .detach();
}

fn pane(m: &MainWindow, window: &Window, cx: &App) -> Option<Entity<TerminalView>> {
    let _ = cx;
    if let Some(s) = &m.split
        && s.view.read(cx).focus_handle().is_focused(window)
    {
        return Some(s.view.clone());
    }
    m.terminal.clone()
}

fn mods(names: &[&str]) -> Modifiers {
    Modifiers { platform: names.contains(&"cmd"), shift: names.contains(&"shift"), alt: names.contains(&"alt"), control: names.contains(&"ctrl"), function: false }
}

fn run_step(main: &Entity<MainWindow>, verb: &str, arg: &str, window: &mut Window, cx: &mut App) {
    // A fresh frame first, so hit testing and the pane's bounds are current (a locked or
    // occluded screen doesn't draw on its own).
    window.refresh();
    window.draw(cx).clear(cx);
    let m = main.read(cx);
    let nums: Vec<f32> = arg.split(',').filter_map(|s| s.trim().parse().ok()).collect();
    let words: Vec<&str> = arg.split(',').map(str::trim).filter(|s| s.parse::<f32>().is_err()).collect();
    let pane = pane(m, window, cx);
    let at = |cx: &App, c: f32, r: f32| pane.as_ref().and_then(|t| t.read(cx).grid_point(c, r));
    match verb {
        "key" => {
            if let Ok(ks) = Keystroke::parse(arg) {
                window.dispatch_keystroke(ks, cx);
            }
        }
        "text" | "run" => {
            for ch in arg.chars() {
                let key = if ch == ' ' { "space".to_string() } else { ch.to_lowercase().to_string() };
                let ks = Keystroke { modifiers: Modifiers { shift: ch.is_uppercase(), ..Default::default() }, key, key_char: Some(ch.to_string()) };
                window.dispatch_keystroke(ks, cx);
            }
            if verb == "run" {
                window.dispatch_keystroke(Keystroke::parse("enter").unwrap(), cx);
            }
        }
        "paste" => {
            // As if ⌘V pasted ARG (`\n` = newline), without touching the pasteboard.
            if let Some(t) = pane {
                let text = arg.replace("\\n", "\n");
                t.update(cx, |t, cx| t.paste_text(&text, window, cx));
            }
        }
        "wheel" => {
            let Some(p) = at(cx, 10., 5.) else { return };
            let dy = nums.first().copied().unwrap_or(-60.);
            // A trackpad gesture arrives as several small pixel deltas.
            for _ in 0..4 {
                let ev = ScrollWheelEvent { position: p, delta: ScrollDelta::Pixels(point(px(0.), px(-dy / 4.))), modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved };
                window.dispatch_event(PlatformInput::ScrollWheel(ev), cx);
            }
        }
        "drag" if nums.len() >= 4 => {
            let md = mods(&words);
            let (Some(a), Some(b)) = (at(cx, nums[0], nums[1]), at(cx, nums[2], nums[3])) else {
                return;
            };
            window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: a, modifiers: md, click_count: 1, first_mouse: false }), cx);
            for i in 1..=6 {
                let f = i as f32 / 6.;
                let p = point(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f);
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: Some(MouseButton::Left), modifiers: md }), cx);
            }
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: b, modifiers: md, click_count: 1 }), cx);
        }
        "click" if nums.len() >= 2 => {
            let md = mods(&words);
            let Some(p) = at(cx, nums[0], nums[1]) else {
                return;
            };
            for n in 1..=nums.get(2).copied().unwrap_or(1.) as usize {
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: p, modifiers: md, click_count: n, first_mouse: false }), cx);
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: p, modifiers: md, click_count: n }), cx);
            }
        }
        // Click at window coordinates in points (`clickxy:300,22,2` = double-click), for chrome
        // outside the terminal grid (header, sidebar).
        "clickxy" if nums.len() >= 2 => {
            let (p, md) = (point(px(nums[0]), px(nums[1])), mods(&words));
            for n in 1..=nums.get(2).copied().unwrap_or(1.) as usize {
                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: p, modifiers: md, click_count: n, first_mouse: false }), cx);
                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: p, modifiers: md, click_count: n }), cx);
            }
        }
        // Pointer move to a window point (onto a card over the grid, e.g. the link preview).
        "hoverxy" if nums.len() >= 2 => {
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: point(px(nums[0]), px(nums[1])), pressed_button: None, modifiers: mods(&words) }), cx);
        }
        // Drag in window points (e.g. a sidebar row to a new place).
        "dragxy" if nums.len() >= 4 => {
            let (a, b) = (point(px(nums[0]), px(nums[1])), point(px(nums[2]), px(nums[3])));
            window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: a, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }), cx);
            for i in 1..=12 {
                let f = i as f32 / 12.;
                let p = point(a.x + (b.x - a.x) * f, a.y + (b.y - a.y) * f);
                window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }), cx);
                window.refresh();
                window.draw(cx).clear(cx);
            }
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: b, modifiers: Modifiers::default(), click_count: 1 }), cx);
        }
        // Release all modifiers (e.g. let go of ⌘ during a ⌘Q hold).
        "release" => {
            window.dispatch_event(PlatformInput::ModifiersChanged(ModifiersChangedEvent { modifiers: Modifiers::default(), capslock: Default::default() }), cx);
        }
        // Right-click at a cell (opens the context menu).
        "rclick" if nums.len() >= 2 => {
            let Some(p) = at(cx, nums[0], nums[1]) else {
                return;
            };
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Right, position: p, modifiers: mods(&words), click_count: 1, first_mouse: false }),
                cx,
            );
            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Right, position: p, modifiers: mods(&words), click_count: 1 }), cx);
        }
        // Pointer move to a cell (`hover:12,3,cmd` = ⌘-hover, underlines a link).
        "hover" if nums.len() >= 2 => {
            let Some(p) = at(cx, nums[0], nums[1]) else {
                return;
            };
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: p, pressed_button: None, modifiers: mods(&words) }), cx);
        }
        "split" => main.update(cx, |m, cx| crate::ui::split::toggle(m, window, cx)),
        "stack" => main.update(cx, |m, cx| {
            if let Some(s) = m.split.as_mut() {
                s.stacked = !s.stacked;
            }
            cx.notify();
        }),
        "focus" => {
            let v = if arg == "split" { m.split.as_ref().map(|s| s.view.clone()) } else { m.terminal.clone() };
            if let Some(v) = v {
                v.read(cx).focus_handle().clone().focus(window, cx);
            }
        }
        "shot" => shot(arg, window, cx),
        "quit" => cx.quit(),
        _ => eprintln!("midna-app debug-term: unknown step {verb}:{arg}"),
    }
    window.refresh();
}

#[cfg(feature = "snapshot")]
fn shot(name: &str, window: &mut Window, cx: &mut App) {
    let dir = crate::dev::var("MIDNA_SHOT_DIR").unwrap_or_else(|_| ".".into());
    let path = format!("{dir}/terminal-{name}.png");
    window.refresh();
    window.draw(cx).clear(cx);
    match window.render_to_image() {
        Ok(img) => match img.save(&path) {
            Ok(()) => eprintln!("midna-app debug-term: saved {path}"),
            Err(e) => eprintln!("midna-app debug-term: save failed: {e}"),
        },
        Err(e) => eprintln!("midna-app debug-term: render_to_image failed: {e:#}"),
    }
}

#[cfg(not(feature = "snapshot"))]
fn shot(name: &str, _window: &mut Window, _cx: &mut App) {
    eprintln!("midna-app debug-term: shot {name} needs --features snapshot");
}
