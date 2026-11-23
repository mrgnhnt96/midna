## Verdict

I reviewed concept A, "Chip in the line", on the canvas (index.html). It is meant for macOS, a pointer-and-keyboard window, so I used the desktop rules. I looked at both screens (the shell prompt and the composer) in Copying, Landed and Cancelled, in Twilight dark and Daylight light, at 1440×900 and 640×480, at 100% and 200% text. I measured with the driver's `inspect`, used the ✕ with real hover, click and Esc input, and read the CSS. Blurred, the chip reads clearly. It is a small purple token in the line, set in a different face from the monospace text around it, so it looks like a placeholder that belongs to the app, not like typed text. The text typed after it reads as ordinary input. In this scope that is the right call, and the screen around it stays calm. Its text passes contrast in both schemes (about 5.2:1 in dark; `inspect` found no failures in either), and the ✕ passes WCAG 2.5.8 because it has enough space around it. Counts: 0 Catastrophe, 1 Major, 5 Minor, 2 Cosmetic. The top problem is that the design has no failure or stuck state. Universal Clipboard transfers can stall or fail. If that happens the chip would spin forever, the keys typed after ⌘V would never reach the shell, and nothing tells the user why it is taking so long.

## Findings

### R1 · 3 Major · No failure or long-wait state: the chip can spin forever and the typed keys are never sent
- **Element:** Both screens › pending-paste chip, all sizes and schemes (the only states are copying, landed and cancelled)
- **Evidence:** Confirmed
- **Problem:** If the transfer from the other device stalls or fails, the design has nothing to show. The chip says 'Pasting…' indefinitely, the keys typed after ⌘V stay held and are never sent to the shell, and the user can't tell a slow transfer from a dead one. 'Pasting…' doesn't say where the data is coming from or why it is slow, so even a normal 3–5 s wait looks like a hang.
- **Principle:** Visibility of system status [NNG-H1]; help users recognize, diagnose and recover from errors [NNG-H9]; walkthrough Q4, progress toward the goal [NNG-CW]
- **Fix:** Add two states. (1) After about 2 s, change the chip label to 'Pasting from <device>… 3s' so it names the source and counts the seconds. (2) After a timeout (for example 15 s), or when macOS reports a failure, swap the spinner for a ⚠ icon in var(--need) with 'Paste failed · Retry ⌘V'. Keep the typed text, and either send the held keys at that point or show them as still held, with the choice stated in the design. Show both new states on the canvas.

### R2 · 2 Minor · At the shell prompt, cancelling can only be found through a hover tooltip, and Esc clashes with the shell
- **Element:** 1 · ⌘V at a shell prompt › chip ✕ (title 'Cancel paste (esc)'), copying state
- **Evidence:** Confirmed
- **Problem:** In the composer the hint says 'esc cancels the paste'. In the terminal the only mention of Esc is the ✕'s tooltip, which appears only after the pointer rests on a 24×18 px target. That is a pointer path for a keyboard user who is mid-typing. Esc is also a key the shell uses (vi mode, the Meta prefix), so taking it over silently can cancel a paste the user didn't mean to cancel, or swallow an Esc they meant for the shell.
- **Principle:** Recognition rather than recall [NNG-H6]; consistency [NNG-H4]; hover is never the only path [desktop:HIG-POINTER]
- **Fix:** Show the same hint the composer has: put 'esc cancels the paste' in the pane header's status slot (.tstat) or the status bar's right side while copying. State in the design that Esc cancels only while a paste is pending and goes to the shell otherwise. Consider labelling the chip 'Pasting… esc' so the key sits right next to the spinner.

### R3 · 2 Minor · With reduced motion on, the green 'landed' highlight never goes away
- **Element:** Both screens › .landed span, landed state, prefers-reduced-motion: reduce
- **Evidence:** Confirmed
- **Problem:** The fade to transparent (`fadeok`) runs only under `prefers-reduced-motion: no-preference`. With reduced motion on, the 14% green background stays on the pasted text for good. It then looks like a selection or a lasting mark, not a brief 'this just landed' cue. Reduced motion is about movement, and a colour fade doesn't move anything.
- **Principle:** Visibility of system status, with the right signal at the right time [NNG-H1]; aesthetic and minimalist design [NNG-H8]
- **Fix:** Keep the fade under reduced motion, because a background-colour fade is not motion. Or, under reduce, show the tint for 1.2 s and then remove it in one step. Only the spinner's rotation needs the reduce guard. Under reduce, give the spinner a static ⋯ or an opacity pulse in its place.

### R4 · 2 Minor · The chip isn't a whole number of terminal cells wide, so it knocks the text after it off the grid
- **Element:** 1 · ⌘V at a shell prompt › chip inside .term (13 px mono, 7.8 px cell), 1440×900, both schemes
- **Evidence:** Confirmed
- **Problem:** The chip measures about 114.7 CSS px, roughly 14.7 cells, because it uses a 12 px proportional font with padding. Everything typed after it starts part-way through a cell, so it no longer lines up with the lines above and below. A cell-based GPUI terminal also can't draw that without a special case. The design doesn't say how the chip behaves at the right edge when the line wraps either.
- **Principle:** Alignment [SCR-4]; platform convention for terminal text on a grid [desktop:HIG-MACOS]
- **Fix:** Define the chip as a whole number of cells, for example 12 cells: '⟳ Pasting… ✕' drawn as cell content, padded to the cell boundary, with the tint filling exactly those cells and the full line height. Make it wrap as one unit, moving whole to the next row when it doesn't fit.

### R5 · 2 Minor · While copying, the composer hint drops the Enter instruction and doesn't say what Enter does
- **Element:** 2 · ⌘V in the composer › hint on the right, copying state
- **Evidence:** Confirmed
- **Problem:** The hint changes from '↩ send to claude · esc cancel' to 'esc cancels the paste'. The user can't tell whether Enter now sends the message without the paste, waits for it, or does nothing. Esc also means two things here: it cancels the paste while copying and cancels the composer otherwise. Someone who presses Esc twice out of impatience could lose the whole draft.
- **Principle:** Error prevention [NNG-H5]; visibility of system status [NNG-H1]; unsaved input survives in-app actions [NNG-H3]
- **Fix:** Decide the behaviour and show it in the hint. Recommended: Enter waits and sends once the paste lands, with the hint reading '↩ sends when the paste lands · esc cancels the paste'. Make the Esc that cancels the paste the only Esc in that run of presses: ignore a second Esc within 500 ms, or require the draft-cancel Esc to come after the chip has gone.

### R6 · 2 Minor · Landing and cancelling aren't announced; a button sits inside a live region
- **Element:** Both screens › .chip[role=status] containing button.x; landed and cancelled states
- **Evidence:** Likely
- **Problem:** In the prototype the button sits inside role=status, so the live region announces 'Pasting… Cancel paste'. When the paste lands or is cancelled, the chip is simply removed and nothing is announced. A VoiceOver user doesn't learn that the paste finished or that their Esc worked. The native GPUI accessibility tree may differ, so this is Likely.
- **Principle:** Status messages [WCAG 4.1.3]; name, role, value [WCAG 4.1.2]
- **Fix:** Keep the status text ('Pasting…') in a live region and keep the ✕ outside it. On landing, post an NSAccessibility announcement 'Pasted 64 characters'; on cancel, post 'Paste cancelled'.

### R7 · 1 Cosmetic · The ✕ is taller than the chip, and the spinner doesn't scale with the text
- **Element:** Both screens › chip › ✕ button and .spin, 100% and 200% text
- **Evidence:** Confirmed
- **Problem:** The ✕ is fixed at 24×18 px inside a chip 16.2 px tall (1.35em of 12 px), so its hover fill runs about 1 px past the chip border at the top and bottom. The spinner is fixed at 10 px, so at 200% text it is a dot next to 24 px text. The ✕ target, 24×18, passes WCAG 2.5.8 only because of the space around it and is small to hit for a pointer.
- **Principle:** Alignment [SCR-4]; target size [WCAG 2.5.8] [NNG-FITTS]
- **Fix:** Size both in em. Make the chip height 1.5em with 2px vertical padding, the ✕ 1.6em × 1.4em with its hover radius kept inside the chip, and the spinner 0.85em with a 0.12em stroke.

### R8 · 1 Cosmetic · The landed highlight is faint in Twilight dark
- **Element:** 2 · ⌘V in the composer › .landed, landed state, dark scheme
- **Evidence:** Likely
- **Problem:** 14% of #4CC38A over the #151821 panel is barely different from the panel, judged from the screenshot. For a long or multi-line paste the user may not see where the pasted text went before the fade ends.
- **Principle:** Visibility of system status [NNG-H1]
- **Fix:** Raise the tint to 22% in dark (keep 14% in light), or add a 1px bottom border in var(--ok) for the first 600 ms of the fade.

## What works

- The chip uses a sans face, an accent fill and a border, so it reads as a placeholder the app owns and not as typed text. The text typed after it reads as ordinary input, which is the goal.
- The chip's text and the ✕ pass contrast in both schemes (`inspect` found no contrast issues; dark is about 5.2:1), and the ✕ has an accessible name, 'Cancel paste'.
- The composer hint switches to 'esc cancels the paste' while copying, which puts the keyboard way to cancel where the user is already looking.
- At 640×480 with 200% text the chip wraps as one unit and isn't clipped; the ✕ stays inside the composer.

## Not checked

- What cancel actually does: clicking the ✕ and pressing Esc with real input are stubs in the prototype (the state stayed 'copying'), so I couldn't test focus after cancel or the held keys being sent.
- Reduced motion was judged from the CSS only. The driver can't emulate prefers-reduced-motion.
- Real Universal Clipboard timing, and how the native GPUI chip renders in the terminal grid compared with this HTML stand-in.
- VoiceOver output and the native accessibility tree.
- The Daylight (light) look was measured with `inspect` but not looked at in a screenshot.
- Multi-line or very long pastes, and the chip at the right edge where the line wraps, aren't shown on the canvas.
- The rest of the window (sidebar, status bar and their overlaps at 640×480 with 200% text) is out of scope per the brief.
- One reviewer finds only some of the problems; a second reviewer or a usability test would find more [NNG-HE].
