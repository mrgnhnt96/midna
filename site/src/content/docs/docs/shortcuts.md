---
title: Keyboard shortcuts
description: The main midna shortcuts, and how to change them.
---

Shortcuts are settings named `keys.*`. **Settings › Shortcuts** lists all of them, and you can change one there, by asking an agent, or from the CLI:

```bash
midna settings set keys.next_needs_you ctrl-j
```

Keys are written like `cmd-shift-t`, with modifiers `cmd`, `shift`, `alt` and `ctrl`. Changes apply right away.

## Main window

| Shortcut | Action |
| --- | --- |
| <kbd>⌘</kbd> <kbd>K</kbd> | Command bar |
| <kbd>⌘</kbd> <kbd>J</kbd> | Needs you: open, then skip to the next item |
| <kbd>⌘</kbd> <kbd>↩</kbd> / <kbd>⌘</kbd> <kbd>⌫</kbd> | Approve once / deny the selected terminal's approval |
| <kbd>⌘</kbd> <kbd>T</kbd> | New terminal in the current project |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>T</kbd> | New agent in the current project |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>T</kbd> | New terminal in your home folder |
| <kbd>⌘</kbd> <kbd>O</kbd> | Open a folder as a project |
| <kbd>⌘</kbd> <kbd>1</kbd>–<kbd>9</kbd> | Go to project 1–9 |
| <kbd>⌘</kbd> <kbd>B</kbd> | Collapse or expand the sidebar |
| <kbd>⌘</kbd> <kbd>D</kbd> | Split, or close the split |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>O</kbd> | Pop the terminal out into its own window |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>E</kbd> | Open the terminal's folder in your editor |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>R</kbd> | Restart the terminal |
| <kbd>⌘</kbd> <kbd>W</kbd> | Close the terminal (twice if it's busy) |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>R</kbd> / <kbd>G</kbd> / <kbd>I</kbd> | Rules / Triggers / Insights |
| <kbd>⌘</kbd> <kbd>,</kbd> | Settings |
| <kbd>⌘</kbd> <kbd>Q</kbd> | Hold to quit. Terminals keep running. |
| <kbd>esc</kbd> | Back to the terminal |

## Terminal

| Shortcut | Action |
| --- | --- |
| <kbd>⌘</kbd> <kbd>F</kbd> | Find in scrollback |
| <kbd>⌘</kbd> <kbd>↑</kbd> / <kbd>⌘</kbd> <kbd>↓</kbd> | Top or bottom of the scrollback |
| <kbd>⌘</kbd>-click | Open a link or file path |
| <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>D</kbd> | Composer for a long prompt |
| <kbd>⌘</kbd> <kbd>I</kbd> | Attach images with notes to the next message |
| <kbd>⌘</kbd> <kbd>U</kbd> | Queued messages |
| <kbd>⌘</kbd> <kbd>L</kbd> | Session links |
| <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↑</kbd> / <kbd>⌥</kbd> <kbd>⌘</kbd> <kbd>↓</kbd> | Agent: previous or next prompt you sent |
| <kbd>⌘</kbd> <kbd>P</kbd> | Agent: list your prompts |

## Command bar

| Shortcut | Action |
| --- | --- |
| <kbd>↩</kbd> | Run. Destructive commands need a second <kbd>↩</kbd>. |
| <kbd>⇧</kbd> <kbd>↩</kbd> | Start an agent with what you typed |
| <kbd>⇥</kbd> | Switch the agent: Claude or Codex |
| <kbd>⇧</kbd> <kbd>⇥</kbd> | Switch where it starts: this project or your home folder |

## Needs you

| Shortcut | Action |
| --- | --- |
| <kbd>⌘</kbd> <kbd>↩</kbd> | Main action: approve, restart, I've done it |
| <kbd>⌘</kbd> <kbd>⌫</kbd> | Other action: deny, dismiss, keep rule |
| <kbd>⌘</kbd> <kbd>⇧</kbd> <kbd>↩</kbd> | Approve all once, when several prompts ask the same thing |
| <kbd>⌘</kbd> <kbd>J</kbd> | Skip to the next item |
| <kbd>⌘</kbd> <kbd>O</kbd> | Open the item's terminal |
