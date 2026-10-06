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
mod finder;
mod frame;
mod haptics;
mod icons;
mod ide;
mod install;
mod kass;
mod lifecycle;
mod model;
mod notify;
mod report;
mod settings_window;
mod sounds;
mod term_debug;
mod term_edit;
mod terminal;
mod theme;
mod ui;
mod updater;
mod windows;

use actions::*;
use gpui_kit::*;
use theme::Theme;

fn main() {
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) };
    if let Some(code) = lifecycle::headless(&std::env::args().collect::<Vec<_>>()) {
        std::process::exit(code);
    }
    let backend = backend::from_env();
    std::thread::spawn(annotate::sweep);
    let w: f32 = std::env::var("MIDNA_W").ok().and_then(|v| v.parse().ok()).unwrap_or(1280.);
    let h: f32 = std::env::var("MIDNA_H").ok().and_then(|v| v.parse().ok()).unwrap_or(800.);
    // Folders from Finder's "Open in Midna" (finder.rs) or dropped on the Dock icon; they can
    // arrive before the windows exist, so they wait in a channel.
    let (otx, orx) = async_channel::unbounded::<Vec<String>>();
    let app = gpui_kit::application().with_assets(icons::Assets);
    app.on_open_urls(move |urls| drop(otx.try_send(urls)));
    app.run(move |cx| {
        let (ui_font, mono_font) = theme::load_fonts(cx);
        let system_dark = matches!(cx.window_appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
        cx.set_global(Theme::from_def(&theme::startup_def(&backend, system_dark), ui_font, mono_font));
        bind_keys(cx, |_| None);
        // First-launch install / login item / CLI link / auto-update (dev mode: spawn midnad).
        lifecycle::start(backend.clone(), cx);
        // A trackpad tap on every click of something clickable (haptics.rs, `ui.haptics`).
        haptics::start();
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
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        // ⌘⇧N: another main window, empty until you open something in it.
        let b = backend.clone();
        cx.on_action(move |_: &NewWindow, cx| {
            let _ = windows::open(b.clone(), None, cx);
        });
        cx.set_menus(vec![Menu {
            name: "midna".into(),
            items: vec![
                MenuItem::action("New Window", NewWindow),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Hide midna", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Close", CloseWindow),
                MenuItem::action("Quit midna", Quit),
            ],
            disabled: false,
        }]);
        // The windows, places and terminals from last time (or one centered window).
        let fallback = Bounds::centered(None, size(px(w), px(h)), cx);
        let handle = windows::restore(backend.clone(), fallback, cx).expect("open main window");
        // A clicked notification shows its terminal, in whichever window has it (notify.rs).
        let (ntx, nrx) = async_channel::unbounded::<notify::Clicked>();
        notify::start(ntx);
        cx.spawn(async move |cx| {
            while let Ok(c) = nrx.recv().await {
                cx.update(|cx| {
                    cx.activate(true);
                    windows::reveal_need(c.session, c.needs_you, cx);
                });
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            while let Ok(urls) = orx.recv().await {
                cx.update(|cx| urls.iter().filter_map(|u| finder::folder_of(u)).for_each(|d| finder::open(d, cx)));
            }
        })
        .detach();
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

/// Dev: render the main window offscreen to `MIDNA_SNAPSHOT` after `MIDNA_SNAPSHOT_DELAY_MS`
/// (`MIDNA_SNAPSHOT_WINDOW=last`: the window opened last instead, e.g. a pop-out).
#[cfg(feature = "snapshot")]
fn snapshot(handle: WindowHandle<app::MainWindow>, cx: &mut App) {
    let Ok(path) = std::env::var("MIDNA_SNAPSHOT") else {
        return;
    };
    let delay: u64 = std::env::var("MIDNA_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    let main: AnyWindowHandle = handle.into();
    let last = std::env::var("MIDNA_SNAPSHOT_WINDOW").is_ok_and(|v| v == "last");
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(delay)).await;
        let any = if last { cx.update(|cx| cx.windows().last().copied()).unwrap_or(main) } else { main };
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
