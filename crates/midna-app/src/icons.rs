//! SVG icons from the design boards, served to GPUI's `svg()` through an AssetSource.
//! GPUI renders SVGs as an alpha mask tinted with `text_color`, so `currentColor` works.
use gpui_kit::prelude::*;
use gpui_kit::*;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Claude,
    Codex,
    Monitor,
    Shell,
    Dots,
    Branch,
    Pr,
    Split,
    PopOut,
    /// Opposite of `PopOut`: put a popped-out terminal back in the main window.
    DockIn,
    Restart,
    Triggers,
    Rules,
    Settings,
    Chevron,
    // command bar / needs-you (CommandBar-A, NeedsYou-C)
    Project,
    Play,
    Screen,
    Check,
    Cross,
    Plus,
    Search,
    Lock,
    Bell,
    /// A muted terminal (no notifications).
    BellOff,
    // image annotations
    Image,
    Pin,
    Area,
    Trash,
    // session links
    Link,
    Artifact,
    Globe,
    File,
    PushPin,
    PushPinFill,
    /// Settings ▸ Shortcuts: record keys to search by.
    Keyboard,
    // queued messages
    Queue,
    SendNow,
    Pencil,
    Pause,
    /// Collapse / expand the sidebar.
    Sidebar,
    /// Subagents (a body with a small satellite).
    Orbit,
}

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Icon::Claude => "icons/claude.svg",
            Icon::Codex => "icons/codex.svg",
            Icon::Monitor => "icons/monitor.svg",
            Icon::Shell => "icons/shell.svg",
            Icon::Dots => "icons/dots.svg",
            Icon::Branch => "icons/branch.svg",
            Icon::Pr => "icons/pr.svg",
            Icon::Split => "icons/split.svg",
            Icon::PopOut => "icons/popout.svg",
            Icon::DockIn => "icons/dockin.svg",
            Icon::Restart => "icons/restart.svg",
            Icon::Triggers => "icons/triggers.svg",
            Icon::Rules => "icons/rules.svg",
            Icon::Settings => "icons/settings.svg",
            Icon::Chevron => "icons/chevron.svg",
            Icon::Project => "icons/project.svg",
            Icon::Play => "icons/play.svg",
            Icon::Screen => "icons/screen.svg",
            Icon::Check => "icons/check.svg",
            Icon::Cross => "icons/cross.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::Search => "icons/search.svg",
            Icon::Lock => "icons/lock.svg",
            Icon::Bell => "icons/bell.svg",
            Icon::BellOff => "icons/bell-off.svg",
            Icon::Image => "icons/image.svg",
            Icon::Pin => "icons/pin.svg",
            Icon::Area => "icons/area.svg",
            Icon::Trash => "icons/trash.svg",
            Icon::Link => "icons/link.svg",
            Icon::Artifact => "icons/artifact.svg",
            Icon::Globe => "icons/globe.svg",
            Icon::File => "icons/file.svg",
            Icon::PushPin => "icons/pushpin.svg",
            Icon::PushPinFill => "icons/pushpin-fill.svg",
            Icon::Keyboard => "icons/keyboard.svg",
            Icon::Queue => "icons/queue.svg",
            Icon::SendNow => "icons/sendnow.svg",
            Icon::Pencil => "icons/pencil.svg",
            Icon::Pause => "icons/pause.svg",
            Icon::Sidebar => "icons/sidebar.svg",
            Icon::Orbit => "icons/orbit.svg",
        }
    }

    pub fn from_glyph(g: crate::model::Glyph) -> Icon {
        match g {
            crate::model::Glyph::Claude => Icon::Claude,
            crate::model::Glyph::Codex => Icon::Codex,
            crate::model::Glyph::Monitor => Icon::Monitor,
            crate::model::Glyph::Shell => Icon::Shell,
        }
    }

    /// An icon a custom status names (`set_status --icon`), when this set has it.
    pub fn from_name(name: &str) -> Option<Icon> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "bell" => Icon::Bell,
            "bell-off" | "bell_off" | "mute" => Icon::BellOff,
            "lock" => Icon::Lock,
            "check" => Icon::Check,
            "cross" | "x" => Icon::Cross,
            "restart" | "refresh" => Icon::Restart,
            "branch" => Icon::Branch,
            "pr" => Icon::Pr,
            "bolt" | "trigger" | "triggers" => Icon::Triggers,
            "shell" | "terminal" => Icon::Shell,
            "monitor" => Icon::Monitor,
            "pin" => Icon::PushPin,
            "link" => Icon::Link,
            "globe" => Icon::Globe,
            "file" => Icon::File,
            "search" => Icon::Search,
            "play" => Icon::Play,
            "claude" => Icon::Claude,
            "codex" => Icon::Codex,
            "rules" | "shield" => Icon::Rules,
            "keyboard" => Icon::Keyboard,
            "queue" => Icon::Queue,
            "orbit" | "subagent" | "subagents" => Icon::Orbit,
            _ => return None,
        })
    }

    pub fn el(self, size: f32, color: Hsla) -> Svg {
        svg().path(self.path()).size(px(size)).flex_none().text_color(color)
    }
}

fn source(path: &str) -> Option<&'static str> {
    // Paths copied verbatim from docs/design/Main.dc.html.
    Some(match path {
        "icons/claude.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round"><path d="M8 1.5v13M1.5 8h13M3.4 3.4l9.2 9.2M12.6 3.4l-9.2 9.2"/></svg>"##
        }
        "icons/codex.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round"><path d="M8 1.5 14 5v6l-6 3.5L2 11V5z"/><circle cx="8" cy="8" r="2"/></svg>"##
        }
        "icons/monitor.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M1.5 8h3l2-4.5 3 9 2-4.5h3"/></svg>"##
        }
        "icons/shell.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 4.5 6 8l-3.5 3.5M8 11.5h5.5"/></svg>"##
        }
        "icons/dots.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="#000"><circle cx="3" cy="8" r="1.4"/><circle cx="8" cy="8" r="1.4"/><circle cx="13" cy="8" r="1.4"/></svg>"##
        }
        "icons/branch.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5"><circle cx="4.5" cy="3.5" r="1.8"/><circle cx="4.5" cy="12.5" r="1.8"/><circle cx="11.5" cy="5.5" r="1.8"/><path d="M4.5 5.3v5.4M11.5 7.3c0 3-7 2-7 3.4"/></svg>"##
        }
        "icons/pr.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5"><circle cx="4" cy="3.5" r="1.8"/><circle cx="4" cy="12.5" r="1.8"/><circle cx="12" cy="12.5" r="1.8"/><path d="M4 5.3v5.4M12 10.7V6.5a2 2 0 0 0-2-2H7.5M9 3 7.5 4.5 9 6"/></svg>"##
        }
        "icons/split.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M8 2.5v11"/></svg>"##
        }
        "icons/popout.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><rect x="1.5" y="5.5" width="9" height="9" rx="1.5"/><path d="M9 1.5h5.5V7M14.5 1.5 8 8"/></svg>"##
        }
        "icons/dockin.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><rect x="1.5" y="5.5" width="9" height="9" rx="1.5"/><path d="M8 2.5V8h5.5M14.5 1.5 8 8"/></svg>"##
        }
        "icons/restart.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><path d="M13.5 8a5.5 5.5 0 1 1-1.6-3.9M13.5 2v3h-3"/></svg>"##
        }
        "icons/triggers.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round"><path d="M9 1.5 3.5 9H8l-1 5.5L12.5 7H8z"/></svg>"##
        }
        "icons/rules.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round"><path d="M8 1.5 13.5 3.5v4c0 3.2-2.3 5.6-5.5 7-3.2-1.4-5.5-3.8-5.5-7v-4z"/><path d="M5.8 8.2 7.4 9.8 10.4 6.6"/></svg>"##
        }
        "icons/settings.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><circle cx="8" cy="8" r="2.2"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/></svg>"##
        }
        "icons/queue.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round"><path d="M3 4h10M3 8h10M3 12h6"/></svg>"##
        }
        "icons/sendnow.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M8 13V3M3.5 7.5 8 3l4.5 4.5"/></svg>"##
        }
        "icons/pencil.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linejoin="round"><path d="M2.5 13.5l.6-3L11 2.6l2.4 2.4-7.9 7.9z"/></svg>"##
        }
        "icons/pause.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round"><path d="M5.5 3.5v9M10.5 3.5v9"/></svg>"##
        }
        "icons/sidebar.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M6 2.5v11M3.5 5.5h0.5M3.5 7.5h0.5"/></svg>"##
        }
        "icons/orbit.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.6"><circle cx="7" cy="9" r="4.5"/><circle cx="13" cy="3.2" r="1.7" fill="#000" stroke="none"/></svg>"##
        }
        "icons/keyboard.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linecap="round"><rect x="1.5" y="3.5" width="13" height="9" rx="1.8"/><path d="M4.2 6.4h.1M6.7 6.4h.1M9.2 6.4h.1M11.7 6.4h.1M4.2 8.4h.1M11.7 8.4h.1M6.3 10h3.4"/></svg>"##
        }
        "icons/chevron.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5 6 7.5 9 4.5"/></svg>"##
        }
        "icons/project.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linejoin="round"><path d="M1.5 4a1 1 0 0 1 1-1h3.6l1.6 1.6h5.8a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z"/></svg>"##
        }
        "icons/play.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round"><path d="M4.5 2.8v10.4L13 8z"/></svg>"##
        }
        "icons/screen.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M1.5 6h13"/></svg>"##
        }
        "icons/check.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 8.5 6.5 12 13 4.5"/></svg>"##
        }
        "icons/cross.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round"><path d="M4 4l8 8M12 4l-8 8"/></svg>"##
        }
        "icons/plus.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.6" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg>"##
        }
        "icons/search.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round"><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5 14 14"/></svg>"##
        }
        "icons/lock.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5"><rect x="3" y="7" width="10" height="7" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/></svg>"##
        }
        "icons/image.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linejoin="round"><rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><circle cx="5.5" cy="6.2" r="1.3"/><path d="m14.5 10.5-3.5-3.5-7.5 6.5"/></svg>"##
        }
        "icons/pin.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round"><circle cx="8" cy="6" r="3"/><path d="M8 9v5.5"/></svg>"##
        }
        "icons/area.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-dasharray="2 2"><rect x="2" y="3" width="12" height="10" rx="1"/></svg>"##
        }
        "icons/trash.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 4.5h11M6 4.5v-2h4v2M4 4.5l.8 9h6.4l.8-9"/></svg>"##
        }
        // Session links, from docs/design/Links-A.dc.html.
        "icons/link.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linecap="round"><path d="M6.5 9.5l3-3M7 4.5l1.2-1.2a2.8 2.8 0 0 1 4 4L11 8.5M9 11.5l-1.2 1.2a2.8 2.8 0 0 1-4-4L5 7.5"/></svg>"##
        }
        "icons/artifact.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linejoin="round"><rect x="2" y="2" width="12" height="12" rx="2"/><path d="M5 10.5 7 8l1.5 1.5L11 6"/></svg>"##
        }
        "icons/globe.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4"><circle cx="8" cy="8" r="6.5"/><path d="M1.5 8h13M8 1.5c2 2 2.8 4.2 2.8 6.5S10 12.5 8 14.5C6 12.5 5.2 10.3 5.2 8S6 3.5 8 1.5"/></svg>"##
        }
        "icons/file.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.4" stroke-linejoin="round"><path d="M4 1.5h5l3 3v10H4z"/><path d="M6 8.5h4M6 11h4"/></svg>"##
        }
        "icons/pushpin.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.3" stroke-linejoin="round"><path d="M10.5 1.5 14.5 5.5 12 6.5 9.5 9l.5 3.5-1 1L6 10.5 2.5 14l-.5-.5L5.5 10 2.5 7l1-1L7 6.5 9.5 4z"/></svg>"##
        }
        "icons/pushpin-fill.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="#000"><path d="M10.5 1.5 14.5 5.5 12 6.5 9.5 9l.5 3.5-1 1L6 10.5 2.5 14l-.5-.5L5.5 10 2.5 7l1-1L7 6.5 9.5 4z"/></svg>"##
        }
        "icons/bell.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round"><path d="M4 11V7a4 4 0 0 1 8 0v4l1.5 1.5h-11zM6.5 14h3"/></svg>"##
        }
        "icons/bell-off.svg" => {
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#000" stroke-width="1.5" stroke-linejoin="round" stroke-linecap="round"><path d="M4 11V7a4 4 0 0 1 6.2-3.3M12 7v4l1.5 1.5h-11zM6.5 14h3M2 2l12 12"/></svg>"##
        }
        _ => return None,
    })
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(source(path).map(|s| Cow::Borrowed(s.as_bytes())))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let all = [
            "claude", "codex", "monitor", "shell", "dots", "branch", "pr", "split", "popout", "dockin", "restart", "triggers", "rules", "settings", "chevron", "project", "play", "screen",
            "check", "cross", "plus", "search", "lock", "bell",
        ];
        Ok(all.iter().map(|n| format!("icons/{n}.svg")).filter(|p| p.starts_with(path)).map(Into::into).collect())
    }
}
