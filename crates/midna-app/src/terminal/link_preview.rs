//! Link preview (board `docs/design/LinkPreview-A.dc.html`): resting the pointer on a path or
//! URL in the grid (or ⌘-hovering it, per `terminal.link_preview`) opens a card at the bottom
//! right of the pane, above the queued-messages pill. It opens after `SHOW_DELAY` and stays while
//! the pointer is on the link, on the card, or while its IDE menu is open. When the pointer
//! leaves, a bar along the card's top drains for `GRACE` before it closes, so there's time to
//! reach it. A key press or a click in the terminal closes it at once.
//!
//! Files show the lines around the target line (wide lines scroll sideways), folders their
//! entries, images the image, GitHub pull requests their state and checks (`gh`), other web
//! pages their title and description (fetched with `curl`, never for local or private hosts).
//! The footer opens a file in the IDE (`ide.rs`: real app icons, remembered per project; with
//! nothing remembered yet the first click lists the editors), and clicking the path in the
//! header reveals, opens or copies it (`terminal.preview_path_click`).
use super::{LINE_H, PAD_Y, TerminalView};
use crate::app::MainWindow;
use crate::icons::Icon;
use crate::ide::{self, Ide};
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const SHOW_DELAY: Duration = Duration::from_millis(350);
const GRACE: Duration = Duration::from_millis(1500);
/// Clear of the prompt rail on the right edge (as the queue pill).
const RIGHT: f32 = 22.;
/// Lines of a file shown, and how many of them come before the target line.
const FILE_LINES: usize = 12;
const BEFORE: usize = 4;
const DIR_ROWS: usize = 8;
const CHECKS: usize = 8;
/// Fetched pages and pull requests are reused for this long.
const URL_TTL: Duration = Duration::from_secs(90);

/// `terminal.link_preview`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Mode {
    #[default]
    Hover,
    Cmd,
    Off,
}

/// `terminal.preview_path_click`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PathClick {
    #[default]
    Reveal,
    Ide,
    Copy,
}

/// What the main window shares with every pane, pop-outs included: the editors found, the IDE
/// settings, project folders and the preview settings.
#[derive(Clone, Default)]
pub struct PreviewEnv {
    pub ides: Vec<Ide>,
    pub ide_app: Option<String>,
    pub ide_rules: Vec<String>,
    pub projects: Vec<String>,
    pub mode: Mode,
    pub path_click: PathClick,
}

impl Global for PreviewEnv {}

/// Called by the main window whenever settings, projects or the editor list change.
pub fn share(m: &MainWindow, cx: &mut App) {
    let s = |k: &str| m.settings.get(k).and_then(Value::as_str).map(str::to_string);
    cx.set_global(PreviewEnv {
        ides: m.ide.list.clone(),
        ide_app: s("ide.app"),
        ide_rules: m.settings.get("ide.rules").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        projects: m.projects.iter().map(|p| p.path.clone()).collect(),
        mode: match s("terminal.link_preview").as_deref() {
            Some("cmd") => Mode::Cmd,
            Some("off") => Mode::Off,
            _ => Mode::Hover,
        },
        path_click: match s("terminal.preview_path_click").as_deref() {
            Some("ide") => PathClick::Ide,
            Some("copy") => PathClick::Copy,
            _ => PathClick::Reveal,
        },
    });
}

pub(super) fn mode(cx: &App) -> Mode {
    cx.try_global::<PreviewEnv>().map(|e| e.mode).unwrap_or_default()
}

fn env(cx: &App) -> PreviewEnv {
    cx.try_global::<PreviewEnv>().cloned().unwrap_or_default()
}

/// A `session.link_at` result worth previewing.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Link {
    url: bool,
    target: String,
    line: Option<u32>,
}

impl Link {
    pub(super) fn from_link_at(v: &Value) -> Option<Link> {
        let target = v.get("target").and_then(Value::as_str).filter(|t| !t.is_empty())?.to_string();
        let url = match v.get("kind").and_then(Value::as_str)? {
            "url" => true,
            "file" => false,
            _ => return None,
        };
        let line = v.get("line").and_then(Value::as_u64).map(|n| n as u32).filter(|n| *n > 0);
        Some(Link { url, target, line })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Check {
    Pass,
    Fail,
    Pending,
}

#[derive(Clone, Debug)]
struct Pr {
    title: String,
    /// OPEN, DRAFT, MERGED or CLOSED.
    state: String,
    head: String,
    base: String,
    additions: u64,
    deletions: u64,
    checks: Vec<(String, Check)>,
}

#[derive(Clone, Debug)]
enum Body {
    Loading,
    /// Numbered lines, and the one to highlight.
    Code { lines: Vec<(u32, String)>, focus: Option<u32> },
    /// A dim one-liner: a binary or empty file, an error.
    Note(String),
    Image(PathBuf),
    /// (name, is a folder, size), the first `DIR_ROWS`, and the count of all entries.
    Dir { rows: Vec<(String, bool, Option<u64>)>, total: usize },
    Pr(Pr),
    Web { title: Option<String>, desc: Option<String> },
}

struct Card {
    link: Link,
    is_dir: bool,
    /// The project folder holding the path (longest match), for remembering an IDE.
    project: Option<String>,
    name: String,
    sub: String,
    body: Body,
    /// The editor the IDE rules pick for this path, and whether the user chose it.
    ide: Option<(Ide, bool)>,
    _load: Task<()>,
}

#[derive(Default)]
pub(super) struct Preview {
    /// The link under the pointer.
    hot: Option<Link>,
    /// Waiting out `SHOW_DELAY` for `hot`.
    pending: Option<Task<()>>,
    card: Option<Card>,
    over: bool,
    menu: bool,
    /// When a click outside closed the menu (so the chevron's own click doesn't reopen it).
    menu_closed: Option<Instant>,
    /// The grace period running (its number names the bar's animation).
    grace: Option<(u64, Task<()>)>,
    seq: u64,
    /// A short note in the footer ("Copied"), and whether it's an error.
    flash: Option<(String, bool)>,
    flash_task: Option<Task<()>>,
}

impl TerminalView {
    /// The pointer is on `link` (`None`: on no link, or off the grid).
    pub(super) fn preview_hover(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
        let link = link.filter(|_| mode(cx) != Mode::Off);
        if link == self.preview.hot {
            return;
        }
        self.preview.hot = link.clone();
        self.preview.pending = None;
        let Some(link) = link else {
            self.preview_grace(cx);
            return;
        };
        if self.preview.card.as_ref().is_some_and(|c| c.link == link) {
            self.preview_hold(cx);
            return;
        }
        self.preview.pending = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SHOW_DELAY).await;
            let _ = this.update(cx, |t, cx| {
                if t.preview.hot.as_ref() == Some(&link) {
                    t.preview_show(link, cx);
                }
            });
        }));
    }

    fn preview_show(&mut self, link: Link, cx: &mut Context<Self>) {
        let env = env(cx);
        let path = Path::new(&link.target);
        let is_dir = !link.url && path.is_dir();
        let project = (!link.url).then(|| env.projects.iter().filter(|p| path.starts_with(p)).max_by_key(|p| p.len()).cloned()).flatten();
        let (name, sub) = if link.url { url_title(&link.target) } else { path_title(path, is_dir, project.as_deref()) };
        let ide = (!link.url).then(|| self.preview_ide(&env, path, is_dir, project.as_deref())).flatten();
        let l = link.clone();
        let load = cx.spawn(async move |this, cx| {
            let what = l.clone();
            let body = cx.background_executor().spawn(async move { load(&what) }).await;
            let _ = this.update(cx, |t, cx| {
                if let Some(c) = t.preview.card.as_mut().filter(|c| c.link == l) {
                    c.body = body;
                    cx.notify();
                }
            });
        });
        self.preview.grace = None;
        self.preview.menu = false;
        self.preview.flash = None;
        self.preview.card = Some(Card { link, is_dir, project, name, sub, body: Body::Loading, ide, _load: load });
        cx.notify();
    }

    fn preview_ide(&self, env: &PreviewEnv, path: &Path, is_dir: bool, project: Option<&str>) -> Option<(Ide, bool)> {
        let dir = if is_dir { path } else { path.parent()? };
        let dirs: Vec<&Path> = std::iter::once(dir).chain(project.map(Path::new).filter(|p| *p != dir)).collect();
        let (ide, source) = ide::choose_for(&env.ides, env.ide_app.as_deref(), &env.ide_rules, &dirs)?;
        let remembered = ide::is_remembered(&source, env.ide_app.as_deref());
        Some((ide, remembered))
    }

    /// The pointer came back (to the link or the card): stop the grace period.
    fn preview_hold(&mut self, cx: &mut Context<Self>) {
        if self.preview.grace.take().is_some() {
            cx.notify();
        }
    }

    /// Start the grace period, unless something still holds the card open.
    fn preview_grace(&mut self, cx: &mut Context<Self>) {
        let p = &mut self.preview;
        if p.card.is_none() || p.over || p.menu || p.hot.is_some() || p.grace.is_some() {
            return;
        }
        p.seq += 1;
        let seq = p.seq;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(GRACE).await;
            let _ = this.update(cx, |t, cx| {
                if t.preview.grace.as_ref().is_some_and(|(s, _)| *s == seq) {
                    if let Some((_, me)) = t.preview.grace.take() {
                        me.detach();
                    }
                    t.preview_close(cx);
                }
            });
        });
        p.grace = Some((seq, task));
        cx.notify();
    }

    pub(super) fn preview_close(&mut self, cx: &mut Context<Self>) {
        self.preview.pending = None;
        if self.preview.card.is_none() {
            return;
        }
        let p = &mut self.preview;
        p.card = None;
        p.grace = None;
        p.menu = false;
        p.over = false;
        p.flash = None;
        p.flash_task = None;
        cx.notify();
    }

    fn preview_flash(&mut self, text: impl Into<String>, err: bool, cx: &mut Context<Self>) {
        self.preview.flash = Some((text.into(), err));
        self.preview.flash_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(if err { 4000 } else { 1600 })).await;
            let _ = this.update(cx, |t, cx| {
                t.preview.flash = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    // ------------------------------------------------------------------ actions

    fn card_link(&self) -> Option<Link> {
        self.preview.card.as_ref().map(|c| c.link.clone())
    }

    /// Open the card's path in its IDE, or list the editors when none was chosen yet.
    fn preview_open_ide(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.preview.card.as_ref() else { return };
        match c.ide.clone() {
            Some((ide, true)) => self.preview_open_in(ide, cx),
            Some(_) => self.preview_toggle_menu(cx),
            None => {
                // No editor found: the $EDITOR route ⌘-click uses.
                let l = c.link.clone();
                super::open_file(self.backend.clone(), self.session_id.clone(), l.target, l.line);
                self.preview_close(cx);
            }
        }
    }

    fn preview_open_in(&mut self, ide: Ide, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
            eprintln!("midna-app debug-term: open {} in {}", l.target, ide.name);
            self.preview_flash(format!("Opened in {}", ide.name), false, cx);
            return;
        }
        let name = ide.name.clone();
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { ide::open_file(&ide, &l.target, l.line) }).await;
            let _ = this.update(cx, |t, cx| match res {
                Ok(()) => t.preview_close(cx),
                Err(e) => t.preview_flash(format!("Couldn't open {name}: {e}"), true, cx),
            });
        })
        .detach();
    }

    /// A row in the IDE menu: remember it for the project (⌥: everywhere), then open there.
    fn preview_pick(&mut self, ide: Ide, everywhere: bool, cx: &mut Context<Self>) {
        let env = env(cx);
        let Some(c) = self.preview.card.as_mut() else { return };
        let path = Path::new(&c.link.target);
        let dir = if c.is_dir { Some(path) } else { path.parent() };
        let dirs: Vec<&Path> = dir.into_iter().chain(c.project.as_deref().map(Path::new)).collect();
        let changes = ide::remember_settings(&env.ides, env.ide_app.as_deref(), &env.ide_rules, c.project.as_deref().map(Path::new), &dirs, &ide, everywhere);
        c.ide = Some((ide.clone(), true));
        self.preview.menu = false;
        let backend = self.backend.clone();
        cx.background_executor()
            .spawn(async move {
                for (key, value) in changes {
                    let _ = backend.call("settings.set", json!({ "key": key, "value": value }));
                }
            })
            .detach();
        self.preview_open_in(ide, cx);
    }

    fn preview_toggle_menu(&mut self, cx: &mut Context<Self>) {
        let just_closed = self.preview.menu_closed.take().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        self.preview.menu = !self.preview.menu && !just_closed;
        if !self.preview.menu {
            self.preview_grace(cx);
        }
        cx.notify();
    }

    fn preview_reveal(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_none() {
            let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(&l.target).spawn();
        }
        self.preview_flash("Revealed in Finder", false, cx);
    }

    fn preview_copy(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_none() {
            cx.write_to_clipboard(ClipboardItem::new_string(l.target));
        }
        self.preview_flash("Copied", false, cx);
    }

    fn preview_path_click(&mut self, cx: &mut Context<Self>) {
        match env(cx).path_click {
            PathClick::Reveal => self.preview_reveal(cx),
            PathClick::Copy => self.preview_copy(cx),
            PathClick::Ide => self.preview_open_ide(cx),
        }
    }

    /// A folder: a new shell there, in this terminal's project.
    fn preview_new_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        let (backend, session) = (self.backend.clone(), self.session_id.clone());
        self.preview_close(cx);
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
            eprintln!("midna-app debug-term: new terminal in {}", l.target);
            return;
        }
        cx.background_executor()
            .spawn(async move {
                let s = backend.call("session.get", json!({ "id": session })).unwrap_or(Value::Null);
                let mut p = json!({ "kind": "shell", "cwd": l.target });
                if let Some(v) = s.get("project_id").filter(|v| v.is_string()) {
                    p["project_id"] = v.clone();
                }
                if let Ok(opened) = backend.call("session.open", p)
                    && let Some(id) = opened.get("id").and_then(Value::as_str)
                {
                    let _ = backend.call("session.focus", json!({ "id": id }));
                }
            })
            .detach();
    }

    fn preview_browse(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        if crate::dev::var_os("MIDNA_DEBUG_TERM").is_some() {
            eprintln!("midna-app debug-term: link {}", l.target);
        } else {
            cx.open_url(&l.target);
        }
        self.preview_close(cx);
    }

    fn preview_add_link(&mut self, cx: &mut Context<Self>) {
        let Some(l) = self.card_link() else { return };
        let (backend, session) = (self.backend.clone(), self.session_id.clone());
        cx.spawn(async move |this, cx| {
            let res = cx.background_executor().spawn(async move { backend.call("links.add", json!({ "session": session, "target": l.target })) }).await;
            let _ = this.update(cx, |t, cx| match res {
                Ok(_) => t.preview_flash("Added to links", false, cx),
                Err(e) => t.preview_flash(format!("Couldn't add it: {e}"), true, cx),
            });
        })
        .detach();
    }

    // ------------------------------------------------------------------ card

    /// `pill`: the queue pill is showing; `chip`: the "↓ N lines below" chip is.
    pub(super) fn render_link_preview(&self, t: &Theme, pill: bool, chip: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = self.preview.card.as_ref()?;
        let b = self.bounds.get()?;
        let pane_h = f32::from(b.size.height);
        let env = env(cx);

        // Above the input box when it's on screen (and above the queue pill), else the corner.
        let lines: Vec<String> = if self.ext.at_bottom() { self.grid.iter().map(|r| r.text()).collect() } else { vec![] };
        let mut edge = match super::queue_pill::input_box_top(&lines).map(|r| PAD_Y + r as f32 * LINE_H - 6.).filter(|y| *y > PAD_Y + 80.) {
            Some(y) => y,
            None => pane_h - if chip { 46. } else { 10. },
        };
        if pill {
            edge -= super::queue_pill::PILL_H + 8.;
        }
        let held = self.preview.over || self.preview.menu;

        let bar = match &self.preview.grace {
            Some((seq, _)) if !crate::ui::queue::reduce_motion() => div()
                .h_full()
                .rounded_full()
                .bg(t.accent)
                .with_animation(SharedString::from(format!("link-preview-grace-{seq}")), Animation::new(GRACE), |el, d| el.w(relative(1. - d)))
                .into_any_element(),
            Some(_) => div().h_full().w(relative(0.4)).rounded_full().bg(t.accent).into_any_element(),
            None => div().h_full().w_full().rounded_full().when(held, |d| d.bg(t.accent)).into_any_element(),
        };
        let bar = div().mx(px(10.)).mt(px(4.)).h(px(2.)).rounded_full().bg(t.line).flex().child(bar);

        let icon = if c.link.url {
            if matches!(c.body, Body::Pr(_)) { Icon::Pr.el(14., t.ok) } else { Icon::Globe.el(14., t.dim) }
        } else if c.is_dir {
            Icon::Project.el(14., t.dim)
        } else {
            Icon::File.el(14., t.dim)
        };
        let sub = match (&c.body, c.is_dir) {
            (Body::Dir { total, .. }, true) => format!("{}{}{total} item{}", c.sub, if c.sub.is_empty() { "" } else { " · " }, if *total == 1 { "" } else { "s" }),
            _ => c.sub.clone(),
        };
        let title = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(div().flex_none().font_weight(FontWeight::BOLD).whitespace_nowrap().child(c.name.clone()))
            .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(sub));
        let title = if c.link.url {
            div().flex_1().min_w_0().child(title).into_any_element()
        } else {
            let tip = match env.path_click {
                PathClick::Reveal => "Reveal in Finder".to_string(),
                PathClick::Copy => "Copy path".to_string(),
                PathClick::Ide => c.ide.as_ref().map(|(i, _)| format!("Open in {}", i.name)).unwrap_or_else(|| "Open".into()),
            };
            div()
                .id("lp-path")
                .flex_1()
                .min_w_0()
                .px(px(4.))
                .py(px(2.))
                .ml(px(-4.))
                .rounded(px(5.))
                .cursor_pointer()
                .hover(|s| s.bg(t.panel))
                .tooltip(crate::ui::header::tip(tip))
                .on_click(cx.listener(|t, _, _, cx| t.preview_path_click(cx)))
                .child(title)
                .into_any_element()
        };
        let close = div()
            .id("lp-close")
            .flex_none()
            .size(px(26.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .cursor_pointer()
            .hover(|s| s.bg(t.panel))
            .tooltip(crate::ui::header::tip("Close"))
            .on_click(cx.listener(|t, _, _, cx| t.preview_close(cx)))
            .child(Icon::Cross.el(11., t.dim));
        let header = div().flex().items_center().gap(px(8.)).pl(px(14.)).pr(px(8.)).pt(px(6.)).pb(px(6.)).border_b_1().border_color(t.line).child(icon).child(title).child(close);

        let body = render_body(&c.body, &c.link, t);
        let footer = self.render_footer(c, &env, t, cx);

        let card = div()
            .id("link-preview")
            .occlude()
            .absolute()
            .right(px(RIGHT))
            .bottom(px(pane_h - edge))
            .min_w(px(400.))
            .max_w(px(600.).min(b.size.width - px(2. * RIGHT)))
            .max_h(px((edge - 12.).max(120.)))
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(if held { t.accent } else { t.line })
            .bg(t.raised)
            .opacity(if self.preview.grace.is_some() { 0.9 } else { 1. })
            .font_family(t.ui_font.clone())
            .text_size(px(13.))
            .text_color(t.fg)
            .cursor(CursorStyle::Arrow)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.4), offset: point(px(0.), px(14.)), blur_radius: px(36.), spread_radius: px(0.), inset: false }])
            .on_hover(cx.listener(|t, over: &bool, _, cx| {
                t.preview.over = *over;
                if *over { t.preview_hold(cx) } else { t.preview_grace(cx) }
                cx.notify();
            }))
            // The terminal under it must not start a selection or take the click.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(bar)
            .child(header)
            .child(body)
            .child(footer);
        Some(card.into_any_element())
    }

    fn render_footer(&self, c: &Card, env: &PreviewEnv, t: &Theme, cx: &mut Context<Self>) -> Div {
        let primary = |id: &'static str| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(6.))
                .bg(t.accent)
                .text_color(t.accent_fg)
                .font_weight(FontWeight::MEDIUM)
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
        };
        let secondary = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .h(px(28.))
                .px(px(10.))
                .flex()
                .items_center()
                .rounded(px(6.))
                .border_1()
                .border_color(t.line)
                .bg(t.panel)
                .whitespace_nowrap()
                .cursor_pointer()
                .hover(|s| s.bg(t.term))
                .child(label)
        };
        let chevron = |d: Stateful<Div>| d.child(Icon::Chevron.el(10., t.accent_fg));

        let mut row = div().flex().items_center().gap(px(6.)).px(px(10.)).py(px(8.)).border_t_1().border_color(t.line);
        if c.link.url {
            row = row
                .child(primary("lp-browse").on_click(cx.listener(|t, _, _, cx| t.preview_browse(cx))).child("Open in browser"))
                .child(secondary("lp-copy", "Copy URL").on_click(cx.listener(|t, _, _, cx| t.preview_copy(cx))))
                .child(secondary("lp-add", "Add to links").on_click(cx.listener(|t, _, _, cx| t.preview_add_link(cx))));
        } else {
            let open = if c.is_dir {
                primary("lp-new-term").on_click(cx.listener(|t, _, _, cx| t.preview_new_terminal(cx))).child("New terminal here").into_any_element()
            } else {
                let menu = self.preview.menu.then(|| self.render_ide_menu(c, env, t, cx));
                let button = match &c.ide {
                    Some((ide, true)) => div()
                        .flex()
                        .child(
                            primary("lp-ide")
                                .rounded_r(px(0.))
                                .pl(px(6.))
                                .on_click(cx.listener(|t, _, _, cx| t.preview_open_ide(cx)))
                                .child(app_icon(Some(ide), 18., t))
                                .child(format!("Open in {}", ide.name)),
                        )
                        .child(
                            chevron(primary("lp-ide-menu").rounded_l(px(0.)).px(px(7.)).border_l_1().border_color(hsla(0., 0., 0., 0.22)))
                                .tooltip(crate::ui::header::tip("Choose a different IDE"))
                                .on_click(cx.listener(|t, _, _, cx| t.preview_toggle_menu(cx))),
                        )
                        .into_any_element(),
                    Some(_) => chevron(primary("lp-ide").on_click(cx.listener(|t, _, _, cx| t.preview_toggle_menu(cx))).child("Open in IDE")).into_any_element(),
                    None => primary("lp-ide").on_click(cx.listener(|t, _, _, cx| t.preview_open_ide(cx))).child("Open").into_any_element(),
                };
                div().relative().flex().child(button).children(menu).into_any_element()
            };
            row = row.child(open);
            if env.path_click != PathClick::Copy {
                row = row.child(secondary("lp-copy", "Copy path").on_click(cx.listener(|t, _, _, cx| t.preview_copy(cx))));
            }
            if env.path_click != PathClick::Reveal {
                row = row.child(secondary("lp-reveal", "Reveal").on_click(cx.listener(|t, _, _, cx| t.preview_reveal(cx))));
            }
        }
        row = row.child(div().flex_1().min_w(px(8.)));
        if let Some((text, err)) = &self.preview.flash {
            row = row.child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().text_size(px(11.5)).text_color(if *err { t.err } else { t.ok }).child(text.clone()));
        }
        row
    }

    /// The editors, opening upward from the IDE button.
    fn render_ide_menu(&self, c: &Card, env: &PreviewEnv, t: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let cur = c.ide.as_ref().filter(|(_, r)| *r).map(|(i, _)| i.id.clone());
        let mut list = div()
            .id("lp-ide-list")
            .occlude()
            .absolute()
            .left_0()
            .bottom(px(34.))
            .w(px(250.))
            .max_h(px(360.))
            .overflow_y_scroll()
            .p(px(6.))
            .flex()
            .flex_col()
            .rounded(px(9.))
            .border_1()
            .border_color(t.line)
            .bg(t.panel)
            .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(10.)), blur_radius: px(28.), spread_radius: px(0.), inset: false }])
            .on_mouse_down_out(cx.listener(|t, _, _, cx| {
                if t.preview.menu {
                    t.preview.menu = false;
                    t.preview.menu_closed = Some(Instant::now());
                    t.preview_grace(cx);
                    cx.notify();
                }
            }));
        for (i, ide) in env.ides.iter().enumerate() {
            let on = cur.as_deref() == Some(ide.id.as_str());
            let pick = ide.clone();
            list = list.child(
                div()
                    .id(("lp-ide-row", i))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(px(32.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(on, |d| d.bg(t.accent_soft))
                    .hover(|s| s.bg(t.raised))
                    .on_click(cx.listener(move |t, ev: &ClickEvent, _, cx| t.preview_pick(pick.clone(), ev.modifiers().alt, cx)))
                    .child(app_icon(Some(ide), 20., t))
                    .child(div().flex_1().whitespace_nowrap().child(ide.name.clone()))
                    .when(on, |d| d.child(Icon::Check.el(12., t.accent))),
            );
        }
        list.into_any_element()
    }
}

fn app_icon(ide: Option<&Ide>, size: f32, t: &Theme) -> AnyElement {
    match ide.and_then(|i| i.icon.clone()) {
        Some(p) => img(p).size(px(size)).flex_none().into_any_element(),
        None => Icon::Code.el(size - 2., t.dim).into_any_element(),
    }
}

fn render_body(body: &Body, link: &Link, t: &Theme) -> AnyElement {
    let note = |text: String| div().px(px(14.)).py(px(12.)).text_color(t.dim).child(text).into_any_element();
    match body {
        Body::Loading => note("Loading…".into()),
        Body::Note(s) => note(s.clone()),
        Body::Code { lines, focus } => {
            let mut col = div().flex().flex_col().min_w_full();
            for (n, text) in lines {
                let on = Some(*n) == *focus;
                col = col.child(
                    div()
                        .flex()
                        .min_w_full()
                        .when(on, |d| d.bg(t.accent_soft))
                        .child(div().flex_none().w(px(46.)).pr(px(12.)).text_right().text_color(if on { t.accent } else { t.dim }).child(n.to_string()))
                        .child(div().flex_none().pr(px(14.)).whitespace_nowrap().child(text.clone())),
                );
            }
            div().id("lp-code").overflow_x_scroll().bg(t.term).py(px(8.)).font_family(t.mono_font.clone()).text_size(px(11.5)).line_height(px(18.)).child(col).into_any_element()
        }
        Body::Image(p) => div().p(px(10.)).flex().justify_center().bg(t.term).child(img(p.clone()).max_h(px(240.)).max_w(px(560.)).object_fit(ObjectFit::Contain)).into_any_element(),
        Body::Dir { rows, total } => {
            let mut col = div().flex().flex_col().py(px(6.)).font_family(t.mono_font.clone()).text_size(px(12.)).line_height(px(22.));
            for (name, dir, size) in rows {
                col = col.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(16.))
                        .px(px(14.))
                        .child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(if *dir { format!("{name}/") } else { name.clone() }))
                        .child(div().flex_none().text_color(t.dim).child(size.map(human_size).unwrap_or_default())),
                );
            }
            if *total > rows.len() {
                col = col.child(div().px(px(14.)).text_color(t.dim).child(format!("{} more", total - rows.len())));
            }
            if *total == 0 {
                col = col.child(div().px(px(14.)).text_color(t.dim).child("Empty folder"));
            }
            col.into_any_element()
        }
        Body::Pr(pr) => {
            let (label, bg) = match pr.state.as_str() {
                "MERGED" => ("Merged", t.accent),
                "CLOSED" => ("Closed", t.err),
                "DRAFT" => ("Draft", t.dim),
                _ => ("Open", t.ok),
            };
            let mono = |s: String| div().font_family(t.mono_font.clone()).child(s);
            let mut checks = div().flex().flex_wrap().gap_x(px(14.)).gap_y(px(4.)).font_family(t.mono_font.clone()).text_size(px(11.5));
            for (name, st) in pr.checks.iter().take(CHECKS) {
                let (mark, color) = match st {
                    Check::Pass => ("✓", t.ok),
                    Check::Fail => ("✗", t.err),
                    Check::Pending => ("●", t.need),
                };
                checks = checks.child(div().flex().gap(px(5.)).child(div().text_color(color).child(mark)).child(name.clone()));
            }
            if pr.checks.len() > CHECKS {
                checks = checks.child(div().text_color(t.dim).child(format!("+{} more", pr.checks.len() - CHECKS)));
            }
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(14.))
                .py(px(12.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).line_height(px(20.)).child(pr.title.clone()))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(10.))
                        .text_size(px(12.))
                        .text_color(t.dim)
                        .child(div().px(px(8.)).rounded_full().bg(bg).text_color(t.term).font_weight(FontWeight::BOLD).child(label))
                        .child(mono(format!("{} → {}", pr.head, pr.base)))
                        .child(div().flex().gap(px(6.)).font_family(t.mono_font.clone()).child(div().text_color(t.ok).child(format!("+{}", pr.additions))).child(div().text_color(t.err).child(format!("−{}", pr.deletions)))),
                )
                .when(!pr.checks.is_empty(), |d| d.child(checks))
                .into_any_element()
        }
        Body::Web { title, desc } => div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .px(px(14.))
            .py(px(12.))
            .when_some(title.clone(), |d, s| d.child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).line_height(px(20.)).child(s)))
            .when_some(desc.clone(), |d, s| d.child(div().text_color(t.dim).child(s)))
            .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(link.target.clone()))
            .into_any_element(),
    }
}

// ------------------------------------------------------------------ titles

/// (name, where): `terminal.rs`, `crates/midna-app/src` (inside its project) or `~/notes`.
fn path_title(path: &Path, is_dir: bool, project: Option<&str>) -> (String, String) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let name = if is_dir { format!("{name}/") } else { name };
    let parent = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let sub = match project.and_then(|p| Path::new(&parent).strip_prefix(p).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => project.and_then(|p| Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        Some(rel) => rel.to_string_lossy().into_owned(),
        None => crate::commands::tilde(&parent),
    };
    (name, sub)
}

/// (name, where): `#42`, `owner/repo` for a pull request, else the host and the rest.
fn url_title(url: &str) -> (String, String) {
    if let Some((owner, repo, n)) = github_pr(url) {
        return (format!("#{n}"), format!("{owner}/{repo}"));
    }
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.rsplit('@').next().unwrap_or(host);
    (host.trim_start_matches("www.").to_string(), path.trim_end_matches('/').to_string())
}

/// `https://github.com/<owner>/<repo>/pull/<n>…`.
fn github_pr(url: &str) -> Option<(String, String, u64)> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let mut seg = rest.strip_prefix("github.com/")?.split(['/', '?', '#']);
    let (owner, repo, pull, n) = (seg.next()?, seg.next()?, seg.next()?, seg.next()?);
    (pull == "pull" && !owner.is_empty() && !repo.is_empty()).then_some(())?;
    Some((owner.to_string(), repo.to_string(), n.parse().ok()?))
}

pub(super) fn human_size(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.0} KB", n as f64 / 1024.),
        n if n < 1024 * 1024 * 1024 => format!("{:.1} MB", n as f64 / (1024. * 1024.)),
        n => format!("{:.1} GB", n as f64 / (1024. * 1024. * 1024.)),
    }
}

// ------------------------------------------------------------------ loading (off the main thread)

fn load(link: &Link) -> Body {
    if link.url { load_url(&link.target) } else { load_path(Path::new(&link.target), link.line) }
}

fn load_path(path: &Path, line: Option<u32>) -> Body {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Body::Note(format!("Couldn't read it: {e}")),
    };
    if meta.is_dir() {
        return load_dir(path);
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return Body::Image(path.to_path_buf());
    }
    if meta.len() == 0 {
        return Body::Note("Empty file".into());
    }
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return Body::Note(format!("Couldn't read it: {e}")),
    };
    let mut head = vec![0u8; 8192];
    let n = f.read(&mut head).unwrap_or(0);
    if head[..n].contains(&0) {
        return Body::Note(format!("Binary file · {}", human_size(meta.len())));
    }
    match std::fs::File::open(path) {
        Ok(f) => {
            let (lines, focus) = excerpt(std::io::BufReader::new(f), line);
            if lines.is_empty() { Body::Note("Empty file".into()) } else { Body::Code { lines, focus } }
        }
        Err(e) => Body::Note(format!("Couldn't read it: {e}")),
    }
}

/// `FILE_LINES` numbered lines around `line` (the top of the file without one), tabs expanded
/// and very long lines cut. Reads no further than it needs (and gives up 64 MB in).
fn excerpt(mut r: impl BufRead, line: Option<u32>) -> (Vec<(u32, String)>, Option<u32>) {
    let start = line.map(|l| (l as usize).saturating_sub(1 + BEFORE)).unwrap_or(0);
    let mut out = vec![];
    let (mut n, mut read) = (0usize, 0usize);
    let mut buf = vec![];
    while out.len() < FILE_LINES && read < 64 << 20 {
        buf.clear();
        match r.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(k) => read += k,
        }
        n += 1;
        if n > start {
            let s = String::from_utf8_lossy(&buf);
            let s = s.trim_end_matches(['\n', '\r']).replace('\t', "    ");
            let s: String = s.chars().take(500).collect();
            out.push((n as u32, s));
        }
    }
    // A line past the end: show the file's last lines instead of nothing.
    let focus = line.filter(|l| out.iter().any(|(n, _)| n == l));
    (out, focus)
}

fn load_dir(path: &Path) -> Body {
    let rd = match std::fs::read_dir(path) {
        Ok(r) => r,
        Err(e) => return Body::Note(format!("Couldn't list it: {e}")),
    };
    let mut all: Vec<(String, bool, Option<u64>)> = rd
        .flatten()
        .filter(|e| e.file_name() != ".DS_Store")
        .map(|e| {
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            let size = (!dir).then(|| e.metadata().ok().map(|m| m.len())).flatten();
            (e.file_name().to_string_lossy().into_owned(), dir, size)
        })
        .collect();
    all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    let total = all.len();
    all.truncate(DIR_ROWS);
    Body::Dir { rows: all, total }
}

static URL_CACHE: LazyLock<Mutex<HashMap<String, (Instant, Body)>>> = LazyLock::new(Default::default);

fn load_url(url: &str) -> Body {
    if let Some((at, b)) = URL_CACHE.lock().ok().and_then(|c| c.get(url).cloned())
        && at.elapsed() < URL_TTL
    {
        return b;
    }
    let body = github_pr(url).and_then(|(o, r, n)| load_pr(&o, &r, n)).map(Body::Pr).unwrap_or_else(|| load_page(url));
    if let Ok(mut c) = URL_CACHE.lock() {
        c.retain(|_, (at, _)| at.elapsed() < URL_TTL);
        c.insert(url.to_string(), (Instant::now(), body.clone()));
    }
    body
}

/// Run `cmd`, keeping at most `limit` bytes of its output; killed after `timeout`.
fn run_capped(mut cmd: std::process::Command, limit: usize, timeout: Duration) -> Option<(bool, Vec<u8>)> {
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = vec![];
        let _ = (&mut out).take(limit as u64).read_to_end(&mut buf);
        buf // dropping the pipe here stops a writer that has more
    });
    let until = Instant::now() + timeout;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    Some((ok, reader.join().ok()?))
}

fn gh() -> std::process::Command {
    let bin = ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"].into_iter().find(|p| Path::new(p).exists()).unwrap_or("gh");
    std::process::Command::new(bin)
}

fn load_pr(owner: &str, repo: &str, n: u64) -> Option<Pr> {
    let mut cmd = gh();
    cmd.args(["pr", "view", &n.to_string(), "-R", &format!("{owner}/{repo}"), "--json", "title,state,isDraft,headRefName,baseRefName,additions,deletions,statusCheckRollup"]);
    let (ok, out) = run_capped(cmd, 1 << 20, Duration::from_secs(8))?;
    if !ok {
        return None;
    }
    parse_pr(&serde_json::from_slice(&out).ok()?)
}

fn parse_pr(v: &Value) -> Option<Pr> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let state = if s("state") == "OPEN" && v.get("isDraft").and_then(Value::as_bool) == Some(true) { "DRAFT".into() } else { s("state") };
    let mut checks: Vec<(String, Check)> = vec![];
    for c in v.get("statusCheckRollup").and_then(Value::as_array).into_iter().flatten() {
        let g = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_uppercase();
        let name = c.get("name").or_else(|| c.get("context")).and_then(Value::as_str).unwrap_or("check").to_string();
        // A check run has a status and (once done) a conclusion; a status context, a state.
        let result = if c.get("conclusion").is_some() || c.get("status").is_some() { if g("status") == "COMPLETED" { g("conclusion") } else { "PENDING".into() } } else { g("state") };
        let st = match result.as_str() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => Check::Pass,
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => Check::Fail,
            _ => Check::Pending,
        };
        if !checks.iter().any(|(n, _)| *n == name) {
            checks.push((name, st));
        }
    }
    // Failures first, then the running ones.
    checks.sort_by_key(|(_, s)| match s {
        Check::Fail => 0,
        Check::Pending => 1,
        Check::Pass => 2,
    });
    let num = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(Pr { title: s("title"), state, head: s("headRefName"), base: s("baseRefName"), additions: num("additions"), deletions: num("deletions"), checks })
}

/// Hosts never fetched: this Mac, the local network, and anything that isn't http(s).
fn fetchable(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://")) else { return false };
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = auth.rsplit('@').next().unwrap_or(auth);
    let host = if let Some(h) = host.strip_prefix('[') { h.split(']').next().unwrap_or("") } else { host.split(':').next().unwrap_or("") }.to_lowercase();
    if host.is_empty() || host == "localhost" || !host.contains('.') && host.parse::<std::net::IpAddr>().is_err() {
        return false;
    }
    if [".local", ".localhost", ".internal", ".lan", ".home.arpa"].iter().any(|s| host.ends_with(s)) {
        return false;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => !(ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified() || ip.octets()[0] == 100 && ip.octets()[1] & 0xC0 == 64),
        Ok(std::net::IpAddr::V6(ip)) => !(ip.is_loopback() || ip.is_unspecified() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80),
        Err(_) => true,
    }
}

fn load_page(url: &str) -> Body {
    if !fetchable(url) {
        return Body::Web { title: None, desc: None };
    }
    let mut cmd = std::process::Command::new("/usr/bin/curl");
    cmd.args(["-sL", "--max-time", "6", "--max-redirs", "4", "--proto", "=http,https", "--proto-redir", "=http,https", "-A", "Mozilla/5.0 (Macintosh) midna-link-preview", "-H", "Accept: text/html"]).arg(url);
    let Some((_, out)) = run_capped(cmd, 512 << 10, Duration::from_secs(7)) else {
        return Body::Web { title: None, desc: None };
    };
    let (title, desc) = page_meta(&String::from_utf8_lossy(&out));
    Body::Web { title, desc }
}

/// The page's title and description: Open Graph first, then `<title>` and `<meta name=description>`.
fn page_meta(html: &str) -> (Option<String>, Option<String>) {
    let lower = html.to_ascii_lowercase();
    let mut og_title = None;
    let mut desc = None;
    let mut og_desc = None;
    let mut i = 0;
    while let Some(at) = lower[i..].find("<meta") {
        let start = i + at;
        let end = lower[start..].find('>').map(|e| start + e).unwrap_or(lower.len());
        let tag = &html[start..end];
        let key = attr(tag, "property").or_else(|| attr(tag, "name")).map(|k| k.to_ascii_lowercase());
        if let (Some(k), Some(v)) = (key, attr(tag, "content")) {
            match k.as_str() {
                "og:title" | "twitter:title" => og_title = og_title.or(Some(v)),
                "og:description" | "twitter:description" => og_desc = og_desc.or(Some(v)),
                "description" => desc = desc.or(Some(v)),
                _ => {}
            }
        }
        i = end;
    }
    let title = lower.find("<title").and_then(|s| {
        let open = s + lower[s..].find('>')? + 1;
        let close = open + lower[open..].find("</title")?;
        Some(html[open..close].to_string())
    });
    let clean = |s: String| {
        let s = decode(&s).split_whitespace().collect::<Vec<_>>().join(" ");
        let s: String = if s.chars().count() > 240 { s.chars().take(239).collect::<String>() + "…" } else { s };
        (!s.is_empty()).then_some(s)
    };
    (og_title.and_then(clean).or_else(|| title.and_then(clean)), og_desc.and_then(clean).or_else(|| desc.and_then(clean)))
}

/// An attribute's value in one tag (quoted or not).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find(name) {
        let at = from + p;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let vstart = tag.len() - rest.len() + 1;
        let v = tag[vstart..].trim_start();
        return Some(match v.chars().next() {
            Some(q @ ('"' | '\'')) => v[1..].split(q).next().unwrap_or("").to_string(),
            _ => v.split(|c: char| c.is_whitespace() || c == '/').next().unwrap_or("").to_string(),
        });
    }
    None
}

fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest.find(';').filter(|e| *e <= 10);
        let ent = end.map(|e| &rest[1..e]);
        let ch = match ent {
            Some("amp") => Some('&'),
            Some("lt") => Some('<'),
            Some("gt") => Some('>'),
            Some("quot") => Some('"'),
            Some("apos") => Some('\''),
            Some("nbsp") => Some(' '),
            Some(e) if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            Some(e) if e.starts_with('#') => e[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match (ch, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{Check, FILE_LINES, excerpt, fetchable, github_pr, page_meta, parse_pr, path_title, url_title};
    use std::path::Path;

    #[test]
    fn excerpt_centers_on_the_line() {
        let text: String = (1..=40).map(|n| format!("line {n}\n")).collect();
        let (lines, focus) = excerpt(text.as_bytes(), Some(20));
        assert_eq!(lines.first().map(|l| l.0), Some(16));
        assert_eq!(lines.len(), FILE_LINES);
        assert_eq!(focus, Some(20));
        let (lines, focus) = excerpt(text.as_bytes(), None);
        assert_eq!((lines[0].0, focus), (1, None));
        // Near the top, the window starts at line 1.
        assert_eq!(excerpt(text.as_bytes(), Some(2)).0[0].0, 1);
        // Tabs expand; CRLF goes.
        assert_eq!(excerpt("\ta\r\n".as_bytes(), Some(1)).0[0].1, "    a");
    }

    #[test]
    fn github_pull_requests() {
        assert_eq!(github_pr("https://github.com/mrgnhnt96/midna/pull/42"), Some(("mrgnhnt96".into(), "midna".into(), 42)));
        assert_eq!(github_pr("https://github.com/a/b/pull/7/files#diff"), Some(("a".into(), "b".into(), 7)));
        assert_eq!(github_pr("https://github.com/a/b/issues/7"), None);
        assert_eq!(url_title("https://www.docs.rs/gpui/latest/gpui/"), ("docs.rs".into(), "gpui/latest/gpui".into()));
        assert_eq!(url_title("https://github.com/a/b/pull/7"), ("#7".into(), "a/b".into()));
    }

    #[test]
    fn local_hosts_are_never_fetched() {
        for u in ["http://localhost:3000/x", "http://127.0.0.1/", "https://192.168.1.4/", "http://10.0.0.2", "http://[::1]:8080/", "https://printer.local/", "http://intranet/", "ftp://example.com/", "http://100.101.1.2/"] {
            assert!(!fetchable(u), "{u}");
        }
        for u in ["https://docs.rs/gpui", "http://example.com:8080/a?b", "https://8.8.8.8/"] {
            assert!(fetchable(u), "{u}");
        }
    }

    #[test]
    fn page_meta_prefers_open_graph() {
        let html = r#"<html><head><title> Plain &amp; simple </title><meta name="description" content="Desc"><meta property="og:title" content="OG &#39;title&#39;"/></head>"#;
        assert_eq!(page_meta(html), (Some("OG 'title'".into()), Some("Desc".into())));
        assert_eq!(page_meta("<TITLE>Only\n title</TITLE>"), (Some("Only title".into()), None));
        // An empty Open Graph title doesn't hide the page's own.
        assert_eq!(page_meta(r#"<meta property="og:title" content=" "><title>Real</title>"#).0, Some("Real".into()));
    }

    #[test]
    fn pr_checks_from_runs_and_contexts() {
        let v = serde_json::json!({
            "title": "T", "state": "OPEN", "isDraft": true, "headRefName": "h", "baseRefName": "main", "additions": 3, "deletions": 1,
            "statusCheckRollup": [
                {"__typename": "CheckRun", "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"__typename": "CheckRun", "name": "smoke", "status": "IN_PROGRESS", "conclusion": ""},
                {"__typename": "StatusContext", "context": "ci/lint", "state": "FAILURE"}
            ]
        });
        let pr = parse_pr(&v).unwrap();
        assert_eq!(pr.state, "DRAFT");
        assert_eq!(pr.checks, vec![("ci/lint".into(), Check::Fail), ("smoke".into(), Check::Pending), ("build".into(), Check::Pass)]);
    }

    #[test]
    fn titles_are_relative_to_the_project() {
        let p = Path::new("/w/midna/crates/app/src/terminal.rs");
        assert_eq!(path_title(p, false, Some("/w/midna")), ("terminal.rs".into(), "crates/app/src".into()));
        assert_eq!(path_title(Path::new("/w/midna/docs"), true, Some("/w/midna")), ("docs/".into(), "midna".into()));
    }
}
