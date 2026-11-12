//! `summary.*`: a project's record of its closed terminals, apps' panels on them, and resuming one.
mod common;
use common::*;
use serde_json::json;

fn shell_in(h: &mut midna_proto::Client, dir: &std::path::Path) -> (String, String) {
    let s = call(h, "session.open", json!({ "kind": "shell", "cwd": dir, "command": ["/bin/sh"], "name": "build" }));
    (s["id"].as_str().unwrap().to_string(), s["project_id"].as_str().unwrap().to_string())
}

#[test]
fn a_closed_terminal_is_recorded_with_the_panel_an_app_put_on_it() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let dir = d.home.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    let (sid, project) = shell_in(&mut h, &dir);

    let panel = json!({ "title": "T731 Dropped files", "badge": "Done", "color": "green", "line": "T731 Done",
                        "buttons": [{ "label": "Open task", "open": "taskboard://task/T731" }] });
    let mut a = d.agent(None);
    let r = call(&mut a, "summary.annotate", json!({ "id": sid, "owner": "taskboard", "panel": panel }));
    assert_eq!(r["running"], true, "it waits for the close");
    assert!(call(&mut h, "summary.list", json!({}))["closed"].as_array().unwrap().is_empty());

    call(&mut h, "session.close", json!({ "id": sid, "force": true }));
    let list = call(&mut h, "summary.list", json!({ "project_id": project }));
    let closed = list["closed"].as_array().unwrap();
    assert_eq!(closed.len(), 1, "{list}");
    let c = &closed[0];
    assert_eq!(c["id"], sid.as_str());
    assert_eq!(c["name"], "build");
    assert_eq!(c["how"], "force_close");
    assert_eq!(c["by"]["kind"], "human");
    assert_eq!(c["apps"]["taskboard"]["title"], "T731 Dropped files", "the panel carried over: {c}");
    assert_eq!(list["keep_days"], 7);

    // An app updates its panel on the closed record; a bad one is refused.
    let r = call(&mut a, "summary.annotate", json!({ "id": sid, "owner": "taskboard", "panel": { "title": "T731", "badge": "In review" } }));
    assert_eq!(r["running"], false);
    let c = &call(&mut h, "summary.list", json!({}))["closed"][0];
    assert_eq!(c["apps"]["taskboard"]["badge"], "In review");
    let e = call_err(&mut a, "summary.annotate", json!({ "id": sid, "owner": "taskboard", "panel": { "title": "x", "url": "file:///etc/passwd" } }));
    assert!(e.message.contains("URL"), "{e:?}");

    let r = call(&mut a, "summary.remove", json!({ "id": sid }));
    assert_eq!(r["removed"], 1);
    assert!(call(&mut h, "summary.list", json!({}))["closed"].as_array().unwrap().is_empty());
}

#[test]
fn a_replaced_terminal_is_recorded_and_a_resumed_one_leaves_the_summary() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let dir = d.home.join("work");
    std::fs::create_dir_all(&dir).unwrap();
    let (old, _) = shell_in(&mut h, &dir);
    let new = call(&mut h, "session.replace", json!({ "id": old }))["id"].as_str().unwrap().to_string();
    let closed = call(&mut h, "summary.list", json!({}))["closed"].clone();
    assert_eq!(closed.as_array().unwrap().len(), 1, "a replace ends the old session: {closed}");
    assert_eq!(closed[0]["id"], old.as_str());
    assert_eq!(closed[0]["how"], "replace");
    assert_eq!(call(&mut h, "summary.remove", json!({ "id": old }))["removed"], 1);

    call(&mut h, "session.close", json!({ "id": new, "force": true }));
    let r = call(&mut h, "summary.resume", json!({ "id": new }));
    let back = r["session"]["id"].as_str().unwrap().to_string();
    assert_ne!(back, new);
    assert_eq!(r["session"]["kind"], "shell");
    assert_eq!(r["session"]["cwd"].as_str(), dir.to_str());
    assert!(call(&mut h, "summary.list", json!({}))["closed"].as_array().unwrap().is_empty(), "resumed: off the Summary");

    // Clearing a project's Summary.
    call(&mut h, "session.close", json!({ "id": back, "force": true }));
    let project = call(&mut h, "summary.list", json!({}))["closed"][0]["project_id"].as_str().unwrap().to_string();
    assert_eq!(call(&mut h, "summary.remove", json!({ "project_id": project }))["removed"], 1);
}

#[test]
fn a_terminal_at_root_is_recorded_under_root() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let s = call(&mut h, "session.open", json!({ "kind": "shell", "project_id": "root", "command": ["/bin/sh"], "name": "scratch" }));
    let sid = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["project_id"], "root");
    call(&mut h, "session.close", json!({ "id": sid, "force": true }));

    let closed = call(&mut h, "summary.list", json!({ "project_id": "root" }))["closed"].clone();
    assert_eq!(closed.as_array().unwrap().len(), 1, "{closed}");
    assert_eq!(closed[0]["id"], sid.as_str());
    assert_eq!(closed[0]["project_id"], "root");
    assert_eq!(call(&mut h, "summary.remove", json!({ "project_id": "root" }))["removed"], 1);
}
