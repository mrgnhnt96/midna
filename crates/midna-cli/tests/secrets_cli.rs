//! `midna secret` against an in-process daemon (file secret store). This test process is the
//! "GUI", so it stores secrets as the human; the CLI runs as an agent.
use midna_proto::Client;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct D {
    handle: Option<midnad::Handle>,
    home: PathBuf,
}

impl D {
    fn start(tag: &str) -> D {
        let home = PathBuf::from(format!("/tmp/midna-sc-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let mut cfg = midnad::Config::for_home(home.clone());
        cfg.app_path = Some(std::env::current_exe().unwrap().to_string_lossy().into_owned());
        cfg.cli_path = env!("CARGO_BIN_EXE_midna").into();
        cfg.webhooks.secrets = midnad::webhooks::secrets::Mode::File;
        cfg.webhooks.port_override = Some(0);
        cfg.webhooks.tailscale_bin = Some("/nonexistent/tailscale".into());
        D { handle: Some(midnad::start(cfg).unwrap()), home }
    }
    fn human(&self) -> Client {
        let mut c = Client::connect(self.home.join("midnad.sock")).unwrap();
        c.set_caller(None);
        c
    }
    fn midna(&self, args: &[&str], cwd: Option<&std::path::Path>) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
        cmd.args(args).env("MIDNA_SOCKET", self.home.join("midnad.sock")).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
        if let Some(c) = cwd {
            cmd.current_dir(c);
        }
        cmd.stdin(Stdio::null()).output().unwrap()
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
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const VALUE: &str = "ghp_aB3dE6gH9jK2mN5pQ8sT1vW4yZ7bC0eF3hJ6";

#[test]
fn exec_sets_env_and_masks_output() {
    let d = D::start("exec");
    let s = d.human().call_value("secret.set", json!({ "name": "GITHUB_TOKEN", "value": VALUE, "label": "GitHub token" })).unwrap();
    assert_eq!(s["name"], "GITHUB_TOKEN");
    assert!(s.get("value").is_none());

    // The command sees the value; the output the agent reads doesn't, in any common form.
    let o = d.midna(&["secret", "exec", "GITHUB_TOKEN", "GH=GITHUB_TOKEN", "--", "sh", "-c", "echo \"t=$GITHUB_TOKEN\"; printf %s \"$GH\" | base64; echo \"$GH\" >&2; exit 7"], None);
    assert_eq!(o.status.code(), Some(7), "{}", err(&o));
    assert!(!out(&o).contains(VALUE) && !err(&o).contains(VALUE));
    let b64 = String::from_utf8(Command::new("sh").args(["-c", &format!("printf %s {VALUE} | base64")]).output().unwrap().stdout).unwrap();
    assert!(!out(&o).contains(b64.trim()), "{}", out(&o));
    assert_eq!(out(&o).lines().next(), Some("t=‹GITHUB_TOKEN›"));
    assert_eq!(err(&o).trim(), "‹GITHUB_TOKEN›");

    // Listing shows names and use, never values; events and audit don't carry it either.
    let o = d.midna(&["secret", "list", "--json"], None);
    let list: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(list[0]["name"], "GITHUB_TOKEN");
    assert_eq!(list[0]["used"], 1);
    assert!(!out(&o).contains(VALUE));
    let log = std::fs::read_to_string(d.home.join("events.jsonl")).unwrap();
    assert!(log.contains("secret.used") && !log.contains(VALUE), "value leaked into the event log");
    assert!(!std::fs::read_to_string(d.home.join("state.json")).unwrap_or_default().contains(VALUE));

    // Unknown names fail before anything runs.
    let o = d.midna(&["secret", "exec", "NOPE", "--", "sh", "-c", "echo ran"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(!out(&o).contains("ran") && err(&o).contains("no secret NOPE"), "{}", err(&o));
}

#[test]
fn values_only_reach_exec() {
    let d = D::start("guard");
    d.human().call_value("secret.set", json!({ "name": "TOK", "value": VALUE })).unwrap();
    // `midna call` refuses the internal method outright.
    let o = d.midna(&["call", "secret.exec_env", r#"{"names":["TOK"],"command":["x"]}"#], None);
    assert_ne!(o.status.code(), Some(0));
    assert!(!out(&o).contains(VALUE));
    // Any other client (not the midna binary) is refused by the daemon.
    let mut agent = Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(None);
    let e = agent.call_value("secret.exec_env", json!({ "names": ["TOK"], "command": ["x"] })).unwrap_err();
    assert!(e.to_string().contains("only `midna secret exec`"), "{e}");
    let e = d.human().call_value("secret.exec_env", json!({ "names": ["TOK"], "command": ["x"] })).unwrap_err();
    assert!(e.to_string().contains("only `midna secret exec`"), "{e}");
    // Removal by an agent asks the human; the secret stays.
    let o = d.midna(&["secret", "rm", "TOK"], None);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("human only"), "{}", err(&o));
    assert_eq!(d.human().call_value("secret.list", json!({})).unwrap().as_array().unwrap().len(), 1);
    // The human removes it, value and all.
    d.human().call_value("secret.remove", json!({ "name": "TOK" })).unwrap();
    assert!(std::fs::read_dir(d.home.join("vault")).unwrap().next().is_none());
}

impl D {
    fn midna_stdin(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_midna"));
        cmd.args(args).env("MIDNA_SOCKET", self.home.join("midnad.sock")).env_remove("MIDNA_SESSION").env_remove("MIDNA_HOME");
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
    fn files_contain(&self, needle: &str) -> bool {
        ["state.json", "events.jsonl"].iter().any(|f| std::fs::read_to_string(self.home.join(f)).unwrap_or_default().contains(needle))
    }
}

#[test]
fn agents_save_piped_tokens() {
    let d = D::start("save");
    // Piped in: stored, not exposed, never echoed; the trailing newline is the command's.
    let o = d.midna_stdin(&["secret", "save", "API_TOKEN", "--label", "Acme token", "--global"], "tok-from-a-command-123\n");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("saved API_TOKEN for every project") && !out(&o).contains("tok-from"), "{}", out(&o));
    let o = d.midna(&["secret", "exec", "API_TOKEN", "--", "sh", "-c", "printf '%s|' \"$API_TOKEN\""], None);
    assert_eq!(out(&o), "‹API_TOKEN›|");
    let list = d.human().call_value("secret.list", json!({})).unwrap();
    assert_eq!(list[0]["added_by"]["kind"], "agent");
    assert!(list[0].get("exposed").is_none(), "piped values aren't exposed: {list}");
    assert!(!d.files_contain("tok-from-a-command-123"));
    // The value is never an argument, and stdin must not be empty.
    assert_eq!(d.midna(&["secret", "save", "X", "the-value"], None).status.code(), Some(2));
    assert_eq!(d.midna_stdin(&["secret", "save", "X"], "").status.code(), Some(2));

    // A value the agent sends itself (MCP, `midna call`) is saved but marked exposed.
    let mut agent = Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(None);
    let s = agent.call_value("secret.set", json!({ "name": "SEEN", "value": "agent-sent-value-123", "global": true })).unwrap();
    assert_eq!(s["exposed"], true);
    assert!(!d.files_contain("agent-sent-value-123"));
    // An agent may replace an agent's secret at once.
    let o = d.midna_stdin(&["secret", "save", "API_TOKEN", "--global"], "tok-rotated-456");
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
}

#[test]
fn replacing_the_humans_secret_asks_first() {
    let d = D::start("replace");
    let mut h = d.human();
    h.call_value("secret.set", json!({ "name": "STRIPE_KEY", "value": "human-value-111" })).unwrap();
    let vault = || std::fs::read_dir(d.home.join("vault")).unwrap().count();

    // The agent's value waits in the vault; nothing changes and nothing is written to state.
    let o = d.midna_stdin(&["secret", "save", "STRIPE_KEY", "--global"], "agent-value-222");
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("human only") && err(&o).contains("needs-you"), "{}", err(&o));
    assert_eq!(vault(), 2);
    assert!(!d.files_contain("agent-value-222"));
    let item = h.call_value("needs_you.list", json!({})).unwrap()[0].clone();
    assert!(item["title"].as_str().unwrap().contains("replace your secret STRIPE_KEY"), "{item}");
    assert!(!item.to_string().contains("agent-value-222"));
    // The agent can't run the approval itself.
    let mut agent = Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(None);
    let e = agent.call_value("secret.replace", json!({ "pending": "p_x", "name": "STRIPE_KEY", "added_by": { "kind": "agent" } })).unwrap_err();
    assert!(e.to_string().contains("human"), "{e}");

    // Deny: the parked value is deleted and the human's stays.
    h.call_value("needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "deny" } })).unwrap();
    assert_eq!(vault(), 1);
    let o = d.midna(&["secret", "exec", "STRIPE_KEY", "--", "sh", "-c", "test \"$STRIPE_KEY\" = human-value-111 && echo same"], None);
    assert_eq!(out(&o).trim(), "same");

    // Approve: the agent's value replaces it, and it's marked as the agent's.
    d.midna_stdin(&["secret", "save", "STRIPE_KEY", "--global"], "agent-value-333");
    let item = h.call_value("needs_you.list", json!({})).unwrap()[0].clone();
    h.call_value("needs_you.resolve", json!({ "id": item["id"], "resolution": { "kind": "approve", "scope": { "kind": "once" } } })).unwrap();
    assert_eq!(vault(), 1);
    assert!(!d.files_contain("agent-value-333"));
    // (This command line names the value, so it lands in the audit trail from here on.)
    let o = d.midna(&["secret", "exec", "STRIPE_KEY", "--", "sh", "-c", "test \"$STRIPE_KEY\" = agent-value-333 && echo replaced"], None);
    assert_eq!(out(&o).trim(), "replaced");
    let list = h.call_value("secret.list", json!({})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["added_by"]["kind"], "agent");
}

#[test]
fn write_sets_a_dotenv_key() {
    let d = D::start("write");
    d.human().call_value("secret.set", json!({ "name": "STRIPE_SECRET_KEY", "value": "sk_live_abc def\"x" })).unwrap();
    let dir = d.home.join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".env"), "PORT=3000\nSTRIPE_KEY=old\n").unwrap();
    let o = d.midna(&["secret", "write", "STRIPE_SECRET_KEY", ".env", "--as", "STRIPE_KEY"], Some(&dir));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("replaced STRIPE_KEY") && !out(&o).contains("sk_live"), "{}", out(&o));
    assert_eq!(std::fs::read_to_string(dir.join(".env")).unwrap(), "PORT=3000\nSTRIPE_KEY=\"sk_live_abc def\\\"x\"\n");
    // A new file is private.
    let o = d.midna(&["secret", "write", "STRIPE_SECRET_KEY", "new.env"], Some(&dir));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(dir.join("new.env")).unwrap().permissions().mode() & 0o777, 0o600);
    let list = d.human().call_value("secret.list", json!({})).unwrap();
    assert_eq!(list[0]["written_to"].as_array().unwrap().len(), 2);
}

#[test]
fn project_secrets_shadow_global_ones() {
    let d = D::start("scope");
    let mut h = d.human();
    let dir = d.home.join("p");
    std::fs::create_dir_all(&dir).unwrap();
    let p = h.call_value("project.add", json!({ "path": dir.to_string_lossy() })).unwrap();
    let pid = p["id"].as_str().unwrap();
    h.call_value("secret.set", json!({ "name": "API_KEY", "value": "global-value-1" })).unwrap();
    h.call_value("secret.set", json!({ "name": "API_KEY", "value": "project-value-2", "project_id": pid })).unwrap();
    assert_eq!(h.call_value("secret.list", json!({})).unwrap().as_array().unwrap().len(), 2);
    let mut agent = Client::connect(d.home.join("midnad.sock")).unwrap().as_agent(None);
    assert_eq!(agent.call_value("secret.list", json!({ "project_id": pid })).unwrap().as_array().unwrap().len(), 2);
    assert_eq!(agent.call_value("secret.list", json!({})).unwrap().as_array().unwrap().len(), 1, "no project = globals only");
    // Replacing keeps one entry.
    h.call_value("secret.set", json!({ "name": "API_KEY", "value": "global-value-3" })).unwrap();
    assert_eq!(h.call_value("secret.list", json!({})).unwrap().as_array().unwrap().len(), 2);
    let e = h.call_value("secret.set", json!({ "name": "api-key", "value": "x" })).unwrap_err();
    assert!(e.to_string().contains("bad secret name"), "{e}");
}
