# Twilight sounds

midna's own notification sounds and sound effects. They're original sounds, synthesized from scratch in the style of Midna from Twilight Princess (gibberish chirps, breaths, soft tones). No game audio is used. `synth.py` is plain Python 3 with no dependencies: `python3 synth.py` writes `out/*.wav` (44.1 kHz, 16-bit mono).

The shipped files are `crates/midnad/assets/sounds/twilight/<Name>.wav`. midnad embeds them, writes them to `MIDNA_HOME/notify/twilight/` at startup, and every kind's default sound is one of them (`midna_proto::notify::TWILIGHT`).

| Name | Default for | Rendered as |
|---|---|---|
| Portal | approval | `tw-approval-b` |
| Call | attention | `tw-attention` |
| Uh-oh | failed | `tw-failed` |
| Strum | turn_done | `tw-done` |
| Hm | agent, from_trigger | `tw-ping` |
| Rise | approved | `tw-approved-c` |
| Nn-nn | denied | `tw-denied` |
| Whoosh | queue_sent | `tw-sent` |
| Fwip | image_added | `tw-sparkle-b` |
| Close | closed | `tw-closed` |
| Tick | switched | `tw-tick` |
| Thump | command_bar | `tw-portal-c` |
| Tick-tick | copied | `tw-copy` |

## Changing one

Edit its block in `synth.py`, run it, listen, and copy `out/<rendered as>.wav` over the shipped `<Name>.wav`. A changed file reaches existing installs on the next midnad start.

The script was tuned over several rounds of listening, so it doesn't rebuild every shipped file exactly. The shipped WAVs are the source of truth.

- **Rebuilt exactly:** Portal, Call, Strum, Rise, Fwip, Thump.
- **Close relatives only:** Hm, Nn-nn, Whoosh, Close and Tick-tick come from the first round, when every sound shared one random sequence; their recipes are at the end of the script. Uh-oh and Tick were approved before `write()` started trimming and fading the ends.

`KEEP` lists the approved sounds, and `write()` won't overwrite a file in `out/` that's in it. To redo one, remove it from `KEEP`. Every block calls `seed("<name>")`, so a sound's noise stays the same from run to run.
