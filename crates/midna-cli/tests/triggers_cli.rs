//! `midna triggers` / `midna webhooks` against an in-process daemon (file secrets, random
//! receiver port). The CLI runs as an agent here, so human-only steps are refused.
use serde_json::{Value, json};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct D {
    handle: Option<midnad::Handle>,
    home: PathBuf,
}

impl D {
    fn start(tag: &str) -> D {
        let home = PathBuf::from(format!("/tmp/midna-ct-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
        cfg.webhooks.secrets = midnad::webhooks::secrets::Mode::File;
        cfg.webhooks.port_override = Some(0);
        cfg.webhooks.tailscale_bin = Some("/nonexistent/tailscale".into());
        D { handle: Some(midnad::start(cfg).unwrap()), home }
    }
    fn midna(&self, args: &[&str], stdin: Option<&str>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
        cmd.args(args).env("MIDNA_SOCKET", self.home.join("midnad.sock")).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME").env_remove("MIDNA_TRIGGER").env_remove("MIDNA_DELIVERY");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        if let Some(input) = stdin {
            child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        }
        drop(child.stdin.take());
        child.wait_with_output().unwrap()
    }
}

impl Drop for D {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            h.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn triggers_cli_flow() {
    let d = D::start("flow");
    let o = d.midna(
        &["triggers", "add", "--name", "Review new PRs", "--event", "pull_request.opened", "--repo", "mrgnhnt96/midna", "--attention", "PR {{pr.number}}: {{pr.title}}", "--json"],
        None,
    );
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let t: Value = serde_json::from_str(&out(&o)).unwrap();
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(t["state"], "needs_secret");
    // The agent can't set the secret, even from stdin.
    let o = d.midna(&["triggers", "set-secret", &id], Some("hunter2\n"));
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("human only"));
    assert!(!d.home.join("secrets").join(&id).exists());
    // list (human text) and test (dry run from stdin)
    let o = d.midna(&["triggers", "list"], None);
    assert!(out(&o).contains("Review new PRs") && out(&o).contains("needs_secret"), "{}", out(&o));
    let payload = json!({ "action": "opened", "pull_request": { "number": 9, "title": "Hi" }, "repository": { "full_name": "mrgnhnt96/midna" } }).to_string();
    let o = d.midna(&["triggers", "test", &id, "--payload", "-"], Some(&payload));
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).contains("would raise attention: PR 9: Hi"), "{}", out(&o));
    // enable is human only → a needs-you item (secret first, since there's none).
    let o = d.midna(&["triggers", "enable", &id], None);
    assert_eq!(o.status.code(), Some(1));
    let o = d.midna(&["triggers", "deliveries"], None);
    assert!(out(&o).contains("no deliveries"));
    let o = d.midna(&["webhooks", "status", "--json"], None);
    let s: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(s["path"], "off");
    assert_eq!(s["receiver"]["listening"], true);
    let o = d.midna(&["webhooks", "configure", "tailscale_funnel"], None);
    assert_eq!(o.status.code(), Some(1), "configure is human only");
    let o = d.midna(&["triggers", "remove", &id], None);
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn local_triggers_cli() {
    let d = D::start("local");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}{}", out(o), String::from_utf8_lossy(&o.stderr));
    // Source is inferred from the event; --enable turns a local trigger on right away.
    let o = d.midna(
        &["triggers", "add", "--name", "Auto-compact", "--event", "agent.prompt_blocked", "--match", "message=*Compact first*", "--send", "/compact", "--send", "{{last_prompt}}", "--enable", "--json"],
        None,
    );
    ok(&o);
    let t: Value = serde_json::from_str(&out(&o)).unwrap();
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(t["source"], "local");
    assert_eq!(t["enabled"], true);
    assert_eq!(t["filter"]["match"], json!({ "message": "*Compact first*" }));
    assert_eq!(t["action"], json!({ "kind": "send_to_session", "steps": [{ "text": "/compact", "enter": true }, { "text": "{{last_prompt}}", "enter": true }] }));
    // --idle-for implies the idle event.
    let o = d.midna(&["triggers", "add", "--name", "Keep warm", "--idle-for", "55m", "--session", "s_nope", "--send", "ping", "--json"], None);
    ok(&o);
    let warm: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(warm["event"], "idle");
    assert_eq!(warm["filter"]["idle_minutes"], 55);
    let warm_id = warm["id"].as_str().unwrap().to_string();
    // --cron implies the schedule event; show lists the next runs.
    let o = d.midna(&["triggers", "add", "--name", "Standup", "--cron", "0 9 * * mon-fri", "--notify", "Standup", "--json"], None);
    ok(&o);
    let cron: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!((cron["event"].as_str(), cron["source"].as_str(), cron["filter"]["cron"].as_str()), (Some("schedule"), Some("local"), Some("0 9 * * mon-fri")));
    let cron_id = cron["id"].as_str().unwrap().to_string();
    let shown = out(&d.midna(&["triggers", "show", &cron_id], None));
    assert!(shown.contains(r#"local schedule cron="0 9 * * mon-fri""#) && shown.contains(" 09:00, ") && shown.contains("(local time)"), "{shown}");
    assert_eq!(d.midna(&["triggers", "add", "--name", "x", "--cron", "0 9 * *", "--notify", "x"], None).status.code(), Some(2));
    // A set_status trigger; then edit only its color and cooldown.
    let o = d.midna(
        &["triggers", "add", "--name", "Stopped", "--event", "hook.Stop", "--set-status", "Stopped", "--color", "teal", "--base", "done", "--clear-on", "turn", "--json"],
        None,
    );
    ok(&o);
    let st: Value = serde_json::from_str(&out(&o)).unwrap();
    let st_id = st["id"].as_str().unwrap().to_string();
    let o = d.midna(&["triggers", "update", &st_id, "--color", "#ff8800", "--cooldown", "5m", "--json"], None);
    ok(&o);
    let st: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(st["action"]["color"], "#ff8800");
    assert_eq!(st["action"]["label"], "Stopped");
    assert_eq!(st["cooldown_secs"], 300);
    // Human output: list and show summarize local filters and actions.
    let o = d.midna(&["triggers", "list"], None);
    let text = out(&o);
    assert!(text.contains(r#"send: "/compact" → "{{last_prompt}}""#), "{text}");
    assert!(text.contains("idle=55m") && text.contains("status: Stopped (#ff8800, done, clears on turn)"), "{text}");
    assert!(text.contains("(builtin)"), "the built-in Prompt blocked trigger is listed: {text}");
    let o = d.midna(&["triggers", "show", &id], None);
    assert!(out(&o).contains("cooldown 60s") && out(&o).contains(r#"match message="*Compact first*""#), "{}", out(&o));
    // Bad flags are usage errors (exit 2), before reaching the daemon.
    assert_eq!(d.midna(&["triggers", "add", "--name", "x", "--event", "pull_request.opened", "--source", "github", "--send", "hi"], None).status.code(), Some(2));
    assert_eq!(d.midna(&["triggers", "add", "--name", "x", "--event", "hook.Stop", "--set-status", "X", "--color", "mauve", "--base", "idle"], None).status.code(), Some(2));
    // Dry run with an inline payload.
    let o = d.midna(&["triggers", "test", &id, "--payload", r#"{"hook":"UserPromptSubmit","message":"Context low. Compact first.","prompt":"hi"}"#], None);
    ok(&o);
    for t in [&id, &warm_id, &st_id, &cron_id] {
        let o = d.midna(&["triggers", "disable", t], None);
        ok(&o);
        ok(&d.midna(&["triggers", "remove", t], None));
    }
}

#[test]
fn run_command_background_and_headless_cli() {
    let d = D::start("run");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}{}", out(o), String::from_utf8_lossy(&o.stderr));
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap();
    h.set_caller(None);
    let p: Value = h.call("project.add", json!({ "path": dir })).unwrap();
    let pid = p["id"].as_str().unwrap();

    let o = d.midna(&["triggers", "add", "--name", "Compaction", "--cron", "0 6 * * mon-fri", "--run", "echo compacted", "--project", pid, "--headless", "--timeout", "10m", "--enable", "--json"], None);
    ok(&o);
    let t: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(t["action"], json!({ "kind": "run_command", "project_id": pid, "command": "echo compacted", "headless": true, "timeout_secs": 600 }));
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!(d.midna(&["triggers", "add", "--name", "x", "--cron", "@daily", "--run", "true", "--project", pid, "--background", "--headless"], None).status.code(), Some(2));

    // It runs with no terminal; `deliveries` shows how it ended.
    let at = midna_proto::cron::Cron::parse("0 6 * * mon-fri").unwrap().next_after(midna_proto::time::now_unix()).unwrap();
    midnad::local::fire_schedule_now(&d.handle.as_ref().unwrap().daemon, &id, at);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let text = loop {
        let text = out(&d.midna(&["triggers", "deliveries", "--trigger", &id], None));
        if text.contains("(exit 0)") || std::time::Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(text.contains("Running with no terminal › Compaction (exit 0)"), "{text}");
    let list = out(&d.midna(&["triggers", "list"], None));
    assert!(list.contains("with no terminal (timeout 600s)"), "{list}");

    // --background alone moves it to a terminal in the Background group.
    let o = d.midna(&["triggers", "update", &id, "--background", "--json"], None);
    ok(&o);
    let t: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(t["action"], json!({ "kind": "run_command", "project_id": pid, "command": "echo compacted", "background": true }));
}

#[test]
fn stream_publish_cli() {
    let d = D::start("stream");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}{}", out(o), String::from_utf8_lossy(&o.stderr));
    let o = d.midna(&["triggers", "add", "--name", "CI", "--event", "stream.slack.*", "--match", "text=*failed*", "--notify", "{{stream.name}}: {{data.text}}", "--json"], None);
    ok(&o);
    let t: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(t["source"], "local", "stream events are local");
    // Inline, from stdin and from a file; --json gives the seq and kind.
    let o = d.midna(&["stream", "publish", "slack", "message", "--id", "17.1", "--data", r#"{"text":"build failed"}"#], None);
    ok(&o);
    assert!(out(&o).contains("published stream.slack.message"), "{}", out(&o));
    let o = d.midna(&["stream", "publish", "slack", "message", "--json"], Some(r#"{"text":"from stdin"}"#));
    ok(&o);
    let r: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(r["event"], "stream.slack.message");
    let f = d.home.join("payload.json");
    std::fs::write(&f, r#"{"text":"from a file"}"#).unwrap();
    ok(&d.midna(&["stream", "publish", "ci", "build.done", "--data", &format!("@{}", f.display())], None));
    let o = d.midna(&["events", "--kind", "stream.slack.message", "--json"], None);
    let texts = out(&o);
    assert!(texts.contains("build failed") && texts.contains("from stdin") && texts.contains("17.1"), "{texts}");
    // Refused: a bad name, a payload that isn't an object, a missing kind.
    let o = d.midna(&["stream", "publish", "Slack", "message"], None);
    assert!(String::from_utf8_lossy(&o.stderr).contains("a-z 0-9 -") && o.status.code() != Some(0), "{}", out(&o));
    let o = d.midna(&["stream", "publish", "slack", "message", "--data", "[1]"], None);
    assert_ne!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stderr).contains("object"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = d.midna(&["stream", "publish", "slack"], None);
    assert_ne!(o.status.code(), Some(0));
}

/// A trigger whose command publishes to the stream it listens on fires once, not in a loop:
/// what its run_command starts (in a terminal or headless) carries MIDNA_TRIGGER, the CLI
/// forwards it, and the event it publishes is recorded with `caused_by` but matched by nothing.
/// The human publishing again still fires it.
#[test]
fn trigger_caused_publishes_fire_no_triggers() {
    let d = D::start("loop");
    let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}{}", out(o), String::from_utf8_lossy(&o.stderr));
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = midna_proto::Client::connect(d.home.join("midnad.sock")).unwrap();
    h.set_caller(None);
    let p: Value = h.call("project.add", json!({ "path": dir })).unwrap();
    let pid = p["id"].as_str().unwrap();
    let cli = env!("CARGO_BIN_EXE_midna");
    let mut add = |name: &str, stream: &str, extra: Value| -> String {
        let mut action = json!({ "kind": "run_command", "project_id": pid, "command": format!("{cli} stream publish {stream} again --data '{{\"n\":1}}'") });
        action.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        // No cooldown: the human's second publish below must fire it again within the minute.
        let t: Value = h.call("trigger.add", json!({ "name": name, "source": "local", "event": format!("stream.{stream}.*"), "action": action, "enabled": true, "cooldown_secs": 0 })).unwrap();
        t["id"].as_str().unwrap().to_string()
    };
    let in_terminal = add("Echo in a terminal", "loop", json!({ "background": true }));
    let headless = add("Echo headless", "hl", json!({ "headless": true }));
    let fired = |h: &mut midna_proto::Client, id: &str| -> u64 {
        let list: Value = h.call("trigger.list", json!({})).unwrap();
        let list = list.get("triggers").cloned().unwrap_or(list);
        list.as_array().unwrap().iter().find(|t| t["id"] == id).map(|t| t["fired"].as_u64().unwrap_or(0)).unwrap()
    };
    // One JSON event per line.
    let events = |kind: &str| -> Vec<Value> {
        out(&d.midna(&["events", "--kind", kind, "--json"], None)).lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).unwrap()).collect()
    };
    let wait_for = |kind: &str, n: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while events(kind).len() < n && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        events(kind)
    };

    for (stream, id) in [("loop", &in_terminal), ("hl", &headless)] {
        ok(&d.midna(&["stream", "publish", stream, "start"], None));
        let again = wait_for(&format!("stream.{stream}.again"), 1);
        assert_eq!(again.len(), 1, "{stream}: the trigger's command published once");
        assert_eq!(again[0]["stream"]["caused_by"]["trigger"], json!(id), "{stream}: {}", again[0]);
        // Long enough for a loop to show: each round is a few hundred ms.
        std::thread::sleep(std::time::Duration::from_secs(3));
        assert_eq!(events(&format!("stream.{stream}.again")).len(), 1, "{stream}: the trigger-caused publish fired nothing");
        assert_eq!(fired(&mut h, id), 1, "{stream}: fired once");
    }
    // The terminal carries the trigger to anything run in it; its delivery too.
    let again = &events("stream.loop.again")[0]["stream"]["caused_by"];
    assert!(again["delivery"].as_str().is_some_and(|d| d.starts_with("d_")), "{again}");

    // A publish nothing started still fires, and a forwarded MIDNA_TRIGGER is reported back.
    let o = d.midna(&["stream", "publish", "loop", "start"], None);
    ok(&o);
    assert!(!out(&o).contains("fires no triggers"), "{}", out(&o));
    assert_eq!(wait_for("stream.loop.again", 2).len(), 2);
    assert_eq!(fired(&mut h, &in_terminal), 2);
    let mut cmd = Command::new(cli);
    cmd.args(["stream", "publish", "quiet", "x"]).env("MIDNA_SOCKET", d.home.join("midnad.sock")).env_remove("MIDNA_SESSION").env("MIDNA_TRIGGER", "t_x");
    let o = cmd.output().unwrap();
    ok(&o);
    assert!(out(&o).contains("trigger t_x started this publisher, so the event fires no triggers"), "{}", out(&o));
}
