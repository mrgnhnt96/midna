//! Hardening regressions: malformed input on the control socket, caller identity at the
//! human/agent boundary, view-only agent streams, and agents answering permission prompts.
//! Each test runs an in-process daemon on a temp MIDNA_HOME (see common/mod.rs).
mod common;
use common::{TestDaemon, call, call_err, open_sh, read, wait_for};
use midna_proto::error::{HUMAN_ONLY, PARSE_ERROR, REFUSED};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn raw(d: &TestDaemon) -> UnixStream {
    let s = UnixStream::connect(d.socket()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s
}

fn send_line(s: &mut UnixStream, bytes: &[u8]) -> Value {
    s.write_all(bytes).unwrap();
    s.write_all(b"\n").unwrap();
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad reply {line:?}: {e}"))
}

fn alive(d: &TestDaemon) {
    let mut h = d.human();
    assert_eq!(call(&mut h, "daemon.info", json!({}))["pid"], std::process::id());
}

#[test]
fn garbage_on_the_control_socket_never_hurts_the_daemon() {
    let d = TestDaemon::start();
    let mut s = raw(&d);
    // Invalid JSON, invalid UTF-8, non-object JSON, deep nesting: errors, connection stays up.
    assert_eq!(send_line(&mut s, b"{not json")["error"]["code"], PARSE_ERROR);
    assert_eq!(send_line(&mut s, b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}")["error"]["code"], PARSE_ERROR);
    assert_eq!(send_line(&mut s, b"\xc3\x28\x00\x01garbage")["error"]["code"], PARSE_ERROR);
    assert_eq!(send_line(&mut s, b"[1,2,3]")["error"]["code"], -32600);
    assert_eq!(send_line(&mut s, b"42")["error"]["code"], -32600);
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(send_line(&mut s, deep.as_bytes())["error"]["code"], PARSE_ERROR);
    // Wrong shapes for the envelope and for params.
    let r = send_line(&mut s, br#"{"jsonrpc":"2.0","id":2,"method":42,"params":"x"}"#);
    assert_eq!(r["error"]["code"], -32601);
    let r = send_line(&mut s, br#"{"jsonrpc":"2.0","id":{"a":[1]},"method":"session.read","params":[1,2]}"#);
    assert!(r["error"].is_object(), "{r}");
    let r = send_line(&mut s, br#"{"jsonrpc":"2.0","id":3,"method":"daemon.info"}"#);
    assert_eq!(r["result"]["pid"], std::process::id(), "connection still serves requests: {r}");

    // Every catalog method with garbage params, as an agent (so nothing human-only runs).
    // daemon.* would restart/stop the host process; stream.attach switches protocols.
    let shapes = [json!(null), json!([]), json!("x"), json!(7), json!({}), json!({ "id": "\u{0}\u{ffff}", "session": 1, "kind": [], "text": {} }),
        json!({ "id": "nope", "lines": -1, "cols": 65535, "rows": 65535, "since_seq": u64::MAX, "limit": u64::MAX })];
    let mut a = d.agent(None);
    for m in midna_proto::catalog() {
        if m.name.starts_with("daemon.") || m.name == "stream.attach" || m.name == "policy.request" {
            continue;
        }
        for p in &shapes {
            let _ = a.call_value(m.name, p.clone());
        }
    }
    alive(&d);

    // A huge line (over the cap) closes that connection only.
    let mut big = raw(&d);
    let chunk = vec![b'a'; 1 << 20];
    let mut closed = false;
    for _ in 0..(midnad::conn::MAX_LINE >> 20) + 2 {
        if big.write_all(&chunk).is_err() {
            closed = true;
            break;
        }
    }
    if !closed {
        let mut buf = String::new();
        let _ = big.read_to_string(&mut buf);
        assert!(buf.contains("longer than"), "{buf}");
    }
    alive(&d);
    // Random bytes.
    let mut seed = 0x2545F4914F6CDD1Du64;
    let mut junk = vec![0u8; 256 << 10];
    for b in junk.iter_mut() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *b = seed as u8;
    }
    let mut r = raw(&d);
    let _ = r.write_all(&junk);
    let _ = r.write_all(b"\n");
    drop(r);
    alive(&d);
}

#[test]
fn oversized_resize_is_clamped() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    call(&mut h, "session.resize", json!({ "id": id, "cols": 65535, "rows": 65535 }));
    let r = call(&mut h, "session.read", json!({ "id": id, "screen": true }));
    assert!(r["cols"].as_u64().unwrap() <= midnad::term::MAX_COLS as u64, "{r}");
    assert!(r["rows"].as_u64().unwrap() <= midnad::term::MAX_ROWS as u64, "{r}");
    call(&mut h, "session.input", json!({ "id": id, "text": "echo still-$((40+2))", "enter": true }));
    wait_for(5, "shell output", || read(&mut h, &id).contains("still-42").then_some(()));
}

#[test]
fn slow_readers_are_cut_off_not_buffered_forever() {
    let (tx, _rx) = midnad::conn::OutTx::pair();
    let line = "x".repeat(1 << 20);
    let mut sent = 0;
    while tx.send(midnad::conn::Out::Line(line.clone())).is_ok() {
        sent += 1;
        assert!(sent < 1000, "queue never filled");
    }
    assert!(tx.pending() <= midnad::conn::MAX_PENDING);
    assert!(tx.send(midnad::conn::Out::Line("y".into())).is_err(), "stays cut off");
}

#[test]
fn a_non_reading_subscriber_does_not_block_others() {
    let d = TestDaemon::start();
    // Subscribe and never read.
    let mut stuck = raw(&d);
    stuck.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"events.subscribe\",\"params\":{\"since_seq\":0}}\n").unwrap();
    let mut a = d.agent(None);
    let t0 = std::time::Instant::now();
    for i in 0..3000 {
        call(&mut a, "needs_you.raise", json!({ "kind": "note", "message": format!("note {i} {}", "z".repeat(400)) }));
    }
    assert!(t0.elapsed() < Duration::from_secs(60), "calls stayed fast: {:?}", t0.elapsed());
    alive(&d);
}

#[test]
fn a_terminal_can_only_speak_for_itself() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let a = open_sh(&mut h);
    let b = open_sh(&mut h);
    // Wide, so the JSON reply isn't wrapped mid-phrase.
    call(&mut h, "session.resize", json!({ "id": a, "cols": 400, "rows": 40 }));
    let sock = d.socket();
    let req = |claim: &str, msg: &str| {
        format!(
            "printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"needs_you.raise\",\"params\":{{\"kind\":\"note\",\"message\":\"{msg}\",\"caller\":{{\"session\":\"{claim}\"}}}}}}' | nc -U {} -w 2",
            sock.display()
        )
    };
    // From inside A, claiming B: refused.
    call(&mut h, "session.input", json!({ "id": a, "text": req(&b, "spoofed"), "enter": true }));
    wait_for(10, "refusal on screen", || read(&mut h, &a).replace('\n', "").contains("does not match the terminal").then_some(()));
    // From inside A, claiming A: fine, and attributed to A.
    call(&mut h, "session.input", json!({ "id": a, "text": req(&a, "honest"), "enter": true }));
    let item = wait_for(10, "honest note", || {
        call(&mut h, "needs_you.list", json!({})).as_array().unwrap().iter().find(|n| n["title"] == "honest").cloned()
    });
    assert_eq!(item["session_id"], a.as_str());
    assert_eq!(item["asked_by"]["session"], a.as_str());
    let items = call(&mut h, "needs_you.list", json!({}));
    assert!(!items.as_array().unwrap().iter().any(|n| n["title"] == "spoofed"), "{items}");
    assert_eq!(REFUSED, 1);
}

/// Helper run *inside* a terminal by `gui_binary_inside_a_terminal_is_an_agent`: connects as
/// this test binary (the configured GUI) and prints the role the daemon gave it.
#[test]
#[ignore]
fn helper_print_role() {
    let Ok(sock) = std::env::var("MIDNA_TEST_SOCKET") else { return };
    let mut c = midna_proto::Client::connect(sock).unwrap();
    c.set_caller(None);
    let info = c.call_value("daemon.info", json!({})).unwrap();
    println!("ROLE={}", info["role"].as_str().unwrap_or("?"));
}

#[test]
fn gui_binary_inside_a_terminal_is_an_agent() {
    let d = TestDaemon::start();
    let mut h = d.human();
    assert_eq!(call(&mut h, "daemon.info", json!({}))["role"], "human");
    let id = open_sh(&mut h);
    let exe = std::env::current_exe().unwrap();
    let cmd = format!("MIDNA_TEST_SOCKET={} {} --exact helper_print_role --ignored --nocapture", d.socket().display(), exe.display());
    call(&mut h, "session.input", json!({ "id": id, "text": cmd, "enter": true }));
    let out = wait_for(20, "helper output", || {
        let t = read(&mut h, &id);
        t.contains("ROLE=").then_some(t)
    });
    assert!(out.contains("ROLE=agent"), "{out}");
}

#[test]
fn agent_streams_are_view_only() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    let mut s = raw(&d);
    let attach = json!({ "jsonrpc": "2.0", "id": 1, "method": "stream.attach",
        "params": { "session": id, "cols": 40, "rows": 10, "cell_w": 8, "cell_h": 16, "caller": { "role": "agent" } } });
    let r = send_line(&mut s, attach.to_string().as_bytes());
    assert_eq!(r["result"]["ok"], true, "{r}");
    let text = b"echo typed-by-agent-stream\r";
    let mut msg = vec![0x03];
    msg.extend_from_slice(&(text.len() as u32).to_le_bytes());
    msg.extend_from_slice(text);
    s.write_all(&msg).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(!read(&mut h, &id).contains("typed-by-agent-stream"), "agent stream input must be ignored");
    // The human's stream still types.
    let mut st = midna_proto::Client::attach_stream(d.socket(), &id, 40, 10, 8, 16).unwrap();
    st.input(b"echo typed-by-human-$((1+1))\r").unwrap();
    wait_for(5, "human stream input", || read(&mut h, &id).contains("typed-by-human-2").then_some(()));
}

#[test]
fn agents_cannot_type_into_a_permission_prompt() {
    let d = TestDaemon::start();
    let mut h = d.human();
    let id = open_sh(&mut h);
    let mut a = d.agent(Some(&id));
    // A permission prompt is pending on this terminal (raised the way agent.hook does).
    let daemon = d.daemon();
    let mut n = daemon.new_needs_you(midna_proto::NeedsYouKind::PermissionPrompt, "Bash(ls)".into(), midna_proto::Actor::system(), Some(id.clone()));
    n.detail = "test".into();
    daemon.raise_needs_you(n);
    let e = call_err(&mut a, "session.input", json!({ "id": id, "text": "1", "enter": true }));
    assert_eq!(e.code, HUMAN_ONLY, "{e:?}");
    let e = call_err(&mut a, "session.key", json!({ "id": id, "key": "enter" }));
    assert_eq!(e.code, HUMAN_ONLY, "{e:?}");
    // The human can.
    call(&mut h, "session.input", json!({ "id": id, "text": "echo human-ok", "enter": true }));
    wait_for(5, "human input", || read(&mut h, &id).contains("human-ok").then_some(()));
}

#[test]
fn socket_and_home_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let d = TestDaemon::start();
    let sock = std::fs::metadata(d.socket()).unwrap().permissions().mode() & 0o777;
    assert_eq!(sock, 0o600, "socket mode {sock:o}");
    let home = std::fs::metadata(&d.home).unwrap().permissions().mode() & 0o777;
    assert_eq!(home, 0o700, "home mode {home:o}");
}
