//! Settings ▸ Nearby: who can find this Mac (with Everyone's countdown), the Midnas on this
//! network, and the devices it's paired with (their tier, and Forget). Pairing happens in a
//! sheet over the window: Pair connects, both Macs show the same six digits, and each person
//! confirms (as a contact or as one of their own Macs) or cancels. A request from another Mac
//! opens the sheet when this section is showing; it's also a needs-you card (ui/nearby.rs).
//! Under each of my devices, the kinds of share it may send without asking (all off at first).
//! Until macOS lets Midna use the local network, the status and the sheet say so calmly: wait
//! for the prompt, or (denied) a button to System Settings › Privacy & Security › Local Network.
use super::*;
use crate::ui::screen_kit::{btn, btn_primary, now_unix};
use midna_proto::nearby::{self as nb, LocalNetwork, Pairing, PairingRole, PairingState, PeersListResult, ShareKind, Tier, Visibility};

/// The `@` rows this section adds to `LAYOUT`.
pub(super) const ROWS: [&str; 3] = ["@nearby_me", "@nearby_peers", "@nearby_paired"];

#[derive(Default)]
pub(super) struct State {
    /// `peers.list`, once it answered.
    list: Option<PeersListResult>,
    /// Why `peers.list` failed.
    error: Option<String>,
    sheet: Option<Sheet>,
    /// The paired device whose Forget was clicked once (the second click forgets it).
    armed_forget: Option<String>,
}

enum Sheet {
    /// `peers.pair` is connecting and agreeing on the code.
    Connecting { fingerprint: String, name: String },
    /// A pairing, kept current from `peers.list` and the pairing events (`pairing` stays once
    /// the daemon drops an ended one). `tier`: how to keep the device when this side confirms.
    Pairing { pairing: Pairing, tier: Tier, busy: bool },
    /// `peers.pair` failed before there was a pairing.
    Failed { name: String, error: String },
}

/// A Nearby row's control (`Control::Nearby`).
pub(super) enum Ctl {
    /// `nearby.visibility`; picking Everyone again restarts its 10 minutes.
    Visibility { current: String, left: Option<i64> },
    /// A Midna on the network: Pair, or Show code while a pairing with it is open.
    Pair { fingerprint: String, name: String, open: Option<String> },
    /// A paired device: its tier and Forget.
    Paired { fingerprint: String, tier: Tier },
    /// One of my devices: the kinds of share accepted from it without asking.
    AutoAccept { fingerprint: String, on: Vec<ShareKind> },
}

impl Ctl {
    /// What a search finds in it besides the row's name.
    pub(super) fn fields(&self) -> Vec<(&'static str, String)> {
        match self {
            Ctl::Visibility { .. } => VISIBILITY.iter().map(|(_, l)| ("Option", l.to_string())).collect(),
            Ctl::Pair { .. } => vec![("Button", "Pair".into())],
            Ctl::Paired { .. } => vec![("Option", "Contact".into()), ("Option", "My device".into()), ("Button", "Forget".into())],
            Ctl::AutoAccept { .. } => ShareKind::ALL.iter().map(|k| ("Option", kind_label(*k).to_string())).collect(),
        }
    }

    /// It goes under the row's name, like chips.
    pub(super) fn wide(&self) -> bool {
        matches!(self, Ctl::AutoAccept { .. })
    }
}

/// A kind of share as the auto-accept chips name it.
fn kind_label(k: ShareKind) -> &'static str {
    match k {
        ShareKind::Message => "Messages",
        ShareKind::Snippet => "Snippets",
        ShareKind::Link => "Links",
        ShareKind::File => "Files",
        ShareKind::Tab => "Tabs",
        ShareKind::Object => "Summaries & themes",
        ShareKind::Agent => "Agent sessions",
    }
}

/// What accepting a kind without asking does, for its chip's tooltip.
fn kind_tip(k: ShareKind) -> &'static str {
    match k {
        ShareKind::Message => "Marked read as it arrives",
        ShareKind::Snippet => "Kept in your inbox; never pasted by itself",
        ShareKind::Link => "Opened in your browser as it arrives",
        ShareKind::File => "Saved into your downloads folder; nothing is opened or run",
        ShareKind::Tab => "Opened as a new tab with a fresh shell; nothing they ran runs here",
        ShareKind::Object => "Session summaries and themes are added. Rules, triggers, settings and queued prompts always ask",
        ShareKind::Agent => "Agent sessions always ask",
    }
}

const VISIBILITY: [(&str, &str); 3] = [("off", "Off"), ("paired", "Paired only"), ("everyone", "Everyone for 10 min")];

/// "9:41": minutes and seconds left.
pub(super) fn mmss(secs: i64) -> String {
    let s = secs.max(0);
    format!("{}:{:02}", s / 60, s % 60)
}

/// The first 12 hex digits of a device id: enough for the CLI to find it.
fn id12(fp: &str) -> &str {
    &fp[..fp.len().min(12)]
}

fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::Contact => "Contact",
        Tier::MyDevice => "My device",
    }
}

/// The tier as the CLI's `--as` takes it.
fn tier_cli(t: Tier) -> &'static str {
    match t {
        Tier::Contact => "contact",
        Tier::MyDevice => "my-device",
    }
}

/// The status and the sheet while macOS is likely asking about the local network.
const WAITING_WHY: &str = "macOS is asking whether Midna may use the local network. Choose Allow in that prompt; Nearby carries on by itself.";
const DENIED_WHY: &str = "Midna needs it to find and reach other Macs. Turn it on for Midna in System Settings › Privacy & Security › Local Network.";

fn seconds_until(ts: &str) -> Option<i64> {
    midna_proto::time::parse_rfc3339(ts).map(|s| s - now_unix())
}

impl SettingsWindow {
    /// `peers.list`, then the rows and the sheet follow it.
    pub(super) fn load_peers(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("peers.list", json!({})) }).await;
            let _ = this.update(cx, |s, cx| {
                match r.and_then(|v| Ok(serde_json::from_value::<PeersListResult>(v)?)) {
                    Ok(list) => {
                        if let Some(Sheet::Pairing { pairing, .. }) = &mut s.nearby.sheet
                            && let Some(p) = list.pairings.iter().find(|p| p.id == pairing.id)
                        {
                            *pairing = p.clone();
                        }
                        s.nearby.list = Some(list);
                        s.nearby.error = None;
                    }
                    Err(e) => s.nearby.error = Some(format!("{e:#}")),
                }
                if crate::dev::var("MIDNA_SETTINGS_PAIRING").is_ok() && s.nearby.sheet.is_none() {
                    // dev (screenshots): the sheet for the first pairing
                    if let Some(p) = s.nearby.list.as_ref().and_then(|l| l.pairings.first()) {
                        s.nearby.sheet = Some(Sheet::Pairing { pairing: p.clone(), tier: Tier::Contact, busy: false });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A `peers.*` event: a pairing that started or ended updates the sheet (an incoming one
    /// opens it while this section is showing), then everything reloads.
    pub(super) fn peers_event(&mut self, e: &Event, cx: &mut Context<Self>) {
        let pairing = || serde_json::from_value::<Pairing>(e.data.clone()).ok();
        match e.kind.as_str() {
            midna_proto::kinds::PEERS_PAIRING | midna_proto::kinds::PEERS_PAIRING_ENDED => {
                if let Some(p) = pairing() {
                    self.show_pairing(p, e.kind == midna_proto::kinds::PEERS_PAIRING);
                }
            }
            midna_proto::kinds::PEERS_FORGOTTEN => self.nearby.armed_forget = None,
            _ => {}
        }
        self.load_peers(cx);
        cx.notify();
    }

    /// Put a pairing in the sheet when it's the one the sheet is about: the pairing `peers.pair`
    /// is connecting for, or the one it shows. `started`: an incoming one may open the sheet.
    fn show_pairing(&mut self, p: Pairing, started: bool) {
        match &mut self.nearby.sheet {
            Some(Sheet::Connecting { fingerprint, .. }) if *fingerprint == p.fingerprint => {
                self.nearby.sheet = Some(Sheet::Pairing { pairing: p, tier: Tier::Contact, busy: false });
            }
            Some(Sheet::Pairing { pairing, .. }) if pairing.id == p.id => *pairing = p,
            None if started && p.role == PairingRole::Incoming && self.view == View::Section(Sec::Nearby) => {
                self.nearby.sheet = Some(Sheet::Pairing { pairing: p, tier: Tier::Contact, busy: false });
            }
            _ => {}
        }
    }

    /// Pair: connect and agree on a code (`peers.pair` returns once both screens show it).
    fn pair(&mut self, fingerprint: String, name: String, cx: &mut Context<Self>) {
        self.nearby.sheet = Some(Sheet::Connecting { fingerprint: fingerprint.clone(), name: name.clone() });
        let backend = self.backend.clone();
        let cmd = format!("midna peers pair {}", id12(&fingerprint));
        let fp = fingerprint.clone();
        cx.spawn(async move |this, cx| {
            let r = cx.background_executor().spawn(async move { backend.call("peers.pair", json!({ "fingerprint": fp })) }).await;
            let _ = this.update(cx, |s, cx| {
                match r.and_then(|v| Ok(serde_json::from_value::<Pairing>(v)?)) {
                    Ok(p) => {
                        let id = p.id.clone();
                        s.show_pairing(p, false);
                        if s.nearby.sheet.is_none() {
                            // cancelled while connecting: end it on both Macs
                            s.decline(&id, cx);
                        }
                    }
                    Err(e) => {
                        let error = s.call_error(&e);
                        if matches!(&s.nearby.sheet, Some(Sheet::Connecting { fingerprint: f, .. }) if *f == fingerprint) {
                            s.nearby.sheet = Some(Sheet::Failed { name, error: error.trim_start_matches("✗ ").to_string() });
                        }
                        s.last = Some(Last { cmd, ok: false, result: error, who: "you, from this window".into(), at: Instant::now() });
                    }
                }
                s.load_peers(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Open the sheet on a pairing that's in progress (a row's Show code).
    fn reopen(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(p) = self.nearby.list.as_ref().and_then(|l| l.pairings.iter().find(|p| p.id == id)) else { return };
        self.nearby.sheet = Some(Sheet::Pairing { pairing: p.clone(), tier: p.tier.unwrap_or_default(), busy: false });
        cx.notify();
    }

    /// The sheet's Confirm: this side's code matches; keep the device as the picked tier.
    fn confirm_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Pairing { pairing, tier, busy }) = &mut self.nearby.sheet else { return };
        if *busy || pairing.confirmed || pairing.state != PairingState::Comparing {
            return;
        }
        *busy = true;
        let (id, tier) = (pairing.id.clone(), *tier);
        let cmd = format!("midna peers confirm {id} --as {}", tier_cli(tier));
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let params = json!({ "pairing": id, "accept": true, "as": tier.as_str() });
            let r = cx.background_executor().spawn(async move { backend.call("peers.confirm", params) }).await;
            let _ = this.update(cx, |s, cx| {
                if let Some(Sheet::Pairing { busy, .. }) = &mut s.nearby.sheet {
                    *busy = false;
                }
                let ok = r.is_ok();
                let result = match r.and_then(|v| Ok(serde_json::from_value::<Pairing>(v)?)) {
                    Ok(p) => {
                        s.show_pairing(p, false);
                        "✓ confirmed".into()
                    }
                    Err(e) => s.call_error(&e),
                };
                s.last = Some(Last { cmd, ok, result, who: "you, from this window".into(), at: Instant::now() });
                s.load_peers(cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Cancel: the pairing ends on both Macs (`peers.confirm accept:false`).
    fn decline(&mut self, id: &str, cx: &mut Context<Self>) {
        self.kind_call("peers.confirm", json!({ "pairing": id, "accept": false }), format!("midna peers confirm {id} --no"), cx);
    }

    /// The sheet's Cancel (or Close once there's nothing to cancel).
    fn cancel_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(Sheet::Pairing { pairing, .. }) = &self.nearby.sheet
            && !pairing.state.is_over()
        {
            let id = pairing.id.clone();
            self.decline(&id, cx);
        }
        self.nearby.sheet = None;
        cx.notify();
    }

    /// Close the sheet; a pairing in it goes on (its needs-you card and Show code stay).
    fn hide_sheet(&mut self, cx: &mut Context<Self>) {
        self.nearby.sheet = None;
        cx.notify();
    }

    /// ↩ and esc while the sheet is open. True when the sheet took the key.
    pub(super) fn sheet_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let Some(sheet) = &self.nearby.sheet else { return false };
        let comparing = matches!(sheet, Sheet::Pairing { pairing, .. } if pairing.state == PairingState::Comparing && !pairing.confirmed);
        match key {
            "escape" => self.hide_sheet(cx),
            "enter" if comparing => self.confirm_pairing(cx),
            "enter" if !matches!(sheet, Sheet::Connecting { .. }) && !matches!(sheet, Sheet::Pairing { pairing, .. } if !pairing.state.is_over()) => self.hide_sheet(cx),
            _ => return false,
        }
        true
    }

    /// A paired device's tier (`peers.set_tier`).
    fn set_tier(&mut self, fingerprint: String, tier: Tier, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let cmd = format!("midna peers tier {} --as {}", id12(&fingerprint), tier_cli(tier));
        cx.spawn(async move |this, cx| {
            let params = json!({ "fingerprint": fingerprint, "as": tier.as_str() });
            let r = cx.background_executor().spawn(async move { backend.call("peers.set_tier", params) }).await;
            let _ = this.update(cx, |s, cx| {
                let result = match &r {
                    Ok(_) => "✓ applied".into(),
                    Err(e) if format!("{e:#}").contains("unknown method") => "✗ This midnad can't change a tier yet. Forget the device and pair again to change it.".into(),
                    Err(e) => s.call_error(e),
                };
                s.last = Some(Last { cmd, ok: r.is_ok(), result, who: "you, from this window".into(), at: Instant::now() });
                s.load_peers(cx);
            });
        })
        .detach();
    }

    /// Accept `kind` from one of my devices without asking, or ask again (`peers.set_auto_accept`).
    fn set_auto_accept(&mut self, fingerprint: String, kind: ShareKind, on: bool, cx: &mut Context<Self>) {
        let cmd = format!("midna peers auto-accept {} {} {}", id12(&fingerprint), kind.as_str(), if on { "on" } else { "off" });
        self.kind_call("peers.set_auto_accept", json!({ "fingerprint": fingerprint, "kind": kind, "on": on }), cmd, cx);
    }

    /// Forget: armed by the first click, done by the second.
    fn forget(&mut self, fingerprint: String, cx: &mut Context<Self>) {
        if self.nearby.armed_forget.as_deref() != Some(fingerprint.as_str()) {
            self.nearby.armed_forget = Some(fingerprint);
            cx.notify();
            return;
        }
        self.nearby.armed_forget = None;
        let cmd = format!("midna peers forget {}", id12(&fingerprint));
        self.kind_call("peers.forget", json!({ "fingerprint": fingerprint }), cmd, cx);
    }

    /// Seconds Everyone has left, while visibility is Everyone.
    fn everyone_left(&self) -> Option<i64> {
        let me = &self.nearby.list.as_ref()?.me;
        (me.visibility == Visibility::Everyone).then_some(())?;
        me.everyone_until.as_deref().and_then(seconds_until).map(|s| s.max(0))
    }

    /// Something on screen counts down (Everyone's time, or a pairing's): redraw each second.
    pub(super) fn nearby_ticking(&self) -> bool {
        let pairing = matches!(&self.nearby.sheet, Some(Sheet::Pairing { pairing, .. }) if !pairing.state.is_over());
        pairing || (self.view == View::Section(Sec::Nearby) && self.everyone_left().is_some())
    }

    /// `nearby.visibility` and `nearby.name`, as this section shows them.
    pub(super) fn nearby_key_row(&self, t: &Theme, key: &str, r: &mut RowSpec) {
        match key {
            "nearby.visibility" => {
                let current = value_text(&self.value(key));
                let left = self.everyone_left();
                if let Some(l) = left {
                    r.note = Some((format!("Anyone on this network can find this Mac for {} more, then only paired devices.", mmss(l)), t.need));
                } else if current == "off" {
                    r.note = Some(("Nearby is off: this Mac doesn't show itself or look for other Midnas.".into(), Hsla::default()));
                }
                r.control = Control::Nearby(Ctl::Visibility { current, left });
            }
            "nearby.name" => {
                r.control = Control::Edit { key: key.into(), actions: vec![] };
                if let Some(me) = self.nearby.list.as_ref().map(|l| &l.me)
                    && self.value(key).as_str().is_none_or(|n| n.trim().is_empty())
                {
                    r.note = Some((format!("Empty: other Midnas see this Mac's computer name, “{}”.", me.name), Hsla::default()));
                }
            }
            _ => {}
        }
    }

    /// What `nearby.name`'s empty field shows: the name others see without one.
    pub(super) fn nearby_name_placeholder(&self) -> &'static str {
        "This Mac's computer name"
    }

    /// The `@nearby_*` rows.
    pub(super) fn nearby_rows(&self, t: &Theme, name: &str) -> Vec<RowSpec> {
        let note = |text: String| Some((text, Hsla::default()));
        let status = |label: &str, dot: Hsla, text: String, n: Option<(String, Hsla)>, cli: &str| RowSpec {
            label: label.into(),
            note: n,
            control: Control::Text { dot: Some(dot), text, color: Hsla::default(), action: None },
            cli: cli.into(),
            who: Who::ReadOnly,
            warn: false,
        };
        let Some(list) = &self.nearby.list else {
            if name != "@nearby_me" {
                return vec![];
            }
            let mut r = match &self.nearby.error {
                Some(e) if e.contains("unknown method") => status("Nearby", t.need, "Not available".into(), Some((self.needs_newer_daemon(), t.need)), "midna peers"),
                Some(e) => status("Nearby", t.err, "Not available".into(), Some((e.clone(), t.err)), "midna peers"),
                None => status("Nearby", t.dim, "Checking…".into(), None, "midna peers"),
            };
            if self.daemon_behind()
                && let Control::Text { action, .. } = &mut r.control
            {
                *action = Some(("Update daemon".into(), Act::Life(crate::lifecycle::Cmd::UpgradeDaemon), true));
            }
            return vec![r];
        };
        let me = &list.me;
        match name {
            "@nearby_me" => {
                let mut action = None;
                let (dot, text, why) = if me.service.is_none() {
                    (t.dim, "Off in this midnad".to_string(), "It was started without Nearby (MIDNA_NEARBY_SERVICE=off).".to_string())
                } else if me.visibility != Visibility::Off && me.local_network == Some(LocalNetwork::Waiting) {
                    (t.need, "Waiting for Local Network access".into(), WAITING_WHY.into())
                } else if me.visibility != Visibility::Off && me.local_network == Some(LocalNetwork::Denied) {
                    action = Some(("Open Settings".into(), Act::Url(nb::LOCAL_NETWORK_PANE.into()), false));
                    (t.need, "Local Network access is off".into(), DENIED_WHY.into())
                } else if let Some(e) = &me.error {
                    (t.err, "Bonjour isn't working".into(), format!("{e}. midnad tries again every few seconds."))
                } else {
                    match me.visibility {
                        Visibility::Off => (t.dim, "Off".into(), "Not showing this Mac or looking for other Midnas.".into()),
                        Visibility::Everyone if me.advertising => (t.ok, format!("Visible to everyone as “{}”", me.name), "Midnas on this network list it and can ask to pair.".into()),
                        Visibility::Paired if me.advertising => (t.ok, "Visible to paired devices".into(), "Only Macs you paired with recognise it. Others don't see it.".into()),
                        Visibility::Paired if list.paired.is_empty() => (t.ok, "Hidden".into(), "No paired devices yet, so nothing is shown. To pair, pick Everyone.".into()),
                        _ => (t.need, "Starting…".into(), "Not on the network yet.".into()),
                    }
                };
                let mut row = status("Status", dot, text, note(format!("{why} Device ID {}.", nb::short_fingerprint(&me.fingerprint))), "midna peers");
                if let Control::Text { action: a, .. } = &mut row.control {
                    *a = action;
                }
                vec![row]
            }
            "@nearby_peers" => {
                let open = |fp: &str| list.pairings.iter().find(|p| p.fingerprint == fp && !p.state.is_over()).map(|p| p.id.clone());
                let mut rows: Vec<RowSpec> = list
                    .peers
                    .iter()
                    .filter(|p| p.tier.is_none())
                    .map(|p| {
                        let seen = crate::ui::screen_kit::ago(Some(&p.seen_since));
                        let n = match &p.claims_name_of {
                            Some(_) => (format!("Not your paired “{}”: it has the same name but a different key. Pair only if you know whose it is.", p.name), t.need),
                            None if p.protocol != nb::PROTOCOL => ("Runs a different version of midna, so it can't pair with this one.".into(), t.dim),
                            None => (format!("Seen {seen} · device {}", nb::short_fingerprint(&p.fingerprint)), Hsla::default()),
                        };
                        RowSpec {
                            label: p.name.clone(),
                            note: Some(n),
                            control: Control::Nearby(Ctl::Pair { fingerprint: p.fingerprint.clone(), name: p.name.clone(), open: open(&p.fingerprint) }),
                            cli: format!("midna peers pair {}", id12(&p.fingerprint)),
                            who: Who::Agents,
                            warn: false,
                        }
                    })
                    .collect();
                // a pairing with a Mac that isn't listed (it asked from a hidden one)
                for p in list.pairings.iter().filter(|p| !p.state.is_over() && !list.peers.iter().any(|x| x.fingerprint == p.fingerprint)) {
                    let code = p.code.as_deref().map(nb::spaced_code).unwrap_or_default();
                    rows.push(RowSpec {
                        label: p.name.clone(),
                        note: Some((format!("Asked to pair · code {code}"), t.need)),
                        control: Control::Nearby(Ctl::Pair { fingerprint: p.fingerprint.clone(), name: p.name.clone(), open: Some(p.id.clone()) }),
                        cli: format!("midna peers confirm {}", p.id),
                        who: Who::Human,
                        warn: false,
                    });
                }
                if rows.is_empty() {
                    let why = match me.visibility {
                        Visibility::Off => "Nearby is off. Pick Paired only or Everyone above to look for other Midnas.",
                        _ => "A Midna shows here while it's visible to everyone. To pair, ask its owner to pick Everyone in Settings › Nearby.",
                    };
                    rows.push(status("No other Midnas", t.dim, "None seen".into(), note(why.into()), "midna peers"));
                }
                rows
            }
            "@nearby_paired" => {
                let mut rows: Vec<RowSpec> = vec![];
                for p in &list.paired {
                    let when = crate::ui::screen_kit::day_label(&p.paired_at);
                    let here = if p.nearby { "On this network" } else { "Not on this network" };
                    rows.push(RowSpec {
                        label: p.name.clone(),
                        note: note(format!("{here} · paired {when} · device {}", nb::short_fingerprint(&p.fingerprint))),
                        control: Control::Nearby(Ctl::Paired { fingerprint: p.fingerprint.clone(), tier: p.tier }),
                        cli: format!("midna peers forget {}", id12(&p.fingerprint)),
                        who: Who::Human,
                        warn: false,
                    });
                    if p.tier == Tier::MyDevice {
                        rows.push(RowSpec {
                            label: format!("Auto-accept from {}", p.name),
                            note: note(auto_accept_note(&p.auto_accept)),
                            control: Control::Nearby(Ctl::AutoAccept { fingerprint: p.fingerprint.clone(), on: p.auto_accept.clone() }),
                            cli: format!("midna peers auto-accept {} <kinds|all> on|off", id12(&p.fingerprint)),
                            who: Who::Human,
                            warn: false,
                        });
                    }
                }
                if rows.is_empty() {
                    rows.push(status("No paired devices", t.dim, "None yet".into(), note("Pair with a Midna on this network to send things to it.".into()), "midna peers"));
                }
                rows
            }
            _ => vec![],
        }
    }

    /// Toggle chips like the settings' own lists: one per kind of share, on when it's accepted
    /// without asking.
    fn nearby_auto_chips(&self, t: &Theme, id: usize, fingerprint: String, on: Vec<ShareKind>, cx: &mut Context<Self>) -> Div {
        let mut row = div().flex().flex_wrap().gap(px(6.)).w_full();
        for (n, kind) in ShareKind::ALL.into_iter().enumerate() {
            let is_on = on.contains(&kind);
            let fp = fingerprint.clone();
            let tip = format!("{}. Click to {}", kind_tip(kind), if is_on { "ask first again" } else { "accept without asking" });
            row = row.child(
                div()
                    .id(SharedString::from(format!("nearby-auto-{id}-{n}")))
                    .h(px(26.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(7.))
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .border_1()
                    .cursor_pointer()
                    .map(|c| {
                        if is_on {
                            c.bg(t.accent).border_color(t.accent).text_color(t.accent_fg).font_weight(FontWeight::BOLD)
                        } else {
                            c.bg(t.raised).border_color(t.line).text_color(t.dim).hover(|s| s.text_color(t.fg).border_color(t.dim))
                        }
                    })
                    .tooltip(crate::ui::header::tip(tip))
                    .on_click(cx.listener(move |s, _, _, cx| s.set_auto_accept(fp.clone(), kind, !is_on, cx)))
                    .child(kind_label(kind)),
            );
        }
        row
    }

    /// A segmented choice like the settings' own (the picked one raised).
    fn nearby_seg(&self, t: &Theme, id: String, options: Vec<(String, bool, Box<dyn Fn(&mut Self, &mut Context<Self>)>)>, cx: &mut Context<Self>) -> Div {
        let mut seg = div().flex().p(px(2.)).gap(px(1.)).rounded(px(7.)).border_1().border_color(t.line).bg(t.bg);
        for (i, (label, on, click)) in options.into_iter().enumerate() {
            seg = seg.child(
                div()
                    .id(SharedString::from(format!("{id}-{i}")))
                    .h(px(24.))
                    .px(px(9.))
                    .flex()
                    .items_center()
                    .rounded(px(5.))
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .cursor_pointer()
                    .when(on, |d| {
                        d.bg(t.raised).text_color(t.fg).font_weight(FontWeight::BOLD).shadow(vec![BoxShadow {
                            color: hsla(0., 0., 0., 0.2),
                            offset: point(px(0.), px(1.)),
                            blur_radius: px(2.),
                            spread_radius: px(0.),
                            inset: false,
                        }])
                    })
                    .when(!on, |d| d.text_color(t.dim).hover(|s| s.text_color(t.fg)))
                    .on_click(cx.listener(move |s, _, _, cx| click(s, cx)))
                    .child(label),
            );
        }
        seg
    }

    pub(super) fn nearby_control(&self, t: &Theme, ctl: Ctl, id: usize, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        match ctl {
            Ctl::Visibility { current, left } => {
                let options = VISIBILITY
                    .iter()
                    .map(|&(v, label)| {
                        let on = v == current;
                        let label = match left {
                            Some(l) if v == "everyone" => format!("Everyone · {}", mmss(l)),
                            _ => label.to_string(),
                        };
                        // Everyone again restarts its 10 minutes
                        let click: Box<dyn Fn(&mut Self, &mut Context<Self>)> = Box::new(move |s, cx| {
                            if !on || v == "everyone" {
                                s.set("nearby.visibility", json!(v), cx)
                            }
                        });
                        (label, on, click)
                    })
                    .collect();
                self.nearby_seg(t, "nearby-vis".into(), options, cx).into_any_element()
            }
            Ctl::Pair { fingerprint, name, open } => {
                let label = if open.is_some() { "Show code" } else { "Pair…" };
                btn(t, SharedString::from(format!("nearby-pair-{id}")), label)
                    .bg(t.raised)
                    .on_click(cx.listener(move |s, _, window, cx| {
                        s.focus.focus(window, cx);
                        match &open {
                            Some(pid) => s.reopen(pid, cx),
                            None => s.pair(fingerprint.clone(), name.clone(), cx),
                        }
                    }))
                    .into_any_element()
            }
            Ctl::Paired { fingerprint, tier } => {
                let armed = self.nearby.armed_forget.as_deref() == Some(fingerprint.as_str());
                let options = [Tier::Contact, Tier::MyDevice]
                    .into_iter()
                    .map(|x| {
                        let fp = fingerprint.clone();
                        let click: Box<dyn Fn(&mut Self, &mut Context<Self>)> = Box::new(move |s, cx| {
                            if x != tier {
                                s.set_tier(fp.clone(), x, cx)
                            }
                        });
                        (tier_label(x).to_string(), x == tier, click)
                    })
                    .collect();
                let fp = fingerprint.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.nearby_seg(t, format!("nearby-tier-{id}"), options, cx))
                    .child(
                        div()
                            .id(SharedString::from(format!("nearby-forget-{id}")))
                            .flex_none()
                            .h(px(26.))
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(7.))
                            .text_size(px(12.))
                            .font_weight(FontWeight::BOLD)
                            .cursor_pointer()
                            .border_1()
                            .border_color(t.err)
                            .bg(if armed { t.err } else { gpui_kit::transparent_black() })
                            .text_color(if armed { gpui_kit::white() } else { t.err })
                            .on_click(cx.listener(move |s, _, _, cx| s.forget(fp.clone(), cx)))
                            .child(if armed { "Forget for good" } else { "Forget…" }),
                    )
                    .into_any_element()
            }
            Ctl::AutoAccept { fingerprint, on } => self.nearby_auto_chips(t, id, fingerprint, on, cx).into_any_element(),
        }
    }

    /// Local Network access as the last `peers.list` said.
    fn local_network(&self) -> Option<LocalNetwork> {
        self.nearby.list.as_ref().and_then(|l| l.me.local_network)
    }

    /// The pairing sheet, over the whole window.
    pub(super) fn pairing_sheet(&self, t: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.nearby.sheet.as_ref()?;
        let access = self.local_network();
        let title = |s: String| div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(s);
        let text = |s: String, c: Hsla| div().text_color(c).child(s);
        let code_box = |code: &str, size: f32, c: Hsla| {
            div()
                .flex()
                .justify_center()
                .py(px(if size > 24. { 14. } else { 8. }))
                .rounded(px(10.))
                .bg(t.bg)
                .border_1()
                .border_color(t.line)
                .child(div().font_family(t.mono_font.clone()).text_size(px(size)).font_weight(FontWeight::BOLD).text_color(c).child(nb::spaced_code(code)))
        };
        let mut card = div().flex().flex_col().gap(px(14.));
        let mut buttons = div().flex().items_center().gap(px(8.)).child(div().flex_1());
        match sheet {
            Sheet::Connecting { name, .. } if access == Some(LocalNetwork::Waiting) => {
                card = card
                    .child(title(format!("Pairing with {name}")))
                    .child(text("Allow Local Network access in the macOS prompt to connect.".into(), t.fg))
                    .child(text("Midna keeps trying for up to a minute, then you can try again.".into(), t.dim));
                buttons = buttons.child(btn(t, "pair-cancel", "Cancel").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
            }
            Sheet::Connecting { name, .. } => {
                card = card.child(title(format!("Pairing with {name}"))).child(text("Connecting and agreeing on a code…".into(), t.dim));
                buttons = buttons.child(btn(t, "pair-cancel", "Cancel").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
            }
            Sheet::Failed { name, .. } if access == Some(LocalNetwork::Denied) => {
                card = card.child(title(format!("Couldn't pair with {name}"))).child(text(DENIED_WHY.into(), t.dim));
                buttons = buttons
                    .child(btn(t, "pair-done", "Done").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))))
                    .child(btn_primary(t, "pair-open-settings", "Open Settings").on_click(|_, _, cx| cx.open_url(nb::LOCAL_NETWORK_PANE)));
            }
            Sheet::Failed { name, error } => {
                card = card.child(title(format!("Couldn't pair with {name}"))).child(text(error.clone(), t.err));
                buttons = buttons.child(btn_primary(t, "pair-done", "Done  ↩").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
            }
            Sheet::Pairing { pairing: p, tier, busy } => {
                let code = p.code.clone().unwrap_or_default();
                let left = seconds_until(&p.expires_at).map(|s| s.max(0));
                match p.state {
                    PairingState::Connecting => {
                        card = card.child(title(format!("Pairing with {}", p.name))).child(text("Connecting and agreeing on a code…".into(), t.dim));
                        buttons = buttons.child(btn(t, "pair-cancel", "Cancel").on_click(cx.listener(|s, _, _, cx| s.cancel_sheet(cx))));
                    }
                    PairingState::Comparing if !p.confirmed => {
                        let asked = match p.role {
                            PairingRole::Outgoing => format!("{} shows a code too. Confirm only if it's this one.", p.name),
                            PairingRole::Incoming => format!("{} asks to pair with this Mac. Confirm only if it shows this code.", p.name),
                        };
                        card = card.child(title(format!("Pair with {}?", p.name))).child(text(asked, t.dim)).child(code_box(&code, 34., t.fg));
                        if let Some(fp) = &p.claims_name_of {
                            card = card.child(text(
                                format!(
                                    "Careful: you already paired with a device called {}, and this isn't it (device {}, not {}).",
                                    p.name,
                                    nb::short_fingerprint(&p.fingerprint),
                                    nb::short_fingerprint(fp)
                                ),
                                t.need,
                            ));
                        }
                        card = card.child(self.tier_picker(t, *tier, cx));
                        if let Some(l) = left {
                            buttons = div().flex().items_center().gap(px(8.)).child(div().text_size(px(12.)).text_color(t.dim).child(format!("{} left to confirm", mmss(l)))).child(div().flex_1());
                        }
                        buttons = buttons
                            .child(btn(t, "pair-cancel", "Cancel").on_click(cx.listener(|s, _, _, cx| s.cancel_sheet(cx))))
                            .child(btn_primary(t, "pair-confirm", if *busy { "Confirming…" } else { "Confirm  ↩" }).on_click(cx.listener(|s, _, _, cx| s.confirm_pairing(cx))));
                    }
                    PairingState::Comparing => {
                        let kept = p.tier.unwrap_or(*tier);
                        let as_what = if kept == Tier::MyDevice { "one of your Macs" } else { "a contact" };
                        card = card
                            .child(title(format!("Waiting for {}…", p.name)))
                            .child(text(format!("Confirmed here, as {as_what}. It pairs once the other person confirms the same code."), t.dim))
                            .child(code_box(&code, 20., t.dim));
                        if let Some(l) = left {
                            buttons = div().flex().items_center().gap(px(8.)).child(div().text_size(px(12.)).text_color(t.dim).child(format!("{} left", mmss(l)))).child(div().flex_1());
                        }
                        buttons = buttons
                            .child(btn(t, "pair-stop", "Stop pairing").text_color(t.err).on_click(cx.listener(|s, _, _, cx| s.cancel_sheet(cx))))
                            .child(btn(t, "pair-hide", "Close").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
                    }
                    PairingState::Paired => {
                        let as_what = if p.tier == Some(Tier::MyDevice) { "one of your Macs" } else { "a contact" };
                        card = card
                            .child(title(format!("Paired with {}", p.name)))
                            .child(text(format!("Kept as {as_what}. Change it any time under Paired devices."), t.dim))
                            .child(div().flex().items_center().gap(px(8.)).text_color(t.ok).child(Icon::Check.el(14., t.ok)).child(format!("Device {}", nb::short_fingerprint(&p.fingerprint))));
                        buttons = buttons.child(btn_primary(t, "pair-done", "Done  ↩").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
                    }
                    PairingState::Declined | PairingState::Failed => {
                        let why = p.error.clone().unwrap_or_else(|| "It ended before both people confirmed.".into());
                        card = card.child(title(format!("Not paired with {}", p.name))).child(text(sentence(&why), t.dim));
                        buttons = buttons.child(btn_primary(t, "pair-done", "Done  ↩").on_click(cx.listener(|s, _, _, cx| s.hide_sheet(cx))));
                    }
                }
            }
        }
        let card = div()
            .id("pair-card")
            .w(px(460.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .p(px(20.))
            .rounded(px(12.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(card)
            .child(buttons);
        Some(
            deferred(
                div()
                    .id("pair-sheet")
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(hsla(0., 0., 0., 0.45))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(card),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// Contact or My device, as two cards with a line each.
    fn tier_picker(&self, t: &Theme, tier: Tier, cx: &mut Context<Self>) -> Div {
        let choice = |x: Tier, what: &'static str| {
            let on = x == tier;
            div()
                .id(SharedString::from(format!("pair-tier-{}", x.as_str())))
                .flex_1()
                .flex_basis(px(0.))
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .p(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |d| d.bg(t.accent_soft))
                .cursor_pointer()
                .hover(|s| s.border_color(if on { t.accent } else { t.dim }))
                .on_click(cx.listener(move |s, _, _, cx| {
                    if let Some(Sheet::Pairing { tier, .. }) = &mut s.nearby.sheet {
                        *tier = x;
                        cx.notify();
                    }
                }))
                .child(div().font_weight(FontWeight::BOLD).child(tier_label(x)))
                .child(div().text_size(px(12.)).text_color(t.dim).child(what))
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(div().text_size(px(12.)).text_color(t.dim).child("Keep it as"))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(choice(Tier::Contact, "Someone else's Midna. What it sends asks you first."))
                    .child(choice(Tier::MyDevice, "One of your own Macs.")),
            )
    }
}

/// "the other device declined (declined)" → "The other device declined (declined)."
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut out: String = c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default();
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

/// What an auto-accept row says under its name.
fn auto_accept_note(on: &[ShareKind]) -> String {
    let always = "Rules, triggers, settings and queued prompts always ask.";
    if on.is_empty() {
        return format!("Off: everything it sends asks you first. Turn on a kind to accept it without asking. {always}");
    }
    let names: Vec<String> = on.iter().map(|k| kind_label(*k).to_lowercase()).collect();
    format!("Accepted without asking: {}. Everything else asks first. {always}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::{ShareKind, auto_accept_note, mmss, sentence};

    #[test]
    fn auto_accept_says_what_still_asks() {
        assert!(auto_accept_note(&[]).starts_with("Off: everything it sends asks you first."));
        let n = auto_accept_note(&[ShareKind::Message, ShareKind::Object]);
        assert!(n.starts_with("Accepted without asking: messages, summaries & themes."), "{n}");
        assert!(n.ends_with("Rules, triggers, settings and queued prompts always ask."), "{n}");
    }

    #[test]
    fn countdown() {
        assert_eq!(mmss(600), "10:00");
        assert_eq!(mmss(61), "1:01");
        assert_eq!(mmss(-3), "0:00");
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert_eq!(sentence("time ran out before both people confirmed"), "Time ran out before both people confirmed.");
        assert_eq!(sentence("declined on this Mac."), "Declined on this Mac.");
        assert_eq!(sentence(""), ".");
    }
}
