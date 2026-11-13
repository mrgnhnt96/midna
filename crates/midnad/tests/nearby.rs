//! Nearby (`peers.list`, `peers.appeared|left`, settings `nearby.*`): two daemons on their own
//! Bonjour service type, kept on this Mac (LocalOnly), find each other only as far as their
//! visibility allows; `everyone` reverts by itself; the device id survives a restart.
//!
//! Pairing (`peers.pair|confirm|forget`): both sides show the same code and only the human
//! confirms; a man in the middle gets two different codes; paired devices find each other while
//! paired only; anything but a pairing request from a key that isn't paired is refused at the
//! connection, forgetting cuts a device off, and a device using a paired one's name or copying
//! its advertisement is flagged and can't pass for it.
//!
//! Shares (`share.send|accept|decline`, `inbox.list`): only paired devices can offer one; it
//! waits in the receiver's inbox (kept across restarts) until a human there accepts or declines
//! it, agents can't, a snippet is pasted only into the terminal the human picks and never with
//! Enter, and the answer reaches the sender.
//!
//! The guard: what an agent sends leaves only with the human's OK (an approval, or a rule the
//! human gave; an agent's own allow doesn't count), and an agent asking to confirm a pairing or
//! take a share is pointed at the needs-you item that waits for the human.
//!
//! Files (`share.send` kind file): the offer carries the size and each file's checksum; accepting
//! fetches the files in chunks with `share.progress`, picks up where it stopped after the sender
//! drops out, and lands them (folders with their tree) in `nearby.downloads`; a file that changed
//! fails cleanly, and offers or fetches that reach outside their folder or share are refused.
//!
//! Auto-accept (`peers.set_auto_accept`): only the human turns it on, per kind, for one of their
//! own devices (never a contact); those shares are accepted as they arrive (`auto`), while rules,
//! triggers, settings and queued prompts still wait for review and an agent session can't be set
//! to; keeping the device as a contact
//! or forgetting it turns it all off.
//!
//! Tabs (`share.send` kind tab): the receiver opens a new shell in its own checkout of the
//! sender's repo (or the folder the human picks, or home), the sender's scrollback shown above
//! it and nothing of theirs run; a tab whose scrollback carries control sequences is refused.
//!
//! Agent sessions (`share.send` kind agent): the offer carries the agent's conversation and its
//! transcript (whole lines only); nothing is placed or resumed until the human accepts, then the
//! transcript, with the sender's checkout paths turned into the receiver's, goes where Claude Code
//! or Codex looks for it and a new agent terminal resumes it; accepting is refused while a
//! terminal here runs that conversation.
//!
//! Agent starts (`share.send` kind agent_start, `share.answer`): an agent's start leaves only with
//! its human's OK, the receiver's human sees the prompt and nothing runs until they accept; then a
//! new agent terminal in the receiver's checkout gets the prompt, accepting again is refused, and
//! with `want_reply` the agent's last message when its first turn ends comes back (cut to the
//! message limit) as a read reply that `share.answer` waits for; later turns send nothing.
mod common;
use common::*;
use midna_proto::error::{HUMAN_ONLY, NOT_FOUND, REFUSED};
use midna_proto::nearby as nb;
use midna_proto::{Client, ClientError, RpcError, kinds};
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
    nearby_with(svc, everyone_secs, None)
}

/// Like `nearby`, launching `agent_bin` for agent terminals.
fn nearby_with(svc: &str, everyone_secs: i64, agent_bin: Option<String>) -> TestDaemon {
    let svc = svc.to_string();
    TestDaemon::start_with(move |cfg| {
        cfg.nearby = Setup { mode: Mode::Service(svc), local_only: true, everyone_secs, ..Setup::default() };
        cfg.agent_bin = agent_bin;
    })
}

/// Restart `d` on the same Bonjour service.
fn restart_nearby(d: &mut TestDaemon, svc: &str) {
    let svc = svc.to_string();
    d.restart_with(move |cfg| cfg.nearby = Setup { mode: Mode::Service(svc), local_only: true, everyone_secs: 600, ..Setup::default() });
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
    assert_eq!(me["local_network"], "ok", "this Mac only: Local Network privacy never blocks it: {me}");
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
    assert!(me["local_network"].is_null(), "no Nearby, no Local Network state: {me}");
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
    alpha_bravo_with(svc, None)
}

fn alpha_bravo_with(svc: &str, agent_bin: Option<String>) -> (TestDaemon, TestDaemon, String, String) {
    let (a, b) = (nearby_with(svc, 600, agent_bin.clone()), nearby_with(svc, 600, agent_bin));
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
    let e = call_err(&mut agent, "peers.confirm", json!({ "pairing": mine["id"] }));
    let on_a = approval(&mut ha, &mine);
    assert_eq!((e.code, waits_on(&e)), (HUMAN_ONLY, on_a["id"].as_str().map(str::to_string)), "{}", e.message);
    assert!(e.message.contains(on_a["id"].as_str().unwrap()), "{}", e.message);
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

    // The tier changes by human only RPC and survives in the list.
    assert_eq!(call_err(&mut b.agent(None), "peers.set_tier", json!({ "fingerprint": fa, "as": "my_device" })).code, HUMAN_ONLY);
    assert_eq!(paired(&mut hb)[0]["tier"], "contact");
    let changed = call(&mut hb, "peers.set_tier", json!({ "fingerprint": fa, "as": "my_device" }));
    assert_eq!(changed["tier"], "my_device");
    assert_eq!(paired(&mut hb)[0]["tier"], "my_device");
    assert_eq!(events(&mut hb, kinds::PEERS_CHANGED).len(), 1);
    assert_eq!(call_err(&mut hb, "peers.set_tier", json!({ "fingerprint": "nope", "as": "contact" })).code, NOT_FOUND);

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

// ---- shares ----

fn inbox(c: &mut Client) -> Value {
    call(c, "inbox.list", json!({}))
}

/// The share `id` in this daemon's inbox.
fn share(c: &mut Client, id: &str) -> Option<Value> {
    inbox(c)["items"].as_array().unwrap().iter().find(|s| s["id"] == id).cloned()
}

fn send(c: &mut Client, peer: &str, p: Value) -> Value {
    let mut p = p;
    p["peer"] = json!(peer);
    call(c, "share.send", p)
}

/// The events of `kind` about share `id`.
fn share_events(c: &mut Client, kind: &str, id: &str) -> Vec<Value> {
    events(c, kind).into_iter().filter(|e| e["data"]["id"] == id).collect()
}

/// The needs-you item a refusal points at.
fn waits_on(e: &RpcError) -> Option<String> {
    e.data.as_ref().and_then(|d| d["needs_you_id"].as_str()).map(str::to_string)
}

/// Two paired daemons that see each other.
fn paired_pair(svc: &str) -> (TestDaemon, TestDaemon, String, String) {
    paired_pair_with(svc, None)
}

/// Like `paired_pair`, launching `agent_bin` for agent terminals.
fn paired_pair_with(svc: &str, agent_bin: Option<String>) -> (TestDaemon, TestDaemon, String, String) {
    let (a, b, fa, fb) = alpha_bravo_with(svc, agent_bin);
    pair_both(&a, &b, &fa, &fb);
    let mut hb = b.human();
    wait_for(10, "B sees A", || peer(&mut hb, &fa));
    (a, b, fa, fb)
}

#[test]
fn a_share_lands_in_the_inbox_and_only_the_human_takes_it() {
    let svc = service();
    let (a, b, fa, fb) = alpha_bravo(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());

    // Not paired yet: nothing gets through.
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "message", "text": "hi" }));
    assert!(e.message.contains("isn't paired") || e.message.contains("not paired"), "{}", e.message);
    assert!(inbox(&mut hb)["items"].as_array().unwrap().is_empty());
    pair_both(&a, &b, &fa, &fb);

    // A bad payload never leaves A.
    for bad in [
        json!({ "kind": "link", "url": "javascript:alert(1)" }),
        json!({ "kind": "link", "url": "file:///etc/passwd" }),
        json!({ "kind": "message", "text": "" }),
        json!({ "kind": "snippet", "text": "x", "reply_to": "sh_abcdabcd" }),
    ] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }

    // The human sends; B gets an offer and an approval.
    let sent = send(&mut ha, &fb, json!({ "kind": "message", "text": "lunch?" }));
    let id = sent["id"].as_str().unwrap().to_string();
    assert_eq!((sent["direction"].as_str(), sent["state"].as_str(), sent["peer_name"].as_str()), (Some("outgoing"), Some("offered"), Some("Bravo")), "{sent}");
    let got = share(&mut hb, &id).expect("B has the share as soon as send returns");
    assert_eq!((got["direction"].as_str(), got["from"].as_str(), got["peer_name"].as_str()), (Some("incoming"), Some(fa.as_str()), Some("Alpha")), "{got}");
    assert_eq!((got["payload"]["kind"].as_str(), got["payload"]["text"].as_str(), got["size"].as_u64()), (Some("message"), Some("lunch?"), Some(6)));
    assert_eq!(inbox(&mut hb)["waiting"], 1);
    assert_eq!(share_events(&mut ha, kinds::SHARE_SENT, &id).len(), 1);
    assert_eq!(share_events(&mut hb, kinds::SHARE_OFFERED, &id).len(), 1);
    let ny = got["needs_you_id"].as_str().unwrap().to_string();
    let item = call(&mut hb, "needs_you.get", json!({ "id": ny }));
    assert!(item["item"]["title"].as_str().unwrap().contains("Alpha"), "{item}");

    // Agents on B can't accept or decline it, not by the methods and not by the approval.
    let mut agent_b = b.agent(None);
    for m in ["share.accept", "share.decline"] {
        let e = call_err(&mut agent_b, m, json!({ "id": id }));
        assert_eq!((e.code, waits_on(&e)), (HUMAN_ONLY, Some(ny.clone())), "{m}: {}", e.message);
    }
    let e = call_err(&mut agent_b, "share.accept", json!({ "id": "sh_nothere1" }));
    assert_eq!((e.code, waits_on(&e)), (HUMAN_ONLY, None), "{}", e.message);
    let approve = json!({ "id": ny, "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    assert_eq!(call_err(&mut agent_b, "needs_you.resolve", approve.clone()).code, HUMAN_ONLY);
    assert_eq!(share(&mut hb, &id).unwrap()["state"], "offered");

    // The inbox survives a restart.
    let mut b = b;
    restart_nearby(&mut b, &svc);
    let mut hb = b.human();
    wait_for(10, "B sees A again", || peer(&mut hb, &fa));
    let kept = share(&mut hb, &id).expect("kept across the restart");
    assert_eq!((kept["state"].as_str(), kept["needs_you_id"].as_str()), (Some("offered"), Some(ny.as_str())), "{kept}");
    let mode = std::fs::metadata(b.home.join("nearby/inbox.json")).unwrap();
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode.permissions()) & 0o777, 0o600);

    // The human approves: the message is read, the approval closes, and A hears it.
    call(&mut hb, "needs_you.resolve", approve);
    let read = wait_for(10, "accepted", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    assert_eq!(read["result"], "read");
    assert!(read["needs_you_id"].is_null(), "{read}");
    assert_eq!(share_events(&mut hb, kinds::SHARE_ACCEPTED, &id).len(), 1);
    let theirs = wait_for(20, "A hears B accepted", || share(&mut ha, &id).filter(|s| s["state"] == "accepted"));
    assert!(theirs["decided_at"].is_string(), "{theirs}");
    wait_for(10, "A's share.accepted", || {
        share_events(&mut ha, kinds::SHARE_ACCEPTED, &id).first().filter(|e| e["data"]["direction"] == "outgoing").cloned()
    });

    // B replies; denying that one's approval on A declines it, and B hears that.
    let reply = send(&mut hb, &fa, json!({ "kind": "message", "text": "yes", "reply_to": id }));
    let rid = reply["id"].as_str().unwrap().to_string();
    let on_a = share(&mut ha, &rid).unwrap();
    assert_eq!(on_a["payload"]["reply_to"], id.as_str());
    let deny = json!({ "id": on_a["needs_you_id"], "resolution": { "kind": "deny", "scope": { "kind": "once" } } });
    call(&mut ha, "needs_you.resolve", deny);
    assert_eq!(wait_for(10, "declined", || share(&mut ha, &rid).filter(|s| s["state"] == "declined"))["needs_you_id"], Value::Null);
    wait_for(20, "B hears A declined", || share(&mut hb, &rid).filter(|s| s["state"] == "declined"));
    assert_eq!(share_events(&mut hb, kinds::SHARE_DECLINED, &rid).len(), 1);
    assert_eq!(call_err(&mut ha, "share.accept", json!({ "id": rid })).code, midna_proto::error::CONFLICT, "declined stays declined");

    // Only incoming shares can be decided here.
    assert_eq!(call_err(&mut hb, "share.accept", json!({ "id": rid })).code, midna_proto::error::NOT_FOUND);
    let only_in = call(&mut ha, "inbox.list", json!({ "direction": "incoming" }));
    assert!(only_in["items"].as_array().unwrap().iter().all(|s| s["direction"] == "incoming"), "{only_in}");
}

#[test]
fn a_snippet_is_pasted_only_where_the_human_says_and_never_run() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());
    let _ = fa;

    // Nothing reaches a terminal on offer.
    let term = call(&mut hb, "session.open", json!({ "kind": "shell", "cwd": "/tmp", "command": ["/bin/cat"] }))["id"].as_str().unwrap().to_string();
    let one = send(&mut ha, &fb, json!({ "kind": "snippet", "text": "echo pasted-7f3\n", "title": "try this" }))["id"].as_str().unwrap().to_string();
    stays(1, "an offered snippet stays out of every terminal", || !read(&mut hb, &term).contains("pasted-7f3"));

    // Accepting without a terminal keeps it in the inbox; accepting into one pastes it, without
    // the Enter (cat would print the line a second time).
    let kept = call(&mut hb, "share.accept", json!({ "id": one }));
    assert_eq!((kept["state"].as_str(), kept["result"].as_str()), (Some("accepted"), Some("kept in the inbox")), "{kept}");
    let pasted = call(&mut hb, "share.accept", json!({ "id": one, "session": term }));
    assert_eq!(pasted["result"].as_str(), Some(format!("pasted into {term}").as_str()));
    wait_for(10, "the paste shows", || read(&mut hb, &term).contains("echo pasted-7f3").then_some(()));
    stays(1, "no Enter after the paste", || read(&mut hb, &term).matches("pasted-7f3").count() == 1);
    assert_eq!(share_events(&mut hb, kinds::SHARE_ACCEPTED, &one).len(), 1, "accepting again is no new decision");

    // Several lines where bracketed paste is off would run line by line: refused.
    let two = send(&mut ha, &fb, json!({ "kind": "snippet", "text": "echo one\necho two" }))["id"].as_str().unwrap().to_string();
    let e = call_err(&mut hb, "share.accept", json!({ "id": two, "session": term }));
    assert!(e.message.contains("bracketed paste"), "{}", e.message);
    assert_eq!(share(&mut hb, &two).unwrap()["state"], "offered");
    stays(1, "nothing typed", || !read(&mut hb, &term).contains("echo one"));
    let e = call_err(&mut hb, "share.accept", json!({ "id": two, "session": "t_nope" }));
    assert_eq!(e.code, midna_proto::error::NOT_FOUND);

    // A link the human pins goes to that terminal's links, never opened; a declined share is
    // never pasted.
    let url = "https://example.com/from-alpha";
    let l = send(&mut ha, &fb, json!({ "kind": "link", "url": url, "title": "docs" }))["id"].as_str().unwrap().to_string();
    let pinned = call(&mut hb, "share.accept", json!({ "id": l, "action": "pin", "session": term }));
    assert_eq!(pinned["result"].as_str(), Some(format!("pinned to {term}").as_str()));
    let links = call(&mut hb, "links.list", json!({ "session": term }));
    assert!(links.to_string().contains(url), "{links}");
    call(&mut hb, "share.decline", json!({ "id": two }));
    assert_eq!(call_err(&mut hb, "share.accept", json!({ "id": two, "session": term })).code, midna_proto::error::CONFLICT);
    for id in [&one, &l, &two] {
        wait_for(20, "A hears every answer", || share(&mut ha, id).filter(|s| s["state"] != "offered"));
    }
}

#[test]
fn a_paired_device_cant_slip_a_bad_share_past_the_checks() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let mut hb = b.human();
    let _ = (fa, fb);
    // A's own key, straight at B's port, skipping A's checks.
    let a_key = identity::load_or_create(&a.home);
    let mut ch = transport::connect(&addr(port(&mut hb)), &Local::new(&a_key)).unwrap();
    for (id, payload) in [
        ("sh_bad1bad1", json!({ "kind": "link", "url": "file:///etc/passwd" })),
        ("sh_bad2bad2", json!({ "kind": "command", "argv": ["rm", "-rf", "/"] })),
        ("not-an-id", json!({ "kind": "message", "text": "hi" })),
        ("sh_bad3bad3", json!({ "kind": "snippet", "text": "x".repeat(nb::MAX_SNIPPET + 1) })),
    ] {
        let got = ask(&mut ch, &Wire::ShareOffer { id: id.into(), payload: payload.clone() });
        assert!(matches!(got, Wire::Refused { .. }), "{payload}: {got:?}");
    }
    assert!(inbox(&mut hb)["items"].as_array().unwrap().is_empty());
    assert!(events(&mut hb, kinds::SHARE_OFFERED).is_empty());

    // A share id another device used is taken; the same device sending it again is acked once.
    let ok = ask(&mut ch, &Wire::ShareOffer { id: "sh_good1234".into(), payload: json!({ "kind": "message", "text": "hi" }) });
    assert_eq!(ok, Wire::ShareAck { id: "sh_good1234".into() });
    let again = ask(&mut ch, &Wire::ShareOffer { id: "sh_good1234".into(), payload: json!({ "kind": "message", "text": "hi" }) });
    assert_eq!(again, Wire::ShareAck { id: "sh_good1234".into() });
    assert_eq!(inbox(&mut hb)["items"].as_array().unwrap().len(), 1);
    assert_eq!(events(&mut hb, kinds::SHARE_OFFERED).len(), 1);
}

/// The open approvals for agents' shares.
fn share_asks(c: &mut Client) -> Vec<Value> {
    let items = call(c, "needs_you.list", json!({}));
    items.as_array().unwrap().iter().filter(|n| n["approval"]["action"]["value"].as_str().is_some_and(|v| v.starts_with("share send "))).cloned().collect()
}

fn answer(c: &mut Client, item: &Value, resolution: Value) {
    call(c, "needs_you.resolve", json!({ "id": item["id"], "resolution": resolution }));
}

fn refused(r: Result<Value, ClientError>) -> RpcError {
    match r {
        Err(ClientError::Rpc(e)) => e,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// Texts B has received.
fn received(c: &mut Client) -> Vec<String> {
    inbox(c)["items"].as_array().unwrap().iter().filter(|s| s["direction"] == "incoming").filter_map(|s| s["payload"]["text"].as_str().map(str::to_string)).collect()
}

#[test]
fn an_agents_share_leaves_only_with_the_humans_ok() {
    let svc = service();
    let (a, b, _fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb, mut agent) = (a.human(), b.human(), a.agent(None));
    let in_background = |kind: &str, text: &str| {
        let mut c = a.agent(None);
        let p = json!({ "peer": fb, "kind": kind, "text": text });
        std::thread::spawn(move || c.call_value("share.send", p))
    };

    // The human's own sends don't ask.
    send(&mut ha, &fb, json!({ "kind": "message", "text": "from the human" }));
    assert!(share_asks(&mut ha).is_empty());

    // An agent's send waits for the human, who sees what would leave and where; denying keeps it here.
    let t = in_background("message", "the secret plans");
    let item = wait_for(10, "A asks", || share_asks(&mut ha).into_iter().next());
    assert_eq!(item["approval"]["action"]["value"], format!("share send message {fb}"));
    assert!(item["title"].as_str().unwrap().contains("Bravo"), "{item}");
    assert!(item["detail"].as_str().unwrap().contains("the secret plans"), "{item}");
    assert!(received(&mut hb).iter().all(|t| t != "the secret plans"), "nothing left before the answer");
    answer(&mut ha, &item, json!({ "kind": "deny" }));
    let e = refused(t.join().unwrap());
    assert_eq!((e.code, waits_on(&e)), (midna_proto::error::REFUSED, item["id"].as_str().map(str::to_string)), "{}", e.message);
    assert!(!received(&mut hb).contains(&"the secret plans".to_string()));

    // An agent's own allow rule doesn't let it through; approving once does, for that share only.
    call(&mut agent, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "cli", "pattern": "share send*" } }));
    let t = in_background("message", "approved once");
    let item = wait_for(10, "A asks despite the agent's rule", || share_asks(&mut ha).into_iter().next());
    answer(&mut ha, &item, json!({ "kind": "approve", "scope": { "kind": "once" } }));
    let sent = t.join().unwrap().expect("sent once approved");
    assert!(share(&mut hb, sent["id"].as_str().unwrap()).is_some(), "B has it");

    // Approving "always" lets agents send this device this kind without asking again; another kind still asks.
    let t = in_background("message", "first");
    let item = wait_for(10, "A asks", || share_asks(&mut ha).into_iter().next());
    answer(&mut ha, &item, json!({ "kind": "approve", "scope": { "kind": "always" } }));
    t.join().unwrap().expect("sent");
    send(&mut agent, &fb, json!({ "kind": "message", "text": "second" }));
    assert!(share_asks(&mut ha).is_empty());
    let t = in_background("snippet", "rm -rf ~");
    let item = wait_for(10, "a snippet still asks", || share_asks(&mut ha).into_iter().next());
    assert_eq!(item["approval"]["action"]["value"], format!("share send snippet {fb}"));
    answer(&mut ha, &item, json!({ "kind": "deny" }));
    refused(t.join().unwrap());
    let got = received(&mut hb);
    assert!(got.contains(&"second".to_string()) && !got.contains(&"rm -rf ~".to_string()), "{got:?}");

    // No answer in time: nothing is sent.
    call(&mut ha, "settings.set", json!({ "key": "policy.request_timeout_secs", "value": 1 }));
    let e = call_err(&mut agent, "share.send", json!({ "peer": fb, "kind": "snippet", "text": "late" }));
    assert!(e.code == midna_proto::error::REFUSED && e.message.contains("no answer"), "{}", e.message);
    assert!(!received(&mut hb).contains(&"late".to_string()));

    // A deny (anyone may add one) beats the human's "always", without asking.
    call(&mut agent, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "cli", "pattern": "share send message*" } }));
    let e = call_err(&mut agent, "share.send", json!({ "peer": fb, "kind": "message", "text": "third" }));
    assert!(e.code == midna_proto::error::REFUSED && e.message.contains("denied by rule"), "{}", e.message);
    assert!(share_asks(&mut ha).is_empty());

    // A device that isn't paired is refused before anyone is asked.
    let e = call_err(&mut agent, "share.send", json!({ "peer": "ab".repeat(32), "kind": "snippet", "text": "x" }));
    assert!(e.code == midna_proto::error::REFUSED && e.message.contains("isn't paired"), "{}", e.message);
    assert!(share_asks(&mut ha).is_empty());
    assert!(!received(&mut hb).contains(&"third".to_string()));
}

// ---- files ----

/// A fresh folder for a test's files.
fn scratch(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("midna-files-{}-{}-{tag}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::canonicalize(&p).unwrap()
}

/// `n` bytes that differ from one test file to the next.
fn bytes(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u32).wrapping_mul(2654435761).rotate_left(seed as u32 % 31) as u8 ^ seed).collect()
}

/// B lands accepted files in its own scratch folder.
fn downloads_into(hb: &mut Client, tag: &str) -> std::path::PathBuf {
    let dl = scratch(tag).join("Downloads");
    set(hb, "nearby.downloads", dl.to_str().unwrap());
    dl
}

/// Send `path` from A to B and accept it on B; the share id.
fn send_file(ha: &mut Client, hb: &mut Client, fb: &str, path: &std::path::Path) -> String {
    let sent = send(ha, fb, json!({ "kind": "file", "path": path.to_str().unwrap() }));
    let id = sent["id"].as_str().unwrap().to_string();
    call(hb, "share.accept", json!({ "id": id }));
    id
}

fn transfer(c: &mut Client, id: &str) -> Value {
    share(c, id).map(|s| s["transfer"].clone()).unwrap_or_default()
}

/// Wait for share `id`'s transfer on this side to be over; it.
fn over(c: &mut Client, id: &str, secs: u64) -> Value {
    wait_for(secs, "the transfer is over", || Some(transfer(c, id)).filter(|t| t["state"] == "done" || t["state"] == "failed"))
}

#[test]
fn files_and_folders_land_whole_in_downloads() {
    let svc = service();
    let (a, b, _fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());
    let dl = downloads_into(&mut hb, "dl");
    let src = scratch("src");

    // One file, several chunks: the offer shows its size and checksum; nothing moves until
    // the human accepts.
    let big = bytes(3 * 192 * 1024 + 7, 1);
    std::fs::write(src.join("photo.raw"), &big).unwrap();
    let sent = send(&mut ha, &fb, json!({ "kind": "file", "path": src.join("photo.raw").to_str().unwrap() }));
    let id = sent["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    assert_eq!((got["payload"]["kind"].as_str(), got["payload"]["name"].as_str(), got["size"].as_u64()), (Some("file"), Some("photo.raw"), Some(big.len() as u64)), "{got}");
    assert_eq!(got["payload"]["files"][0]["sha256"].as_str().unwrap().len(), 64);
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    assert!(ny["item"]["title"].as_str().unwrap().contains(&nb::size_label(big.len() as u64)), "{ny}");
    assert!(ny["item"]["detail"].as_str().unwrap().contains(dl.to_str().unwrap()), "{ny}");
    stays(1, "nothing lands before accepting", || !dl.exists() && got["transfer"].is_null());
    let e = call_err(&mut b.agent(None), "share.accept", json!({ "id": id }));
    assert_eq!(e.code, HUMAN_ONLY);

    // An agent's file share asks A's human first, naming the file and its size; denied, B gets nothing.
    let before = inbox(&mut hb)["items"].as_array().unwrap().len();
    let mut agent = a.agent(None);
    let p = json!({ "peer": fb, "kind": "file", "path": src.join("photo.raw").to_str().unwrap() });
    let t = std::thread::spawn(move || agent.call_value("share.send", p));
    let item = wait_for(10, "A asks about the file", || share_asks(&mut ha).into_iter().next());
    assert_eq!(item["approval"]["action"]["value"], format!("share send file {fb}"));
    assert!(item["title"].as_str().unwrap().contains("the file “photo.raw”"), "{item}");
    assert!(item["detail"].as_str().unwrap().contains(&nb::size_label(big.len() as u64)), "{item}");
    answer(&mut ha, &item, json!({ "kind": "deny" }));
    assert_eq!(refused(t.join().unwrap()).code, midna_proto::error::REFUSED);
    assert_eq!(inbox(&mut hb)["items"].as_array().unwrap().len(), before);

    // Approving the needs-you item accepts it: the file arrives, checked, marked downloaded and
    // not executable, with progress on both sides.
    call(&mut hb, "needs_you.resolve", json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    let t = over(&mut hb, &id, 30);
    assert_eq!((t["state"].as_str(), t["done"].as_u64(), t["files_done"].as_u64()), (Some("done"), Some(big.len() as u64), Some(1)), "{t}");
    let landed = std::path::PathBuf::from(t["saved_to"].as_str().unwrap());
    assert_eq!(landed, dl.join("photo.raw"));
    assert_eq!(std::fs::read(&landed).unwrap(), big);
    let mode = std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&landed).unwrap().permissions()) & 0o777;
    assert_eq!(mode, 0o644);
    let q = std::process::Command::new("/usr/bin/xattr").arg("-p").arg("com.apple.quarantine").arg(&landed).output().unwrap();
    assert!(String::from_utf8_lossy(&q.stdout).contains("Midna"), "{q:?}");
    assert_eq!(share(&mut hb, &id).unwrap()["result"].as_str(), Some(format!("saved to {}", landed.display()).as_str()));
    assert!(!share_events(&mut hb, kinds::SHARE_PROGRESS, &id).is_empty());
    let mine = wait_for(10, "A hears it was saved", || Some(transfer(&mut ha, &id)).filter(|t| t["state"] == "done"));
    assert_eq!(mine["done"].as_u64(), Some(big.len() as u64));
    assert!(share_events(&mut ha, kinds::SHARE_PROGRESS, &id).iter().any(|e| e["data"]["transfer"]["state"] == "done"));
    assert!(!b.home.join("nearby/incoming").join(&id).exists(), "staging is cleaned up");

    // The same name again doesn't replace the first.
    let again = send_file(&mut ha, &mut hb, &fb, &src.join("photo.raw"));
    assert_eq!(over(&mut hb, &again, 30)["saved_to"].as_str(), Some(dl.join("photo 2.raw").to_str().unwrap()));
    assert_eq!(std::fs::read(dl.join("photo.raw")).unwrap(), big);

    // A folder keeps its tree, empty folders included; links in it aren't followed.
    let proj = src.join("proj");
    std::fs::create_dir_all(proj.join("src/deep")).unwrap();
    std::fs::create_dir_all(proj.join("empty")).unwrap();
    std::fs::write(proj.join("README.md"), b"# hi\n").unwrap();
    std::fs::write(proj.join("src/main.rs"), bytes(200_000, 2)).unwrap();
    std::fs::write(proj.join("src/deep/zero"), b"").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", proj.join("src/passwd")).unwrap();
    let fid = send_file(&mut ha, &mut hb, &fb, &proj);
    let offered = share(&mut hb, &fid).unwrap();
    assert_eq!(offered["payload"]["folder"], true);
    assert_eq!(offered["payload"]["files"].as_array().unwrap().len(), 3, "{offered}");
    let t = over(&mut hb, &fid, 30);
    assert_eq!(t["state"], "done", "{t}");
    let root = dl.join("proj");
    assert_eq!(t["saved_to"].as_str(), Some(root.to_str().unwrap()));
    assert_eq!(std::fs::read(root.join("README.md")).unwrap(), b"# hi\n");
    assert_eq!(std::fs::read(root.join("src/main.rs")).unwrap(), bytes(200_000, 2));
    assert_eq!(std::fs::read(root.join("src/deep/zero")).unwrap(), b"");
    assert!(root.join("empty").is_dir());
    assert!(!root.join("src/passwd").exists());

    // A file that changes after the offer (same size, other bytes) fails cleanly: nothing
    // lands, and accepting again after it changes back fetches it whole.
    let doc = src.join("doc.txt");
    std::fs::write(&doc, b"version one").unwrap();
    let sent = send(&mut ha, &fb, json!({ "kind": "file", "path": doc.to_str().unwrap() }));
    let cid = sent["id"].as_str().unwrap().to_string();
    std::fs::write(&doc, b"version two").unwrap();
    call(&mut hb, "share.accept", json!({ "id": cid }));
    let t = over(&mut hb, &cid, 30);
    assert_eq!(t["state"], "failed", "{t}");
    assert!(t["error"].as_str().unwrap().contains("checksum"), "{t}");
    assert!(!dl.join("doc.txt").exists());
    assert!(share(&mut hb, &cid).unwrap()["result"].as_str().unwrap().starts_with("failed"));
    wait_for(10, "A hears it failed", || Some(transfer(&mut ha, &cid)).filter(|t| t["state"] == "failed"));
    std::fs::write(&doc, b"version one").unwrap();
    call(&mut hb, "share.accept", json!({ "id": cid }));
    assert_eq!(over(&mut hb, &cid, 30)["state"], "done");
    assert_eq!(std::fs::read(dl.join("doc.txt")).unwrap(), b"version one");

    // Bad sends never leave A.
    for bad in [
        json!({ "kind": "file", "path": "relative/file" }),
        json!({ "kind": "file", "path": src.join("missing").to_str().unwrap() }),
        json!({ "kind": "file" }),
        json!({ "kind": "file", "path": doc.to_str().unwrap(), "text": "x" }),
        json!({ "kind": "message", "text": "x", "path": doc.to_str().unwrap() }),
    ] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }
}

#[test]
fn a_transfer_picks_up_where_it_stopped_when_the_sender_drops_out() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let mut hb = b.human();
    let dl = downloads_into(&mut hb, "dl");
    let src = scratch("src");
    let data = bytes(40 * 192 * 1024 + 123, 3);
    std::fs::write(src.join("big.bin"), &data).unwrap();
    let mut a = a;
    let slow = |svc: String| move |cfg: &mut midnad::Config| cfg.nearby = Setup { mode: Mode::Service(svc), local_only: true, everyone_secs: 600, serve_delay: Duration::from_millis(100) };
    a.restart_with(slow(svc.clone()));
    let mut ha = a.human();
    wait_for(20, "B sees A again", || peer(&mut hb, &fa));
    wait_for(20, "A sees B again", || peer(&mut ha, &fb));
    let id = send_file(&mut ha, &mut hb, &fb, &src.join("big.bin"));

    // Partway through, A's midna stops; B keeps what it has and waits.
    let part = wait_for(30, "some of it arrived", || Some(transfer(&mut hb, &id)).filter(|t| t["done"].as_u64().unwrap_or(0) >= 5 * 192 * 1024));
    assert_eq!(part["state"], "moving", "{part}");
    a.restart_with(slow(svc.clone()));
    let waiting = wait_for(30, "B is waiting", || Some(transfer(&mut hb, &id)).filter(|t| t["state"] == "waiting" || t["resumed"].as_u64() > Some(0)));
    let staged = b.home.join("nearby/incoming").join(&id).join("root/big.bin");
    let _ = waiting;

    // A is back: B carries on from what it staged (not from the start), and the whole file
    // checks out.
    let t = over(&mut hb, &id, 60);
    assert_eq!(t["state"], "done", "{t}");
    assert!(t["resumed"].as_u64().unwrap() >= 1, "{t}");
    assert_eq!(std::fs::read(dl.join("big.bin")).unwrap(), data);
    assert!(!staged.exists());
    let mut ha = a.human();
    wait_for(20, "A hears it was saved", || Some(transfer(&mut ha, &id)).filter(|t| t["state"] == "done"));
}

#[test]
fn a_transfer_carries_on_after_the_receiver_restarts() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let mut a = a;
    let slow = Setup { mode: Mode::Service(svc.clone()), local_only: true, everyone_secs: 600, serve_delay: Duration::from_millis(100) };
    a.restart_with(move |cfg| cfg.nearby = slow);
    let (mut ha, mut hb) = (a.human(), b.human());
    let dl = downloads_into(&mut hb, "dl");
    let src = scratch("src");
    let data = bytes(30 * 192 * 1024, 4);
    std::fs::write(src.join("movie.mov"), &data).unwrap();
    wait_for(20, "A sees B again", || peer(&mut ha, &fb));
    wait_for(20, "B sees A again", || peer(&mut hb, &fa));
    let id = send_file(&mut ha, &mut hb, &fb, &src.join("movie.mov"));
    wait_for(30, "some of it arrived", || Some(transfer(&mut hb, &id)).filter(|t| t["done"].as_u64().unwrap_or(0) >= 4 * 192 * 1024));

    let mut b = b;
    restart_nearby(&mut b, &svc);
    let mut hb = b.human();
    let t = over(&mut hb, &id, 60);
    assert_eq!(t["state"], "done", "{t}");
    assert!(t["resumed"].as_u64().unwrap() >= 1, "{t}");
    assert_eq!(std::fs::read(dl.join("movie.mov")).unwrap(), data);
}

#[test]
fn a_peer_cant_reach_outside_the_folder_or_its_own_shares() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let _ = fb;
    let mut hb = b.human();
    let dl = downloads_into(&mut hb, "dl");
    let a_key = identity::load_or_create(&a.home);
    let mut ch = transport::connect(&addr(port(&mut hb)), &Local::new(&a_key)).unwrap();
    let sha = "0".repeat(64);
    let file = |path: &str| json!({ "path": path, "size": 1, "sha256": sha });
    let offers = [
        json!({ "kind": "file", "name": "..", "files": [file("..")] }),
        json!({ "kind": "file", "name": "../x", "files": [file("../x")] }),
        json!({ "kind": "file", "name": "x", "files": [file("../../x")] }),
        json!({ "kind": "file", "name": "x", "files": [file("x"), file("y")] }),
        json!({ "kind": "file", "name": "x", "files": [] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("../../.ssh/authorized_keys")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("/etc/passwd")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("a//b")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("a/./b")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("a\nb")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("Read"), file("read")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("a"), file("a/b")] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [file("a")], "dirs": ["../up"] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [{ "path": "a", "size": 1, "sha256": "zz" }] }),
        json!({ "kind": "file", "name": "d", "folder": true, "files": [{ "path": "a", "size": u64::MAX, "sha256": sha }, file("b")] }),
    ];
    for (n, payload) in offers.iter().enumerate() {
        let got = ask(&mut ch, &Wire::ShareOffer { id: format!("sh_bad{n:07}"), payload: payload.clone() });
        assert!(matches!(got, Wire::Refused { .. }), "{payload}: {got:?}");
    }
    assert!(inbox(&mut hb)["items"].as_array().unwrap().is_empty());

    // A file named to look like a path still lands inside the folder, under a safe name.
    let ok = json!({ "kind": "file", "name": "a:b", "files": [file("a:b")] });
    assert_eq!(ask(&mut ch, &Wire::ShareOffer { id: "sh_colon0001".into(), payload: ok }), Wire::ShareAck { id: "sh_colon0001".into() });
    let ny = share(&mut hb, "sh_colon0001").unwrap();
    assert!(ny["needs_you_id"].is_string());

    // Fetching: only the files of a share this Mac offered that device, in range.
    let mut hb2 = b.human();
    let src = scratch("src");
    std::fs::write(src.join("secret.txt"), b"for A only").unwrap();
    let mine = send(&mut hb2, &fa, json!({ "kind": "file", "path": src.join("secret.txt").to_str().unwrap() }))["id"].as_str().unwrap().to_string();
    for (id, file) in [("sh_colon0001", 0), ("sh_nothere01", 0), (mine.as_str(), 1), (mine.as_str(), usize::MAX)] {
        let got = ask(&mut ch, &Wire::FileGet { id: id.into(), file, offset: 0 });
        assert!(matches!(got, Wire::Refused { .. }), "{id} {file}: {got:?}");
    }
    let past = ask(&mut ch, &Wire::FileGet { id: mine.clone(), file: 0, offset: 1000 });
    assert!(matches!(past, Wire::Refused { .. }), "{past:?}");
    ch.send(&Wire::FileGet { id: mine.clone(), file: 0, offset: 4 }).unwrap();
    let Wire::FileChunk { data, offset, .. } = ch.expect(Duration::from_secs(10)).unwrap() else { panic!("no chunk") };
    assert_eq!(offset, 4);
    assert_eq!(data, "QSBvbmx5", "from the offset on");
    assert_eq!(ch.expect(Duration::from_secs(10)).unwrap(), Wire::FileEnd { id: mine.clone(), file: 0 });

    // A FileDone for a share A never got changes nothing on B.
    let done = ask(&mut ch, &Wire::FileDone { id: "sh_colon0001".into(), ok: true, error: None });
    assert!(matches!(done, Wire::ShareAck { .. }));
    assert!(share(&mut hb, "sh_colon0001").unwrap()["transfer"].is_null());
    assert!(!dl.exists());
}

// ---- tabs ----

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git").args(args).current_dir(dir).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// A checkout of `remote` at `dir`, with a `src` folder in it.
fn checkout(dir: &std::path::Path, remote: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", remote]);
    std::fs::canonicalize(dir).unwrap()
}

/// The terminal a tab share's result names (`opened as t_… in …`).
fn opened(result: &Value) -> String {
    let r = result["result"].as_str().unwrap();
    r.strip_prefix("opened as ").and_then(|r| r.split(' ').next()).unwrap_or_else(|| panic!("{result}")).to_string()
}

#[test]
fn a_tab_opens_in_the_receivers_checkout_with_its_scrollback_and_nothing_runs() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());
    let _ = fa;

    // A's terminal sits in src of its checkout, cloned over ssh; B has the same repo, cloned
    // over https, as a project somewhere else.
    let theirs = checkout(&a.home.join("w/widget"), "git@github.com:Acme/widget.git");
    let ours = checkout(&b.home.join("code/widget-clone"), "https://github.com/acme/widget");
    let pid = call(&mut hb, "project.add", json!({ "path": ours, "name": "widget" }))["id"].as_str().unwrap().to_string();
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": theirs.join("src"), "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    call(&mut ha, "session.input", json!({ "id": t, "text": "echo tab-$((6*7))-marker; touch ran-here", "enter": true }));
    wait_for(10, "A's terminal shows the marker", || read(&mut ha, &t).contains("tab-42-marker").then_some(()));

    // Kinds that aren't tabs can't name a terminal; a tab can't carry text.
    for bad in [json!({ "kind": "message", "text": "hi", "session": t }), json!({ "kind": "tab", "text": "hi", "session": t })] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }

    let sent = send(&mut ha, &fb, json!({ "kind": "tab", "session": t, "title": "widget api" }));
    let id = sent["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    let p = &got["payload"];
    assert_eq!((p["kind"].as_str(), p["title"].as_str(), p["shell"].as_str()), (Some("tab"), Some("widget api"), Some("sh")), "{got}");
    assert_eq!((p["project"]["remote"].as_str(), p["project"]["path"].as_str()), (Some("github.com/acme/widget"), Some("src")), "{got}");
    assert!(p["scrollback"].as_str().unwrap().contains("tab-42-marker"), "{got}");
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    let detail = ny["item"]["detail"].as_str().unwrap();
    assert!(ny["item"]["title"].as_str().unwrap().contains("widget api"), "{ny}");
    assert!(detail.contains(&format!("It opens in {} (project widget)", ours.join("src").display())), "{detail}");

    // An agent on B can't take it; the human's approval opens a new tab in B's checkout.
    assert_eq!(call_err(&mut b.agent(None), "share.accept", json!({ "id": id })).code, HUMAN_ONLY);
    let before = call(&mut hb, "session.list", json!({})).as_array().map_or(0, Vec::len);
    let approve = json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    call(&mut hb, "needs_you.resolve", approve);
    let done = wait_for(10, "accepted", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    let sid = opened(&done);
    let s = call(&mut hb, "session.get", json!({ "id": sid }));
    assert_eq!((s["cwd"].as_str(), s["project_id"].as_str(), s["name"].as_str()), (ours.join("src").to_str(), Some(pid.as_str()), Some("widget api")), "{s}");
    assert_eq!(s["kind"], "shell");
    let shown = read(&mut hb, &sid);
    assert!(shown.contains("sent from Alpha") && shown.contains("tab-42-marker"), "{shown}");
    assert_eq!(call(&mut hb, "session.list", json!({})).as_array().map_or(0, Vec::len), before + 1);
    stays(1, "nothing from A's terminal ran on B", || !ours.join("src/ran-here").exists());
    wait_for(20, "A hears B accepted", || share(&mut ha, &id).filter(|s| s["state"] == "accepted"));

    // A repo B doesn't have opens in home with the scrollback; the human can pick the folder.
    let other = checkout(&a.home.join("w/gadget"), "https://github.com/acme/gadget.git");
    let t2 = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": other, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    call(&mut ha, "session.input", json!({ "id": t2, "text": "echo gadget-$((2+3))", "enter": true }));
    wait_for(10, "A's second terminal", || read(&mut ha, &t2).contains("gadget-5").then_some(()));
    let unknown = send(&mut ha, &fb, json!({ "kind": "tab", "session": t2 }))["id"].as_str().unwrap().to_string();
    let ny = share(&mut hb, &unknown).unwrap()["needs_you_id"].clone();
    let item = call(&mut hb, "needs_you.get", json!({ "id": ny }));
    assert!(item["item"]["detail"].as_str().unwrap().contains("github.com/acme/gadget isn't checked out"), "{item}");
    let e = call_err(&mut hb, "share.accept", json!({ "id": unknown, "cwd": "relative/dir" }));
    assert_eq!(e.code, midna_proto::error::BAD_PARAMS, "{}", e.message);
    let e = call_err(&mut hb, "share.accept", json!({ "id": unknown, "session": sid }));
    assert_eq!(e.code, midna_proto::error::BAD_PARAMS, "{}", e.message);
    let picked = std::fs::canonicalize(b.home.join("code")).unwrap();
    let r = call(&mut hb, "share.accept", json!({ "id": unknown, "cwd": picked }));
    let s2 = call(&mut hb, "session.get", json!({ "id": opened(&r) }));
    assert_eq!(s2["cwd"].as_str(), picked.to_str(), "{s2}");
    wait_for(10, "the scrollback shows", || read(&mut hb, &opened(&r)).contains("gadget-5").then_some(()));
    let home = send(&mut ha, &fb, json!({ "kind": "tab", "session": t2 }))["id"].as_str().unwrap().to_string();
    let r = call(&mut hb, "share.accept", json!({ "id": home }));
    assert!(r["result"].as_str().unwrap().contains("home folder"), "{r}");
    assert_eq!(call(&mut hb, "session.get", json!({ "id": opened(&r) }))["project_id"], midna_proto::ROOT_PROJECT_ID);

    // `cwd` is for tabs only.
    let m = send(&mut ha, &fb, json!({ "kind": "message", "text": "hi" }))["id"].as_str().unwrap().to_string();
    assert_eq!(call_err(&mut hb, "share.accept", json!({ "id": m, "cwd": picked })).code, midna_proto::error::BAD_PARAMS);
}

#[test]
fn a_tab_whose_scrollback_could_drive_the_terminal_is_refused() {
    let svc = service();
    let (a, b, _fa, _fb) = paired_pair(&svc);
    let mut hb = b.human();
    let a_key = identity::load_or_create(&a.home);
    let mut ch = transport::connect(&addr(port(&mut hb)), &Local::new(&a_key)).unwrap();
    let tab = |scrollback: &str, cwd: &str, remote: &str| {
        json!({ "kind": "tab", "title": "t", "cwd": cwd, "shell": "zsh", "scrollback": scrollback,
                "project": { "remote": remote, "path": "src" } })
    };
    for (n, payload) in [
        tab("ok\x1b[6n", "/w", "github.com/a/b"),
        tab("ok\x1b]52;c;cm0gLXJmIC8=\x07", "/w", "github.com/a/b"),
        tab("ok", "relative", "github.com/a/b"),
        tab("ok", "/w", "github.com/a/b\nx"),
        tab(&"x".repeat(nb::MAX_SCROLLBACK + 1), "/w", "github.com/a/b"),
        json!({ "kind": "tab", "title": "t", "cwd": "/w", "scrollback": "", "project": { "remote": "github.com/a/b", "path": "../.." } }),
        json!({ "kind": "tab", "title": "t", "cwd": "/w", "shell": "/bin/rm", "scrollback": "" }),
    ]
    .into_iter()
    .enumerate()
    {
        let got = ask(&mut ch, &Wire::ShareOffer { id: format!("sh_badtab{n:02}"), payload: payload.clone() });
        assert!(matches!(got, Wire::Refused { .. }), "{payload}: {got:?}");
    }
    assert!(inbox(&mut hb)["items"].as_array().unwrap().is_empty());
}

/// The kinds this Mac auto-accepts from paired device `fp`.
fn auto_accepts(c: &mut Client, fp: &str) -> Vec<String> {
    let p = paired(c).into_iter().find(|p| p["fingerprint"] == fp).unwrap_or_else(|| panic!("{fp} isn't paired"));
    p["auto_accept"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect()
}

/// Send from B and wait until A has it; the share as A has it.
fn offered(hb: &mut Client, ha: &mut Client, fa: &str, p: Value) -> Value {
    let id = send(hb, fa, p)["id"].as_str().unwrap().to_string();
    wait_for(10, "A has it", || share(ha, &id))
}

/// Wait until A has decided share `id` by itself, or raised its approval.
fn settled(ha: &mut Client, id: &str) -> Value {
    wait_for(10, "accepted or asked", || share(ha, id).filter(|s| s["state"] != "offered" || s["needs_you_id"].is_string()))
}

#[test]
fn only_my_devices_auto_accept_and_never_rules_or_settings() {
    let svc = service();
    let (a, b, fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());
    wait_for(10, "A sees B", || peer(&mut ha, &fb));
    // A keeps B as one of its own devices; B keeps A as a contact.
    let set = |c: &mut Client, fp: &str, kind: &str, on: bool| call(c, "peers.set_auto_accept", json!({ "fingerprint": fp, "kind": kind, "on": on }));
    assert!(auto_accepts(&mut ha, &fb).is_empty(), "all off by default");

    // Only the human turns it on, only for a paired device, never for a contact.
    let e = call_err(&mut a.agent(None), "peers.set_auto_accept", json!({ "fingerprint": fb, "kind": "message", "on": true }));
    assert_eq!(e.code, HUMAN_ONLY, "{}", e.message);
    assert_eq!(call_err(&mut ha, "peers.set_auto_accept", json!({ "fingerprint": "nope", "kind": "message", "on": true })).code, NOT_FOUND);
    let e = call_err(&mut hb, "peers.set_auto_accept", json!({ "fingerprint": fa, "kind": "message", "on": true }));
    assert_eq!(e.code, REFUSED, "{}", e.message);
    assert!(e.message.contains("contact"), "{}", e.message);
    assert!(auto_accepts(&mut hb, &fa).is_empty());
    // An agent session always asks: accepting it starts an agent here.
    let e = call_err(&mut ha, "peers.set_auto_accept", json!({ "fingerprint": fb, "kind": "agent", "on": true }));
    assert_eq!(e.code, midna_proto::error::BAD_PARAMS, "{}", e.message);

    // Off: a message from B waits for A's person.
    let first = offered(&mut hb, &mut ha, &fa, json!({ "kind": "message", "text": "ask me" }));
    let first = settled(&mut ha, first["id"].as_str().unwrap());
    assert_eq!(first["state"], "offered", "{first}");

    // On for messages: one is read as it arrives, with no approval, and B hears it.
    let changed = set(&mut ha, &fb, "message", true);
    assert_eq!(changed["auto_accept"], json!(["message"]), "{changed}");
    assert!(!events(&mut ha, kinds::PEERS_CHANGED).is_empty());
    let got = offered(&mut hb, &mut ha, &fa, json!({ "kind": "message", "text": "just read it" }));
    let id = got["id"].as_str().unwrap().to_string();
    let done = wait_for(10, "auto-accepted", || share(&mut ha, &id).filter(|s| s["state"] == "accepted"));
    assert_eq!((done["result"].as_str(), done["auto"].as_bool()), (Some("read"), Some(true)), "{done}");
    assert!(done["needs_you_id"].is_null(), "{done}");
    let asks = call(&mut ha, "needs_you.list", json!({}));
    assert!(!asks.to_string().contains(&id), "nothing asked: {asks}");
    assert_eq!(share_events(&mut ha, kinds::SHARE_ACCEPTED, &id).len(), 1);
    assert_eq!(inbox(&mut ha)["waiting"], 1, "only the first one waits");
    let theirs = wait_for(20, "B hears A read it", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    assert!(theirs["auto"].is_null(), "the sender's copy isn't marked: {theirs}");

    // A kind that isn't on still asks (a link is never opened by itself here).
    let link = offered(&mut hb, &mut ha, &fa, json!({ "kind": "link", "url": "https://example.com/never-opened" }));
    let link = settled(&mut ha, link["id"].as_str().unwrap());
    assert_eq!(link["state"], "offered", "{link}");

    // Midna's own things: a theme is taken; rules and settings wait, even ones anyone may change.
    set(&mut ha, &fb, "object", true);
    std::fs::create_dir_all(b.home.join("themes")).unwrap();
    std::fs::write(b.home.join("themes/pinky.json"), r##"{"name":"Pinky","extends":"nord","colors":{"accent":"#FF79C6"}}"##).unwrap();
    let theme = offered(&mut hb, &mut ha, &fa, json!({ "kind": "object", "object": "theme", "ids": ["pinky"] }));
    let theme = wait_for(10, "theme auto-accepted", || share(&mut ha, theme["id"].as_str().unwrap()).filter(|s| s["state"] == "accepted"));
    assert_eq!((theme["result"].as_str(), theme["auto"].as_bool()), (Some("added 1 (theme)"), Some(true)), "{theme}");
    assert!(a.home.join("themes/pinky.json").exists());

    let rule = call(&mut hb, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "cli", "pattern": "deploy prod*" } }));
    call(&mut hb, "settings.set", json!({ "key": "summary.keep_days", "value": 30 }));
    for object in [json!({ "object": "rules", "ids": [rule["id"]] }), json!({ "object": "settings", "ids": ["summary.keep_days"] })] {
        let mut p = object.clone();
        p["kind"] = json!("object");
        let got = offered(&mut hb, &mut ha, &fa, p);
        let got = settled(&mut ha, got["id"].as_str().unwrap());
        assert_eq!(got["state"], "offered", "{object}: {got}");
        let item = call(&mut ha, "needs_you.get", json!({ "id": got["needs_you_id"] }))["item"].clone();
        assert!(item["detail"].as_str().unwrap().contains("always ask"), "{item}");
    }
    let rules = call(&mut ha, "rule.list", json!({}));
    assert!(!rules.to_string().contains("deploy prod"), "{rules}");
    assert_eq!(call(&mut ha, "settings.get", json!({ "key": "summary.keep_days" }))["value"], 7);

    // Kept as a contact: all of it is off, and stays off when it's one of my devices again.
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "contact" }));
    assert!(auto_accepts(&mut ha, &fb).is_empty());
    assert_eq!(call_err(&mut ha, "peers.set_auto_accept", json!({ "fingerprint": fb, "kind": "message", "on": true })).code, REFUSED);
    let back = call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "my_device" }));
    assert_eq!(back["auto_accept"], Value::Null, "{back}");
    let after = offered(&mut hb, &mut ha, &fa, json!({ "kind": "message", "text": "ask again" }));
    assert_eq!(settled(&mut ha, after["id"].as_str().unwrap())["state"], "offered");

    // Forgotten: nothing to set.
    set(&mut ha, &fb, "message", true);
    call(&mut ha, "peers.forget", json!({ "fingerprint": fb }));
    assert_eq!(call_err(&mut ha, "peers.set_auto_accept", json!({ "fingerprint": fb, "kind": "message", "on": true })).code, NOT_FOUND);
}

/// A stand-in agent that prints the arguments it was started with and waits.
fn fake_agent(tag: &str) -> String {
    let bin = std::path::PathBuf::from(format!("/tmp/midna-nb-{}-{tag}.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nfor a; do echo \"ARG:$a\"; done\necho ARGS-END\nsleep 60\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    bin.to_string_lossy().into_owned()
}

/// The arguments the stand-in agent in terminal `sid` got.
fn agent_args(c: &mut Client, sid: &str) -> Vec<String> {
    let text = wait_for(10, "the agent's arguments", || Some(read(c, sid)).filter(|t| t.contains("ARGS-END")));
    text.lines().filter_map(|l| l.trim().strip_prefix("ARG:")).map(str::to_string).collect()
}

/// Claude Code's folder under `projects/` for directory `dir`.
fn claude_folder(dir: &std::path::Path) -> String {
    dir.to_str().unwrap().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Fixture transcript `name` with its checkout `@CWD@` and conversation `@CONV@` filled in.
fn fixture(name: &str, cwd: &std::path::Path, conv: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    std::fs::read_to_string(path).unwrap().replace("@CWD@", cwd.to_str().unwrap()).replace("@CONV@", conv)
}

/// The JSON lines of a transcript.
fn lines_of(text: &str) -> Vec<Value> {
    text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"))).collect()
}

fn agents(c: &mut Client) -> Vec<Value> {
    call(c, "session.list", json!({})).as_array().unwrap().iter().filter(|s| s["kind"] == "agent").cloned().collect()
}

/// Wait for agent share `id` to be resumed here; its share.
fn resumed(c: &mut Client, id: &str) -> Value {
    wait_for(20, "the agent session is resumed", || {
        share(c, id).filter(|s| s["result"].as_str().is_some_and(|r| r.starts_with("opened as")) || s["transfer"]["state"] == "failed")
    })
}

#[test]
fn an_agent_session_resumes_on_the_receiver_only_once_the_human_accepts() {
    let bin = fake_agent("claude");
    let svc = service();
    let (a, b, _fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let theirs = checkout(&a.home.join("w/widget"), "git@github.com:Acme/widget.git");
    let ours = checkout(&b.home.join("code/widget-clone"), "https://github.com/acme/widget");
    let pid = call(&mut hb, "project.add", json!({ "path": ours, "name": "widget" }))["id"].as_str().unwrap().to_string();
    let conv = "6f1c2d3e-4a5b-4c6d-8e7f-0a1b2c3d4e5f";
    let t = call(&mut ha, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": theirs.join("src") }))["id"].as_str().unwrap().to_string();

    // Until the agent says which conversation it's in, there is nothing to send.
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "agent", "session": t }));
    assert_eq!(e.code, midna_proto::error::CONFLICT, "{}", e.message);
    let folder = a.home.join("agent-config/claude/projects").join(claude_folder(&theirs.join("src")));
    std::fs::create_dir_all(&folder).unwrap();
    let transcript = folder.join(format!("{conv}.jsonl"));
    let text = fixture("nearby-claude-session.jsonl", &theirs, conv);
    std::fs::write(&transcript, &text).unwrap();
    let start = json!({ "session_id": conv, "transcript_path": transcript, "cwd": theirs.join("src") });
    call(&mut a.agent(Some(&t)), "agent.hook", json!({ "agent": "claude", "event": "SessionStart", "payload": start }));

    // A shell isn't an agent session, and an agent session carries no text.
    let sh = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": theirs, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    for bad in [json!({ "kind": "agent", "session": sh }), json!({ "kind": "agent", "session": t, "text": "hi" })] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }

    // The offer names the conversation and carries the transcript's whole lines.
    let sent = send(&mut ha, &fb, json!({ "kind": "agent", "session": t, "title": "widget api" }));
    let id = sent["id"].as_str().unwrap().to_string();
    let whole = &text[..=text.rfind('\n').unwrap()];
    let got = share(&mut hb, &id).unwrap();
    let p = &got["payload"];
    assert_eq!((p["kind"].as_str(), p["agent"].as_str(), p["title"].as_str()), (Some("agent"), Some("claude"), Some("widget api")), "{got}");
    assert_eq!((p["conversation"].as_str(), p["cwd"].as_str()), (Some(conv), theirs.join("src").to_str()), "{got}");
    assert_eq!((p["project"]["remote"].as_str(), p["project"]["path"].as_str()), (Some("github.com/acme/widget"), Some("src")), "{got}");
    assert_eq!(p["transcript"]["size"].as_u64(), Some(whole.len() as u64), "{got}");
    assert_eq!(p["transcript"]["path"].as_str(), Some(format!("{conv}.jsonl").as_str()), "{got}");
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    assert!(ny["item"]["title"].as_str().unwrap().contains("sent an agent session: widget api"), "{ny}");
    let detail = ny["item"]["detail"].as_str().unwrap();
    assert!(detail.contains("every prompt") && detail.contains(&ours.join("src").display().to_string()), "{detail}");

    // Nothing lands or starts before the human says so, and an agent on B can't say so.
    let placed = b.home.join("agent-config/claude/projects").join(claude_folder(&ours.join("src"))).join(format!("{conv}.jsonl"));
    stays(1, "nothing placed or resumed before accepting", || !placed.exists() && agents(&mut hb).is_empty());
    assert_eq!(call_err(&mut b.agent(None), "share.accept", json!({ "id": id })).code, HUMAN_ONLY);
    let approve = json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    call(&mut hb, "needs_you.resolve", approve);
    let done = resumed(&mut hb, &id);
    let sid = opened(&done);

    // The transcript sits where Claude Code looks for B's checkout, its paths made B's (a
    // sibling folder's left alone), and a new agent terminal resumes the conversation.
    let want = fixture("nearby-claude-session.jsonl", &ours, conv).replace(&format!("{}-old", ours.display()), &format!("{}-old", theirs.display()));
    let want = lines_of(&want[..=want.rfind('\n').unwrap()]);
    assert_eq!(lines_of(&std::fs::read_to_string(&placed).unwrap()), want);
    let s = call(&mut hb, "session.get", json!({ "id": sid }));
    assert_eq!((s["kind"].as_str(), s["agent"].as_str(), s["name"].as_str()), (Some("agent"), Some("claude"), Some("widget api")), "{s}");
    assert_eq!((s["cwd"].as_str(), s["project_id"].as_str()), (ours.join("src").to_str(), Some(pid.as_str())), "{s}");
    let args = agent_args(&mut hb, &sid);
    let at = args.iter().position(|a| a == "--resume").unwrap_or_else(|| panic!("{args:?}"));
    assert_eq!(args.get(at + 1).map(String::as_str), Some(conv), "{args:?}");
    assert!(read(&mut hb, &sid).contains("conversation sent from Alpha"));
    assert_eq!(agents(&mut hb).len(), 1);

    // A hears it went, and drops its copy of the transcript.
    wait_for(20, "A hears B accepted", || share(&mut ha, &id).filter(|s| s["state"] == "accepted" && s["transfer"]["state"] == "done"));
    wait_for(10, "A drops its copy", || (!a.home.join("nearby/outgoing").join(&id).exists()).then_some(()));

    // The same conversation again can't be taken while B's terminal runs it.
    let again = send(&mut ha, &fb, json!({ "kind": "agent", "session": t }))["id"].as_str().unwrap().to_string();
    let e = call_err(&mut hb, "share.accept", json!({ "id": again }));
    assert!(e.code == midna_proto::error::CONFLICT && e.message.contains(&sid), "{}", e.message);
    assert_eq!(agents(&mut hb).len(), 1);
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_codex_session_resumes_in_the_folder_the_human_picks() {
    let bin = fake_agent("codex");
    let svc = service();
    let (a, b, _fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    std::fs::create_dir_all(a.home.join("w/notes")).unwrap();
    let theirs = std::fs::canonicalize(a.home.join("w/notes")).unwrap();
    let conv = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    let t = call(&mut ha, "session.open", json!({ "kind": "agent", "agent": "codex", "cwd": theirs }))["id"].as_str().unwrap().to_string();
    let rel = format!("2026/11/12/rollout-2026-11-12T10-00-00-{conv}.jsonl");
    let rollout = a.home.join("agent-config/codex/sessions").join(&rel);
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(&rollout, fixture("nearby-codex-rollout.jsonl", &theirs, conv)).unwrap();
    call(&mut a.agent(Some(&t)), "agent.hook", json!({ "agent": "codex", "event": "agent-turn-complete", "payload": { "thread-id": conv, "cwd": theirs } }));

    let id = send(&mut ha, &fb, json!({ "kind": "agent", "session": t }))["id"].as_str().unwrap().to_string();
    let p = share(&mut hb, &id).unwrap()["payload"].clone();
    assert_eq!((p["agent"].as_str(), p["conversation"].as_str(), p["transcript"]["path"].as_str()), (Some("codex"), Some(conv), Some(rel.as_str())), "{p}");
    assert!(p["project"].is_null(), "{p}");

    // B already has a file there: it's kept beside the new one.
    let placed = b.home.join("agent-config/codex/sessions").join(&rel);
    std::fs::create_dir_all(placed.parent().unwrap()).unwrap();
    std::fs::write(&placed, "old\n").unwrap();
    std::fs::create_dir_all(b.home.join("picked")).unwrap();
    let picked = std::fs::canonicalize(b.home.join("picked")).unwrap();
    assert_eq!(call_err(&mut hb, "share.accept", json!({ "id": id, "cwd": "relative" })).code, midna_proto::error::BAD_PARAMS);
    let r = call(&mut hb, "share.accept", json!({ "id": id, "cwd": picked }));
    assert!(r["result"].as_str().unwrap().starts_with("receiving the transcript"), "{r}");
    let sid = opened(&resumed(&mut hb, &id));
    assert_eq!(lines_of(&std::fs::read_to_string(&placed).unwrap()), lines_of(&fixture("nearby-codex-rollout.jsonl", &picked, conv)));
    let kept = placed.with_file_name(format!("{}.before-{id}", placed.file_name().unwrap().to_str().unwrap()));
    assert_eq!(std::fs::read_to_string(kept).unwrap(), "old\n");
    let s = call(&mut hb, "session.get", json!({ "id": sid }));
    assert_eq!((s["kind"].as_str(), s["agent"].as_str(), s["cwd"].as_str()), (Some("agent"), Some("codex"), picked.to_str()), "{s}");
    let args = agent_args(&mut hb, &sid);
    assert_eq!((args.first().map(String::as_str), args.last().map(String::as_str)), (Some("resume"), Some(conv)), "{args:?}");
    let _ = std::fs::remove_file(bin);
}

/// The answers to agent start `id` in this inbox.
fn answers_to(c: &mut Client, id: &str) -> Vec<Value> {
    inbox(c)["items"].as_array().unwrap().iter().filter(|s| s["direction"] == "incoming" && s["payload"]["reply_to"] == id).cloned().collect()
}

#[test]
fn an_agent_start_runs_only_once_accepted_and_its_answer_comes_back() {
    let bin = fake_agent("start");
    let svc = service();
    let (a, b, _fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let theirs = checkout(&a.home.join("w/widget"), "git@github.com:Acme/widget.git");
    let ours = checkout(&b.home.join("code/widget-clone"), "https://github.com/acme/widget");
    let pid = call(&mut hb, "project.add", json!({ "path": ours, "name": "widget" }))["id"].as_str().unwrap().to_string();
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": theirs.join("src"), "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    let prompt = "Why does the widget build fail on this Mac?";

    // A start needs a prompt and carries no text.
    for bad in [json!({ "kind": "agent_start", "session": t }), json!({ "kind": "agent_start", "session": t, "prompt": prompt, "text": "hi" })] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }

    // An agent's start leaves only with A's human's OK, who sees the prompt.
    let mut c = a.agent(Some(&t));
    let p = json!({ "peer": fb, "kind": "agent_start", "session": t, "prompt": prompt, "want_reply": true });
    let bg = std::thread::spawn(move || c.call_value("share.send", p));
    let item = wait_for(10, "A asks", || share_asks(&mut ha).into_iter().next());
    assert_eq!(item["approval"]["action"]["value"], format!("share send agent_start {fb}"));
    assert!(item["title"].as_str().unwrap().contains("Start a Claude Code agent on Bravo"), "{item}");
    assert!(item["detail"].as_str().unwrap().contains(prompt), "{item}");
    answer(&mut ha, &item, json!({ "kind": "approve", "scope": { "kind": "once" } }));
    let id = bg.join().unwrap().expect("sent once approved")["id"].as_str().unwrap().to_string();

    // B's human sees the prompt and where it would run; nothing runs before they accept.
    let got = wait_for(10, "B has it", || share(&mut hb, &id));
    let p = &got["payload"];
    assert_eq!((p["kind"].as_str(), p["agent"].as_str(), p["prompt"].as_str()), (Some("agent_start"), Some("claude"), Some(prompt)), "{got}");
    assert_eq!((p["want_reply"].as_bool(), p["project"]["remote"].as_str(), p["project"]["path"].as_str()), (Some(true), Some("github.com/acme/widget"), Some("src")), "{got}");
    assert_eq!(got["state"], "offered", "{got}");
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    assert!(ny["item"]["title"].as_str().unwrap().contains("asks Claude Code here"), "{ny}");
    let detail = ny["item"]["detail"].as_str().unwrap();
    assert!(detail.contains(prompt) && detail.contains(&ours.join("src").display().to_string()), "{detail}");
    stays(1, "nothing starts before accepting", || agents(&mut hb).is_empty());
    assert_eq!(call_err(&mut b.agent(None), "share.accept", json!({ "id": id })).code, HUMAN_ONLY);
    let none = call(&mut ha, "share.answer", json!({ "id": id }));
    assert!(none["answer"].is_null() && none["share"]["id"] == id, "{none}");

    // Accepting opens a new agent terminal in B's checkout with the prompt.
    let approve = json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    call(&mut hb, "needs_you.resolve", approve);
    let done = wait_for(20, "the agent starts", || share(&mut hb, &id).filter(|s| s["result"].as_str().is_some_and(|r| r.starts_with("opened as"))));
    let sid = opened(&done);
    let s = call(&mut hb, "session.get", json!({ "id": sid }));
    assert_eq!((s["kind"].as_str(), s["agent"].as_str()), (Some("agent"), Some("claude")), "{s}");
    assert_eq!((s["cwd"].as_str(), s["project_id"].as_str()), (ours.join("src").to_str(), Some(pid.as_str())), "{s}");
    let args = agent_args(&mut hb, &sid);
    assert!(args.iter().any(|a| a == prompt), "{args:?}");
    wait_for(20, "A hears B accepted", || share(&mut ha, &id).filter(|s| s["state"] == "accepted"));

    // Accepting it again would run the prompt again: refused.
    let e = call_err(&mut hb, "share.accept", json!({ "id": id }));
    assert_eq!(e.code, midna_proto::error::CONFLICT, "{}", e.message);
    assert_eq!(agents(&mut hb).len(), 1);

    // The first turn ends: its last message goes back, cut to the message limit, read at once.
    let long = format!("The build needs Zig 0.15.2. {}", "x".repeat(nb::MAX_MESSAGE_TEXT));
    let mut w = a.human();
    let wid = id.clone();
    let waiter = std::thread::spawn(move || call(&mut w, "share.answer", json!({ "id": wid, "wait_secs": 30 })));
    let stop = json!({ "agent": "claude", "event": "Stop", "payload": { "last_assistant_message": long } });
    call(&mut b.agent(Some(&sid)), "agent.hook", stop);
    let r = waiter.join().unwrap();
    let ans = &r["answer"];
    let text = ans["payload"]["text"].as_str().unwrap_or_else(|| panic!("{r}"));
    assert!(text.starts_with("The build needs Zig 0.15.2.") && text.ends_with('…') && text.len() <= nb::MAX_MESSAGE_TEXT, "{} bytes", text.len());
    assert_eq!((ans["payload"]["reply_to"].as_str(), ans["direction"].as_str()), (Some(id.as_str()), Some("incoming")), "{r}");
    assert_eq!((ans["state"].as_str(), ans["auto"].as_bool()), (Some("accepted"), Some(true)), "{r}");
    assert!(ans["needs_you_id"].is_null(), "an answer asks nothing: {r}");
    let heard = events(&mut ha, kinds::SHARE_ANSWERED);
    assert!(heard.iter().any(|e| e["data"]["payload"]["reply_to"] == id), "{heard:?}");

    // Later turns send nothing more.
    let again = json!({ "agent": "claude", "event": "Stop", "payload": { "last_assistant_message": "a second answer" } });
    call(&mut b.agent(Some(&sid)), "agent.hook", again);
    stays(2, "one answer only", || answers_to(&mut ha, &id).len() == 1);

    // Only agent starts sent with want_reply have an answer to wait for.
    let msg = send(&mut ha, &fb, json!({ "kind": "message", "text": "hi" }))["id"].as_str().unwrap().to_string();
    assert_eq!(call_err(&mut ha, "share.answer", json!({ "id": msg })).code, midna_proto::error::BAD_PARAMS);
    assert_eq!(call_err(&mut ha, "share.answer", json!({ "id": "sh_nope" })).code, NOT_FOUND);
    let _ = std::fs::remove_file(bin);
}

#[test]
fn an_agent_start_whose_terminal_ends_first_still_answers() {
    // A stand-in agent that exits at once, before any turn ends.
    let bin = std::path::PathBuf::from(format!("/tmp/midna-nb-{}-quits.sh", std::process::id()));
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let svc = service();
    let (a, b, _fa, fb) = paired_pair_with(&svc, Some(bin.to_string_lossy().into_owned()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": a.home, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    let p = json!({ "kind": "agent_start", "session": t, "prompt": "hello", "agent": "codex", "want_reply": true });
    let id = send(&mut ha, &fb, p)["id"].as_str().unwrap().to_string();
    let got = wait_for(10, "B has it", || share(&mut hb, &id));
    assert_eq!(got["payload"]["agent"], "codex", "{got}");
    call(&mut hb, "share.accept", json!({ "id": id }));
    let r = call(&mut ha, "share.answer", json!({ "id": id, "wait_secs": 30 }));
    let text = r["answer"]["payload"]["text"].as_str().unwrap_or_else(|| panic!("{r}"));
    assert!(text.contains("ended before it answered"), "{text}");
    let _ = std::fs::remove_file(bin);
}

/// Keep A and B as each other's own devices.
fn both_mine(ha: &mut Client, hb: &mut Client, fa: &str, fb: &str) {
    call(ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "my_device" }));
    call(hb, "peers.set_tier", json!({ "fingerprint": fa, "as": "my_device" }));
}

/// Open an agent terminal named `name` (running the stand-in agent).
fn agent_terminal(c: &mut Client, cwd: &std::path::Path, name: &str) -> String {
    call(c, "session.open", json!({ "kind": "agent", "agent": "claude", "cwd": cwd, "name": name }))["id"].as_str().unwrap().to_string()
}

fn hook(c: &mut Client, event: &str, payload: Value) {
    call(c, "agent.hook", json!({ "agent": "claude", "event": event, "payload": payload }));
}

fn queued(c: &mut Client, sid: &str) -> Vec<Value> {
    call(c, "queue.list", json!({ "session": sid }))["items"].as_array().cloned().unwrap_or_default()
}

#[test]
fn an_agent_ask_queues_in_my_other_macs_tab_and_its_answer_comes_back() {
    let bin = fake_agent("ask");
    let svc = service();
    let (a, b, fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let sid = agent_terminal(&mut hb, &b.home, "api tests");
    let shell = call(&mut hb, "session.open", json!({ "kind": "shell", "cwd": b.home, "command": ["/bin/sh"], "name": "plain shell" }))["id"].as_str().unwrap().to_string();
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": a.home, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    let prompt = "Did the migration pass?";

    // A keeps B as its own, but B keeps A as a contact: B shows A nothing and takes no asks.
    let e = call_err(&mut ha, "peers.sessions", json!({ "peer": fb }));
    assert!(e.message.contains("only your own devices"), "B keeps A as a contact: {}", e.message);
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "agent_ask", "session": "api tests", "prompt": prompt }));
    assert!(e.message.contains("only your own devices"), "{}", e.message);
    // And a contact isn't even asked.
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "contact" }));
    let e = call_err(&mut ha, "peers.sessions", json!({ "peer": fb }));
    assert_eq!(e.code, REFUSED, "{}", e.message);
    assert!(e.message.contains("kept as a contact"), "{}", e.message);
    both_mine(&mut ha, &mut hb, &fa, &fb);

    // Mine both ways: B's agent terminals, names and ids only; shells aren't listed.
    let v = call(&mut a.agent(Some(&t)), "peers.sessions", json!({ "peer": fb }));
    assert_eq!(v["peer_name"], "Bravo", "{v}");
    let listed = v["sessions"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{v}");
    let s = &listed[0];
    assert_eq!((s["id"].as_str(), s["name"].as_str(), s["agent"].as_str(), s["busy"].as_bool()), (Some(sid.as_str()), Some("api tests"), Some("claude"), Some(false)), "{v}");
    assert!(s.get("scrollback").is_none() && s.get("text").is_none(), "{v}");

    // An ask naming no agent terminal there is refused with the reason, before anything waits.
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "agent_ask", "session": "nope", "prompt": prompt }));
    assert!(e.message.contains("no agent terminal here is called “nope”"), "{}", e.message);
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "agent_ask", "session": shell, "prompt": prompt }));
    assert!(e.message.contains("isn't an agent terminal"), "{}", e.message);
    let e = call_err(&mut ha, "share.send", json!({ "peer": fb, "kind": "agent_ask", "prompt": prompt }));
    assert_eq!(e.code, midna_proto::error::BAD_PARAMS, "{}", e.message);
    assert!(inbox(&mut hb)["items"].as_array().unwrap().iter().all(|s| s["payload"]["kind"] != "agent_ask"), "nothing was offered");

    // B's agent is busy with something else.
    hook(&mut b.agent(Some(&sid)), "UserPromptSubmit", json!({ "prompt": "earlier work" }));

    // An agent's ask leaves only with A's human's OK, whose title names the tab and the question.
    let mut c = a.agent(Some(&t));
    let p = json!({ "peer": fb, "kind": "agent_ask", "session": "API Tests", "prompt": prompt, "want_reply": true });
    let bg = std::thread::spawn(move || c.call_value("share.send", p));
    let item = wait_for(10, "A asks", || share_asks(&mut ha).into_iter().next());
    assert_eq!(item["approval"]["action"]["value"], format!("share send agent_ask {fb}"));
    assert_eq!(item["title"], "Ask “API Tests” on Bravo: Did the migration pass?", "{item}");
    assert!(item["detail"].as_str().unwrap().contains(prompt), "{item}");
    answer(&mut ha, &item, json!({ "kind": "approve", "scope": { "kind": "once" } }));
    let id = bg.join().unwrap().expect("sent once approved")["id"].as_str().unwrap().to_string();

    // B's human sees which terminal and the prompt; nothing is queued before they accept.
    let got = wait_for(10, "B has it", || share(&mut hb, &id));
    assert_eq!((got["payload"]["kind"].as_str(), got["state"].as_str()), (Some("agent_ask"), Some("offered")), "{got}");
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    assert_eq!(ny["item"]["title"], "Alpha asks “api tests” here: Did the migration pass?", "{ny}");
    assert!(ny["item"]["detail"].as_str().unwrap().contains(&sid), "{ny}");
    stays(1, "nothing queued before accepting", || queued(&mut hb, &sid).is_empty());
    assert_eq!(call_err(&mut b.agent(None), "share.accept", json!({ "id": id })).code, HUMAN_ONLY);

    // Accepting queues it in that terminal, held while its agent works.
    call(&mut hb, "needs_you.resolve", json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    let done = wait_for(10, "accepted", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    assert!(done["result"].as_str().unwrap().starts_with(&format!("queued in {sid}")), "{done}");
    let q = queued(&mut hb, &sid);
    assert_eq!((q.len(), q[0]["text"].as_str()), (1, Some(prompt)), "{q:?}");
    stays(2, "never typed into a busy agent", || !read(&mut hb, &sid).contains(prompt));
    assert_eq!(call_err(&mut hb, "share.accept", json!({ "id": id })).code, midna_proto::error::CONFLICT, "accepting again would ask again");

    // The busy turn ends: that isn't the answer. Then the queue types the prompt.
    hook(&mut b.agent(Some(&sid)), "Stop", json!({ "last_assistant_message": "earlier work done" }));
    wait_for(20, "the prompt is typed", || read(&mut hb, &sid).contains(prompt).then_some(()));
    wait_for(10, "the queue is empty", || queued(&mut hb, &sid).is_empty().then_some(()));
    stays(2, "no answer from the earlier turn", || answers_to(&mut ha, &id).is_empty());

    // The turn the prompt started ends: its last message goes back.
    let mut w = a.human();
    let wid = id.clone();
    let waiter = std::thread::spawn(move || call(&mut w, "share.answer", json!({ "id": wid, "wait_secs": 30 })));
    hook(&mut b.agent(Some(&sid)), "UserPromptSubmit", json!({ "prompt": prompt }));
    hook(&mut b.agent(Some(&sid)), "Stop", json!({ "last_assistant_message": "Yes: all 12 steps passed." }));
    let r = waiter.join().unwrap();
    assert_eq!(r["answer"]["payload"]["text"], "Yes: all 12 steps passed.", "{r}");
    assert_eq!(r["answer"]["payload"]["reply_to"], id.as_str(), "{r}");
    hook(&mut b.agent(Some(&sid)), "Stop", json!({ "last_assistant_message": "later" }));
    stays(2, "one answer only", || answers_to(&mut ha, &id).len() == 1);
    let _ = std::fs::remove_file(bin);
}

#[test]
fn an_agent_ask_that_cant_be_asked_any_more_tells_the_sender_why() {
    let bin = fake_agent("ask-gone");
    let svc = service();
    let (a, b, fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    both_mine(&mut ha, &mut hb, &fa, &fb);
    let sid = agent_terminal(&mut hb, &b.home, "worker");
    hook(&mut b.agent(Some(&sid)), "UserPromptSubmit", json!({ "prompt": "busy" }));
    let ask = |ha: &mut Client, text: &str| send(ha, &fb, json!({ "kind": "agent_ask", "session": "work", "prompt": text, "want_reply": true }))["id"].as_str().unwrap().to_string();

    // Taken out of the queue before it was typed: the sender hears so.
    let id = ask(&mut ha, "first question");
    wait_for(10, "B has it", || share(&mut hb, &id));
    call(&mut hb, "share.accept", json!({ "id": id }));
    let q = wait_for(10, "queued", || queued(&mut hb, &sid).first().cloned());
    call(&mut hb, "queue.remove", json!({ "session": sid, "id": q["id"] }));
    let r = call(&mut ha, "share.answer", json!({ "id": id, "wait_secs": 30 }));
    let text = r["answer"]["payload"]["text"].as_str().unwrap_or_else(|| panic!("{r}"));
    assert!(text.contains("taken out of worker's queue"), "{text}");

    // The terminal closed before the ask was accepted: accepting declines it, with the reason.
    let id = ask(&mut ha, "second question");
    wait_for(10, "B has it", || share(&mut hb, &id));
    call(&mut hb, "session.close", json!({ "id": sid, "force": true }));
    wait_for(10, "closed", || call(&mut hb, "session.list", json!({})).as_array().unwrap().iter().all(|s| s["id"] != sid).then_some(()));
    let item = call(&mut hb, "share.accept", json!({ "id": id }));
    assert_eq!(item["state"], "declined", "{item}");
    let r = call(&mut ha, "share.answer", json!({ "id": id, "wait_secs": 30 }));
    assert_eq!(r["share"]["state"], "declined", "{r}");
    assert!(r["answer"].is_null(), "{r}");
    assert!(r["share"]["result"].as_str().is_some_and(|w| w.contains("no agent terminal here is called “work”")), "the reason travels back: {r}");
    let _ = std::fs::remove_file(bin);
}

#[test]
fn a_tab_or_an_agent_start_opens_in_a_project_named_wherever_the_sender_is() {
    let bin = fake_agent("named");
    let svc = service();
    let (a, b, fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let dir = b.home.join("code/mudora");
    std::fs::create_dir_all(&dir).unwrap();
    let dir = std::fs::canonicalize(dir).unwrap();
    let pid = call(&mut hb, "project.add", json!({ "path": dir, "name": "mudora" }))["id"].as_str().unwrap().to_string();
    let tab = |project: &str| json!({ "kind": "tab", "project": project });

    // Only a tab or an agent start names a project, and an agent start names one place.
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    for bad in [
        json!({ "kind": "message", "text": "hi", "project": "mudora" }),
        json!({ "kind": "agent_start", "prompt": "hi", "project": "mudora", "session": t }),
        json!({ "kind": "tab", "project": "two\nlines" }),
    ] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        assert_eq!(call_err(&mut ha, "share.send", p).code, midna_proto::error::BAD_PARAMS, "{bad}");
    }

    // A contact can't name a project: not from this side, and not on the other (B keeps A as a
    // contact).
    let mut p = tab("mudora");
    p["peer"] = json!(fb);
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "contact" }));
    let e = call_err(&mut ha, "share.send", p.clone());
    assert!(e.code == REFUSED && e.message.contains("contact"), "{}", e.message);
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "my_device" }));
    let e = call_err(&mut ha, "share.send", p);
    assert!(e.code == REFUSED && e.message.contains("only your own devices"), "{}", e.message);
    call(&mut hb, "peers.set_tier", json!({ "fingerprint": fa, "as": "my_device" }));

    // A name the other Mac has no project for is refused at once, saying only that.
    let mut p = tab("nope");
    p["peer"] = json!(fb);
    let e = call_err(&mut ha, "share.send", p);
    assert!(e.code == REFUSED && e.message.contains("no project named “nope” here"), "{}", e.message);
    assert!(!e.message.contains("mudora"), "{}", e.message);

    // A new tab, sent from no terminal, opens as an empty shell in that project (any case).
    let id = send(&mut ha, &fb, tab("Mudora"))["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    let p = &got["payload"];
    assert_eq!((p["project_name"].as_str(), p["cwd"].as_str(), p["scrollback"].as_str()), (Some("Mudora"), Some(""), Some("")), "{got}");
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    let detail = ny["item"]["detail"].as_str().unwrap();
    assert!(ny["item"]["title"].as_str().unwrap().contains("opens a new tab here"), "{ny}");
    assert!(detail.contains(&format!("It opens in {} (project mudora)", dir.display())), "{detail}");
    let approve = json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    call(&mut hb, "needs_you.resolve", approve);
    let done = wait_for(10, "accepted", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    let s = call(&mut hb, "session.get", json!({ "id": opened(&done) }));
    assert_eq!((s["cwd"].as_str(), s["project_id"].as_str(), s["kind"].as_str()), (dir.to_str(), Some(pid.as_str()), Some("shell")), "{s}");
    assert!(read(&mut hb, &opened(&done)).contains("opened from Alpha"));

    // An agent start needs no terminal here either.
    let prompt = "Put the report in docs/";
    let id = send(&mut ha, &fb, json!({ "kind": "agent_start", "project": "mudora", "prompt": prompt }))["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    let r = call(&mut hb, "share.accept", json!({ "id": id }));
    let s = call(&mut hb, "session.get", json!({ "id": opened(&r) }));
    assert_eq!((s["cwd"].as_str(), s["project_id"].as_str(), s["kind"].as_str()), (dir.to_str(), Some(pid.as_str()), Some("agent")), "{got}");
    assert!(agent_args(&mut hb, &opened(&r)).iter().any(|a| a == prompt));

    // A project removed after the offer: accepting refuses, unless the human picks a folder.
    let gone = b.home.join("code/gone");
    std::fs::create_dir_all(&gone).unwrap();
    let gid = call(&mut hb, "project.add", json!({ "path": gone, "name": "gone" }))["id"].as_str().unwrap().to_string();
    let id = send(&mut ha, &fb, tab("gone"))["id"].as_str().unwrap().to_string();
    call(&mut hb, "project.remove", json!({ "id": gid }));
    let e = call_err(&mut hb, "share.accept", json!({ "id": id }));
    assert!(e.code == midna_proto::error::CONFLICT && e.message.contains("no project named “gone”"), "{}", e.message);
    let r = call(&mut hb, "share.accept", json!({ "id": id, "cwd": dir }));
    assert_eq!(call(&mut hb, "session.get", json!({ "id": opened(&r) }))["cwd"].as_str(), dir.to_str());
    let _ = std::fs::remove_file(bin);
}

#[test]
fn an_agent_start_hands_off_notes_in_its_prompt_or_as_a_file_in_midnas_home() {
    let bin = fake_agent("notes");
    let svc = service();
    let (a, b, fa, fb) = paired_pair_with(&svc, Some(bin.clone()));
    let (mut ha, mut hb) = (a.human(), b.human());
    let t = call(&mut ha, "session.open", json!({ "kind": "shell", "cwd": a.home, "command": ["/bin/sh"] }))["id"].as_str().unwrap().to_string();
    let start = |notes: &str| json!({ "kind": "agent_start", "session": t, "prompt": "Carry on with the release", "notes": notes });
    let long = format!("# Handoff\nTried the build twice.\n{}", "- a step that is left\n".repeat(1000));

    // Only my own devices take notes: not a contact here, and not where B keeps A as one.
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "contact" }));
    for notes in ["short", long.as_str()] {
        let e = call_err(&mut ha, "share.send", { let mut p = start(notes); p["peer"] = json!(fb); p });
        assert!(e.code == REFUSED && e.message.contains("only your own devices take handoff notes"), "{}", e.message);
    }
    call(&mut ha, "peers.set_tier", json!({ "fingerprint": fb, "as": "my_device" }));
    call(&mut hb, "peers.set_tier", json!({ "fingerprint": fa, "as": "contact" }));
    let e = call_err(&mut ha, "share.send", { let mut p = start(&long); p["peer"] = json!(fb); p });
    assert!(e.code == REFUSED && e.message.contains("only your own devices take handoff notes"), "{}", e.message);
    stays(1, "a refused offer leaves no copy", || std::fs::read_dir(a.home.join("nearby/outgoing")).map_or(true, |mut d| d.next().is_none()));
    both_mine(&mut ha, &mut hb, &fa, &fb);
    let e = call_err(&mut ha, "share.send", { let mut p = start("bell\x07"); p["peer"] = json!(fb); p });
    assert_eq!(e.code, midna_proto::error::BAD_PARAMS, "{}", e.message);

    // Short notes ride in the prompt.
    let id = send(&mut ha, &fb, start("Tried X.\r\nLeft: Y."))["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    assert!(got["payload"]["notes"].is_null(), "{got}");
    assert_eq!(got["payload"]["prompt"], "Carry on with the release\n\n---\nHandoff notes:\n\nTried X.\nLeft: Y.", "{got}");
    let r = call(&mut hb, "share.accept", json!({ "id": id }));
    let screen = wait_for(10, "the agent's arguments", || Some(read(&mut hb, &opened(&r))).filter(|t| t.contains("ARGS-END")));
    assert!(screen.contains("Left: Y."), "{screen}");

    // Longer ones travel as a file; the approval shows their size and first lines.
    let id = send(&mut ha, &fb, start(&long))["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).unwrap();
    let n = &got["payload"]["notes"];
    assert_eq!((n["file"]["path"].as_str(), n["file"]["size"].as_u64()), (Some(nb::NOTES_FILE), Some(long.trim().len() as u64)), "{got}");
    assert!(n["head"].as_str().unwrap().starts_with("# Handoff\nTried the build twice.\n"), "{got}");
    assert_eq!(got["payload"]["prompt"], "Carry on with the release", "{got}");
    let saved = b.home.join("nearby/notes").join(format!("{id}.md"));
    let ny = call(&mut hb, "needs_you.get", json!({ "id": got["needs_you_id"] }));
    let detail = ny["item"]["detail"].as_str().unwrap();
    assert!(detail.contains("Handoff notes (") && detail.contains("# Handoff\nTried the build twice.") && detail.contains(&saved.display().to_string()), "{detail}");
    stays(1, "nothing starts before accepting", || !saved.exists());

    // Accepting fetches them into Midna's home, then starts the agent pointing at them.
    let approve = json!({ "id": got["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } });
    call(&mut hb, "needs_you.resolve", approve);
    let done = wait_for(20, "the agent starts", || {
        share(&mut hb, &id).filter(|s| s["result"].as_str().is_some_and(|r| r.starts_with("opened as")) || s["transfer"]["state"] == "failed")
    });
    assert_eq!(done["transfer"]["state"], "done", "{done}");
    assert_eq!(std::fs::read_to_string(&saved).unwrap(), long.trim());
    assert_eq!(std::os::unix::fs::PermissionsExt::mode(&std::fs::metadata(&saved).unwrap().permissions()) & 0o777, 0o600);
    let screen = wait_for(10, "the agent's arguments", || Some(read(&mut hb, &opened(&done))).filter(|t| t.contains("ARGS-END")));
    let flat = screen.replace('\n', "");
    assert!(flat.contains(&format!("Handoff notes from Alpha are in {}. Read them first.", saved.display())), "{screen}");
    wait_for(10, "A drops its copy", || (!a.home.join("nearby/outgoing").join(&id).exists()).then_some(()));

    // Accepting again changes nothing.
    call(&mut hb, "share.accept", json!({ "id": id }));
    stays(1, "one agent per start", || agents(&mut hb).len() == 2);
    let _ = std::fs::remove_file(bin);
}
