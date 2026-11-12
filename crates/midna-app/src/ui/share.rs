//! Nearby sharing in the app.
//!
//! The Share picker is a sheet that sends a terminal selection, the clipboard or a message you
//! type to one of your paired Macs (`share.send`). A share another Mac sends arrives as a
//! needs-you approval whose action is `midna inbox accept sh_…`: [`offer_of`] reads it for its
//! card in Needs you and [`panel`] draws the card's middle. `inbox.list` is cached on
//! `MainWindow.share` for the cards' full text and for the Notifications screen's Nearby tab
//! ([`inbox_rows`], [`inbox_detail`]), which lists past shares both ways.
use crate::app::{MainWindow, Overlay, Screen, refresh};
use crate::model::{ApprovalScope, Event, NeedsYou, Resolution, StatusState};
use crate::theme::Theme;
use crate::ui::screen_kit::{LineInput, btn, btn_primary, clock, day_label};
use gpui_kit::prelude::*;
use gpui_kit::*;
use midna_proto::nearby::{self as nb, InboxListResult, PairedPeer, PeersListResult, ShareDirection, ShareItem, ShareKind, SharePayload, ShareSendParams, ShareState, Tier, Transfer, TransferState};
use serde_json::{Value, json};

/// Nearby sharing's state on `MainWindow.share`.
#[derive(Default)]
pub struct Share {
    /// The open Share picker.
    pub picker: Option<Picker>,
    /// `inbox.list`: shares both ways, newest first.
    pub items: Vec<ShareItem>,
    /// Incoming shares still waiting for you.
    pub waiting: usize,
    /// The Nearby tab's selected share; None = the newest.
    pub sel: Option<String>,
    /// The Nearby tab's paste-into / pin-to chooser is open.
    pub chooser: bool,
}

/// The open Share picker.
pub struct Picker {
    kind: ShareKind,
    /// What it was opened with: a snippet's text, a link's URL. Empty for a message.
    text: String,
    message: LineInput,
    /// A reply: the share it answers.
    reply_to: Option<String>,
    /// A reply from a message's card: that card, marked read once the reply is sent.
    resolves: Option<String>,
    /// The device to choose first (a fingerprint, full or short).
    prefer: Option<String>,
    /// Your paired Macs, the ones on the network first; None while `peers.list` runs.
    peers: Option<Vec<PairedPeer>>,
    /// Midnas on the network that aren't paired with this Mac.
    unpaired: usize,
    sel: usize,
    busy: bool,
    error: Option<String>,
    /// The sheet's own focus: it opens over screens that hold `overlay_focus`.
    focus: FocusHandle,
}

// ------------------------------------------------------------------ pure helpers

/// The kinds `text` can be sent as, the likeliest first: a lone http(s) URL as a link, any
/// other text as a snippet; a message (typed in the picker) always.
pub fn kinds(text: &str) -> Vec<ShareKind> {
    let mut v = Vec::new();
    if is_url(text) {
        v.push(ShareKind::Link);
    }
    if !text.trim().is_empty() {
        v.push(ShareKind::Snippet);
    }
    v.push(ShareKind::Message);
    v
}

/// One http(s) URL and nothing else.
pub fn is_url(text: &str) -> bool {
    let t = text.trim();
    let rest = t.strip_prefix("https://").or_else(|| t.strip_prefix("http://"));
    rest.is_some_and(|r| !r.is_empty()) && !t.chars().any(|c| c.is_whitespace() || c.is_control()) && t.len() <= nb::MAX_URL
}

/// What the picker sends.
pub fn payload(kind: ShareKind, text: &str, message: &str, reply_to: Option<String>) -> SharePayload {
    match kind {
        ShareKind::Message | ShareKind::File | ShareKind::Tab | ShareKind::Object => SharePayload::Message { text: message.trim().to_string(), reply_to },
        ShareKind::Snippet => SharePayload::Snippet { text: text.to_string(), title: None },
        ShareKind::Link => SharePayload::Link { url: text.trim().to_string(), title: None },
    }
}

pub fn send_params(peer: &str, p: SharePayload) -> ShareSendParams {
    let kind = p.kind();
    let (text, url, title, reply_to) = match p {
        SharePayload::Message { text, reply_to } => (Some(text), None, None, reply_to),
        SharePayload::Snippet { text, title } => (Some(text), None, title, None),
        SharePayload::Link { url, title } => (None, Some(url), title, None),
        SharePayload::File { .. } | SharePayload::Tab { .. } | SharePayload::Object { .. } => (None, None, None, None),
    };
    ShareSendParams { peer: peer.to_string(), kind, text, url, title, reply_to, path: None, session: None, object: None, ids: Vec::new() }
}

/// Paired Macs on the network first, then by name.
pub fn order(mut v: Vec<PairedPeer>) -> Vec<PairedPeer> {
    v.sort_by(|a, b| b.nearby.cmp(&a.nearby).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    v
}

/// The device to choose first: `prefer` when it's on the network, else the first that is.
fn first_choice(peers: &[PairedPeer], prefer: Option<&str>) -> usize {
    let is = |p: &PairedPeer, f: &str| p.fingerprint == f || nb::short_fingerprint(&p.fingerprint) == f;
    prefer.and_then(|f| peers.iter().position(|p| p.nearby && is(p, f))).or_else(|| peers.iter().position(|p| p.nearby)).unwrap_or(0)
}

pub fn noun(k: ShareKind) -> &'static str {
    match k {
        ShareKind::Message => "message",
        ShareKind::Snippet => "snippet",
        ShareKind::Link => "link",
        ShareKind::File => "file",
        ShareKind::Tab => "tab",
        ShareKind::Object => "Midna items",
    }
}

fn kind_label(k: ShareKind) -> &'static str {
    match k {
        ShareKind::Message => "Message",
        ShareKind::Snippet => "Snippet",
        ShareKind::Link => "Link",
        ShareKind::File => "File",
        ShareKind::Tab => "Tab",
        ShareKind::Object => "Midna",
    }
}

pub fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::MyDevice => "My device",
        Tier::Contact => "Contact",
    }
}

/// "312 B", "4.2 KB".
pub fn size(bytes: u64) -> String {
    if bytes < 1024 { format!("{bytes} B") } else { format!("{:.1} KB", bytes as f64 / 1024.) }
}

/// The toast for one of your shares answered on the other Mac.
pub fn answered(it: &ShareItem) -> Option<String> {
    let what = noun(it.payload.kind());
    match (it.state, it.payload.kind()) {
        (ShareState::Accepted, ShareKind::Message) => Some(format!("{} read your message.", it.peer_name)),
        (ShareState::Accepted, _) => Some(format!("{} accepted your {what}.", it.peer_name)),
        (ShareState::Declined, _) => Some(format!("{} declined your {what}.", it.peer_name)),
        (ShareState::Offered, _) => None,
    }
}

// ------------------------------------------------------------------ offers (Needs you)

/// What a share card needs from its needs-you item (midnad's `nearby/share.rs` `ask`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    /// The share, `sh_…`.
    pub id: String,
    /// The sending Mac's name.
    pub name: String,
    pub tier: Option<Tier>,
    /// Its short fingerprint.
    pub device: String,
    pub kind: ShareKind,
    /// The text or URL, as the item carries it (long ones cut).
    pub body: String,
}

/// The share a needs-you item offers, if it's one.
pub fn offer_of(n: &NeedsYou) -> Option<Offer> {
    let a = n.approval.as_ref()?;
    let id = a.action.value.strip_prefix("midna inbox accept ")?.split_whitespace().next()?;
    if !nb::valid_share_id(id) {
        return None;
    }
    let (name, tier, device) = from_line(&n.detail).unwrap_or_default();
    let body = n.detail.rsplit_once("\n\n").map(|(b, _)| b).unwrap_or("").to_string();
    Some(Offer { id: id.to_string(), kind: kind_of_title(&n.title, &name), name, tier, device, body })
}

/// "From Ada's MacBook, a contact (device bbbb bbbb bbbb bbbb). Deny to decline it."
fn from_line(detail: &str) -> Option<(String, Option<Tier>, String)> {
    let l = detail.lines().rev().find_map(|l| l.strip_prefix("From "))?;
    let l = l.strip_suffix(". Deny to decline it.").unwrap_or(l);
    let (head, dev) = l.rsplit_once(" (device ")?;
    let (name, who) = head.rsplit_once(", ")?;
    let tier = match who {
        "one of your Macs" => Some(Tier::MyDevice),
        "a contact" => Some(Tier::Contact),
        _ => None,
    };
    Some((name.to_string(), tier, dev.trim_end_matches(')').to_string()))
}

/// "X sent a snippet…", "X sent a link: …", "X sent a file: …", "X sent a terminal tab: …", "X sent from Midna: …",
/// else "X: …" (a message).
fn kind_of_title(title: &str, name: &str) -> ShareKind {
    let rest = title.strip_prefix(name).filter(|_| !name.is_empty());
    let has = |s: &str| match rest {
        Some(r) => r.starts_with(s),
        None => title.contains(s),
    };
    if has(" sent from Midna: ") {
        ShareKind::Object
    } else if has(" sent a file: ") || has(" sent a folder: ") {
        ShareKind::File
    } else if has(" sent a terminal tab: ") {
        ShareKind::Tab
    } else if has(" sent a snippet") {
        ShareKind::Snippet
    } else if has(" sent a link: ") {
        ShareKind::Link
    } else {
        ShareKind::Message
    }
}

/// An offer's payload: the inbox's copy when there is one (whole), else what the card carries.
fn offer_payload(m: &MainWindow, o: &Offer) -> SharePayload {
    if let Some(it) = m.share.items.iter().find(|i| i.id == o.id) {
        return it.payload.clone();
    }
    match o.kind {
        ShareKind::Message => SharePayload::Message { text: o.body.clone(), reply_to: None },
        ShareKind::Snippet => SharePayload::Snippet { text: o.body.clone(), title: None },
        ShareKind::Link => SharePayload::Link { url: o.body.clone(), title: None },
        ShareKind::File => {
            let (name, folder) = file_of_body(&o.body);
            SharePayload::File { name, folder, files: Vec::new(), dirs: Vec::new() }
        }
        ShareKind::Tab => SharePayload::Tab { title: "Terminal tab".into(), project: None, cwd: String::new(), shell: None, scrollback: o.body.clone() },
        ShareKind::Object => SharePayload::Snippet { text: o.body.clone(), title: None },
    }
}

/// A file card's body, "report.pdf, 1.2 MB." or "photos, 3 files, 4.0 MB.": the name, and is it a folder.
fn file_of_body(body: &str) -> (String, bool) {
    let head = body.trim().trim_end_matches('.');
    let head = head.rsplit_once(", ").map_or(head, |(h, _)| h);
    let counted = |c: &str| c.split_once(' ').is_some_and(|(n, w)| n.parse::<usize>().is_ok() && (w == "file" || w == "files"));
    match head.rsplit_once(", ") {
        Some((name, c)) if counted(c) => (name.to_string(), true),
        _ => (head.to_string(), false),
    }
}

/// The sender's full fingerprint, from the inbox, else the short one on the card.
pub fn offer_peer(m: &MainWindow, o: &Offer) -> String {
    m.share.items.iter().find(|i| i.id == o.id).map(|i| i.peer.clone()).unwrap_or_else(|| o.device.clone())
}

/// A share card's middle: who sent it and what it is.
pub fn panel(m: &MainWindow, t: &Theme, o: &Offer) -> Div {
    let p = offer_payload(m, o);
    let note = match o.kind {
        ShareKind::Message => format!("Mark read clears it. Reply sends a message back to {}.", o.name),
        ShareKind::Snippet => {
            "Nothing runs: pasting never presses Enter, and several lines only go into a terminal at a prompt that takes them whole (bracketed paste). Keep it to paste later from Notifications › Nearby.".into()
        }
        ShareKind::Link => "Open opens it in your browser. Pin adds it to a terminal's links instead.".into(),
        ShareKind::File => "Accept saves it into your Nearby downloads folder, marked as downloaded. Nothing is opened or run.".into(),
        ShareKind::Tab => "Open tab opens a new terminal in your checkout of its project (else your home folder) with their scrollback shown read-only above a fresh shell. Nothing they ran runs here, and no process or environment comes with it.".into(),
        ShareKind::Object => "Approve makes only the changes marked +. Rules, triggers and human-only settings come in only through you, triggers arrive turned off, and queued prompts wait in your inbox until you pick a terminal.".into(),
    };
    let transfer = m.share.items.iter().find(|i| i.id == o.id).and_then(|i| i.transfer.clone());
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .text_size(px(13.))
        .child(sender(t, &o.name, o.tier, &o.device))
        .child(if o.kind == ShareKind::Object { review_box(t, &o.body) } else { payload_box(t, &p) })
        .children(transfer.map(|x| progress(t, &x)))
        .child(div().text_color(t.dim).child(note))
}

/// What accepting Midna items would change here, as midnad worked it out: `+` lines apply, `·` lines are skipped.
fn review_box(t: &Theme, review: &str) -> Div {
    const SHOWN: usize = 14;
    let lines: Vec<&str> = review.lines().collect();
    let more = lines.len().saturating_sub(SHOWN);
    div()
        .flex()
        .flex_col()
        .px(px(14.))
        .py(px(10.))
        .rounded(px(9.))
        .border_1()
        .border_color(t.line)
        .bg(t.bg)
        .font_family(t.mono_font.clone())
        .text_size(px(12.))
        .children(lines.iter().take(SHOWN).map(|l| {
            let skipped = l.trim_start().starts_with('·');
            div().text_color(if skipped { t.dim } else { t.fg }).child(if l.is_empty() { " ".to_string() } else { l.to_string() })
        }))
        .when(more > 0, |d| d.child(div().text_color(t.dim).child(format!("… {more} more line{}", if more == 1 { "" } else { "s" }))))
}

/// The sender line: name, tier and device.
fn sender(t: &Theme, name: &str, tier: Option<Tier>, device: &str) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(px(8.))
        .child(div().font_weight(FontWeight::BOLD).child(name.to_string()))
        .children(tier.map(|x| tier_chip(t, x)))
        .when(!device.is_empty(), |d| d.child(div().text_color(t.dim).font_family(t.mono_font.clone()).text_size(px(11.5)).child(format!("device {device}"))))
}

pub fn tier_chip(t: &Theme, tier: Tier) -> Div {
    let c = if tier == Tier::MyDevice { t.accent } else { t.dim };
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(20.))
        .px(px(8.))
        .rounded(px(10.))
        .bg(c.opacity(0.14))
        .text_color(c)
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(tier_label(tier))
}

/// A share's text or link, as it would arrive.
fn payload_box(t: &Theme, p: &SharePayload) -> Div {
    let frame = || div().flex().flex_col().px(px(14.)).py(px(10.)).rounded(px(9.)).border_1().border_color(t.line).bg(t.bg);
    match p {
        SharePayload::Message { text, .. } => frame().text_size(px(14.)).line_height(px(21.)).children(text.lines().map(|l| div().child(if l.is_empty() { " ".to_string() } else { l.to_string() }))),
        SharePayload::Snippet { text, title } => {
            const SHOWN: usize = 14;
            let lines: Vec<&str> = text.lines().collect();
            let more = lines.len().saturating_sub(SHOWN);
            let head = format!("{} · {} line{} · {}", title.as_deref().unwrap_or("Snippet"), lines.len().max(1), if lines.len() == 1 { "" } else { "s" }, size(text.len() as u64));
            div()
                .flex()
                .flex_col()
                .rounded(px(9.))
                .border_1()
                .border_color(t.line)
                .bg(t.term)
                .overflow_hidden()
                .child(div().px(px(12.)).py(px(6.)).border_b_1().border_color(t.line).text_size(px(11.5)).text_color(t.dim).child(head))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .px(px(12.))
                        .py(px(8.))
                        .font_family(t.mono_font.clone())
                        .text_size(px(12.))
                        .children(lines.iter().take(SHOWN).map(|l| div().whitespace_nowrap().overflow_hidden().child(if l.is_empty() { " ".to_string() } else { l.to_string() })))
                        .when(more > 0, |d| d.child(div().text_color(t.dim).child(format!("… {more} more line{}", if more == 1 { "" } else { "s" })))),
                )
        }
        SharePayload::Link { url, title } => frame()
            .gap(px(4.))
            .children(title.clone().map(|x| div().font_weight(FontWeight::BOLD).child(x)))
            .child(div().font_family(t.mono_font.clone()).text_size(px(12.5)).text_color(t.accent).child(url.clone())),
        SharePayload::File { name, folder, files, .. } => {
            const SHOWN: usize = 6;
            let total = files.iter().fold(0u64, |n, f| n.saturating_add(f.size));
            let count = if *folder { format!(" · {} file{}", files.len(), if files.len() == 1 { "" } else { "s" }) } else { String::new() };
            let head = format!("{}{} · {}{count}", nb::safe_name(name), if *folder { "/" } else { "" }, nb::size_label(total));
            let more = files.len().saturating_sub(SHOWN);
            frame()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::BOLD).child(head))
                .when(*folder, |d| {
                    d.children(files.iter().take(SHOWN).map(|f| {
                        div().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.dim).child(format!("{} · {}", nb::safe_name(&f.path), nb::size_label(f.size)))
                    }))
                    .when(more > 0, |d| d.child(div().text_color(t.dim).child(format!("… {more} more"))))
                })
        }
        SharePayload::Tab { title, project, cwd, scrollback, .. } => {
            const SHOWN: usize = 14;
            let lines: Vec<&str> = scrollback.lines().collect();
            let place = match project {
                Some(p) if p.path.is_empty() => p.remote.clone(),
                Some(p) => format!("{}/{}", p.remote, p.path),
                None => cwd.clone(),
            };
            let head = if place.is_empty() { title.clone() } else { format!("{title} · {place}") };
            frame()
                .gap(px(4.))
                .child(div().font_weight(FontWeight::BOLD).child(head))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .font_family(t.mono_font.clone())
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .children(lines[lines.len().saturating_sub(SHOWN)..].iter().map(|l| div().whitespace_nowrap().overflow_hidden().child(if l.is_empty() { " ".to_string() } else { l.to_string() }))),
                )
        }
        SharePayload::Object { object } => frame().child(div().font_weight(FontWeight::BOLD).child(object.label())),
    }
}

/// A file share's transfer: a bar and where it is.
fn progress(t: &Theme, x: &Transfer) -> Div {
    let frac = if x.total == 0 { 1. } else { (x.done as f64 / x.total as f64).clamp(0., 1.) };
    let (label, color) = match x.state {
        TransferState::Done => (x.saved_to.as_ref().map(|p| format!("Saved to {p}")).unwrap_or_else(|| "Done".into()), t.ok),
        TransferState::Failed => (format!("Failed: {}", x.error.as_deref().unwrap_or("the transfer stopped")), t.err),
        _ => (format!("{} of {} · file {} of {}", nb::size_label(x.done), nb::size_label(x.total), (x.files_done + 1).min(x.files.max(1)), x.files.max(1)), t.accent),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(div().h(px(6.)).w_full().rounded(px(3.)).bg(t.line).child(div().h_full().w(relative(frac as f32)).rounded(px(3.)).bg(color)))
        .child(div().text_size(px(11.5)).text_color(if x.state == TransferState::Failed { t.err } else { t.dim }).child(label))
}

// ------------------------------------------------------------------ RPCs

/// A backend call with the result handed back as is, errors included.
fn call(m: &MainWindow, method: &'static str, params: Value, cx: &mut Context<MainWindow>, then: impl FnOnce(&mut MainWindow, anyhow::Result<Value>, &mut Window, &mut Context<MainWindow>) + 'static) {
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let r = cx.background_executor().spawn(async move { backend.call(method, params) }).await;
        let _ = this.update_in(cx, |m, w, cx| then(m, r, w, cx));
    })
    .detach();
}

/// Refresh the inbox (quietly: a daemon without Nearby has none).
pub fn fetch(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    call(m, "inbox.list", json!({ "limit": 200 }), cx, |m, r, _, cx| {
        if let Some(l) = r.ok().and_then(|v| serde_json::from_value::<InboxListResult>(v).ok()) {
            m.share.items = l.items;
            m.share.waiting = l.waiting;
            cx.notify();
        }
    });
}

/// `share.*`: refresh the inbox; say so when the other Mac answered one of yours.
pub fn on_event(m: &mut MainWindow, e: &Event, cx: &mut Context<MainWindow>) {
    if e.kind == midna_proto::kinds::SHARE_PROGRESS {
        if let Ok(it) = serde_json::from_value::<ShareItem>(e.data.clone()) {
            match m.share.items.iter_mut().find(|i| i.id == it.id) {
                Some(slot) => *slot = it,
                None => fetch(m, cx),
            }
            cx.notify();
        }
        return;
    }
    fetch(m, cx);
    if e.kind == midna_proto::kinds::SHARE_ACCEPTED || e.kind == midna_proto::kinds::SHARE_DECLINED {
        let it = serde_json::from_value::<ShareItem>(e.data.clone()).ok().filter(|i| i.direction == ShareDirection::Outgoing);
        if let Some(msg) = it.as_ref().and_then(answered) {
            m.toast(msg, cx);
        }
    }
}

/// `share.accept` an incoming share: paste a snippet into `session`, pin a link to it (`pin`),
/// or plain accept (`session` None). `done` runs once it worked.
pub fn accept(m: &mut MainWindow, id: String, session: Option<String>, pin: bool, cx: &mut Context<MainWindow>, done: impl FnOnce(&mut MainWindow, &mut Context<MainWindow>) + 'static) {
    let mut params = json!({ "id": id });
    if let Some(s) = &session {
        params["session"] = json!(s);
    }
    if pin {
        params["action"] = json!("pin");
    }
    let name = session.as_deref().and_then(|s| m.sessions.iter().find(|x| x.id == s)).map(|s| s.name.clone()).unwrap_or_default();
    call(m, "share.accept", params, cx, move |m, r, _, cx| match r {
        Ok(_) => {
            match (session.is_some(), pin) {
                (true, false) => m.toast(format!("Pasted into {name}. Nothing ran: press Enter there to run it."), cx),
                (true, true) => m.toast(format!("Pinned to {name}'s links."), cx),
                _ => {}
            }
            done(m, cx);
            m.request_refresh(refresh::NEEDS, cx);
            fetch(m, cx);
        }
        Err(e) => m.toast(format!("{}: {e:#}", if pin { "Couldn't pin it" } else if session.is_some() { "Couldn't paste it" } else { "Couldn't accept it" }), cx),
    });
}

fn decline(m: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    call(m, "share.decline", json!({ "id": id }), cx, |m, r, _, cx| match r {
        Ok(_) => {
            m.request_refresh(refresh::NEEDS, cx);
            fetch(m, cx);
        }
        Err(e) => m.toast(format!("Couldn't decline it: {e:#}"), cx),
    });
}

/// Terminals a snippet can be pasted into (or a link pinned to): the selected one first.
pub fn targets(m: &MainWindow) -> Vec<(String, String)> {
    let mut v: Vec<_> = m.sessions.iter().filter(|s| s.status.state != StatusState::Exited).collect();
    if let Some(i) = m.selected.as_ref().and_then(|id| v.iter().position(|s| &s.id == id)) {
        let s = v.remove(i);
        v.insert(0, s);
    }
    v.into_iter().take(12).map(|s| (s.id.clone(), s.name.clone())).collect()
}

/// The paste-into / pin-to menu: one row per terminal; `pick` gets its id.
pub fn chooser(m: &MainWindow, t: &Theme, prefix: &str, pick: impl Fn(&mut MainWindow, String, &mut Context<MainWindow>) + Clone + 'static, cx: &Context<MainWindow>) -> Div {
    let mut b = super::sidebar::menu_box(t).min_w(px(260.)).max_h(px(360.));
    let targets = targets(m);
    if targets.is_empty() {
        return b.child(div().px(px(10.)).py(px(8.)).text_color(t.dim).child("No open terminals."));
    }
    for (i, (sid, name)) in targets.into_iter().enumerate() {
        let hint = if i == 0 && m.selected.as_deref() == Some(sid.as_str()) { "this terminal" } else { "" };
        let pick = pick.clone();
        b = b.child(super::sidebar::menu_item(t, &format!("{prefix}-{i}"), &name, hint, cx.listener(move |m, _, _, cx| pick(m, sid.clone(), cx))));
    }
    b
}

// ------------------------------------------------------------------ the picker

/// Open the picker to share `text` (a selection, the clipboard).
pub fn open(m: &mut MainWindow, text: String, window: &mut Window, cx: &mut Context<MainWindow>) {
    let kind = kinds(&text)[0];
    start(m, kind, text, None, None, None, window, cx);
}

/// Open the picker in the home window: for a terminal's selection, which may be in a pop-out.
pub fn open_in_home(text: String, cx: &mut App) {
    crate::windows::on_home(cx, move |m, window, cx| open(m, text, window, cx));
}

/// Open the picker to write a message; a reply answers `reply_to` (a share id) from `peer`,
/// and marks the card `resolves` read once it's sent.
pub fn open_message(m: &mut MainWindow, reply_to: Option<String>, peer: Option<String>, resolves: Option<String>, window: &mut Window, cx: &mut Context<MainWindow>) {
    start(m, ShareKind::Message, String::new(), reply_to, peer, resolves, window, cx);
}

#[allow(clippy::too_many_arguments)]
fn start(m: &mut MainWindow, kind: ShareKind, text: String, reply_to: Option<String>, prefer: Option<String>, resolves: Option<String>, window: &mut Window, cx: &mut Context<MainWindow>) {
    let message = LineInput::new(cx, false, if reply_to.is_some() { "Write a reply" } else { "Write a message" });
    m.share.picker = Some(Picker { kind, text, message, reply_to, resolves, prefer, peers: None, unpaired: 0, sel: 0, busy: false, error: None, focus: cx.focus_handle() });
    set_kind(m, kind, window, cx);
    call(m, "peers.list", json!({}), cx, |m, r, _, cx| {
        let Some(p) = m.share.picker.as_mut() else { return };
        match r.and_then(|v| Ok(serde_json::from_value::<PeersListResult>(v)?)) {
            Ok(l) => {
                let paired = order(l.paired);
                p.unpaired = l.peers.iter().filter(|x| x.tier.is_none() && x.claims_name_of.is_none()).count();
                p.sel = first_choice(&paired, p.prefer.as_deref());
                p.peers = Some(paired);
            }
            Err(e) => {
                p.peers = Some(Vec::new());
                p.error = Some(format!("Couldn't list your paired Macs: {e:#}"));
            }
        }
        cx.notify();
    });
}

fn set_kind(m: &mut MainWindow, kind: ShareKind, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(p) = m.share.picker.as_mut() else { return };
    p.kind = kind;
    p.error = None;
    if kind == ShareKind::Message {
        // A short line of the selection starts the message.
        if p.message.is_empty(cx) && !p.text.contains('\n') && p.text.len() <= nb::MAX_MESSAGE_TEXT {
            let text = p.text.trim().to_string();
            p.message.set_text(&text, cx);
        }
        p.message.focus.focus(window, cx);
    } else {
        p.focus.focus(window, cx);
    }
    cx.notify();
}

/// Close the picker, back to what it opened over.
fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.share.picker = None;
    if m.overlay == Overlay::NeedsYou {
        m.stack.focus.focus(window, cx);
    } else if m.overlay == Overlay::None && m.screen != Screen::Terminal {
        m.overlay_focus.focus(window, cx);
    } else {
        m.focus_terminal(window, cx);
    }
    cx.notify();
}

/// ↑↓: the next paired Mac on the network.
fn step(m: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    let Some(p) = m.share.picker.as_mut() else { return };
    let Some(peers) = p.peers.as_ref() else { return };
    let near: Vec<usize> = (0..peers.len()).filter(|&i| peers[i].nearby).collect();
    if near.is_empty() {
        return;
    }
    let at = near.iter().position(|&i| i == p.sel).unwrap_or(0) as isize;
    p.sel = near[(at + by).clamp(0, near.len() as isize - 1) as usize];
    cx.notify();
}

fn send(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(p) = m.share.picker.as_mut() else { return };
    if p.busy {
        return;
    }
    let Some(peer) = p.peers.as_ref().and_then(|v| v.get(p.sel)).cloned() else {
        p.error = Some("Pick a Mac to send it to.".into());
        return cx.notify();
    };
    if !peer.nearby {
        p.error = Some(format!("{} isn't on the network right now.", peer.name));
        return cx.notify();
    }
    let body = payload(p.kind, &p.text, &p.message.text(cx), p.reply_to.clone());
    if let Err(e) = body.check() {
        p.error = Some(format!("Can't send that: {e}."));
        return cx.notify();
    }
    p.busy = true;
    p.error = None;
    let what = noun(body.kind());
    let resolves = p.resolves.clone();
    let params = serde_json::to_value(send_params(&peer.fingerprint, body)).unwrap_or_default();
    call(m, "share.send", params, cx, move |m, r, w, cx| match r {
        Ok(_) => {
            close(m, w, cx);
            if let Some(n) = resolves.filter(|n| m.needs.iter().any(|x| &x.id == n)) {
                m.resolve(n, Resolution::Approve { scope: ApprovalScope::Once }, cx);
            }
            m.toast(format!("Sent your {what} to {}. It waits there until they accept it.", peer.name), cx);
            fetch(m, cx);
        }
        Err(e) => {
            if let Some(p) = m.share.picker.as_mut() {
                p.busy = false;
                p.error = Some(format!("{e:#}"));
            }
            cx.notify();
        }
    });
    cx.notify();
}

pub fn render(m: &MainWindow, t: &Theme, window: &Window, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let p = m.share.picker.as_ref()?;
    let replying = p.reply_to.is_some();
    let title = if replying { "Reply" } else { "Share with a nearby Mac" };

    let kinds = if replying { vec![ShareKind::Message] } else { kinds(&p.text) };
    let switch = (kinds.len() > 1).then(|| {
        div().flex().gap(px(6.)).children(kinds.into_iter().map(|k| {
            let on = p.kind == k;
            div()
                .id(SharedString::from(format!("share-kind-{}", noun(k))))
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(10.))
                .rounded(px(12.))
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |d| d.bg(t.accent_soft).text_color(t.accent))
                .text_size(px(12.))
                .cursor_pointer()
                .hover(|s| s.bg(t.accent_soft))
                .on_click(cx.listener(move |m, _, w, cx| set_kind(m, k, w, cx)))
                .child(kind_label(k))
        }))
    });

    let preview: AnyElement = match p.kind {
        ShareKind::Message => p.message.render(t, "share-message", window).into_any_element(),
        k => payload_box(t, &payload(k, &p.text, "", None)).into_any_element(),
    };

    let list: AnyElement = match &p.peers {
        None => div().py(px(10.)).text_color(t.dim).child("Looking for your paired Macs…").into_any_element(),
        Some(peers) if peers.is_empty() => div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .py(px(6.))
            .text_color(t.dim)
            .child("You haven't paired this Mac with another Midna yet. Pair one in Settings › Nearby, then share with it here.")
            .into_any_element(),
        Some(peers) => {
            let mut col = div().id("share-peers").flex().flex_col().gap(px(2.)).max_h(px(240.)).overflow_y_scroll();
            for (i, peer) in peers.iter().enumerate() {
                let on = i == p.sel && peer.nearby;
                let near = peer.nearby;
                col = col.child(
                    div()
                        .id(SharedString::from(format!("share-peer-{i}")))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(42.))
                        .px(px(10.))
                        .rounded(px(8.))
                        .border_1()
                        .border_color(if on { t.accent } else { t.raised })
                        .when(on, |d| d.bg(t.accent_soft))
                        .when(!near, |d| d.opacity(0.55))
                        .when(near, |d| {
                            d.cursor_pointer().hover(|s| s.bg(t.accent_soft)).on_click(cx.listener(move |m, ev: &ClickEvent, _, cx| {
                                if let Some(p) = m.share.picker.as_mut() {
                                    p.sel = i;
                                }
                                // a double click sends
                                if ev.click_count() >= 2 {
                                    send(m, cx);
                                }
                                cx.notify();
                            }))
                        })
                        .child(div().flex_none().size(px(8.)).rounded_full().bg(if near { t.ok } else { t.dim }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().font_weight(FontWeight::BOLD).truncate().child(peer.name.clone()))
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(t.dim)
                                        .truncate()
                                        .child(if near { format!("device {}", nb::short_fingerprint(&peer.fingerprint)) } else { "not on the network".to_string() }),
                                ),
                        )
                        .child(tier_chip(t, peer.tier)),
                );
            }
            col.into_any_element()
        }
    };

    let unpaired = (p.unpaired > 0).then(|| {
        let n = p.unpaired;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .text_color(t.dim)
            .child(format!("{n} other Midna{} nearby {} paired with this Mac.", if n == 1 { "" } else { "s" }, if n == 1 { "isn't" } else { "aren't" }))
            .child(
                div()
                    .id("share-pair")
                    .text_color(t.accent)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(|m, _, w, cx| {
                        close(m, w, cx);
                        crate::ui::settings::open(m.backend.clone(), cx);
                    }))
                    .child("Pair in Settings"),
            )
    });

    let chosen = p.peers.as_ref().and_then(|v| v.get(p.sel)).filter(|x| x.nearby);
    let send_label = match (p.busy, chosen) {
        (true, _) => "Sending…".to_string(),
        (false, Some(x)) => format!("Send to {}  ↩", x.name),
        (false, None) => "Send  ↩".to_string(),
    };
    let paired_none = p.peers.as_ref().is_some_and(|v| v.is_empty());

    let card = div()
        .id("share-card")
        .w(px(540.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .p(px(20.))
        .rounded(px(12.))
        .bg(t.raised)
        .border_1()
        .border_color(t.line)
        .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(div().flex().items_center().gap(px(10.)).child(div().flex_1().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title)).children(switch))
        .child(preview)
        .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child("SEND TO"))
        .child(list)
        .children(unpaired)
        .children(p.error.clone().map(|e| div().text_color(t.err).child(e)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().flex_1().text_size(px(11.5)).text_color(t.dim).child("↑↓ choose · ↩ send · esc cancel"))
                .child(btn(t, "share-cancel", "Cancel").on_click(cx.listener(|m, _, w, cx| close(m, w, cx))))
                .when(paired_none, |d| {
                    d.child(btn_primary(t, "share-settings", "Open Settings").on_click(cx.listener(|m, _, w, cx| {
                        close(m, w, cx);
                        crate::ui::settings::open(m.backend.clone(), cx);
                    })))
                })
                .when(!paired_none, |d| d.child(btn_primary(t, "share-send", send_label).on_click(cx.listener(|m, _, _, cx| send(m, cx))))),
        );
    Some(
        div()
            .id("share-sheet")
            .track_focus(&p.focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_mouse_down(MouseButton::Left, cx.listener(|m, _, w, cx| close(m, w, cx)))
            .on_key_down(cx.listener(|m, ev: &KeyDownEvent, w, cx| {
                let ks = &ev.keystroke;
                if ks.modifiers.platform || ks.modifiers.control {
                    return;
                }
                match ks.key.as_str() {
                    "escape" => close(m, w, cx),
                    "enter" => send(m, cx),
                    "up" => step(m, -1, cx),
                    "down" => step(m, 1, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .child(card)
            .into_any_element(),
    )
}

// ------------------------------------------------------------------ Notifications › Nearby

fn state_badge(t: &Theme, it: &ShareItem) -> (&'static str, Hsla) {
    match (it.direction, it.state) {
        (ShareDirection::Incoming, ShareState::Offered) => ("Waiting", t.need),
        (ShareDirection::Outgoing, ShareState::Offered) => ("Sent", t.dim),
        (_, ShareState::Accepted) if it.payload.kind() == ShareKind::Message => ("Read", t.ok),
        (_, ShareState::Accepted) => ("Accepted", t.ok),
        (_, ShareState::Declined) => ("Declined", t.err),
    }
}

/// The first line of what it carries.
fn gist(it: &ShareItem) -> String {
    let s = match &it.payload {
        SharePayload::Message { text, .. } => text.clone(),
        SharePayload::Snippet { text, title } => title.clone().unwrap_or_else(|| text.clone()),
        SharePayload::Link { url, title } => title.clone().unwrap_or_else(|| url.clone()),
        SharePayload::File { name, .. } => nb::safe_name(name),
        SharePayload::Tab { title, .. } => title.clone(),
        SharePayload::Object { object } => object.label(),
    };
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

fn who(it: &ShareItem) -> String {
    match it.direction {
        ShareDirection::Incoming => format!("From {}", it.peer_name),
        ShareDirection::Outgoing => format!("To {}", it.peer_name),
    }
}

/// The selected share: `sel`, else the newest.
pub fn current(m: &MainWindow) -> Option<&ShareItem> {
    m.share.sel.as_ref().and_then(|id| m.share.items.iter().find(|i| &i.id == id)).or(m.share.items.first())
}

/// ↑↓ in the Nearby tab.
pub fn step_inbox(m: &mut MainWindow, by: isize, cx: &mut Context<MainWindow>) {
    let items = &m.share.items;
    if items.is_empty() {
        return;
    }
    let at = m.share.sel.as_ref().and_then(|id| items.iter().position(|i| &i.id == id)).unwrap_or(0) as isize;
    m.share.sel = Some(items[(at + by).clamp(0, items.len() as isize - 1) as usize].id.clone());
    m.share.chooser = false;
    cx.notify();
}

/// The Nearby tab's left column.
pub fn inbox_rows(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let cur = current(m).map(|i| i.id.clone());
    let mut col = div().id("nt-share-list").flex_none().w(px(480.)).h_full().overflow_y_scroll().border_r_1().border_color(t.line).pb(px(12.));
    let mut last_day = String::new();
    for it in &m.share.items {
        let day = day_label(&it.sent_at);
        if day != last_day {
            col = col.child(div().px(px(16.)).pt(px(12.)).pb(px(4.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(day.to_uppercase()));
            last_day = day;
        }
        let (badge, color) = state_badge(t, it);
        let id = it.id.clone();
        let on = cur.as_deref() == Some(it.id.as_str());
        col = col.child(
            div()
                .id(SharedString::from(format!("nt-share-{}", it.id)))
                .flex()
                .gap(px(10.))
                .px(px(16.))
                .py(px(9.))
                .when(on, |d| d.bg(t.accent_soft))
                .cursor_pointer()
                .hover(|s| s.bg(t.raised))
                .on_click(cx.listener(move |m, _, _, cx| {
                    m.share.sel = Some(id.clone());
                    m.share.chooser = false;
                    cx.notify();
                }))
                .child(div().flex_none().w(px(52.)).text_size(px(12.)).text_color(t.dim).child(clock(&it.sent_at).chars().take(5).collect::<String>()))
                .child(div().flex_none().mt(px(6.)).size(px(8.)).rounded_full().bg(color))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(div().font_weight(FontWeight::BOLD).truncate().child(who(it)))
                                .child(div().flex_none().text_size(px(11.5)).text_color(t.dim).child(kind_label(it.payload.kind())))
                                .child(div().flex_none().px(px(5.)).rounded(px(4.)).bg(color.opacity(0.14)).text_color(color).text_size(px(10.5)).font_weight(FontWeight::BOLD).child(badge)),
                        )
                        .child(div().text_size(px(12.5)).text_color(t.dim).truncate().child(gist(it))),
                ),
        );
    }
    col
}

/// The Nearby tab's right side: one share and what you can still do with it.
pub fn inbox_detail(m: &MainWindow, t: &Theme, it: &ShareItem, cx: &mut Context<MainWindow>) -> Stateful<Div> {
    let (badge, color) = state_badge(t, it);
    let incoming = it.direction == ShareDirection::Incoming;
    let id = it.id.clone();
    let mut actions = div().flex().flex_wrap().items_center().gap(px(8.));
    let open = it.state != ShareState::Declined;
    match (&it.payload, incoming) {
        (SharePayload::Snippet { .. }, true) if open => {
            let menu = m.share.chooser.then(|| {
                let id = id.clone();
                let pick = move |m: &mut MainWindow, sid: String, cx: &mut Context<MainWindow>| {
                    m.share.chooser = false;
                    accept(m, id.clone(), Some(sid), false, cx, |_, _| {});
                };
                div().absolute().left_0().bottom(px(34.)).child(deferred(chooser(m, t, "nt-share-into", pick, cx)).with_priority(3))
            });
            actions = actions.child(
                div().relative().child(btn_primary(t, "nt-share-paste", "Paste into…").on_click(cx.listener(|m, _, _, cx| {
                    m.share.chooser = !m.share.chooser;
                    cx.notify();
                }))).children(menu),
            );
        }
        (SharePayload::Link { url, .. }, true) if open => {
            let (a, url) = (id.clone(), url.clone());
            actions = actions.child(btn_primary(t, "nt-share-open", "Open link").on_click(cx.listener(move |m, _, _, cx| {
                if m.share.items.iter().any(|i| i.id == a && i.state == ShareState::Offered) {
                    accept(m, a.clone(), None, false, cx, |_, _| {});
                } else {
                    crate::notify::open_url(&url);
                }
            })));
            let menu = m.share.chooser.then(|| {
                let id = id.clone();
                let pick = move |m: &mut MainWindow, sid: String, cx: &mut Context<MainWindow>| {
                    m.share.chooser = false;
                    accept(m, id.clone(), Some(sid), true, cx, |_, _| {});
                };
                div().absolute().left_0().bottom(px(34.)).child(deferred(chooser(m, t, "nt-share-pin", pick, cx)).with_priority(3))
            });
            actions = actions.child(
                div().relative().child(btn(t, "nt-share-pin", "Pin to…").on_click(cx.listener(|m, _, _, cx| {
                    m.share.chooser = !m.share.chooser;
                    cx.notify();
                }))).children(menu),
            );
        }
        (SharePayload::Message { .. }, true) => {
            let (r, peer) = (id.clone(), it.peer.clone());
            actions = actions.child(btn_primary(t, "nt-share-reply", "Reply…").on_click(cx.listener(move |m, _, w, cx| open_message(m, Some(r.clone()), Some(peer.clone()), None, w, cx))));
            if it.state == ShareState::Offered {
                let a = id.clone();
                actions = actions.child(btn(t, "nt-share-read", "Mark read").on_click(cx.listener(move |m, _, _, cx| accept(m, a.clone(), None, false, cx, |_, _| {}))));
            }
        }
        _ => {}
    }
    let copy = match &it.payload {
        SharePayload::Message { text, .. } | SharePayload::Snippet { text, .. } => text.clone(),
        SharePayload::Link { url, .. } => url.clone(),
        SharePayload::File { name, .. } => it.transfer.as_ref().and_then(|x| x.saved_to.clone()).unwrap_or_else(|| name.clone()),
        SharePayload::Tab { scrollback, .. } => scrollback.clone(),
        SharePayload::Object { object } => serde_json::to_string_pretty(object).unwrap_or_default(),
    };
    actions = actions.child(btn(t, "nt-share-copy", "Copy").on_click(cx.listener(move |m, _, _, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
        m.toast("Copied.", cx);
    })));
    if incoming && it.state == ShareState::Offered {
        let d = id.clone();
        actions = actions.child(super::screen_kit::btn_danger(t, "nt-share-decline", "Decline", 28.).on_click(cx.listener(move |m, _, _, cx| decline(m, d.clone(), cx))));
    }

    let when = match &it.decided_at {
        Some(d) => format!("{} {} · {} {}", if incoming { "came" } else { "sent" }, day_label(&it.sent_at), badge.to_lowercase(), day_label(d)),
        None => format!("{} {}", if incoming { "came" } else { "sent" }, day_label(&it.sent_at)),
    };
    div()
        .id("nt-share-detail")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(14.))
        .px(px(26.))
        .py(px(22.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(div().size(px(9.)).rounded_full().bg(color))
                .child(div().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(color).child(format!("{} · {}", kind_label(it.payload.kind()).to_uppercase(), badge.to_uppercase())))
                .child(div().text_color(t.dim).child(format!("· {when}"))),
        )
        .child(div().text_size(px(18.)).font_weight(FontWeight::BOLD).child(who(it)))
        .child(sender(t, &it.peer_name, None, &nb::short_fingerprint(&it.peer)))
        .child(payload_box(t, &it.payload))
        .children(it.transfer.as_ref().map(|x| progress(t, x)))
        .children(it.result.clone().map(|r| div().text_color(t.dim).child(format!("On this Mac: {r}."))))
        .child(actions)
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the std one.
    use super::{Offer, answered, file_of_body, first_choice, is_url, kinds, offer_of, order, payload, send_params};
    use crate::model::{ApprovalRequest, NeedsYou, NeedsYouKind, PolicyAction};
    use midna_proto::nearby::{PairedPeer, ShareDirection, ShareItem, ShareKind, SharePayload, ShareState, Tier};

    fn need(title: &str, detail: &str, value: &str) -> NeedsYou {
        NeedsYou {
            kind: NeedsYouKind::Approval,
            title: title.into(),
            detail: detail.into(),
            approval: Some(ApprovalRequest { action: PolicyAction { kind: "cli".into(), value: value.into(), ..Default::default() }, matched_rule: None }),
            ..Default::default()
        }
    }

    fn peer(name: &str, c: char, nearby: bool) -> PairedPeer {
        PairedPeer { fingerprint: c.to_string().repeat(64), name: name.into(), tier: Tier::Contact, paired_at: String::new(), nearby }
    }

    #[test]
    fn a_snippet_offer_is_read() {
        let n = need(
            "Ada's MacBook sent a snippet: deploy (2 lines)",
            "cargo build\ncargo test\n\nApproving keeps it in your inbox; nothing runs. To paste it into a terminal (never followed by Enter): midna inbox accept sh_ab12cd34 --into <terminal>\nFrom Ada's MacBook, a contact (device bbbb bbbb bbbb bbbb). Deny to decline it.",
            "midna inbox accept sh_ab12cd34",
        );
        let want = Offer {
            id: "sh_ab12cd34".into(),
            name: "Ada's MacBook".into(),
            tier: Some(Tier::Contact),
            device: "bbbb bbbb bbbb bbbb".into(),
            kind: ShareKind::Snippet,
            body: "cargo build\ncargo test".into(),
        };
        assert_eq!(offer_of(&n), Some(want));
    }

    #[test]
    fn message_and_link_offers_are_told_apart() {
        let from = "\nFrom Studio, one of your Macs (device dddd dddd dddd dddd). Deny to decline it.";
        let m = need("Studio: lunch? we could, sent a link: no", &format!("lunch?\n\nMessage from Studio. Approving marks it read.{from}"), "midna inbox accept sh_m001");
        let o = offer_of(&m).unwrap();
        assert_eq!((o.kind, o.tier, o.body.as_str()), (ShareKind::Message, Some(Tier::MyDevice), "lunch?"));
        let l = need("Studio sent a link: https://x.dev", &format!("https://x.dev\n\nApproving opens it in your browser.{from}"), "midna inbox accept sh_l001");
        assert_eq!(offer_of(&l).unwrap().kind, ShareKind::Link);
    }

    #[test]
    fn a_name_with_a_comma_keeps_it() {
        let n = need("Sam, Jo's iMac: hi", "hi\n\nMessage.\nFrom Sam, Jo's iMac, a contact (device eeee). Deny to decline it.", "midna inbox accept sh_c0ffee");
        let o = offer_of(&n).unwrap();
        assert_eq!((o.name.as_str(), o.kind), ("Sam, Jo's iMac", ShareKind::Message));
    }

    #[test]
    fn a_file_offer_is_read() {
        let from = "From Studio, one of your Macs (device aaaa aaaa aaaa aaaa). Deny to decline it.";
        let n = need(
            "Studio sent a file: report, final.pdf (1.2 MB)",
            &format!("report, final.pdf, 1.2 MB.\n\nApproving saves it into ~/Downloads/Nearby, marked as downloaded; nothing is opened or run.\n{from}"),
            "midna inbox accept sh_file0001",
        );
        let o = offer_of(&n).unwrap();
        assert_eq!((o.kind, o.name.as_str(), o.tier), (ShareKind::File, "Studio", Some(Tier::MyDevice)));
        assert_eq!(file_of_body(&o.body), ("report, final.pdf".to_string(), false));
        // a title that doesn't start with the sender's name still reads as a file
        let n = need("photos sent a folder: photos (4.0 MB, 3 files)", &format!("photos, 3 files, 4.0 MB.\n\nApproving saves it.\n{from}"), "midna inbox accept sh_file0002");
        let o = offer_of(&n).unwrap();
        assert_eq!(o.kind, ShareKind::File);
        assert_eq!(file_of_body(&o.body), ("photos".to_string(), true));
    }

    #[test]
    fn a_midna_items_offer_carries_its_review() {
        let review = "+ rule: allow cargo test in midna (project)\n· rule: allow ls in widget: no project by that name\nOnly you can accept this: it changes policy, triggers or human-only settings.";
        let n = need(
            "Studio sent from Midna: 2 rules",
            &format!("{review}\n\nApproving makes the changes marked +; nothing else changes.\nFrom Studio, one of your Macs (device aaaa aaaa aaaa aaaa). Deny to decline it."),
            "midna inbox accept sh_obj00001",
        );
        let o = offer_of(&n).unwrap();
        assert_eq!((o.kind, o.name.as_str()), (ShareKind::Object, "Studio"));
        assert_eq!(o.body, review);
    }

    #[test]
    fn other_approvals_are_not_offers() {
        assert_eq!(offer_of(&need("Run git push?", "", "git push")), None);
        assert_eq!(offer_of(&need("Pair with X? Code 1", "", "midna peers confirm pr_1")), None);
        assert_eq!(offer_of(&need("X sent a link", "", "midna inbox accept --all")), None);
        assert_eq!(offer_of(&NeedsYou { title: "X sent a snippet".into(), ..Default::default() }), None);
    }

    #[test]
    fn what_text_can_be_sent_as() {
        assert_eq!(kinds("https://midna.dev/docs "), vec![ShareKind::Link, ShareKind::Snippet, ShareKind::Message]);
        assert_eq!(kinds("ls -la\npwd"), vec![ShareKind::Snippet, ShareKind::Message]);
        assert_eq!(kinds(""), vec![ShareKind::Message]);
        assert!(!is_url("see https://midna.dev"));
        assert!(!is_url("https://"));
        assert!(!is_url("ftp://x"));
    }

    #[test]
    fn the_picker_sends_what_it_shows() {
        let p = send_params("fp", payload(ShareKind::Link, " https://x.dev\n", "", None));
        assert_eq!((p.kind, p.url.as_deref(), p.text), (ShareKind::Link, Some("https://x.dev"), None));
        let p = send_params("fp", payload(ShareKind::Message, "ignored", " hi ", Some("sh_abcd".into())));
        assert_eq!((p.text.as_deref(), p.reply_to.as_deref()), (Some("hi"), Some("sh_abcd")));
        assert!(payload(ShareKind::Message, "", "  ", None).check().is_err());
        assert!(payload(ShareKind::Snippet, "a\nb", "", None).check().is_ok());
    }

    #[test]
    fn nearby_macs_come_first_and_are_chosen_first() {
        let v = order(vec![peer("zed", 'z', true), peer("Away", 'a', false), peer("Bee", 'b', true)]);
        assert_eq!(v.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Bee", "zed", "Away"]);
        assert_eq!(first_choice(&v, None), 0);
        assert_eq!(first_choice(&v, Some("zzzz zzzz zzzz zzzz")), 1);
        // not on the network: the first one that is
        assert_eq!(first_choice(&v, Some(&"a".repeat(64))), 0);
    }

    #[test]
    fn answers_to_your_shares() {
        let it = |kind: SharePayload, state| ShareItem {
            id: "sh_abcd".into(),
            direction: ShareDirection::Outgoing,
            peer: "f".into(),
            peer_name: "Studio".into(),
            from: "me".into(),
            size: 2,
            payload: kind,
            state,
            sent_at: String::new(),
            decided_at: None,
            result: None,
            needs_you_id: None,
            transfer: None,
        };
        let msg = SharePayload::Message { text: "hi".into(), reply_to: None };
        let snip = SharePayload::Snippet { text: "ls".into(), title: None };
        assert_eq!(answered(&it(msg.clone(), ShareState::Accepted)).as_deref(), Some("Studio read your message."));
        assert_eq!(answered(&it(snip, ShareState::Declined)).as_deref(), Some("Studio declined your snippet."));
        assert_eq!(answered(&it(msg, ShareState::Offered)), None);
    }
}
