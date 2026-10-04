#!/usr/bin/env python3
"""PLACEHOLDER app icon for Midna, drawn in code (no image tools needed).

A dark plum rounded square with a lavender (#B79AE8, the UI accent) crescent and a block
cursor: "a terminal at twilight". Replace with a designed icon before a public release.

Usage: packaging/make-icon.py packaging/assets/Midna.icns
Writes a 1024px PNG with the stdlib only, then uses sips + iconutil for the .icns sizes.
"""
import math, os, struct, subprocess, sys, tempfile, zlib

N = 1024
ACCENT = (0xB7, 0x9A, 0xE8)
TOP, BOTTOM = (0x2A, 0x21, 0x38), (0x12, 0x0E, 0x19)


def rrect_sdf(x, y, cx, cy, hw, hh, r):
    qx, qy = abs(x - cx) - hw + r, abs(y - cy) - hh + r
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r


def cov(d):  # signed distance (px) -> coverage, ~1px anti-aliasing
    return 0.0 if d >= 0.5 else 1.0 if d <= -0.5 else 0.5 - d


def blend(dst, src, a):
    return tuple(d + (s - d) * a for d, s in zip(dst, src))


def pixel(x, y):
    # macOS icon grid: 824px body, centred, ~185px corners, soft shadow below
    body = rrect_sdf(x, y, 512, 512, 412, 412, 185)
    shadow = rrect_sdf(x, y, 512, 524, 412, 412, 185)
    sa = max(0.0, 1.0 - max(shadow, 0) / 28.0) * 0.16 if shadow > -1 else 0.16
    out = (0.0, 0.0, 0.0, 0.0)
    if sa > 0 and body > -0.5:
        out = (0.0, 0.0, 0.0, sa)
    a = cov(body)
    if a <= 0:
        return out
    t = (y - 100) / 824
    bg = blend(TOP, BOTTOM, min(max(t, 0), 1))
    # faint accent glow upper-left
    g = max(0.0, 1.0 - math.hypot(x - 420, y - 380) / 520) ** 2 * 0.22
    bg = blend(bg, ACCENT, g)
    # inner hairline
    if -6 < body <= 0:
        bg = blend(bg, (255, 255, 255), 0.06 * cov(abs(body + 3) - 2))
    # crescent: big disc minus offset disc
    d1 = math.hypot(x - 470, y - 488) - 250
    d2 = math.hypot(x - 562, y - 418) - 222
    crescent = max(d1, -d2)
    bg = blend(bg, ACCENT, cov(crescent))
    # block cursor in the crescent's opening
    cur = rrect_sdf(x, y, 676, 452, 38, 54, 9)
    bg = blend(bg, ACCENT, cov(cur) * 0.92)
    # composite over the shadow
    oa = out[3]
    ra = a + oa * (1 - a)
    rgb = tuple((c * a + 0 * oa * (1 - a)) / ra for c in bg) if ra > 0 else (0, 0, 0)
    return (*rgb, ra)


def write_png(path, w, h, rows):
    raw = b"".join(b"\x00" + bytes(r) for r in rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else "Midna.icns"
    rows = []
    for y in range(N):
        row = bytearray()
        for x in range(N):
            r, g, b, a = pixel(x + 0.5, y + 0.5)
            row += bytes((int(r + 0.5), int(g + 0.5), int(b + 0.5), int(a * 255 + 0.5)))
        rows.append(row)
    with tempfile.TemporaryDirectory() as tmp:
        master = os.path.join(tmp, "icon_1024.png")
        write_png(master, N, N, rows)
        iconset = os.path.join(tmp, "Midna.iconset")
        os.mkdir(iconset)
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                px = size * scale
                name = f"icon_{size}x{size}{'@2x' if scale == 2 else ''}.png"
                subprocess.run(["sips", "-z", str(px), str(px), master, "--out", os.path.join(iconset, name)], check=True, capture_output=True)
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True)
        png_out = os.path.splitext(out)[0] + ".png"
        subprocess.run(["sips", "-z", "512", "512", master, "--out", png_out], check=True, capture_output=True)
    print(f"wrote {out} (placeholder icon)")


if __name__ == "__main__":
    main()
