//! Tailscale Funnel delivery path: find the CLI, read `tailscale status --json` and
//! `tailscale funnel status --json`, and build the `tailscale funnel` command.
//!
//! The funnel command must run as a direct foreground child with a timeout. A backgrounded
//! subshell makes the macOS GUI-bundled CLI fail with "GUI failed to start".
use midna_proto::TailscaleStatus;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const APP_CLI: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
pub const FUNNEL_PORT: u16 = 8443;

/// `MIDNA_TAILSCALE` override, else the app bundle's CLI, else `tailscale` on PATH.
pub fn find_cli(override_path: Option<&str>) -> Option<String> {
    if let Some(p) = override_path {
        return std::path::Path::new(p).is_file().then(|| p.to_string());
    }
    if std::path::Path::new(APP_CLI).is_file() {
        return Some(APP_CLI.into());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':').map(|d| std::path::Path::new(d).join("tailscale")).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

/// Run argv directly (no shell) with a timeout. Returns (exit code if it exited, stdout+stderr).
pub fn run_capture(argv: &[String], timeout: Duration) -> (Option<i32>, String) {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (None, format!("could not run {}: {e}", argv[0])),
    };
    let reader = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            b
        })
    };
    let out = child.stdout.take().map(|o| reader(Box::new(o)));
    let err = child.stderr.take().map(|e| reader(Box::new(e)));
    let start = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code(),
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let mut text = String::new();
    for h in [out, err].into_iter().flatten() {
        if let Ok(b) = h.join() {
            text.push_str(&String::from_utf8_lossy(&b));
        }
    }
    (code, text)
}

/// Parse `tailscale status --json`.
pub fn parse_status(json: &str) -> TailscaleStatus {
    let mut st = TailscaleStatus::default();
    let v: Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(e) => {
            st.error = Some(format!("unreadable tailscale status: {e}"));
            return st;
        }
    };
    st.backend_state = v.get("BackendState").and_then(Value::as_str).map(str::to_string);
    let me = v.get("Self").cloned().unwrap_or_default();
    st.dns_name = me.get("DNSName").and_then(Value::as_str).map(|d| d.trim_end_matches('.').to_string()).filter(|d| !d.is_empty());
    st.online = me.get("Online").and_then(Value::as_bool).unwrap_or(false) && st.backend_state.as_deref() == Some("Running");
    let caps_list = me.get("Capabilities").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>()).unwrap_or_default();
    let cap_map: Vec<String> = me.get("CapMap").and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default();
    let has = |c: &str| caps_list.iter().chain(cap_map.iter()).any(|x| x == c);
    let ports_ok = caps_list.iter().chain(cap_map.iter()).filter(|x| x.contains("cap/funnel-ports")).all(|x| {
        x.split("ports=").nth(1).is_none_or(|p| p.split(',').any(|p| p.trim() == FUNNEL_PORT.to_string()))
    });
    st.funnel_allowed = has("funnel") && ports_ok;
    st
}

/// From `tailscale funnel status --json`: where `<dns>:8443` proxies to, and whether Funnel
/// (public) is on for it. Returns (funnel_on_for_our_port, target).
pub fn parse_funnel_status(json: &str, dns: &str, port: u16) -> (bool, Option<String>) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (false, None) };
    let hostport = format!("{dns}:{FUNNEL_PORT}");
    let target = v
        .pointer(&format!("/Web/{}/Handlers", hostport.replace('~', "~0").replace('/', "~1")))
        .and_then(Value::as_object)
        .and_then(|h| h.get("/").or_else(|| h.values().next()))
        .and_then(|h| h.get("Proxy"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let allowed = v.get("AllowFunnel").and_then(|a| a.get(&hostport)).and_then(Value::as_bool).unwrap_or(false);
    let ours = target.as_deref().is_some_and(|t| {
        let t = t.trim_end_matches('/');
        [format!("http://127.0.0.1:{port}"), format!("http://localhost:{port}"), format!("127.0.0.1:{port}"), format!("localhost:{port}"), port.to_string()]
            .iter()
            .any(|x| x == t)
    });
    (allowed && ours, target)
}

/// `tailscale funnel --bg --https=8443 http://127.0.0.1:<port>`
pub fn funnel_on_args(cli: &str, port: u16) -> Vec<String> {
    vec![cli.into(), "funnel".into(), "--bg".into(), format!("--https={FUNNEL_PORT}"), format!("http://127.0.0.1:{port}")]
}

/// `tailscale funnel --https=8443 off`
pub fn funnel_off_args(cli: &str) -> Vec<String> {
    vec![cli.into(), "funnel".into(), format!("--https={FUNNEL_PORT}"), "off".into()]
}

/// The "enable Funnel for your tailnet" URL Tailscale prints when it isn't enabled.
pub fn find_enable_url(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|w| w.starts_with("https://login.tailscale.com/"))
        .map(|w| w.trim_end_matches(['.', ',', ')']).to_string())
}

pub fn public_base(dns: &str) -> String {
    format!("https://{dns}:{FUNNEL_PORT}")
}

/// Query both status commands (read-only).
pub fn query(cli: &str, port: u16) -> TailscaleStatus {
    let t = Duration::from_secs(8);
    let (code, out) = run_capture(&[cli.into(), "status".into(), "--json".into()], t);
    let mut st = if code == Some(0) {
        parse_status(&out)
    } else {
        TailscaleStatus { error: Some(format!("tailscale status failed: {}", out.lines().next().unwrap_or("timeout"))), ..Default::default() }
    };
    st.cli = Some(cli.to_string());
    if let Some(dns) = st.dns_name.clone() {
        let (code, out) = run_capture(&[cli.into(), "funnel".into(), "status".into(), "--json".into()], t);
        if code == Some(0) {
            let (on, target) = parse_funnel_status(&out, &dns, port);
            st.funnel_on = on;
            st.funnel_target = target;
        }
    }
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"{
      "Version": "1.102.4-t3caf7d9e7-g084ee3b64",
      "BackendState": "Running",
      "Self": {
        "DNSName": "morgans-macbook-pro.tail439d44.ts.net.",
        "HostName": "Morgan’s MacBook Pro",
        "Online": true,
        "CapMap": { "funnel": null, "https": null, "https://tailscale.com/cap/funnel-ports?ports=443,8443,10000": null }
      },
      "MagicDNSSuffix": "tail439d44.ts.net",
      "CertDomains": ["morgans-macbook-pro.tail439d44.ts.net"]
    }"#;

    #[test]
    fn status_fixture() {
        let st = parse_status(STATUS);
        assert_eq!(st.dns_name.as_deref(), Some("morgans-macbook-pro.tail439d44.ts.net"));
        assert!(st.online);
        assert!(st.funnel_allowed);
        let no_funnel = STATUS.replace("\"funnel\": null, ", "");
        assert!(!parse_status(&no_funnel).funnel_allowed);
        let stopped = STATUS.replace("\"Running\"", "\"Stopped\"");
        assert!(!parse_status(&stopped).online);
        assert!(parse_status("nope").error.is_some());
    }

    #[test]
    fn funnel_status_fixture() {
        let dns = "morgans-macbook-pro.tail439d44.ts.net";
        let on = r#"{"TCP":{"8443":{"HTTPS":true}},
            "Web":{"morgans-macbook-pro.tail439d44.ts.net:8443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7787"}}}},
            "AllowFunnel":{"morgans-macbook-pro.tail439d44.ts.net:8443":true}}"#;
        assert_eq!(parse_funnel_status(on, dns, 7787), (true, Some("http://127.0.0.1:7787".into())));
        assert!(!parse_funnel_status(on, dns, 9999).0, "proxies to a different port");
        let serve_only = on.replace("true}}", "false}}");
        assert!(!parse_funnel_status(&serve_only, dns, 7787).0);
        assert_eq!(parse_funnel_status("{}", dns, 7787), (false, None));
    }

    #[test]
    fn commands_and_enable_url() {
        assert_eq!(funnel_on_args("/x/tailscale", 7787), ["/x/tailscale", "funnel", "--bg", "--https=8443", "http://127.0.0.1:7787"]);
        assert_eq!(funnel_off_args("ts"), ["ts", "funnel", "--https=8443", "off"]);
        let out = "Funnel is not enabled on your tailnet.\nTo enable, visit:\n\n         https://login.tailscale.com/f/funnel?node=nXXXXCNTRL\n";
        assert_eq!(find_enable_url(out).as_deref(), Some("https://login.tailscale.com/f/funnel?node=nXXXXCNTRL"));
        assert_eq!(find_enable_url("Available on the internet:\nhttps://x.ts.net:8443/"), None);
        assert_eq!(public_base("h.ts.net"), "https://h.ts.net:8443");
    }

    #[test]
    fn run_capture_times_out() {
        let (code, _) = run_capture(&["/bin/sleep".into(), "5".into()], Duration::from_millis(100));
        assert_eq!(code, None);
        let (code, out) = run_capture(&["/bin/sh".into(), "-c".into(), "echo hi; echo err >&2".into()], Duration::from_secs(5));
        assert_eq!(code, Some(0));
        assert!(out.contains("hi") && out.contains("err"));
    }
}
