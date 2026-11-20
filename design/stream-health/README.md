# Stream health: design concepts

Canvas: https://claude.ai/artifact/XeRumkZmiDRpB2iCKiHxvE (attached to goal G234).

**Chosen: C, Triggers screen.** Streams are listed in the Triggers screen as trigger
sources, above the triggers, with a detail pane: state chip, Try now / Restart, Pause /
Resume, a menu holding Remove, last event, events today, restarts, uptime, the command,
secret names, which triggers it feeds, and Recent events (with verdicts) and Log tabs.
The sidebar shows nothing while every stream is healthy or paused. When a stream
restarts, fails or waits for approval, the footer's Triggers button gets a dot and a
one-word label (Restarting / Failing / Approve), and clicking it opens Triggers with that
stream selected. A failing stream raises a needs-you item (the pill goes up by one).

A (sidebar group) and B (status-bar item) are kept for reference.

Rebuild the canvas with ultrahand's `shared/canvas/build.mjs`:

    node <ultrahand>/shared/canvas/build.mjs --presets <ultrahand>/skills/desktop/references/presets.md \
      --config config.json --concepts concepts.json --title "Stream health: concepts" --out index.html
