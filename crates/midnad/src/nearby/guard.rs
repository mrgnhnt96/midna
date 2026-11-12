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
    let (title, what) = match payload {
        SharePayload::Message { text, .. } => (format!("Send {name} a message?"), cut(text)),
        SharePayload::Snippet { text, title } => {
            let named = title.as_deref().map(|t| format!(" ({t})")).unwrap_or_default();
            (format!("Send {name} a snippet{named}?"), cut(text))
        }
        SharePayload::Link { url, title } => {
            let named = title.as_deref().map(|t| format!("{t}\n")).unwrap_or_default();
            (format!("Send {name} a link?"), format!("{named}{}", cut(url)))
        }
        SharePayload::File { name: file, folder, files, .. } => {
            let size = nb::size_label(payload.size());
            let shown = nb::safe_name(file);
            let listed = files.iter().take(20).map(|f| format!("  {}  {}", f.path, nb::size_label(f.size))).collect::<Vec<_>>().join("\n");
            let more = if files.len() > 20 { format!("\n  … and {} more", files.len() - 20) } else { String::new() };
            if *folder {
                let n = files.len();
                (format!("Send {name} the folder “{shown}”?"), format!("{shown} ({size}, {n} file{})\n{listed}{more}", if n == 1 { "" } else { "s" }))
            } else {
                (format!("Send {name} the file “{shown}”?"), format!("{shown} ({size})"))
            }
        }
    };
    let value = send_value(payload, fp);
    let detail = format!(
        "{what}\n\nAn agent wants to send this to {name}, {who} (device {}). Approving lets it leave this Mac; \
         \"always\" lets agents here send {name} this kind of share without asking. Deny to keep it here.",
        nb::short_fingerprint(fp)
    );
    crate::rpc::policy::gate_off_machine(d, ctx, &value, title, detail)
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
