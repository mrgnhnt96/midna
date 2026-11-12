//! What agents may do with Nearby. Anything an agent sends to another device (`share.send` from
//! the CLI, MCP or a trigger's run) goes through policy first: the human approves it, or a rule
//! the human gave allows it ([`send`]). Taking what other devices send and confirming a pairing
//! are the human's alone; an agent asking gets the needs-you item that already waits for the
//! human ([`refusal`]).
use super::{link, pairing, share};
use crate::daemon::Daemon;
use crate::rpc::Ctx;
use midna_proto::RpcError;
use midna_proto::nearby::{self as nb, SharePayload, Tier};
use serde_json::{Value, json};

/// The most of a share's text the approval shows.
const SHOWN: usize = 2000;

/// The policy action an agent's share is checked as: `share send <kind> <device id>`. A rule
/// (or approving "always") can allow one kind to one device; the text isn't part of it.
pub fn send_value(payload: &SharePayload, fp: &str) -> String {
    let kind = serde_json::to_value(payload.kind()).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    format!("share send {kind} {fp}")
}

/// Before an agent's `share.send` connects: refuse a device it can't send to without asking,
/// then go through policy. The human's own sends pass.
pub fn send(d: &Daemon, ctx: &Ctx, payload: &SharePayload, fp: &str) -> Result<(), RpcError> {
    if ctx.is_human() {
        return Ok(());
    }
    if !link::trusted(d, fp) {
        return Err(RpcError::refused("that device isn't paired with this Mac"));
    }
    let (name, tier) = {
        let mut i = d.nearby.inner();
        let stored = i.store(&d.cfg.home).get(fp).map(|s| (s.name.clone(), s.tier));
        let seen = i.peers.contains_key(fp);
        let Some(stored) = stored else { return Err(RpcError::refused("that device isn't paired with this Mac")) };
        if !seen {
            return Err(RpcError::not_found("that device can't be seen on the network now"));
        }
        stored
    };
    let who = match tier {
        Tier::MyDevice => "one of your Macs",
        Tier::Contact => "a contact",
    };
    let (title, what) = ask(&name, payload);
    let value = send_value(payload, fp);
    let detail = format!(
        "{what}\n\nAn agent wants to send this to {name}, {who} (device {}). Approving lets it leave this Mac; \
         \"always\" lets agents here send {name} this kind of share without asking. Deny to keep it here.",
        nb::short_fingerprint(fp)
    );
    crate::rpc::policy::gate_off_machine(d, ctx, &value, title, detail)
}

/// The approval's title and what it shows of the share. The first line of what it shows is a
/// one-line summary (the banner shows only that line); the text, file list or scrollback follow
/// after a blank line. Names people gave (tabs, agent sessions, snippets, files) are in “…”, so
/// they don't run into the device's name.
fn ask(name: &str, payload: &SharePayload) -> (String, String) {
    let lines = |s: &str| {
        let n = s.split('\n').count();
        format!("{n} line{}", if n == 1 { "" } else { "s" })
    };
    match payload {
        SharePayload::Message { text, .. } => {
            let (head, whole) = headline(text);
            let what = if whole { format!("Message: {head}") } else { format!("Message, {}: {head}\n\n{}", lines(text), cut(text)) };
            (format!("Send {name} a message?"), what)
        }
        SharePayload::Snippet { text, title } => {
            let named = title.as_deref().map(|t| format!(" “{}”", headline(t).0)).unwrap_or_default();
            (format!("Send {name} a snippet{named}?"), format!("Snippet{named}, {}\n\n{}", lines(text), cut(text)))
        }
        SharePayload::Link { url, title } => {
            let (head, whole) = headline(url);
            let named = title.as_deref().map(|t| format!(" “{}”", headline(t).0)).unwrap_or_default();
            let rest = if whole { String::new() } else { format!("\n\n{}", cut(url)) };
            (format!("Send {name} a link?"), format!("Link{named}: {head}{rest}"))
        }
        SharePayload::File { name: file, folder, files, .. } => {
            let size = nb::size_label(payload.size());
            let shown = nb::safe_name(file);
            if *folder {
                let n = files.len();
                let listed = files.iter().take(20).map(|f| format!("  {}  {}", f.path, nb::size_label(f.size))).collect::<Vec<_>>().join("\n");
                let more = if n > 20 { format!("\n  … and {} more", n - 20) } else { String::new() };
                let what = format!("Folder: {shown} ({size}, {n} file{})\n\n{listed}{more}", if n == 1 { "" } else { "s" });
                (format!("Send {name} the folder “{shown}”?"), what)
            } else {
                (format!("Send {name} the file “{shown}”?"), format!("File: {shown} ({size})"))
            }
        }
        SharePayload::Tab { title, .. } => (format!("Send {name} the terminal tab “{title}”?"), super::tab::describe(payload, SHOWN)),
        SharePayload::Object { object } => {
            (format!("Send {name} {}?", object.label()), format!("From Midna: {}\n\n{}", object.label(), cut(&super::objects::listing(object))))
        }
        SharePayload::Agent { title, agent, .. } => {
            (format!("Send {name} the {} conversation “{title}”?", super::session::agent_name(*agent)), super::session::describe(payload))
        }
    }
}

/// The first line of `s` with text on it, cut to 80 characters, and whether that is all of `s`.
fn headline(s: &str) -> (String, bool) {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let mut head: String = line.chars().take(80).collect();
    if head.len() < line.len() {
        head.push('…');
    }
    let whole = head == s.trim();
    (head, whole)
}

fn cut(s: &str) -> String {
    let mut out: String = s.chars().take(SHOWN).collect();
    if out.len() < s.len() {
        out.push('…');
    }
    out
}

/// An agent called a human-only Nearby method: the refusal names the needs-you item the human
/// answers instead (each pairing and each incoming share raised one), so the agent can point at
/// it. None for other methods.
pub fn refusal(d: &Daemon, method: &str, params: &Value) -> Option<RpcError> {
    let id = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let (message, waiting) = match method {
        pairing::CONFIRM_METHOD => {
            let pr = id("pairing");
            (
                "peers.confirm is human only: the human compares the codes on both Macs and confirms".to_string(),
                d.nearby.inner().pairings.get(&pr).filter(|s| !s.info.confirmed && !s.info.state.is_over()).and_then(|s| s.info.needs_you_id.clone()),
            )
        }
        share::ACCEPT_METHOD | share::DECLINE_METHOD => {
            (format!("{method} is human only: agents can't accept or decline what other devices send"), share::waiting_on(d, &id("id")))
        }
        _ => return None,
    };
    Some(match waiting {
        Some(ny) => RpcError::human_only(format!("{message}; it waits for them as needs-you {ny}")).with_data(json!({ "needs_you_id": ny })),
        None => RpcError::human_only(format!("{message} (nothing of that id waits for them here)")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use midna_proto::nearby::{ShareFile, TabProject};

    fn file(path: &str, size: u64) -> ShareFile {
        ShareFile { path: path.into(), size, sha256: String::new() }
    }

    fn first_line(what: &str) -> &str {
        what.lines().next().unwrap_or_default()
    }

    #[test]
    fn a_tab_is_named_in_quotes_and_summed_up_before_its_scrollback() {
        let rows: Vec<String> = (0..412).map(|n| format!("row {n}")).collect();
        let tab = SharePayload::Tab {
            title: "widget api".into(),
            project: Some(TabProject { remote: "github.com/acme/widget".into(), path: String::new() }),
            cwd: "/w/widget".into(),
            shell: Some("zsh".into()),
            scrollback: rows.join("\n"),
        };
        let (title, what) = ask("Bravo", &tab);
        assert_eq!(title, "Send Bravo the terminal tab “widget api”?");
        assert_eq!(first_line(&what), "Tab: github.com/acme/widget (in /w/widget, zsh), 412 lines of scrollback.");
        assert!(what.contains("\n\nrow 382\n") && what.ends_with("row 411"), "{what}");
    }

    #[test]
    fn an_agent_session_is_named_in_quotes() {
        let agent = SharePayload::Agent {
            title: "fix login".into(),
            agent: midna_proto::AgentKind::Claude,
            project: None,
            cwd: "/w/app".into(),
            home: "/Users/a".into(),
            conversation: "c1".into(),
            transcript: file("c1.jsonl", 2048),
        };
        let (title, what) = ask("Bravo", &agent);
        assert!(title.ends_with(" conversation “fix login”?"), "{title}");
        assert!(first_line(&what).contains("conversation c1: /w/app"), "{what}");
    }

    #[test]
    fn texts_lead_with_a_one_line_summary() {
        let (_, what) = ask("Bravo", &SharePayload::Message { text: "the secret plans".into(), reply_to: None });
        assert_eq!(what, "Message: the secret plans");

        let (_, what) = ask("Bravo", &SharePayload::Message { text: "\nfirst\nsecond".into(), reply_to: None });
        assert_eq!(what, "Message, 3 lines: first\n\n\nfirst\nsecond");

        let long = "x".repeat(100);
        let (_, what) = ask("Bravo", &SharePayload::Message { text: long.clone(), reply_to: None });
        assert_eq!(first_line(&what), format!("Message, 1 line: {}…", &long[..80]));

        let (title, what) = ask("Bravo", &SharePayload::Snippet { text: "make\nmake test".into(), title: Some("build".into()) });
        assert_eq!((title.as_str(), what.as_str()), ("Send Bravo a snippet “build”?", "Snippet “build”, 2 lines\n\nmake\nmake test"));

        let (_, what) = ask("Bravo", &SharePayload::Link { url: "https://example.com/a".into(), title: Some("Docs".into()) });
        assert_eq!(what, "Link “Docs”: https://example.com/a");
    }

    #[test]
    fn a_folder_is_summed_up_before_its_files() {
        let files: Vec<ShareFile> = (0..22).map(|n| file(&format!("f{n}"), 10)).collect();
        let folder = SharePayload::File { name: "logs".into(), folder: true, files, dirs: vec![] };
        let (title, what) = ask("Bravo", &folder);
        assert_eq!(title, "Send Bravo the folder “logs”?");
        assert!(first_line(&what).starts_with("Folder: logs (") && first_line(&what).ends_with(", 22 files)"), "{what}");
        assert!(what.contains("\n\n  f0  ") && what.ends_with("\n  … and 2 more"), "{what}");

        let one = SharePayload::File { name: "a.txt".into(), folder: false, files: vec![file("a.txt", 5)], dirs: vec![] };
        assert!(ask("Bravo", &one).1.starts_with("File: a.txt ("));
    }
}
