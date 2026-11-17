//! The needs-you pill's Mac tabs (T1949 option B, "Mac tab"): what waits on each of my other
//! Macs, as a small tab in that Mac's colour on the pill's outer corner, like an app icon's
//! badge. The pill itself counts only this Mac. A Mac with nothing waiting has no tab; with
//! more than one Mac, each has its own, side by side.
//!
//! This is the part that doesn't draw: following [`mac_feed`](crate::mac_feed)'s feeds into
//! tabs and telling what just came in, the words for the tooltip and VoiceOver, and the
//! arrival's timing. `badge.rs` draws them.
use crate::mac_feed::{Feed, Place, Post, Reach};
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

/// The keys, after the counts in the tooltip and for VoiceOver: `next` (`keys.next_needs_you`)
/// and `anywhere` (`keys.next_needs_you_anywhere`) as their labels show them. A key that's
/// unbound (an empty label) is left out.
pub fn keys(next: &str, anywhere: &str) -> String {
    let parts = [(next, "next on this window's Mac"), (anywhere, "next anywhere")];
    parts.iter().filter(|(k, _)| !k.is_empty()).map(|(k, what)| format!("{k} {what}")).collect::<Vec<_>>().join(" · ")
}

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

/// What came in last on a Mac, for the capsule that springs out beside the pill: a needs-you
/// item or a post that stays.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrival {
    pub mac: String,
    pub peer: String,
    /// Its id there (`row_id` with `peer` is its row's).
    pub id: String,
    pub category: String,
    pub title: String,
    /// When it came in (RFC 3339).
    pub at: String,
    pub place: Option<Place>,
}

impl Arrival {
    fn of_need(f: &Feed, n: &NeedsYou) -> Arrival {
        let (category, title) = (super::badge::category_of(n.kind).to_string(), super::badge::capitalize(n.title.trim()));
        Arrival { mac: f.mac.name.clone(), peer: f.mac.peer.clone(), id: n.id.clone(), category, title, at: n.created_at.clone(), place: f.place(n).cloned() }
    }

    fn of_post(f: &Feed, p: &Post) -> Arrival {
        let body = midna_proto::notify::plain_body(&p.posted.body);
        let title = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(&p.posted.title).to_string();
        Arrival { mac: f.mac.name.clone(), peer: f.mac.peer.clone(), id: p.id(), category: p.posted.category.clone(), title, at: p.at.clone(), place: f.place_of(p.session.as_deref()).cloned() }
    }
}

/// Follow `feeds` (`mac_feed::list`) on from `prev`: the tabs as they are now, and the newest
/// item that came in since, if one did. `serial` names each arrival.
pub fn follow(prev: &[Tab], feeds: &[Feed], serial: &mut u64, now: Instant) -> (Vec<Tab>, Option<Arrival>) {
    let mut tabs = Vec::with_capacity(feeds.len());
    let mut arrival: Option<Arrival> = None;
    for f in feeds {
        let was = prev.iter().find(|t| t.peer == f.mac.peer);
        let held = f.held();
        let ids: Vec<String> = f.needs.iter().map(|n| n.id.clone()).chain(held.iter().map(|p| p.id())).collect();
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
            let needs = f.needs.iter().filter(|n| !was.ids.contains(&n.id)).map(|n| Arrival::of_need(f, n));
            let posts = held.iter().filter(|p| !was.ids.contains(&p.id())).map(|p| Arrival::of_post(f, p));
            if let Some(n) = needs.chain(posts).max_by(|a, b| a.at.cmp(&b.at)) {
                *serial += 1;
                tab.came = Some(Came { n: *serial, at: now, pops: was.count() == 0 });
                // Of several Macs at once, the newest item.
                if arrival.as_ref().is_none_or(|a| n.at > a.at) {
                    arrival = Some(n);
                }
            }
        }
        tabs.push(tab);
    }
    (tabs, arrival)
}

/// ⌥⌘J's item: the newest of what waits on each Mac (`(mac, items)`, mac None for this one or
/// another's peer), by when it came in. Of two at the same moment, the Mac listed first wins.
pub fn newest<'a>(macs: impl IntoIterator<Item = (Option<&'a str>, &'a [NeedsYou])>) -> Option<(Option<String>, &'a NeedsYou)> {
    let mut best: Option<(i64, Option<&str>, &NeedsYou)> = None;
    for (mac, needs) in macs {
        for n in needs {
            let at = crate::model::parse_rfc3339(&n.created_at).unwrap_or(i64::MIN);
            if best.is_none_or(|(b, _, _)| at > b) {
                best = Some((at, mac, n));
            }
        }
    }
    best.map(|(_, mac, n)| (mac.map(str::to_string), n))
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

/// What VoiceOver reads for the pill: the tooltip's counts and `keys`.
pub fn spoken(local: usize, tabs: &[Tab], keys: &str) -> String {
    if keys.is_empty() {
        return format!("Needs you: {}", tip(local, tabs));
    }
    format!("Needs you: {}. {keys}", tip(local, tabs))
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

/// Switching the open list from one Mac's to another's (clicking the other part): the old
/// rows slide out toward the part you left (`SWITCH_OUT`, easing in), the new ones slide in
/// from the part you clicked (`SWITCH_IN`, easing out), `SWITCH_DX` across, and the notch and
/// the list's height glide over `SWITCH_GLIDE` (the canvas's 220, 300 and 280 ms).
pub const SWITCH_IN: Duration = Duration::from_millis(300);
pub const SWITCH_OUT: Duration = Duration::from_millis(220);
pub const SWITCH_GLIDE: Duration = Duration::from_millis(280);
pub const SWITCH_DX: f32 = 28.;

/// A list row's id for another Mac's item: its peer and its id there (ids are only unique on
/// their own Mac).
pub fn row_id(peer: &str, id: &str) -> String {
    format!("{peer}/{id}")
}

/// The tabs as the pill shows them: less what you cleared from their lists (`row_id`s), as the
/// pill's own count is.
pub fn uncleared(tabs: &[Tab], cleared: &[String]) -> Vec<Tab> {
    tabs.iter().map(|t| Tab { ids: t.ids.iter().filter(|id| !cleared.contains(&row_id(&t.peer, id))).cloned().collect(), ..t.clone() }).collect()
}

/// What a click on a part of the pill does: `part` is the part (None the pill, this Mac's;
/// a Mac's tab, its peer), `open` whose list is open (None: none is).
#[derive(Debug, PartialEq)]
pub enum Click {
    Open,
    Switch,
    Close,
}

pub fn click(open: Option<&Option<String>>, part: &Option<String>) -> Click {
    match open {
        None => Click::Open,
        Some(mac) if mac == part => Click::Close,
        Some(_) => Click::Switch,
    }
}

/// Which way the rows go: +1 when the part you clicked (`to`, its middle) is right of the one
/// you left (`from`), so the new rows come in from the right and the old ones leave to the left.
pub fn slide_dir(from: f32, to: f32) -> f32 {
    if to < from { -1. } else { 1. }
}

/// A switch `since` it started: how far the new rows are in, the old ones out, and the notch
/// and height across (each 0 to 1).
pub fn switch_at(since: Duration) -> (f32, f32, f32) {
    let k = |d: Duration| (since.as_secs_f32() / d.as_secs_f32()).min(1.);
    let ease_out = |x: f32| 1. - (1. - x).powi(4);
    (ease_out(k(SWITCH_IN)), k(SWITCH_OUT).powi(2), ease_out(k(SWITCH_GLIDE)))
}

pub fn switching(since: Duration) -> bool {
    since < SWITCH_IN.max(SWITCH_OUT).max(SWITCH_GLIDE)
}

/// The card's words for a Mac that can't be reached: "asleep since 14:02" (local), the day
/// first when it wasn't today ("asleep since 2 Sep 14:02"), or "asleep" when it was never
/// reached (`since`: unix seconds when its link dropped; `now` likewise).
pub fn asleep_since(since: Option<i64>, now: i64) -> String {
    use super::screen_kit::{hm, same_day, weekday_date};
    match since {
        Some(s) if same_day(s, now) => format!("asleep since {}", hm(s)),
        // "Saturday 7 Nov" → "7 Nov".
        Some(s) => format!("asleep since {} {}", weekday_date(s).split_once(' ').map_or("", |(_, d)| d), hm(s)),
        None => "asleep".into(),
    }
}

/// What's off on an asleep Mac's card, and what to do instead.
pub fn asleep_why(mac: &str) -> String {
    format!("Approve and Deny come back when {mac} wakes; nothing is queued. Open when back shows its window reconnecting, then this terminal.")
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
        assert_eq!((a.mac.as_str(), a.id.as_str(), a.place.map(|p| p.terminal)), ("Home Mac Studio", "n1", Some("sync".to_string())));
        assert_eq!(tabs[0].came, Some(Came { n: 1, at: now, pops: true }));
        // The same items again: nothing new, the last arrival kept.
        let (tabs, a) = follow(&tabs, &[f.clone()], &mut serial, now);
        assert!(a.is_none());
        assert_eq!(tabs[0].came.map(|c| c.n), Some(1));
        // A second one while the tab's there: it pings, it doesn't pop.
        f.needs.push(need("n2", "2026-11-16T10:01:00Z", None));
        let (tabs, a) = follow(&tabs, &[f], &mut serial, now);
        assert_eq!(a.map(|a| a.id), Some("n2".to_string()));
        assert_eq!(tabs[0].came, Some(Came { n: 2, at: now, pops: false }));
    }

    #[test]
    fn a_post_that_stays_counts_and_arrives_too() {
        use crate::mac_feed::Post;
        let mut serial = 0;
        let now = Instant::now();
        let (tabs, _) = follow(&[], &[feed("m", "Home Mac Studio", Reach::Connected, vec![])], &mut serial, now);
        let mut f = feed("m", "Home Mac Studio", Reach::Connected, vec![]);
        f.places.insert("s1".into(), Place { terminal: "deploy".into(), project: None });
        let posted = serde_json::from_value(serde_json::json!({ "category": "attention", "title": "", "via": "app", "id": "go", "body": "[Ship 1.4?](https://example.com/r)\nAll checks green", "actions": ["Ship", "Later"] })).unwrap();
        f.posts.push(Post { seq: 7, session: Some("s1".into()), at: "2026-11-16T10:00:00Z".into(), posted });
        let (tabs, a) = follow(&tabs, &[f.clone()], &mut serial, now);
        let a = a.expect("it arrived");
        assert_eq!((a.id.as_str(), a.category.as_str(), a.title.as_str()), ("n7", "attention", "Ship 1.4?"));
        assert_eq!(a.place.map(|p| p.terminal), Some("deploy".into()));
        assert_eq!((tabs[0].count(), tabs[0].came.map(|c| c.pops)), (1, Some(true)));
        // Withdrawn there: the tab goes.
        f.posts.clear();
        let (tabs, a) = follow(&tabs, &[f], &mut serial, now);
        assert!(a.is_none() && tabs[0].count() == 0);
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
    fn an_asleep_macs_card_says_since_when() {
        let now = crate::ui::screen_kit::now_unix();
        let at = crate::ui::screen_kit::hm(now);
        assert_eq!(asleep_since(Some(now), now), format!("asleep since {at}"));
        let week = asleep_since(Some(now - 7 * 86_400), now);
        assert!(week.starts_with("asleep since ") && week.ends_with(&at) && week.len() > format!("asleep since {at}").len() + 3, "{week}");
        assert_eq!(asleep_since(None, now), "asleep");
        assert!(asleep_why("Studio").starts_with("Approve and Deny come back when Studio wakes; nothing is queued."));
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
        let both = keys("⌘J", "⌥⌘J");
        assert_eq!(spoken(2, &[tab("Studio", 1, false)], &both), "Needs you: 2 on this Mac · 1 on Studio. ⌘J next on this window's Mac · ⌥⌘J next anywhere");
        assert_eq!(spoken(2, &[tab("Studio", 1, false)], ""), "Needs you: 2 on this Mac · 1 on Studio");
    }

    #[test]
    fn the_newest_anywhere_is_by_when_it_came_in() {
        fn picked<'a>(macs: &[(Option<&'a str>, &'a [NeedsYou])]) -> Option<(Option<String>, String)> {
            newest(macs.iter().copied()).map(|(mac, n)| (mac, n.id.clone()))
        }
        let here = [need("a", "2026-11-16T10:00:00Z", Some("t1")), need("b", "2026-11-16T10:05:00Z", None)];
        let studio = [need("c", "2026-11-16T10:03:00Z", Some("t9"))];
        let peer = Some("peer-s");
        assert_eq!(picked(&[(None, &here), (peer, &studio)]), Some((None, "b".into())));
        assert_eq!(picked(&[(None, &here[..1]), (peer, &studio)]), Some((Some("peer-s".into()), "c".into())));
        // Another offset, the same moment as this Mac's "b": the Mac listed first wins.
        let same = [need("d", "2026-11-16T11:05:00+01:00", None)];
        assert_eq!(picked(&[(None, &here), (peer, &same)]), Some((None, "b".into())));
        assert_eq!(picked(&[(peer, &same), (None, &here)]), Some((Some("peer-s".into()), "d".into())));
        assert_eq!(picked(&[(None, &[]), (peer, &[])]), None);
    }

    #[test]
    fn the_keys_follow_their_bindings() {
        assert_eq!(keys("⌘J", "⌥⌘J"), "⌘J next on this window's Mac · ⌥⌘J next anywhere");
        assert_eq!(keys("⌃J", "⇧⌃J"), "⌃J next on this window's Mac · ⇧⌃J next anywhere");
        assert_eq!(keys("⌘J", ""), "⌘J next on this window's Mac");
        assert_eq!(keys("", ""), "");
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

    #[test]
    fn a_click_opens_closes_or_switches() {
        let studio = Some("studio".to_string());
        assert_eq!(click(None, &None), Click::Open);
        assert_eq!(click(None, &studio), Click::Open);
        assert_eq!(click(Some(&None), &None), Click::Close, "the lit pill again closes");
        assert_eq!(click(Some(&studio), &studio), Click::Close, "the lit tab again closes");
        assert_eq!(click(Some(&None), &studio), Click::Switch);
        assert_eq!(click(Some(&studio), &None), Click::Switch);
        assert_eq!(click(Some(&studio), &Some("air".into())), Click::Switch);
    }

    #[test]
    fn rows_slide_from_the_part_you_clicked() {
        // The tab sits left of the pill (top-right corner): to it, the new rows come from the left.
        assert_eq!(slide_dir(380., 330.), -1.);
        assert_eq!(slide_dir(330., 380.), 1.);
        let (i, o, g) = switch_at(Duration::ZERO);
        assert_eq!((i, o, g), (0., 0., 0.));
        let (i, o, g) = switch_at(Duration::from_millis(150));
        assert!(i > 0.8 && o > 0.4 && o < 0.6 && g > 0.8, "{i} {o} {g}");
        assert_eq!(switch_at(SWITCH_IN), (1., 1., 1.));
        assert!(switching(Duration::from_millis(299)) && !switching(SWITCH_IN));
    }

    #[test]
    fn what_you_cleared_leaves_its_tab() {
        let tab = Tab { peer: "studio".into(), name: "Studio".into(), ids: vec!["a".into(), "b".into()], asleep: false, seeded: true, came: None };
        let shown = uncleared(&[tab.clone()], &[row_id("studio", "a"), row_id("air", "b")]);
        assert_eq!(shown[0].ids, vec!["b".to_string()]);
        assert_eq!(uncleared(&[tab], &[]).first().map(Tab::count), Some(2));
    }
}
