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
        /// Open the needs-you card stack (NeedsYou-C). Placeholder overlay for now.
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
        /// ⌘W: in the main window, close the selected terminal (the window when none is
        /// selected); in Settings or a pop-out, close that window.
        CloseWindow,
        Quit,
        /// ⌘Q: hold for 2.5 s to quit (see `ui::quit_hold`). The menu item quits at once.
        HoldToQuit,
        // terminal
        TermCopy,
        TermPaste,
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
        /// Rename the selected terminal in the header.
        RenameSession,
        /// The header's … menu.
        ToggleTerminalMenu,
        CopySessionId,
        /// Mute / unmute notifications for the selected terminal.
        ToggleMute,
        /// Split: side by side ⇄ stacked.
        ToggleSplitOrientation,
        /// Show the split's terminal in the main pane.
        SplitToMain,
        /// Focus the other split pane.
        FocusOtherPane,
        /// Pop-out window: keep on top.
        ToggleKeepOnTop,
        /// The current project's … menu / fold its group in the sidebar.
        ToggleProjectMenu,
        FoldProject,
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

/// One rebindable shortcut: its `keys.*` setting and the binding it makes (the catalog in
/// `midna_proto::settings` holds the default and the description).
pub struct Shortcut {
    pub setting: &'static str,
    bind: fn(&str) -> KeyBinding,
}

macro_rules! shortcut {
    ($setting:literal, $action:expr, $ctx:expr) => {
        Shortcut { setting: $setting, bind: |k| KeyBinding::new(k, $action, $ctx) }
    };
}

const MAIN: Option<&str> = Some(CTX_MAIN);
const TERM: Option<&str> = Some(CTX_TERM);

/// Every rebindable shortcut. Each has a `keys.*` row in the settings catalog (a test checks).
pub const SHORTCUTS: &[Shortcut] = &[
    shortcut!("keys.command_bar", ToggleCommandBar, MAIN),
    shortcut!("keys.next_needs_you", NextNeedsYou, MAIN),
    shortcut!("keys.needs_you", OpenNeedsYou, MAIN),
    shortcut!("keys.new_terminal", NewTerminal, MAIN),
    shortcut!("keys.new_agent", NewAgent, MAIN),
    shortcut!("keys.new_terminal_root", NewTerminalRoot, MAIN),
    shortcut!("keys.new_agent_root", NewAgentRoot, MAIN),
    shortcut!("keys.approve", ApproveOnce, MAIN),
    shortcut!("keys.approve_options", ApproveOptions, MAIN),
    shortcut!("keys.deny", Deny, MAIN),
    shortcut!("keys.settings", OpenSettings, None),
    shortcut!("keys.rules", OpenRules, MAIN),
    shortcut!("keys.triggers", OpenTriggers, MAIN),
    shortcut!("keys.insights", OpenInsights, MAIN),
    shortcut!("keys.open_project", OpenProject, MAIN),
    shortcut!("keys.new_window", NewWindow, None),
    shortcut!("keys.composer", crate::composer::OpenComposer, MAIN),
    shortcut!("keys.add_image", crate::annotate::AddImage, MAIN),
    shortcut!("keys.edit_attachment", crate::annotate::EditAttachment, MAIN),
    shortcut!("keys.links", crate::ui::links::ToggleLinks, MAIN),
    shortcut!("keys.subagents", crate::ui::subagents::ToggleSubagents, MAIN),
    shortcut!("keys.queue", crate::ui::queue::ToggleQueue, MAIN),
    shortcut!("keys.prev_prompt", PrevPrompt, TERM),
    shortcut!("keys.next_prompt", NextPrompt, TERM),
    shortcut!("keys.prompts", OpenPrompts, MAIN),
    shortcut!("keys.next_terminal", NextTerminal, MAIN),
    shortcut!("keys.prev_terminal", PrevTerminal, MAIN),
    shortcut!("keys.project_menu", ToggleProjectMenu, MAIN),
    shortcut!("keys.fold_project", FoldProject, MAIN),
    shortcut!("keys.sidebar", ToggleSidebar, MAIN),
    shortcut!("keys.rename", RenameSession, MAIN),
    shortcut!("keys.restart", RestartSession, MAIN),
    shortcut!("keys.terminal_menu", ToggleTerminalMenu, MAIN),
    shortcut!("keys.copy_session_id", CopySessionId, MAIN),
    shortcut!("keys.mute", ToggleMute, MAIN),
    shortcut!("keys.clear", TermClear, TERM),
    shortcut!("keys.split", SplitRight, MAIN),
    shortcut!("keys.split_orientation", ToggleSplitOrientation, MAIN),
    shortcut!("keys.split_to_main", SplitToMain, MAIN),
    shortcut!("keys.focus_pane", FocusOtherPane, MAIN),
    shortcut!("keys.pop_out", PopOut, MAIN),
    shortcut!("keys.keep_on_top", ToggleKeepOnTop, MAIN),
    shortcut!("keys.close", CloseWindow, None),
    shortcut!("keys.quit", HoldToQuit, None),
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
    fixed(&["cmd-c"], "Copy the selection", "Terminal"),
    fixed(&["cmd-v"], "Paste (an image on the clipboard opens the image sheet)", "Terminal"),
    fixed(&["cmd-a"], "Select all", "Terminal"),
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
    fixed(&["p"], "Pin tool", "Image sheet"),
    fixed(&["b"], "Box tool", "Image sheet"),
    fixed(&["enter"], "Edit the selected note", "Image sheet"),
    fixed(&["backspace"], "Remove the selected note", "Image sheet"),
    fixed(&["cmd-up", "cmd-down"], "Previous / next image", "Image sheet"),
    fixed(&["cmd-=", "cmd--", "cmd-0"], "Zoom in / out / fit", "Image sheet"),
    fixed(&["cmd-v"], "Add the image on the clipboard", "Image sheet"),
    fixed(&["enter"], "Send to the terminal", "Composer"),
    fixed(&["cmd-z", "cmd-shift-z"], "Undo / redo", "Composer"),
    fixed(&["cmd-1", "cmd-2", "cmd-3"], "Rows / settings.json / Shortcuts", "Settings"),
    fixed(&["cmd-k"], "Ask an agent to change a setting", "Settings"),
    fixed(&["cmd-f"], "Search shortcuts", "Settings"),
    fixed(&["cmd-alt-k"], "Record keys: search by pressing a shortcut", "Settings"),
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
            b.push((s.bind)(&k));
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
        b.push(KeyBinding::new(&format!("cmd-{i}"), SelectProject(i - 1), MAIN));
    }
    b.push(KeyBinding::new("escape", Dismiss, Some("MidnaOverlay")));
    b.push(KeyBinding::new("cmd-c", TermCopy, TERM));
    b.push(KeyBinding::new("cmd-v", TermPaste, TERM));
    b.push(KeyBinding::new("cmd-a", TermSelectAll, TERM));
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
}
