---
title: Keyboard shortcuts
description: Every midna shortcut, and how to change them.
---

## Changing shortcuts

Most shortcuts are settings named `keys.*`, so you can change them like any other setting. Ask an agent ("make ⌘T open Claude at the project root"), or set one yourself:

```bash
midna settings set keys.next_needs_you cmd-u
```

Keys are written like `cmd-shift-t`, with modifiers `cmd`, `shift`, `alt` and `ctrl`. Changes apply right away. **Settings › Keybindings** lists the current ones.

## Main window

| Shortcut | Action | Setting |
| --- | --- | --- |
| <kbd>⌘</kbd> <kbd>K</kbd> | Command bar | `keys.command_bar` |
| <kbd>⌘</kbd> <kbd>J</kbd> | Needs you: open, then skip to the next item | `keys.next_needs_you` |
| <kbd>⌘</kbd> <kbd>T</kbd> | New terminal in the current project | `keys.new_terminal` |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>T</kbd> | New agent in the current project | `keys.new_agent` |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>T</kbd> | New terminal at root (starts in your home folder) | `keys.new_terminal_root` |
| <kbd>⌥</kbd> <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>T</kbd> | New agent at root | `keys.new_agent_root` |
| <kbd>⌘</kbd> <kbd>O</kbd> | Open a folder as a project | `keys.open_project` |
| <kbd>⌘</kbd> <kbd>1</kbd>–<kbd>9</kbd> | Go to project 1–9 | |
| <kbd>⌘</kbd> <kbd>↩</kbd> | Approve the selected approval once | `keys.approve` |
| <kbd>⌘</kbd> <kbd>⌫</kbd> | Deny it | `keys.deny` |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>R</kbd> | Rules | `keys.rules` |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>G</kbd> | Triggers | `keys.triggers` |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>I</kbd> | Insights | `keys.insights` |
| <kbd>⌘</kbd> <kbd>,</kbd> | Settings | `keys.settings` |
| <kbd>⌘</kbd> <kbd>D</kbd> | Split: open or close a pane beside the terminal | |
| <kbd>⌘</kbd> <kbd>W</kbd> | Close the selected terminal (twice if it's busy) | |
| <kbd>⌘</kbd> <kbd>Q</kbd> | Hold to quit. Terminals keep running. | |
| <kbd>esc</kbd> | Close the command bar, Needs you, Rules, Triggers or Insights | |

## Terminal

| Shortcut | Action | Setting |
| --- | --- | --- |
| <kbd>⌘</kbd> <kbd>C</kbd> / <kbd>⌘</kbd> <kbd>V</kbd> / <kbd>⌘</kbd> <kbd>A</kbd> | Copy, paste, select all | |
| <kbd>⌘</kbd> <kbd>F</kbd> | Find. <kbd>↩</kbd> older, <kbd>⇧</kbd> <kbd>↩</kbd> newer, <kbd>esc</kbd> close. | |
| <kbd>⌘</kbd> <kbd>↑</kbd> / <kbd>⌘</kbd> <kbd>↓</kbd> | Shells: top and bottom of the scrollback. Agents: start and end of what you're typing. | |
| <kbd>⇧</kbd> <kbd>Page Up</kbd> / <kbd>⇧</kbd> <kbd>Page Down</kbd> | Scroll a page | |
| <kbd>⌘</kbd>-click | Open a link or file path | |
| <kbd>⌘</kbd> <kbd>←</kbd> / <kbd>⌘</kbd> <kbd>→</kbd>, <kbd>⌥</kbd> <kbd>←</kbd> / <kbd>⌥</kbd> <kbd>→</kbd> | Start or end of line, word back or forward | |
| <kbd>⌥</kbd> <kbd>⌫</kbd> / <kbd>⌘</kbd> <kbd>⌫</kbd> | Delete a word, or to the start of the line | |
| <kbd>⇧</kbd> with any of the above | Select, then type to replace | |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>D</kbd> | Open the composer to type or paste a long prompt | `keys.composer` |
| <kbd>⌘</kbd> <kbd>I</kbd> | Add images with notes to the next message | `keys.add_image` |
| <kbd>⌘</kbd> <kbd>E</kbd> | Edit the images waiting to be sent | |
| <kbd>⌘</kbd> <kbd>L</kbd> | Session links | `keys.links` |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↑</kbd> / <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↓</kbd> | Agents: previous or next prompt you sent | `keys.prev_prompt`, `keys.next_prompt` |
| <kbd>⌘</kbd> <kbd>P</kbd> | Agents: list your prompts to jump to one | `keys.prompts` |

When the selected terminal has an approval waiting, <kbd>⌘</kbd> <kbd>⌫</kbd> denies it instead of deleting.

## Command bar

| Shortcut | Action |
| --- | --- |
| <kbd>↑</kbd> / <kbd>↓</kbd> | Move |
| <kbd>↩</kbd> | Run. Destructive commands need a second <kbd>↩</kbd>. |
| <kbd>⇧</kbd> <kbd>↩</kbd> | Ask an agent with what you typed |
| <kbd>⇥</kbd> | Switch the agent: Claude or Codex |
| <kbd>⇧</kbd> <kbd>⇥</kbd> | Switch where it starts: this project or root |
| <kbd>esc</kbd> | Close |

## Needs you

| Shortcut | Action |
| --- | --- |
| <kbd>⌘</kbd> <kbd>↩</kbd> | Main action: approve, restart, I've done it, … |
| <kbd>⌘</kbd> <kbd>⌫</kbd> | Other action: deny, dismiss, keep rule, … |
| <kbd>⌘</kbd> <kbd>⇧</kbd> <kbd>↩</kbd> | Approve all once, when offered |
| <kbd>⌘</kbd> <kbd>J</kbd> | Skip to the next item |
| <kbd>⌘</kbd> <kbd>O</kbd> | Open the item's terminal |
| <kbd>esc</kbd> | Back |

## Composer

| Shortcut | Action |
| --- | --- |
| <kbd>↩</kbd> | Send to the terminal |
| <kbd>⇧</kbd> <kbd>↩</kbd> | New line |
| <kbd>esc</kbd> | Cancel |
