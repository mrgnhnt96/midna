//! Streams midnad runs (`streams.add`): the producer's stdout lines are published like
//! `streams.publish`, everything else goes to its log, it starts again after exiting (with a
//! backoff), when midnad starts and after a wake, and nothing it started outlives it. An agent's
//! stream waits for the human's approval.
mod common;
use common::*;
use midna_proto::Client;
use midna_proto::error::{CONFLICT, HUMAN_ONLY};
use serde_json::{Value, json};
use std::path::Path;

/// Secrets in the file store, so tests never touch the Keychain.
fn start() -> TestDaemon {
    TestDaemon::start_with(|cfg| cfg.webhooks.secrets = midnad::webhooks::secrets::Mode::File)
}

fn restart(d: &mut TestDaemon) {
    d.restart_with(|cfg| cfg.webhooks.secrets = midnad::webhooks::secrets::Mode::File);
}

/// Add stream `name` running `script` with /bin/sh in the test's home.
fn add(h: &mut Client, d: &TestDaemon, name: &str, script: &str) -> Value {
    call(h, "streams.add", json!({ "name": name, "command": ["/bin/sh", "-c", script], "cwd": d.home }))
}

fn status(h: &mut Client, name: &str) -> Value {
    let all = call(h, "streams.list", json!({}));
    all.as_array().unwrap().iter().find(|s| s["name"] == name).cloned().unwrap_or(Value::Null)
}

fn events(h: &mut Client, kind: &str) -> Vec<Value> {
    call(h, "events.list", json!({ "filter": { "kinds": [kind] }, "limit": 100 })).as_array().cloned().unwrap_or_default()
}

fn alive(pid: i64) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn gone(pid: i64) -> bool {
    // A zombie still answers kill(0); `ps` shows it as Z.
    !alive(pid) || std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim().starts_with('Z'))
}

fn running_pid(h: &mut Client, name: &str) -> i64 {
    wait_for(10, "stream running", || {
        let s = status(h, name);
        (s["state"] == "running").then(|| s["pid"].as_i64()).flatten()
    })
}

fn read_pid(path: &Path) -> i64 {
    wait_for(10, "pid file", || std::fs::read_to_string(path).ok().and_then(|t| t.trim().parse().ok()))
}

fn log(d: &TestDaemon, name: &str) -> String {
    std::fs::read_to_string(d.home.join("streams").join(format!("{name}.log"))).unwrap_or_default()
}

#[test]
fn a_producer_s_lines_are_published_and_the_rest_goes_to_its_log() {
    let d = start();
    let mut h = d.human();
    let script = r#"sleep 60 & echo $! > child.pid
echo '{"kind":"build.done","id":"1","data":{"n":1}}'
echo 'not json at all'
echo '{"id":"2"}'
echo 'oops on stderr' >&2
echo '{"kind":"build.done","id":"1","data":{"n":1}}'
echo '{"kind":"BAD KIND"}'
echo '{"kind":"build.done","id":3,"stream":"other"}'
wait"#;
    let s = add(&mut h, &d, "ci", script);
    assert_eq!((s["name"].as_str(), s["enabled"].as_bool()), (Some("ci"), Some(true)), "{s}");
    assert!(s["log"].as_str().unwrap().ends_with("/streams/ci.log"), "{s}");

    let got = wait_for(10, "two events", || Some(events(&mut h, "stream.ci.build.done")).filter(|e| e.len() == 2));
    assert_eq!(got[0]["data"], json!({ "n": 1 }));
    assert_eq!(got[0]["stream"], json!({ "name": "ci", "kind": "build.done", "id": "1" }));
    assert_eq!(got[0]["actor"]["kind"], "system", "{}", got[0]);
    assert_eq!(got[0]["actor"]["name"], "stream ci", "{}", got[0]);
    assert_eq!(got[1]["stream"], json!({ "name": "ci", "kind": "build.done", "id": "3" }), "a numeric id is a string; its own stream field is ignored");
    let pub_ = call(&mut h, "streams.events", json!({ "stream": "ci" }));
    assert_eq!(pub_["counts"]["duplicate"], 1, "the same intake as streams.publish: {pub_}");

    let l = wait_for(10, "log lines", || Some(log(&d, "ci")).filter(|l| l.contains("oops") && l.contains("BAD KIND")));
    assert!(l.contains("started: pid "), "{l}");
    assert!(l.contains("stdout (not JSON): not json at all"), "{l}");
    assert!(l.contains(r#"stdout (no `kind`): {"id":"2"}"#), "{l}");
    assert!(l.contains("stderr: oops on stderr"), "{l}");
    assert!(l.contains(r#"dropped: "#) && l.contains(r#"{"kind":"BAD KIND"}"#), "a publish the intake refuses is logged: {l}");

    let pid = running_pid(&mut h, "ci");
    let child = read_pid(&d.home.join("child.pid"));
    let st = status(&mut h, "ci");
    assert_eq!((st["restarts"].as_u64(), st["last_event"]["kind"].as_str()), (Some(0), Some("build.done")), "{st}");
    assert!(alive(child));

    // Removing it stops the producer and what it started; it's gone from the list and state.
    assert_eq!(call(&mut h, "streams.remove", json!({ "name": "ci" })), json!({ "ok": true }));
    wait_for(5, "producer and its child stopped", || (gone(pid) && gone(child)).then_some(()));
    assert_eq!(call(&mut h, "streams.list", json!({})), json!([]));
    assert_eq!(call_err(&mut h, "streams.remove", json!({ "name": "ci" })).code, 3);
    d.daemon().save_now();
    let state = std::fs::read_to_string(d.home.join("state.json")).unwrap();
    assert!(!state.contains("\"streams\""), "{state}");
}

#[test]
fn an_exiting_producer_starts_again_after_a_backoff_and_pause_stops_it() {
    let d = start();
    let mut h = d.human();
    add(&mut h, &d, "flaky", r#"echo '{"kind":"up"}'; exit 3"#);
    let s = wait_for(10, "two restarts", || Some(status(&mut h, "flaky")).filter(|s| s["restarts"].as_u64() >= Some(2)));
    assert_eq!((s["last_exit"]["code"].as_i64(), s["error"].as_str()), (Some(3), None), "{s}");
    assert!(s["state"] == "backoff" || s["state"] == "running" || s["state"] == "starting", "{s}");
    let l = log(&d, "flaky");
    assert!(l.contains("exited: exit code 3") && l.contains("starting again in 1s") && l.contains("starting again in 2s"), "{l}");

    // Paused: stopped and kept stopped (also across a restart of midnad), until resumed.
    let p = call(&mut h, "streams.set_enabled", json!({ "name": "flaky", "enabled": false }));
    assert_eq!((p["state"].as_str(), p["enabled"].as_bool(), p["retry_at"].as_str()), (Some("paused"), Some(false), None), "{p}");
    let e = call_err(&mut h, "streams.restart", json!({ "name": "flaky" }));
    assert!(e.message.contains("paused"), "{e:?}");
    let restarts = status(&mut h, "flaky")["restarts"].as_u64().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2500));
    let still = status(&mut h, "flaky");
    assert_eq!((still["state"].as_str(), still["restarts"].as_u64(), still["pid"].as_i64()), (Some("paused"), Some(restarts), None), "{still}");

    // A long-lived one: resume starts it, restart replaces its process.
    call(&mut h, "streams.remove", json!({ "name": "flaky" }));
    add(&mut h, &d, "steady", "exec sleep 60");
    let first = running_pid(&mut h, "steady");
    call(&mut h, "streams.set_enabled", json!({ "name": "steady", "enabled": false }));
    wait_for(5, "paused producer stopped", || gone(first).then_some(()));
    let r = call(&mut h, "streams.set_enabled", json!({ "name": "steady", "enabled": true }));
    assert_eq!(r["enabled"], true, "{r}");
    let second = running_pid(&mut h, "steady");
    assert_ne!(first, second);
    call(&mut h, "streams.restart", json!({ "name": "steady" }));
    let third = wait_for(10, "a new process", || status(&mut h, "steady")["pid"].as_i64().filter(|&p| p != second));
    assert!(gone(second) || wait_for(5, "old one stopped", || gone(second).then_some(true)));
    assert!(alive(third));
}

#[test]
fn streams_start_again_after_a_wake_and_with_midnad_and_never_outlive_it() {
    let mut d = start();
    let mut h = d.human();
    add(&mut h, &d, "tail", "exec sleep 60");
    let first = running_pid(&mut h, "tail");

    // After a wake, every stream starts again.
    d.daemon().clock.simulate_sleep(3600);
    let woke = wait_for(10, "started again after the wake", || status(&mut h, "tail")["pid"].as_i64().filter(|&p| p != first));
    wait_for(5, "pre-sleep producer stopped", || gone(first).then_some(()));

    // A producer an earlier midnad left behind (a crash): its pidfile names it, and the next
    // midnad stops it before starting its streams.
    let mut stray = {
        use std::os::unix::process::CommandExt;
        let mut c = std::process::Command::new("/bin/sleep");
        c.arg("60");
        unsafe {
            c.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        c.spawn().unwrap()
    };
    let spid = stray.id() as i32;
    let start = wait_for(5, "stray's start time", || midnad::procs::table_start(spid));
    std::fs::write(d.home.join("streams").join("old.pid"), format!("{spid} {start}\n")).unwrap();

    // Stopping midnad stops the producer; the next midnad starts it again (state.json keeps it).
    drop(h);
    restart(&mut d);
    assert!(gone(woke), "midnad stopped, so its producer did");
    // midnad reaps it (it's a child of this process, as one is after an upgrade's exec).
    wait_for(5, "the leftover producer stopped", || gone(spid as i64).then_some(()));
    let _ = stray.try_wait();
    let mut h = d.human();
    let again = running_pid(&mut h, "tail");
    assert_ne!(again, woke);
    assert!(!d.home.join("streams").join("old.pid").exists());
    let s = status(&mut h, "tail");
    assert_eq!((&s["command"], s["cwd"].as_str()), (&json!(["/bin/sh", "-c", "exec sleep 60"]), d.home.to_str()), "{s}");
}

fn approvals(h: &mut Client) -> Vec<Value> {
    call(h, "needs_you.list", json!({})).as_array().unwrap().iter().filter(|n| n["kind"] == "approval").cloned().collect()
}

fn approve(h: &mut Client, id: &str) {
    call(h, "needs_you.resolve", json!({ "id": id, "resolution": { "kind": "approve", "scope": { "kind": "once" } } }));
}

fn approve_err(h: &mut Client, id: &str) -> i64 {
    call_err(h, "needs_you.resolve", json!({ "id": id, "resolution": { "kind": "approve", "scope": { "kind": "once" } } })).code
}

/// A script that writes `marker`'s name into it each time it starts, then waits.
fn marking(d: &TestDaemon, marker: &str) -> Value {
    json!(["/bin/sh", "-c", format!("echo started >> {} ; exec sleep 60", d.home.join(marker).display())])
}

fn starts(d: &TestDaemon, marker: &str) -> usize {
    std::fs::read_to_string(d.home.join(marker)).map(|t| t.lines().count()).unwrap_or(0)
}

#[test]
fn an_agent_s_stream_waits_for_the_human_s_approval() {
    let d = start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut h, "secret.set", json!({ "name": "TOK", "value": "t0ken-v4lue-for-approval" }));
    let s = call(&mut a, "streams.add", json!({ "name": "tail", "command": marking(&d, "m"), "cwd": d.home, "secrets": ["TOK"] }));
    assert_eq!((s["state"].as_str(), s["enabled"].as_bool(), s["pid"].as_i64()), (Some("awaiting_approval"), Some(false), None), "{s}");
    // One approval, naming what it would run.
    let items = approvals(&mut h);
    assert_eq!(items.len(), 1, "{items:?}");
    let nid = items[0]["id"].as_str().unwrap().to_string();
    assert_eq!(s["approval"], nid.as_str());
    let detail = items[0]["detail"].as_str().unwrap();
    let exact = detail.split("Approving runs exactly: ").nth(1).and_then(|r| r.lines().next()).unwrap_or("");
    let shown: Value = serde_json::from_str(exact.trim_start_matches("streams.set_enabled ")).unwrap_or(Value::Null);
    assert_eq!(shown, json!({ "name": "tail", "enabled": true }), "{detail}");
    for part in ["command: /bin/sh -c 'echo started", &format!("cwd: {}", d.home.display()), "project: ", "secrets: TOK"] {
        assert!(detail.contains(part), "{part} in {detail}");
    }
    assert!(!detail.contains("t0ken-v4lue"), "{detail}");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(starts(&d, "m"), 0, "nothing runs before the human approves");
    // Asking again is the same item; it can't be restarted or approved by the agent.
    let e = call_err(&mut a, "streams.set_enabled", json!({ "name": "tail", "enabled": true }));
    assert_eq!(e.code, HUMAN_ONLY);
    assert_eq!(e.data.unwrap()["needs_you_id"], nid.as_str());
    assert_eq!(approvals(&mut h).len(), 1);
    assert!(call_err(&mut a, "streams.restart", json!({ "name": "tail" })).message.contains("approval"));
    assert_eq!(approve_err(&mut a, &nid), HUMAN_ONLY);
    // The human approves: it runs.
    approve(&mut h, &nid);
    running_pid(&mut h, "tail");
    wait_for(10, "it started", || (starts(&d, "m") == 1).then_some(()));
    let s = status(&mut h, "tail");
    assert_eq!((s["enabled"].as_bool(), s.get("approval")), (Some(true), None), "{s}");
    assert!(approvals(&mut h).is_empty());
    // Agents may pause, resume what was approved, and restart.
    assert_eq!(call(&mut a, "streams.set_enabled", json!({ "name": "tail", "enabled": false }))["state"], "paused");
    call(&mut a, "streams.set_enabled", json!({ "name": "tail", "enabled": true }));
    let pid = running_pid(&mut h, "tail");
    call(&mut a, "streams.restart", json!({ "name": "tail" }));
    wait_for(10, "restarted", || (running_pid(&mut h, "tail") != pid).then_some(()));
    assert!(approvals(&mut h).is_empty());
    // Changing what it runs pauses it and asks again.
    let before = starts(&d, "m");
    let pid = running_pid(&mut h, "tail");
    let s = call(&mut a, "streams.update", json!({ "name": "tail", "command": marking(&d, "m2") }));
    assert_eq!((s["state"].as_str(), s["enabled"].as_bool()), (Some("awaiting_approval"), Some(false)), "{s}");
    wait_for(10, "stopped", || gone(pid).then_some(()));
    let items = approvals(&mut h);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(s["approval"], items[0]["id"]);
    assert!(items[0]["detail"].as_str().unwrap().contains("m2"), "{}", items[0]["detail"]);
    assert_eq!(starts(&d, "m"), before);
    // Changing the secrets does too, replacing the open approval with one for the new definition.
    let s = call(&mut a, "streams.update", json!({ "name": "tail", "secrets": [] }));
    let items = approvals(&mut h);
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(s["approval"], items[0]["id"]);
    assert!(items[0]["detail"].as_str().unwrap().contains("secrets: none"), "{}", items[0]["detail"]);
    // Removing a stream that isn't running is the agent's call; its approval goes with it.
    call(&mut a, "streams.remove", json!({ "name": "tail" }));
    assert_eq!(call(&mut h, "streams.list", json!({})), json!([]));
    assert!(approvals(&mut h).is_empty());
    assert_eq!(starts(&d, "m2"), 0);
}

#[test]
fn an_agent_removing_a_running_stream_asks_the_human() {
    let d = start();
    let mut h = d.human();
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    add(&mut h, &d, "run", "exec sleep 60");
    let pid = running_pid(&mut h, "run");
    let e = call_err(&mut a, "streams.remove", json!({ "name": "run" }));
    assert_eq!(e.code, HUMAN_ONLY, "{e:?}");
    let nid = e.data.unwrap()["needs_you_id"].as_str().unwrap().to_string();
    assert_eq!(running_pid(&mut h, "run"), pid, "still running");
    let item = approvals(&mut h).into_iter().find(|n| n["id"] == nid.as_str()).unwrap();
    assert!(item["detail"].as_str().unwrap().contains("command: /bin/sh -c 'exec sleep 60'"), "{}", item["detail"]);
    approve(&mut h, &nid);
    assert_eq!(call(&mut h, "streams.list", json!({})), json!([]));
    wait_for(10, "stopped", || gone(pid).then_some(()));
    // A paused one the agent removes itself.
    add(&mut h, &d, "paused", "exec sleep 60");
    call(&mut a, "streams.set_enabled", json!({ "name": "paused", "enabled": false }));
    call(&mut a, "streams.remove", json!({ "name": "paused" }));
    assert_eq!(call(&mut h, "streams.list", json!({})), json!([]));
}

#[test]
fn approving_a_stream_runs_only_what_the_human_was_shown() {
    let d = start();
    let mut h = d.human();
    // Keep the stale approval below open for the human to answer (the sweep would take it back).
    call(&mut h, "settings.set", json!({ "key": "needs_you.withdraw_orphans", "value": false }));
    let sid = open_sh(&mut h);
    let mut a = d.agent(Some(&sid));
    call(&mut a, "streams.add", json!({ "name": "s", "command": marking(&d, "first"), "cwd": d.home }));
    let nid = approvals(&mut h)[0]["id"].as_str().unwrap().to_string();
    // The agent swaps the command after asking: that approval is taken back, a new one shows it.
    call(&mut a, "streams.update", json!({ "name": "s", "command": ["/bin/sh", "-c", "echo swapped > /dev/null; exec sleep 60"] }));
    assert_eq!(approve_err(&mut h, &nid), 3, "the first approval is gone");
    let items = approvals(&mut h);
    assert_eq!(items.len(), 1);
    assert!(items[0]["detail"].as_str().unwrap().contains("swapped"), "{}", items[0]["detail"]);
    assert_eq!(status(&mut h, "s")["state"], "awaiting_approval");
    // The human resuming it approves it; the open approval no longer matches and is refused.
    let nid2 = items[0]["id"].as_str().unwrap().to_string();
    call(&mut h, "streams.set_enabled", json!({ "name": "s", "enabled": true }));
    running_pid(&mut h, "s");
    assert_eq!(approve_err(&mut h, &nid2), CONFLICT);
    // The human changing a running stream restarts it with the change, no approval needed.
    let pid = running_pid(&mut h, "s");
    let s = call(&mut h, "streams.update", json!({ "name": "s", "command": marking(&d, "second") }));
    assert_eq!(s["enabled"], true, "{s}");
    wait_for(10, "started with the new command", || (starts(&d, "second") == 1).then_some(()));
    assert!(gone(pid) || running_pid(&mut h, "s") != pid);
    assert!(approvals(&mut h).is_empty());
    assert_eq!(starts(&d, "first"), 0);
    // A change to nothing is no change; bad changes are refused.
    assert_eq!(call(&mut a, "streams.update", json!({ "name": "s", "command": marking(&d, "second") }))["state"], "running");
    assert!(call_err(&mut a, "streams.update", json!({ "name": "s", "cwd": "rel" })).message.contains("absolute"));
    assert!(call_err(&mut a, "streams.update", json!({ "name": "s", "secrets": ["NOPE"] })).message.contains("no secret NOPE"));
    assert_eq!(call_err(&mut a, "streams.update", json!({ "name": "nope", "command": ["/bin/true"] })).code, 3);
    assert!(approvals(&mut h).is_empty());
}

#[test]
fn bad_streams_are_refused() {
    let d = start();
    let mut h = d.human();
    let bad = |h: &mut Client, p: Value| call_err(h, "streams.add", p).message;
    let home = d.home.to_str().unwrap();
    assert!(bad(&mut h, json!({ "name": "Bad Name", "command": ["/bin/true"], "cwd": home })).contains("a-z"));
    assert!(bad(&mut h, json!({ "name": "x", "command": [], "cwd": home })).contains("command is empty"));
    assert!(bad(&mut h, json!({ "name": "x", "command": ["/bin/true"], "cwd": "rel/dir" })).contains("absolute"));
    assert!(bad(&mut h, json!({ "name": "x", "command": ["/bin/true"], "cwd": home, "secrets": ["NOPE"] })).contains("no secret NOPE"));
    assert!(bad(&mut h, json!({ "name": "x", "command": ["/bin/true"], "cwd": home, "secrets": ["lower"] })).contains("isn't a secret name"));
    add(&mut h, &d, "x", "exec sleep 60");
    assert!(bad(&mut h, json!({ "name": "x", "command": ["/bin/true"], "cwd": home })).contains("exists"));
    assert_eq!(call_err(&mut h, "streams.restart", json!({ "name": "nope" })).code, 3);
}

#[test]
fn secrets_go_only_in_the_producer_s_environment() {
    let d = start();
    let mut h = d.human();
    let value = "v4lue-of-the-t0ken-9f2c";
    let other = "pr0ject-scoped-v4lue-77ab";
    call(&mut h, "secret.set", json!({ "name": "TOK", "value": value }));
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pid = call(&mut h, "project.add", json!({ "path": dir }))["id"].clone();
    call(&mut h, "secret.set", json!({ "name": "TOK", "value": other, "project_id": pid }));

    let script = r#"printf '%s' "$TOK" > "$OUT"
echo "the token is $TOK" >&2
echo "{\"kind\":\"leak\",\"data\":{\"t\":\"$TOK\"}}"
echo "not an event: $TOK"
exec sleep 60"#;
    let out = d.home.join("got");
    let cmd = json!(["/bin/sh", "-c", format!("OUT={} ; {script}", out.display())]);
    let s = call(&mut h, "streams.add", json!({ "name": "sec", "command": cmd, "cwd": d.home, "secrets": ["TOK", "TOK"] }));
    assert_eq!(s["secrets"], json!(["TOK"]), "{s}");
    assert_eq!(wait_for(10, "producer wrote its env", || std::fs::read_to_string(&out).ok().filter(|t| !t.is_empty())), value);

    // In a project, its own secret wins over the global one (like `secret exec`).
    let pout = d.home.join("got-proj");
    let cmd = json!(["/bin/sh", "-c", format!("OUT={} ; {script}", pout.display())]);
    call(&mut h, "streams.add", json!({ "name": "sec-proj", "command": cmd, "project_id": pid, "secrets": ["TOK"] }));
    assert_eq!(wait_for(10, "project producer wrote its env", || std::fs::read_to_string(&pout).ok().filter(|t| !t.is_empty())), other);

    let leak = wait_for(10, "leak event", || events(&mut h, "stream.sec.leak").pop());
    assert_eq!(leak["data"]["t"], "‹TOK›", "what it prints is masked: {leak}");
    let l = wait_for(10, "log", || Some(log(&d, "sec")).filter(|l| l.contains("the token is") && l.contains("not an event")));
    assert!(l.contains("the token is ‹TOK›"), "{l}");
    wait_for(10, "project log", || Some(log(&d, "sec-proj")).filter(|l| l.contains("the token is ‹TOK›") && l.contains("not an event")));

    d.daemon().save_now();
    let files = [d.home.join("state.json"), d.home.join("events.jsonl"), d.home.join("streams/sec.log"), d.home.join("streams/sec-proj.log")];
    for f in &files {
        let text = std::fs::read_to_string(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        assert!(!text.contains(value) && !text.contains(other), "a secret value is in {}", f.display());
    }
}

/// Once midna runs a stream, only its producer (the token it got at its last start) or the
/// human can publish to it; an agent's publish is refused and listed. Other names stay open.
#[test]
fn a_declared_stream_takes_only_its_producer_s_and_the_human_s_publishes() {
    let d = start();
    let mut h = d.human();
    let out = d.home.join("token");
    let script = format!(r#"printf '%s' "$MIDNA_STREAM_TOKEN" > {}
echo "token $MIDNA_STREAM_TOKEN" >&2
exec sleep 60"#, out.display());
    add(&mut h, &d, "ci", &script);
    let read_token = || wait_for(10, "producer wrote its token", || std::fs::read_to_string(&out).ok().filter(|t| !t.is_empty()));
    let token = read_token();
    assert_eq!(token.len(), 64, "{token}");
    let with = |t: Option<&str>| {
        let mut c = d.agent(None);
        c.set_caller(Some(midna_proto::Caller { role: Some("agent".into()), stream_token: t.map(Into::into), ..Default::default() }));
        c
    };
    let publish = |c: &mut Client, stream: &str, id: &str| c.call_value("streams.publish", json!({ "stream": stream, "kind": "build.failed", "id": id }));
    let refused = |r: Result<Value, midna_proto::ClientError>| match r {
        Err(midna_proto::ClientError::Rpc(e)) => {
            assert_eq!(e.code, midna_proto::error::REFUSED, "{e:?}");
            assert_eq!(e.data.as_ref().map(|v| v["verdict"].clone()), Some(json!("refused")), "{e:?}");
            assert!(e.message.contains("stream `ci` is one midna runs"), "{}", e.message);
        }
        other => panic!("not refused: {other:?}"),
    };

    // An agent, with no token or another one, is refused; its producer's token and the human get in.
    refused(publish(&mut d.agent(None), "ci", "a1"));
    refused(publish(&mut with(Some("f00d")), "ci", "a2"));
    assert_eq!(publish(&mut with(Some(&token)), "ci", "p1").unwrap()["verdict"], "accepted");
    assert_eq!(publish(&mut h, "ci", "h1").unwrap()["verdict"], "accepted");
    // A name midna doesn't run is open to anyone; the token doesn't matter there.
    assert_eq!(publish(&mut d.agent(None), "adhoc", "x1").unwrap()["verdict"], "accepted");
    let ev = call(&mut h, "streams.events", json!({ "stream": "ci" }));
    assert_eq!((ev["counts"]["refused"].as_u64(), ev["counts"]["accepted"].as_u64()), (Some(2), Some(2)), "{ev}");
    let rows = ev["events"].as_array().unwrap();
    assert_eq!((rows[3]["verdict"].as_str(), rows[3]["id"].as_str()), (Some("refused"), Some("a1")), "{ev}");
    assert_eq!(events(&mut h, "stream.ci.build.failed").len(), 2, "refused publishes aren't logged");

    // A restart gives the producer a new token; the old one stops working at once.
    std::fs::remove_file(&out).unwrap();
    call(&mut h, "streams.restart", json!({ "name": "ci" }));
    refused(publish(&mut with(Some(&token)), "ci", "p2"));
    let fresh = read_token();
    assert_ne!(fresh, token);
    refused(publish(&mut with(Some(&token)), "ci", "p3"));
    assert_eq!(publish(&mut with(Some(&fresh)), "ci", "p4").unwrap()["verdict"], "accepted");
    // Paused, the stream is still declared and nobody but the human publishes to it.
    call(&mut h, "streams.set_enabled", json!({ "name": "ci", "enabled": false }));
    refused(publish(&mut with(Some(&fresh)), "ci", "p5"));
    assert_eq!(publish(&mut h, "ci", "h2").unwrap()["verdict"], "accepted");

    // Never stored or logged: its log shows it masked.
    wait_for(10, "masked in the log", || Some(log(&d, "ci")).filter(|l| l.matches("token ‹MIDNA_STREAM_TOKEN›").count() == 2));
    d.daemon().save_now();
    for f in ["state.json", "events.jsonl", "streams/ci.log"] {
        let text = std::fs::read_to_string(d.home.join(f)).unwrap();
        assert!(!text.contains(&token) && !text.contains(&fresh), "a token is in {f}");
    }
}

fn failing_items(h: &mut Client) -> Vec<Value> {
    call(h, "needs_you.list", json!({})).as_array().unwrap().iter().filter(|n| n["kind"] == "stream_failing").cloned().collect()
}

fn health(h: &mut Client, name: &str) -> Vec<String> {
    events(h, "stream.health").iter().filter(|e| e["data"]["stream"] == name).map(|e| e["data"]["state"].as_str().unwrap_or("").to_string()).collect()
}

#[test]
fn a_stream_that_keeps_exiting_is_failing_with_one_needs_you_item_until_paused() {
    let mut d = start();
    midnad::producers::quick_backoff(&d.daemon());
    let mut h = d.human();
    // `stream.*` is for a stream's events: its health never fires a trigger.
    let t = call(&mut h, "trigger.add", json!({ "name": "Any", "source": "local", "event": "stream.*", "action": { "kind": "notify", "title": "x" }, "enabled": true, "cooldown_secs": 0 }));
    add(&mut h, &d, "slack", "echo invalid_auth >&2; exit 1");

    let s = wait_for(20, "failing", || Some(status(&mut h, "slack")).filter(|s| s["state"] == "failing"));
    assert_eq!(s["why"], "invalid_auth", "{s}");
    assert!(s["exits_recent"].as_u64() >= Some(5) && s["failing_since"].is_string(), "{s}");
    let items = wait_for(5, "one item", || Some(failing_items(&mut h)).filter(|i| !i.is_empty()));
    assert_eq!(items.len(), 1, "{items:?}");
    let item = &items[0];
    assert_eq!((item["stream"].as_str(), item["asked_by"]["name"].as_str()), (Some("slack"), Some("stream slack")), "{item}");
    assert_eq!(item["title"], "Stream slack keeps failing: invalid_auth", "{item}");
    assert!(item["detail"].as_str().unwrap().contains("midna stream pause slack"), "{item}");
    let id = item["id"].as_str().unwrap().to_string();
    let s = wait_for(5, "item on the status", || Some(status(&mut h, "slack")).filter(|s| s["needs_you"].is_string()));
    assert_eq!(s["needs_you"], id.as_str());
    assert!(s["log_tail"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("stderr: invalid_auth")), "{s}");

    // It keeps retrying, and raises nothing more.
    let restarts = s["restarts"].as_u64().unwrap();
    wait_for(20, "more restarts", || (status(&mut h, "slack")["restarts"].as_u64() > Some(restarts + 1)).then_some(()));
    assert_eq!(failing_items(&mut h).len(), 1);
    let states = health(&mut h, "slack");
    assert!(states.contains(&"backoff".to_string()) && states.iter().filter(|s| *s == "failing").count() == 1, "{states:?}");

    let info = call(&mut h, "daemon.info", json!({}));
    assert_eq!((info["streams"]["total"].as_u64(), info["streams"]["healthy"].as_u64()), (Some(1), Some(0)), "{info}");
    assert_eq!((info["streams"]["unhealthy"][0]["name"].as_str(), info["streams"]["unhealthy"][0]["state"].as_str()), (Some("slack"), Some("failing")), "{info}");

    // A restart of midnad keeps the same item: still failing, nothing raised twice.
    restart(&mut d);
    midnad::producers::quick_backoff(&d.daemon());
    let mut h = d.human();
    let s = wait_for(10, "failing after restart", || Some(status(&mut h, "slack")).filter(|s| s["state"] == "failing" && s["restarts"].as_u64() >= Some(2)));
    assert_eq!(s["needs_you"], id.as_str(), "{s}");
    let items = failing_items(&mut h);
    assert_eq!((items.len(), items[0]["id"].as_str()), (1, Some(id.as_str())), "{items:?}");

    // Pausing it is the human's answer: the item resolves.
    let p = call(&mut h, "streams.set_enabled", json!({ "name": "slack", "enabled": false }));
    assert_eq!((p["state"].as_str(), p["needs_you"].as_str(), p["why"].as_str()), (Some("paused"), None, None), "{p}");
    assert!(failing_items(&mut h).is_empty());
    assert_eq!(health(&mut h, "slack").last().map(String::as_str), Some("paused"));
    // Resumed, it starts afresh: healthy until it fails 5 more times.
    let r = call(&mut h, "streams.set_enabled", json!({ "name": "slack", "enabled": true }));
    assert_ne!(r["state"], "failing", "{r}");
    call(&mut h, "streams.set_enabled", json!({ "name": "slack", "enabled": false }));
    let fired = call(&mut h, "trigger.deliveries", json!({ "trigger_id": t["id"] }));
    assert_eq!(fired, json!([]), "stream.health fired a trigger");
}

#[test]
fn a_failing_stream_is_healthy_again_once_a_run_lasts_ten_minutes() {
    let d = start();
    midnad::producers::quick_backoff(&d.daemon());
    let mut h = d.human();
    add(&mut h, &d, "ci", "if [ -f ok ]; then exec sleep 60; fi; echo 'no token' >&2; exit 2");
    wait_for(20, "failing", || (status(&mut h, "ci")["state"] == "failing").then_some(()));
    let id = wait_for(5, "item", || failing_items(&mut h).first().map(|i| i["id"].as_str().unwrap().to_string()));

    // Fixed: the next run stays up, but it's still failing until it has run 10 minutes.
    std::fs::write(d.home.join("ok"), "").unwrap();
    let s = wait_for(20, "up again", || Some(status(&mut h, "ci")).filter(|s| s["pid"].is_i64()));
    assert_eq!((s["state"].as_str(), s["needs_you"].as_str()), (Some("failing"), Some(id.as_str())), "{s}");
    midnad::producers::backdate_up(&d.daemon(), "ci", 600);
    let s = wait_for(5, "healthy", || Some(status(&mut h, "ci")).filter(|s| s["state"] == "running"));
    assert!(s["needs_you"].is_null() && s["failing_since"].is_null() && s["why"].is_null(), "{s}");
    assert!(failing_items(&mut h).is_empty());
    let resolved = events(&mut h, "needs_you.resolved");
    assert!(resolved.iter().any(|e| e["data"]["id"] == id.as_str()), "{resolved:?}");
    assert_eq!(health(&mut h, "ci").last().map(String::as_str), Some("running"));
    let info = call(&mut h, "daemon.info", json!({}));
    assert_eq!((info["streams"]["healthy"].as_u64(), info["streams"]["unhealthy"].as_array().map_or(0, Vec::len)), (Some(1), 0), "{info}");
}
