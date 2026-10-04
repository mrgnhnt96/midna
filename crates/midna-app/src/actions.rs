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
    ]
);

/// ⌘1–9: select project N (0-based index into the sidebar order).
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = midna, no_json)]
pub struct SelectProject(pub usize);

pub const CTX_MAIN: &str = "MidnaMain";
pub const CTX_TERM: &str = "Terminal";

/// Default `keys.*` values (the daemon's settings catalog is the source of truth).
pub fn default_keys() -> HashMap<&'static str, &'static str> {
    [
        ("keys.command_bar", "cmd-k"),
        ("keys.next_needs_you", "cmd-j"),
        ("keys.new_terminal", "cmd-t"),
        ("keys.new_agent", "cmd-shift-t"),
        ("keys.new_terminal_root", "cmd-alt-t"),
        ("keys.new_agent_root", "cmd-alt-shift-t"),
        ("keys.approve", "cmd-enter"),
        ("keys.deny", "cmd-backspace"),
        ("keys.settings", "cmd-,"),
        ("keys.rules", ""),
        ("keys.triggers", ""),
        ("keys.insights", ""),
        ("keys.open_project", "cmd-o"),
        ("keys.composer", "cmd-shift-d"),
        ("keys.add_image", "cmd-i"),
    ]
    .into_iter()
    .collect()
}

fn valid(keys: &str) -> bool {
    !keys.trim().is_empty() && keys.split_whitespace().all(|k| Keystroke::parse(k).is_ok())
}

/// Rebuild all keybindings from the current settings (`get` returns a `keys.*` value).
pub fn bind_keys(cx: &mut App, get: impl Fn(&str) -> Option<String>) {
    cx.clear_key_bindings();
    let defaults = default_keys();
    let key = |k: &str| normalize_keys(&get(k).unwrap_or_else(|| defaults.get(k).copied().unwrap_or("").to_string()));
    let mut b: Vec<KeyBinding> = Vec::new();
    let ctx = Some(CTX_MAIN);
    let mut add = |k: String, f: &dyn Fn(&str) -> KeyBinding| {
        if valid(&k) {
            b.push(f(&k));
        } else if !k.is_empty() {
            eprintln!("midna-app: ignoring invalid keybinding {k:?}");
        }
    };
    add(key("keys.command_bar"), &|k| KeyBinding::new(k, ToggleCommandBar, ctx));
    add(key("keys.next_needs_you"), &|k| KeyBinding::new(k, NextNeedsYou, ctx));
    add(key("keys.new_terminal"), &|k| KeyBinding::new(k, NewTerminal, ctx));
    add(key("keys.new_agent"), &|k| KeyBinding::new(k, NewAgent, ctx));
    add(key("keys.new_terminal_root"), &|k| KeyBinding::new(k, NewTerminalRoot, ctx));
    add(key("keys.new_agent_root"), &|k| KeyBinding::new(k, NewAgentRoot, ctx));
    add(key("keys.approve"), &|k| KeyBinding::new(k, ApproveOnce, ctx));
    add(key("keys.deny"), &|k| KeyBinding::new(k, Deny, ctx));
    add(key("keys.settings"), &|k| KeyBinding::new(k, OpenSettings, None));
    add(key("keys.rules"), &|k| KeyBinding::new(k, OpenRules, ctx));
    add(key("keys.triggers"), &|k| KeyBinding::new(k, OpenTriggers, ctx));
    add(key("keys.insights"), &|k| KeyBinding::new(k, OpenInsights, ctx));
    add(key("keys.open_project"), &|k| KeyBinding::new(k, OpenProject, ctx));
    add(key("keys.composer"), &|k| KeyBinding::new(k, crate::composer::OpenComposer, ctx));
    add(key("keys.add_image"), &|k| KeyBinding::new(k, crate::annotate::AddImage, ctx));
    b.extend(crate::composer::bindings());
    for i in 1..=9 {
        b.push(KeyBinding::new(&format!("cmd-{i}"), SelectProject(i - 1), ctx));
    }
    b.push(KeyBinding::new("escape", Dismiss, Some("MidnaOverlay")));
    b.push(KeyBinding::new("cmd-d", SplitRight, ctx));
    b.push(KeyBinding::new("cmd-e", crate::annotate::EditAttachment, ctx));
    b.push(KeyBinding::new("cmd-q", HoldToQuit, None));
    b.push(KeyBinding::new("cmd-w", CloseWindow, None));
    b.push(KeyBinding::new("cmd-c", TermCopy, Some(CTX_TERM)));
    b.push(KeyBinding::new("cmd-v", TermPaste, Some(CTX_TERM)));
    b.push(KeyBinding::new("cmd-a", TermSelectAll, Some(CTX_TERM)));
    cx.bind_keys(b);
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
    let keys = &normalize_keys(keys);
    let mut out = String::new();
    for (i, stroke) in keys.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let parts: Vec<&str> = stroke.split('-').collect();
        let (mods, key) = match parts.split_last() {
            Some((k, m)) if !k.is_empty() => (m.to_vec(), *k),
            _ => (vec![], "-"),
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
