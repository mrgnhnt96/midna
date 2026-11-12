//! Nearby object shares (`share.send` kind object): midna's own summaries, rules, triggers,
//! themes, settings and queued prompts. The sender reads them from its stores, swaps project ids
//! for names and leaves out what only it has (secrets, paths, trust, one terminal's things); the
//! receiver reviews each against its own state and applies only what the review marks `+`.
//! Rules, triggers and human-only settings take the human; triggers arrive turned off.
mod common;
use common::*;
use midna_proto::Actor;
use midna_proto::error::{BAD_PARAMS, HUMAN_ONLY};
use midna_proto::nearby::{MidnaObject, ObjectType};
use midnad::nearby::objects;
use midnad::nearby::{Mode, Setup};
use midnad::rpc::Ctx;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

fn human() -> Ctx {
    Ctx::internal_human()
}

fn agent() -> Ctx {
    Ctx::internal_trigger(Actor::agent(None))
}

fn ids(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// A shell in `<home>/<name>`, which makes (or reuses) the project named `name`.
fn shell_in(d: &TestDaemon, name: &str) -> (String, String) {
    let dir = d.home.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let s = call(&mut d.human(), "session.open", json!({ "kind": "shell", "cwd": dir, "command": ["/bin/sh"], "name": "build" }));
    (s["id"].as_str().unwrap().to_string(), s["project_id"].as_str().unwrap().to_string())
}

fn build(d: &TestDaemon, kind: ObjectType, v: &[&str]) -> MidnaObject {
    objects::build(&d.daemon(), kind, &ids(v)).unwrap_or_else(|e| panic!("build {kind:?}: {}", e.message))
}

fn build_err(d: &TestDaemon, kind: ObjectType, v: &[&str]) -> String {
    match objects::build(&d.daemon(), kind, &ids(v)) {
        Ok(o) => panic!("{kind:?} {v:?} was built: {o:?}"),
        Err(e) => {
            assert!(e.code == BAD_PARAMS || e.code == midna_proto::error::NOT_FOUND, "{}", e.message);
            e.message
        }
    }
}

fn review(d: &TestDaemon, o: &MidnaObject) -> String {
    objects::review(&d.daemon(), o)
}

fn apply(d: &TestDaemon, ctx: &Ctx, o: &MidnaObject) -> String {
    objects::apply(&d.daemon(), ctx, o, None, "Alpha").unwrap_or_else(|e| panic!("apply: {}", e.message))
}

/// What travels is what the receiver gets: the object goes through JSON on the way.
fn over_the_wire(o: &MidnaObject) -> MidnaObject {
    serde_json::from_str(&serde_json::to_string(o).unwrap()).unwrap()
}

fn rules(d: &TestDaemon) -> Vec<Value> {
    call(&mut d.human(), "rule.list", json!({})).as_array().unwrap().clone()
}

fn triggers(d: &TestDaemon) -> Vec<Value> {
    call(&mut d.human(), "trigger.list", json!({})).as_array().unwrap().iter().filter(|t| t["builtin"].is_null()).cloned().collect()
}

fn setting(d: &TestDaemon, key: &str) -> Value {
    call(&mut d.human(), "settings.get", json!({ "key": key }))["value"].clone()
}

#[test]
fn rules_arrive_by_project_name_and_only_the_human_adds_them() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    let mut ha = a.human();
    let (sid, pa) = shell_in(&a, "web");
    let everywhere = call(&mut ha, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "cli", "pattern": "git push*" } }));
    let in_web = call(&mut ha, "rule.add", json!({ "effect": "deny", "matcher": { "kind": "command", "pattern": "rm -rf*" }, "scope": { "kind": "project", "id": pa }, "expires_in_secs": 7200 }));
    let one_terminal = call(&mut ha, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "command", "pattern": "make*" }, "scope": { "kind": "session", "id": sid } }));
    let (r1, r2, r3) = (everywhere["id"].as_str().unwrap(), in_web["id"].as_str().unwrap(), one_terminal["id"].as_str().unwrap());

    assert!(build_err(&a, ObjectType::Rules, &[r3]).contains("one terminal"));
    assert!(build_err(&a, ObjectType::Rules, &[]).contains("ids"));
    assert!(build_err(&a, ObjectType::Rules, &["r_nothere"]).contains("no rule"));
    let o = over_the_wire(&build(&a, ObjectType::Rules, &[r1, r2]));
    let MidnaObject::Rules { rules: sent } = &o else { panic!("{o:?}") };
    assert_eq!(sent[1].project.as_deref(), Some("web"), "the project goes by name");
    assert!(sent[1].expires_in_secs.is_some_and(|s| s <= 7200 && s > 7000), "{sent:?}");
    assert_eq!(o.label(), "2 rules");

    // B has no project called web yet: that rule is skipped, the other is marked +.
    let r = review(&b, &o);
    assert!(r.contains("+ rule: allow cli `git push*`, everywhere"), "{r}");
    assert!(r.contains("· skipped: rule deny command `rm -rf*`: no project named “web” here"), "{r}");
    assert!(r.contains("Only you can accept this"), "{r}");
    assert!(objects::needs_human(&b.daemon(), &o));

    // Policy never comes in from an agent.
    let e = objects::apply(&b.daemon(), &agent(), &o, None, "Alpha").unwrap_err();
    assert_eq!(e.code, HUMAN_ONLY, "{}", e.message);
    assert!(rules(&b).is_empty());

    // Once B has a web project, both land; the project rule in B's own web.
    let (_, pb) = shell_in(&b, "web");
    let r = review(&b, &o);
    assert!(r.contains("+ rule: deny command `rm -rf*`, in web, for 1 hours") || r.contains("+ rule: deny command `rm -rf*`, in web, for 2 hours"), "{r}");
    assert_eq!(apply(&b, &human(), &o), "added 2 (rules)");
    let got = rules(&b);
    assert_eq!(got.len(), 2, "{got:?}");
    let web = got.iter().find(|r| r["matcher"]["pattern"] == "rm -rf*").unwrap();
    assert_eq!((web["scope"]["kind"].as_str(), web["scope"]["id"].as_str(), web["effect"].as_str()), (Some("project"), Some(pb.as_str()), Some("deny")), "{web}");
    assert!(web["expires_at"].is_string(), "{web}");
    assert_eq!(web["added_by"]["kind"], "human");

    // Accepting again adds nothing.
    assert!(review(&b, &o).ends_with("Nothing here would change."), "{}", review(&b, &o));
    assert_eq!(apply(&b, &human(), &o), "added 0 (rules), skipped 2");
    assert_eq!(rules(&b).len(), 2);
}

#[test]
fn triggers_arrive_turned_off_and_never_carry_a_secret() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    let mut ha = a.human();
    let (sid, pa) = shell_in(&a, "web");
    let gh = call(&mut ha, "trigger.add", json!({ "name": "Review new PRs", "source": "github", "event": "pull_request.opened",
        "filter": { "repo": "mrgnhnt96/midna", "project": pa },
        "action": { "kind": "start_agent", "project_id": pa, "agent": "claude", "prompt_template": "Review {{pr.title}}" } }));
    let gh = gh["id"].as_str().unwrap().to_string();
    call(&mut ha, "trigger.set_secret", json!({ "id": gh, "secret": "s3cret-value-that-stays" }));
    call(&mut ha, "trigger.set_enabled", json!({ "id": gh, "enabled": true }));
    let local = call(&mut ha, "trigger.add", json!({ "name": "Compact and resend", "source": "local", "event": "agent.prompt_blocked",
        "filter": { "match": { "message": "*compact first*" } },
        "action": { "kind": "send_to_session", "steps": [{ "text": "/compact" }] }, "enabled": true }));
    let local = local["id"].as_str().unwrap().to_string();
    assert_eq!(local_state(&a, &local), "active");
    let here = call(&mut ha, "trigger.add", json!({ "name": "This one", "source": "local", "event": "agent.prompt_blocked",
        "filter": { "session": sid }, "action": { "kind": "send_to_session", "steps": [{ "text": "x" }] } }));
    let builtin = call(&mut ha, "trigger.list", json!({})).as_array().unwrap().iter().find(|t| !t["builtin"].is_null()).unwrap()["id"].as_str().unwrap().to_string();

    assert!(build_err(&a, ObjectType::Triggers, &[here["id"].as_str().unwrap()]).contains("one terminal"));
    assert!(build_err(&a, ObjectType::Triggers, &[&builtin]).contains("built in"));
    let o = over_the_wire(&build(&a, ObjectType::Triggers, &[&gh, &local]));
    let wire = serde_json::to_string(&o).unwrap();
    assert!(!wire.contains("s3cret") && !wire.contains(&pa), "no secret and no project id leave: {wire}");

    let (_, pb) = shell_in(&b, "web");
    let r = review(&b, &o);
    assert!(r.contains("+ trigger “Review new PRs” on pull_request.opened (off, until you give it its secret)"), "{r}");
    assert!(r.contains("+ trigger “Compact and resend” on agent.prompt_blocked (off, as a draft)"), "{r}");
    assert!(r.contains("Only you can accept this"), "{r}");
    assert_eq!(objects::apply(&b.daemon(), &agent(), &o, None, "Alpha").unwrap_err().code, HUMAN_ONLY);
    assert!(triggers(&b).is_empty());

    assert_eq!(apply(&b, &human(), &o), "added 2 (triggers)");
    let got = triggers(&b);
    assert_eq!(got.len(), 2, "{got:?}");
    for t in &got {
        assert_eq!((t["enabled"].as_bool(), t["secret_set"].as_bool()), (Some(false), Some(false)), "{t}");
        assert_eq!(t["created_by"]["kind"], "human");
    }
    let gh_b = got.iter().find(|t| t["source"] == "github").unwrap();
    assert_eq!(gh_b["state"], "needs_secret", "{gh_b}");
    assert_eq!((gh_b["action"]["project_id"].as_str(), gh_b["filter"]["project"].as_str()), (Some(pb.as_str()), Some(pb.as_str())), "{gh_b}");
    assert_eq!(got.iter().find(|t| t["source"] == "local").unwrap()["state"], "draft");

    assert!(review(&b, &o).ends_with("Nothing here would change."));
    assert_eq!(apply(&b, &human(), &o), "added 0 (triggers), skipped 2");
}

fn local_state(d: &TestDaemon, id: &str) -> String {
    triggers(d).into_iter().find(|t| t["id"] == id).unwrap()["state"].as_str().unwrap().to_string()
}

#[test]
fn settings_travel_but_this_macs_own_stay_and_human_only_stays_human_only() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    let mut ha = a.human();
    call(&mut ha, "settings.set", json!({ "key": "summary.keep_days", "value": 30 }));
    call(&mut ha, "settings.set", json!({ "key": "agents.may_move_windows", "value": true }));
    call(&mut ha, "settings.set", json!({ "key": "nearby.name", "value": "Alpha" }));
    call(&mut ha, "settings.set", json!({ "key": "agents.trust_folders", "value": ["/tmp"] }));

    // No keys: every changed setting that may leave, and nothing else.
    let o = over_the_wire(&build(&a, ObjectType::Settings, &[]));
    let MidnaObject::Settings { values } = &o else { panic!("{o:?}") };
    assert_eq!(values.get("summary.keep_days"), Some(&json!(30)), "{values:?}");
    assert_eq!(values.get("agents.may_move_windows"), Some(&json!(true)), "{values:?}");
    for own in ["nearby.name", "agents.trust_folders"] {
        assert!(!values.contains_key(own), "{own} stays: {values:?}");
        assert!(build_err(&a, ObjectType::Settings, &[own]).contains("this Mac's own"), "{own}");
    }
    assert!(build_err(&a, ObjectType::Settings, &["no.such"]).contains("no setting"));

    let r = review(&b, &o);
    assert!(r.contains("+ summary.keep_days: 7 → 30"), "{r}");
    assert!(r.contains("+ agents.may_move_windows: false → true (human only)"), "{r}");
    assert!(r.contains("Only you can accept this"), "{r}");
    assert_eq!(objects::apply(&b.daemon(), &agent(), &o, None, "Alpha").unwrap_err().code, HUMAN_ONLY);
    assert_eq!(setting(&b, "summary.keep_days"), 7);

    // A device that sends this Mac's own settings anyway gets them skipped here too.
    let sneaky: MidnaObject = serde_json::from_value(json!({ "type": "settings", "values": {
        "nearby.visibility": "everyone", "agents.trust_folders": ["/"], "webhooks.relay_url": "https://evil.example",
        "summary.keep_days": "lots", "no.such": 1 } })).unwrap();
    let r = review(&b, &sneaky);
    assert_eq!(r.matches("· skipped").count(), 5, "{r}");
    assert!(r.contains("nearby.visibility: this Mac's own") && r.contains("no.such: no such setting here"), "{r}");
    assert_eq!(apply(&b, &human(), &sneaky), "added 0 (settings), skipped 5");
    assert_eq!(setting(&b, "nearby.visibility"), "paired");

    // Settings anyone may change don't need the human.
    let plain: MidnaObject = serde_json::from_value(json!({ "type": "settings", "values": { "summary.keep_days": 14 } })).unwrap();
    assert!(!objects::needs_human(&b.daemon(), &plain));
    assert_eq!(apply(&b, &agent(), &plain), "added 1 (settings)");

    assert_eq!(apply(&b, &human(), &o), "added 2 (settings)");
    assert_eq!((setting(&b, "summary.keep_days"), setting(&b, "agents.may_move_windows")), (json!(30), json!(true)));
    assert!(review(&b, &o).contains("· skipped: summary.keep_days: already 30"));
}

#[test]
fn a_theme_lands_as_a_file_and_never_overwrites_one_of_yours() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    std::fs::create_dir_all(a.home.join("themes")).unwrap();
    std::fs::write(a.home.join("themes/pinky.json"), r##"{"name":"Pinky","extends":"nord","colors":{"accent":"#FF79C6"}}"##).unwrap();
    assert!(build_err(&a, ObjectType::Theme, &["twilight"]).contains("built in"));
    assert!(build_err(&a, ObjectType::Theme, &["pinky", "pinky"]).contains("one"));
    let o = over_the_wire(&build(&a, ObjectType::Theme, &["pinky"]));
    let MidnaObject::Theme { theme } = &o else { panic!("{o:?}") };
    assert_eq!((theme.dark, theme.colors.get("accent").map(String::as_str), theme.ansi.len()), (true, Some("#FF79C6"), 16), "colors fully resolved: {theme:?}");

    let r = review(&b, &o);
    assert!(r.contains("+ the dark theme “Pinky”; pick it in Settings → Appearance"), "{r}");
    assert!(!objects::needs_human(&b.daemon(), &o));
    assert_eq!(apply(&b, &human(), &o), "added 1 (theme)");
    let listed = call(&mut b.human(), "themes.list", json!({}));
    let pinky = listed["themes"].as_array().unwrap().iter().find(|t| t["id"] == "pinky").cloned().unwrap_or_else(|| panic!("{listed}"));
    assert_eq!((pinky["colors"]["accent"].as_str(), pinky["kind"].as_str()), (Some("#FF79C6"), Some("dark")));
    assert!(listed["errors"].as_array().unwrap().is_empty(), "{listed}");
    assert!(review(&b, &o).contains("· skipped: the theme “Pinky”: you have it"));

    // A different theme of the same id comes in beside it.
    let c = TestDaemon::start();
    std::fs::create_dir_all(c.home.join("themes")).unwrap();
    let mine = r##"{"name":"Pinky","extends":"daylight","colors":{"accent":"#123456"}}"##;
    std::fs::write(c.home.join("themes/pinky.json"), mine).unwrap();
    assert!(review(&c, &o).contains("as pinky-2 (you have a different pinky)"), "{}", review(&c, &o));
    apply(&c, &human(), &o);
    assert_eq!(std::fs::read_to_string(c.home.join("themes/pinky.json")).unwrap(), mine);
    let listed = call(&mut c.human(), "themes.list", json!({}));
    assert!(listed["themes"].as_array().unwrap().iter().any(|t| t["id"] == "pinky-2" && t["colors"]["accent"] == "#FF79C6"), "{listed}");

    // A theme id that would leave the themes folder never makes it in.
    let bad: Result<MidnaObject, _> = serde_json::from_value(json!({ "type": "theme", "theme": { "id": "../x", "name": "X", "dark": true, "colors": {}, "ansi": [] } }));
    if let Ok(bad) = bad {
        assert!(review(&b, &bad).contains("· skipped"), "{}", review(&b, &bad));
    }
}

#[test]
fn a_summary_comes_without_its_conversation_under_the_same_named_project() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    let (sid, _) = shell_in(&a, "web");
    call(&mut a.human(), "session.close", json!({ "id": sid, "force": true }));
    assert!(build_err(&a, ObjectType::Summary, &[&sid, &sid]).contains("one"));
    assert!(build_err(&a, ObjectType::Summary, &["s_nothere"]).contains("no closed terminal"));
    let o = over_the_wire(&build(&a, ObjectType::Summary, &[&sid]));
    let MidnaObject::Summary { project, record } = &o else { panic!("{o:?}") };
    assert_eq!(project, "web");
    assert!(record.conversation_id.is_none() && record.agent_args.is_empty(), "{record:?}");

    // B has no web: it would go under Root, and says so.
    let r = review(&b, &o);
    assert!(r.contains("+ the Summary of “build”") && r.contains("Root (no project named “web” here)"), "{r}");
    assert!(!objects::needs_human(&b.daemon(), &o));

    let (_, pb) = shell_in(&b, "web");
    let r = review(&b, &o);
    assert!(r.contains("+ the Summary of “build”") && r.ends_with("under web"), "{r}");
    assert_eq!(apply(&b, &human(), &o), "added 1 (summary)");
    let got = call(&mut b.human(), "summary.list", json!({ "project_id": pb }))["closed"].clone();
    let c = got.as_array().unwrap().iter().find(|c| c["id"] == sid.as_str()).cloned().unwrap_or_else(|| panic!("{got}"));
    assert_eq!((c["name"].as_str(), c["apps"]["nearby"]["title"].as_str()), (Some("build"), Some("Sent from Alpha")), "{c}");
    assert!(c["conversation_id"].is_null(), "{c}");
    let here = std::fs::canonicalize(b.home.join("web")).unwrap();
    assert_eq!(c["cwd"].as_str().map(|p| std::fs::canonicalize(p).unwrap()), Some(here), "the cwd is this Mac's project folder: {c}");

    assert!(review(&b, &o).contains("· skipped: the Summary of “build”: you have it"));
    assert_eq!(apply(&b, &human(), &o), "added 0 (summary), skipped 1");
}

#[test]
fn queued_prompts_wait_until_the_human_picks_a_terminal() {
    let (a, b) = (TestDaemon::start(), TestDaemon::start());
    let mut ha = a.human();
    let (sa, _) = shell_in(&a, "web");
    call(&mut ha, "queue.pause", json!({ "session": sa, "paused": true }));
    for text in ["echo one", "echo two"] {
        call(&mut ha, "queue.add", json!({ "session": sa, "text": text }));
    }
    let q: Vec<String> = call(&mut ha, "queue.list", json!({ "session": sa }))["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(q.len(), 2);
    assert!(build_err(&a, ObjectType::Queue, &["q_nothere"]).contains("no queued prompt"));
    let o = over_the_wire(&build(&a, ObjectType::Queue, &[&q[0], &q[1]]));
    assert_eq!(o.label(), "2 queued prompts");

    let r = review(&b, &o);
    assert!(r.contains("+ queue “echo one”") && r.contains("+ queue “echo two”"), "{r}");
    assert!(!objects::needs_human(&b.daemon(), &o));
    let (sb, _) = shell_in(&b, "web");
    let mut hb = b.human();
    call(&mut hb, "queue.pause", json!({ "session": sb, "paused": true }));
    let kept = objects::apply(&b.daemon(), &human(), &o, None, "Alpha").unwrap();
    assert!(kept.contains("kept in the inbox"), "{kept}");
    assert!(call(&mut hb, "queue.list", json!({ "session": sb }))["items"].as_array().unwrap().is_empty());
    assert!(objects::apply(&b.daemon(), &human(), &o, Some("s_nothere"), "Alpha").is_err());

    assert_eq!(objects::apply(&b.daemon(), &human(), &o, Some(&sb), "Alpha").unwrap(), format!("queued 2 in {sb}"));
    let items = call(&mut hb, "queue.list", json!({ "session": sb }))["items"].as_array().unwrap().clone();
    let texts: Vec<&str> = items.iter().map(|i| i["text"].as_str().unwrap()).collect();
    assert_eq!(texts, ["echo one", "echo two"]);

    // Only prompts go into a terminal.
    let rules = MidnaObject::Rules { rules: vec![] };
    assert_eq!(objects::apply(&b.daemon(), &human(), &rules, Some(&sb), "Alpha").unwrap_err().code, BAD_PARAMS);
}

// ---- over Nearby ----

fn service() -> String {
    format!("_mo{:x}x{}._tcp", std::process::id() & 0xfffff, N.fetch_add(1, Ordering::Relaxed))
}

fn nearby(svc: &str) -> TestDaemon {
    let svc = svc.to_string();
    TestDaemon::start_with(move |cfg| cfg.nearby = Setup { mode: Mode::Service(svc), local_only: true, everyone_secs: 600, ..Setup::default() })
}

fn list(c: &mut midna_proto::Client) -> Value {
    call(c, "peers.list", json!({}))
}

fn fingerprint(c: &mut midna_proto::Client) -> String {
    list(c)["me"]["fingerprint"].as_str().unwrap().to_string()
}

fn peer(c: &mut midna_proto::Client, fp: &str) -> Option<Value> {
    list(c)["peers"].as_array().unwrap().iter().find(|p| p["fingerprint"] == fp).cloned()
}

/// Alpha and Bravo, paired and seeing each other.
fn paired_pair(svc: &str) -> (TestDaemon, TestDaemon, String, String) {
    let (a, b) = (nearby(svc), nearby(svc));
    let (mut ha, mut hb) = (a.human(), b.human());
    call(&mut ha, "settings.set", json!({ "key": "nearby.name", "value": "Alpha" }));
    call(&mut hb, "settings.set", json!({ "key": "nearby.name", "value": "Bravo" }));
    call(&mut hb, "settings.set", json!({ "key": "nearby.visibility", "value": "everyone" }));
    let (fa, fb) = (fingerprint(&mut ha), fingerprint(&mut hb));
    wait_for(10, "A sees B", || peer(&mut ha, &fb));
    let mine = call(&mut ha, "peers.pair", json!({ "fingerprint": fb }));
    let theirs = wait_for(10, "B has the pairing", || list(&mut hb)["pairings"].as_array().unwrap().iter().find(|p| p["fingerprint"] == fa.as_str()).cloned());
    call(&mut ha, "peers.confirm", json!({ "pairing": mine["id"], "as": "my_device" }));
    call(&mut hb, "needs_you.resolve", json!({ "id": theirs["needs_you_id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    wait_for(10, "both keep each other", || {
        (list(&mut ha)["paired"].as_array().unwrap().len() == 1 && list(&mut hb)["paired"].as_array().unwrap().len() == 1).then_some(())
    });
    wait_for(10, "B sees A", || peer(&mut hb, &fa));
    (a, b, fa, fb)
}

fn share(c: &mut midna_proto::Client, id: &str) -> Option<Value> {
    call(c, "inbox.list", json!({}))["items"].as_array().unwrap().iter().find(|s| s["id"] == id).cloned()
}

#[test]
fn an_object_share_shows_its_review_and_waits_for_the_human() {
    let svc = service();
    let (a, b, _fa, fb) = paired_pair(&svc);
    let (mut ha, mut hb) = (a.human(), b.human());
    let rule = call(&mut ha, "rule.add", json!({ "effect": "allow", "matcher": { "kind": "cli", "pattern": "deploy staging*" } }));
    let trig = call(&mut ha, "trigger.add", json!({ "name": "Ping on push", "source": "github", "event": "push",
        "action": { "kind": "attention", "message": "pushed {{branch}}" } }));

    // Bad object sends never leave A.
    for bad in [
        json!({ "kind": "object" }),
        json!({ "kind": "object", "object": "rules", "ids": [rule["id"]], "text": "x" }),
        json!({ "kind": "message", "text": "x", "ids": ["r_1"] }),
        json!({ "kind": "object", "object": "rules", "ids": ["r_nothere"] }),
    ] {
        let mut p = bad.clone();
        p["peer"] = json!(fb);
        let e = call_err(&mut ha, "share.send", p);
        assert!(e.code == BAD_PARAMS || e.code == midna_proto::error::NOT_FOUND, "{bad}: {}", e.message);
    }

    let sent = call(&mut ha, "share.send", json!({ "peer": fb, "kind": "object", "object": "rules", "ids": [rule["id"]] }));
    let id = sent["id"].as_str().unwrap().to_string();
    let got = share(&mut hb, &id).expect("B has it");
    assert_eq!((got["payload"]["kind"].as_str(), got["payload"]["object"]["type"].as_str()), (Some("object"), Some("rules")), "{got}");
    let ny = got["needs_you_id"].as_str().unwrap().to_string();
    let item = call(&mut hb, "needs_you.get", json!({ "id": ny }))["item"].clone();
    assert_eq!(item["title"], "Alpha sent from Midna: 1 rule", "{item}");
    let detail = item["detail"].as_str().unwrap();
    assert!(detail.contains("+ rule: allow cli `deploy staging*`, everywhere") && detail.contains("Only you can accept this"), "{detail}");

    // An agent on B can't take it.
    let e = call_err(&mut b.agent(None), "share.accept", json!({ "id": id }));
    assert_eq!(e.code, HUMAN_ONLY);
    assert!(rules(&b).is_empty());

    // The human approves: the rule is B's now.
    call(&mut hb, "needs_you.resolve", json!({ "id": ny, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
    let done = wait_for(10, "accepted", || share(&mut hb, &id).filter(|s| s["state"] == "accepted"));
    assert_eq!(done["result"], "added 1 (rules)", "{done}");
    assert!(rules(&b).iter().any(|r| r["matcher"]["pattern"] == "deploy staging*"));

    // A webhook trigger comes in off; accepting from the CLI path does the same.
    let sent = call(&mut ha, "share.send", json!({ "peer": fb, "kind": "object", "object": "triggers", "ids": [trig["id"]] }));
    let id = sent["id"].as_str().unwrap().to_string();
    let r = call(&mut hb, "share.accept", json!({ "id": id }));
    assert_eq!(r["result"], "added 1 (triggers)", "{r}");
    let t = triggers(&b).into_iter().find(|t| t["name"] == "Ping on push").unwrap();
    assert_eq!((t["enabled"].as_bool(), t["state"].as_str()), (Some(false), Some("needs_secret")), "{t}");
}
