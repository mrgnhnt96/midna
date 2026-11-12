//! A Nearby pairing request in the needs-you inbox. midnad raises an approval for each side of
//! a pairing ("Pair with X? Code 123 456", action `midna peers confirm pr_…`); its card shows
//! the code large and answers it three ways: pair as a contact (approve), pair as one of your
//! own Macs (`peers.confirm … as my_device`), or Codes differ (deny, which ends the pairing).
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

#[cfg(test)]
mod tests {
    use super::{PairingAsk, pairing_of, parse_title};
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
}
