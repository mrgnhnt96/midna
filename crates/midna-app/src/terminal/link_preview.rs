//! Link preview (board `docs/design/LinkPreview-A.dc.html`): resting the pointer on a path or
//! URL in the grid (or ⌘-hovering it, per `terminal.link_preview`) opens a card at the bottom
//! right of the pane, above the queued-messages pill. It opens after `SHOW_DELAY` and stays while
//! the pointer is on the link, on the card, or while its IDE menu is open. When the pointer
//! leaves, the card fades out over `GRACE` (it holds near full strength, then falls away) before
//! it closes, so there's time to reach it; reaching it eases it back in. A key press or a click
//! in the terminal closes it at once.
//!
//! Files show the lines around the target line (wide lines scroll sideways), folders their
//! entries, images the image, GitHub pull requests their state and checks (`gh`), other web
//! pages their title and description (fetched with `curl`, never for local or private hosts),
//! app links (`taskboard://…`) whatever their app's `MidnaLinkPreview` command prints.
//! The footer opens a file in the IDE (`ide.rs`: real app icons, remembered per project; with
//! nothing remembered yet the first click lists the editors), and clicking the path in the
//! header reveals, opens or copies it (`terminal.preview_path_click`).
//!
//! The header's links popover (`ui/links.rs`) drives the same card: resting on one of its rows
//! previews that link here, and the grid's own hover leaves the card alone meanwhile.
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
/// How long the card takes to come back to full when the pointer returns mid-fade.
const RECOVER: Duration = Duration::from_millis(160);
/// How far the card sinks as it fades.
const SINK: f32 = 6.;
/// Clear of the prompt rail on the right edge (as the queue pill).
const RIGHT: f32 = 22.;
/// Lines of a file shown, and how many of them come before the target line.
const FILE_LINES: usize = 12;
const BEFORE: usize = 4;
const DIR_ROWS: usize = 8;
const CHECKS: usize = 8;
/// Fetched pages and pull requests are reused for this long.
const URL_TTL: Duration = Duration::from_secs(90);

/// The card's opacity `x` of the way through the grace period: `cubic-bezier(.7, 0, .84, 0)`, so
/// it's still 97% at the halfway mark and does most of its fading in the last quarter.
fn fade(x: f32) -> f32 {
    1. - crate::ui::setup_screen::bezier(0.7, 0., 0.84, 0., x.clamp(0., 1.))
}

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
    /// The app a `taskboard://…` link opens in (`Taskboard`); never fetched.
    app: Option<String>,
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
        let app = v.get("app").and_then(Value::as_str).filter(|a| url && !a.is_empty()).map(str::to_string);
        Some(Link { url, target, line, app })
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
    /// What the link's app says about it (`MidnaLinkPreview`).
    App(AppPreview),
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
    /// Opened from a links-popover row, so it's in the links already: no "Add to links".
    listed: bool,
    _load: Task<()>,
}

#[derive(Default)]
pub(super) struct Preview {
    /// The link under the pointer.
    hot: Option<Link>,
    /// `hot` is a links-popover row, not the grid: the grid's hover doesn't move the card.
    external: bool,
    /// Waiting out `SHOW_DELAY` for `hot`.
    pending: Option<Task<()>>,
    card: Option<Card>,
    over: bool,
    menu: bool,
    /// When a click outside closed the menu (so the chevron's own click doesn't reopen it).
    menu_closed: Option<Instant>,
    /// The grace period running since then (its number names the fade's animation).
    grace: Option<(u64, Instant, Task<()>)>,
    /// The pointer came back mid-fade: the card eases back from this opacity (named by the
    /// grace period it cut short).
    recover: Option<(u64, f32)>,
    seq: u64,
    /// A short note in the footer ("Copied"), and whether it's an error.
    flash: Option<(String, bool)>,
    flash_task: Option<Task<()>>,
}

impl TerminalView {
    /// The pointer is on a links-popover row for `target` (a URL when `url`, opening in `app`
    /// for an app link), or (`None`) left it.
    pub fn preview_external(&mut self, target: Option<(String, bool, Option<String>)>, cx: &mut Context<Self>) {
        self.preview.external = target.is_some();
        self.preview_set(target.map(|(target, url, app)| Link { url, target, line: None, app }), cx);
    }

    /// The pointer is on `link` (`None`: on no link, or off the grid).
    pub(super) fn preview_hover(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
        if !self.preview.external {
            self.preview_set(link, cx);
        }
    }

    fn preview_set(&mut self, link: Option<Link>, cx: &mut Context<Self>) {
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
        if let Some(c) = self.preview.card.as_mut().filter(|c| c.link == link) {
            c.listed |= self.preview.external;
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
        let (name, sub) = match &link.app {
            Some(app) => (app.clone(), link.target.split_once("://").map_or(link.target.as_str(), |(_, r)| r).to_string()),
            None if link.url => url_title(&link.target),
            None => path_title(path, is_dir, project.as_deref()),
        };
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
        self.preview.recover = None;
        self.preview.menu = false;
        self.preview.flash = None;
        self.preview.card = Some(Card { link, is_dir, project, name, sub, body: Body::Loading, ide, listed: self.preview.external, _load: load });
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
        if let Some((seq, at, _)) = self.preview.grace.take() {
            let from = fade(at.elapsed().as_secs_f32() / GRACE.as_secs_f32());
            self.preview.recover = (from < 0.99).then_some((seq, from));
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
                if t.preview.grace.as_ref().is_some_and(|(s, _, _)| *s == seq) {
                    if let Some((_, _, me)) = t.preview.grace.take() {
                        me.detach();
                    }
                    t.preview_close(cx);
                }
            });
        });
        p.grace = Some((seq, Instant::now(), task));
        p.recover = None;
        cx.notify();
    }

    pub(super) fn preview_close(&mut self, cx: &mut Context<Self>) {
        self.preview.pending = None;
        self.preview.external = false;
        if self.preview.card.is_none() {
            return;
        }
        let p = &mut self.preview;
        p.card = None;
        p.grace = None;
        p.recover = None;
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
        crate::sounds::play("copied");
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

        let icon = if c.link.app.is_some() {
            Icon::Link.el(14., t.dim)
        } else if c.link.url {
            if matches!(c.body, Body::Pr(_)) { Icon::Pr.el(14., t.ok) } else { Icon::Globe.el(14., t.dim) }
        } else if c.is_dir {
            Icon::Project.el(14., t.dim)
        } else {
            Icon::File.el(14., t.dim)
        };
        let sub = match (&c.body, c.is_dir) {
            (Body::App(p), _) => p.title.clone(),
            (Body::Dir { total, .. }, true) => format!("{}{}{total} item{}", c.sub, if c.sub.is_empty() { "" } else { " · " }, if *total == 1 { "" } else { "s" }),
            _ => c.sub.clone(),
        };
        let title = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .min_w_0()
            .child(div().flex_none().font_weight(FontWeight::BOLD).whitespace_nowrap().child(c.name.clone()))
            .child(match c.body {
                Body::App(_) => div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(sub),
                _ => div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(sub),
            });
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
            .child(header)
            .child(body)
            .child(footer);
        // Fading out over the grace period, or easing back in from wherever it was cut short.
        let bottom = pane_h - edge;
        let at = move |el: Stateful<Div>, o: f32| el.opacity(o).bottom(px(bottom - SINK * (1. - o)));
        let reduce = crate::ui::queue::reduce_motion();
        Some(match (&self.preview.grace, self.preview.recover) {
            (Some(_), _) if reduce => card.opacity(0.8).into_any_element(),
            (Some((seq, _, _)), _) => {
                card.with_animation(SharedString::from(format!("link-preview-fade-{seq}")), Animation::new(GRACE), move |el, d| at(el, fade(d))).into_any_element()
            }
            (None, Some((seq, from))) if !reduce => {
                let ease = |x: f32| 1. - (1. - x).powi(3);
                card.with_animation(SharedString::from(format!("link-preview-back-{seq}")), Animation::new(RECOVER).with_easing(ease), move |el, d| at(el, from + (1. - from) * d))
                    .into_any_element()
            }
            _ => card.into_any_element(),
        })
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
                .child(primary("lp-browse").on_click(cx.listener(|t, _, _, cx| t.preview_browse(cx))).child(match (&c.body, &c.link.app) {
                    (Body::App(AppPreview { open_label: Some(l), .. }), _) => l.clone(),
                    (_, Some(app)) => format!("Open in {app}"),
                    (_, None) => "Open in browser".into(),
                }))
                .child(secondary("lp-copy", "Copy URL").on_click(cx.listener(|t, _, _, cx| t.preview_copy(cx))))
                .when(!c.listed, |r| r.child(secondary("lp-add", "Add to links").on_click(cx.listener(|t, _, _, cx| t.preview_add_link(cx)))));
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
        Body::App(p) => {
            let pill = p.status.as_ref().map(|(label, tone)| {
                let bg = match tone {
                    Tone::Ok => t.ok,
                    Tone::Warn => t.need,
                    Tone::Err => t.err,
                    Tone::Busy => t.work,
                    Tone::Idle => t.dim,
                };
                div().flex_none().px(px(8.)).rounded_full().bg(bg).text_color(t.term).font_weight(FontWeight::BOLD).child(label.clone())
            });
            let bar = p.progress.map(|(done, total)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().flex_1().h(px(5.)).rounded_full().bg(t.line).child(div().h_full().rounded_full().bg(t.ok).w(relative(done as f32 / total as f32))))
                    .child(div().flex_none().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(format!("{done}/{total}")))
            });
            let mut fields = div().flex().flex_col().gap(px(2.)).text_size(px(12.));
            for (k, v) in &p.fields {
                fields = fields.child(div().flex().gap(px(10.)).child(div().flex_none().w(px(84.)).text_color(t.dim).child(k.clone())).child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(v.clone())));
            }
            let mut lines = div().flex().flex_col().gap(px(2.)).text_color(t.dim);
            for l in &p.lines {
                lines = lines.child(div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(l.clone()));
            }
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(14.))
                .py(px(12.))
                .when(pill.is_some() || p.subtitle.is_some(), |d| {
                    d.child(div().flex().items_center().gap(px(10.)).text_size(px(12.)).children(pill).when_some(p.subtitle.clone(), |d, s| d.child(div().min_w_0().text_color(t.dim).child(s))))
                })
                .when(!p.fields.is_empty(), |d| d.child(fields))
                .when(!p.lines.is_empty(), |d| d.child(lines))
                .children(bar)
                .child(div().overflow_hidden().text_ellipsis().whitespace_nowrap().font_family(t.mono_font.clone()).text_size(px(11.5)).text_color(t.dim).child(link.target.clone()))
                .into_any_element()
        }
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
    if let Some(app) = &link.app {
        return load_app(&link.target).map(Body::App).unwrap_or_else(|| Body::Note(format!("Opens in {app}")));
    }
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

// ------------------------------------------------------------------ app links
//
// The app that opens a link's scheme can fill the card. Its Info.plist names a command inside
// the bundle, `MidnaLinkPreview` = `["Contents/Resources/bin/tool", "preview"]` (the path, then
// any args; a bare string is the path alone). Midna runs `<path> <args…> <url>` with no stdin,
// for at most `APP_TIMEOUT`, and reads one JSON object from its stdout:
//
//   {"title": "…", "subtitle": "…", "status": {"label": "Working", "tone": "ok|warn|err|busy|idle"},
//    "fields": [["Goal", "G6"], …], "lines": ["…", …], "progress": {"done": 1, "total": 2},
//    "open_label": "Open task"}
//
// Only `title` is required. A non-zero exit, a timeout or bad JSON falls back to "Opens in <App>".

const APP_TIMEOUT: Duration = Duration::from_millis(1500);
/// App previews show live state (a task's status), so they're reused for less time than pages.
const APP_TTL: Duration = Duration::from_secs(15);
const APP_FIELDS: usize = 6;
const APP_LINES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    Ok,
    Warn,
    Err,
    Busy,
    Idle,
}

#[derive(Clone, Debug, PartialEq)]
struct AppPreview {
    title: String,
    subtitle: Option<String>,
    status: Option<(String, Tone)>,
    fields: Vec<(String, String)>,
    lines: Vec<String>,
    /// (done, total), total > 0.
    progress: Option<(u64, u64)>,
    open_label: Option<String>,
}

/// One line of text from the app, trimmed and cut to `max` characters (None when empty).
fn app_text(v: Option<&Value>, max: usize) -> Option<String> {
    let s = v?.as_str()?.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = if s.chars().count() > max { s.chars().take(max - 1).collect::<String>() + "…" } else { s };
    (!s.is_empty()).then_some(s)
}

fn parse_app_preview(out: &[u8]) -> Option<AppPreview> {
    let v: Value = serde_json::from_slice(out).ok()?;
    let title = app_text(v.get("title"), 200)?;
    let status = v.get("status").and_then(|s| {
        let label = app_text(s.get("label"), 40)?;
        let tone = match s.get("tone").and_then(Value::as_str) {
            Some("ok") => Tone::Ok,
            Some("warn") => Tone::Warn,
            Some("err") => Tone::Err,
            Some("busy") => Tone::Busy,
            _ => Tone::Idle,
        };
        Some((label, tone))
    });
    let fields = v.get("fields").and_then(Value::as_array).into_iter().flatten().filter_map(|f| Some((app_text(f.get(0), 40)?, app_text(f.get(1), 200)?))).take(APP_FIELDS).collect();
    let lines = v.get("lines").and_then(Value::as_array).into_iter().flatten().filter_map(|l| app_text(Some(l), 300)).take(APP_LINES).collect();
    let progress = v.get("progress").and_then(|p| {
        let (done, total) = (p.get("done")?.as_u64()?, p.get("total")?.as_u64()?);
        (total > 0).then_some((done.min(total), total))
    });
    Some(AppPreview { title, subtitle: app_text(v.get("subtitle"), 300), status, fields, lines, progress, open_label: app_text(v.get("open_label"), 40) })
}

/// `MidnaLinkPreview` (as `plutil` prints it in JSON) -> (path inside the bundle, args).
fn parse_preview_command(v: &Value) -> Option<(String, Vec<String>)> {
    let mut parts = match v {
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()?,
        _ => return None,
    }
    .into_iter();
    let path = parts.next().filter(|p| !p.is_empty() && !p.starts_with('/') && !Path::new(p).components().any(|c| matches!(c, std::path::Component::ParentDir)))?;
    Some((path, parts.collect()))
}

/// The command `bundle` declares for previews, resolved inside it (a symlink out of it is refused).
fn preview_command(bundle: &Path) -> Option<(PathBuf, Vec<String>)> {
    let mut cmd = std::process::Command::new("/usr/bin/plutil");
    cmd.args(["-extract", "MidnaLinkPreview", "json", "-o", "-"]).arg(bundle.join("Contents/Info.plist"));
    let (ok, out) = run_capped(cmd, 16 << 10, Duration::from_secs(2))?;
    let (rel, args) = parse_preview_command(&serde_json::from_slice(&out).ok().filter(|_| ok)?)?;
    let bundle = bundle.canonicalize().ok()?;
    let path = bundle.join(rel).canonicalize().ok()?;
    (path.starts_with(&bundle) && path.is_file()).then_some((path, args))
}

/// URL -> when it was asked and what came back (None, so a failing command isn't rerun each hover).
type AppCache = HashMap<String, (Instant, Option<AppPreview>)>;
static APP_CACHE: LazyLock<Mutex<AppCache>> = LazyLock::new(Default::default);

/// The preview the link's app supplies, None when it declares none or it fails.
fn load_app(url: &str) -> Option<AppPreview> {
    if let Some((at, p)) = APP_CACHE.lock().ok().and_then(|c| c.get(url).cloned())
        && at.elapsed() < APP_TTL
    {
        return p;
    }
    let preview = ls::default_app(url).and_then(|bundle| preview_command(&bundle)).and_then(|(path, args)| run_preview(&path, &args, url, APP_TIMEOUT));
    if let Ok(mut c) = APP_CACHE.lock() {
        c.retain(|_, (at, _)| at.elapsed() < APP_TTL);
        c.insert(url.to_string(), (Instant::now(), preview.clone()));
    }
    preview
}

/// `<path> <args…> <url>`, when it exits 0 in time with a preview on stdout.
fn run_preview(path: &Path, args: &[String], url: &str, timeout: Duration) -> Option<AppPreview> {
    let mut cmd = std::process::Command::new(path);
    cmd.args(args).arg(url);
    let (ok, out) = run_capped(cmd, 64 << 10, timeout)?;
    ok.then(|| parse_app_preview(&out)).flatten()
}

/// Launch Services' default app for a URL, as `midnad`'s `schemes.rs` names it for `session.link_at`.
mod ls {
    use core_foundation_sys::base::{CFRelease, CFTypeRef, kCFAllocatorDefault};
    use core_foundation_sys::string::{CFStringCreateWithBytes, kCFStringEncodingUTF8};
    use core_foundation_sys::url::{CFURLCreateWithString, CFURLGetFileSystemRepresentation, CFURLRef};
    use std::path::PathBuf;

    #[link(name = "CoreServices", kind = "framework")]
    unsafe extern "C" {
        fn LSCopyDefaultApplicationURLForURL(url: CFURLRef, roles: u32, err: *mut CFTypeRef) -> CFURLRef;
    }

    /// kLSRolesAll.
    const ROLES_ALL: u32 = 0xFFFF_FFFF;

    /// The bundle of the app that opens `url`.
    pub fn default_app(url: &str) -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        unsafe {
            let s = CFStringCreateWithBytes(kCFAllocatorDefault, url.as_ptr(), url.len() as _, kCFStringEncodingUTF8, 0);
            if s.is_null() {
                return None;
            }
            let u = CFURLCreateWithString(kCFAllocatorDefault, s, std::ptr::null());
            CFRelease(s as CFTypeRef);
            if u.is_null() {
                return None;
            }
            let app = LSCopyDefaultApplicationURLForURL(u, ROLES_ALL, std::ptr::null_mut());
            CFRelease(u as CFTypeRef);
            if app.is_null() {
                return None;
            }
            let mut buf = [0u8; 4096];
            let ok = CFURLGetFileSystemRepresentation(app, 1, buf.as_mut_ptr(), buf.len() as _);
            CFRelease(app as CFTypeRef);
            if ok == 0 {
                return None;
            }
            let len = buf.iter().position(|b| *b == 0)?;
            Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
        }
    }
}

/// Run `cmd`, keeping at most `limit` bytes of its output; killed after `timeout`, with anything
/// it started (in its own process group, so a child holding the pipe can't outlive it).
fn run_capped(mut cmd: std::process::Command, limit: usize, timeout: Duration) -> Option<(bool, Vec<u8>)> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0).spawn().ok()?;
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
                unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
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
    use super::{AppPreview, Body, Check, FILE_LINES, Link, Tone, excerpt, fade, fetchable, github_pr, load, page_meta, parse_app_preview, parse_pr, parse_preview_command, path_title, preview_command, run_preview, url_title};
    use serde_json::json;
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn app_links_name_their_app() {
        let l = Link::from_link_at(&json!({ "kind": "url", "target": "midna-test-unregistered-scheme://#/?task=T6", "app": "Taskboard" })).unwrap();
        assert_eq!(l.app.as_deref(), Some("Taskboard"));
        // No app supplies a preview: the card just says where it opens.
        assert!(matches!(load(&l), Body::Note(n) if n == "Opens in Taskboard"));
        let l = Link::from_link_at(&json!({ "kind": "url", "target": "https://a.com" })).unwrap();
        assert_eq!(l.app, None);
    }

    #[test]
    fn fade_holds_then_falls() {
        assert_eq!(fade(0.), 1.);
        assert!((fade(0.5) - 0.97).abs() < 0.01);
        assert!((fade(0.75) - 0.83).abs() < 0.01);
        assert!(fade(1.) < 0.001);
    }

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

    #[test]
    fn app_previews_parse() {
        let out = br#"{"title": " Ask the  app ", "subtitle": "G6", "status": {"label": "Working", "tone": "busy"},
            "fields": [["Goal", "G6"], ["bad"], [1, 2], ["PR", "12"]], "lines": ["one", "", 3, "two"],
            "progress": {"done": 5, "total": 2}, "open_label": "Open task", "extra": true}"#;
        assert_eq!(
            parse_app_preview(out),
            Some(AppPreview {
                title: "Ask the app".into(),
                subtitle: Some("G6".into()),
                status: Some(("Working".into(), Tone::Busy)),
                fields: vec![("Goal".into(), "G6".into()), ("PR".into(), "12".into())],
                lines: vec!["one".into(), "two".into()],
                progress: Some((2, 2)),
                open_label: Some("Open task".into()),
            })
        );
        // Only the title is needed; an unknown tone is idle; an empty total is no progress.
        let p = parse_app_preview(br#"{"title": "T", "status": {"label": "Odd", "tone": "purple"}, "progress": {"done": 0, "total": 0}}"#).unwrap();
        assert_eq!((p.status, p.progress, p.fields.len()), (Some(("Odd".into(), Tone::Idle)), None, 0));
        // No title, an empty one, not an object, not JSON: no preview.
        for bad in [&br#"{"subtitle": "x"}"#[..], br#"{"title": "  "}"#, b"[1]", b"Error: no such task", b""] {
            assert_eq!(parse_app_preview(bad), None, "{}", String::from_utf8_lossy(bad));
        }
        // Long text is cut.
        let long = format!(r#"{{"title": "{}"}}"#, "x".repeat(500));
        assert_eq!(parse_app_preview(long.as_bytes()).unwrap().title.chars().count(), 200);
    }

    #[test]
    fn preview_commands_stay_in_the_bundle() {
        assert_eq!(parse_preview_command(&json!("Contents/MacOS/tool")), Some(("Contents/MacOS/tool".into(), vec![])));
        assert_eq!(parse_preview_command(&json!(["Contents/Resources/bin/tb", "preview", "--json"])), Some(("Contents/Resources/bin/tb".into(), vec!["preview".into(), "--json".into()])));
        for bad in [json!("/bin/sh"), json!("Contents/../../x"), json!([]), json!([""]), json!(["a", 1]), json!({"path": "a"})] {
            assert_eq!(parse_preview_command(&bad), None, "{bad}");
        }
    }

    fn script(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn bundles_declare_their_preview() {
        let dir = std::env::temp_dir().join(format!("midna-app-preview-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let app = dir.join("Fake.app");
        let plist = |v: &str| {
            let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleName</key><string>Fake</string>{v}</dict></plist>"#);
            std::fs::create_dir_all(app.join("Contents")).unwrap();
            std::fs::write(app.join("Contents/Info.plist"), xml).unwrap();
        };
        script(&app.join("Contents/Resources/bin/tool"), r#"echo "{\"title\": \"$1 $2\"}""#);
        script(&dir.join("outside"), "true");

        plist("<key>MidnaLinkPreview</key><array><string>Contents/Resources/bin/tool</string><string>preview</string></array>");
        let (path, args) = preview_command(&app).unwrap();
        assert_eq!(args, vec!["preview".to_string()]);
        // The command gets its args, then the URL.
        assert_eq!(run_preview(&path, &args, "fake://x/1", Duration::from_secs(5)).map(|p| p.title), Some("preview fake://x/1".into()));

        // Declared nothing, or a path that leads out of the bundle: no command.
        plist("");
        assert_eq!(preview_command(&app), None);
        std::os::unix::fs::symlink(dir.join("outside"), app.join("Contents/Resources/bin/escape")).unwrap();
        plist("<key>MidnaLinkPreview</key><string>Contents/Resources/bin/escape</string>");
        assert_eq!(preview_command(&app), None);

        // A failure, bad output or a hang falls back.
        let bin = app.join("Contents/Resources/bin");
        script(&bin.join("fails"), r#"echo '{"title": "x"}'; exit 1"#);
        script(&bin.join("garbled"), "echo not json");
        script(&bin.join("hangs"), r#"sleep 5; echo '{"title": "x"}'"#);
        for name in ["fails", "garbled", "hangs"] {
            assert_eq!(run_preview(&bin.join(name), &[], "fake://x", Duration::from_millis(300)), None, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
