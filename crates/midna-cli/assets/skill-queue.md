## Queued messages

`midna queue add <text>` leaves a message for a terminal (yours by default, `--session ID` for
another). midna types it, and presses Enter unless `--no-enter`, once it is first in line and the
agent is ready for input: not working or waiting on the human, no dialog on screen, nothing typed
in its input box. Then the next one goes. The human sees the queue on the terminal and can edit,
reorder or remove it.

- Use it for a follow-up you can't send mid-turn: queue `/compact`, then the next step, and both
  go in order once you stop. Or hand another terminal its next step without interrupting it.
- Sequence work across terminals with `--after ID`: the message waits until that terminal is idle
  with an empty queue (`midna queue add --session B --after A "Review what A just pushed"`).
- Other conditions: `--idle 10m|1h|90` (idle that long; bare number = minutes), `--at 18:00`
  (local, today or tomorrow) or `--at <RFC 3339>`. Default is as soon as the agent is ready.
- Text is the words after `add`, everything after `--`, or `-` for stdin. `--image PATH` attaches
  an image. `--first` or `--position N` puts it ahead of others (1 = next).
- `midna queue` lists it: state, when, who queued it, and what the first one is waiting for. A
  failed message holds the queue until `midna queue edit <id> --retry`, `send-now` or `rm`.
- `midna queue edit|rm|mv <id> <to>|send-now|clear|pause|resume` manage it. Positions are 1-based.
- Not for something to send right now (`midna send`) or a reaction that should repeat (a local
  trigger).
