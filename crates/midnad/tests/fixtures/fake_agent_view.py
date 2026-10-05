#!/usr/bin/env python3
"""A stand-in for Claude Code's full-screen view, for tests/prompts.rs.

Draws a transcript of PROMPTS prompts (rows `❯ prompt N one two`, each followed by REPLY reply
rows) on the alternate screen with mouse reporting on, scrolled by PageUp/PageDown (half the
view), wheel reports (one row each) and ctrl-End (back to the bottom). Like Claude Code it pins
the prompt being read on row 0 while scrolled back and draws `Jump to bottom` over the right
of the last transcript row. The input box is a `❯` row between two rules.
"""
import os
import re
import sys
import tty

PROMPTS, REPLY = 6, 25
T = ["  fake agent view", ""]
for k in range(1, PROMPTS + 1):
    T.append(f"❯ prompt {k} one two")
    T.append("")
    T += [f"  reply {k}.{i}" for i in range(1, REPLY + 1)]
    T.append("")

out = sys.stdout


def draw(top):
    cols, rows = os.get_terminal_size()
    h = rows - 3
    bottom = max(0, len(T) - h)
    top = max(0, min(top, bottom))
    lines = T[top:top + h] + [""] * max(0, h - (len(T) - top))
    if top > 0 and not T[top].startswith("❯"):
        lines[0] = next((T[i] for i in range(top, -1, -1) if T[i].startswith("❯")), lines[0])
    if top < bottom:
        lines[-1] = lines[-1].ljust(cols - 30)[: cols - 30] + "Jump to bottom: fn+↓"
    buf = "\x1b[H\x1b[2J" + "\r\n".join(l[:cols] for l in lines)
    buf += "\r\n" + "─" * cols + "\r\n❯ \r\n" + "─" * cols
    buf += f"\x1b[{h + 2};3H"
    out.write(buf)
    out.flush()
    return top


def main():
    tty.setraw(0)
    out.write("\x1b[?1049h\x1b[?1000h\x1b[?1006h")
    top = draw(10**9)
    pending = b""
    while True:
        data = os.read(0, 1024)
        if not data:
            return
        pending += data
        while pending:
            _, rows = os.get_terminal_size()
            half = (rows - 3) // 2
            m = re.match(rb"\x1b\[<(\d+);\d+;\d+[Mm]|\x1b\[5~|\x1b\[6~|\x1b\[1;5F|\x1b\[[0-9;]*[A-Za-z~]|.", pending, re.S)
            if not m:
                break
            tok, pending = m.group(0), pending[m.end():]
            if tok == b"\x1b[5~":
                top -= half
            elif tok == b"\x1b[6~":
                top += half
            elif tok == b"\x1b[1;5F":
                top = 10**9
            elif m.group(1) == b"64":
                top -= 1
            elif m.group(1) == b"65":
                top += 1
            elif tok == b"\x03":
                return
            else:
                continue
            top = draw(top)


main()
