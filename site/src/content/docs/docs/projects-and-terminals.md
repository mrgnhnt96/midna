---
title: Projects and terminals
description: Shells, monitors and agents, and everything you can do with a terminal in midna.
---

## Kinds of terminals

- **Shell.** Your login shell, like any terminal.
- **Monitor.** A long-running command you want to see, like a dev server or a test watcher. If it exits with an error, it raises a **failed** item in [Needs you](/docs/needs-you/) with the last lines of output, and you can restart it from there.
- **Agent.** Claude Code or Codex, started by midna. See [Claude Code and Codex](/docs/agents/).

A project's **saved commands** show up in <kbd>⌘</kbd> <kbd>K</kbd> as **Run <name>** and run as monitors. Agents can add them with `midna projects add-command`.

## Status

Each terminal's dot in the sidebar says what it's doing:

| Status | Meaning |
| --- | --- |
| idle | Waiting for input |
| working | An agent turn or a command is running |
| needs you | Waiting on you: an approval, a permission prompt, or an agent that's blocked |
| done | An agent finished its turn and you haven't looked yet |
| failed | The process exited with an error |
| exited | The process exited cleanly |

Status only comes from real signals: agent hooks, the terminal title, what's on screen, and exit codes. `midna explain <terminal-id>` says why a terminal has the status it has.

## Terminals survive everything but a crash

Your terminals run in `midnad`, not in the app. Quitting midna, relaunching it, or [updating](/docs/updates/) it keeps every shell running with its scrollback and screen. While the daemon restarts, the status bar says "midnad restarting…" and the panes reconnect on their own.

The exception is the daemon being killed outright or crashing. Then its terminals are lost, and they show as exited the next time it starts.

## Working in a terminal

- **Scrollback.** Scroll with the trackpad or mouse wheel, <kbd>⇧</kbd> <kbd>Page Up</kbd> / <kbd>⇧</kbd> <kbd>Page Down</kbd>, or <kbd>⌘</kbd> <kbd>↑</kbd> / <kbd>⌘</kbd> <kbd>↓</kbd> for the top and bottom. Typing jumps back to the bottom.
- **Selection.** Drag to select, double-click for a word, triple-click for a line, and hold <kbd>⌥</kbd> while dragging for a rectangle. <kbd>⌘</kbd> <kbd>C</kbd> copies and <kbd>⌘</kbd> <kbd>A</kbd> selects everything.
- **Find.** <kbd>⌘</kbd> <kbd>F</kbd> searches the scrollback and screen. <kbd>↩</kbd> goes to the next older match, <kbd>⇧</kbd> <kbd>↩</kbd> to the newer one, and <kbd>esc</kbd> closes it. The search ignores case unless you type a capital.
- **Links.** Hold <kbd>⌘</kbd> and hover to underline a link, then <kbd>⌘</kbd>-click to open it. This works for URLs and for file paths like `src/main.rs:12:5`, which open in your `$EDITOR` at that line in a new terminal.
- **Right-click** for Copy, Paste, Open link, Select All and Clear.
- **Text editing.** <kbd>⌘</kbd> <kbd>←</kbd> / <kbd>⌘</kbd> <kbd>→</kbd>, <kbd>⌥</kbd> <kbd>←</kbd> / <kbd>⌥</kbd> <kbd>→</kbd>, <kbd>⌥</kbd> <kbd>⌫</kbd> and <kbd>⌘</kbd> <kbd>⌫</kbd> move and delete the way they do in a Mac text field. In shells and agents, <kbd>⇧</kbd> plus those keys selects text you can type over.
- **Option as Meta.** By default <kbd>⌥</kbd> acts as Meta, so <kbd>⌥</kbd> <kbd>B</kbd> moves back a word in your shell. Turn off `terminal.option_as_meta` if you'd rather type accented characters with <kbd>⌥</kbd>.

## Split, pop out and rename

- **Split.** <kbd>⌘</kbd> <kbd>D</kbd> or the split button in the header opens a new shell beside the current terminal. The pane's strip switches between side by side and stacked, moves its terminal to the main pane, or closes the split. Closing a split never closes its terminal.
- **Pop out.** The pop-out button in the header moves the terminal into its own window that floats above other apps. Use **Keep on top** to drop it to a normal window, and the dock button to put it back in the main window.
- **Rename.** Double-click a terminal's name in the header or sidebar, type, and press <kbd>↩</kbd>.
- **Reorder.** Drag a terminal row up or down within its group in the sidebar.
- **Restart.** The restart button reruns the terminal's command in the same tab. For an agent, it reopens the same conversation.

## Closing

<kbd>⌘</kbd> <kbd>W</kbd> closes the selected terminal and kills its process. If the terminal is working or waiting on you, press <kbd>⌘</kbd> <kbd>W</kbd> again within two seconds to confirm.

<kbd>⌘</kbd> <kbd>Q</kbd> is hold to quit: keep holding for a moment and the window dims while a ring fills. Quitting the app never closes your terminals.

## Session links

Agent terminals collect the links that come up in their conversation: URLs you or the agent wrote, pull requests the agent opened, artifacts it published, and files it created or edited. The links button in the header shows how many there are, and turns highlighted when a new one arrives.

Press <kbd>⌘</kbd> <kbd>L</kbd> to open the list. Type to filter, <kbd>⇥</kbd> to switch between kinds, <kbd>↩</kbd> to open one (files open in your editor), <kbd>⇧</kbd> <kbd>↩</kbd> to pin it, and <kbd>⌥</kbd> <kbd>↩</kbd> to find it in the terminal. Agents can pin the links they think you'll want to come back to, like the PR they opened.

## Prompt fast travel

Claude Code and Codex draw their whole screen themselves, so their conversation doesn't land in the terminal's scrollback. Midna can still take you back to an earlier prompt by scrolling the agent's own view.

- <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↑</kbd> goes to the previous prompt you sent, and <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↓</kbd> to the next one. Past the last prompt, you're back to live.
- While you're scrolled back, a bar at the top shows which prompt you're reading, with **Live ↓** to return. A rail of ticks on the right edge marks each prompt; hover for its text, click to jump.
- <kbd>⌘</kbd> <kbd>P</kbd> lists every prompt you sent that terminal, newest first, to search and jump to. Prompts from before a `/clear` are listed but can't be jumped to.

This is tested with Claude Code. Codex prompts are recognized, but scrolling its view hasn't been checked.

## Images with notes

Press <kbd>⌘</kbd> <kbd>I</kbd>, or paste a screenshot with <kbd>⌘</kbd> <kbd>V</kbd>, to attach images to an agent terminal's next message. Click to drop a numbered pin or drag to mark an area, and type a note for each. **Add to chat** holds the images under the terminal until you next press <kbd>↩</kbd> there, then sends the images and your notes together. <kbd>⌘</kbd> <kbd>E</kbd> edits the attachment before then.
