//! Nearby (`peers.list`, `peers.appeared|left`, settings `nearby.*`): two daemons on their own
//! Bonjour service type, kept on this Mac (LocalOnly), find each other only as far as their
//! visibility allows; `everyone` reverts by itself; the device id survives a restart.
mod common;
use common::*;
use midna_proto::error::HUMAN_ONLY;
use midna_proto::{Client, kinds};
use midnad::nearby::{Mode, Setup};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

/// A service type no other test run (or real Midna) uses.
fn service() -> String {
    format!("_mt{:x}x{}._tcp", std::process::id() & 0xfffff, N.fetch_add(1, Ordering::Relaxed))
}

fn nearby(svc: &str, everyone_secs: i64) -> TestDaemon {
    let svc = svc.to_string();
    TestDaemon::start_with(move |cfg| cfg.nearby = Setup { mode: Mode::Service(svc), local_only: true, everyone_secs })
}

fn list(c: &mut Client) -> Value {
    call(c, "peers.list", json!({}))
}

fn sees(c: &mut Client, fp: &str) -> bool {
    list(c)["peers"].as_array().unwrap().iter().any(|p| p["fingerprint"] == fp)
}

fn set(c: &mut Client, key: &str, value: &str) {
    call(c, "settings.set", json!({ "key": key, "value": value }));
}

fn events(c: &mut Client, kind: &str) -> Vec<Value> {
    call(c, "events.list", json!({ "filter": { "kinds": [kind] }, "limit": 1000 })).as_array().unwrap().clone()
}

/// Nothing changes for a while (a peer that shouldn't show up doesn't).
fn stays(secs: u64, what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        assert!(f(), "{what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn two_daemons_find_each_other_as_visibility_allows() {
    let svc = service();
    let (a, b) = (nearby(&svc, 600), nearby(&svc, 600));
    let (mut ha, mut hb) = (a.human(), b.human());
    set(&mut ha, "nearby.name", "Alpha");
    set(&mut hb, "nearby.name", "Bravo");
    let (me_a, me_b) = (list(&mut ha)["me"].clone(), list(&mut hb)["me"].clone());
    let (fa, fb) = (me_a["fingerprint"].as_str().unwrap().to_string(), me_b["fingerprint"].as_str().unwrap().to_string());
    assert_eq!(fa.len(), 64);
    assert_ne!(fa, fb, "each home has its own device id");
    assert_eq!((me_a["name"].as_str(), me_a["visibility"].as_str(), me_a["service"].as_str()), (Some("Alpha"), Some("paired"), Some(svc.as_str())));

    // Paired only (the default): both look, neither shows itself.
    wait_for(10, "both browsing", || (list(&mut ha)["me"]["browsing"] == true && list(&mut hb)["me"]["browsing"] == true).then_some(()));
    stays(2, "paired-only daemons stay hidden", || !sees(&mut ha, &fb) && !sees(&mut hb, &fa));
    assert_eq!(list(&mut ha)["me"]["advertising"], false);

    // A shows itself to everyone: B sees it, with its name, and gets peers.appeared.
    set(&mut ha, "nearby.visibility", "everyone");
    let peer = wait_for(10, "B sees A", || list(&mut hb)["peers"].as_array().unwrap().iter().find(|p| p["fingerprint"] == fa.as_str()).cloned());
    assert_eq!(peer["name"], "Alpha");
    assert_eq!(peer["protocol"], midna_proto::nearby::PROTOCOL);
    assert!(peer["port"].as_u64().is_some_and(|p| p > 0), "{peer}");
    let me = list(&mut ha)["me"].clone();
    assert_eq!(me["advertising"], true, "{me}");
    assert!(me["everyone_until"].is_string(), "{me}");
    let appeared = events(&mut hb, kinds::PEERS_APPEARED);
    assert_eq!(appeared.iter().filter(|e| e["data"]["fingerprint"] == fa.as_str()).count(), 1, "{appeared:?}");
    // A still can't see B (B is paired only), and never lists itself.
    stays(1, "A sees no one", || list(&mut ha)["peers"].as_array().unwrap().is_empty());

    // A renames itself: B gets the new name.
    set(&mut ha, "nearby.name", "Alpha Two");
    wait_for(10, "B sees the new name", || list(&mut hb)["peers"].as_array().unwrap().iter().any(|p| p["name"] == "Alpha Two").then_some(()));

    // A turns Nearby off: it stops looking, and B sees it leave.
    set(&mut ha, "nearby.visibility", "off");
    wait_for(10, "A gone from B", || (!sees(&mut hb, &fa)).then_some(()));
    let left = events(&mut hb, kinds::PEERS_LEFT);
    assert!(left.iter().any(|e| e["data"]["fingerprint"] == fa.as_str()), "{left:?}");
    let me = list(&mut ha)["me"].clone();
    assert_eq!((me["browsing"].as_bool(), me["advertising"].as_bool()), (Some(false), Some(false)), "{me}");
    assert!(me["everyone_until"].is_null());

    // B shows itself; A, off, doesn't look. Back on paired only, A sees B but B still can't see A.
    set(&mut hb, "nearby.visibility", "everyone");
    stays(2, "A is off", || !sees(&mut ha, &fb));
    set(&mut ha, "nearby.visibility", "paired");
    wait_for(10, "A sees B", || sees(&mut ha, &fb).then_some(()));
    stays(2, "B can't see paired-only A", || !sees(&mut hb, &fa));
}

#[test]
fn everyone_reverts_and_only_the_human_picks_it() {
    let mut d = TestDaemon::start_with(|cfg| cfg.nearby.everyone_secs = 2);
    let mut h = d.human();
    let me = list(&mut h)["me"].clone();
    assert!(me["service"].is_null(), "in-process daemons keep Bonjour off: {me}");
    let fp = me["fingerprint"].as_str().unwrap().to_string();

    let mut a = d.agent(None);
    assert_eq!(call_err(&mut a, "settings.set", json!({ "key": "nearby.visibility", "value": "everyone" })).code, HUMAN_ONLY);
    assert_eq!(list(&mut h)["me"]["visibility"], "paired");
    call(&mut a, "settings.set", json!({ "key": "nearby.name", "value": "Agent's pick" }));
    assert_eq!(list(&mut h)["me"]["name"], "Agent's pick");

    set(&mut h, "nearby.visibility", "everyone");
    let me = list(&mut h)["me"].clone();
    assert_eq!(me["visibility"], "everyone");
    assert!(me["everyone_until"].is_string(), "{me}");
    wait_for(10, "back to paired", || (list(&mut h)["me"]["visibility"] == "paired").then_some(()));
    assert!(list(&mut h)["me"]["everyone_until"].is_null());
    let changes = events(&mut h, kinds::SETTINGS_CHANGED);
    let revert = changes.iter().rev().find(|e| e["data"]["key"] == "nearby.visibility").unwrap();
    assert_eq!((revert["data"]["value"].as_str(), revert["actor"]["kind"].as_str()), (Some("paired"), Some("system")), "{revert}");

    // The device id is the same after a restart.
    d.restart();
    assert_eq!(list(&mut d.human())["me"]["fingerprint"], fp.as_str());
    let key = d.home.join("nearby/device.key");
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&key).unwrap().permissions());
    assert_eq!(mode & 0o777, 0o600);
}
