"""Render the Nullamp icon as PNG: the LCD with a lit spectrum above a mix timeline, on a graphite plate.
Standard library only, so packaging needs no image tools besides the platform's own.
Everything is drawn in a 1024 unit space from signed distances, so any output size stays anti-aliased."""
import math, struct, sys, zlib

MINT = (0x7C, 0xF0, 0xC8)
AMBER = (0xFF, 0xB5, 0x47)
ORANGE = (0xFF, 0x8A, 0x2B)
LCD_OFF = (0x14, 0x24, 0x20)

# The spectrum: lit segments per column, and which columns carry an amber peak.
COLUMNS = [5, 8, 11, 7, 12, 9, 6, 10, 4]
SEG_N, SEG_H, SEG_PITCH = 13, 20, 25
COL_W, COL_PITCH = 46, 60
BASE_Y = 622
LCD = (512, 445, 330, 205, 44)  # centre x, centre y, half width, half height, radius
TIMELINE = (512, 735, 320, 30, 16)
MIX = (418, 704)  # x range of the active setlist segment
PLAYHEAD_X = 598


def rrect(x, y, cx, cy, hw, hh, r):
    qx, qy = abs(x - cx) - hw + r, abs(y - cy) - hh + r
    return math.hypot(max(qx, 0), max(qy, 0)) + min(max(qx, qy), 0) - r


def squircle(x, y, half):
    """Distance estimate to a superellipse, close to the macOS icon shape."""
    u, v = abs(x - 512) / half, abs(y - 512) / half
    f = u ** 5 + v ** 5
    if f == 0:
        return -half
    g = 5 * math.hypot(u ** 4, v ** 4) / half
    return (f - 1) / g


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def over(dst, col, cov):
    """Composite col with coverage cov over dst, both premultiplied-free (r, g, b, a) floats."""
    if cov <= 0:
        return dst
    a = cov
    oa = a + dst[3] * (1 - a)
    if oa == 0:
        return dst
    return tuple((col[i] * a + dst[i] * dst[3] * (1 - a)) / oa for i in range(3)) + (oa,)


def cov(d, px):
    return min(max(0.5 - d / px, 0.0), 1.0)


# Linux docks and Windows taskbars show icons edge to edge, so the macOS grid's margin
# (the plate spans about 80% of the canvas, the rest is room for the shadow) makes Nullamp look
# smaller than its neighbours there. The full variant drops the shadow and fills about 90%.
FULL_SCALE = 412 / 460


def shade(x, y, px, full=False):
    out = (0.0, 0.0, 0.0, 0.0)
    if full:
        x, y, px = 512 + (x - 512) * FULL_SCALE, 512 + (y - 512) * FULL_SCALE, px * FULL_SCALE
    body = squircle(x, y, 412)

    # Soft shadow under the plate.
    sd = squircle(x, y - 22, 412)
    if sd < 40 and not full:
        out = over(out, (0, 0, 0), 0.45 * min(max((40 - sd) / 70, 0), 1) ** 2)

    # Plate: graphite, lit from the top, with a thin rim light.
    t = min(max((y - 100) / 824, 0), 1)
    plate = mix((0x2B, 0x30, 0x36), (0x15, 0x18, 0x1B), t)
    out = over(out, plate, cov(body, px))
    rim = max(-body - 0.0, 0)
    if body < 0 and rim < 5 and y < 560:
        out = over(out, (0x5A, 0x62, 0x6C), cov(-(rim - 3), px) * 0.55 * (1 - y / 560))
    if body > 6:
        return out

    # LCD window: bezel, dark glass with a faint teal wash toward the top.
    d = rrect(x, y, *LCD)
    if d < px:
        out = over(out, (0x07, 0x0A, 0x0A), cov(d - 8, px))
        out = over(out, (0x3A, 0x40, 0x48), cov(d, px) * (1 - cov(d + 5, px)) if d > -5 else 0)
        glass = mix((0x0E, 0x1C, 0x19), (0x06, 0x0A, 0x09), min(max((y - 240) / 410, 0), 1))
        out = over(out, glass, cov(d + 5, px))

        # Spectrum: unlit segments stay visible as the LCD's ghost digits do.
        x0 = 512 - (len(COLUMNS) * COL_PITCH - (COL_PITCH - COL_W)) / 2
        col = int((x - x0) // COL_PITCH)
        if 0 <= col < len(COLUMNS) and abs(d) > 0:
            lx = x - (x0 + col * COL_PITCH)
            dx = abs(lx - COL_W / 2) - COL_W / 2
            seg = int((BASE_Y - y) // SEG_PITCH)
            if 0 <= seg < SEG_N:
                cy = BASE_Y - seg * SEG_PITCH - SEG_H / 2
                sdist = rrect(lx, y, COL_W / 2, cy, COL_W / 2, SEG_H / 2, 3)
                lit = seg < COLUMNS[col]
                peak = seg == COLUMNS[col] - 1 and COLUMNS[col] >= 9
                c = AMBER if peak else MINT
                if lit:
                    # A little vertical shading so the segments read as lit glass.
                    c = mix(c, (255, 255, 255), 0.12 * (1 - (y - (cy - SEG_H / 2)) / SEG_H))
                out = over(out, c if lit else LCD_OFF, cov(sdist, px))

    # Timeline: dark track, the active mix segment in amber tones, the playhead in orange.
    td = rrect(x, y, *TIMELINE)
    if td < px:
        track = (0x1B, 0x44, 0x38)
        out = over(out, (0x07, 0x0A, 0x0A), cov(td - 4, px))
        out = over(out, track, cov(td, px))
        if MIX[0] <= x <= MIX[1]:
            out = over(out, (0x55, 0x63, 0x44), cov(td, px))
            if y < TIMELINE[1] - TIMELINE[3] + 4:
                out = over(out, ORANGE, cov(td, px))
    pd = rrect(x, y, PLAYHEAD_X, TIMELINE[1], 7, TIMELINE[3] + 26, 7)
    if pd < px:
        out = over(out, (0xFF, 0x8A, 0x2B), cov(pd, px))
    return out


def render(size, full=False):
    px = 1024 / size
    rows = bytearray()
    for j in range(size):
        rows.append(0)
        y = (j + 0.5) * px
        for i in range(size):
            r, g, b, a = shade((i + 0.5) * px, y, px, full)
            rows += bytes((int(r + 0.5), int(g + 0.5), int(b + 0.5), int(a * 255 + 0.5)))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(rows), 9)) + chunk(b"IEND", b"")


if __name__ == "__main__":
    # Usage: icon.py [OUT] [SIZE] [--full]   (--full for Linux and Windows, see FULL_SCALE)
    full = "--full" in sys.argv
    args = [a for a in sys.argv[1:] if a != "--full"]
    out = args[0] if args else "icon-1024.png"
    size = int(args[1]) if len(args) > 1 else 1024
    open(out, "wb").write(render(size, full))
