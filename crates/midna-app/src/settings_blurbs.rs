//! Settings rows' one-line descriptions. The catalog's descriptions (`midna_proto::settings`)
//! are written for agents and the CLI, so they run long and name other keys; a row shows its
//! line from here, and the whole catalog description on hover. Keep each to one sentence that
//! fits a line or two beside its control (a test checks the length).

/// A setting's line under its name, or None to fall back to the catalog's first sentence.
pub(super) fn blurb(key: &str) -> Option<&'static str> {
    Some(match key {
        // General
        "updates.channel" => "Beta gets new releases before Stable does.",
        "ui.ask.agent" => "The agent ⌘K's Ask starts; tab switches it there.",
        "ui.ask.scope" => "Start it in the current project or in your home folder.",
        "projects.roots" => "Their subfolders show up in ⌘K, ready to open.",
        "ide.app" => "Used when no per-folder rule matches.",
        "ide.rules" => "Pick an IDE from a terminal's header to add a rule.",
        "windows.close_with_terminals" => "When you close one main window while another is open.",
        "finder.quick_action" => "Right-click a folder in Finder to open it in midna.",
        // Appearance
        "theme" => "Colors for the app and its terminals.",
        "theme.dark" => "Used while macOS is in dark mode.",
        "theme.light" => "Used while macOS is in light mode.",
        "theme.colors" => "Change single colors from the CLI: midna explain theme.colors.",
        "density" => "Spacing of sidebar rows and headers.",
        "ui.haptics" => "A light tap on a Force Touch trackpad when you click.",
        "ui.header.script" => "The git details in each terminal's header.",
        "ui.header.buttons" => "Buttons in a terminal's header, left to right.",
        "ui.row.script" => "The extra text on each sidebar row.",
        "ui.status.script" => "What the status bar's script item shows.",
        "ui.status.items" => "What the status bar shows, left to right.",
        "ui.status.looks" => "Each status's color, icon and label.",
        "ui.status.background_ring" => "Ring on a done dot while background shells or agents run.",
        "git.refresh_secs" => "How often git details update.",
        "sidebar.footer.stats" => "The numbers on the sidebar's Insights card.",
        "sidebar.footer.range" => "What the Insights card counts.",
        "sidebar.footer.buttons" => "Buttons under the Insights card, up to four.",
        "sidebar.footer.compact" => "The Insights card on one line, with icon-only buttons.",
        "insights.layout" => "Pick a layout on the Insights screen.",
        "insights.layouts" => "Arrange them on the Insights screen.",
        "insights.colors.agents" => "Agent work in Insights charts.",
        "insights.colors.you" => "Your own activity in Insights charts.",
        "insights.colors.waiting" => "Agents blocked on you in Insights charts.",
        // Terminal
        "terminal.auto_name" => "Name terminals from what they're doing; a name you set stays.",
        "terminal.auto_name_updates" => "Keep renaming as the work changes, or name each one once.",
        "terminal.link_preview" => "A card that previews the path or link under the pointer.",
        "terminal.preview_path_click" => "Clicking the path at the top of a preview.",
        "terminal.image_paste" => "Add notes in the image sheet first, or paste the image's path.",
        "terminal.option_as_meta" => "Option-b moves back a word; off, Option types characters like é.",
        "terminal.prompt_bar" => "Which of your prompts you're reading, on the terminal's top row.",
        // Agents
        "agents.mcp" => "Agents midna starts can drive midna as tools.",
        "agents.claude.statusline" => "Reports each Claude session's real cost to Insights.",
        "agents.system_hint" => "Two lines of system prompt for the agents midna starts.",
        "agents.restart_on_update" => "Restart the terminal into the same conversation.",
        "agents.restart_idle_secs" => "How long an agent must be quiet before it restarts.",
        "agents.adopt_typed" => "A claude or codex typed in a shell gets midna's status and restarts.",
        "agents.shell_on_exit" => "The terminal turns into your shell, in the same folder.",
        "agents.resume_after_sleep" => "Retry agents whose turn died while the Mac slept.",
        "agents.resume_after_network" => "Retry agents whose turn died when the network dropped.",
        "agents.resume_after_sleep_prompt" => "What midna types into an agent to resume it.",
        "keep_awake.enabled" => "Stops idle sleep during the hours below.",
        "keep_awake.mode" => "Only while agents have work, or the whole time.",
        "keep_awake.start" => "When keep-awake starts and ends each day.",
        "keep_awake.days" => "The days the hours apply.",
        "keep_awake.min_battery" => "On battery, stop below this charge.",
        "keep_awake.linger_mins" => "Stay awake a little after the last work.",
        "keep_awake.wake" => "Wakes the Mac 2 minutes before scheduled work.",
        "system.busy_load" => "Load per core that counts as busy; a busy Mac delays daemon updates.",
        "guard.overload_alert" => "Show which terminals are using the CPU, with Pause and Stop.",
        "guard.overload_secs" => "How long the Mac must stay busy first.",
        "guard.loop_max_hours" => "Stop polling loops agents left running; 0 = never.",
        "worktrees.auto_clean_hours" => "Idle, clean worktrees with nothing in them; 0 = never.",
        "cleanup.enabled" => "A cheap model removes what a closed terminal left behind.",
        "cleanup.sessions" => "Agent terminals only, or shells too.",
        "cleanup.model" => "The Claude model that cleans up, like haiku.",
        "cleanup.items" => "Built-in items, or your own instructions, one per line.",
        "cleanup.keep" => "Branches and folders cleanup never touches.",
        "cleanup.tools" => "Extra tools the model may use for your own items.",
        "cleanup.timeout_secs" => "How long one cleanup may run.",
        "kass.auto_send" => "Send the text without reviewing it first.",
        // Agent limits
        "agents.may_move_windows" => "Move, pop out, snap or close windows.",
        "agents.may_close_idle" => "Close terminals that are idle, done or exited.",
        "agents.may_force_close" => "Close any terminal, even a working one.",
        "agents.may_install_updates" => "Install a downloaded update; its signature is still checked.",
        "approve.from_cli" => "Approve their own session's requests from the CLI.",
        "agents.trust_folders" => "Agents starting here skip the “trust this folder?” question.",
        "policy.default" => "Auto asks before destructive commands and allows the rest.",
        "policy.request_timeout_secs" => "How long an approval waits for your answer.",
        // Notifications
        "notify.enabled" => "A terminal can still override this for itself.",
        "notify.turn_done_min_secs" => "Shorter turns finish quietly; 0 = every turn.",
        "notify.when_app_closed" => "midnad posts them itself while the app is quit.",
        "notify.badge" => "Counts what needs you in a corner of the screen.",
        "notify.badge.idle" => "Off: it hides until something needs you.",
        "notify.badge.corner" => "You can also drag the badge to a corner.",
        "notify.badge.snap" => "Where the badge goes when you drop it.",
        "notify.badge.inset_x" | "notify.badge.inset_y" => "Drag the badge to change this.",
        "notify.badge.sharing" => "While Zoom, Meet or a recording can see your screen.",
        "needs_you.replace" => "Only a terminal's latest blocked or note item shows.",
        "needs_you.clear_notes_on_open" => "Opening a terminal marks its notes as seen.",
        "needs_you.clear_failed_on_run" => "Its failed items clear when it starts work again.",
        "needs_you.withdraw_orphans" => "Approvals no one is waiting on anymore.",
        "needs_you.expire_hours" => "Approvals, prompts, notes and failures nobody answered.",
        "notify.badge.clear_on_open" => "Needs-you items stay until they're handled.",
        // One row per notification kind (`notify.<kind>`)
        "notify.approval" => "An agent waits on an approval, a permission or a question.",
        "notify.attention" => "An agent raises a note or says it's blocked.",
        "notify.failed" => "A command fails or an agent's turn ends in an error.",
        "notify.turn_done" => "An agent finishes a long turn.",
        "notify.agent" => "Notifications agents send on purpose.",
        "notify.from_trigger" => "Notifications your triggers send.",
        "notify.requests" => "A trigger to enable, a secret to set, a rule to remove.",
        "notify.background" => "A background shell an agent started finishes.",
        "notify.pr_checks" => "The checks on a terminal's pull request finish.",
        "notify.exited" => "A terminal's process exits cleanly.",
        "notify.triggers" => "A webhook trigger fires.",
        "notify.restarted" => "midna restarts an agent after an update.",
        "notify.cleanup" => "Cleanup finished after a terminal closed.",
        // Sounds
        "notify.sounds" => "Every notification sound and sound effect.",
        "notify.sounds_in_app" => "Off: you only hear what happens while you're in another app.",
        "notify.volume" => "Scales every sound; 0 = silent.",
        "notify.image" => "Shown beside the text; each kind can pick its own.",
        // Webhooks
        "webhooks.path" => "How GitHub and Bitbucket webhooks reach this Mac.",
        "webhooks.port" => "The local port the webhook receiver listens on.",
        "webhooks.relay_url" => "Needed when the delivery path is Self-hosted.",
        "triggers.agent_mode" => "Supervised agents still ask before running tools.",
        _ => return None,
    })
}

/// A shortcut row's name: its catalog description, shortened where that runs long.
pub(super) fn shortcut_title(setting: &str) -> String {
    let short = match setting {
        "keys.new_agent" => "New agent terminal in this project",
        "keys.new_terminal" => "New terminal in this project",
        "keys.new_agent_root" => "New agent terminal in your home folder",
        "keys.new_terminal_root" => "New terminal in your home folder",
        "keys.open_project" => "Open a folder as a project",
        "keys.composer" => "Open the composer for a long prompt",
        "keys.add_image" => "Add images to the next message",
        "keys.prev_prompt" => "Previous prompt you sent",
        "keys.next_prompt" => "Next prompt you sent",
        "keys.prompts" => "List the prompts you sent",
        "keys.queue" => "Open queued messages",
        "keys.links" => "Open the terminal's links",
        "keys.subagents" => "Open the terminal's subagents",
        "keys.needs_you" => "Open the needs-you cards",
        "keys.approve_options" => "Show the approve options",
        "keys.next_terminal" => "Next terminal",
        "keys.prev_terminal" => "Previous terminal",
        "keys.project_menu" => "Open the project's … menu",
        "keys.fold_project" => "Fold or unfold the project",
        "keys.sidebar" => "Collapse or expand the sidebar",
        "keys.replace" => "Replace the terminal with a new session",
        "keys.terminal_menu" => "Open the terminal's … menu",
        "keys.copy_session_id" => "Copy the session id",
        "keys.mute" => "Mute or unmute the terminal",
        "keys.clear" => "Clear the screen and scrollback",
        "keys.split" => "Open or close a split",
        "keys.split_orientation" => "Flip the split",
        "keys.split_to_main" => "Show the split in the main pane",
        "keys.focus_pane" => "Focus the other pane",
        "keys.open_ide" => "Open the folder in your IDE",
        "keys.choose_ide" => "Choose an IDE for the folder",
        "keys.pop_out" => "Pop the terminal out",
        "keys.keep_on_top" => "Keep a pop-out on top",
        "keys.edit_attachment" => "Edit images not sent yet",
        "keys.paste_image_inline" => "Paste an image as its path",
        "keys.note_newline" => "New line in an image note",
        "keys.close" => "Close the terminal or window",
        "keys.quit" => "Quit midna",
        _ => return midna_proto::settings::setting(setting).map(|s| s.description.trim_end_matches('.').to_string()).unwrap_or_else(|| setting.to_string()),
    };
    short.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blurbs_are_one_short_sentence() {
        for spec in midna_proto::settings::SETTINGS {
            let Some(b) = blurb(spec.key) else { continue };
            assert!(b.chars().count() <= 75, "{}: {} chars", spec.key, b.chars().count());
            // the row shows the first sentence only
            assert!(!b.trim_end_matches('.').contains(". "), "{}: more than one sentence", spec.key);
        }
    }

    #[test]
    fn every_listed_setting_and_kind_has_a_blurb() {
        let listed = super::super::LAYOUT.iter().flat_map(|(_, _, items)| items.iter()).filter(|i| !i.starts_with('@'));
        for k in listed {
            assert!(blurb(k).is_some(), "{k} has no blurb");
        }
        for c in midna_proto::notify::CATEGORIES {
            assert!(blurb(&midna_proto::notify::setting_key(c.key)).is_some(), "notify.{} has no blurb", c.key);
        }
    }

    #[test]
    fn shortcut_titles_stay_short() {
        for s in crate::actions::SHORTCUTS {
            let t = shortcut_title(s.setting);
            assert!(t.chars().count() <= 45, "{}: {t}", s.setting);
        }
    }
}
