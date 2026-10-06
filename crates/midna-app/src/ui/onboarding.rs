//! Onboarding (Onboarding-B, "live checklist"), five steps: background daemon, notifications,
//! first project, webhooks, theme. Each step's "done" is read from the real state, so doing it
//! anywhere else (Settings, ⌘K, an agent) ticks it here too. The steps show on the full-window
//! Twilight Tiles screen (`setup_screen.rs`); every step can be put off with "Later". While the
//! screen is hidden the sidebar shows "Setup N of 5" until setup is finished.
//!
//! Accessibility and agent hooks are deliberately not steps: Accessibility is asked for when
//! Kass first dictates (`ax_prompt.rs`), hooks live in the status bar (`hooks.rs`).
//!
//! The setup screen walks the steps in order: one already done (a project an agent opened, a
//! daemon from an earlier install) still shows, with its result and Next, rather than being
//! skipped over. Only Next or Later moves past a step.
//!
//! Remembered in `app-state.json` as
//! `onboarding: {"finished": bool, "later": [step ids], "passed": [step ids]}`.
use crate::app::{MainWindow, Screen};
use crate::install::LoginItem;
use crate::notify::Permission;
use crate::theme::Theme;
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
}

/// Persisted part.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub later: Vec<String>,
    /// Steps the human moved past with Next.
    #[serde(default)]
    pub passed: Vec<String>,
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
    /// "Begin" was pressed on the setup screen's intro this launch.
    pub intro_seen: bool,
    /// The setup screen's click ripple: (which play, origin column, origin row).
    pub ripple: Option<(u64, f32, f32)>,
    /// Bumped on every step change: replays the card's entrance.
    pub card_seq: u64,
    /// The `card_seq` the launch opening brought in (no second entrance for it).
    pub card_seq_opened: u64,
    /// The theme step's choice while browsing (None: not touched yet, read from settings).
    /// Browsing only previews; "Finish setup" writes it.
    pub theme_pick: Option<ThemePick>,
}

/// What the theme step has picked: one theme (linked), or a dark and a light one that follow
/// macOS (unlinked).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemePick {
    /// Unlinked: follow macOS (theme = system, theme.dark / theme.light).
    pub sys: bool,
    /// Matching macOS: the dark slot is the one being browsed.
    pub dark_slot: bool,
    pub one: String,
    pub dark: String,
    pub light: String,
}

impl ThemePick {
    /// The theme being browsed.
    pub fn current(&self) -> &str {
        match (self.sys, self.dark_slot) {
            (false, _) => &self.one,
            (true, true) => &self.dark,
            (true, false) => &self.light,
        }
    }

    pub fn set_current(&mut self, id: String) {
        match (self.sys, self.dark_slot) {
            (false, _) => self.one = id,
            (true, true) => self.dark = id,
            (true, false) => self.light = id,
        }
    }
}

/// The theme step's pick: what the human browsed to, else what the settings say now.
pub fn theme_pick(m: &MainWindow, system_dark: bool) -> ThemePick {
    if let Some(p) = &m.onboarding.theme_pick {
        return p.clone();
    }
    use midna_proto::themes::choose;
    let s = |k: &str| m.setting_str(k).unwrap_or_default();
    let (theme, dark, light) = (s("theme"), s("theme.dark"), s("theme.light"));
    // Linked is the default: `system` with the stock pair (the untouched default) shows as one
    // theme; a pair the human chose shows unlinked.
    let stock = choose("system", &dark, &light, true) == midna_proto::themes::DEFAULT_DARK && choose("system", &dark, &light, false) == midna_proto::themes::DEFAULT_LIGHT;
    ThemePick {
        sys: (theme.is_empty() || theme == "system") && !stock,
        dark_slot: system_dark,
        one: choose(&theme, &dark, &light, system_dark),
        dark: choose("system", &dark, &light, true),
        light: choose("system", &dark, &light, false),
    }
}

pub fn load(backend: &std::sync::Arc<dyn crate::backend::Backend>) -> Onboarding {
    let saved: Saved = crate::ui::statusbar::load_state(backend, "onboarding");
    Onboarding { theme_done: saved.finished, saved, ..Default::default() }
}

fn save(m: &MainWindow) {
    crate::ui::statusbar::update_state(&m.backend, "onboarding", serde_json::to_value(&m.onboarding.saved).unwrap_or(Value::Null));
}

pub(crate) fn login() -> LoginItem {
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

pub(crate) fn later(m: &MainWindow, s: Step) -> bool {
    m.onboarding.saved.later.iter().any(|l| l == s.id())
}

pub fn done_count(m: &MainWindow) -> usize {
    STEPS.iter().filter(|s| done(m, **s)).count()
}

fn passed(m: &MainWindow, s: Step) -> bool {
    m.onboarding.saved.passed.iter().any(|l| l == s.id())
}

/// The step the card shows: the picked one, else the first the human hasn't moved past (Next)
/// or put off (Later). Done steps are not skipped: they show their result and Next.
pub(crate) fn current(m: &MainWindow) -> Option<Step> {
    m.onboarding.picked.or_else(|| STEPS.into_iter().find(|s| !passed(m, *s) && !later(m, *s)))
}

pub fn active(m: &MainWindow) -> bool {
    !m.onboarding.saved.finished && m.conn == crate::backend::ConnState::Connected
}

pub(crate) fn finish(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
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

pub(crate) fn pick(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    m.onboarding.picked = Some(s);
    m.onboarding.card_hidden = false;
    m.onboarding.intro_seen = true;
    m.onboarding.card_seq += 1;
    cx.notify();
}

pub(crate) fn put_off(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    if !later(m, s) {
        m.onboarding.saved.later.push(s.id().into());
        save(m);
    }
    m.onboarding.picked = None;
    m.onboarding.card_seq += 1;
    // With nothing left, the setup screen shows "All set" (finish() closes it).
    cx.notify();
}

pub(crate) fn next(m: &mut MainWindow, s: Step, cx: &mut Context<MainWindow>) {
    m.onboarding.saved.later.retain(|l| l != s.id());
    if !passed(m, s) {
        m.onboarding.saved.passed.push(s.id().into());
    }
    m.onboarding.picked = None;
    m.onboarding.card_seq += 1;
    save(m);
    cx.notify();
}

pub(crate) struct View {
    pub(crate) status: Option<(String, bool)>,
    /// Primary action label; None = the step is done (the button says Next / Finish).
    pub(crate) action: Option<&'static str>,
}

pub(crate) fn view(m: &MainWindow, s: Step) -> View {
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
                status,
                action,
            }
        }
        Step::Notifications => {
            let (status, action) = match crate::notify::permission() {
                Permission::Allowed => (Some(("Allowed.".to_string(), true)), None),
                Permission::Dev => (Some(("Dev build: notifications go through osascript.".to_string(), true)), None),
                Permission::Denied => (Some(("Off in System Settings ▸ Notifications ▸ midna.".to_string(), false)), Some("Open Notifications")),
                Permission::NotAsked | Permission::Unknown => (None, Some("Allow notifications")),
            };
            View { status, action }
        }
        Step::Project => {
            let added: Vec<_> = m.projects.iter().filter(|p| p.id != midna_proto::ROOT_PROJECT_ID).collect();
            let status = match added.as_slice() {
                [] => None,
                [p] => Some(format!("Added {} · {}", p.name, p.path)),
                [p, rest @ ..] => Some(format!("Added {} · {} and {} more", p.name, p.path, rest.len())),
            };
            View {
                status: status.map(|t| (t, true)),
                action: (!d).then_some("Choose folder…"),
            }
        }
        Step::Webhooks => View {
            status: d.then(|| (format!("Webhooks arrive via {}.", crate::ui::statusbar::webhooks_label(m.webhooks.get("path").and_then(Value::as_str).unwrap_or(""))), true)),
            action: (!d).then_some("Set up webhooks"),
        },
        Step::Theme => View { status: None, action: None },
    }
}

pub(crate) fn act(m: &mut MainWindow, s: Step, window: &mut Window, cx: &mut Context<MainWindow>) {
    match s {
        Step::Daemon => match login() {
            LoginItem::RequiresApproval => crate::lifecycle::command(crate::lifecycle::Cmd::OpenLoginItems, cx),
            _ => crate::lifecycle::command(crate::lifecycle::Cmd::Register, cx),
        },
        Step::Notifications => {
            if crate::notify::permission() == Permission::Denied {
                cx.open_url(midna_proto::paths::NOTIFICATIONS_PANE);
                poll_permission(cx);
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

/// macOS answers the notification prompt (or the switch in System Settings) asynchronously;
/// re-render for a while so the step ticks as soon as the answer changes.
fn poll_permission(cx: &mut Context<MainWindow>) {
    let start = crate::notify::permission();
    cx.spawn(async move |this, cx| {
        for _ in 0..240 {
            cx.background_executor().timer(std::time::Duration::from_millis(500)).await;
            crate::notify::refresh_permission();
            if this.update(cx, |_, cx| cx.notify()).is_err() || crate::notify::permission() != start {
                break;
            }
        }
    })
    .detach();
}

/// "Finish setup" on the theme step: save what was picked (one theme, or system with a dark
/// and a light one).
pub(crate) fn save_theme(m: &mut MainWindow, p: &ThemePick, cx: &mut Context<MainWindow>) {
    m.onboarding.theme_done = true;
    let sets: Vec<(&str, String)> = if p.sys {
        vec![("theme.dark", p.dark.clone()), ("theme.light", p.light.clone()), ("theme", "system".into())]
    } else {
        vec![("theme", p.one.clone())]
    };
    let backend = m.backend.clone();
    std::thread::spawn(move || {
        for (k, v) in sets {
            let _ = backend.call("settings.set", json!({ "key": k, "value": v }));
        }
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
