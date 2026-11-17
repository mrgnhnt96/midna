//! midna-app: the GPUI client for midnad. See docs/ARCHITECTURE.md ("GUI").
//!
//! Env:
//! - `MIDNA_BACKEND=fake` — in-memory fake daemon with the design's sample data.
//! - `MIDNA_HOME` / `MIDNA_SOCKET` — where to find midnad.
//! - `MIDNA_FAKE_SETTINGS="theme=light,density=compact"` — fake backend initial settings.
//! - `MIDNA_SELECT=<session id>` — initial selection; `MIDNA_W` / `MIDNA_H` — window size.
//! - `MIDNA_DEBUG_APPROVE_MENU=1`, `MIDNA_DEBUG_SCREEN=rules|triggers|insights|commands|more-menu|system-upgrade|system-overload[-midna]|send-image[-drop]|commands:<query>` — screenshot states.
//! - `MIDNA_DEBUG_REMOTE=1|<name>` — also open another of my Macs' window (the fake's second Mac).
//! - `MIDNA_FAKE_LINK=finding|proving|loading` — hold that window on a connecting step;
//!   `reconnecting|offline` — its link drops 3 s in (`offline`: the pill's Mac tab goes asleep).
//! - `MIDNA_FAKE_NEEDS=<n>` / `MIDNA_FAKE_MAC_NEEDS=<n>` — keep that many needs-you items on this
//!   Mac / the Mac mini (the other Macs get none); `MIDNA_FAKE_MAC_ARRIVE=<s>` — one comes in on
//!   the Mac mini after that.
//! - `MIDNA_NO_ACTIVATE=1` — don't take focus on launch (automated screenshots).
//! - `MIDNA_DEBUG_BADGE_DOCK=1` — the needs-you pill docks in the window though midna isn't in front.
//! - `MIDNA_DEBUG_BADGE=list|card|menu|mac|switch:<ms>|tip` — once something's waiting, open this
//!   Mac's list (with its first card) or the menu; the first other Mac's list (`mac`); or this
//!   Mac's, then switch to the other Mac's and hold the slide `<ms>` in (`switch`); or rest on the
//!   pill with its tooltip up (`tip`).
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
mod list_edit;
mod local_auth;
mod mac_feed;
mod macs;
mod model;
mod notify;
mod report;
mod settings_window;
mod sounds;
mod tab_history;
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
        bind_keys(cx, |_| None, &[]);
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
        // A window with nothing focused (the overload window, a subagent's) dispatches keys to
        // its root node only, which is above the root div's own CloseWindow handler, so ⌘W
        // lands here. The window is mid-dispatch (taken out of the app), so close it after.
        // A main window's ⌘W closes a terminal or asks first (`close_tab`): never just remove it.
        cx.on_action(|_: &CloseWindow, cx| {
            let Some(w) = cx.active_window() else { return };
            if w.downcast::<app::MainWindow>().is_some() {
                return;
            }
            cx.defer(move |cx| {
                let _ = w.update(cx, |_, window, _| window.remove_window());
            });
        });
        cx.on_action(|_: &Hide, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        // ⌘⇧N: another main window, empty until you open something in it.
        let b = backend.clone();
        cx.on_action(move |_: &NewWindow, cx| {
            let _ = windows::open(b.clone(), None, cx);
        });
        // ⌃⌘1…9, the Window menu and the Mac switcher: that Mac's window (opened over a drive
        // link the first time).
        let b = backend.clone();
        cx.on_action(move |a: &SwitchMac, cx| macs::switch(&b, a.0, cx));
        let b = backend.clone();
        cx.on_action(move |_: &PairMac, cx| settings_window::open_nearby(b.clone(), cx));
        // The Window menu's Macs come from peers.list and drive.list (and follow pairing and
        // the links, see `MainWindow`).
        macs::set_menus(cx);
        macs::refresh(backend.clone(), cx);
        // This Mac's picks for its other Macs' windows (Settings → Nearby).
        theme::refresh_picks(backend.clone(), cx);
        // The windows, places and terminals from last time (or one centered window).
        let fallback = Bounds::centered(None, size(px(w), px(h)), cx);
        let handle = windows::restore(backend.clone(), fallback, cx).expect("open main window");
        // A clicked notification shows its terminal, in whichever window has it (notify.rs).
        let (ntx, nrx) = async_channel::unbounded::<notify::Clicked>();
        notify::start(ntx);
        let b = backend.clone();
        cx.spawn(async move |cx| {
            while let Ok(c) = nrx.recv().await {
                cx.update(|cx| {
                    notify::on_click(&b, c, |session, needs_you| {
                        cx.activate(true);
                        windows::reveal_need(session, needs_you, cx);
                    });
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
        // Screenshots: the fake's other Mac in its own window, in front.
        if let Ok(name) = crate::dev::var("MIDNA_DEBUG_REMOTE") {
            let name = if name == "1" { "Morgan's Mac mini".to_string() } else { name };
            windows::open_mac(&backend, Some(backend::Mac { peer: "f".repeat(64), name }), cx);
        }
        if crate::dev::var("MIDNA_DEBUG_SETTINGS").is_ok() {
            settings_window::open(backend.clone(), cx);
        }
        // The floating badge (hidden until there's something to show).
        ui::badge::init(backend.clone(), cx);
        let b = backend.clone();
        cx.on_window_closed(move |cx, _| {
            // The badge alone doesn't keep midna running.
            if cx.windows().into_iter().all(|w| ui::badge::is_badge(w, cx)) {
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
/// (`MIDNA_SNAPSHOT_WINDOW=last`: the window opened last instead, e.g. a pop-out; `badge`: the
/// needs-you pill's window).
#[cfg(feature = "snapshot")]
fn snapshot(handle: WindowHandle<app::MainWindow>, cx: &mut App) {
    let Ok(path) = std::env::var("MIDNA_SNAPSHOT") else {
        return;
    };
    let delay: u64 = std::env::var("MIDNA_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(2500);
    let main: AnyWindowHandle = handle.into();
    let which = std::env::var("MIDNA_SNAPSHOT_WINDOW").unwrap_or_default();
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(delay)).await;
        let any = match which.as_str() {
            "last" => cx.update(|cx| cx.windows().into_iter().rfind(|w| !ui::badge::is_badge(*w, cx))).unwrap_or(main),
            "badge" => cx.update(|cx| cx.windows().into_iter().find(|w| ui::badge::is_badge(*w, cx))).unwrap_or(main),
            _ => main,
        };
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
