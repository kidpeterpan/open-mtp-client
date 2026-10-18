#!/usr/bin/env python3
"""Draws the open-mtp-client app icon (1024×1024 PNG) with no dependencies.

A phone with a two-way arrow beside it, on a blue-to-teal rounded square
inset like other macOS icons. Usage: make_icon.py <out.png>
"""
import struct
import sys
import zlib

SIZE = 1024
INSET = 100              # macOS icons leave a margin around the tile
RADIUS = 185
TOP = (59, 91, 219)      # blue
BOTTOM = (18, 184, 134)  # teal
WHITE = (255, 255, 255)


def in_round_rect(x, y, left, top, right, bottom, radius):
    if not (left <= x < right and top <= y < bottom):
        return False
    cx = min(max(x, left + radius), right - radius - 1)
    cy = min(max(y, top + radius), bottom - radius - 1)
    return (x - cx) ** 2 + (y - cy) ** 2 <= radius * radius


def phone(x, y):
    """A phone outline with a screen cut-out and a home dot."""
    outer = in_round_rect(x, y, 250, 250, 560, 790, 60)
    screen = in_round_rect(x, y, 286, 320, 524, 690, 14)
    dot = (x - 405) ** 2 + (y - 740) ** 2 <= 22 * 22
    return outer and not screen and not dot


def arrow(x, y, cy, pointing_right):
    """A thick horizontal arrow centred on row `cy`, x from 600 to 800."""
    shaft = 600 <= x <= 760 and abs(y - cy) <= 22 if pointing_right else 640 <= x <= 800 and abs(y - cy) <= 22
    if pointing_right:
        head = 720 <= x <= 810 and abs(y - cy) <= (810 - x) * 0.9
    else:
        head = 590 <= x <= 680 and abs(y - cy) <= (x - 590) * 0.9
    return shaft or head


def pixel(x, y):
    if not in_round_rect(x, y, INSET, INSET, SIZE - INSET, SIZE - INSET, RADIUS):
        return (0, 0, 0, 0)
    if phone(x, y) or arrow(x, y, 460, True) or arrow(x, y, 580, False):
        return (*WHITE, 255)
    t = (y - INSET) / (SIZE - 2 * INSET)
    return tuple(round(a + (b - a) * t) for a, b in zip(TOP, BOTTOM)) + (255,)


def write_png(path):
    rows = bytearray()
    for y in range(SIZE):
        rows.append(0)
        for x in range(SIZE):
            rows.extend(pixel(x, y))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)
    with open(path, "wb") as out:
        out.write(b"\x89PNG\r\n\x1a\n")
        out.write(chunk(b"IHDR", header))
        out.write(chunk(b"IDAT", zlib.compress(bytes(rows), 9)))
        out.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    write_png(sys.argv[1])
