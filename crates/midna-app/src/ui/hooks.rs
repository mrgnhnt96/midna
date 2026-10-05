//! Agent hooks: the status bar item and the sheet behind it. midna's hooks can live in Claude
//! Code's and Codex's global config (`hooks.install`), so an agent typed into a midna terminal
//! reports status too. Without them, midna adds its hooks to each agent it starts itself.
//! The sheet shows the exact change (`hooks.preview`) before anything is written.
use crate::app::MainWindow;
use crate::theme::Theme;
use crate::ui::header::tip;
use crate::ui::screen_kit::{btn, btn_danger, btn_primary};
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};

/// The open sheet (on `MainWindow.hooks_sheet`).
#[derive(Clone, Default)]
pub struct HooksSheet {
    /// Previewing (and offering) removal instead of install.
    pub uninstall: bool,
    /// `hooks.preview` result, once loaded.
    pub preview: Option<Value>,
    pub busy: bool,
    pub error: Option<String>,
}

/// What the status bar shows, from `hooks.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Summary {
    /// Neither agent is set up on this Mac (or no status yet): no item.
    Hidden,
    NotInstalled,
    Current,
    /// Installed but out of date, or a config file midna can't read.
    NeedsReinstall,
}

/// The agents whose config folder exists, as (name, entry).
fn present(status: &Value) -> Vec<(&'static str, &Value)> {
    ["claude", "codex"].into_iter().map(|k| (k, &status[k])).filter(|(_, h)| h["state"].as_str().is_some_and(|s| s != "unavailable")).collect()
}

pub fn summary(status: &Value) -> Summary {
    let p = present(status);
    let state = |s: &str| p.iter().any(|(_, h)| h["state"] == s);
    if p.is_empty() {
        Summary::Hidden
    } else if state("stale") || state("error") {
        Summary::NeedsReinstall
    } else if p.iter().all(|(_, h)| h["state"] == "current") {
        Summary::Current
    } else {
        Summary::NotInstalled
    }
}

pub fn open(m: &mut MainWindow, uninstall: bool, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.hooks_sheet = Some(HooksSheet { uninstall, ..Default::default() });
    m.overlay_focus.focus(window, cx);
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let r = cx.background_executor().spawn(async move { backend.call("hooks.preview", json!({ "uninstall": uninstall })) }).await;
        let _ = this.update(cx, |m, cx| {
            if let Some(s) = m.hooks_sheet.as_mut().filter(|s| s.uninstall == uninstall) {
                match r {
                    Ok(v) => s.preview = Some(v),
                    Err(e) => s.error = Some(format!("{e:#}")),
                }
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.hooks_sheet = None;
    m.focus_terminal(window, cx);
    cx.notify();
}

fn apply(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(sheet) = m.hooks_sheet.as_mut() else { return };
    if sheet.busy {
        return;
    }
    sheet.busy = true;
    sheet.error = None;
    let uninstall = sheet.uninstall;
    let method = if uninstall { "hooks.uninstall" } else { "hooks.install" };
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let r = cx.background_executor().spawn(async move { backend.call(method, json!({})) }).await;
        let _ = this.update_in(cx, |m, window, cx| match r {
            Ok(v) => {
                m.hooks = v;
                close(m, window, cx);
                m.toast(if uninstall { "Hooks removed. midna adds them to the agents it starts." } else { "Hooks installed. New claude and codex sessions report to midna." }, cx);
            }
            Err(e) => {
                if let Some(s) = m.hooks_sheet.as_mut() {
                    s.busy = false;
                    s.error = Some(format!("{e:#}"));
                }
                cx.notify();
            }
        });
    })
    .detach();
    cx.notify();
}

/// The status bar item: "Install hooks", "● Hooks", or an amber "Reinstall hooks".
pub fn status_item(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let s = summary(&m.hooks);
    if s == Summary::Hidden {
        return None;
    }
    let fg = t.fg;
    let dot = |c: Hsla| div().text_color(c).child("●");
    let base = div().id("hooks").flex().gap(px(4.)).cursor_pointer().hover(move |s| s.text_color(fg)).on_click(cx.listener(move |m, _, w, cx| open(m, false, w, cx)));
    let item = match s {
        Summary::NotInstalled => base.text_color(t.accent).child("Install hooks").tooltip(tip(
            "Agents midna starts already report status. Install midna's hooks so a claude or codex you type into a terminal reports too.",
        )),
        Summary::Current => base.child(dot(t.ok)).child("Hooks").tooltip(tip("midna's hooks are in Claude Code's and Codex's config. They do nothing outside midna terminals.")),
        Summary::NeedsReinstall => {
            let why = present(&m.hooks).iter().filter_map(|(k, h)| h["detail"].as_str().map(|d| format!("{k}: {d}"))).collect::<Vec<_>>().join("\n");
            base.text_color(t.need).child(dot(t.need)).child("Reinstall hooks").tooltip(tip(format!("midna's hooks are out of date, so agents you start by hand may not report.\n{why}")))
        }
        Summary::Hidden => return None,
    };
    Some(item.into_any_element())
}

fn diff_block(t: &Theme, f: &Value) -> Div {
    let path = f["path"].as_str().unwrap_or("").to_string();
    let note = if let Some(e) = f["error"].as_str() {
        Some((e.to_string(), t.err))
    } else if f["unchanged"] == true {
        Some(("No change".to_string(), t.dim))
    } else if f["creates"] == true {
        Some(("New file".to_string(), t.dim))
    } else {
        None
    };
    let lines = f["lines"].as_array().cloned().unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(div().flex().gap(px(8.)).child(div().font_family(t.mono_font.clone()).text_color(t.accent).child(path)).children(note.map(|(n, c)| div().text_color(c).child(n))))
        .when(!lines.is_empty(), |d| {
            d.child(
                div().flex().flex_col().p(px(8.)).rounded(px(6.)).bg(t.term).border_1().border_color(t.line).font_family(t.mono_font.clone()).text_size(px(11.)).children(lines.iter().map(|l| {
                    let op = l["op"].as_str().unwrap_or(" ");
                    let color = match op {
                        "+" => t.ok,
                        "-" => t.err,
                        _ => t.dim,
                    };
                    let text = if op == "…" { "…".to_string() } else { format!("{op} {}", l["text"].as_str().unwrap_or("")) };
                    div().text_color(color).whitespace_nowrap().overflow_hidden().text_ellipsis().child(text)
                })),
            )
        })
}

pub fn render(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let sheet = m.hooks_sheet.as_ref()?;
    let s = summary(&m.hooks);
    let (title, body) = match (sheet.uninstall, s) {
        (true, _) => ("Remove midna's hooks?", "midna takes its entries out of the files below and puts Codex's previous notify back. Agents midna starts keep reporting; ones you type by hand stop."),
        (false, Summary::NeedsReinstall) => ("Reinstall midna's hooks", "The hooks in your agents' config are out of date, so a claude or codex you type into a midna terminal may not report status. Reinstalling replaces midna's entries and leaves yours alone."),
        (false, Summary::Current) => ("midna's hooks are installed", "A claude or codex you type into a midna terminal reports status. Outside midna terminals the hooks do nothing."),
        _ => ("Report status from every claude and codex", "Agents midna starts already report. Install midna's hooks in your agents' config so one you type into a midna terminal reports too. Outside midna terminals they do nothing, and Codex's current notify keeps running."),
    };
    let reasons: Vec<String> = present(&m.hooks).iter().filter_map(|(k, h)| h["detail"].as_str().map(|d| format!("{k}: {d}"))).collect();
    let files = sheet.preview.as_ref().and_then(|p| p["files"].as_array().cloned()).unwrap_or_default();
    let nothing = sheet.preview.is_some() && files.iter().all(|f| f["unchanged"] == true);
    let blocked = files.iter().any(|f| f["error"].is_string()) && files.iter().all(|f| f["error"].is_string() || f["unchanged"] == true);
    let primary = if sheet.uninstall || (s == Summary::Current && nothing) {
        None
    } else {
        Some(if s == Summary::NeedsReinstall { "Reinstall hooks  ↩" } else { "Install hooks  ↩" })
    };
    let busy = sheet.busy;
    let loading = sheet.preview.is_none() && sheet.error.is_none();
    let card = div()
        .id("hooks-card")
        .w(px(620.))
        .max_h(px(560.))
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
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).child(title))
        .child(div().text_color(t.dim).child(body))
        .when(!reasons.is_empty() && !sheet.uninstall, |d| d.child(div().flex().flex_col().text_color(t.need).children(reasons.into_iter().map(|r| div().child(r)))))
        .child(
            div()
                .id("hooks-diff")
                .flex()
                .flex_col()
                .gap(px(12.))
                .min_h_0()
                .flex_shrink(1.)
                .overflow_y_scroll()
                .when(loading, |d| d.child(div().text_color(t.dim).child("Reading your config…")))
                .children(files.iter().map(|f| diff_block(t, f))),
        )
        .children(sheet.error.clone().map(|e| div().text_color(t.err).child(e)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .when(!sheet.uninstall && s == Summary::Current, |d| {
                    d.child(btn_danger(t, "hooks-remove", "Remove hooks…", 28.).on_click(cx.listener(|m, _, w, cx| open(m, true, w, cx))))
                })
                .child(div().flex_1())
                .child(btn(t, "hooks-cancel", if primary.is_some() || sheet.uninstall { "Cancel" } else { "Close" }).on_click(cx.listener(|m, _, w, cx| close(m, w, cx))))
                .when(sheet.uninstall, |d| {
                    d.child(btn_danger(t, "hooks-uninstall", if busy { "Removing…" } else { "Remove hooks" }, 28.).on_click(cx.listener(|m, _, _, cx| apply(m, cx))))
                })
                .when_some(primary.filter(|_| !loading && !blocked), |d, label| {
                    d.child(btn_primary(t, "hooks-install", if busy { "Installing…" } else { label }).on_click(cx.listener(|m, _, _, cx| apply(m, cx))))
                }),
        );
    Some(
        div()
            .id("hooks-sheet")
            .track_focus(&m.overlay_focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_mouse_down(MouseButton::Left, cx.listener(|m, _, w, cx| close(m, w, cx)))
            .on_key_down(cx.listener(|m, ev: &KeyDownEvent, w, cx| match ev.keystroke.key.as_str() {
                "escape" => close(m, w, cx),
                "enter" => {
                    let ready = m.hooks_sheet.as_ref().is_some_and(|s| s.preview.is_some() && !s.uninstall);
                    if ready && summary(&m.hooks) != Summary::Current {
                        apply(m, cx);
                    }
                }
                _ => {}
            }))
            .child(card)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's `test` attribute would shadow the std one.
    use super::{Summary, summary};
    use serde_json::{Value, json};

    fn st(claude: &str, codex: &str) -> Value {
        json!({ "claude": { "state": claude }, "codex": { "state": codex } })
    }

    #[test]
    fn summary_ignores_agents_that_are_not_set_up() {
        assert_eq!(summary(&Value::Null), Summary::Hidden);
        assert_eq!(summary(&st("unavailable", "unavailable")), Summary::Hidden);
        assert_eq!(summary(&st("current", "unavailable")), Summary::Current);
        assert_eq!(summary(&st("current", "not_installed")), Summary::NotInstalled);
        assert_eq!(summary(&st("current", "stale")), Summary::NeedsReinstall);
        assert_eq!(summary(&st("error", "current")), Summary::NeedsReinstall);
    }
}
