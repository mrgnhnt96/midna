# Monitor grant: design concepts

Canvas: https://claude.ai/artifact/V8QTV2YcVeeSXXqjCkXkyY (attached to goal G262).

**Chosen: B and C together.** To grant: right-click a terminal's sidebar row and open
"Monitor other tabs ›" (B), a submenu that picks what it answers (prompts and approvals,
prompts only, approvals only) and how long (1 hour, 4 hours, until 8 AM, until revoked,
or a typed time), or type `monitor <name> 2am` in the command palette (C), which takes
times like 2am, 8h or 90m and a scope word. To show it: a live grant gets its own line
on the row (B), an eye, the time left and what it answers, with an always-visible
Revoke. The line turns amber in its last 15 minutes. The status line stays free.

A (popover and chip) is kept for reference, as are C's status line and header pill.

Rebuild the canvas with ultrahand's `shared/canvas/build.mjs`:

    node <ultrahand>/shared/canvas/build.mjs --presets <ultrahand>/skills/desktop/references/presets.md \
      --config config.json --concepts concepts.json --title "Monitor grant: options" --out index.html
