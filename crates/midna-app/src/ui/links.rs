//! Session links (Links-A): the header's links button and its popover (⌘L). The daemon
//! collects the links, PRs, artifacts and files that come up in an agent terminal's
//! conversation (`links.list`); here they are filtered, opened, pinned, or found in the
//! terminal. ↩ opens, ⌥↩ finds where it came up, ⇧↩ pins, ⇥ / ⇧⇥ switch the kind tab.
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use crate::model::AgentKind;
use midna_proto::{Link, LinkKind, LinkSource, time};
use serde_json::{Value, json};
use std::collections::HashMap;

actions!(
    midna,
    [
        /// ⌘L: open or close the selected terminal's links.
        ToggleLinks,
    ]
);

/// The kind tabs, in order (`None` = all).
const TABS: [(Option<LinkKind>, &str); 5] =
    [(None, "All"), (Some(LinkKind::Pr), "PRs"), (Some(LinkKind::Artifact), "Artifacts"), (Some(LinkKind::Web), "Web"), (Some(LinkKind::File), "Files")];

pub struct LinksPanel {
    pub input: Entity<super::text_input::TextField>,
    pub focus: FocusHandle,
    query: String,
    tab: usize,
    sel: usize,
    scroll: ScrollHandle,
    /// Each terminal's links as last fetched.
    pub by_session: HashMap<String, Vec<Link>>,
    /// When each terminal's links were last looked at: newer ones light the button's dot.
    seen_at: HashMap<String, String>,
    /// `seen_at` from before this opening, for the NEW badges while the popover is open.
    badge_since: String,
}

impl LinksPanel {
    pub fn new(cx: &mut App) -> LinksPanel {
        let input = cx.new(|cx| super::text_input::TextField::new(cx, false, "Filter links"));
        let focus = input.read(cx).focus.clone();
        LinksPanel {
            input,
            focus,
            query: String::new(),
            tab: 0,
            sel: 0,
            scroll: ScrollHandle::new(),
            by_session: HashMap::new(),
            seen_at: HashMap::new(),
            badge_since: String::new(),
        }
    }
}

/// Keep `query` in sync with the field: call once from MainWindow::new.
pub fn wire(p: &LinksPanel, cx: &mut Context<MainWindow>) {
    cx.subscribe(&p.input, |m, field, _: &super::text_input::FieldChanged, cx| {
        let text = field.read(cx).text().to_string();
        if text != m.links.query {
            m.links.query = text;
            m.links.sel = 0;
            m.links.scroll.scroll_to_item(0);
            cx.notify();
        }
    })
    .detach();
}

/// Fetch a terminal's links (on selection, `links.changed`, and opening the popover).
pub fn fetch(m: &mut MainWindow, sid: String, cx: &mut Context<MainWindow>) {
    let backend = m.backend.clone();
    cx.spawn(async move |this, cx| {
        let p = json!({ "session": sid });
        let res = cx.background_executor().spawn(async move { backend.call("links.list", p) }).await;
        let Ok(v) = res else { return };
        let links: Vec<Link> = v.get("links").cloned().and_then(|l| serde_json::from_value(l).ok()).unwrap_or_default();
        let _ = this.update(cx, |m, cx| {
            // What was already there when midna first looked is not new.
            m.links.seen_at.entry(sid.clone()).or_insert_with(time::now_rfc3339);
            if m.menu == Menu::Links && m.selected.as_deref() == Some(sid.as_str()) {
                m.links.seen_at.insert(sid.clone(), time::now_rfc3339());
            }
            m.links.by_session.insert(sid, links);
            cx.notify();
        });
    })
    .detach();
}

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.menu == Menu::Links {
        close(m, window, cx);
        return;
    }
    let Some(sid) = m.selected.clone() else { return };
    m.menu = Menu::Links;
    m.links.badge_since = m.links.seen_at.get(&sid).cloned().unwrap_or_default();
    m.links.seen_at.insert(sid.clone(), time::now_rfc3339());
    m.links.input.update(cx, |f, cx| f.clear(cx));
    m.links.query.clear();
    m.links.sel = 0;
    m.links.scroll.scroll_to_item(0);
    m.links.focus.focus(window, cx);
    fetch(m, sid, cx);
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.focus_terminal(window, cx);
    cx.notify();
}

fn current(m: &MainWindow) -> &[Link] {
    m.selected.as_ref().and_then(|s| m.links.by_session.get(s)).map(Vec::as_slice).unwrap_or(&[])
}

/// Links whose first mention is newer than `since`.
fn new_count(links: &[Link], since: &str) -> usize {
    links.iter().filter(|l| l.first_at.as_str() > since).count()
}

fn matches(l: &Link, q: &str) -> bool {
    q.split_whitespace().all(|w| {
        let w = w.to_lowercase();
        [Some(&l.title), Some(&l.target), l.via.as_ref(), l.note.as_ref()].into_iter().flatten().any(|s| s.to_lowercase().contains(&w))
    })
}

/// The rows shown: pinned first (the daemon's order), filtered by tab and query.
fn rows(m: &MainWindow) -> Vec<Link> {
    let kind = TABS[m.links.tab].0;
    let q = m.links.query.trim();
    current(m).iter().filter(|l| kind.is_none_or(|k| l.kind == k) && matches(l, q)).cloned().collect()
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let ks = &ev.keystroke;
    let md = &ks.modifiers;
    let rows = rows(m);
    let n = rows.len();
    match ks.key.as_str() {
        "up" | "down" if !md.platform => {
            if n > 0 {
                let cur = m.links.sel.min(n - 1);
                m.links.sel = if ks.key == "down" { (cur + 1) % n } else { (cur + n - 1) % n };
                m.links.scroll.scroll_to_item(m.links.sel);
            }
        }
        "enter" if !md.platform => {
            let Some(l) = rows.get(m.links.sel.min(n.saturating_sub(1))).cloned() else { return };
            if md.shift {
                pin(m, &l, cx);
            } else if md.alt {
                jump(m, &l, window, cx);
            } else {
                open(m, &l, window, cx);
            }
        }
        "tab" => set_tab(m, if md.shift { m.links.tab + TABS.len() - 1 } else { m.links.tab + 1 }, cx),
        "escape" => close(m, window, cx),
        // Everything else is the field's: editing keys, and typing through the IME.
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

fn set_tab(m: &mut MainWindow, tab: usize, cx: &mut Context<MainWindow>) {
    m.links.tab = tab % TABS.len();
    m.links.sel = 0;
    m.links.scroll.scroll_to_item(0);
    cx.notify();
}

fn open(m: &mut MainWindow, l: &Link, window: &mut Window, cx: &mut Context<MainWindow>) {
    if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
        eprintln!("midna-app debug-term: link {}", l.target); // dev runs never launch apps
    } else if l.kind == LinkKind::File {
        if let Some(sid) = m.selected.clone() {
            crate::terminal::open_file(m.backend.clone(), sid, l.target.clone(), None);
        }
    } else {
        cx.open_url(&l.target);
    }
    close(m, window, cx);
}

fn pin(m: &mut MainWindow, l: &Link, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone() else { return };
    let pinned = !l.pinned;
    // Optimistic; `links.changed` brings the daemon's order.
    if let Some(x) = m.links.by_session.get_mut(&sid).and_then(|v| v.iter_mut().find(|x| x.id == l.id)) {
        x.pinned = pinned;
    }
    m.rpc("links.pin", json!({ "session": sid, "link": l.id, "pinned": pinned }), cx, |_, _, _, _| {});
    cx.notify();
}

/// Find where the link came up in the terminal (`session.find`, newest first).
fn jump(m: &mut MainWindow, l: &Link, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(sid) = m.selected.clone() else { return };
    // Files show up in the transcript as written, usually relative; PRs as printed.
    let query = if l.kind == LinkKind::File { l.title.trim_start_matches("~/").to_string() } else { l.target.clone() };
    close(m, window, cx);
    m.rpc("session.find", json!({ "id": sid, "query": query, "backwards": true }), cx, |m, v, _, cx| {
        if v.get("count").and_then(Value::as_u64) == Some(0) {
            m.toast("It's not on screen or in the scrollback any more", cx);
        }
    });
}

// ------------------------------------------------------------------ rendering

/// The header button: link icon, count, and a dot while something new hasn't been seen.
pub fn button(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let s = m.selected_session()?;
    let links = m.links.by_session.get(&s.id).map(Vec::as_slice).unwrap_or(&[]);
    if links.is_empty() && s.agent.is_none() {
        return None;
    }
    let open = m.menu == Menu::Links;
    let fresh = !open && m.links.seen_at.get(&s.id).is_some_and(|since| new_count(links, since) > 0);
    let tip = format!("Session links ({})", links.len());
    let color = if open || fresh { t.accent } else { t.dim };
    let el = div()
        .id("tb-links")
        .flex()
        .items_center()
        .gap(px(6.))
        .h(px(32.))
        .px(px(9.))
        .rounded(px(7.))
        .cursor_pointer()
        .border_1()
        .border_color(if open { t.accent } else { transparent_black() })
        .when(open, |d| d.bg(t.accent_soft))
        .hover(|st| st.bg(t.raised))
        .tooltip(super::header::tip_keys(tip, "keys.links"))
        .on_click(cx.listener(|m, _, window, cx| toggle(m, window, cx)))
        .child(Icon::Link.el(16., color))
        .when(!links.is_empty(), |d| d.child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(color).child(links.len().to_string())))
        .when(fresh, |d| d.child(div().size(px(6.)).rounded_full().bg(t.accent)));
    Some(div().relative().child(el).when(open, |d| d.child(popover(m, t, cx))).into_any_element())
}

fn kind_icon(k: LinkKind) -> Icon {
    match k {
        LinkKind::Pr => Icon::Pr,
        LinkKind::Artifact => Icon::Artifact,
        LinkKind::Web => Icon::Globe,
        LinkKind::File => Icon::File,
    }
}

/// "2m ago", "3h ago", "Oct 2".
fn ago(at: &str) -> String {
    let Some(then) = time::parse_rfc3339(at) else { return String::new() };
    let s = (time::now_unix() - then).max(0);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// Where it came up, in words: "you pasted · 2m ago", "edited 3× · 1h ago".
fn subtitle(l: &Link, agent: &str) -> String {
    let n = l.mentions;
    let how = match l.source {
        LinkSource::User => "you sent".to_string(),
        LinkSource::Agent => format!("{agent} said"),
        LinkSource::Fetched => format!("{agent} fetched"),
        LinkSource::Tool => match &l.via {
            Some(v) => format!("from {v}"),
            None => "from a tool".into(),
        },
        LinkSource::Created if n > 1 => format!("created, edited {}×", n - 1),
        LinkSource::Created => "created".into(),
        LinkSource::Edited if n > 1 => format!("edited {n}×"),
        LinkSource::Edited => "edited".into(),
        LinkSource::Added => "added".into(),
    };
    let times = if n > 1 && !matches!(l.source, LinkSource::Created | LinkSource::Edited) { format!(" · {n}×") } else { String::new() };
    let lead = l.note.clone().unwrap_or(how);
    format!("{lead}{times} · {}", ago(&l.last_at))
}

fn popover(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let all = current(m);
    let rows = rows(m);
    let sel = m.links.sel.min(rows.len().saturating_sub(1));
    let agent = match m.selected_session().and_then(|s| s.agent) {
        Some(AgentKind::Codex) => "Codex",
        _ => "Claude",
    };

    let tabs = TABS.iter().enumerate().fold(div().flex().gap(px(4.)).px(px(12.)).pb(px(8.)).border_b_1().border_color(t.line), |d, (i, (kind, label))| {
        let n = all.iter().filter(|l| kind.is_none_or(|k| l.kind == k)).count();
        let on = i == m.links.tab;
        d.child(
            div()
                .id(SharedString::from(format!("links-tab-{i}")))
                .flex()
                .items_center()
                .gap(px(5.))
                .h(px(26.))
                .px(px(9.))
                .rounded(px(6.))
                .text_size(px(12.))
                .cursor_pointer()
                .text_color(if on { t.accent } else { t.dim })
                .when(on, |d| d.bg(t.accent_soft))
                .on_click(cx.listener(move |m, _, _, cx| set_tab(m, i, cx)))
                .child(label.to_string())
                .child(div().opacity(0.7).child(n.to_string())),
        )
    });

    let heading = |text: &str| div().px(px(14.)).pt(px(10.)).pb(px(2.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child(text.to_uppercase());
    let mut list = div().id("links-list").flex().flex_col().max_h(px(420.)).overflow_y_scroll().track_scroll(&m.links.scroll).pb(px(6.));
    let pinned = rows.iter().take_while(|l| l.pinned).count();
    for (i, l) in rows.iter().enumerate() {
        if i == 0 && pinned > 0 {
            list = list.child(heading("Pinned"));
        }
        if i == pinned {
            list = list.child(heading(if pinned > 0 { "This session" } else { "Links" }));
        }
        list = list.child(row(m, t, l, i, i == sel, agent, cx));
    }
    if rows.is_empty() {
        let text = if all.is_empty() {
            format!("Nothing yet. Links, PRs, artifacts and files show up here as they come up in the conversation, and {agent} can pin what matters.")
        } else {
            "Nothing matches.".to_string()
        };
        list = list.child(div().px(px(14.)).py(px(14.)).text_color(t.dim).child(text));
    }

    let key = |k: &str, what: &str| {
        div().flex().gap(px(5.)).child(div().font_family(t.mono_font.clone()).child(k.to_string())).child(what.to_string())
    };
    let footer = div()
        .flex()
        .gap(px(14.))
        .px(px(14.))
        .py(px(9.))
        .border_t_1()
        .border_color(t.line)
        .text_size(px(11.5))
        .text_color(t.dim)
        .child(key("↩", "open"))
        .child(key("⌥↩", "find in terminal"))
        .child(key("⇧↩", "pin"))
        .child(div().flex_1())
        .child(key("⇥", "kind"));

    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            div()
                .id("links-popover")
                .key_context("MidnaOverlay")
                .track_focus(&m.links.focus)
                .on_key_down(cx.listener(on_key))
                .occlude()
                .mt(px(38.))
                .w(px(460.))
                .flex()
                .flex_col()
                .rounded(px(12.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_color(t.fg)
                .text_size(px(13.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(18.)), blur_radius: px(48.), spread_radius: px(0.), inset: false }])
                .child(
                    div().p(px(10.)).child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(32.))
                            .px(px(10.))
                            .rounded(px(7.))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.panel)
                            .child(Icon::Search.el(14., t.dim))
                            .child(div().flex().flex_1().min_w_0().items_center().overflow_hidden().child(m.links.input.clone())),
                    ),
                )
                .child(tabs)
                .child(list)
                .child(footer),
        ),
    )
    .with_priority(1)
}

#[allow(clippy::too_many_arguments)]
fn row(m: &MainWindow, t: &Theme, l: &Link, i: usize, selected: bool, agent: &str, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let fresh = !m.links.badge_since.is_empty() && l.first_at > m.links.badge_since;
    let (open_l, pin_l) = (l.clone(), l.clone());
    div()
        .id(SharedString::from(format!("link-{}", l.id)))
        .flex()
        .items_center()
        .gap(px(10.))
        .mx(px(6.))
        .pl(px(8.))
        .pr(px(4.))
        .py(px(6.))
        .rounded(px(7.))
        .cursor_pointer()
        .when(selected, |d| d.bg(t.accent_soft))
        .on_mouse_move(cx.listener(move |m, _, _, cx| {
            if m.links.sel != i {
                m.links.sel = i;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |m, ev: &ClickEvent, window, cx| {
            if ev.modifiers().alt {
                jump(m, &open_l, window, cx);
            } else {
                open(m, &open_l, window, cx);
            }
        }))
        .child(kind_icon(l.kind).el(16., t.dim))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .min_w_0()
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_weight(FontWeight::MEDIUM).child(l.title.clone()))
                        .when(fresh, |d| {
                            d.child(div().flex_none().px(px(5.)).rounded(px(4.)).bg(t.accent).text_color(t.accent_fg).text_size(px(10.)).font_weight(FontWeight::BOLD).child("NEW"))
                        }),
                )
                .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.5)).text_color(t.dim).child(subtitle(l, agent))),
        )
        .child(
            div()
                .id(SharedString::from(format!("link-pin-{}", l.id)))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(px(6.))
                .hover(|st| st.bg(t.panel))
                .tooltip(super::header::tip_fixed(if l.pinned { "Unpin" } else { "Pin" }, "⇧↩"))
                .on_click(cx.listener(move |m, _, _, cx| {
                    cx.stop_propagation();
                    pin(m, &pin_l, cx);
                }))
                .child(if l.pinned { Icon::PushPinFill.el(14., t.accent) } else { Icon::PushPin.el(14., t.dim) }),
        )
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui's glob re-export would shadow `#[test]`.
    use super::{ago, matches, new_count, subtitle};
    use midna_proto::{Link, LinkKind, LinkSource};

    fn link(title: &str, kind: LinkKind, source: LinkSource, mentions: u32) -> Link {
        Link {
            id: "l_1".into(),
            kind,
            target: format!("https://x.com/{title}"),
            title: title.into(),
            source,
            via: Some("gh pr create".into()),
            mentions,
            first_at: "2026-10-04T10:00:00Z".into(),
            last_at: "2026-10-04T10:00:00Z".into(),
            pinned: false,
            pinned_by: None,
            note: None,
        }
    }

    #[test]
    fn subtitles_say_where_it_came_from() {
        let s = |l: &Link| subtitle(l, "Claude").split(" · ").take(2).collect::<Vec<_>>().join(" · ");
        assert_eq!(s(&link("a", LinkKind::Pr, LinkSource::Tool, 1)), format!("from gh pr create · {}", ago("2026-10-04T10:00:00Z")));
        assert!(s(&link("a", LinkKind::Web, LinkSource::Agent, 3)).starts_with("Claude said · 3×"));
        assert!(s(&link("a", LinkKind::File, LinkSource::Created, 3)).starts_with("created, edited 2×"));
        let mut noted = link("a", LinkKind::Web, LinkSource::Added, 1);
        noted.note = Some("the spec".into());
        assert!(subtitle(&noted, "Claude").starts_with("the spec · "));
    }

    #[test]
    fn filter_matches_every_word_anywhere() {
        let l = link("PR #12 · o/r", LinkKind::Pr, LinkSource::Tool, 1);
        assert!(matches(&l, "pr 12"));
        assert!(matches(&l, "gh create"));
        assert!(!matches(&l, "issue"));
        assert_eq!(new_count(&[l], "2026-10-04T09:00:00Z"), 1);
    }
}
