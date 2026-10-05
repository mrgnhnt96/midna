//! Onboarding (Onboarding-B, "live checklist"), five steps: background daemon, notifications,
//! first project, webhooks, theme. Each step's "done" is read from the real state, so doing it
//! anywhere else (Settings, ⌘K, an agent) ticks it here too. A card over the terminal pane
//! shows the current step; every step can be put off with "Later". The sidebar shows
//! "Setup N of 5" until setup is finished.
//!
//! Accessibility and agent hooks are deliberately not steps: Accessibility is asked for when
//! Kass first dictates (`ax_prompt.rs`), hooks live in the status bar (`hooks.rs`).
//!
//! Remembered in `app-state.json` as `onboarding: {"finished": bool, "later": [step ids]}`.
use crate::app::{MainWindow, Screen};
use crate::install::LoginItem;
use crate::notify::Permission;
use crate::theme::Theme;
use crate::ui::screen_kit::{btn, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Daemon,
    Notifications,
    Project,
    Webhooks,
    Theme,
}

pub const STEPS: [Step; 5] = [Step::Daemon, Step::Notifications, Step::Project, Step::Webhooks, Step::Theme];

impl Step {
    fn id(self) -> &'static str {
        match self {
            Step::Daemon => "daemon",
            Step::Notifications => "notifications",
            Step::Project => "project",
            Step::Webhooks => "webhooks",
            Step::Theme => "theme",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Step::Daemon => "Background daemon",
            Step::Notifications => "Notifications",
            Step::Project => "First project",
            Step::Webhooks => "Webhooks",
            Step::Theme => "Theme",
        }
    }

    fn optional(self) -> bool {
        matches!(self, Step::Notifications | Step::Webhooks | Step::Theme)
    }

    /// macOS asks a person, not an agent.
    fn human_only(self) -> bool {
        matches!(self, Step::Daemon | Step::Notifications)
    }
}

/// Persisted part.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub later: Vec<String>,
}

/// On `MainWindow.onboarding`.
#[derive(Clone, Debug, Default)]
pub struct Onboarding {
    pub saved: Saved,
    /// The step the human picked in the checklist (else the first open one).
    pub picked: Option<Step>,
    /// The card is tucked away ("Continue setup" in the checklist brings it back).
    pub card_hidden: bool,
    /// The theme step was confirmed.
    pub theme_done: bool,
}

pub fn load(backend: &std::sync::Arc<dyn crate::backend::Backend>) -> Onboarding {
    let saved: Saved = crate::ui::statusbar::load_state(backend, "onboarding");
    Onboarding { theme_done: saved.finished, saved, ..Default::default() }
}

fn save(m: &MainWindow) {
    crate::ui::statusbar::update_state(&m.backend, "onboarding", serde_json::to_value(&m.onboarding.saved).unwrap_or(Value::Null));
}

fn login() -> LoginItem {
    if std::env::var("MIDNA_BACKEND").as_deref() == Ok("fake") {
        // Screenshots: show the step as it looks on a fresh Mac.
        return if crate::dev::var("MIDNA_DEBUG_ONBOARDING").is_ok() { LoginItem::NotRegistered } else { LoginItem::Enabled };
    }
    crate::lifecycle::snapshot().map(|s| s.login).unwrap_or(LoginItem::Checking)
}

pub fn done(m: &MainWindow, s: Step) -> bool {
    match s {
        Step::Daemon => matches!(login(), LoginItem::Enabled | LoginItem::Legacy | LoginItem::Dev),
        Step::Notifications => matches!(crate::notify::permission(), Permission::Allowed | Permission::Dev),
        Step::Project => m.projects.iter().any(|p| p.id != midna_proto::ROOT_PROJECT_ID),
        Step::Webhooks => {
            let p = m.webhooks.get("path").and_then(Value::as_str).map(str::to_string).or_else(|| m.setting_str("webhooks.path")).unwrap_or_default();
            !p.is_empty() && p != "off"
        }
        Step::Theme => m.onboarding.theme_done,
    }
}

fn later(m: &MainWindow, s: Step) -> bool {
    m.onboarding.saved.later.iter().any(|l| l == s.id())
}

pub fn done_count(m: &MainWindow) -> usize {
    STEPS.iter().filter(|s| done(m, **s)).count()
}

/// The step the card shows: the picked one, else the first not done and not put off.
fn current(m: &MainWindow) -> Option<Step> {
    m.onboarding.picked.or_else(|| STEPS.into_iter().find(|s| !done(m, *s) && !later(m, *s)))
}

pub fn active(m: &MainWindow) -> bool {
    !m.onboarding.saved.finished && m.conn == crate::backend::ConnState::Connected
}

fn finish(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.onboarding.saved.finished = true;
    m.onboarding.picked = None;
    save(m);
    m.toast("Setup finished. Settings has everything you skipped.", cx);
    cx.notify();
}

/// Show the checklist again (⌘K "Setup checklist").
pub fn reopen(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    m.onboarding.saved.finished = false;
    m.onboarding.card_hidden = false;
    m.onboarding.picked = None;
    save(m);
    cx.notify();
}

fn pick(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    m.onboarding.picked = Some(s);
    m.onboarding.card_hidden = false;
    cx.notify();
}

fn put_off(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    if !later(m, s) {
        m.onboarding.saved.later.push(s.id().into());
        save(m);
    }
    m.onboarding.picked = None;
    if current(m).is_none() {
        m.onboarding.card_hidden = true;
    }
    cx.notify();
}

fn next(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    m.onboarding.saved.later.retain(|l| l != s.id());
    m.onboarding.picked = None;
    save(m);
    if STEPS.iter().all(|s| done(m, *s)) {
        finish(m, cx);
    }
    cx.notify();
}

struct View {
    title: &'static str,
    body: &'static str,
    status: Option<(String, bool)>,
    /// Primary action label; None = the step is done (the button says Next / Finish).
    action: Option<&'static str>,
    ask: Option<&'static str>,
}

fn view(m: &MainWindow, s: Step) -> View {
    let d = done(m, s);
    match s {
        Step::Daemon => {
            let (status, action) = match login() {
                LoginItem::Enabled | LoginItem::Legacy => (Some(("midnad is running and starts at login.".to_string(), true)), None),
                LoginItem::Dev => (Some(("Dev build: midnad runs without a login item.".to_string(), true)), None),
                LoginItem::RequiresApproval => (Some(("Waiting for you to switch on midna in System Settings ▸ Login Items.".to_string(), false)), Some("Open Login Items")),
                LoginItem::Failed(e) => (Some((format!("Couldn't register: {e}"), false)), Some("Try again")),
                LoginItem::Checking => (None, None),
                LoginItem::NotRegistered => (None, Some("Install daemon")),
            };
            View {
                title: "Keep shells alive when midna quits",
                body: "Install midnad and your terminals survive quitting midna and updating it. macOS then asks you to allow it under Login Items.",
                status,
                action,
                ask: None,
            }
        }
        Step::Notifications => {
            let (status, action) = match crate::notify::permission() {
                Permission::Allowed => (Some(("Allowed.".to_string(), true)), None),
                Permission::Dev => (Some(("Dev build: notifications go through osascript.".to_string(), true)), None),
                Permission::Denied => (Some(("Off in System Settings ▸ Notifications.".to_string(), false)), Some("Open Notifications")),
                Permission::NotAsked | Permission::Unknown => (None, Some("Allow notifications")),
            };
            View { title: "Get a ping when a terminal needs you", body: "Approvals, questions and failures. Clicking one jumps to that terminal.", status, action, ask: None }
        }
        Step::Project => {
            let first = m.projects.iter().find(|p| p.id != midna_proto::ROOT_PROJECT_ID);
            View {
                title: "Add your first project",
                body: "A project is a folder. It gets ⌘1, a shell, and git status in the header. You can also drop a folder on the sidebar.",
                status: first.map(|p| (format!("Added {} · {}", p.name, p.path), true)),
                action: (!d).then_some("Choose folder…"),
                ask: Some("add ~/Development/my-app as a project"),
            }
        }
        Step::Webhooks => View {
            title: "Start agents from GitHub and Bitbucket",
            body: "Webhooks let a pull request or a comment start an agent here. Tailscale Funnel gives midnad a public URL for free, with nothing to host. Triggers come later, by asking.",
            status: d.then(|| (format!("Webhooks arrive via {}.", crate::ui::statusbar::webhooks_label(m.webhooks.get("path").and_then(Value::as_str).unwrap_or(""))), true)),
            action: (!d).then_some("Set up webhooks"),
            ask: Some("start Claude on every PR opened in my repo"),
        },
        Step::Theme => View { title: "Pick a theme", body: "It applies right away.", status: None, action: None, ask: Some("use the light theme") },
    }
}

fn act(m: &mut MainWindow, s: Step, window: &mut Window, cx: &mut Context<MainWindow>) {
    match s {
        Step::Daemon => match login() {
            LoginItem::RequiresApproval => crate::lifecycle::command(crate::lifecycle::Cmd::OpenLoginItems, cx),
            _ => crate::lifecycle::command(crate::lifecycle::Cmd::Register, cx),
        },
        Step::Notifications => {
            if crate::notify::permission() == Permission::Denied {
                cx.open_url("x-apple.systempreferences:com.apple.preference.notifications");
            } else {
                crate::notify::request_permission();
                poll_permission(cx);
            }
        }
        Step::Project => m.pick_project(cx),
        Step::Webhooks => {
            if m.screen != Screen::Triggers {
                m.set_screen(Screen::Triggers, window, cx);
            }
            crate::ui::triggers::open_delivery_path(m, cx);
        }
        Step::Theme => {}
    }
    cx.notify();
}

/// macOS answers the notification prompt asynchronously; re-render for a while so the step
/// ticks as soon as it does.
fn poll_permission(cx: &mut Context<MainWindow>) {
    cx.spawn(async move |this, cx| {
        for _ in 0..60 {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            crate::notify::refresh_permission();
            if this.update(cx, |_, cx| cx.notify()).is_err() || crate::notify::permission() != Permission::NotAsked {
                break;
            }
        }
    })
    .detach();
}

fn set_theme(m: &mut MainWindow, theme: &'static str, cx: &mut Context<MainWindow>) {
    m.onboarding.theme_done = true;
    let backend = m.backend.clone();
    std::thread::spawn(move || {
        let _ = backend.call("settings.set", json!({ "key": "theme", "value": theme }));
    });
    cx.notify();
}

fn mark(t: &Theme, done: bool, current: bool) -> Div {
    let d = div().size(px(14.)).flex_none().flex().items_center().justify_center().rounded_full().text_size(px(9.));
    if done {
        d.bg(t.ok).text_color(t.bg).child("✓")
    } else if current {
        d.border_2().border_color(t.accent)
    } else {
        d.border_1().border_color(t.dim)
    }
}

/// Sidebar card above Today: "Setup N of 5" and the steps.
pub fn checklist(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !active(m) {
        return None;
    }
    let cur = (!m.onboarding.card_hidden).then(|| current(m)).flatten();
    let n = done_count(m);
    let rows = STEPS.into_iter().map(|s| {
        let d = done(m, s);
        let is_cur = cur == Some(s);
        let tag = if d {
            ""
        } else if later(m, s) && !is_cur {
            "later"
        } else if s.optional() && !is_cur {
            "optional"
        } else {
            ""
        };
        div()
            .id(SharedString::from(format!("setup-{}", s.id())))
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(6.))
            .py(px(3.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(is_cur, |r| r.bg(t.accent_soft))
            .hover(|r| r.bg(t.accent_soft))
            .child(mark(t, d, is_cur))
            .child(div().flex_1().when(d, |x| x.text_color(t.dim).line_through()).when(is_cur, |x| x.font_weight(FontWeight::BOLD)).child(s.name()))
            .when(!tag.is_empty(), |r| r.child(div().text_size(px(11.)).text_color(t.dim).child(tag)))
            .on_click(cx.listener(move |m, _, _, cx| pick(m, s, cx)))
    });
    Some(
        div()
            .id("setup")
            .flex()
            .flex_col()
            .gap(px(4.))
            .px(px(8.))
            .py(px(10.))
            .rounded(px(9.))
            .border_1()
            .border_color(t.line)
            .bg(t.raised)
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(px(6.))
                    .pb(px(2.))
                    .child(div().flex_1().text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child("SETUP"))
                    .child(div().text_size(px(11.)).text_color(t.dim).child(format!("{n} of {}", STEPS.len()))),
            )
            .children(rows)
            .when(m.onboarding.card_hidden || cur.is_none(), |d| {
                d.child(
                    div()
                        .id("setup-continue")
                        .px(px(6.))
                        .pt(px(4.))
                        .flex()
                        .gap(px(12.))
                        .text_size(px(11.5))
                        .child(div().id("setup-resume").text_color(t.accent).cursor_pointer().child(format!("Continue setup · {n}/{}", STEPS.len())).on_click(cx.listener(|m, _, _, cx| {
                            m.onboarding.card_hidden = false;
                            m.onboarding.picked = STEPS.into_iter().find(|s| !done(m, *s));
                            cx.notify();
                        })))
                        .child(div().id("setup-finish").text_color(t.dim).cursor_pointer().child("Finish").on_click(cx.listener(|m, _, _, cx| finish(m, cx)))),
                )
            })
            .into_any_element(),
    )
}

/// The step card over the terminal pane.
pub fn card(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    if !active(m) || m.onboarding.card_hidden || m.screen != Screen::Terminal {
        return None;
    }
    let s = current(m)?;
    let v = view(m, s);
    let d = done(m, s);
    let idx = STEPS.iter().position(|x| *x == s).unwrap_or(0) + 1;
    let last = idx == STEPS.len();
    let theme_now = m.setting_str("theme").unwrap_or_else(|| "system".into());
    let chips = (s == Step::Theme).then(|| {
        div().flex().gap(px(8.)).children(["dark", "light", "system"].into_iter().map(|th| {
            let on = theme_now == th;
            div()
                .id(SharedString::from(format!("setup-theme-{th}")))
                .h(px(28.))
                .px(px(12.))
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(if on { t.accent } else { t.line })
                .when(on, |c| c.bg(t.accent_soft))
                .cursor_pointer()
                .child(match th {
                    "dark" => "Dark",
                    "light" => "Light",
                    _ => "Match macOS",
                })
                .on_click(cx.listener(move |m, _, _, cx| set_theme(m, th, cx)))
        }))
    });
    let primary: Option<(String, bool)> = match (v.action, d || s == Step::Theme) {
        (Some(a), false) => Some((a.to_string(), true)),
        _ if last => Some(("Finish setup".into(), false)),
        _ => Some(("Next".into(), false)),
    };
    let ask_line = v.ask.map(|a| format!("Or press {} and ask: “{a}”", m.key_label("keys.command_bar")));
    Some(
        div()
            .id("setup-card")
            .w(px(420.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(16.))
            .rounded(px(10.))
            .bg(t.raised)
            .border_1()
            .border_color(t.line)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().flex_1().font_weight(FontWeight::BOLD).child(v.title))
                    .child(div().text_size(px(11.)).text_color(t.dim).child(format!("Step {idx} of {}", STEPS.len()))),
            )
            .child(div().text_color(t.dim).child(v.body))
            .children(chips)
            .children(v.status.map(|(text, ok)| div().flex().gap(px(6.)).text_color(if ok { t.ok } else { t.need }).child("●").child(text)))
            .when(s.human_only() && !d, |c| c.child(div().text_size(px(11.5)).text_color(t.dim).child("Only you can do this one: macOS asks a person, not an agent.")))
            .children(ask_line.filter(|_| !d).map(|a| div().text_size(px(11.5)).text_color(t.dim).child(a)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().id("setup-hide").text_size(px(11.5)).text_color(t.dim).cursor_pointer().child("Hide").on_click(cx.listener(|m, _, _, cx| {
                        m.onboarding.card_hidden = true;
                        cx.notify();
                    })))
                    .child(div().flex_1())
                    .when(!d && s != Step::Theme, |r| r.child(btn(t, "setup-later", "Later").on_click(cx.listener(move |m, _, _, cx| put_off(m, s, cx)))))
                    .children(primary.map(|(label, is_action)| {
                        btn_primary(t, "setup-primary", label).on_click(cx.listener(move |m, _, w, cx| {
                            if is_action {
                                act(m, s, w, cx);
                            } else if last {
                                m.onboarding.theme_done = true;
                                finish(m, cx);
                            } else {
                                next(m, s, cx);
                            }
                        }))
                    })),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::Saved;

    #[test]
    fn saved_state_tolerates_missing_fields() {
        let s: Saved = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Saved::default());
        let s: Saved = serde_json::from_str(r#"{"finished":true,"later":["webhooks"]}"#).unwrap();
        assert!(s.finished);
        assert_eq!(s.later, ["webhooks"]);
    }
}
