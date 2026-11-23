# Review brief: Midna pending-paste placeholder

Design: ../../index.html (a canvas; screens "⌘V at a shell prompt" and "⌘V in the composer over an agent"; states copying / landed / cancelled).

App: Midna, a native macOS terminal app (Rust/GPUI). Twilight dark and Daylight light themes. Sidebar of terminal rows on the left, terminal pane, a composer field at the bottom over agent terminals.

Scope: ONLY the pending-paste placeholder and the text around it. The rest of the window is a stand-in for the real app and is out of scope.

What it does: ⌘V of clipboard data still copying from another device (macOS Universal Clipboard can take seconds). The app no longer freezes. A "Pasting…" chip with a spinner and a ✕ holds the paste's spot at the cursor. The user keeps typing; keys typed after ⌘V show after the chip as normal input (in a terminal the app draws them itself and sends them to the shell after the paste lands, in order). When the data lands, the chip is replaced by the pasted text with a brief green highlight that fades. Esc or the ✕ cancels: the chip disappears, typed text stays. In the composer the hint on the right reads "esc cancels the paste" while copying.

Check: legibility and contrast of the chip in both schemes, whether it reads as part of the input or as a control, alignment with the monospace line, target size of ✕, whether the cancel affordance is discoverable, the landed/cancelled states, reduced motion, and 200% text.

Don't edit anything.
