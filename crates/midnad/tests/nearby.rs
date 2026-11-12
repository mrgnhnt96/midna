//! Nearby (`peers.list`, `peers.appeared|left`, settings `nearby.*`): two daemons on their own
//! Bonjour service type, kept on this Mac (LocalOnly), find each other only as far as their
//! visibility allows; `everyone` reverts by itself; the device id survives a restart.
//!
//! Pairing (`peers.pair|confirm|forget`): both sides show the same code and only the human
//! confirms; a man in the middle gets two different codes; paired devices find each other while
//! paired only; anything but a pairing request from a key that isn't paired is refused at the
//! connection, forgetting cuts a device off, and a device using a paired one's name or copying
//! its advertisement is flagged and can't pass for it.
mod common;
use common::*;
use midna_proto::error::HUMAN_ONLY;
use midna_proto::nearby as nb;
use midna_proto::{Client, kinds};
use midnad::nearby::identity::{self, Identity};
use midnad::nearby::transport::{self, Channel, Local, Wire};
use midnad::nearby::{Mode, Setup, dnssd, pairing};
use serde_json::{Value, json};
use std::net::{SocketAddr, TcpListener};
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

// ---- pairing ----

fn fingerprint(c: &mut Client) -> String {
    list(c)["me"]["fingerprint"].as_str().unwrap().to_string()
}

fn port(c: &mut Client) -> u16 {
    let me = list(c)["me"].clone();
    me["port"].as_u64().unwrap_or_else(|| panic!("no port: {me}")) as u16
}

fn peer(c: &mut Client, fp: &str) -> Option<Value> {
    list(c)["peers"].as_array().unwrap().iter().find(|p| p["fingerprint"] == fp).cloned()
}

fn pairings(c: &mut Client) -> Vec<Value> {
    list(c)["pairings"].as_array().unwrap().clone()
}

fn paired(c: &mut Client) -> Vec<Value> {
    list(c)["paired"].as_array().unwrap().clone()
}

/// The open approval this pairing raised.
fn approval(c: &mut Client, pairing: &Value) -> Value {
    let id = pairing["needs_you_id"].as_str().unwrap().to_string();
    let items = call(c, "needs_you.list", json!({}));
    items.as_array().unwrap().iter().find(|n| n["id"] == id.as_str()).cloned().unwrap_or_else(|| panic!("no approval {id}: {items}"))
}

fn addr(port: u16) -> Vec<SocketAddr> {
    vec![SocketAddr::from(([127, 0, 0, 1], port))]
}

/// A device that isn't one of the daemons: its own key, from its own home.
fn stranger() -> (Identity, Local, std::path::PathBuf) {
    let home = std::env::temp_dir().join(format!("midna-stranger-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&home);
    let id = identity::load_or_create(&home);
    let local = Local::new(&id);
    (id, local, home)
}

/// Send one message and return the answer.
fn ask(ch: &mut Channel, m: &Wire) -> Wire {
    ch.send(m).unwrap();
    ch.expect(Duration::from_secs(10)).unwrap()
}

/// A advertises like a Midna visible to everyone, under `name` and `fp` (whoever's it is),
/// pointing at `port`. Dropping the result withdraws it.
fn advertise(svc: &str, name: &str, fp: &str, port: u16) -> dnssd::Ref {
    let txt = nb::txt_record(&[(nb::TXT_PROTOCOL, &nb::PROTOCOL.to_string()), (nb::TXT_FINGERPRINT, fp), (nb::TXT_NAME, name)]);
    dnssd::register(99, dnssd::LOCAL_ONLY, &format!("{name} {}", &fp[..6]), svc, port, &txt).unwrap()
}

/// Two daemons, Alpha and Bravo, with B visible to everyone and A seeing it.
fn alpha_bravo(svc: &str) -> (TestDaemon, TestDaemon, String, String) {
    let (a, b) = (nearby(svc, 600), nearby(svc, 600));
    let (mut ha, mut hb) = (a.human(), b.human());
    set(&mut ha, "nearby.name", "Alpha");
    set(&mut hb, "nearby.name", "Bravo");
    set(&mut hb, "nearby.visibility", "everyone");
    let (fa, fb) = (fingerprint(&mut ha), fingerprint(&mut hb));
    wait_for(10, "A sees B", || peer(&mut ha, &fb));
    (a, b, fa, fb)
}

/// A asks B to pair; both people confirm (A keeps B as one of its own Macs, B keeps A as a
/// contact through its needs-you approval).
fn pair_both(a: &TestDaemon, b: &TestDaemon, fa: &str, fb: &str) {
    let (mut ha, mut hb) = (a.human(), b.human());
    let mine = call(&mut ha, "peers.pair", json!({ "fingerprint": fb }));
    let theirs = wait_for(10, "B has the pairing", || pairings(&mut hb).into_iter().find(|p| p["fingerprint"] == fa));
    assert_eq!(mine["code"], theirs["code"]);
    call(&mut ha, "peers.confirm", json!({ "pairing": mine["id"], "as": "my_device" }));
    let item = approval(&mut hb, &theirs);
    call(&mut hb, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    wait_for(10, "both keep each other", || (paired(&mut ha).len() == 1 && paired(&mut hb).len() == 1).then_some(()));
}

#[test]
fn both_screens_show_the_code_and_only_the_human_confirms() {
    let svc = service();
    let (a, b, fa, fb) = alpha_bravo(&svc);
    let (mut ha, mut hb, mut agent) = (a.human(), b.human(), a.agent(None));

    // An agent may ask to pair; both sides then show the same 6-digit code and raise an approval.
    let mine = call(&mut agent, "peers.pair", json!({ "fingerprint": fb }));
    assert_eq!((mine["state"].as_str(), mine["role"].as_str(), mine["name"].as_str()), (Some("comparing"), Some("outgoing"), Some("Bravo")), "{mine}");
    let code = mine["code"].as_str().unwrap().to_string();
    assert!(code.len() == nb::CODE_DIGITS && code.bytes().all(|c| c.is_ascii_digit()), "{code}");
    let theirs = wait_for(10, "B has the pairing", || pairings(&mut hb).into_iter().find(|p| p["fingerprint"] == fa.as_str()));
    assert_eq!((theirs["role"].as_str(), theirs["name"].as_str(), theirs["code"].as_str()), (Some("incoming"), Some("Alpha"), Some(code.as_str())), "{theirs}");
    let item = approval(&mut hb, &theirs);
    assert!(item["title"].as_str().unwrap().contains(&nb::spaced_code(&code)), "{item}");
    let started = events(&mut ha, kinds::PEERS_PAIRING);
    assert_eq!(started.len(), 1, "{started:?}");
    assert_eq!(call_err(&mut agent, "peers.pair", json!({ "fingerprint": fb })).code, midna_proto::error::CONFLICT);

    // Agents can't confirm: not by the method, not by approving the item.
    let mut agent_b = b.agent(None);
    assert_eq!(call_err(&mut agent, "peers.confirm", json!({ "pairing": mine["id"] })).code, HUMAN_ONLY);
    let res = json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    assert_eq!(call_err(&mut agent_b, "needs_you.resolve", res.clone()).code, HUMAN_ONLY);
    assert_eq!(pairings(&mut ha)[0]["confirmed"], false);

    // One side confirming keeps nothing yet; the other side sees it.
    let c = call(&mut ha, "peers.confirm", json!({ "pairing": mine["id"], "as": "my_device" }));
    assert_eq!((c["confirmed"].as_bool(), c["tier"].as_str()), (Some(true), Some("my_device")), "{c}");
    wait_for(10, "B hears A confirmed", || (pairings(&mut hb)[0]["peer_confirmed"] == true).then_some(()));
    assert!(paired(&mut ha).is_empty() && paired(&mut hb).is_empty());

    // The human approves on B: both keep each other, each with its own pick.
    call(&mut hb, "needs_you.resolve", res);
    wait_for(10, "both keep each other", || (paired(&mut ha).len() == 1 && paired(&mut hb).len() == 1).then_some(()));
    let (pa, pb) = (paired(&mut ha)[0].clone(), paired(&mut hb)[0].clone());
    assert_eq!((pa["fingerprint"].as_str(), pa["name"].as_str(), pa["tier"].as_str()), (Some(fb.as_str()), Some("Bravo"), Some("my_device")), "{pa}");
    assert_eq!((pb["fingerprint"].as_str(), pb["name"].as_str(), pb["tier"].as_str()), (Some(fa.as_str()), Some("Alpha"), Some("contact")), "{pb}");
    for (h, fp) in [(&mut ha, &fb), (&mut hb, &fa)] {
        wait_for(10, "pairing shows paired", || (pairings(h)[0]["state"] == "paired").then_some(()));
        let ev = events(h, kinds::PEERS_PAIRED);
        assert!(ev.len() == 1 && ev[0]["data"]["fingerprint"] == fp.as_str(), "{ev:?}");
        assert!(call(h, "needs_you.list", json!({})).as_array().unwrap().is_empty(), "the approvals are closed");
    }
    let kept = std::fs::metadata(b.home.join("nearby/peers.json")).unwrap();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&kept.permissions()) & 0o777, 0o600);

    // Back to paired only, both still find each other (and nobody else can tell who they are).
    let b_port = peer(&mut ha, &fb).unwrap()["port"].clone();
    set(&mut hb, "nearby.visibility", "paired");
    wait_for(10, "B's everyone advert gone", || events(&mut ha, kinds::PEERS_LEFT).iter().any(|e| e["data"]["fingerprint"] == fb.as_str()).then_some(()));
    let back = wait_for(10, "A sees B by its tag", || peer(&mut ha, &fb));
    assert_eq!((back["name"].as_str(), back["tier"].as_str(), &back["port"]), (Some("Bravo"), Some("my_device"), &b_port), "{back}");
    let seen = wait_for(10, "B sees A by its tag", || peer(&mut hb, &fa));
    assert_eq!((seen["name"].as_str(), seen["tier"].as_str()), (Some("Alpha"), Some("contact")), "{seen}");

    // After a restart B still knows A.
    let mut b = b;
    b.restart();
    let mut hb = b.human();
    assert_eq!(paired(&mut hb)[0]["fingerprint"], fa.as_str());
}

#[test]
fn only_paired_keys_get_past_the_connection_and_forgetting_cuts_them_off() {
    let svc = service();
    let (a, b, fa, fb) = alpha_bravo(&svc);
    pair_both(&a, &b, &fa, &fb);
    let mut hb = b.human();
    let at = addr(port(&mut hb));

    // A key B never paired with: everything but a pairing request is refused, and the
    // connection closes.
    let (stranger_id, stranger, _home) = stranger();
    let mut ch = transport::connect(&at, &stranger).unwrap();
    assert_eq!(ch.fingerprint, fb, "the handshake proves B's key");
    assert_eq!(ask(&mut ch, &Wire::Ping), Wire::Refused { reason: transport::NOT_PAIRED.into() });
    assert!(ch.recv(Duration::from_secs(5)).is_err(), "closed after refusing");
    let _ = stranger_id;

    // A's key (A's home is right here): it gets through.
    let a_key = identity::load_or_create(&a.home);
    assert_eq!(a_key.fingerprint, fa);
    let mut ch = transport::connect(&at, &Local::new(&a_key)).unwrap();
    assert_eq!(ask(&mut ch, &Wire::Ping), Wire::Pong);

    // Forgetting is human only; once B forgets A, the open connection is cut off too.
    let mut agent = b.agent(None);
    assert_eq!(call_err(&mut agent, "peers.forget", json!({ "fingerprint": fa })).code, HUMAN_ONLY);
    assert_eq!(paired(&mut hb).len(), 1);
    let gone = call(&mut hb, "peers.forget", json!({ "fingerprint": fa }));
    assert_eq!(gone["forgotten"]["fingerprint"], fa.as_str());
    assert!(paired(&mut hb).is_empty());
    assert_eq!(events(&mut hb, kinds::PEERS_FORGOTTEN).len(), 1);
    assert_eq!(ask(&mut ch, &Wire::Ping), Wire::Refused { reason: transport::NOT_PAIRED.into() });
    let mut ch = transport::connect(&at, &Local::new(&a_key)).unwrap();
    assert_eq!(ask(&mut ch, &Wire::Ping), Wire::Refused { reason: transport::NOT_PAIRED.into() });
    assert!(peer(&mut hb, &fa).is_none_or(|p| p["tier"].is_null()));

    // Nearby off: nothing is served at all, not even to a paired key.
    let mut ha = a.human();
    assert_eq!(paired(&mut ha).len(), 1, "A still has B until A forgets it");
    set(&mut hb, "nearby.visibility", "off");
    let r = transport::connect(&at, &Local::new(&a_key));
    assert!(r.is_err(), "off answers no one");
}

#[test]
fn a_man_in_the_middle_gets_two_different_codes() {
    let svc = service();
    let (a, b, fa, fb) = alpha_bravo(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());

    // Mallory advertises itself as "Bravo" (with its own key: it can't sign as B's) and relays
    // a pairing between A and B, running the protocol with each.
    let (mallory_id, mallory, _home) = stranger();
    let fm = mallory_id.fingerprint.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _ad = advertise(&svc, "Bravo", &fm, listener.local_addr().unwrap().port());
    let b_at = addr(port(&mut hb));
    let relay = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut from_a = transport::accept(stream, &mallory).unwrap();
        let Wire::PairRequest { name, .. } = from_a.expect(Duration::from_secs(10)).unwrap() else { panic!("not a pair request") };
        let mut to_b = transport::connect(&b_at, &mallory).unwrap();
        let with_b = pairing::agree_as_initiator(&mut to_b, &mallory_id.fingerprint, &name).unwrap();
        let with_a = pairing::agree_as_responder(&mut from_a, &mallory_id.fingerprint, "Bravo", name).unwrap();
        (with_a.code, with_b.code, from_a, to_b)
    });
    let found = wait_for(10, "A sees Mallory", || peer(&mut ha, &fm));
    assert_eq!(found["name"], "Bravo");
    assert!(found["tier"].is_null() && found["claims_name_of"].is_null(), "nothing's paired yet: {found}");

    let mine = call(&mut ha, "peers.pair", json!({ "fingerprint": fm }));
    let (code_a, code_b, _from_a, _to_b) = relay.join().unwrap();
    assert_eq!(mine["code"].as_str(), Some(code_a.as_str()));
    let theirs = wait_for(10, "B has the pairing", || pairings(&mut hb).into_iter().next());
    assert_eq!(theirs["code"].as_str(), Some(code_b.as_str()));
    assert_ne!(code_a, code_b, "the two screens disagree");
    assert_eq!(theirs["fingerprint"].as_str(), Some(fm.as_str()), "B sees Mallory's key, not A's");
    assert_ne!(theirs["fingerprint"].as_str(), Some(fa.as_str()));

    // The people see different codes and say no: A on the command line, B on its approval.
    let no = call(&mut ha, "peers.confirm", json!({ "pairing": mine["id"], "accept": false }));
    assert_eq!(no["state"], "declined", "{no}");
    let item = approval(&mut hb, &theirs);
    call(&mut hb, "needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "deny" } }));
    wait_for(10, "B's pairing declined", || (pairings(&mut hb)[0]["state"] == "declined").then_some(()));
    assert_eq!(call_err(&mut hb, "peers.confirm", json!({ "pairing": theirs["id"] })).code, midna_proto::error::CONFLICT, "too late to confirm");
    for h in [&mut ha, &mut hb] {
        assert!(paired(h).is_empty(), "nothing kept");
        assert_eq!(events(h, kinds::PEERS_PAIRING_ENDED).len(), 1);
        assert!(call(h, "needs_you.list", json!({})).as_array().unwrap().is_empty());
    }
    assert!(!b.home.join("nearby/peers.json").exists() || paired(&mut hb).is_empty());
    let _ = fb;
}

#[test]
fn an_impostor_is_flagged_and_cant_pass_for_a_paired_device() {
    let svc = service();
    let (a, b, fa, fb) = alpha_bravo(&svc);
    pair_both(&a, &b, &fa, &fb);
    let (mut ha, mut hb) = (a.human(), b.human());
    set(&mut ha, "nearby.visibility", "paired");
    set(&mut hb, "nearby.visibility", "off");
    wait_for(10, "B gone from A", || peer(&mut ha, &fb).is_none().then_some(()));

    // Mallory shows itself as "bravo" with its own key: listed, but not paired, and flagged.
    let (mallory_id, mallory, _home) = stranger();
    let fm = mallory_id.fingerprint.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mport = listener.local_addr().unwrap().port();
    let ad = advertise(&svc, "bravo", &fm, mport);
    let seen = wait_for(10, "A sees Mallory", || peer(&mut ha, &fm));
    assert!(seen["tier"].is_null(), "{seen}");
    assert_eq!(seen["claims_name_of"].as_str(), Some(fb.as_str()), "{seen}");
    let appeared = events(&mut ha, kinds::PEERS_APPEARED);
    let ev = appeared.iter().rev().find(|e| e["data"]["fingerprint"] == fm.as_str()).unwrap();
    assert_eq!(ev["data"]["claims_name_of"].as_str(), Some(fb.as_str()), "{ev}");

    // Its connections get nowhere: A (paired only) takes no pairing request from it either.
    let a_at = addr(port(&mut ha));
    let mut ch = transport::connect(&a_at, &mallory).unwrap();
    assert_eq!(ask(&mut ch, &Wire::Ping), Wire::Refused { reason: transport::NOT_PAIRED.into() });
    let mut ch = transport::connect(&a_at, &mallory).unwrap();
    assert!(matches!(ask(&mut ch, &Wire::PairRequest { name: "Bravo".into(), protocol: nb::PROTOCOL }), Wire::Refused { .. }));
    assert!(!pairings(&mut ha).iter().any(|p| p["fingerprint"] == fm.as_str()), "no pairing with Mallory");
    drop(ad);

    // Mallory copies B's advertisement (here worked out from A's own record of the pair, the
    // most anyone could copy): A lists B's id at Mallory's address, but connecting proves the
    // key, so A won't take Mallory for B.
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(a.home.join("nearby/peers.json")).unwrap()).unwrap();
    let presence = transport::unhex(stored["peers"][0]["presence"].as_str().unwrap()).unwrap();
    let salt: [u8; 16] = transport::random();
    let mut salted = salt.to_vec();
    salted.extend_from_slice(fb.as_bytes());
    let tag = midnad::nearby::store::tag(&presence, &salted).unwrap();
    let salt = transport::hex(&salt);
    let txt = nb::txt_record(&[(nb::TXT_PROTOCOL, &nb::PROTOCOL.to_string()), (nb::TXT_SALT, &salt), (nb::TXT_TAGS, &tag)]);
    let _copy = dnssd::register(98, dnssd::LOCAL_ONLY, &format!("Midna {}", &salt[..4]), &svc, mport, &txt).unwrap();
    let posing = wait_for(10, "A lists B at Mallory's port", || peer(&mut ha, &fb).filter(|p| p["port"] == mport));
    assert_eq!(posing["tier"], "my_device", "a tag is only a hint");
    let answer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = transport::accept(stream, &mallory);
    });
    let err = match midnad::nearby::link::connect_peer(&a.daemon(), &fb) {
        Ok(_) => panic!("A took Mallory for B"),
        Err(e) => e,
    };
    assert!(err.message.contains("isn't it"), "{err:?}");
    answer.join().unwrap();
}
