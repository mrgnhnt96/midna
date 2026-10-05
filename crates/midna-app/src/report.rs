//! "Report an issue" (⌘K): opens a new GitHub issue on midna's repo with the machine's details
//! filled in. The repo is public, so the body carries versions and hardware only: no paths,
//! project names, terminal output or logs (the reporter can paste those if they choose).
use serde_json::Value;
use std::process::Command;

pub const REPO: &str = "mrgnhnt96/midna";

/// What the issue body reports. Gathered off the main thread ([`gather`] runs processes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub app: String,
    /// `daemon.info`'s version, or `None` when the daemon didn't answer.
    pub daemon: Option<String>,
    pub daemon_uptime: Option<u64>,
    pub channel: String,
    /// "15.1 (24B83)".
    pub macos: String,
    /// "Mac15,6 · Apple M3 Pro · 36 GB".
    pub hardware: String,
    /// "arm64", "x86_64 (Rosetta)".
    pub arch: String,
    pub install: String,
    pub accessibility: bool,
    pub kass: bool,
    pub shell: String,
    /// "3 terminals, 2 agents".
    pub terminals: String,
    /// Running agents, deduplicated: "Claude 2.1.3 (opus)".
    pub agents: Vec<String>,
}

/// Everything the window knows already; [`gather`] adds the system facts.
pub struct Known {
    pub daemon_info: Option<Value>,
    pub channel: String,
    pub accessibility: bool,
    pub kass: bool,
    pub terminals: usize,
    pub agents: Vec<String>,
    pub agent_count: usize,
}

pub fn gather(k: Known) -> Facts {
    let run = |cmd: &str, args: &[&str]| -> String {
        Command::new(cmd).args(args).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
    };
    let sysctl = |name: &str| run("/usr/sbin/sysctl", &["-n", name]);
    let version = run("/usr/bin/sw_vers", &["-productVersion"]);
    let build = run("/usr/bin/sw_vers", &["-buildVersion"]);
    let mem_gb = sysctl("hw.memsize").parse::<u64>().map(|b| format!("{} GB", b >> 30)).unwrap_or_default();
    let hardware = [sysctl("hw.model"), sysctl("machdep.cpu.brand_string"), mem_gb].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    let rosetta = sysctl("sysctl.proc_translated") == "1";
    let info = k.daemon_info.as_ref();
    Facts {
        app: midna_proto::VERSION.into(),
        daemon: info.and_then(|v| v.get("version")).and_then(Value::as_str).map(str::to_string),
        daemon_uptime: info.and_then(|v| v.get("uptime_secs")).and_then(Value::as_u64),
        channel: k.channel,
        macos: if build.is_empty() { version } else { format!("{version} ({build})") },
        hardware,
        arch: format!("{}{}", std::env::consts::ARCH, if rosetta { " (Rosetta)" } else { "" }),
        install: install_kind(&std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default(), &std::env::var("HOME").unwrap_or_default()),
        accessibility: k.accessibility,
        kass: k.kass,
        shell: std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(str::to_string)).unwrap_or_default(),
        terminals: format!("{} terminal{}, {} agent{}", k.terminals, plural(k.terminals), k.agent_count, plural(k.agent_count)),
        agents: k.agents,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Where the running binary lives, without the user's name in it.
fn install_kind(exe: &str, home: &str) -> String {
    if exe.starts_with("/Applications/") {
        "/Applications".into()
    } else if !home.is_empty() && exe.starts_with(&format!("{home}/Applications/")) {
        "~/Applications".into()
    } else if exe.contains("/target/debug/") || exe.contains("/target/release/") {
        "dev build (cargo)".into()
    } else if exe.contains(".app/") {
        "app bundle elsewhere".into()
    } else {
        "other".into()
    }
}

/// The issue body: prompts for the report, then the environment table.
pub fn body(f: &Facts) -> String {
    let yes = |b: bool| if b { "granted" } else { "not granted" };
    let daemon = match (&f.daemon, f.daemon_uptime) {
        (Some(v), Some(up)) => format!("{v} · up {}", crate::ui::charts::duration(up as f64)),
        (Some(v), None) => v.clone(),
        (None, _) => "not connected".into(),
    };
    let mut rows = vec![
        ("midna", format!("{} ({})", f.app, f.channel)),
        ("midnad", daemon),
        ("macOS", f.macos.clone()),
        ("Mac", f.hardware.clone()),
        ("Arch", f.arch.clone()),
        ("Installed in", f.install.clone()),
        ("Accessibility", yes(f.accessibility).into()),
        ("Kass", if f.kass { "seen this run" } else { "not seen" }.into()),
        ("Shell", f.shell.clone()),
        ("Open", f.terminals.clone()),
    ];
    if !f.agents.is_empty() {
        rows.push(("Agents", f.agents.join(", ")));
    }
    let table: String = rows.into_iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("| {k} | {} |\n", v.replace('|', "\\|"))).collect();
    format!(
        "### What happened\n\n\n\n### What you expected\n\n\n\n### Steps to reproduce\n\n1. \n\n\
         ### Environment\n\n| | |\n|---|---|\n{table}\n\
         <!-- Filled in by midna's \"Report an issue\". No paths, project names or terminal output are included. -->\n"
    )
}

/// `https://github.com/<REPO>/issues/new?body=…`.
pub fn issue_url(f: &Facts) -> String {
    format!("https://github.com/{REPO}/issues/new?body={}", encode(&body(f)))
}

/// Percent-encode a query value (everything but RFC 3986 unreserved characters).
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            app: "0.3.0".into(),
            daemon: Some("0.3.0".into()),
            daemon_uptime: None,
            channel: "beta".into(),
            macos: "15.1 (24B83)".into(),
            hardware: "Mac15,6 · Apple M3 Pro · 36 GB".into(),
            arch: "arm64".into(),
            install: "/Applications".into(),
            accessibility: true,
            kass: false,
            shell: "zsh".into(),
            terminals: "3 terminals, 1 agent".into(),
            agents: vec!["Claude 2.1.3 (opus)".into()],
        }
    }

    #[test]
    fn body_lists_the_environment() {
        let b = body(&facts());
        assert!(b.contains("| midna | 0.3.0 (beta) |"));
        assert!(b.contains("| midnad | 0.3.0 |"));
        assert!(b.contains("| macOS | 15.1 (24B83) |"));
        assert!(b.contains("| Accessibility | granted |"));
        assert!(b.contains("| Agents | Claude 2.1.3 (opus) |"));
        assert!(b.starts_with("### What happened"));
    }

    #[test]
    fn empty_rows_and_no_daemon() {
        let b = body(&Facts { daemon: None, shell: String::new(), agents: vec![], ..facts() });
        assert!(b.contains("| midnad | not connected |"));
        assert!(!b.contains("| Shell |"));
        assert!(!b.contains("| Agents |"));
    }

    #[test]
    fn url_is_encoded() {
        let u = issue_url(&facts());
        assert!(u.starts_with("https://github.com/mrgnhnt96/midna/issues/new?body=%23%23%23%20What"));
        assert!(!u[u.find('?').unwrap() + 1..].contains([' ', '\n', '|', '#', '&']));
        assert_eq!(encode("a b/✓"), "a%20b%2F%E2%9C%93");
    }

    #[test]
    fn install_kind_hides_the_home_folder() {
        assert_eq!(install_kind("/Applications/Midna.app/Contents/MacOS/midna", "/Users/x"), "/Applications");
        assert_eq!(install_kind("/Users/x/Applications/Midna.app/Contents/MacOS/midna", "/Users/x"), "~/Applications");
        assert_eq!(install_kind("/Users/x/dev/midna/target/debug/midna-app", "/Users/x"), "dev build (cargo)");
        assert_eq!(install_kind("/Volumes/Midna/Midna.app/Contents/MacOS/midna", "/Users/x"), "app bundle elsewhere");
    }
}
