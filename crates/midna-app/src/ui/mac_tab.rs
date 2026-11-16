//! The needs-you pill's Mac tabs (T1949 option B, "Mac tab"): what waits on each of my other
//! Macs, as a small tab in that Mac's colour on the pill's outer corner, like an app icon's
//! badge. The pill itself counts only this Mac. A Mac with nothing waiting has no tab; with
//! more than one Mac, each has its own, side by side.
//!
//! This is the part that doesn't draw: following [`mac_feed`](crate::mac_feed)'s feeds into
//! tabs and telling what just came in, the words for the tooltip and VoiceOver, and the
//! arrival's timing. `badge.rs` draws them.
use crate::mac_feed::{Feed, Place, Reach};
use crate::model::NeedsYou;
use gpui_kit::{Hsla, hsla};
use std::time::{Duration, Instant};

/// The tab popping onto the corner (the canvas's `cubic-bezier(.34,1.56,.64,1)`, a back-out).
pub const POP: Duration = Duration::from_millis(520);
/// One ping's ring growing out of the tab and fading; it pings [`PINGS`] times.
pub const PING: Duration = Duration::from_millis(1100);
pub const PINGS: u32 = 2;
/// The capsule springs out this long after the tab starts popping.
pub const CAPSULE_AFTER: Duration = Duration::from_millis(160);

/// The keys, after the counts in the tooltip and for VoiceOver (the keys themselves come later).
pub const KEYS: &str = "⌘J next on this window's Mac · ⌥⌘J next anywhere";

/// One of my other Macs, as the pill shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub peer: String,
    pub name: String,
    /// Its needs-you items' ids as last seen: the count, and what's new next time.
    pub ids: Vec<String>,
    pub asleep: bool,
    /// Its items have been seen once since the app started: only what comes in after that
    /// arrives (what was waiting at launch just shows).
    pub seeded: bool,
    /// The last time something came in there.
    pub came: Option<Came>,
}

impl Tab {
    pub fn count(&self) -> usize {
        self.ids.len()
    }
}

/// Something came in on a Mac: the tab pops on (when it had none) and pings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Came {
    /// Names the arrival (its animation).
    pub n: u64,
    pub at: Instant,
    /// The Mac had nothing waiting before: its tab pops onto the corner (otherwise it was
    /// already there and only pings).
    pub pops: bool,
}

/// What came in last on a Mac, for the capsule that springs out beside the pill.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrival {
    pub mac: String,
    pub need: NeedsYou,
    pub place: Option<Place>,
}

/// Follow `feeds` (`mac_feed::list`) on from `prev`: the tabs as they are now, and the newest
/// item that came in since, if one did. `serial` names each arrival.
pub fn follow(prev: &[Tab], feeds: &[Feed], serial: &mut u64, now: Instant) -> (Vec<Tab>, Option<Arrival>) {
    let mut tabs = Vec::with_capacity(feeds.len());
    let mut arrival: Option<Arrival> = None;
    for f in feeds {
        let was = prev.iter().find(|t| t.peer == f.mac.peer);
        let ids: Vec<String> = f.needs.iter().map(|n| n.id.clone()).collect();
        let mut tab = Tab {
            peer: f.mac.peer.clone(),
            name: f.mac.name.clone(),
            ids,
            asleep: matches!(f.reach, Reach::Asleep { .. }),
            // Its first answer: connected (`mac_feed` fetches as it connects, before it tells).
            seeded: was.is_some_and(|w| w.seeded) || f.reach == Reach::Connected,
            came: was.and_then(|w| w.came),
        };
        if let Some(was) = was.filter(|w| w.seeded) {
            let new = f.needs.iter().filter(|n| !was.ids.contains(&n.id)).max_by(|a, b| a.created_at.cmp(&b.created_at));
            if let Some(n) = new {
                *serial += 1;
                tab.came = Some(Came { n: *serial, at: now, pops: was.count() == 0 });
                // Of several Macs at once, the newest item.
                if arrival.as_ref().is_none_or(|a| n.created_at > a.need.created_at) {
                    arrival = Some(Arrival { mac: f.mac.name.clone(), need: n.clone(), place: f.place(n).cloned() });
                }
            }
        }
        tabs.push(tab);
    }
    (tabs, arrival)
}

/// The tooltip's first line: "2 on this Mac · 1 on Home Mac Studio", a Mac's count followed by
/// "(asleep)" while it can't be reached.
pub fn tip(local: usize, tabs: &[Tab]) -> String {
    let mut parts = vec![if local > 0 { format!("{local} on this Mac") } else { "Nothing on this Mac".to_string() }];
    for t in tabs {
        let asleep = if t.asleep { " (asleep)" } else { "" };
        parts.push(if t.count() > 0 { format!("{} on {}{asleep}", t.count(), t.name) } else { format!("nothing on {}{asleep}", t.name) });
    }
    parts.join(" · ")
}

/// What VoiceOver reads for the pill: the tooltip's counts and keys.
pub fn spoken(local: usize, tabs: &[Tab]) -> String {
    format!("Needs you: {}. {KEYS}", tip(local, tabs))
}

/// How far the tab has popped on (`back_out`, overshooting past 1), or None once it's done.
pub fn pop(came: &Came, now: Instant) -> Option<f32> {
    if !came.pops {
        return None;
    }
    let x = now.duration_since(came.at).as_secs_f32() / POP.as_secs_f32();
    (x < 1.).then(|| {
        let (c1, c3) = (1.56_f32, 2.56_f32);
        1. + c3 * (x - 1.).powi(3) + c1 * (x - 1.).powi(2)
    })
}

/// The ping's ring: its scale (1 → 2.6) and opacity (0.8 → 0), easing out, twice over; None
/// once it's done.
pub fn ping(came: &Came, now: Instant) -> Option<(f32, f32)> {
    let since = now.duration_since(came.at);
    if since >= PING * PINGS {
        return None;
    }
    let x = (since.as_secs_f32() / PING.as_secs_f32()).fract();
    let k = 1. - (1. - x).powi(3);
    Some((1. + 1.6 * k, 0.8 * (1. - k)))
}

/// Whether the tab is still moving (the window keeps drawing frames for it).
pub fn moving(came: &Came, now: Instant) -> bool {
    now.duration_since(came.at) < POP.max(PING * PINGS)
}

/// Text on a tab of `color`: near-black on a light one, white on a dark one (the canvas's
/// `--mac-fg`, white in light themes and #06201F in dark ones).
pub fn fg_on(color: Hsla) -> Hsla {
    let c = color.to_rgb();
    let lum = 0.2126 * c.r + 0.7152 * c.g + 0.0722 * c.b;
    if lum > 0.5 { hsla(178. / 360., 0.69, 0.075, 1.) } else { hsla(0., 0., 1., 1.) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Mac;

    fn need(id: &str, at: &str, session: Option<&str>) -> NeedsYou {
        NeedsYou { id: id.into(), created_at: at.into(), title: format!("item {id}"), session_id: session.map(str::to_string), ..Default::default() }
    }

    fn feed(peer: &str, name: &str, reach: Reach, needs: Vec<NeedsYou>) -> Feed {
        let mut f = Feed::new(Mac { peer: peer.into(), name: name.into() });
        f.reach = reach;
        f.needs = needs;
        f
    }

    #[test]
    fn what_waits_at_launch_just_shows() {
        let mut serial = 0;
        let now = Instant::now();
        // Not reached yet: nothing, and nothing seen.
        let (tabs, a) = follow(&[], &[feed("m", "Studio", Reach::Connecting, vec![])], &mut serial, now);
        assert!(a.is_none() && !tabs[0].seeded && tabs[0].count() == 0);
        // Its first answer brings what was already waiting: counted, but it didn't arrive.
        let (tabs, a) = follow(&tabs, &[feed("m", "Studio", Reach::Connected, vec![need("n1", "2026-11-16T10:00:00Z", None)])], &mut serial, now);
        assert!(a.is_none(), "{a:?}");
        assert_eq!((tabs[0].count(), tabs[0].seeded, tabs[0].came), (1, true, None));
    }

    #[test]
    fn a_new_item_pops_the_tab_and_names_its_mac() {
        let mut serial = 0;
        let now = Instant::now();
        let (tabs, _) = follow(&[], &[feed("m", "Home Mac Studio", Reach::Connected, vec![])], &mut serial, now);
        let mut f = feed("m", "Home Mac Studio", Reach::Connected, vec![need("n1", "2026-11-16T10:00:00Z", Some("s1"))]);
        f.places.insert("s1".into(), Place { terminal: "sync".into(), project: Some("garden".into()) });
        let (tabs, a) = follow(&tabs, &[f.clone()], &mut serial, now);
        let a = a.expect("it arrived");
        assert_eq!((a.mac.as_str(), a.need.id.as_str(), a.place.map(|p| p.terminal)), ("Home Mac Studio", "n1", Some("sync".to_string())));
        assert_eq!(tabs[0].came, Some(Came { n: 1, at: now, pops: true }));
        // The same items again: nothing new, the last arrival kept.
        let (tabs, a) = follow(&tabs, &[f.clone()], &mut serial, now);
        assert!(a.is_none());
        assert_eq!(tabs[0].came.map(|c| c.n), Some(1));
        // A second one while the tab's there: it pings, it doesn't pop.
        f.needs.push(need("n2", "2026-11-16T10:01:00Z", None));
        let (tabs, a) = follow(&tabs, &[f], &mut serial, now);
        assert_eq!(a.map(|a| a.need.id), Some("n2".to_string()));
        assert_eq!(tabs[0].came, Some(Came { n: 2, at: now, pops: false }));
    }

    #[test]
    fn of_two_macs_the_newest_item_springs_out() {
        let mut serial = 0;
        let now = Instant::now();
        let empty = [feed("a", "Air", Reach::Connected, vec![]), feed("b", "Studio", Reach::Connected, vec![])];
        let (tabs, _) = follow(&[], &empty, &mut serial, now);
        let both = [feed("a", "Air", Reach::Connected, vec![need("x", "2026-11-16T10:05:00Z", None)]), feed("b", "Studio", Reach::Connected, vec![need("y", "2026-11-16T10:02:00Z", None)])];
        let (tabs, a) = follow(&tabs, &both, &mut serial, now);
        assert_eq!(a.map(|a| a.mac), Some("Air".to_string()));
        assert!(tabs.iter().all(|t| t.came.is_some_and(|c| c.pops)));
    }

    #[test]
    fn an_asleep_mac_keeps_its_count_and_a_woken_one_brings_nothing_new() {
        let mut serial = 0;
        let now = Instant::now();
        let items = vec![need("n1", "2026-11-16T10:00:00Z", None)];
        let (tabs, _) = follow(&[], &[feed("m", "Studio", Reach::Connected, items.clone())], &mut serial, now);
        let (tabs, a) = follow(&tabs, &[feed("m", "Studio", Reach::Asleep { since: Some(1) }, items.clone())], &mut serial, now);
        assert!(a.is_none() && tabs[0].asleep && tabs[0].count() == 1);
        let (tabs, a) = follow(&tabs, &[feed("m", "Studio", Reach::Connected, items)], &mut serial, now);
        assert!(a.is_none() && !tabs[0].asleep);
    }

    #[test]
    fn the_tip_counts_each_mac() {
        let tab = |name: &str, n: usize, asleep: bool| Tab {
            peer: name.into(),
            name: name.into(),
            ids: (0..n).map(|i| i.to_string()).collect(),
            asleep,
            seeded: true,
            came: None,
        };
        assert_eq!(tip(2, &[tab("Home Mac Studio", 1, false)]), "2 on this Mac · 1 on Home Mac Studio");
        assert_eq!(tip(0, &[tab("Home Mac Studio", 1, false)]), "Nothing on this Mac · 1 on Home Mac Studio");
        assert_eq!(tip(3, &[tab("Studio", 0, false), tab("Air", 2, true)]), "3 on this Mac · nothing on Studio · 2 on Air (asleep)");
        assert_eq!(spoken(2, &[tab("Studio", 1, false)]), "Needs you: 2 on this Mac · 1 on Studio. ⌘J next on this window's Mac · ⌥⌘J next anywhere");
    }

    #[test]
    fn it_pops_then_pings_twice() {
        let at = Instant::now();
        let came = Came { n: 1, at, pops: true };
        assert_eq!(pop(&came, at), Some(0.));
        let mid = pop(&came, at + POP / 2).unwrap();
        assert!(mid > 0.9, "back-out is past most of the way by half time: {mid}");
        assert!(pop(&came, at + POP).is_none());
        assert!(pop(&Came { pops: false, ..came }, at).is_none(), "an existing tab doesn't pop");
        let (s, o) = ping(&came, at).unwrap();
        assert!((s - 1.).abs() < 1e-5 && (o - 0.8).abs() < 1e-5);
        let (s, o) = ping(&came, at + PING - Duration::from_millis(1)).unwrap();
        assert!(s > 2.5 && o < 0.01, "{s} {o}");
        let (s, _) = ping(&came, at + PING + Duration::from_millis(1)).unwrap();
        assert!(s < 1.1, "the second ping starts over: {s}");
        assert!(ping(&came, at + PING * PINGS).is_none());
        assert!(moving(&came, at + PING) && !moving(&came, at + PING * PINGS));
    }

    #[test]
    fn its_text_reads_on_its_colour() {
        let white = hsla(0., 0., 1., 1.);
        // The canvas's --mac: #0E7A78 (light) takes white, #3FC4C0 (dark) the dark ink.
        let rgb = |r: u8, g: u8, b: u8| Hsla::from(gpui_kit::Rgba { r: r as f32 / 255., g: g as f32 / 255., b: b as f32 / 255., a: 1. });
        assert_eq!(fg_on(rgb(0x0E, 0x7A, 0x78)), white);
        assert_ne!(fg_on(rgb(0x3F, 0xC4, 0xC0)), white);
    }
}
