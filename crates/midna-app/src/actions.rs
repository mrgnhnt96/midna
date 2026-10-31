//! Actions and keybindings. Bindings come from the `keys.*` settings and are rebuilt on
//! `settings.changed`. Hooks for later screens (palette, needs-you stack, rules, triggers,
//! insights, settings window) are actions too, so a palette can list them.
use gpui_kit::*;
use std::collections::HashMap;

actions!(
    midna,
    [
        /// ⌘K palette (CommandBar-A). Placeholder overlay for now.
        ToggleCommandBar,
        /// Jump to the next needs-you item.
        NextNeedsYou,
        /// Open the needs-you inbox (`ui/needs_you.rs`).
        OpenNeedsYou,
        NewTerminal,
        NewAgent,
        NewTerminalRoot,
        NewAgentRoot,
        /// Folder picker → `project.add` → a terminal in it.
        OpenProject,
        /// Another main window (same daemon, its own selection).
        NewWindow,
        /// Approve the selected session's pending approval once.
        ApproveOnce,
        Deny,
        OpenSettings,
        OpenRules,
        OpenTriggers,
        OpenInsights,
        /// Back to the terminal pane from Rules/Triggers/Insights, or close an overlay.
        Dismiss,
        SplitRight,
        PopOut,
        RestartSession,
        /// Swap the selected terminal for a fresh one in its place (`session.replace`).
        ReplaceSession,
        /// ⌘W: in the main window, close the selected terminal (the window when none is
        /// selected); in Settings or a pop-out, close that window.
        CloseWindow,
        Quit,
        /// ⌘Q: hold for 2.5 s to quit (see `ui::quit_hold`). The menu item quits at once.
        HoldToQuit,
        /// ⌘H / ⌥⌘H / Show All: the standard macOS app-menu items.
        Hide,
        HideOthers,
        ShowAll,
        // terminal
        TermCopy,
        /// ⌘X: copy the selection, and erase it when it's in the input line.
        TermCut,
        TermPaste,
        TermPasteSecret,
        /// Paste the clipboard's image as its path, skipping the image sheet.
        TermPasteImageInline,
        TermSelectAll,
        TermClear,
        /// Agent terminals: scroll to the previous / next prompt you sent (`terminal::prompt_nav`).
        PrevPrompt,
        NextPrompt,
        /// The command bar listing the selected agent terminal's prompts.
        OpenPrompts,
        /// The approval banner's approve options (15 min, 1 h, session, always).
        ApproveOptions,
        /// Select the next / previous terminal in sidebar order.
        NextTerminal,
        PrevTerminal,
        /// Back / forward through the terminals you've shown, like a browser (`tab_history`).
        TerminalBack,
        TerminalForward,
        /// Rename the selected terminal in the header.
        RenameSession,
        /// The header's … menu.
        ToggleTerminalMenu,
        CopySessionId,
        /// Mute / unmute notifications for the selected terminal.
        ToggleMute,
        /// Split: side by side ⇄ stacked.
        ToggleSplitOrientation,
        /// Give the split's terminal its own sidebar row.
        SplitToMain,
        /// Focus the other split pane.
        FocusOtherPane,
        /// Pop-out window: keep on top.
        ToggleKeepOnTop,
        /// The current project's … menu / fold its group in the sidebar.
        ToggleProjectMenu,
        FoldProject,
        /// The current project's Summary of closed terminals.
        ShowProjectSummary,
        /// Collapse the sidebar to a rail / expand it again.
        ToggleSidebar,
    ]
);

/// ⌘1–9: select project N (0-based index into the sidebar order).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct SelectProject(pub usize);

pub const CTX_MAIN: &str = "MidnaMain";
pub const CTX_TERM: &str = "Terminal";
/// Every overlay over the main window has this (escape dismisses it), its [`Kind`] and its
/// id: `MidnaOverlay Modal overlay=annotate`.
pub const CTX_OVERLAY: &str = "MidnaOverlay";

/// What kind of place an overlay is, which decides whether main-window shortcuts reach it.
/// The composer is none of these: it's a field in place over its own terminal (switching
/// terminals puts it away as a draft), so main-window shortcuts keep working there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Takes the window until you finish with it: the image sheet, the needs-you cards. Blocks
    /// main-window shortcuts.
    Modal,
    /// A small panel you dismiss: the command bar, links, queue, subagents, the IDE picker.
    /// Blocks main-window shortcuts.
    Popover,
    /// Stands in for the terminal pane: rules, triggers, insights, notifications, summary.
    /// Main-window shortcuts keep working.
    Screen,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Modal => "Modal",
            Kind::Popover => "Popover",
            Kind::Screen => "Screen",
        }
    }
}

/// Each overlay's id (`overlay=<id>`), for bindings that work in that overlay alone.
pub mod overlay {
    pub const ANNOTATE: &str = "annotate";
    pub const NEEDS_YOU: &str = "needs_you";
    pub const COMMAND_BAR: &str = "command_bar";
    pub const LINKS: &str = "links";
    pub const QUEUE: &str = "queue";
    pub const SUBAGENTS: &str = "subagents";
    pub const IDE: &str = "ide";
    pub const RULES: &str = "rules";
    pub const TRIGGERS: &str = "triggers";
    pub const INSIGHTS: &str = "insights";
    pub const NOTIFICATIONS: &str = "notifications";
    pub const SUMMARY: &str = "summary";
    #[cfg(test)]
    pub const ALL: &[&str] = &[ANNOTATE, NEEDS_YOU, COMMAND_BAR, LINKS, QUEUE, SUBAGENTS, IDE, RULES, TRIGGERS, INSIGHTS, NOTIFICATIONS, SUMMARY];
}

/// An overlay's key context (see [`CTX_OVERLAY`]).
pub fn overlay_context(kind: Kind, id: &'static str) -> KeyContext {
    let mut c = KeyContext::default();
    c.add(CTX_OVERLAY);
    c.add(kind.name());
    c.set("overlay", id);
    c
}

/// Where a binding works: a GPUI context predicate (`&&`, `||`, `!`, `==`, `!=`, `>`), or
/// `None` for anywhere. `!X` is false when any context around the focus has X.
pub type When = Option<&'static str>;

/// The main window (and pop-outs) while no modal or popover is up; screens keep it. The
/// default for main-window shortcuts.
pub const WINDOW: When = Some("MidnaMain && !Modal && !Popover");
/// Anywhere, modals, popovers and Settings included: the app-level actions (settings, new
/// window, quit, hide).
pub const ANYWHERE: When = None;
/// Any window while no modal or popover is up: ⌘W. A modal or popover closes on it instead
/// (see [`OVERLAY_KEYS`]).
pub const UNCOVERED: When = Some("!Modal && !Popover");
/// The focused terminal.
pub const TERM: When = Some("Terminal");
/// Any modal or popover.
pub const COVERED: When = Some("Modal || Popover");

/// One rebindable shortcut: its `keys.*` setting and the binding it makes (the catalog in
/// `midna_proto::settings` holds the default and the description).
pub struct Shortcut {
    pub setting: &'static str,
    /// Where it works (see [`When`]).
    pub when: When,
    bind: fn(&str, When) -> KeyBinding,
}

macro_rules! shortcut {
    ($setting:literal, $action:expr, $when:expr) => {
        Shortcut { setting: $setting, when: $when, bind: |k, when| KeyBinding::new(k, $action, when) }
    };
}

/// Every rebindable shortcut. Each has a `keys.*` row in the settings catalog (a test checks).
/// Main-window shortcuts are blocked under a modal or popover ([`WINDOW`]); only the
/// app-level ones work everywhere.
pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!("keys.command_bar", ToggleCommandBar, WINDOW),
    shortcut!("keys.next_needs_you", NextNeedsYou, WINDOW),
    shortcut!("keys.needs_you", OpenNeedsYou, WINDOW),
    shortcut!("keys.new_terminal", NewTerminal, WINDOW),
    shortcut!("keys.new_agent", NewAgent, WINDOW),
    shortcut!("keys.new_terminal_root", NewTerminalRoot, WINDOW),
    shortcut!("keys.new_agent_root", NewAgentRoot, WINDOW),
    shortcut!("keys.approve", ApproveOnce, WINDOW),
    shortcut!("keys.approve_options", ApproveOptions, WINDOW),
    shortcut!("keys.deny", Deny, WINDOW),
    shortcut!("keys.settings", OpenSettings, ANYWHERE),
    shortcut!("keys.rules", OpenRules, WINDOW),
    shortcut!("keys.triggers", OpenTriggers, WINDOW),
    shortcut!("keys.insights", OpenInsights, WINDOW),
    shortcut!("keys.open_project", OpenProject, WINDOW),
    shortcut!("keys.new_window", NewWindow, ANYWHERE),
    shortcut!("keys.composer", crate::composer::OpenComposer, WINDOW),
    shortcut!("keys.add_image", crate::annotate::AddImage, WINDOW),
    shortcut!("keys.edit_attachment", crate::annotate::EditAttachment, WINDOW),
    shortcut!("keys.paste_image_inline", TermPasteImageInline, TERM),
    shortcut!("keys.note_newline", crate::ui::text_input::Newline, Some(crate::ui::text_input::CTX_WRAP)),
    shortcut!("keys.links", crate::ui::links::ToggleLinks, WINDOW),
    shortcut!("keys.subagents", crate::ui::subagents::ToggleSubagents, WINDOW),
    shortcut!("keys.queue", crate::ui::queue::ToggleQueue, WINDOW),
    shortcut!("keys.prev_prompt", PrevPrompt, TERM),
    shortcut!("keys.next_prompt", NextPrompt, TERM),
    shortcut!("keys.prompts", OpenPrompts, WINDOW),
    shortcut!("keys.next_terminal", NextTerminal, WINDOW),
    shortcut!("keys.prev_terminal", PrevTerminal, WINDOW),
    shortcut!("keys.terminal_back", TerminalBack, WINDOW),
    shortcut!("keys.terminal_forward", TerminalForward, WINDOW),
    shortcut!("keys.project_menu", ToggleProjectMenu, WINDOW),
    shortcut!("keys.fold_project", FoldProject, WINDOW),
    shortcut!("keys.project_summary", ShowProjectSummary, WINDOW),
    shortcut!("keys.sidebar", ToggleSidebar, WINDOW),
    shortcut!("keys.rename", RenameSession, WINDOW),
    shortcut!("keys.restart", RestartSession, WINDOW),
    shortcut!("keys.replace", ReplaceSession, WINDOW),
    shortcut!("keys.terminal_menu", ToggleTerminalMenu, WINDOW),
    shortcut!("keys.copy_session_id", CopySessionId, WINDOW),
    shortcut!("keys.mute", ToggleMute, WINDOW),
    shortcut!("keys.clear", TermClear, TERM),
    shortcut!("keys.split", SplitRight, WINDOW),
    shortcut!("keys.split_orientation", ToggleSplitOrientation, WINDOW),
    shortcut!("keys.split_to_main", SplitToMain, WINDOW),
    shortcut!("keys.focus_pane", FocusOtherPane, WINDOW),
    shortcut!("keys.pop_out", PopOut, WINDOW),
    shortcut!("keys.open_ide", crate::ide::OpenInIde, WINDOW),
    shortcut!("keys.choose_ide", crate::ide::ChooseIde, WINDOW),
    shortcut!("keys.keep_on_top", ToggleKeepOnTop, WINDOW),
    shortcut!("keys.close", CloseWindow, UNCOVERED),
    shortcut!("keys.quit", HoldToQuit, ANYWHERE),
];

/// What a shortcut's keys do inside one overlay, where the main-window binding is blocked
/// (rebinding the shortcut moves these too). Bound after [`SHORTCUTS`]: two bindings that
/// match at the same depth go to the later one, so the image sheet's ⌘W beats the
/// modal-wide one.
pub const OVERLAY_KEYS: &[Shortcut] = &[
    // ⌘W closes a modal or popover, like escape.
    shortcut!("keys.close", Dismiss, COVERED),
    // A popover's own shortcut closes it again.
    shortcut!("keys.command_bar", ToggleCommandBar, Some("overlay == command_bar")),
    shortcut!("keys.needs_you", OpenNeedsYou, Some("overlay == needs_you")),
    shortcut!("keys.links", crate::ui::links::ToggleLinks, Some("overlay == links")),
    shortcut!("keys.queue", crate::ui::queue::ToggleQueue, Some("overlay == queue")),
    shortcut!("keys.subagents", crate::ui::subagents::ToggleSubagents, Some("overlay == subagents")),
    shortcut!("keys.choose_ide", crate::ide::ChooseIde, Some("overlay == ide")),
    // The needs-you cards answer the card on screen.
    shortcut!("keys.approve", ApproveOnce, Some("overlay == needs_you")),
    shortcut!("keys.deny", Deny, Some("overlay == needs_you")),
    shortcut!("keys.next_needs_you", NextNeedsYou, Some("overlay == needs_you")),
    shortcut!("keys.approve", crate::ui::command_bar::RunSelected, Some("overlay == command_bar")),
    shortcut!("keys.deny", crate::ui::command_bar::ClearQuery, Some("overlay == command_bar")),
    shortcut!("keys.approve", crate::annotate::AttachImages, Some("overlay == annotate")),
    shortcut!("keys.close", crate::annotate::RemoveImage, Some("overlay == annotate")),
];

/// Keys that aren't settings: standard editing keys and the keys a screen handles itself.
/// Listed in Settings ▸ Shortcuts so a key search finds them too.
pub struct Fixed {
    /// Every keystroke this row covers (what a key search matches).
    pub keys: &'static [&'static str],
    /// Shown instead of the keys when they're a range (⌘1–9).
    pub label: Option<&'static str>,
    pub title: &'static str,
    /// Where it works.
    pub place: &'static str,
}

const fn fixed(keys: &'static [&'static str], title: &'static str, place: &'static str) -> Fixed {
    Fixed { keys, label: None, title, place }
}

pub const FIXED: &[Fixed] = &[
    Fixed {
        keys: &["cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8", "cmd-9"],
        label: Some("⌘1–9"),
        title: "Jump to project 1–9",
        place: "Main window",
    },
    fixed(&["escape"], "Close the overlay, or go back to the terminal", "Main window"),
    fixed(&["cmd-h"], "Hide midna", "Anywhere"),
    fixed(&["cmd-alt-h"], "Hide other apps", "Anywhere"),
    fixed(&["cmd-c"], "Copy the selection", "Terminal"),
    fixed(&["cmd-v"], "Paste (an image on the clipboard opens the image sheet, or pastes its path with terminal.image_paste = inline; a secret pasted into an agent asks to store it)", "Terminal"),
    fixed(&["cmd-alt-v"], "Paste as Secret: store the clipboard in the Keychain, paste [secret:NAME]", "Terminal"),
    fixed(&["cmd-a"], "Select all", "Terminal"),
    fixed(&["cmd-z", "cmd-shift-z", "cmd-y"], "Undo / redo in Claude Code's input", "Terminal"),
    fixed(&["cmd-f"], "Find in the terminal", "Terminal"),
    fixed(&["cmd-g", "enter"], "Find: older match", "Terminal find"),
    fixed(&["cmd-shift-g", "shift-enter"], "Find: newer match", "Terminal find"),
    fixed(&["cmd-up", "cmd-home"], "Scroll to the top", "Terminal"),
    fixed(&["cmd-down", "cmd-end"], "Scroll to the bottom", "Terminal"),
    fixed(&["cmd-pageup", "cmd-pagedown"], "Scroll a page", "Terminal"),
    fixed(&["up", "down"], "Move the selection", "Command bar, links, lists"),
    fixed(&["enter"], "Run the selected command", "Command bar"),
    fixed(&["shift-enter"], "Ask an agent instead", "Command bar"),
    fixed(&["tab"], "Switch agent (Claude / Codex)", "Command bar"),
    fixed(&["shift-tab"], "Switch between project and root", "Command bar"),
    fixed(&["shift-enter"], "Pin or unpin the selected link", "Links"),
    fixed(&["alt-enter"], "Jump to where the link came up", "Links"),
    fixed(&["tab", "shift-tab"], "Next / previous tab", "Links"),
    fixed(&["enter"], "Queue the message, or edit the selected one", "Queue"),
    fixed(&["shift-enter"], "Send the selected message now", "Queue"),
    fixed(&["alt-up", "alt-down"], "Move the selected message", "Queue"),
    fixed(&["alt-backspace"], "Remove the selected message", "Queue"),
    fixed(&["tab"], "Change when it goes in", "Queue"),
    fixed(&["cmd-o"], "Open the card's terminal", "Needs-you cards"),
    fixed(&["cmd-shift-enter"], "Approve all", "Needs-you cards"),
    fixed(&["enter"], "Edit the selected note", "Image sheet"),
    fixed(&["backspace"], "Remove the selected note", "Image sheet"),
    fixed(&["cmd-z", "cmd-shift-z"], "Undo / redo in the note being edited", "Image sheet"),
    fixed(&["cmd-z"], "Put back the last removed note or image", "Image sheet"),
    fixed(&["cmd-up", "cmd-down"], "Previous / next image", "Image sheet"),
    fixed(&["cmd-w"], "Remove the image, or close the sheet when there's none", "Image sheet"),
    fixed(&["cmd-=", "cmd--", "cmd-0"], "Zoom in / out / fit", "Image sheet"),
    Fixed { keys: &["cmd"], label: Some("⌘ scroll · pinch"), title: "Zoom about the pointer", place: "Image sheet" },
    Fixed { keys: &["alt"], label: Some("⌥ drag · scroll"), title: "Pan the image (a middle-drag too)", place: "Image sheet" },
    fixed(&["cmd-v"], "Add the image on the clipboard", "Image sheet"),
    fixed(&["enter"], "Send to the terminal", "Composer"),
    fixed(&["cmd-z", "cmd-shift-z"], "Undo / redo", "Composer"),
    fixed(&["cmd-k", "cmd-f"], "Search settings (↩ asks an agent instead)", "Settings"),
    fixed(&["cmd-[", "cmd-]"], "Previous / next section", "Settings"),
    fixed(&["cmd-alt-k"], "Record keys: find a shortcut by pressing it", "Settings"),
];

/// The keys bound to each `keys.*` setting right now (normalized), for tooltips and labels.
#[derive(Default)]
pub struct BoundKeys(HashMap<&'static str, String>);
impl Global for BoundKeys {}

/// The catalog default for a `keys.*` setting ("" when unbound or unknown).
pub fn default_key(setting: &str) -> &'static str {
    match midna_proto::settings::setting(setting).map(|s| s.default) {
        Some(midna_proto::settings::DefaultValue::Str(s)) => s,
        _ => "",
    }
}

/// Pretty keys for a `keys.*` setting as bound now ("" when unbound).
pub fn label(cx: &App, setting: &str) -> String {
    let keys = cx.try_global::<BoundKeys>().and_then(|b| b.0.get(setting).cloned()).unwrap_or_else(|| default_key(setting).to_string());
    if keys.is_empty() { String::new() } else { pretty(&keys) }
}

fn valid(keys: &str) -> bool {
    !keys.trim().is_empty() && keys.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// Rebuild all keybindings from the current settings (`get` returns a `keys.*` value).
pub fn bind_keys(cx: &mut App, get: impl Fn(&str) -> Option<String>) {
    cx.clear_key_bindings();
    let mut bound = BoundKeys::default();
    let mut b: Vec<KeyBinding> = Vec::new();
    for s in SHORTCUTS {
        let k = normalize_keys(&get(s.setting).unwrap_or_else(|| default_key(s.setting).to_string()));
        if valid(&k) {
            b.push((s.bind)(&k, s.when));
            bound.0.insert(s.setting, k);
        } else {
            if !k.is_empty() {
                eprintln!("midna-app: ignoring invalid keybinding {k:?}");
            }
            bound.0.insert(s.setting, String::new());
        }
    }
    b.extend(crate::composer::bindings());
    for i in 1..=9 {
        b.push(KeyBinding::new(&format!("cmd-{i}"), SelectProject(i - 1), WINDOW));
    }
    b.push(KeyBinding::new("escape", Dismiss, Some(CTX_OVERLAY)));
    b.push(KeyBinding::new("cmd-c", TermCopy, TERM));
    b.push(KeyBinding::new("cmd-x", TermCut, TERM));
    b.push(KeyBinding::new("cmd-v", TermPaste, TERM));
    b.push(KeyBinding::new("cmd-alt-v", TermPasteSecret, TERM));
    b.push(KeyBinding::new("cmd-a", TermSelectAll, TERM));
    b.push(KeyBinding::new("cmd-h", Hide, ANYWHERE));
    b.push(KeyBinding::new("cmd-alt-h", HideOthers, ANYWHERE));
    for s in OVERLAY_KEYS {
        if let Some(k) = bound.0.get(s.setting).filter(|k| !k.is_empty()) {
            b.push((s.bind)(k, s.when));
        }
    }
    cx.bind_keys(b);
    cx.set_global(bound);
}

/// Whether a binding (`keys`, any format) is exactly the recorded keystrokes (normalized).
pub fn keys_match(keys: &str, recorded: &[String]) -> bool {
    let strokes: Vec<String> = normalize_keys(keys).split_whitespace().map(canonical).collect();
    !recorded.is_empty() && strokes.len() >= recorded.len() && strokes.iter().zip(recorded).all(|(a, b)| *a == canonical(b))
}

/// "alt-cmd-k" and "cmd-alt-k" are the same keystroke: sort the modifiers.
fn canonical(stroke: &str) -> String {
    match Keystroke::parse(stroke) {
        Ok(k) => keystroke_text(&k),
        Err(_) => stroke.to_string(),
    }
}

/// A keystroke as the settings catalog writes it ("cmd-alt-shift-t", "ctrl-shift-tab").
pub fn setting_keys(k: &Keystroke) -> String {
    let m = &k.modifiers;
    let mut out = String::new();
    for (on, name) in [(m.platform, "cmd-"), (m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-")] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&k.key);
    out
}

/// A keystroke in one fixed modifier order (ctrl-alt-shift-cmd), for comparing.
pub fn keystroke_text(k: &Keystroke) -> String {
    let m = &k.modifiers;
    let mut out = String::new();
    for (on, name) in [(m.control, "ctrl-"), (m.alt, "alt-"), (m.shift, "shift-"), (m.platform, "cmd-")] {
        if on {
            out.push_str(name);
        }
    }
    out.push_str(&k.key);
    out
}

/// Named punctuation keys (the daemon's catalog says `cmd-comma`) -> the characters GPUI
/// matches (`cmd-,`).
pub fn normalize_keys(keys: &str) -> String {
    keys.split_whitespace()
        .map(|stroke| match stroke.rsplit_once('-') {
            Some((mods, key)) => {
                let k = match key {
                    "comma" => ",",
                    "period" => ".",
                    "slash" => "/",
                    "semicolon" => ";",
                    "quote" => "'",
                    "backslash" => "\\",
                    "backtick" => "`",
                    "bracketleft" => "[",
                    "bracketright" => "]",
                    "equal" => "=",
                    k => k,
                };
                format!("{mods}-{k}")
            }
            None => stroke.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "cmd-shift-t" -> "⇧⌘T" for display.
pub fn pretty(keys: &str) -> String {
    if keys.trim().is_empty() {
        return String::new();
    }
    let keys = &normalize_keys(keys);
    let mut out = String::new();
    for (i, stroke) in keys.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // the key itself can be "-" ("cmd--")
        let (mods, key): (Vec<&str>, &str) = match stroke.strip_suffix("--") {
            Some(m) => (m.split('-').collect(), "-"),
            None if stroke == "-" => (vec![], "-"),
            None => match stroke.rsplit_once('-') {
                Some((m, k)) => (m.split('-').collect(), k),
                None => (vec![], stroke),
            },
        };
        let has = |m: &str| mods.contains(&m);
        if has("ctrl") {
            out.push('⌃');
        }
        if has("alt") {
            out.push('⌥');
        }
        if has("shift") {
            out.push('⇧');
        }
        if has("cmd") {
            out.push('⌘');
        }
        out.push_str(&match key {
            "enter" => "↩".to_string(),
            "backspace" => "⌫".to_string(),
            "escape" => "⎋".to_string(),
            "space" => "Space".to_string(),
            "tab" => "⇥".to_string(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "left" => "←".into(),
            "right" => "→".into(),
            "pageup" => "PgUp".into(),
            "pagedown" => "PgDn".into(),
            "home" => "Home".into(),
            "end" => "End".into(),
            "delete" => "⌦".into(),
            k => k.to_uppercase(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn pretty_keys() {
        assert_eq!(super::pretty("cmd-enter"), "⌘↩");
        assert_eq!(super::pretty("cmd-shift-t"), "⇧⌘T");
        assert_eq!(super::pretty("cmd-backspace"), "⌘⌫");
        assert_eq!(super::pretty("cmd-,"), "⌘,");
        assert_eq!(super::pretty("cmd-comma"), "⌘,");
        assert_eq!(super::normalize_keys("cmd-shift-comma"), "cmd-shift-,");
        let ks = gpui_kit::Keystroke::parse(&super::normalize_keys("cmd-comma")).unwrap();
        assert_eq!((ks.key.as_str(), ks.modifiers.platform), (",", true));
    }

    #[test]
    fn when_clauses_parse() {
        for s in super::SHORTCUTS.iter().chain(super::OVERLAY_KEYS) {
            if let Some(w) = s.when {
                assert!(gpui_kit::KeyBindingContextPredicate::parse(w).is_ok(), "{}: {w:?}", s.setting);
            }
        }
    }

    #[test]
    fn overlay_keys_name_real_overlays() {
        for s in super::OVERLAY_KEYS {
            let Some(id) = s.when.and_then(|w| w.strip_prefix("overlay == ")) else { continue };
            assert!(super::overlay::ALL.contains(&id), "{}: no overlay {id:?}", s.setting);
        }
    }
}

/// The default bindings in a window shaped like the main one: `MidnaMain` with an overlay in
/// it that has focus. Each test presses keys and reads which actions ran.
#[cfg(test)]
mod dispatch {
    // Not `super::*`: GPUI's test macro would come in with it and shadow `#[test]`.
    use super::{
        ApproveOnce, CTX_MAIN, CloseWindow, Dismiss, HoldToQuit, Kind, NewAgent, OpenSettings, SelectProject, SplitRight,
        bind_keys, overlay, overlay_context,
    };
    use crate::annotate::{AttachImages, RemoveImage};
    use crate::ui::links::ToggleLinks;
    use gpui_kit::prelude::*;
    use gpui_kit::{Action, App, Context, FocusHandle, TestAppContext, Window, div};
    use std::cell::RefCell;
    use std::rc::Rc;

    type Log = Rc<RefCell<Vec<&'static str>>>;

    struct Probe {
        kind: Kind,
        id: &'static str,
        focus: FocusHandle,
        log: Log,
    }

    fn record<A: Action>(log: &Log, name: &'static str) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let log = log.clone();
        move |_, _, _| log.borrow_mut().push(name)
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let l = &self.log;
            div()
                .key_context(CTX_MAIN)
                .size_full()
                .on_action(record::<NewAgent>(l, "new_agent"))
                .on_action(record::<SplitRight>(l, "split"))
                .on_action(record::<SelectProject>(l, "select_project"))
                .on_action(record::<ApproveOnce>(l, "approve"))
                .on_action(record::<CloseWindow>(l, "close_window"))
                .on_action(record::<OpenSettings>(l, "settings"))
                .on_action(record::<HoldToQuit>(l, "quit"))
                .child(
                    div()
                        .key_context(overlay_context(self.kind, self.id))
                        .track_focus(&self.focus)
                        .size_full()
                        .on_action(record::<RemoveImage>(l, "remove_image"))
                        .on_action(record::<AttachImages>(l, "attach"))
                        .on_action(record::<Dismiss>(l, "dismiss"))
                        .on_action(record::<ToggleLinks>(l, "toggle_links")),
                )
        }
    }

    /// Opens the window with the overlay focused, then presses each key and collects what ran.
    fn press(kind: Kind, id: &'static str, keys: &[&str], cx: &mut TestAppContext) -> Vec<(String, Vec<&'static str>)> {
        cx.update(|cx| bind_keys(cx, |_| None));
        let log = Log::default();
        let l = log.clone();
        let window = cx.add_window(|window, cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            Probe { kind, id, focus, log: l }
        });
        cx.run_until_parked();
        keys.iter()
            .map(|k| {
                cx.simulate_keystrokes(window.into(), k);
                (k.to_string(), std::mem::take(&mut *log.borrow_mut()))
            })
            .collect()
    }

    fn ran(k: &str, actions: &[&'static str]) -> (String, Vec<&'static str>) {
        (k.to_string(), actions.to_vec())
    }

    #[gpui_kit::test]
    fn image_sheet_blocks_main_shortcuts(cx: &mut TestAppContext) {
        let got = press(Kind::Modal, overlay::ANNOTATE, &["cmd-t", "cmd-d", "cmd-1", "cmd-w", "cmd-enter", "cmd-q", "cmd-,", "escape"], cx);
        assert_eq!(
            got,
            vec![
                ran("cmd-t", &[]),
                ran("cmd-d", &[]),
                ran("cmd-1", &[]),
                ran("cmd-w", &["remove_image"]),
                ran("cmd-enter", &["attach"]),
                ran("cmd-q", &["quit"]),
                ran("cmd-,", &["settings"]),
                ran("escape", &["dismiss"]),
            ]
        );
    }

    #[gpui_kit::test]
    fn popover_blocks_main_shortcuts(cx: &mut TestAppContext) {
        let got = press(Kind::Popover, overlay::LINKS, &["cmd-t", "cmd-enter", "cmd-w", "cmd-l", "cmd-,"], cx);
        assert_eq!(
            got,
            vec![
                ran("cmd-t", &[]),
                ran("cmd-enter", &[]),
                ran("cmd-w", &["dismiss"]),
                ran("cmd-l", &["toggle_links"]),
                ran("cmd-,", &["settings"]),
            ]
        );
    }

    #[gpui_kit::test]
    fn screens_keep_main_shortcuts(cx: &mut TestAppContext) {
        let got = press(Kind::Screen, overlay::RULES, &["cmd-t", "cmd-1", "cmd-w"], cx);
        assert_eq!(got, vec![ran("cmd-t", &["new_agent"]), ran("cmd-1", &["select_project"]), ran("cmd-w", &["close_window"])]);
    }
}
