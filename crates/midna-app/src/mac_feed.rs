//! What waits for me on my other Macs: their needs-you items and notify posts, for the one pill
//! (T1949, option B "Mac tab").
//!
//! From launch, window or not, the app follows each paired Mac of mine (`windows::my_macs`): it
//! subscribes to that Mac's backend (`backend::for_mac`, the one its windows use too, so there's
//! one drive link per Mac, never two) on a thread of its own ([`follow`]) and keeps what it hears
//! in a [`Feed`] under that Mac's name. Only RPC results and events cross, over this app's link
//! (DECISIONS.md "Nearby: driving"), and nothing here shows a banner: the other Mac's items
//! never reach macOS's notifications on this one.
//!
//! A Mac's [`Reach`] follows midnad's drive link, which retries by itself (at once, then after
//! 1, 2, 4 and 8 s; after a minute of failures it calls the link offline, and parks it while
//! Bonjour can't see that Mac, until it's seen again or this Mac's network changes), as a
//! remote window's reconnect does (T2091). It's connected while the link is up or only
//! reconnecting, and asleep once midnad calls it offline (lid closed or off the network; a
//! locked Mac still answers, so it's connected). Asleep keeps the items, which still wait on
//! that Mac; it's fetched afresh when it's back. When this Mac wakes, asleep Macs are tried at
//! once instead of at their link's next try.
//!
//! A Mac that refuses the link (it doesn't let this Mac drive it, or just stopped) has no items:
//! they're dropped, and it stays followed on its link, which midnad parks until that Mac says it
//! may be driven again (then it connects and its items come back). Nothing here asks it again.
//! While this Mac can't open links (Nearby is off here), each Mac has no items and no link: its
//! follower stops and the link is closed, unless a window onto that Mac is open (the window
//! shows why; the link closes when its last window does), and it's asked again every
//! [`LOCAL_RETRY`]. A Mac no longer paired as mine is dropped too. A Mac that's followed keeps
//! its link when its windows close: its items still come over it.
use crate::backend::{Backend, BackendEvent, ConnState, Mac};
use crate::model::{Event, NeedsYou, Project, Session, parse_list};
use gpui_kit::{App, BorrowAppContext, Global};
use midna_proto::kinds;
use midna_proto::nearby::{DriveListResult, DriveState, PeersListResult};
use midna_proto::notify::Posted;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long a Mac waits to be asked again when this Mac wouldn't open links (Nearby off here),
/// in seconds.
const LOCAL_RETRY: i64 = 30;
/// A notify post older than this when it arrives was replayed after a reconnect, not news
/// (as `MainWindow::on_notification`).
const FRESH: i64 = 30;
/// The newest posts each Mac keeps.
const MAX_POSTS: usize = 50;
/// How often Macs this one couldn't open links to are checked for their next ask, and this Mac
/// for waking up.
const TICK: Duration = Duration::from_secs(15);
/// A tick this much later than due: this Mac slept in between.
const SLEPT: i64 = 45;

/// How another Mac can be reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Not reached yet since the app started.
    Connecting,
    /// Its link is up, or dropped less than a minute ago and being tried again.
    Connected,
    /// Its link has failed for a minute: lid closed or off the network. `since` is when it
    /// dropped (unix seconds), None if it hasn't been reached since the app started.
    Asleep { since: Option<i64> },
    /// It doesn't let this Mac drive it, or this Mac can't open links (Nearby is off here), in
    /// midnad's words. No link, no items.
    NotAllowed { why: String },
}

/// A notify post on another Mac (`notify.posted`).
#[derive(Clone, Debug, PartialEq)]
pub struct Post {
    pub seq: u64,
    pub session: Option<String>,
    pub at: String,
    pub posted: Posted,
}

impl Post {
    /// Its id among that Mac's items (`n<seq>`, as this Mac's pill names its own).
    pub fn id(&self) -> String {
        format!("n{}", self.seq)
    }
}

/// Where an item is on its Mac: its terminal's name and that terminal's project's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Place {
    pub terminal: String,
    pub project: Option<String>,
}

/// One Mac's waiting items, as the pill reads them.
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub mac: Mac,
    pub reach: Reach,
    pub needs: Vec<NeedsYou>,
    /// Oldest first.
    pub posts: Vec<Post>,
    /// Terminal id → where it is.
    pub places: HashMap<String, Place>,
    /// When its link dropped from connected (unix seconds).
    lost: Option<i64>,
}

/// What a follower does after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Next {
    Nothing,
    /// Ask that Mac for its items (and where they are) again.
    Fetch,
    /// This Mac won't open links (Nearby off here): stop following it.
    Stop,
}

/// A look at that Mac (None: that call failed, keep what's there).
#[derive(Default)]
struct Snapshot {
    needs: Option<Vec<NeedsYou>>,
    sessions: Option<Vec<Session>>,
    projects: Option<Vec<Project>>,
}

impl Feed {
    pub fn new(mac: Mac) -> Self {
        Feed { mac, reach: Reach::Connecting, needs: vec![], posts: vec![], places: HashMap::new(), lost: None }
    }

    /// Where `need` is on its Mac, if it's in a terminal that Mac listed.
    pub fn place(&self, need: &NeedsYou) -> Option<&Place> {
        self.places.get(need.session_id.as_deref()?)
    }

    /// Where terminal `session` is on its Mac, if that Mac listed it.
    pub fn place_of(&self, session: Option<&str>) -> Option<&Place> {
        self.places.get(session?)
    }

    /// The posts that wait in its list, oldest first: of kinds that stay (buttons, or
    /// `stay_secs` 0), as this Mac's pill holds its own. One per terminal and kind, or per
    /// `notify.send` id (already so in `posts`). A post about a needs-you item is that item's row.
    pub fn held(&self) -> Vec<&Post> {
        let stays = |p: &&Post| p.posted.needs_you_id.is_none() && crate::ui::toast::stay_of(&p.posted).is_none();
        let held: Vec<&Post> = self.posts.iter().filter(stays).collect();
        let newer = |i: usize, p: &Post| held[i + 1..].iter().any(|q| q.posted.id.is_none() && q.session == p.session && q.posted.category == p.posted.category);
        held.iter().enumerate().filter(|(i, p)| p.posted.id.is_some() || !newer(*i, p)).map(|(_, p)| *p).collect()
    }

    /// The link's state changed (`now`: unix seconds).
    fn on_conn(&mut self, st: &ConnState, now: i64) -> Next {
        let reason = match st {
            ConnState::Link(p) => p.reason.as_deref(),
            ConnState::NotRunning { error, .. } => Some(error.as_str()),
            ConnState::Connected | ConnState::Connecting => None,
        };
        // That Mac's refusal (its link waits, parked, for it to allow this one), or this Mac's
        // own (`drive.open` while Nearby is off here, which would otherwise be asked again
        // every second).
        let here = matches!(st, ConnState::NotRunning { error, .. } if error.contains("Nearby is off"));
        if let Some(why) = reason.filter(|r| crate::macs::refused(r) || here) {
            self.reach = Reach::NotAllowed { why: why.to_string() };
            self.needs.clear();
            self.posts.clear();
            self.places.clear();
            return if here { Next::Stop } else { Next::Nothing };
        }
        match st {
            ConnState::Connected => {
                self.reach = Reach::Connected;
                self.lost = None;
                Next::Fetch
            }
            ConnState::Connecting => Next::Nothing,
            _ => {
                if self.reach == Reach::Connected && self.lost.is_none() {
                    self.lost = Some(now);
                }
                // Reconnecting, connecting, or no link this moment (the next try opens one):
                // as it was until midnad gives up for now.
                if matches!(st, ConnState::Link(p) if p.state == DriveState::Offline) {
                    self.reach = Reach::Asleep { since: self.lost };
                }
                Next::Nothing
            }
        }
    }

    /// One of that Mac's events.
    fn on_event(&mut self, e: &Event, now: i64) -> Next {
        match e.kind.as_str() {
            kinds::NEEDS_YOU_RESOLVED => {
                if let Some(id) = e.data["id"].as_str() {
                    self.needs.retain(|n| n.id != id);
                    self.posts.retain(|p| p.posted.needs_you_id.as_deref() != Some(id));
                }
                Next::Fetch
            }
            k if k.starts_with("needs_you.") => Next::Fetch,
            kinds::NOTIFY_POSTED => {
                let Ok(posted) = serde_json::from_value::<Posted>(e.data.clone()) else { return Next::Nothing };
                let fresh = midna_proto::time::parse_rfc3339(&e.at).is_some_and(|t| now - t < FRESH);
                if !fresh || self.posts.iter().any(|p| p.seq == e.seq) {
                    return Next::Nothing;
                }
                // `notify.send`'s id replaces its own.
                if let Some(id) = posted.id.as_deref() {
                    self.posts.retain(|p| p.posted.id.as_deref() != Some(id));
                }
                let unknown = posted.needs_you_id.as_ref().is_some_and(|id| !self.needs.iter().any(|n| &n.id == id));
                self.posts.push(Post { seq: e.seq, session: e.session_id.clone(), at: e.at.clone(), posted });
                let over = self.posts.len().saturating_sub(MAX_POSTS);
                self.posts.drain(..over);
                if unknown { Next::Fetch } else { Next::Nothing }
            }
            kinds::NOTIFY_WITHDRAWN => {
                if let Some(id) = e.data["id"].as_str() {
                    self.posts.retain(|p| p.posted.id.as_deref() != Some(id));
                }
                Next::Nothing
            }
            _ => Next::Nothing,
        }
    }

    /// What that Mac answered.
    fn apply(&mut self, s: Snapshot) {
        if let Some(needs) = s.needs {
            self.needs = needs;
            let open = |id: &String| self.needs.iter().any(|n| &n.id == id);
            self.posts.retain(|p| p.posted.needs_you_id.as_ref().is_none_or(open));
        }
        if let Some(sessions) = s.sessions {
            let projects: HashMap<String, String> = s.projects.unwrap_or_default().into_iter().map(|p| (p.id, p.name)).collect();
            let project = |s: &Session| s.project_id.as_ref().and_then(|id| projects.get(id).cloned());
            self.places = sessions.iter().map(|s| (s.id.clone(), Place { terminal: s.name.clone(), project: project(s) })).collect();
        }
    }
}

/// Ask `b` for its items and where they are.
fn fetch(b: &dyn Backend) -> Snapshot {
    fn list<T: serde::de::DeserializeOwned>(b: &dyn Backend, method: &str) -> Option<Vec<T>> {
        b.call(method, json!({})).ok().map(|v| parse_list(&v))
    }
    Snapshot { needs: list(b, "needs_you.list"), sessions: list(b, "session.list"), projects: list(b, "project.list") }
}

/// Follow `b`'s Mac into `feed` on a thread of its own until it refuses or the returned
/// receiver is closed. `changed` runs after each change.
fn follow(b: Arc<dyn Backend>, feed: Arc<Mutex<Feed>>, changed: Arc<dyn Fn() + Send + Sync>) -> async_channel::Receiver<BackendEvent> {
    let (tx, rx) = async_channel::unbounded();
    b.subscribe(tx);
    let events = rx.clone();
    let label = b.label().to_string();
    let spawned = std::thread::Builder::new().name("midna-mac-feed".into()).spawn(move || {
        let now = midna_proto::time::now_unix;
        let step = |ev: BackendEvent| {
            let mut f = feed.lock().unwrap();
            let was = f.clone();
            let next = match ev {
                BackendEvent::Conn(st) => f.on_conn(&st, now()),
                BackendEvent::Event(e) => f.on_event(&e, now()),
                _ => Next::Nothing,
            };
            (next, *f != was)
        };
        while let Ok(ev) = events.recv_blocking() {
            let (mut next, mut moved) = step(ev);
            // All that came meanwhile, then one fetch for it.
            while next != Next::Stop
                && let Ok(ev) = events.try_recv()
            {
                let (n, m) = step(ev);
                next = next.max(n);
                moved |= m;
            }
            if next == Next::Fetch {
                let s = fetch(&*b);
                if events.is_closed() {
                    break;
                }
                let mut f = feed.lock().unwrap();
                let was = f.clone();
                f.apply(s);
                moved |= *f != was;
            }
            if moved {
                changed();
            }
            if next == Next::Stop {
                // Lets the event thread go (it doesn't connect again for a closed channel).
                events.close();
                break;
            }
        }
    });
    if let Err(e) = spawned {
        eprintln!("midna-app: following {label}: {e}");
        rx.close();
    }
    rx
}

/// A Mac being followed.
struct Followed {
    feed: Arc<Mutex<Feed>>,
    /// Its follower's events: closing it stops the follower.
    events: async_channel::Receiver<BackendEvent>,
    /// When this Mac couldn't open its link (Nearby off here) and let it go (unix seconds).
    refused_at: Option<i64>,
}

/// Every Mac followed, by fingerprint.
#[derive(Default)]
pub struct MacFeeds {
    local: Option<Arc<dyn Backend>>,
    followed: BTreeMap<String, Followed>,
    /// My Macs as `peers.list` last said.
    mine: Vec<Mac>,
    /// Followers say they changed something.
    changes: Option<async_channel::Sender<()>>,
}

impl Global for MacFeeds {}

impl MacFeeds {
    /// Whether `peer` is followed and its link wasn't let go: it stays for its items (or, while
    /// that Mac refuses, for when it allows this one again).
    fn follows(&self, peer: &str) -> bool {
        self.followed.get(peer).is_some_and(|f| f.refused_at.is_none())
    }
}

/// Each of my other Macs I can drive, by name: what waits there and how it's reached. Observe
/// [`MacFeeds`] (`cx.observe_global`) to hear when it changes.
pub fn list(cx: &App) -> Vec<Feed> {
    let Some(g) = cx.try_global::<MacFeeds>() else { return vec![] };
    let mut feeds: Vec<Feed> = g.followed.values().map(|f| f.feed.lock().unwrap().clone()).filter(|f| !matches!(f.reach, Reach::NotAllowed { .. })).collect();
    feeds.sort_by(|a, b| a.mac.name.to_lowercase().cmp(&b.mac.name.to_lowercase()).then(a.mac.peer.cmp(&b.mac.peer)));
    feeds
}

/// `peers.list` answered (`macs::refresh`): follow each Mac of mine, and stop following one that
/// isn't any more.
pub fn sync(local: &Arc<dyn Backend>, peers: &PeersListResult, cx: &mut App) {
    // Another Mac's peers are its own Macs (this one among them): only this Mac's count.
    if local.mac().is_some() {
        return;
    }
    start(local, cx);
    let mine = crate::windows::my_macs(peers);
    let gone: Vec<String> = cx.global::<MacFeeds>().followed.keys().filter(|p| !mine.iter().any(|m| &m.peer == *p)).cloned().collect();
    for peer in gone {
        if let Some(f) = cx.global_mut::<MacFeeds>().followed.remove(&peer) {
            f.events.close();
            let refused = f.refused_at.is_some();
            if !refused {
                let_go(&peer, cx);
            }
        }
    }
    for mac in &mine {
        match cx.global::<MacFeeds>().followed.get(&mac.peer).map(|f| f.feed.clone()) {
            // Renamed: the feed's name follows.
            Some(feed) => feed.lock().unwrap().mac.name.clone_from(&mac.name),
            None => follow_mac(mac.clone(), cx),
        }
    }
    cx.global_mut::<MacFeeds>().mine = mine;
    changed(cx);
}

/// Set up once: the feeds' global, the task that hears their changes, and the ticker.
fn start(local: &Arc<dyn Backend>, cx: &mut App) {
    if cx.try_global::<MacFeeds>().is_some_and(|g| g.local.is_some()) {
        return;
    }
    let (tx, rx) = async_channel::unbounded::<()>();
    let g = cx.default_global::<MacFeeds>();
    g.local = Some(local.clone());
    g.changes = Some(tx);
    cx.spawn(async move |cx| {
        while rx.recv().await.is_ok() {
            while rx.try_recv().is_ok() {}
            cx.update(on_changes);
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        let mut last = midna_proto::time::now_unix();
        loop {
            cx.background_executor().timer(TICK).await;
            let now = midna_proto::time::now_unix();
            let woke = now - last > SLEPT;
            last = now;
            cx.update(|cx| tick(woke, now, cx));
        }
    })
    .detach();
}

fn follow_mac(mac: Mac, cx: &mut App) {
    let g = cx.global::<MacFeeds>();
    let (Some(local), Some(changes)) = (g.local.clone(), g.changes.clone()) else { return };
    let b = crate::backend::for_mac(&local, mac.clone());
    let feed = Arc::new(Mutex::new(Feed::new(mac.clone())));
    let events = follow(b, feed.clone(), Arc::new(move || {
        let _ = changes.try_send(());
    }));
    cx.global_mut::<MacFeeds>().followed.insert(mac.peer, Followed { feed, events, refused_at: None });
}

/// A follower changed its feed: let go of the links this Mac just couldn't open (Nearby off
/// here), and tell the observers. A Mac that refused keeps its link: midnad wakes it when that
/// Mac allows this one.
fn on_changes(cx: &mut App) {
    let now = midna_proto::time::now_unix();
    let here = |f: &Followed| matches!(&f.feed.lock().unwrap().reach, Reach::NotAllowed { why } if !crate::macs::refused(why));
    let refused: Vec<String> = cx
        .global::<MacFeeds>()
        .followed
        .iter()
        .filter(|(_, f)| f.refused_at.is_none() && here(f))
        .map(|(p, _)| p.clone())
        .collect();
    for peer in refused {
        if let Some(f) = cx.global_mut::<MacFeeds>().followed.get_mut(&peer) {
            f.refused_at = Some(now);
        }
        let_go(&peer, cx);
    }
    changed(cx);
}

/// Every [`TICK`]: ask Macs this one couldn't open links to again once they're due
/// ([`LOCAL_RETRY`]), and after this Mac `woke`, try asleep ones now.
fn tick(woke: bool, now: i64, cx: &mut App) {
    let g = cx.global::<MacFeeds>();
    let due: Vec<Mac> =
        g.mine.iter().filter(|m| g.followed.get(&m.peer).and_then(|f| f.refused_at).is_some_and(|at| now - at >= LOCAL_RETRY)).cloned().collect();
    let asleep: Vec<String> =
        if woke { g.followed.iter().filter(|(_, f)| matches!(f.feed.lock().unwrap().reach, Reach::Asleep { .. })).map(|(p, _)| p.clone()).collect() } else { vec![] };
    let local = g.local.clone();
    for mac in due {
        follow_mac(mac, cx);
    }
    if let (Some(local), false) = (local, asleep.is_empty()) {
        // Opening a link again from this app tries it now (midnad's `drive.open`).
        cx.background_executor()
            .spawn(async move {
                for peer in asleep {
                    let _ = local.call("drive.open", json!({ "peer": peer }));
                }
            })
            .detach();
    }
}

/// Close this app's link to `peer`, unless a window onto that Mac is open (it keeps its link,
/// and shows why it can't be used, until [`window_closed`]).
fn let_go(peer: &str, cx: &mut App) {
    if crate::windows::mac_windows(cx).0.contains(&Some(peer.to_string())) {
        return;
    }
    let Some(local) = cx.global::<MacFeeds>().local.clone() else { return };
    close_link(local, peer.to_string(), cx);
}

/// A window onto `peer` closed (`local`: this Mac's backend). Once it was the last one, close
/// that Mac's link unless it's followed here: one that refused, or isn't one of my Macs any
/// more, kept its link only for the window ([`let_go`]), and midnad would go on trying it.
pub fn window_closed(local: Arc<dyn Backend>, peer: &str, cx: &mut App) {
    if crate::windows::mac_windows(cx).0.contains(&Some(peer.to_string())) {
        return;
    }
    if cx.try_global::<MacFeeds>().is_some_and(|g| g.follows(peer)) {
        return;
    }
    close_link(local, peer.to_string(), cx);
}

/// `drive.close` each of this app's links to `peer`.
fn close_link(local: Arc<dyn Backend>, peer: String, cx: &mut App) {
    cx.background_executor()
        .spawn(async move {
            let links = local.call("drive.list", Value::Null).ok().and_then(|v| serde_json::from_value::<DriveListResult>(v).ok());
            for l in links.map(|l| l.links).unwrap_or_default().into_iter().filter(|l| l.peer == peer) {
                let _ = local.call("drive.close", json!({ "link": l.link }));
            }
        })
        .detach();
}

/// Tell [`MacFeeds`]'s observers.
fn changed(cx: &mut App) {
    cx.update_global::<MacFeeds, _>(|_, _| {});
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::LinkProgress;
    use crate::backend::fake::FakeBackend;
    use std::time::Instant;

    /// Another Mac of the fake's, under a fingerprint of this test's own (`for_mac` keeps one
    /// backend per fingerprint for the whole run).
    fn mac(c: char) -> (Arc<dyn Backend>, Mac) {
        let here: Arc<dyn Backend> = Arc::new(FakeBackend::new());
        let mac = Mac { peer: c.to_string().repeat(64), name: "Morgan's Mac mini".into() };
        (crate::backend::for_mac(&here, mac.clone()), mac)
    }

    /// Follow `mac` through `b` as the app does, without the app.
    fn followed(b: &Arc<dyn Backend>, mac: &Mac) -> (Arc<Mutex<Feed>>, async_channel::Receiver<BackendEvent>) {
        let feed = Arc::new(Mutex::new(Feed::new(mac.clone())));
        let rx = follow(b.clone(), feed.clone(), Arc::new(|| {}));
        (feed, rx)
    }

    /// Wait (up to 5 s) until `feed` is as `ok` wants it.
    fn until(feed: &Mutex<Feed>, what: &str, ok: impl Fn(&Feed) -> bool) {
        let start = Instant::now();
        while !ok(&feed.lock().unwrap()) {
            assert!(start.elapsed() < Duration::from_secs(5), "{what}: {:#?}", feed.lock().unwrap());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn link(state: DriveState, reason: Option<&str>) -> ConnState {
        ConnState::Link(LinkProgress { state, stage: None, reason: reason.map(Into::into) })
    }

    fn raise(b: &Arc<dyn Backend>, title: &str) -> String {
        let sid = b.call("session.list", json!({})).unwrap()[0]["id"].as_str().unwrap().to_string();
        let v = b.call("needs_you.raise", json!({ "kind": "approval", "title": title, "session_id": sid })).unwrap();
        v["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn items_arrive_and_resolve_with_their_mac_and_place() {
        let (b, mac) = mac('g');
        let (feed, _rx) = followed(&b, &mac);
        let start = b.call("needs_you.list", json!({})).unwrap().as_array().unwrap().len();
        until(&feed, "connected, with what was there", |f| f.reach == Reach::Connected && f.needs.len() == start);
        assert!(start > 0);

        let id = raise(&b, "Run cargo publish?");
        until(&feed, "the new item", |f| f.needs.iter().any(|n| n.id == id));
        let f = feed.lock().unwrap().clone();
        assert_eq!(f.mac.name, "Morgan's Mac mini");
        let n = f.needs.iter().find(|n| n.id == id).unwrap();
        let place = f.place(n).expect("its terminal");
        assert!(!place.terminal.is_empty() && place.project.is_some(), "{place:?}");

        b.call("needs_you.resolve", json!({ "id": id, "resolution": { "kind": "deny" } })).unwrap();
        until(&feed, "resolved and gone", |f| !f.needs.iter().any(|n| n.id == id) && f.needs.len() == start);
    }

    #[test]
    fn posts_come_and_go_and_old_ones_are_left_out() {
        let (b, mac) = mac('h');
        let (feed, _rx) = followed(&b, &mac);
        until(&feed, "connected", |f| f.reach == Reach::Connected);
        b.call("notify.send", json!({ "id": "n1", "category": "done", "title": "Tests passed" })).unwrap();
        until(&feed, "the post", |f| f.posts.iter().any(|p| p.posted.title == "Tests passed"));
        b.call("notify.withdraw", json!({ "id": "n1" })).unwrap();
        until(&feed, "withdrawn", |f| f.posts.is_empty());

        // Replayed after a reconnect: not news.
        let mut f = Feed::new(mac);
        let old = Event { seq: 9, at: "2020-01-01T00:00:00Z".into(), kind: kinds::NOTIFY_POSTED.into(), data: json!({ "category": "done", "title": "x", "via": "app" }), ..Default::default() };
        assert_eq!(f.on_event(&old, midna_proto::time::now_unix()), Next::Nothing);
        assert!(f.posts.is_empty());
    }

    #[test]
    fn only_posts_that_stay_wait_in_its_list() {
        let mut f = Feed::new(Mac { peer: "a".repeat(64), name: "Bravo".into() });
        let post = |seq: u64, session: Option<&str>, data: Value| {
            let mut v = json!({ "via": "app", "title": "", "category": "done" });
            for (k, x) in data.as_object().unwrap() {
                v[k] = x.clone();
            }
            Post { seq, session: session.map(str::to_string), at: "2026-11-16T10:00:00Z".into(), posted: serde_json::from_value(v).unwrap() }
        };
        f.posts = vec![
            // Passing (the app's default stay): no.
            post(1, Some("s1"), json!({ "body": "Tests passed" })),
            // Buttons: it stays until one's picked.
            post(2, Some("s1"), json!({ "id": "deploy", "body": "Deploy to prod?", "actions": ["Deploy", "Later"] })),
            // `stay_secs` 0, one per terminal and kind: the newer replaces the older.
            post(3, Some("s1"), json!({ "category": "failed", "body": "build broke", "stay_secs": 0 })),
            post(4, Some("s2"), json!({ "category": "failed", "body": "lint broke", "stay_secs": 0 })),
            post(5, Some("s1"), json!({ "category": "failed", "body": "build broke again", "stay_secs": 0 })),
            // About a needs-you item: that item's row.
            post(6, Some("s1"), json!({ "category": "approval", "needs_you_id": "ny1", "stay_secs": 0 })),
        ];
        let held: Vec<String> = f.held().iter().map(|p| p.id()).collect();
        assert_eq!(held, ["n2", "n4", "n5"]);
    }

    #[test]
    fn asleep_keeps_its_items_and_comes_back_fresh() {
        let (b, mac) = mac('i');
        let (feed, _rx) = followed(&b, &mac);
        until(&feed, "connected", |f| f.reach == Reach::Connected && !f.needs.is_empty());
        let had = feed.lock().unwrap().needs.len();

        // A blip: still connected.
        b.call("fake.link", json!({ "state": "reconnecting" })).unwrap();
        until(&feed, "dropped", |f| f.lost.is_some());
        assert_eq!(feed.lock().unwrap().reach, Reach::Connected);

        // A minute of failures: asleep since it dropped, its items still waiting there.
        b.call("fake.link", json!({ "state": "offline" })).unwrap();
        until(&feed, "asleep", |f| matches!(f.reach, Reach::Asleep { since: Some(_) }));
        assert_eq!(feed.lock().unwrap().needs.len(), had);

        // Answered on that Mac while this one couldn't hear: back, it's gone here too.
        let first = feed.lock().unwrap().needs[0].id.clone();
        b.call("needs_you.resolve", json!({ "id": first, "resolution": { "kind": "deny" } })).unwrap();
        b.call("fake.link", json!({ "state": "connected" })).unwrap();
        until(&feed, "back", |f| f.reach == Reach::Connected && f.lost.is_none() && !f.needs.iter().any(|n| n.id == first));
    }

    #[test]
    fn turning_driving_off_there_drops_its_items_and_waits_for_it_to_come_back_on() {
        let (b, mac) = mac('j');
        let (feed, rx) = followed(&b, &mac);
        until(&feed, "connected", |f| f.reach == Reach::Connected && !f.needs.is_empty());
        b.call("fake.link", json!({ "state": "refused" })).unwrap();
        until(&feed, "refused", |f| matches!(f.reach, Reach::NotAllowed { .. }));
        let f = feed.lock().unwrap().clone();
        assert!(f.needs.is_empty() && f.posts.is_empty() && f.places.is_empty());
        assert_eq!(f.reach, Reach::NotAllowed { why: "Morgan's Mac mini stopped letting this Mac drive it".into() });
        // Still followed on its (parked) link, never asked again from here: when that Mac lets
        // this one drive it, midnad's link comes up and the items are back.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!rx.is_closed(), "the follower stays");
        b.call("fake.link", json!({ "state": "connected" })).unwrap();
        until(&feed, "allowed again", |f| f.reach == Reach::Connected && !f.needs.is_empty());
    }

    #[test]
    fn reach_follows_the_link() {
        let mut f = Feed::new(Mac { peer: "a".repeat(64), name: "Bravo".into() });
        // Never reached since launch, then midnad gives up for now: asleep, since unknown.
        assert_eq!(f.on_conn(&link(DriveState::Connecting, None), 100), Next::Nothing);
        assert_eq!(f.reach, Reach::Connecting);
        f.on_conn(&link(DriveState::Offline, Some("timed out")), 160);
        assert_eq!(f.reach, Reach::Asleep { since: None });
        assert_eq!(f.on_conn(&ConnState::Connected, 200), Next::Fetch);
        assert_eq!(f.reach, Reach::Connected);
        // Dropped at 300; this Mac's daemon then has no link a moment, then offline.
        f.on_conn(&link(DriveState::Reconnecting, Some("the network dropped")), 300);
        f.on_conn(&ConnState::NotRunning { socket: "/x".into(), error: "no link to Bravo".into() }, 310);
        assert_eq!(f.reach, Reach::Connected);
        f.on_conn(&link(DriveState::Offline, Some("timed out")), 360);
        assert_eq!(f.reach, Reach::Asleep { since: Some(300) });
        // The first refusal's wording too (the link parked, as its socket says): followed on,
        // waiting for that Mac.
        let why = "not connected to Bravo now (Bravo doesn't let this Mac drive it: turn it on at Bravo in Settings › Nearby; paused until Bravo lets this Mac drive it)";
        assert_eq!(f.on_conn(&ConnState::NotRunning { socket: "/x".into(), error: why.into() }, 400), Next::Nothing);
        assert_eq!(f.reach, Reach::NotAllowed { why: why.into() });
        assert_eq!(f.on_conn(&ConnState::Connected, 500), Next::Fetch, "allowed again");
        assert_eq!(f.reach, Reach::Connected);
        // Nor while this Mac won't open links: no asking again every second.
        let mut f = Feed::new(Mac { peer: "a".repeat(64), name: "Bravo".into() });
        let off = "Nearby is off on this Mac (visibility off): turn it on to drive another Mac";
        assert_eq!(f.on_conn(&ConnState::NotRunning { socket: "/x".into(), error: off.into() }, 500), Next::Stop);
        assert_eq!(f.reach, Reach::NotAllowed { why: off.into() });
    }
    #[gpui_kit::test]
    fn follows_each_of_my_macs_until_it_isnt_mine(cx: &mut gpui_kit::TestAppContext) {
        let here: Arc<dyn Backend> = Arc::new(FakeBackend::new());
        let peers: PeersListResult = serde_json::from_value(here.call("peers.list", json!({})).unwrap()).unwrap();
        cx.update(|cx| sync(&here, &peers, cx));
        // My devices only (Sam's iMac is a contact), connected over the fake's links.
        let start = Instant::now();
        loop {
            let feeds = cx.update(|cx| list(cx));
            let names: Vec<&str> = feeds.iter().map(|f| f.mac.name.as_str()).collect();
            assert_eq!(names, ["Morgan's Mac mini", "Office iMac", "Studio"]);
            if feeds.iter().all(|f| f.reach == Reach::Connected && !f.needs.is_empty()) {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "{feeds:#?}");
            std::thread::sleep(Duration::from_millis(10));
        }

        // The Mac mini made a contact: no longer followed, and its follower let go.
        let mut fewer = peers.clone();
        fewer.paired.retain(|p| p.name != "Morgan's Mac mini");
        cx.update(|cx| sync(&here, &fewer, cx));
        let names: Vec<String> = cx.update(|cx| list(cx)).into_iter().map(|f| f.mac.name).collect();
        assert_eq!(names, ["Office iMac", "Studio"]);
    }

    /// This Mac's midnad with a link to each of `a`, `b` and `c`: records the calls made to it.
    #[derive(Default)]
    struct Here(Mutex<Vec<String>>);

    impl Backend for Here {
        fn label(&self) -> &'static str {
            "here"
        }
        fn socket_path(&self) -> std::path::PathBuf {
            "/nowhere".into()
        }
        fn open_mac(self: Arc<Self>, _: Mac) -> Arc<dyn Backend> {
            unreachable!("no window opens a Mac here")
        }
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            self.0.lock().unwrap().push(format!("{method} {}", params["link"].as_str().unwrap_or("")).trim().to_string());
            let link = |c: char| json!({ "link": format!("dl_{c}"), "peer": c.to_string().repeat(64), "name": c.to_string(), "socket": "", "state": "connected" });
            Ok(match method {
                "drive.list" => json!({ "links": [link('a'), link('b'), link('c')] }),
                _ => json!({ "closed": true }),
            })
        }
        fn subscribe(&self, _: async_channel::Sender<BackendEvent>) {}
        fn attach(&self, _: crate::backend::AttachRequest<'_>, _: Arc<crate::frame::FrameSink>) -> anyhow::Result<Arc<dyn crate::backend::TermStream>> {
            anyhow::bail!("no terminals here")
        }
    }

    #[gpui_kit::test]
    fn the_last_window_closing_lets_go_of_a_link_only_it_kept(cx: &mut gpui_kit::TestAppContext) {
        let here = Arc::new(Here::default());
        let local: Arc<dyn Backend> = here.clone();
        let peer = |c: char| c.to_string().repeat(64);
        let follow = |c: char, refused_at: Option<i64>| {
            let feed = Arc::new(Mutex::new(Feed::new(Mac { peer: peer(c), name: c.to_string() })));
            Followed { feed, events: async_channel::unbounded().1, refused_at }
        };
        cx.update(|cx| {
            let g = cx.default_global::<MacFeeds>();
            g.followed.insert(peer('a'), follow('a', None));
            g.followed.insert(peer('b'), follow('b', Some(100)));
        });
        let closed = |c: char, cx: &mut gpui_kit::TestAppContext| {
            here.0.lock().unwrap().clear();
            cx.update(|cx| window_closed(local.clone(), &peer(c), cx));
            cx.run_until_parked();
            here.0.lock().unwrap().iter().filter(|c| c.starts_with("drive.close")).cloned().collect::<Vec<_>>()
        };
        // Followed: its items still come over the link.
        assert_eq!(closed('a', cx), Vec::<String>::new());
        // It refused while the window showed why, or isn't one of my Macs any more: nothing
        // here wants the link now.
        assert_eq!(closed('b', cx), ["drive.close dl_b"]);
        assert_eq!(closed('c', cx), ["drive.close dl_c"]);
    }
}
