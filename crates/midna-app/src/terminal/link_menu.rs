//! Link menu (board "Link Actions", design A): a plain click on a link (one that doesn't turn
//! into a drag) opens a menu under it, styled like the right-click menu. Its header is an icon
//! tinted by the link's state, a bold title and a dim subtitle, filled from what the hover
//! preview loads (`link_preview.rs`: the open card's body when the click came from it, else
//! `link_preview::load`, which shares the card's caches). Below it, what fits the link: open,
//! copy, add to links, reveal, mention in the agent's prompt, a new terminal there.
//!
//! The menu is a list of entries (items, dividers and labelled sections) built per kind by
//! `entries`, so more items and app sections can be added to it. ↑/↓ move, ↩ runs, esc closes,
//! and an item's own keys (⌘C, ⇧⌘C, ⌘L) run it while the menu is open.

use super::link_preview::{self, Body, Link, Tone};
use super::pr_status::State;
use super::{LINE_H, PAD_X, PAD_Y, TerminalView, link_span};
use crate::icons::Icon;
use crate::ide::{self, Ide};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use std::path::Path;

/// The link under a click, by what can be done with it.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Kind {
    Url,
    /// A local or private web server (never fetched).
    Server,
    /// A link pattern's match (`T711`, `PROJ-12`), opened in `app` when it names one.
    Pattern { app: Option<String>, text: String },
    File { line: Option<u32> },
    Folder,
    /// Another terminal's id.
    Session,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Act {
    Browse,
    CopyLink,
    CopyMarkdown,
    CopyText(String),
    AddLink,
    OpenIde,
    Reveal,
    CopyPath,
    CopyRelative,
    CopyPathLine,
    Mention,
    NewTerminal,
    GoToTerminal,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Item {
    pub(super) act: Act,
    pub(super) label: String,
    pub(super) keys: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Entry {
    Item(Item),
    Sep,
    /// A labelled group (an app's own actions, or GitHub's), which later menus add.
    #[allow(dead_code)]
    Section(String),
}

/// What the header's icon is tinted by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Tint {
    State(State),
    Tone(Tone),
    Accent,
    Dim,
    File,
    Folder,
    Server,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Head {
    pub(super) icon: Icon,
    pub(super) tint: Tint,
    pub(super) title: String,
    pub(super) sub: String,
}

/// The menu's state on its terminal: the open menu, the click it belongs to, and the hover
/// card's body a click closed (so the header doesn't load it again).
#[derive(Default)]
pub(super) struct LinkMenus {
    pub(super) menu: Option<LinkMenu>,
    /// Bumped by every press: a `session.link_at` answer for an older click is dropped.
    pub(super) click: u64,
    pub(super) seed: Option<(Link, Body)>,
}

pub(super) struct LinkMenu {
    pos: Point<Pixels>,
    /// The `session.link_at` result.
    v: Value,
    kind: Kind,
    target: String,
    project: Option<String>,
    /// The terminal's working directory, for relative paths and mentions.
    cwd: Option<String>,
    ide: Option<Ide>,
    entries: Vec<Entry>,
    /// Index into the items (not the entries).
    sel: usize,
    body: Option<Body>,
    _load: Option<Task<()>>,
}

impl LinkMenu {
    fn items(&self) -> impl Iterator<Item = &Item> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Item(i) => Some(i),
            _ => None,
        })
    }

    fn head(&self) -> Head {
        head(&self.kind, &self.target, self.project.as_deref(), self.body.as_ref())
    }
}

/// What a `session.link_at` hit is. `text` is the link as drawn on screen; `is_dir` whether a
/// file target is a folder.
pub(super) fn kind_of(v: &Value, text: &str, is_dir: bool) -> Option<Kind> {
    let target = v.get("target").and_then(Value::as_str).filter(|t| !t.is_empty())?;
    match v.get("kind").and_then(Value::as_str)? {
        "session" => Some(Kind::Session),
        "file" if is_dir => Some(Kind::Folder),
        "file" => Some(Kind::File { line: v.get("line").and_then(Value::as_u64).map(|n| n as u32).filter(|n| *n > 0) }),
        "url" => {
            let app = v.get("app").and_then(Value::as_str).filter(|a| !a.is_empty()).map(str::to_string);
            let preview = v.get("preview").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
            // A pattern's match is drawn as itself, not as the URL it stands for.
            let bare = target.split_once("://").map_or(target, |(_, r)| r).trim_end_matches('/');
            let matched = !text.is_empty() && text != target && text != bare && !text.contains("://") && !text.contains('/');
            if app.is_some() || preview || matched {
                return Some(Kind::Pattern { app, text: text.to_string() });
            }
            let web = target.starts_with("http://") || target.starts_with("https://");
            Some(if web && !link_preview::fetchable(target) { Kind::Server } else { Kind::Url })
        }
        _ => None,
    }
}

/// The menu's entries for a link. `ide`: the editor a file or folder opens in; `agent`: the
/// terminal runs an agent, whose prompt a path can be mentioned in.
pub(super) fn entries(kind: &Kind, ide: Option<&str>, agent: bool) -> Vec<Entry> {
    let item = |act: Act, label: &str, keys: &'static str| Entry::Item(Item { act, label: label.to_string(), keys });
    let open_in = format!("Open in {}", ide.unwrap_or("editor"));
    match kind {
        Kind::Url => vec![
            item(Act::Browse, "Open in browser", "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::Server => vec![
            item(Act::Browse, "Open in browser", "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::Pattern { app, text } => vec![
            item(Act::Browse, &format!("Open in {}", app.as_deref().unwrap_or("browser")), "↩"),
            item(Act::CopyLink, "Copy link", "⌘C"),
            item(Act::CopyMarkdown, "Copy as Markdown", "⇧⌘C"),
            item(Act::CopyText(text.clone()), &format!("Copy {text}"), ""),
            Entry::Sep,
            item(Act::AddLink, "Add to links", "⌘L"),
        ],
        Kind::File { line } => {
            let mut v = vec![
                item(Act::OpenIde, &open_in, "↩"),
                item(Act::Reveal, "Reveal in Finder", ""),
                Entry::Sep,
                item(Act::CopyPath, "Copy path", "⌘C"),
                item(Act::CopyRelative, "Copy relative path", ""),
            ];
            if line.is_some() {
                v.push(item(Act::CopyPathLine, "Copy path:line", ""));
            }
            if agent {
                v.extend([Entry::Sep, item(Act::Mention, "Mention in prompt", "")]);
            }
            v
        }
        Kind::Folder => vec![
            item(Act::NewTerminal, "New terminal here", "↩"),
            item(Act::OpenIde, &open_in, ""),
            item(Act::Reveal, "Reveal in Finder", ""),
            Entry::Sep,
            item(Act::CopyPath, "Copy path", "⌘C"),
            item(Act::CopyRelative, "Copy relative path", ""),
        ],
        Kind::Session => vec![item(Act::GoToTerminal, "Go to terminal", "↩"), Entry::Sep, item(Act::CopyLink, "Copy id", "⌘C")],
    }
}

/// The header for a link, from what the preview loaded (`body`) when it has.
pub(super) fn head(kind: &Kind, target: &str, project: Option<&str>, body: Option<&Body>) -> Head {
    let h = |icon, tint, title: String, sub: String| Head { icon, tint, title, sub };
    match kind {
        Kind::Session => h(Icon::Shell, Tint::Accent, target.to_string(), "Another terminal".into()),
        Kind::File { line } => {
            let (name, sub) = link_preview::path_title(Path::new(target), false, project);
            let sub = match line {
                Some(n) => format!("{sub} · line {n}"),
                None => sub,
            };
            h(Icon::File, Tint::File, name, sub)
        }
        Kind::Folder => {
            let (name, sub) = link_preview::path_title(Path::new(target), true, project);
            let sub = match body {
                Some(Body::Dir { total, .. }) => format!("{sub} · {total} item{}", if *total == 1 { "" } else { "s" }),
                _ => sub,
            };
            h(Icon::Folder, Tint::Folder, name, sub)
        }
        Kind::Server => {
            let (host, path) = link_preview::url_title(target);
            h(Icon::Server, Tint::Server, host, if path.is_empty() { "Local server".into() } else { format!("/{path}") })
        }
        Kind::Pattern { app, text } => match body {
            Some(Body::App(p)) => {
                let sub = [p.subtitle.clone(), p.status.as_ref().map(|(s, _)| s.clone())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                let tint = p.status.as_ref().map_or(Tint::Accent, |(_, t)| Tint::Tone(*t));
                h(Icon::Tag, tint, p.title.clone(), sub)
            }
            _ => {
                let sub = app.clone().unwrap_or_else(|| link_preview::url_title(target).0);
                h(Icon::Tag, Tint::Accent, text.clone(), sub)
            }
        },
        Kind::Url => {
            let (name, place) = link_preview::url_title(target);
            match body {
                Some(Body::Pr(pr)) => {
                    let state = State::parse(&pr.state).unwrap_or(State::Open);
                    let failing = pr.checks.iter().filter(|(_, c)| *c == link_preview::Check::Fail).count();
                    let pending = pr.checks.iter().any(|(_, c)| *c == link_preview::Check::Pending);
                    let checks = match (pr.checks.is_empty(), failing, pending) {
                        (true, _, _) => String::new(),
                        (_, 0, false) => " · checks passed".into(),
                        (_, 0, true) => " · checks running".into(),
                        (_, n, _) => format!(" · {n} check{} failing", if n == 1 { "" } else { "s" }),
                    };
                    h(state.icon(), Tint::State(state), pr.title.clone(), format!("{place} {name} · {}{checks}", label(state)))
                }
                Some(Body::Run(run)) => {
                    let state = State::parse(run.state.key()).unwrap_or(State::Queued);
                    h(state.icon(), Tint::State(state), run.workflow.clone(), format!("{place} · {} · {}", run.branch, label(state)))
                }
                Some(Body::Web { title: Some(title), .. }) => {
                    h(Icon::Globe, Tint::Dim, title.clone(), if place.is_empty() { name } else { format!("{name}/{place}") })
                }
                _ if link_preview::github_pr(target).is_some() => h(Icon::Pr, Tint::Dim, name, place),
                _ => h(Icon::Globe, Tint::Dim, name, place),
            }
        }
    }
}

fn label(s: State) -> &'static str {
    match s {
        State::Open => "Open",
        State::Draft => "Draft",
        State::Merged => "Merged",
        State::Closed => "Closed",
        State::Queued => "Queued",
        State::Running => "Running",
        State::Passed => "Passed",
        State::Failed => "Failed",
        State::Cancelled => "Cancelled",
        State::Skipped => "Skipped",
        State::Approval => "Needs approval",
    }
}

fn tint_color(t: Tint, theme: &Theme) -> Hsla {
    match t {
        Tint::State(s) => s.color(theme),
        Tint::Tone(Tone::Ok) => theme.ok,
        Tint::Tone(Tone::Err) => theme.err,
        Tint::Tone(Tone::Warn) => theme.need,
        Tint::Tone(Tone::Busy) => theme.work,
        Tint::Tone(Tone::Idle) | Tint::Dim => theme.dim,
        Tint::Accent => theme.accent,
        Tint::File => theme.ansi[12],
        Tint::Folder => theme.need,
        Tint::Server => theme.ansi[14],
    }
}

/// `path` from `base` when it's inside it, else with `~` for the home folder.
pub(super) fn relative(path: &str, base: Option<&str>) -> String {
    match base.and_then(|b| Path::new(path).strip_prefix(b).ok()).filter(|r| !r.as_os_str().is_empty()) {
        Some(rel) => rel.to_string_lossy().into_owned(),
        None => crate::commands::tilde(path),
    }
}

fn debug() -> bool {
    crate::dev::var_os("MIDNA_DEBUG_TERM").is_some()
}

impl TerminalView {
    /// Whether a mouse-up opens the link menu: a plain click (an app that reports the mouse
    /// gets it, unless shift is held, as with the right-click menu).
    pub(super) fn link_menu_click(&self, mods: u8, agent_click: bool) -> bool {
        if self.ext.mouse_tracking && !agent_click {
            mods == super::MOD_SHIFT
        } else {
            mods == 0
        }
    }

    /// Open the link menu for the link at a cell, if there is one.
    pub fn open_link_menu_at(&mut self, cell: (i32, i32), cx: &mut Context<Self>) {
        if cell.0 < 0 || cell.1 < 0 || cell.1 >= self.rows as i32 || cell.0 >= self.cols as i32 {
            return;
        }
        let click = self.link_menu.click;
        let (backend, id) = (self.backend.clone(), self.session_id.clone());
        let params = json!({ "id": id, "col": cell.0, "row": cell.1 });
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let v = backend.call("session.link_at", params).ok().filter(|v| v.get("kind").and_then(Value::as_str).is_some_and(|k| k != "none"))?;
                    let target = v.get("target").and_then(Value::as_str).unwrap_or("").to_string();
                    let file = v.get("kind").and_then(Value::as_str) == Some("file");
                    let is_dir = file && Path::new(&target).is_dir();
                    let cwd = file.then(|| backend.call("session.get", json!({ "id": id })).ok()).flatten();
                    let cwd = cwd.and_then(|s| s.get("cwd").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string));
                    Some((v, is_dir, cwd))
                })
                .await;
            let _ = this.update(cx, |t, cx| {
                if let Some((v, is_dir, cwd)) = found
                    && t.link_menu.click == click
                {
                    t.link_menu_show(cell, v, is_dir, cwd, cx);
                }
            });
        })
        .detach();
    }

    fn link_menu_show(&mut self, cell: (i32, i32), v: Value, is_dir: bool, cwd: Option<String>, cx: &mut Context<Self>) {
        let Some(row) = self.grid.get(cell.1 as usize) else { return };
        let chars: Vec<char> = row.cells.iter().map(|c| c.ch).collect();
        let url = v.get("target").and_then(Value::as_str).filter(|_| v.get("kind").and_then(Value::as_str) == Some("url"));
        let Some((a, b)) = link_span(&chars, cell.0 as usize, url) else { return };
        let text: String = chars[a..=b].iter().collect();
        let Some(kind) = kind_of(&v, &text, is_dir) else { return };
        let Some(bounds) = self.bounds.get() else { return };
        let target = v.get("target").and_then(Value::as_str).unwrap_or("").to_string();
        let env = link_preview::env(cx);
        let path = matches!(kind, Kind::File { .. } | Kind::Folder);
        let project = path.then(|| env.projects.iter().filter(|p| Path::new(&target).starts_with(p)).max_by_key(|p| p.len()).cloned()).flatten();
        let ide = path.then(|| self.preview_ide(&env, Path::new(&target), is_dir, project.as_deref())).flatten().map(|(i, _)| i);
        let entries = entries(&kind, ide.as_ref().map(|i| i.name.as_str()), self.is_agent());
        let link = Link::from_link_at(&v);
        let seed = self.link_menu.seed.take().filter(|(l, _)| Some(l) == link.as_ref()).map(|(_, b)| b);
        let wants = matches!(kind, Kind::Url | Kind::Pattern { .. } | Kind::Folder);
        let load = link.filter(|_| seed.is_none() && wants).map(|l| {
            cx.spawn(async move |this, cx| {
                let body = cx.background_executor().spawn(async move { link_preview::load(&l) }).await;
                let _ = this.update(cx, |t, cx| {
                    if let Some(m) = t.link_menu.menu.as_mut() {
                        m.body = Some(body);
                        cx.notify();
                    }
                });
            })
        });
        let pos = point(bounds.origin.x + px(PAD_X + self.cell_w * a as f32 - 4.), bounds.origin.y + px(PAD_Y + LINE_H * (cell.1 + 1) as f32 + 2.));
        self.clear_hover(cx);
        self.preview_close(cx);
        self.menu = None;
        self.link_menu.menu = Some(LinkMenu { pos, v, kind, target, project, cwd, ide, entries, sel: 0, body: seed, _load: load });
        cx.notify();
    }

    pub(super) fn link_menu_close(&mut self, cx: &mut Context<Self>) {
        if self.link_menu.menu.take().is_some() {
            cx.notify();
        }
    }

    /// A key while the menu is open: true when the menu used it. Any other key closes it and
    /// goes on to the terminal.
    pub(super) fn link_menu_key(&mut self, ks: &Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(m) = self.link_menu.menu.as_mut() else { return false };
        let n = m.items().count();
        let mods = &ks.modifiers;
        let plain = !mods.platform && !mods.control && !mods.alt && !mods.shift;
        let keys = match (mods.platform && !mods.control && !mods.alt, mods.shift, ks.key.as_str()) {
            (true, false, "c") => Some("⌘C"),
            (true, true, "c") => Some("⇧⌘C"),
            (true, false, "l") => Some("⌘L"),
            _ => None,
        };
        match ks.key.as_str() {
            "up" if plain => m.sel = (m.sel + n - 1) % n.max(1),
            "down" if plain => m.sel = (m.sel + 1) % n.max(1),
            "enter" if plain => {
                let i = m.sel;
                self.link_menu_run(i, window, cx);
            }
            "escape" => self.link_menu_close(cx),
            _ => match keys.and_then(|k| m.items().position(|i| i.keys == k)) {
                Some(i) => self.link_menu_run(i, window, cx),
                None => {
                    self.link_menu_close(cx);
                    return false;
                }
            },
        }
        cx.notify();
        true
    }

    fn link_menu_copy(&self, text: String, cx: &mut Context<Self>) {
        if debug() {
            eprintln!("midna-app debug-term: linkmenu copied {text}");
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        crate::sounds::play("copied");
    }

    fn link_menu_run(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(m) = self.link_menu.menu.take() else { return };
        let Some(act) = m.items().nth(i).map(|it| it.act.clone()) else { return };
        let target = m.target.clone();
        let line = match m.kind {
            Kind::File { line } => line,
            _ => None,
        };
        if debug() {
            eprintln!("midna-app debug-term: linkmenu {act:?} {target}");
        }
        match act {
            Act::Browse | Act::GoToTerminal => self.open_link(&m.v, cx),
            Act::CopyLink | Act::CopyPath => self.link_menu_copy(target, cx),
            Act::CopyText(t) => self.link_menu_copy(t, cx),
            Act::CopyMarkdown => {
                let text = match &m.kind {
                    Kind::Pattern { text, .. } => text.clone(),
                    _ => m.head().title,
                };
                self.link_menu_copy(format!("[{}]({target})", text.replace(['[', ']'], "")), cx);
            }
            Act::CopyRelative => self.link_menu_copy(relative(&target, m.project.as_deref().or(m.cwd.as_deref())), cx),
            Act::CopyPathLine => self.link_menu_copy(format!("{target}:{}", line.unwrap_or(1)), cx),
            Act::Mention => {
                let at = format!("@{} ", relative(&target, m.cwd.as_deref().or(m.project.as_deref())));
                self.paste_text(&at, window, cx);
            }
            Act::AddLink => {
                let (backend, session) = (self.backend.clone(), self.session_id.clone());
                cx.background_executor().spawn(async move { backend.call("links.add", json!({ "session": session, "target": target })) }).detach();
            }
            Act::Reveal => {
                if !debug() {
                    let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(&target).spawn();
                }
            }
            Act::NewTerminal => self.new_terminal_in(target, cx),
            Act::OpenIde => match m.ide {
                Some(ide) if !debug() => {
                    cx.background_executor()
                        .spawn(async move {
                            if let Err(e) = ide::open_file(&ide, &target, line) {
                                eprintln!("midna-app: couldn't open {target} in {}: {e}", ide.name);
                            }
                        })
                        .detach();
                }
                Some(_) => {}
                None => self.open_link(&m.v, cx),
            },
        }
        self.focus.focus(window, cx);
        cx.notify();
    }

    pub(super) fn render_link_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let m = self.link_menu.menu.as_ref()?;
        let head = m.head();
        let header = div()
            .flex()
            .gap(px(9.))
            .pt(px(8.))
            .px(px(10.))
            .pb(px(9.))
            .child(div().mt(px(1.)).child(head.icon.el(15., tint_color(head.tint, theme))))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_size(px(12.5)).font_weight(FontWeight::BOLD).text_color(theme.fg).overflow_hidden().text_ellipsis().whitespace_nowrap().child(head.title))
                    .when(!head.sub.is_empty(), |d| {
                        d.child(div().mt(px(2.)).text_size(px(11.5)).text_color(theme.dim).overflow_hidden().text_ellipsis().whitespace_nowrap().child(head.sub))
                    }),
            );
        let mut menu = div()
            .id("link-menu")
            .occlude()
            .w(px(300.))
            .p(px(4.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.line)
            .bg(theme.raised)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.3), offset: point(px(0.), px(8.)), blur_radius: px(24.), spread_radius: px(0.), inset: false }])
            .on_mouse_down_out(cx.listener(|t, _, _, cx| t.link_menu_close(cx)))
            .child(header)
            .child(div().h(px(1.)).mb(px(4.)).mx(px(6.)).bg(theme.line));
        let mut n = 0;
        for e in &m.entries {
            menu = match e {
                Entry::Sep => menu.child(div().h(px(1.)).my(px(4.)).mx(px(6.)).bg(theme.line)),
                Entry::Section(label) => menu.child(div().px(px(10.)).pt(px(4.)).pb(px(2.)).text_size(px(10.5)).text_color(theme.dim).child(label.clone())),
                Entry::Item(it) => {
                    let i = n;
                    n += 1;
                    let on = i == m.sel;
                    menu.child(
                        div()
                            .id(("link-menu-item", i))
                            .flex()
                            .items_center()
                            .gap(px(18.))
                            .h(px(24.))
                            .px(px(10.))
                            .rounded(px(5.))
                            .text_size(px(12.5))
                            .cursor_pointer()
                            .text_color(if on { theme.accent_fg } else { theme.fg })
                            .when(on, |d| d.bg(theme.accent))
                            .on_mouse_move(cx.listener(move |t, _, _, cx| {
                                if let Some(m) = t.link_menu.menu.as_mut()
                                    && m.sel != i
                                {
                                    m.sel = i;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |t, _, window, cx| t.link_menu_run(i, window, cx)))
                            .child(div().flex_1().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(it.label.clone()))
                            .child(div().text_color(if on { theme.accent_fg.opacity(0.75) } else { theme.dim }).text_size(px(11.5)).child(it.keys)),
                    )
                }
            };
        }
        Some(deferred(anchored().position(m.pos).snap_to_window_with_margin(px(8.)).child(menu)).with_priority(2).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::{Body, Entry, Icon, Kind, Tint, entries, head, kind_of, relative};
    use serde_json::json;

    fn labels(kind: &Kind, ide: Option<&str>, agent: bool) -> Vec<String> {
        entries(kind, ide, agent)
            .into_iter()
            .map(|e| match e {
                Entry::Item(i) if i.keys.is_empty() => i.label,
                Entry::Item(i) => format!("{} {}", i.label, i.keys),
                Entry::Sep => "--".into(),
                Entry::Section(s) => format!("[{s}]"),
            })
            .collect()
    }

    #[test]
    fn url_items() {
        assert_eq!(labels(&Kind::Url, None, false), ["Open in browser ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "--", "Add to links ⌘L"]);
        assert_eq!(labels(&Kind::Server, None, false), ["Open in browser ↩", "Copy link ⌘C", "--", "Add to links ⌘L"]);
    }

    #[test]
    fn pattern_items() {
        let k = Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() };
        assert_eq!(labels(&k, None, false), ["Open in Taskboard ↩", "Copy link ⌘C", "Copy as Markdown ⇧⌘C", "Copy T711", "--", "Add to links ⌘L"]);
        let k = Kind::Pattern { app: None, text: "PROJ-12".into() };
        assert_eq!(labels(&k, None, false)[0], "Open in browser ↩");
    }

    #[test]
    fn file_items() {
        let k = Kind::File { line: Some(42) };
        assert_eq!(
            labels(&k, Some("Zed"), true),
            ["Open in Zed ↩", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path", "Copy path:line", "--", "Mention in prompt"]
        );
        // No line, not an agent, no editor found.
        assert_eq!(labels(&Kind::File { line: None }, None, false), ["Open in editor ↩", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path"]);
    }

    #[test]
    fn folder_and_session_items() {
        assert_eq!(
            labels(&Kind::Folder, Some("Cursor"), true),
            ["New terminal here ↩", "Open in Cursor", "Reveal in Finder", "--", "Copy path ⌘C", "Copy relative path"]
        );
        assert_eq!(labels(&Kind::Session, None, false), ["Go to terminal ↩", "--", "Copy id ⌘C"]);
    }

    #[test]
    fn kinds() {
        let url = |t: &str| json!({ "kind": "url", "target": t });
        assert_eq!(kind_of(&url("https://example.com/a"), "https://example.com/a", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("https://example.com"), "example.com", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("https://github.com/a/b"), "github.com/a/b", false), Some(Kind::Url));
        assert_eq!(kind_of(&url("http://localhost:3000/x"), "http://localhost:3000/x", false), Some(Kind::Server));
        assert_eq!(kind_of(&url("https://jira.example.com/browse/PROJ-12"), "PROJ-12", false), Some(Kind::Pattern { app: None, text: "PROJ-12".into() }));
        let app = json!({ "kind": "url", "target": "taskboard://task/T711", "app": "Taskboard" });
        assert_eq!(kind_of(&app, "T711", false), Some(Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() }));
        let file = json!({ "kind": "file", "target": "/tmp/a.rs", "line": 7 });
        assert_eq!(kind_of(&file, "a.rs:7", false), Some(Kind::File { line: Some(7) }));
        assert_eq!(kind_of(&file, "tmp", true), Some(Kind::Folder));
        assert_eq!(kind_of(&json!({ "kind": "session", "target": "s3" }), "s3", false), Some(Kind::Session));
        assert_eq!(kind_of(&json!({ "kind": "none" }), "", false), None);
    }

    #[test]
    fn heads() {
        let f = head(&Kind::File { line: Some(12) }, "/p/proj/src/main.rs", Some("/p/proj"), None);
        assert_eq!((f.icon, f.tint, f.title.as_str(), f.sub.as_str()), (Icon::File, Tint::File, "main.rs", "src · line 12"));
        let body = Body::Dir { rows: vec![], total: 3 };
        let d = head(&Kind::Folder, "/p/proj/src", Some("/p/proj"), Some(&body));
        assert_eq!((d.icon, d.tint, d.title.as_str(), d.sub.as_str()), (Icon::Folder, Tint::Folder, "src/", "proj · 3 items"));
        let s = head(&Kind::Server, "http://localhost:3000", None, None);
        assert_eq!((s.icon, s.tint, s.title.as_str(), s.sub.as_str()), (Icon::Server, Tint::Server, "localhost:3000", "Local server"));
        let p = head(&Kind::Pattern { app: Some("Taskboard".into()), text: "T711".into() }, "taskboard://task/T711", None, None);
        assert_eq!((p.icon, p.tint, p.title.as_str(), p.sub.as_str()), (Icon::Tag, Tint::Accent, "T711", "Taskboard"));
        let w = head(&Kind::Url, "https://example.com/docs", None, Some(&Body::Web { title: Some("Docs".into()), desc: None }));
        assert_eq!((w.icon, w.title.as_str(), w.sub.as_str()), (Icon::Globe, "Docs", "example.com/docs"));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative("/p/proj/src/a.rs", Some("/p/proj")), "src/a.rs");
        assert_eq!(relative("/p/proj", Some("/p/proj")), "/p/proj");
        assert_eq!(relative("/elsewhere/a.rs", Some("/p/proj")), "/elsewhere/a.rs");
    }
}
