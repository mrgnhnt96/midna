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
        cmd.args(args).env("MIDNA_SOCKET", self.home.join("midnad.sock")).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
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
