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
