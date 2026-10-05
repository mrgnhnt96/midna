//! Open in IDE: the editors installed on this Mac, the header's split button and its menu
//! (⌥⇧⌘E). Editors are found by bundle id through Spotlight, so a renamed app still counts,
//! then by name in the usual folders. Which one opens a folder: `ide.rules` (folder rules,
//! then file rules like `pubspec.yaml = android-studio`), else the default `ide.app`. The
//! button and ⌥⌘E open the selected terminal's folder there; picking an editor in the menu
//! opens it there and remembers it for the project (⌥: as the default).
use crate::app::{MainWindow, Menu};
use crate::icons::Icon;
use crate::theme::Theme;
use gpui_kit::prelude::*;
use gpui_kit::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;

actions!(
    midna,
    [
        /// ⌥⌘E: open the selected terminal's folder in the remembered IDE.
        OpenInIde,
        /// ⌥⇧⌘E: the IDE menu (pick one to open there and remember it).
        ChooseIde,
    ]
);

/// Editors midna knows, in the order `ide.app = auto` tries them: (id, name, bundle ids, app names).
pub const KNOWN: &[(&str, &str, &[&str], &[&str])] = &[
    ("cursor", "Cursor", &["com.todesktop.230313mzl4w4u92"], &["Cursor.app"]),
    ("vscode", "VS Code", &["com.microsoft.VSCode"], &["Visual Studio Code.app"]),
    ("vscode-insiders", "VS Code Insiders", &["com.microsoft.VSCodeInsiders"], &["Visual Studio Code - Insiders.app"]),
    ("windsurf", "Windsurf", &["com.exafunction.windsurf"], &["Windsurf.app"]),
    ("kiro", "Kiro", &["dev.kiro.desktop"], &["Kiro.app"]),
    ("zed", "Zed", &["dev.zed.Zed"], &["Zed.app"]),
    ("zed-preview", "Zed Preview", &["dev.zed.Zed-Preview"], &["Zed Preview.app"]),
    ("sublime", "Sublime Text", &["com.sublimetext.4", "com.sublimetext.3"], &["Sublime Text.app"]),
    ("nova", "Nova", &["com.panic.Nova"], &["Nova.app"]),
    ("bbedit", "BBEdit", &["com.barebones.bbedit"], &["BBEdit.app"]),
    ("textmate", "TextMate", &["com.macromates.TextMate"], &["TextMate.app"]),
    ("intellij", "IntelliJ IDEA", &["com.jetbrains.intellij", "com.jetbrains.intellij.ce"], &["IntelliJ IDEA.app", "IntelliJ IDEA CE.app", "IntelliJ IDEA Ultimate.app"]),
    ("rustrover", "RustRover", &["com.jetbrains.rustrover"], &["RustRover.app"]),
    ("webstorm", "WebStorm", &["com.jetbrains.WebStorm"], &["WebStorm.app"]),
    ("pycharm", "PyCharm", &["com.jetbrains.pycharm", "com.jetbrains.pycharm.ce"], &["PyCharm.app", "PyCharm CE.app"]),
    ("goland", "GoLand", &["com.jetbrains.goland"], &["GoLand.app"]),
    ("clion", "CLion", &["com.jetbrains.CLion"], &["CLion.app"]),
    ("phpstorm", "PhpStorm", &["com.jetbrains.PhpStorm"], &["PhpStorm.app"]),
    ("rider", "Rider", &["com.jetbrains.rider"], &["Rider.app"]),
    ("rubymine", "RubyMine", &["com.jetbrains.rubymine"], &["RubyMine.app"]),
    ("android-studio", "Android Studio", &["com.google.android.studio"], &["Android Studio.app"]),
    ("xcode", "Xcode", &["com.apple.dt.Xcode"], &["Xcode.app"]),
];

#[derive(Clone, Debug, PartialEq)]
pub struct Ide {
    /// A `KNOWN` id, or the .app path for one `ide.app` names by path.
    pub id: String,
    pub name: String,
    /// The .app bundle.
    pub path: PathBuf,
    /// Its icon as a PNG in midna's cache, when the bundle has an .icns.
    pub icon: Option<PathBuf>,
}

pub struct IdePanel {
    /// Installed editors, in `KNOWN` order (plus an `ide.app` path last).
    pub list: Vec<Ide>,
    pub focus: FocusHandle,
    sel: usize,
    /// Top-level file names per folder, for `ide.rules` file rules.
    names: std::cell::RefCell<std::collections::HashMap<PathBuf, Vec<String>>>,
}

impl IdePanel {
    pub fn new(cx: &mut App) -> IdePanel {
        IdePanel { list: vec![], focus: cx.focus_handle(), sel: 0, names: Default::default() }
    }
}

// ------------------------------------------------------------------ detection

/// `mdfind -attr kMDItemCFBundleIdentifier` lines -> (path, bundle id). Skips apps inside
/// other bundles and the Trash.
fn parse_mdfind(out: &str) -> Vec<(PathBuf, String)> {
    out.lines()
        .filter_map(|l| {
            let (path, id) = l.rsplit_once("kMDItemCFBundleIdentifier = ")?;
            let path = path.trim_end();
            let nested = path.trim_end_matches(".app").contains(".app/");
            (!nested && !path.contains("/.Trash/") && path.ends_with(".app")).then(|| (PathBuf::from(path), id.trim().trim_matches('"').to_string()))
        })
        .collect()
}

/// Where an app is, preferring /Applications, then ~/Applications, then anywhere else Spotlight saw it.
fn rank(p: &Path, home: &Path) -> u8 {
    if p.starts_with("/Applications") {
        0
    } else if p.starts_with(home.join("Applications")) {
        1
    } else {
        2
    }
}

/// Pick each known editor's bundle from what Spotlight found (`found`) or, failing that, by
/// name in the usual folders (`exists` checks a path).
fn resolve(found: &[(PathBuf, String)], home: &Path, exists: &dyn Fn(&Path) -> bool) -> Vec<(usize, PathBuf)> {
    let dirs = [PathBuf::from("/Applications"), home.join("Applications"), home.join("Applications/JetBrains Toolbox"), PathBuf::from("/Applications/Setapp")];
    let mut out = vec![];
    for (i, (_, _, bundles, names)) in KNOWN.iter().enumerate() {
        let by_id = found.iter().filter(|(_, b)| bundles.contains(&b.as_str())).map(|(p, _)| p).min_by_key(|p| rank(p, home)).cloned();
        let by_name = || dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| exists(p));
        if let Some(p) = by_id.or_else(by_name) {
            out.push((i, p));
        }
    }
    out
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// The installed editors, plus the bundle `ide.app` names by path. Blocking (Spotlight, sips).
pub fn installed(custom: Option<&str>) -> Vec<Ide> {
    let query = KNOWN.iter().flat_map(|k| k.2.iter()).map(|b| format!("kMDItemCFBundleIdentifier == '{b}'")).collect::<Vec<_>>().join(" || ");
    let found = Command::new("/usr/bin/mdfind").args(["-attr", "kMDItemCFBundleIdentifier", &query]).output().map(|o| parse_mdfind(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default();
    let home = home();
    let mut list: Vec<Ide> = resolve(&found, &home, &|p| p.exists())
        .into_iter()
        .map(|(i, path)| Ide { id: KNOWN[i].0.into(), name: KNOWN[i].1.into(), icon: icon_png(KNOWN[i].0, &path), path })
        .collect();
    if let Some(c) = custom.filter(|c| c.ends_with(".app")) {
        let path = PathBuf::from(expand(c));
        if path.exists() && !list.iter().any(|i| i.path == path) {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| c.to_string());
            let key: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            list.push(Ide { id: c.to_string(), name, icon: icon_png(&format!("custom-{key}"), &path), path });
        }
    }
    list
}

fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest).to_string_lossy().into_owned(),
        None => p.to_string(),
    }
}

/// The app's .icns as a 64 px PNG under ~/Library/Caches, redone when the app changes.
fn icon_png(key: &str, app: &Path) -> Option<PathBuf> {
    let dir = home().join("Library/Caches").join(midna_proto::paths::BUNDLE_ID).join("ide-icons");
    let out = dir.join(format!("{key}.png"));
    let plist = app.join("Contents/Info.plist");
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if out.exists() && mtime(&out) >= mtime(&plist) {
        return Some(out);
    }
    let read = |field: &str| {
        let o = Command::new("/usr/bin/plutil").args(["-extract", field, "raw", "-o", "-"]).arg(&plist).output().ok()?;
        let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
        (o.status.success() && !v.is_empty()).then_some(v)
    };
    let icns = ["CFBundleIconFile", "CFBundleIconName"].iter().filter_map(|f| read(f)).map(|n| if n.ends_with(".icns") { n } else { format!("{n}.icns") }).map(|n| app.join("Contents/Resources").join(n)).find(|p| p.exists())?;
    std::fs::create_dir_all(&dir).ok()?;
    let ok = Command::new("/usr/bin/sips").args(["-s", "format", "png", "-Z", "64"]).arg(&icns).arg("--out").arg(&out).output().is_ok_and(|o| o.status.success());
    ok.then_some(out)
}

/// An installed editor named the way `ide.app` and rules name one: its id or .app path.
fn find<'a>(list: &'a [Ide], value: &str) -> Option<&'a Ide> {
    let v = value.trim();
    list.iter().find(|i| i.id == v || i.path == Path::new(&expand(v)))
}

/// The editor `ide.app` picks: its id or path, else (auto, or it was uninstalled) the first found.
pub fn chosen<'a>(list: &'a [Ide], setting: Option<&str>) -> Option<&'a Ide> {
    let want = setting.map(str::trim).filter(|s| !s.is_empty() && *s != "auto");
    want.and_then(|w| find(list, w)).or_else(|| list.first())
}

/// Why an editor opens a folder (shown in the menu's footer).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A folder rule (its match, as written).
    Folder(String),
    /// A file rule (its pattern).
    File(String),
    /// `ide.app`.
    Default,
}

/// A folder rule's match is a path; anything else is a file pattern.
fn is_folder_rule(m: &str) -> bool {
    m.starts_with('/') || m == "~" || m.starts_with("~/")
}

/// `*` and `?` over one file name.
fn glob(pat: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    go(&pat.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

/// What opens a folder: the longest folder rule covering one of `dirs` (the folder, its
/// project), then the first file rule matching one of `names` (the files at their top level),
/// then `ide.app`. Rules naming an editor that isn't installed are skipped.
pub fn choose<'a>(list: &'a [Ide], default: Option<&str>, rules: &[String], dirs: &[&Path], names: &[String]) -> Option<(&'a Ide, Source)> {
    let parsed = rules.iter().filter_map(|r| r.split_once('=')).map(|(m, v)| (m.trim(), v.trim()));
    let folder = parsed
        .clone()
        .filter(|(m, _)| is_folder_rule(m))
        .filter_map(|(m, v)| {
            let root = PathBuf::from(expand(m));
            dirs.iter().any(|d| d.starts_with(&root)).then_some((root.components().count(), m, find(list, v)?))
        })
        .max_by_key(|(depth, ..)| *depth)
        .map(|(_, m, ide)| (ide, Source::Folder(m.to_string())));
    let file = || parsed.clone().filter(|(m, _)| !is_folder_rule(m)).find_map(|(m, v)| names.iter().any(|n| glob(m, n)).then(|| find(list, v)).flatten().map(|ide| (ide, Source::File(m.to_string()))));
    folder.or_else(file).or_else(|| chosen(list, default).map(|i| (i, Source::Default)))
}

/// `rules` without the folder rule for exactly `project`, and with `ide` (written as
/// `shown = ide`) first when given.
fn with_project_rule(rules: &[String], project: &Path, shown: &str, ide: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = rules
        .iter()
        .filter(|r| r.split_once('=').is_none_or(|(m, _)| !(is_folder_rule(m.trim()) && Path::new(&expand(m.trim())) == project)))
        .cloned()
        .collect();
    if let Some(ide) = ide {
        out.insert(0, format!("{shown} = {ide}"));
    }
    out
}

/// Look for editors again (at launch and when the menu opens), off the main thread.
pub fn detect(cx: &mut Context<MainWindow>) {
    cx.spawn(async move |this, cx| {
        let Ok(custom) = this.update(cx, |m, _| m.setting_str("ide.app")) else { return };
        let list = cx.background_executor().spawn(async move { installed(custom.as_deref()) }).await;
        let _ = this.update(cx, |m, cx| {
            if m.ide.list != list {
                m.ide.list = list;
                cx.notify();
            }
        });
    })
    .detach();
}

// ------------------------------------------------------------------ opening

/// The folder to open: the selected terminal's directory, else its project's.
pub fn folder(m: &MainWindow) -> Option<String> {
    let s = m.selected_session()?;
    Some(s.cwd.clone()).filter(|c| !c.is_empty() && Path::new(c).is_dir()).or_else(|| project(m))
}

/// The selected terminal's project folder.
fn project(m: &MainWindow) -> Option<String> {
    let s = m.selected_session()?;
    s.project_id.as_ref().and_then(|p| m.projects.iter().find(|x| &x.id == p)).map(|p| p.path.clone())
}

fn rules(m: &MainWindow) -> Vec<String> {
    m.settings.get("ide.rules").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Top-level file names of the folder and its project, for file rules (cached until the menu
/// opens again, since the header asks every frame).
fn names(m: &MainWindow, dirs: &[&Path]) -> Vec<String> {
    let mut cache = m.ide.names.borrow_mut();
    dirs.iter()
        .flat_map(|d| {
            cache
                .entry(d.to_path_buf())
                .or_insert_with(|| std::fs::read_dir(d).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default())
                .clone()
        })
        .collect()
}

/// The editor for the selected terminal's folder, and why.
pub fn current(m: &MainWindow) -> Option<(Ide, Source)> {
    let dir = folder(m)?;
    let proj = project(m);
    let dirs: Vec<&Path> = std::iter::once(Path::new(&dir)).chain(proj.as_deref().map(Path::new).filter(|p| *p != Path::new(&dir))).collect();
    let names = names(m, &dirs);
    choose(&m.ide.list, m.setting_str("ide.app").as_deref(), &rules(m), &dirs, &names).map(|(i, s)| (i.clone(), s))
}

fn set(m: &mut MainWindow, key: &'static str, value: serde_json::Value, cx: &mut Context<MainWindow>) {
    m.settings.insert(key.to_string(), value.clone());
    let backend = m.backend.clone();
    cx.background_executor().spawn(async move { backend.call("settings.set", json!({"key": key, "value": value})) }).detach();
    cx.notify();
}

/// Remember `ide` for the selected terminal's project (a folder rule in `ide.rules`), or with
/// `everywhere` (or no project) as `ide.app`. A project rule that would pick what the other
/// rules and the default pick anyway is dropped instead, so the list stays short.
pub fn remember(m: &mut MainWindow, ide: &Ide, everywhere: bool, cx: &mut Context<MainWindow>) {
    let rules = rules(m);
    let Some(proj) = project(m).filter(|_| !everywhere) else {
        if m.setting_str("ide.app").as_deref() != Some(ide.id.as_str()) {
            set(m, "ide.app", json!(ide.id), cx);
        }
        if let Some(proj) = project(m) {
            let next = with_project_rule(&rules, Path::new(&proj), "", None);
            if next != rules {
                set(m, "ide.rules", json!(next), cx);
            }
        }
        return;
    };
    let without = with_project_rule(&rules, Path::new(&proj), "", None);
    m.settings.insert("ide.rules".into(), json!(without));
    let fallback = current(m).map(|(i, _)| i.id);
    let next = if fallback.as_deref() == Some(ide.id.as_str()) { without } else { with_project_rule(&rules, Path::new(&proj), &crate::commands::tilde(&proj), Some(&ide.id)) };
    m.settings.insert("ide.rules".into(), json!(rules));
    if next != rules {
        set(m, "ide.rules", json!(next), cx);
    }
}

/// Open `dir` in `ide`.
pub fn open_in(ide: Ide, dir: String, cx: &mut Context<MainWindow>) {
    cx.spawn(async move |this, cx| {
        let app = ide.path.clone();
        let target = dir.clone();
        let res = cx
            .background_executor()
            .spawn(async move {
                let o = Command::new("/usr/bin/open").arg("-a").arg(&app).arg(&target).output().map_err(|e| e.to_string())?;
                if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            })
            .await;
        let _ = this.update(cx, |m, cx| match res {
            Ok(()) => m.toast(format!("Opened {} in {}", crate::commands::tilde(&dir), ide.name), cx),
            Err(e) => m.toast(format!("Couldn't open {}: {e}", ide.name), cx),
        });
    })
    .detach();
}

/// Open the selected terminal's folder in `ide` and remember it (see [`remember`]).
pub fn open_and_remember(m: &mut MainWindow, ide: Ide, everywhere: bool, cx: &mut Context<MainWindow>) {
    let Some(dir) = folder(m) else { return };
    remember(m, &ide, everywhere, cx);
    open_in(ide, dir, cx);
}

/// ⌥⌘E / the button: the selected terminal's folder in the editor its rules pick.
pub fn open_default(m: &mut MainWindow, cx: &mut Context<MainWindow>) {
    let Some(dir) = folder(m) else { return };
    m.ide.names.borrow_mut().clear();
    match current(m) {
        Some((ide, _)) => open_in(ide, dir, cx),
        None => m.toast("No IDE found. Set ide.app to an editor's .app path.", cx),
    }
}

// ------------------------------------------------------------------ menu

pub fn toggle(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    if m.menu == Menu::Ide {
        close(m, window, cx);
        return;
    }
    if m.selected_session().is_none() {
        return;
    }
    detect(cx);
    m.ide.names.borrow_mut().clear();
    m.menu = Menu::Ide;
    let cur = current(m).map(|(c, _)| c.id);
    m.ide.sel = m.ide.list.iter().position(|i| Some(&i.id) == cur.as_ref()).unwrap_or(0);
    m.ide.focus.focus(window, cx);
    cx.notify();
}

fn close(m: &mut MainWindow, window: &mut Window, cx: &mut Context<MainWindow>) {
    m.menu = Menu::None;
    m.focus_terminal(window, cx);
    cx.notify();
}

/// A row: open there and remember it for this project (⌥: for every project).
fn pick(m: &mut MainWindow, i: usize, everywhere: bool, window: &mut Window, cx: &mut Context<MainWindow>) {
    let Some(ide) = m.ide.list.get(i).cloned() else { return };
    close(m, window, cx);
    open_and_remember(m, ide, everywhere, cx);
}

fn on_key(m: &mut MainWindow, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<MainWindow>) {
    let n = m.ide.list.len();
    match ev.keystroke.key.as_str() {
        "up" | "down" if n > 0 => {
            let cur = m.ide.sel.min(n - 1);
            m.ide.sel = if ev.keystroke.key == "down" { (cur + 1) % n } else { (cur + n - 1) % n };
        }
        "enter" => pick(m, m.ide.sel.min(n.saturating_sub(1)), ev.keystroke.modifiers.alt, window, cx),
        "escape" => close(m, window, cx),
        _ => return,
    }
    cx.stop_propagation();
    cx.notify();
}

fn app_icon(ide: Option<&Ide>, size: f32, t: &Theme) -> AnyElement {
    match ide.and_then(|i| i.icon.clone()) {
        Some(p) => img(p).size(px(size)).flex_none().into_any_element(),
        None => Icon::Code.el(size - 2., t.dim).into_any_element(),
    }
}

/// The header's split button: the editor's icon opens the folder there, the chevron opens the
/// menu. None when no editor was found.
pub fn button(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> Option<AnyElement> {
    let (cur, _) = current(m)?;
    let open = m.menu == Menu::Ide;
    let main = div()
        .id("tb-ide")
        .h(px(32.))
        .pl(px(8.))
        .pr(px(4.))
        .flex()
        .items_center()
        .cursor_pointer()
        .rounded_l(px(7.))
        .hover(|st| st.bg(t.raised))
        .tooltip(crate::ui::header::tip_keys(format!("Open in {}", cur.name), "keys.open_ide"))
        .on_click(cx.listener(|m, _, _, cx| open_default(m, cx)))
        .child(app_icon(Some(&cur), 18., t));
    let chevron = div()
        .id("tb-ide-menu")
        .h(px(32.))
        .w(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .rounded_r(px(7.))
        .when(open, |d| d.bg(t.raised))
        .hover(|st| st.bg(t.raised))
        .tooltip(crate::ui::header::tip_keys("Open in…", "keys.choose_ide"))
        .on_click(cx.listener(|m, _, window, cx| toggle(m, window, cx)))
        .child(Icon::Chevron.el(10., t.dim));
    Some(div().relative().flex().items_center().child(main).child(chevron).when(open, |d| d.child(menu(m, t, cx))).into_any_element())
}

fn menu(m: &MainWindow, t: &Theme, cx: &mut Context<MainWindow>) -> impl IntoElement + use<> {
    let cur = current(m);
    let sel = m.ide.sel.min(m.ide.list.len().saturating_sub(1));
    let mut list = div().flex().flex_col().p(px(6.));
    for (i, ide) in m.ide.list.iter().enumerate() {
        let is_cur = cur.as_ref().is_some_and(|(c, _)| c.id == ide.id);
        list = list.child(
            div()
                .id(("ide-row", i))
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(8.))
                .py(px(6.))
                .rounded(px(7.))
                .cursor_pointer()
                .when(i == sel, |d| d.bg(t.accent_soft))
                .on_mouse_move(cx.listener(move |m, _, _, cx| {
                    if m.ide.sel != i {
                        m.ide.sel = i;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |m, ev: &ClickEvent, window, cx| pick(m, i, ev.modifiers().alt, window, cx)))
                .child(app_icon(Some(ide), 20., t))
                .child(div().flex_1().whitespace_nowrap().child(ide.name.clone()))
                .when(is_cur, |d| d.child(Icon::Check.el(13., t.dim))),
        );
    }
    let why = match cur.map(|(_, s)| s) {
        Some(Source::Folder(_)) => "chosen for this project".to_string(),
        Some(Source::File(p)) => format!("rule: {p}"),
        _ => "default IDE".to_string(),
    };
    let dir = folder(m).map(|d| crate::commands::tilde(&d)).unwrap_or_default();
    let line = |text: String| div().min_w_0().overflow_hidden().text_ellipsis().whitespace_nowrap().child(text);
    let footer = div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .px(px(14.))
        .py(px(8.))
        .border_t_1()
        .border_color(t.line)
        .text_size(px(11.5))
        .text_color(t.dim)
        .child(line(dir).font_family(t.mono_font.clone()))
        .child(line(why))
        .child(line(if project(m).is_some() { "↩ this project · ⌥↩ all projects".into() } else { "↩ remembers it for all folders".into() }));
    deferred(
        anchored().anchor(Anchor::TopRight).snap_to_window_with_margin(px(8.)).child(
            div()
                .id("ide-menu")
                .key_context("MidnaOverlay")
                .track_focus(&m.ide.focus)
                .on_key_down(cx.listener(on_key))
                .occlude()
                .mt(px(38.))
                .w(px(260.))
                .flex()
                .flex_col()
                .rounded(px(10.))
                .border_1()
                .border_color(t.line)
                .bg(t.raised)
                .text_color(t.fg)
                .text_size(px(13.))
                .shadow(vec![BoxShadow { color: hsla(0., 0., 0., 0.35), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(0.), inset: false }])
                .child(div().px(px(14.)).pt(px(10.)).text_size(px(11.)).font_weight(FontWeight::BOLD).text_color(t.dim).child("OPEN IN"))
                .child(list)
                .child(footer),
        ),
    )
    .with_priority(1)
}

#[cfg(test)]
mod tests {
    use super::{Ide, KNOWN, Source, choose, chosen, glob, parse_mdfind, resolve, with_project_rule};
    use std::path::{Path, PathBuf};

    #[test]
    fn mdfind_lines_skip_nested_and_trashed_apps() {
        let out = "/Applications/VS Code.app   kMDItemCFBundleIdentifier = com.microsoft.VSCode\n\
                   /Users/me/.Trash/Zed.app   kMDItemCFBundleIdentifier = dev.zed.Zed\n\
                   /Applications/Xcode.app/Contents/Applications/Thing.app   kMDItemCFBundleIdentifier = com.apple.dt.Xcode\n\
                   /Users/me/Applications/Xcode.app   kMDItemCFBundleIdentifier = com.apple.dt.Xcode\n";
        let got = parse_mdfind(out);
        assert_eq!(got, [(PathBuf::from("/Applications/VS Code.app"), "com.microsoft.VSCode".into()), (PathBuf::from("/Users/me/Applications/Xcode.app"), "com.apple.dt.Xcode".into())]);
    }

    #[test]
    fn resolve_prefers_bundle_id_then_name_in_known_order() {
        let home = PathBuf::from("/Users/me");
        let found = vec![
            (PathBuf::from("/Users/me/Downloads/Xcode.app"), "com.apple.dt.Xcode".to_string()),
            (PathBuf::from("/Applications/Xcode.app"), "com.apple.dt.Xcode".to_string()),
            (PathBuf::from("/Applications/VS Code.app"), "com.microsoft.VSCode".to_string()),
        ];
        let exists = |p: &Path| p == Path::new("/Users/me/Applications/JetBrains Toolbox/RustRover.app");
        let got: Vec<(&str, PathBuf)> = resolve(&found, &home, &exists).into_iter().map(|(i, p)| (KNOWN[i].0, p)).collect();
        assert_eq!(
            got,
            [
                ("vscode", PathBuf::from("/Applications/VS Code.app")),
                ("rustrover", PathBuf::from("/Users/me/Applications/JetBrains Toolbox/RustRover.app")),
                ("xcode", PathBuf::from("/Applications/Xcode.app")),
            ]
        );
    }

    #[test]
    fn chosen_falls_back_to_the_first_found() {
        let list = [ide("vscode", "/Applications/VS Code.app"), ide("xcode", "/Applications/Xcode.app")];
        assert_eq!(chosen(&list, Some("xcode")).unwrap().id, "xcode");
        assert_eq!(chosen(&list, Some("/Applications/Xcode.app")).unwrap().id, "xcode", "by path");
        assert_eq!(chosen(&list, Some("auto")).unwrap().id, "vscode");
        assert_eq!(chosen(&list, Some("zed")).unwrap().id, "vscode", "uninstalled");
        assert_eq!(chosen(&list, None).unwrap().id, "vscode");
        assert!(chosen(&[], Some("xcode")).is_none());
    }

    fn ide(id: &str, path: &str) -> Ide {
        Ide { id: id.into(), name: id.into(), path: path.into(), icon: None }
    }

    #[test]
    fn globs_match_one_file_name() {
        assert!(glob("*.xcodeproj", "Runner.xcodeproj"));
        assert!(glob("pubspec.yaml", "pubspec.yaml"));
        assert!(glob("build.gradle*", "build.gradle.kts"));
        assert!(glob("?akefile", "Makefile"));
        assert!(!glob("*.xcodeproj", "Runner.xcworkspace"));
        assert!(!glob("pubspec.yaml", "pubspec.yaml.bak"));
    }

    #[test]
    fn folder_rules_beat_file_rules_beat_the_default() {
        let list = [ide("vscode", "/Applications/VS Code.app"), ide("xcode", "/Applications/Xcode.app"), ide("android-studio", "/Applications/Android Studio.app")];
        let rules: Vec<String> = ["pubspec.yaml = android-studio", "*.xcodeproj = xcode", "/w/app = xcode", "/w/app/packages = vscode", "/w/old = zed"].map(String::from).to_vec();
        let pick = |dirs: &[&str], names: &[&str]| {
            let dirs: Vec<&Path> = dirs.iter().map(Path::new).collect();
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            choose(&list, Some("vscode"), &rules, &dirs, &names).map(|(i, s)| (i.id.clone(), s))
        };
        assert_eq!(pick(&["/w/app/lib", "/w/app"], &["pubspec.yaml"]), Some(("xcode".into(), Source::Folder("/w/app".into()))), "folder rule covers subfolders and wins");
        assert_eq!(pick(&["/w/app/packages/ui"], &[]), Some(("vscode".into(), Source::Folder("/w/app/packages".into()))), "longest folder rule");
        assert_eq!(pick(&["/w/flutter"], &["README.md", "pubspec.yaml", "Runner.xcodeproj"]), Some(("android-studio".into(), Source::File("pubspec.yaml".into()))), "first file rule");
        assert_eq!(pick(&["/w/ios"], &["Runner.xcodeproj"]), Some(("xcode".into(), Source::File("*.xcodeproj".into()))));
        assert_eq!(pick(&["/w/old"], &["pubspec.yaml"]), Some(("android-studio".into(), Source::File("pubspec.yaml".into()))), "a rule for an uninstalled IDE is skipped");
        assert_eq!(pick(&["/w/other"], &["Cargo.toml"]), Some(("vscode".into(), Source::Default)));
    }

    #[test]
    fn project_rules_replace_the_old_one_and_go_first() {
        let rules: Vec<String> = ["pubspec.yaml = android-studio", "/w/app/ = xcode", "/w/other = zed"].map(String::from).to_vec();
        assert_eq!(with_project_rule(&rules, Path::new("/w/app"), "/w/app", Some("cursor")), ["/w/app = cursor", "pubspec.yaml = android-studio", "/w/other = zed"]);
        assert_eq!(with_project_rule(&rules, Path::new("/w/app"), "", None), ["pubspec.yaml = android-studio", "/w/other = zed"]);
    }

    #[test]
    fn known_ids_are_unique_and_listed_in_the_catalog() {
        let mut seen = std::collections::HashSet::new();
        assert!(KNOWN.iter().all(|k| seen.insert(k.0)));
        let spec = midna_proto::settings::SETTINGS.iter().find(|s| s.key == "ide.app").expect("ide.app setting");
        match spec.setting_type() {
            midna_proto::settings::SettingType::Enum(opts) => assert_eq!(opts, std::iter::once("auto").chain(KNOWN.iter().map(|k| k.0)).collect::<Vec<_>>()),
            other => panic!("ide.app is {other:?}"),
        }
    }
}
