//! Approval banner at the bottom of the terminal pane when the selected session has a midna
//! approval waiting (not the agent's own prompt, which the terminal already shows). Every
//! option calls `needs_you.resolve`.
use super::border_w;
use super::sidebar::menu_box;
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::model::*;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<impl IntoElement + use<>> {
    let need = m.approval_for_selected()?.clone();
    let session_name = m.selected_session().map(|s| s.name.clone()).unwrap_or_default();
    let approve_key = m.key_label("keys.approve");
    let deny_key = m.key_label("keys.deny");
    let action = need.approval.as_ref().map(|a| a.action.value.clone()).filter(|v| !v.is_empty());
    let sub = match (action, need.detail.is_empty()) {
        (Some(a), false) => format!("{a} · {}", need.detail),
        (Some(a), true) => a,
        (None, _) => need.detail.clone(),
    };
    let menu_open = m.menu == Menu::Approve;
    let id_once = need.id.clone();
    let id_deny = need.id.clone();

    let options: Vec<(&str, String, ApprovalScope)> = vec![
        ("Approve once", approve_key.clone(), ApprovalScope::Once),
        ("Approve for 15 minutes", "this command".into(), ApprovalScope::Minutes { minutes: 15 }),
        ("Approve for 1 hour", "this command".into(), ApprovalScope::Minutes { minutes: 60 }),
        ("Approve for this session", format!("until {session_name} exits"), ApprovalScope::Session),
        ("Always approve", "adds a rule".into(), ApprovalScope::Always),
    ];

    let menu = menu_open.then(|| {
        let mut b = menu_box(t).occlude().min_w(px(260.));
        for (i, (label, hint, scope)) in options.into_iter().enumerate() {
            let nid = need.id.clone();
            b = b.child(super::sidebar::menu_item(
                t,
                &format!("approve-opt-{i}"),
                label,
                &hint,
                cx.listener(move |m, _, _, cx| m.resolve(nid.clone(), Resolution::Approve { scope: scope.clone() }, cx)),
            ));
        }
        div().absolute().right_0().bottom(px(38.)).child(deferred(b).with_priority(1))
    });

    let approve = div()
        .relative()
        .flex()
        .child(
            div()
                .id("approve")
                .h(px(32.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded_l(px(7.))
                .bg(t.accent)
                .text_color(t.accent_fg)
                .font_weight(FontWeight::BOLD)
                .cursor_pointer()
                .hover(|s| s.opacity(0.92))
                .on_click(cx.listener(move |m, _, _, cx| m.resolve(id_once.clone(), Resolution::Approve { scope: ApprovalScope::Once }, cx)))
                .child(format!("Approve {approve_key}")),
        )
        .child(
            div()
                .id("approve-more")
                .w(px(30.))
                .h(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_r(px(7.))
                .bg(t.accent)
                .border_l_1()
                .border_color(hsla(0., 0., 0., 0.25))
                .cursor_pointer()
                .hover(|s| s.opacity(0.92))
                .on_click(cx.listener(|m, _, _, cx| {
                    m.menu = if m.menu == Menu::Approve { Menu::None } else { Menu::Approve };
                    cx.notify();
                }))
                .child(Icon::Chevron.el(12., t.accent_fg)),
        )
        .children(menu);

    let deny = div()
        .id("deny")
        .h(px(32.))
        .px(px(12.))
        .flex()
        .items_center()
        .rounded(px(7.))
        .border_1()
        .border_color(t.line)
        .cursor_pointer()
        .hover(|s| s.bg(t.raised))
        .on_click(cx.listener(move |m, _, _, cx| m.resolve(id_deny.clone(), Resolution::Deny, cx)))
        .child(format!("Deny {deny_key}"));

    Some(
        border_w(div(), 1.5)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .mx(px(16.))
            .mb(px(16.))
            .px(px(14.))
            .py(px(12.))
            .border_color(t.need)
            .rounded(px(10.))
            .bg(t.panel)
            .shadow(vec![BoxShadow { color: t.need_soft, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(4.), inset: false }])
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .flex_1()
                    .min_w(px(200.))
                    .child(div().font_weight(FontWeight::BOLD).child(need.title.clone()))
                    .child(div().font_family(t.mono_font.clone()).text_size(px(12.)).text_color(t.dim).truncate().child(sub)),
            )
            .child(approve)
            .child(deny),
    )
}
