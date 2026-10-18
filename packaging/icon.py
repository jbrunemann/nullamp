"""Render the Nullamp icon (a graphite record with a signal-orange label) as PNG.
Standard library only, so packaging needs no image tools besides the platform's own."""
import math, struct, sys, zlib

def render(size):
    px = bytearray()
    c = size / 2
    for y in range(size):
        px.append(0)
        for x in range(size):
            dx, dy = x + 0.5 - c, y + 0.5 - c
            r = math.hypot(dx, dy) / c
            # Rounded-square plate.
            m = max(abs(dx), abs(dy)) / c
            corner = math.hypot(max(abs(dx) / c - 0.78, 0), max(abs(dy) / c - 0.78, 0))
            if m > 0.96 or corner > 0.18:
                px += b"\0\0\0\0"; continue
            if r < 0.16:
                col = (0xFF, 0x8A, 0x2B)
            elif r < 0.19:
                col = (0x6B, 0x3C, 0x16)
            elif r < 0.78:
                groove = 0.5 + 0.5 * math.sin(r * 90)
                v = int(0x0E + 10 * groove)
                col = (v, v + 6, v + 4)
            elif r < 0.81:
                col = (0x7C, 0xF0, 0xC8)
            else:
                col = (0x19, 0x1C, 0x20)
            px += bytes(col) + b"\xff"
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(bytes(px), 9)) + chunk(b"IEND", b"")

if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else "icon-1024.png"
    size = int(sys.argv[2]) if len(sys.argv) > 2 else 1024
    open(out, "wb").write(render(size))
