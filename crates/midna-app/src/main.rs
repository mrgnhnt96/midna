//! midna-app: the GPUI client for midnad. See docs/ARCHITECTURE.md ("GUI").
//!
//! Env:
//! - `MIDNA_BACKEND=fake` — in-memory fake daemon with the design's sample data.
//! - `MIDNA_HOME` / `MIDNA_SOCKET` — where to find midnad.
//! - `MIDNA_FAKE_SETTINGS="theme=light,density=compact"` — fake backend initial settings.
//! - `MIDNA_SELECT=<session id>` — initial selection; `MIDNA_W` / `MIDNA_H` — window size.
//! - `MIDNA_DEBUG_APPROVE_MENU=1`, `MIDNA_DEBUG_SCREEN=rules|triggers|insights|commands` — screenshot states.
//! - `MIDNA_NO_ACTIVATE=1` — don't take focus on launch (automated screenshots).
//! - `MIDNA_DEV=1` — dev mode even inside a bundle: no install / login item / updates; a missing
//!   midnad is spawned from next to this binary (`MIDNA_NO_SPAWN=1` to skip). See `lifecycle.rs`.
mod actions;
mod annotate;
mod app;
mod backend;
mod commands;
mod composer;
mod dev;
mod frame;
mod icons;
mod install;
mod kass;
mod lifecycle;
mod model;
mod notify;
mod settings_window;
mod term_debug;
mod term_edit;
mod terminal;
mod theme;
mod ui;
mod updater;

use actions::*;
use gpui_kit::prelude::*;
use gpui_kit::*;
use theme::{Theme, ThemeMode};

fn main() {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    if let Some(code) = lifecycle::headless(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    let backend = backend::from_env();
    std::thread::spawn(annotate::sweep);
    let w: f32 = std::env::var("MIDNA_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1280.);
    let h: f32 = std::env::var("MIDNA_H").ok().and_then(|v| v.parse().ok()).unwrap_or(800.);
    gpui_kit::application().with_assets(icons::Assets).run(move |cx| {
        let (ui_font, mono_font) = theme::load_fonts(cx);
        cx.set_global(Theme::new(ThemeMode::Dark, ui_font, mono_font));
        bind_keys(cx, |_| None);
        // First-launch install / login item / CLI link / auto-update (dev mode: spawn midnad).
        lifecycle::start(backend.clone(), cx);
        let b = backend.clone();
        cx.on_action(move |_: &Quit, cx| {
            lifecycle::log("quit: Quit action");
            backend::fake::shutdown(&*b);
            cx.quit();
        });
        // ⌘Q outside the main window (Settings, pop-outs) has no hold overlay: quit at once.
        cx.on_action(|_: &HoldToQuit, cx| cx.dispatch_action(&Quit));
        // Fallback for a window with nothing focused; each window's root also handles it.
        cx.on_action(|_: &CloseWindow, cx| {
            if let Some(w) = cx.active_window() {
                let _ = w.update(cx, |_, window, _| window.remove_window());
            }
        });
        cx.set_menus(vec![Menu {
            name: "midna".into(),
            items: vec![MenuItem::action("Settings…", OpenSettings), MenuItem::separator(), MenuItem::action("Close", CloseWindow), MenuItem::action("Quit midna", Quit)],
            disabled: false,
        }]);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(w), px(h)), cx))),
            titlebar: Some(TitlebarOptions { title: Some("midna".into()), appears_transparent: true, traffic_light_position: Some(point(px(16.), px(16.))) }),
            window_min_size: Some(size(px(720.), px(420.))),
            app_id: Some("com.mrgnhnt.midna".into()),
            // Keep rendering daemon updates at full rate while the window is in the background.
            inactive_frame_interval: None,
            // Screenshot runs float the window so it is never occluded (occluded windows stop drawing).
            kind: if crate::dev::var("MIDNA_DEBUG_ONTOP").is_ok() { WindowKind::PopUp } else { WindowKind::Normal },
            focus: std::env::var("MIDNA_NO_ACTIVATE").is_err(),
            ..Default::default()
        };
        let b = backend.clone();
        let handle = cx.open_window(opts, |window, cx| cx.new(|cx| app::MainWindow::new(b, window, cx))).expect("open main window");
        #[cfg(feature = "snapshot")]
        snapshot(handle, cx);
        let _ = handle;
        if crate::dev::var("MIDNA_DEBUG_SETTINGS").is_ok() {
            settings_window::open(backend.clone(), cx);
        }
        let b = backend.clone();
        cx.on_window_closed(move |cx, _| {
            if cx.windows().is_empty() {
                backend::fake::shutdown(&*b);
                cx.quit();
            }
        })
        .detach();
        if std::env::var("MIDNA_NO_ACTIVATE").is_err() {
            cx.activate(true);
        }
    });
}

/// Dev: render the main window offscreen to `MIDNA_SNAPSHOT` after `MIDNA_SNAPSHOT_DELAY_MS`.
#[cfg(feature = "snapshot")]
fn snapshot(handle: WindowHandle<app::MainWindow>, cx: &mut App) {
    let Ok(path) = std::env::var("MIDNA_SNAPSHOT") else {
        return;
    };
    let delay: u64 = std::env::var("MIDNA_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    let any: AnyWindowHandle = handle.into();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(delay)).await;
        // two passes so layout-dependent state (terminal size, frames) settles
        for _ in 0..2 {
            let _ = cx.update_window(any, |_, window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            cx.background_executor().timer(std::time::Duration::from_millis(400)).await;
        }
        let _ = cx.update_window(any, |_, window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
            match window.render_to_image() {
                Ok(img) => match img.save(&path) {
                    Ok(()) => eprintln!("midna-app: snapshot saved to {path}"),
                    Err(e) => eprintln!("midna-app: snapshot save failed: {e}"),
                },
                Err(e) => eprintln!("midna-app: render_to_image failed: {e:#}"),
            }
        });
        let _ = cx.update(|cx| cx.dispatch_action(&Quit));
    })
    .detach();
}
