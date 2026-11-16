//! A Nearby pairing request in the needs-you inbox. midnad raises an approval for each side of
//! a pairing ("Pair with X? Code 123 456", action `midna peers confirm pr_…`); its card shows
//! the code large and answers it three ways: pair as a contact (approve), pair as one of your
//! own Macs (`peers.confirm … as my_device`), or Codes differ (deny, which ends the pairing).
//! A box under the code, off until ticked, also lets that Mac drive this one: it counts only
//! for Pair as my Mac (a contact can never drive), which then asks for Touch ID or the password
//! before confirming with `allow_drive`.
use crate::icons::Icon;
use crate::model::NeedsYou;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

/// What a pairing card needs from its needs-you item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingAsk {
    /// The pairing's id, `pr_…`.
    pub id: String,
    /// The other Mac's name.
    pub name: String,
    /// The code, spaced ("123 456"), or empty when the title doesn't carry one.
    pub code: String,
}

/// The pairing a needs-you item asks to confirm, if it's one.
pub fn pairing_of(n: &NeedsYou) -> Option<PairingAsk> {
    let a = n.approval.as_ref()?;
    let id = a.action.value.strip_prefix("midna peers confirm ")?.split_whitespace().next()?;
    if !id.starts_with("pr_") {
        return None;
    }
    let (name, code) = parse_title(&n.title);
    Some(PairingAsk { id: id.to_string(), name, code })
}

/// "Pair with Ada's MacBook? Code 123 456" → ("Ada's MacBook", "123 456").
fn parse_title(title: &str) -> (String, String) {
    let rest = title.strip_prefix("Pair with ").unwrap_or(title);
    match rest.rsplit_once("? Code ") {
        Some((name, code)) => (name.to_string(), code.trim().to_string()),
        None => (rest.trim_end_matches('?').to_string(), String::new()),
    }
}

/// The card's middle: the code, large, and what to check.
pub fn panel(t: &Theme, ask: &PairingAsk, detail: &str) -> Div {
    let clash = detail.lines().find(|l| l.starts_with("Careful:")).map(str::to_string);
    let device = detail.lines().find_map(|l| l.strip_prefix("Device ")).map(|d| d.trim().to_string());
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .text_size(px(13.))
        .child(div().text_color(t.dim).child(format!("Approve only if {} shows the same code.", ask.name)))
        .when(!ask.code.is_empty(), |d| {
            d.child(
                div()
                    .flex()
                    .justify_center()
                    .py(px(16.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.bg)
                    .child(div().font_family(t.mono_font.clone()).text_size(px(38.)).font_weight(FontWeight::BOLD).child(ask.code.clone())),
            )
        })
        .children(clash.map(|c| div().text_color(t.need).child(c)))
        .child(
            div()
                .text_color(t.dim)
                .child(match device {
                    Some(d) => format!("Device {d}. A contact is someone else's Midna: what it sends asks you first. My device is one of your own Macs."),
                    None => "A contact is someone else's Midna: what it sends asks you first. My device is one of your own Macs.".into(),
                }),
        )
}

/// `peers.confirm` for Pair as my Mac; `allow_drive` (the box ticked and Touch ID passed) also
/// lets it drive this Mac. Pair as a contact never sends it.
pub fn confirm_mine_params(pairing: &str, allow_drive: bool) -> serde_json::Value {
    let mut params = serde_json::json!({ "pairing": pairing, "accept": true, "as": "my_device" });
    if allow_drive {
        params["allow_drive"] = true.into();
    }
    params
}

/// The "Also let <name> drive <here>" box (`on`: ticked). The caller makes it clickable.
pub fn drive_box(t: &Theme, id: &'static str, name: &str, here: &str, on: bool) -> Stateful<Div> {
    let mark = div().flex_none().mt(px(2.)).size(px(16.)).rounded(px(4.)).flex().items_center().justify_center();
    let mark = if on { mark.bg(t.accent).child(Icon::Check.el(12., t.accent_fg)) } else { mark.border_1().border_color(t.dim) };
    div()
        .id(id)
        .flex()
        .items_start()
        .gap(px(10.))
        .p(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(t.line)
        .text_size(px(13.))
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .child(mark)
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .min_w_0()
                .child(div().font_weight(FontWeight::BOLD).child(format!("Also let {name} drive {here}")))
                .child(div().text_size(px(12.)).text_color(t.dim).child(format!(
                    "Open, type into and approve {here}'s terminals from {name}. Applies only to “Pair as my Mac”; asks for Touch ID. Change it later in Settings → Nearby."
                ))),
        )
}

#[cfg(test)]
mod tests {
    use super::{PairingAsk, confirm_mine_params, pairing_of, parse_title};
    use crate::model::{ApprovalRequest, NeedsYou, NeedsYouKind, PolicyAction};

    fn need(title: &str, value: &str) -> NeedsYou {
        NeedsYou {
            kind: NeedsYouKind::Approval,
            title: title.into(),
            approval: Some(ApprovalRequest { action: PolicyAction { kind: "cli".into(), value: value.into(), ..Default::default() }, matched_rule: None }),
            ..Default::default()
        }
    }

    #[test]
    fn a_pairing_approval_is_read() {
        let n = need("Pair with Ada's MacBook? Code 123 456", "midna peers confirm pr_7f3a");
        assert_eq!(pairing_of(&n), Some(PairingAsk { id: "pr_7f3a".into(), name: "Ada's MacBook".into(), code: "123 456".into() }));
    }

    #[test]
    fn a_name_with_a_question_mark_keeps_it() {
        let n = need("Pair with Who? Me? Code 000 111", "midna peers confirm pr_1");
        let a = pairing_of(&n).unwrap();
        assert_eq!((a.name.as_str(), a.code.as_str()), ("Who? Me", "000 111"));
    }

    #[test]
    fn other_approvals_are_not_pairings() {
        assert_eq!(pairing_of(&need("Run git push?", "git push")), None);
        assert_eq!(pairing_of(&need("Pair with X? Code 1", "midna peers confirm --no")), None);
        assert_eq!(pairing_of(&NeedsYou { title: "Pair with X? Code 1".into(), ..Default::default() }), None);
    }

    #[test]
    fn a_title_without_a_code() {
        assert_eq!(parse_title("Pair with Studio?"), ("Studio".into(), String::new()));
    }

    #[test]
    fn pair_as_my_mac_asks_to_drive_only_when_ticked() {
        let off = confirm_mine_params("pr_1", false);
        assert_eq!(off, serde_json::json!({ "pairing": "pr_1", "accept": true, "as": "my_device" }));
        let on = confirm_mine_params("pr_1", true);
        assert_eq!(on, serde_json::json!({ "pairing": "pr_1", "accept": true, "as": "my_device", "allow_drive": true }));
    }
}
