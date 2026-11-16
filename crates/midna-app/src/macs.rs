//! The Macs a window can switch to: this one first, then my paired Macs by name, each with how
//! it stands now. The sidebar's name plate lists them (⌃⌘M, `ui::mac_switcher`) and so does the
//! Window menu, where ⌃⌘1…9 jumps straight to one. How one stands comes from this Mac's daemon:
//! `peers.list` (my Macs, and whether each is on the network) and `drive.list` (the drive links
//! this app opened, which keep trying while a Mac is away or refuses); `peers.*` and
//! `drive.state` events ask again.
use crate::backend::{Backend, Mac};
use crate::windows::MacId;
use gpui_kit::*;
use midna_proto::nearby::{DriveLink, DriveListResult, DriveState, PeersListResult};
use std::sync::Arc;

/// How a Mac stands, as far as this one can tell.
#[derive(Clone, Debug, PartialEq)]
pub enum Reach {
    /// This Mac.
    Here,
    /// On the network, or its drive link is up.
    Online,
    Connecting,
    Reconnecting,
    /// Asleep, off or off the network. `last_seen`: when its drive link was last up.
    Offline { last_seen: Option<String> },
    /// It doesn't let this Mac drive it (that Mac's Settings › Nearby says).
    NotAllowed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// None: this Mac.
    pub mac: Option<Mac>,
    pub name: String,
    pub reach: Reach,
}

impl Entry {
    pub fn id(&self) -> MacId {
        self.mac.as_ref().map(|m| m.peer.clone())
    }

    /// The line under its name in the switcher, and after it in the Window menu.
    pub fn detail(&self) -> String {
        match &self.reach {
            Reach::Here => "This Mac".into(),
            Reach::Online => "Online".into(),
            Reach::Connecting => "Connecting…".into(),
            Reach::Reconnecting => "Reconnecting…".into(),
            Reach::Offline { last_seen: Some(t) } => format!("Offline · last seen {}", crate::ui::screen_kit::ago(Some(t))),
            Reach::Offline { last_seen: None } => "Offline".into(),
            Reach::NotAllowed => "Driving not allowed · turn it on there in Settings › Nearby".into(),
        }
    }

    /// Choosing it does something: it can be driven now, or its window is open (it comes
    /// forward and says how the link stands).
    pub fn enabled(&self, window_open: bool) -> bool {
        window_open || !matches!(self.reach, Reach::Offline { .. } | Reach::NotAllowed)
    }
}

/// That Mac's refusal, as midnad words it: "Bravo doesn't let this Mac drive it: …" when it
/// refuses the link, "Bravo stopped letting this Mac drive it" when it turns driving off.
pub(crate) fn refused(reason: &str) -> bool {
    reason.contains("this Mac drive it")
}

fn reach(nearby: bool, link: Option<&DriveLink>) -> Reach {
    let Some(l) = link else {
        return if nearby { Reach::Online } else { Reach::Offline { last_seen: None } };
    };
    match l.state {
        DriveState::Connected => Reach::Online,
        _ if l.reason.as_deref().is_some_and(refused) => Reach::NotAllowed,
        DriveState::Connecting => Reach::Connecting,
        DriveState::Reconnecting => Reach::Reconnecting,
        DriveState::Offline | DriveState::Closed => Reach::Offline { last_seen: l.last_seen.clone() },
    }
}

/// This Mac, then my paired Macs by name (a contact can't be driven), each with how it stands.
pub fn entries(l: &PeersListResult, links: &[DriveLink]) -> Vec<Entry> {
    let me = Some(l.me.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| "This Mac".into());
    let mut out = vec![Entry { mac: None, name: me, reach: Reach::Here }];
    for mac in crate::windows::my_macs(l) {
        let nearby = l.paired.iter().any(|p| p.fingerprint == mac.peer && p.nearby);
        // A closed link is gone from `drive.list`; the newest open one says how it stands.
        let link = links.iter().rev().find(|k| k.peer == mac.peer && k.state != DriveState::Closed);
        out.push(Entry { name: mac.name.clone(), reach: reach(nearby, link), mac: Some(mac) });
    }
    out
}

/// The Macs now, and what the menu bar was last built from.
#[derive(Default)]
struct Macs {
    list: Option<Vec<Entry>>,
    menu: Option<MenuState>,
}
impl Global for Macs {}

/// What the Window menu shows: the Macs, which have a window open, and which one the front
/// window shows (checked).
#[derive(Clone, PartialEq, Debug)]
struct MenuState {
    list: Vec<Entry>,
    open: Vec<bool>,
    front: Option<usize>,
}

/// The Macs now (this Mac alone until its daemon answers).
pub fn list(cx: &App) -> Vec<Entry> {
    cx.try_global::<Macs>().and_then(|m| m.list.clone()).unwrap_or_else(|| vec![Entry { mac: None, name: "This Mac".into(), reach: Reach::Here }])
}

/// Where `mac` is in the list (⌃⌘ and its number, less one).
pub fn index_of(list: &[Entry], mac: &MacId) -> Option<usize> {
    list.iter().position(|e| &e.id() == mac)
}

/// Ask this Mac's daemon how my Macs stand, then update the Window menu and the open switchers.
/// This Mac's name there is the one its windows show beside another Mac's.
pub fn refresh(local: Arc<dyn Backend>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let b = local.clone();
        let r = cx
            .background_executor()
            .spawn(async move {
                let peers = local.call("peers.list", serde_json::json!({})).ok().and_then(|v| serde_json::from_value::<PeersListResult>(v).ok())?;
                let links = local.call("drive.list", serde_json::Value::Null).ok().and_then(|v| serde_json::from_value::<DriveListResult>(v).ok());
                Some((peers, links.map(|l| l.links).unwrap_or_default()))
            })
            .await;
        let Some((peers, links)) = r else { return };
        let _ = cx.update(|cx| {
            let list = entries(&peers, &links);
            if cx.try_global::<Macs>().is_none_or(|m| m.list.as_ref() != Some(&list)) {
                cx.default_global::<Macs>().list = Some(list);
                crate::windows::redraw_all(cx);
            }
            set_menus(cx);
            // Each Mac of mine followed for what waits there, window or not.
            crate::mac_feed::sync(&b, &peers, cx);
            crate::windows::set_local_name(peers.me.name, cx);
        });
    })
    .detach();
}

/// Rebuild the menu bar if the Macs, their open windows or the front window changed.
pub fn set_menus(cx: &mut App) {
    let list = list(cx);
    let (open, front) = crate::windows::mac_windows(cx);
    let state = MenuState { open: list.iter().map(|e| open.contains(&e.id())).collect(), front: front.and_then(|f| index_of(&list, &f)), list };
    if cx.try_global::<Macs>().is_some_and(|m| m.menu.as_ref() == Some(&state)) {
        return;
    }
    cx.set_menus(menus(&state));
    cx.default_global::<Macs>().menu = Some(state);
}

/// A Window-menu entry's title: its name, and why it can't be chosen when it can't.
fn menu_title(e: &Entry, enabled: bool) -> String {
    if enabled { e.name.clone() } else { format!("{} — {}", e.name, e.detail()) }
}

/// The app menu, and the Window menu: New Window, then (once another Mac is paired) every Mac
/// with ⌃⌘1…9, greyed with the reason when it can't be driven. macOS lists the open windows
/// below, by title.
fn menus(s: &MenuState) -> Vec<Menu> {
    use crate::actions::*;
    let mut window = vec![MenuItem::action("New Window", NewWindow), MenuItem::separator()];
    if s.list.len() > 1 {
        for (i, e) in s.list.iter().enumerate() {
            let enabled = e.enabled(s.open[i]);
            // macOS greys an item whose action nothing handles; `SwitchMac` is handled app-wide.
            let item = if enabled { MenuItem::action(menu_title(e, true), SwitchMac(i)) } else { MenuItem::action(menu_title(e, false), MacUnavailable(i)) };
            window.push(item.checked(s.front == Some(i)));
        }
        window.push(MenuItem::separator());
    }
    window.push(MenuItem::action("Pair Another Mac…", PairMac));
    vec![
        Menu {
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
        },
        Menu { name: "Window".into(), items: window, disabled: false },
    ]
}

/// ⌃⌘N, the Window menu or the switcher: the Nth Mac's window opens or comes forward. One that
/// can't be driven opens the switcher instead, which says why.
pub fn switch(local: &Arc<dyn Backend>, i: usize, cx: &mut App) {
    switch_at(local, i, None, cx);
}

/// ⌘K's Mac rows: `mac`'s window, at the project at `path` there when set (`switch` says what
/// happens with a Mac that can't be driven now).
pub fn open(local: &Arc<dyn Backend>, mac: &MacId, path: Option<String>, cx: &mut App) {
    if let Some(i) = index_of(&list(cx), mac) {
        switch_at(local, i, path, cx);
    }
}

fn switch_at(local: &Arc<dyn Backend>, i: usize, path: Option<String>, cx: &mut App) {
    let list = list(cx);
    let Some(e) = list.get(i) else { return };
    let (open, _) = crate::windows::mac_windows(cx);
    if !e.enabled(open.contains(&e.id())) {
        crate::windows::with_active(cx, |m, w, cx| crate::ui::mac_switcher::open(m, Some(i), w, cx));
        return;
    }
    let win = crate::windows::open_mac(local, e.mac.clone(), cx);
    if let (Some(win), Some(path)) = (win, path) {
        let _ = win.update(cx, |m, cx| m.open_at(path, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, MenuState, Reach, entries, menu_title, menus, reach};
    use midna_proto::nearby::{DriveLink, DriveState, PeersListResult};

    fn link(peer: &str, state: DriveState, reason: Option<&str>, last_seen: Option<&str>) -> DriveLink {
        DriveLink {
            link: format!("dl_{peer}"),
            peer: peer.into(),
            name: peer.into(),
            socket: String::new(),
            state,
            stage: None,
            reason: reason.map(str::to_string),
            last_seen: last_seen.map(str::to_string),
            drops: 0,
        }
    }

    fn peers() -> PeersListResult {
        serde_json::from_value(serde_json::json!({
            "me": { "fingerprint": "aaaa", "name": "Work MacBook Pro", "visibility": "paired", "service": "_midna._tcp", "advertising": true, "browsing": true, "port": 1, "protocol": 1 },
            "peers": [],
            "paired": [
                { "fingerprint": "cccc", "name": "Office iMac", "tier": "my_device", "paired_at": "2026-01-01T00:00:00Z", "nearby": false },
                { "fingerprint": "bbbb", "name": "Home Mac Studio", "tier": "my_device", "paired_at": "2026-01-01T00:00:00Z", "nearby": true },
                { "fingerprint": "dddd", "name": "Sam's iMac", "tier": "contact", "paired_at": "2026-01-01T00:00:00Z", "nearby": true },
            ],
            "pairings": [],
        }))
        .unwrap()
    }

    #[test]
    fn this_mac_first_then_my_macs_by_name_without_contacts() {
        let got: Vec<(String, Reach)> = entries(&peers(), &[]).into_iter().map(|e| (e.name, e.reach)).collect();
        assert_eq!(
            got,
            [
                ("Work MacBook Pro".into(), Reach::Here),
                ("Home Mac Studio".into(), Reach::Online),
                ("Office iMac".into(), Reach::Offline { last_seen: None }),
            ]
        );
    }

    #[test]
    fn a_drive_link_says_how_a_mac_stands() {
        assert_eq!(reach(false, Some(&link("b", DriveState::Connected, None, Some("t")))), Reach::Online, "connected though not seen on the network");
        assert_eq!(reach(true, Some(&link("b", DriveState::Connecting, None, None))), Reach::Connecting);
        assert_eq!(reach(true, Some(&link("b", DriveState::Reconnecting, Some("the network dropped"), Some("t")))), Reach::Reconnecting);
        assert_eq!(reach(true, Some(&link("b", DriveState::Offline, Some("timed out"), Some("t")))), Reach::Offline { last_seen: Some("t".into()) });
    }

    #[test]
    fn a_refusal_means_driving_is_not_allowed_however_the_link_stands() {
        let first = "Bravo doesn't let this Mac drive it: turn it on at Bravo in Settings › Nearby";
        let later = "Bravo stopped letting this Mac drive it";
        for (state, why) in [(DriveState::Reconnecting, first), (DriveState::Offline, first), (DriveState::Reconnecting, later), (DriveState::Connecting, later)] {
            assert_eq!(reach(true, Some(&link("b", state, Some(why), None))), Reach::NotAllowed, "{state:?}: {why}");
        }
        assert_eq!(reach(true, Some(&link("b", DriveState::Reconnecting, Some("Midna on Bravo stopped"), None))), Reach::Reconnecting);
    }

    #[test]
    fn the_newest_open_link_counts_and_a_closed_one_is_ignored() {
        let links = [
            link("bbbb", DriveState::Reconnecting, Some("Home Mac Studio stopped letting this Mac drive it"), None),
            link("bbbb", DriveState::Connected, None, Some("t")),
            link("cccc", DriveState::Closed, Some("closed"), Some("t")),
        ];
        let got: Vec<Reach> = entries(&peers(), &links).into_iter().map(|e| e.reach).collect();
        assert_eq!(got, [Reach::Here, Reach::Online, Reach::Offline { last_seen: None }]);
    }

    #[test]
    fn offline_and_refusing_macs_are_greyed_with_why_unless_their_window_is_open() {
        let e = |reach| Entry { mac: None, name: "Bravo".into(), reach };
        let refusing = e(Reach::NotAllowed);
        assert!(!refusing.enabled(false));
        assert!(refusing.enabled(true), "its window comes forward and says why");
        assert_eq!(menu_title(&refusing, false), "Bravo — Driving not allowed · turn it on there in Settings › Nearby");
        assert!(!e(Reach::Offline { last_seen: None }).enabled(false));
        assert_eq!(menu_title(&e(Reach::Offline { last_seen: None }), false), "Bravo — Offline");
        for r in [Reach::Here, Reach::Online, Reach::Connecting, Reach::Reconnecting] {
            assert!(e(r.clone()).enabled(false), "{r:?}");
        }
        assert_eq!(menu_title(&e(Reach::Online), true), "Bravo");
    }

    #[test]
    fn details_name_the_state() {
        let e = |reach| Entry { mac: None, name: String::new(), reach }.detail();
        assert_eq!(e(Reach::Here), "This Mac");
        assert_eq!(e(Reach::Online), "Online");
        assert_eq!(e(Reach::Offline { last_seen: None }), "Offline");
        let two_h = crate::model::rfc3339_from_unix(midna_proto::time::now_unix() - 2 * 3600 - 30);
        assert_eq!(e(Reach::Offline { last_seen: Some(two_h) }), "Offline · last seen 2h ago");
    }
    #[test]
    fn the_window_menu_jumps_to_each_mac_and_greys_the_ones_that_cant_be_driven() {
        let e = |name: &str, reach| Entry { mac: None, name: name.into(), reach };
        let list = vec![e("Work", Reach::Here), e("Home", Reach::Online), e("Office", Reach::Offline { last_seen: None }), e("Bravo", Reach::NotAllowed)];
        let state = MenuState { open: vec![true, false, false, false], front: Some(0), list };
        let window = menus(&state).into_iter().find(|m| m.name == "Window").unwrap();
        let got: Vec<(String, &dyn gpui_kit::Action, bool)> = window
            .items
            .iter()
            .filter_map(|i| match i {
                gpui_kit::MenuItem::Action { name, action, checked, .. } => Some((name.to_string(), action.as_ref(), *checked)),
                _ => None,
            })
            .collect();
        use crate::actions::{MacUnavailable, NewWindow, PairMac, SwitchMac};
        let want: [(&str, Box<dyn gpui_kit::Action>, bool); 6] = [
            ("New Window", Box::new(NewWindow), false),
            ("Work", Box::new(SwitchMac(0)), true),
            ("Home", Box::new(SwitchMac(1)), false),
            ("Office — Offline", Box::new(MacUnavailable(2)), false),
            ("Bravo — Driving not allowed · turn it on there in Settings › Nearby", Box::new(MacUnavailable(3)), false),
            ("Pair Another Mac…", Box::new(PairMac), false),
        ];
        assert_eq!(got.len(), want.len(), "{:?}", got.iter().map(|g| &g.0).collect::<Vec<_>>());
        for ((name, action, checked), (w_name, w_action, w_checked)) in got.iter().zip(&want) {
            assert_eq!((name.as_str(), *checked), (*w_name, *w_checked));
            assert!(action.partial_eq(w_action.as_ref()), "{name}: {action:?}");
        }
    }
}
