---
title: Rules
description: Decide what agents may do on their own, what they have to ask for, and what they can never do.
---

Rules decide whether an action is **allowed**, needs **asking**, or is **denied**. Agents can add rules. Only you can remove them.

![The Rules screen: global, project and terminal lanes, a test strip, and the live "Last fired" feed](../../../assets/screens/live-rules-dark.png)

Open Rules from the sidebar, from <kbd>⌘</kbd> <kbd>K</kbd>, or with <kbd>⇧</kbd> <kbd>⌘</kbd> <kbd>R</kbd>. <kbd>esc</kbd> takes you back to the terminal.

## What a rule is

A rule has an **effect**, a **kind**, a **pattern** and a **scope**.

**Effect**

- **allow**: go ahead without asking
- **ask**: raise an [approval](/docs/needs-you/) and wait for your answer
- **deny**: refuse

**Kind**: what sort of action it matches.

| Kind | Matches | Example pattern |
| --- | --- | --- |
| `tool` | Claude Code tool calls, as `Tool(argument)` | `Bash(rm -rf*)`, `Edit(/etc/*)` |
| `command` | A command line | `git push --force*` |
| `path` | A file path | `~/.ssh/*` |
| `cli` | An agent's own `midna` commands | `close --force*` |
| `window` | An agent asking midna to act on a window | `split*` |

Claude Code checks every tool call against your rules before it runs, through the hooks midna launches it with. Codex has no hook for this, so rules don't gate Codex's tool calls; Codex's own approval prompts still show up as [permission prompts](/docs/needs-you/).

**Pattern**: a glob over the whole value. `*` matches anything, `?` one character, and `\` escapes the next character.

**Scope**: where it applies.

- **Global**: every terminal
- **Project**: every terminal in one project
- **Terminal**: one terminal, often time-boxed

A rule can also expire. Rules made by approving "for 15 minutes" do.

## Which rule wins

1. The narrowest scope that has a matching rule wins: terminal, then project, then global.
2. Within that scope, **deny** beats **ask**, which beats **allow**. One exception: an allow you created by approving **Always** beats an ask in the same scope. Otherwise you'd be asked again every time.
3. If no rule matches, the setting `policy.default` decides.

### When nothing matches

`policy.default` is human only. Its values:

- **auto** (the default): agents' destructive `midna` commands ask (`close --force`, `restart`, `project remove`, `rules remove`, `settings reset`), and everything else is allowed. A Claude Code tool call with no matching rule is left to Claude's own permission settings.
- **allow**, **ask** or **deny**: that decision for everything no rule matches.

## Test an action

The **Test** strip at the top of Rules checks an action as the terminal you're in, without running anything. Pick a kind, type a value and press <kbd>↩</kbd>. You get the decision, the rule that decided it, and why every other rule did or didn't match.

From a shell:

```bash
midna check command -- git push --force origin main
midna explain command -- git push --force origin main
```

## Last fired

The bottom of the screen is a live feed of decisions: the time, the terminal, the action, the verdict and the rule that made it, or "no rule · default". An **ask** row fills in your answer once you give it. Click a row to find its rule.

## Adding rules

You rarely write rules by hand. They come from:

- approving with a scope: **15 minutes**, **1 hour**, **this session** and **always** each add an allow rule ([see Needs you](/docs/needs-you/#approving-for-longer))
- asking an agent: the **Ask** button on the Rules screen opens <kbd>⌘</kbd> <kbd>K</kbd> with "Add a midna rule: …" ready to finish
- the CLI:

```bash
midna rules add deny command 'git push --force*'
midna rules add ask tool 'Bash(rm -rf*)' --scope project:p_1a2b3c
midna rules add allow tool 'Bash(npm test*)' --scope session:ab12cd34 --expires 3600
```

## Removing rules: agents add, humans remove

Click **Remove** on a rule card and confirm. For ten seconds you can **Undo**, which brings the rule back with its id, author and history.

Agents can't remove rules. An agent that thinks a rule is wrong runs:

```bash
midna rules request-removal r_9f8e7d --reason "blocks the release script"
```

That becomes a **rule removal** item in [Needs you](/docs/needs-you/), and a band at the top of Rules shows the rule, who asked and why, with **Remove** and **Keep**. Until you choose, the rule stays in force.
