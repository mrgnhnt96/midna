## Terminals: the rarer commands

- `midna prompts <id>` lists the prompts the human sent an agent terminal (numbered, ▸ = where its
  view is). `midna prompts <id> --jump N|prev|next|latest|live` scrolls the agent's own view there,
  e.g. to show the human the answer to an earlier question.
- `midna procs <id>` lists a terminal's processes and what its agent has in flight (background
  shells, subagents, scheduled wakeups). `midna restart <id>` reopens an agent in the same
  conversation; it refuses while background work would be lost, so use `--idle` to queue it until
  the agent is idle with nothing in flight (`--cancel` drops it, `--fresh` starts over).
  `midna replace <id>` swaps a terminal for a brand-new one in the same place (new id, name and
  conversation; the old one closes) and prints the new id.
- `midna subagent <terminal-id> <agent-id>` reads one of a terminal's subagents (its prompt, tool
  calls and replies) from its transcript; the ids are in `midna procs`.
- `midna usage` shows Claude's plan usage (5-hour and weekly windows, account-wide) and whether
  it is limited until a reset. Check it before starting long work for someone else.
- Claude's question picker (AskUserQuestion) is the human's to answer. `session.answer_question`
  answers it for the midna app and for clients the human listed in `agents.trusted_clients`
  (e.g. Taskboard, outside midna terminals); agents get error 2. If you are blocked on another
  terminal's question, use `midna attention`.
- When you tell the human about another terminal, name it by its id (`midna list`) or link it as
  `[name](midna://session/<id>)`: ⌘-clicking a terminal's id, an agent's conversation id (its
  first 8+ characters do) or a `midna://session/<id>` link brings that terminal to the front.
