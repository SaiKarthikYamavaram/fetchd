#!/usr/bin/env python3
"""Regenerate the whole icon set from the spool mark.

The mark is a ring broken into four segments — the four connections a download
is split across, and the thread on a spool. It is drawn here rather than kept
as a hand-edited file so that every size, the .ico, the .icns, the extension
icons and the web favicon all come from the same geometry.

Two geometries, not one. Below about 48px the elegant version dissolves: the
stroke thins out and the gaps eat the ring until it reads as four detached
blobs. Small sizes get a thicker stroke and narrower gaps, which is the usual
answer for icons and the only one that survived being rendered and looked at.

The same mark is drawn a third time, by hand, in a 24-unit viewBox: `IconMark`
in src/components/icons.tsx, and the copies of it in the extension's popup,
options page and in-page panel. Those sit inside a tile the CSS draws, so they
cannot use these files. They follow the small geometry — a ring that fills its
box with narrow gaps — because at 18px the roomy version reads as four dots
rather than a ring. Change one, change all four.

Needs rsvg-convert (librsvg) and magick (ImageMagick 7).

    python3 src-tauri/icons/generate.py
"""

import math
import struct
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent

# Straight out of src/App.css: --accent and --accent-2.
ACCENT, ACCENT2 = "#5E6AD2", "#8B5CF6"

BOX = 512          # design canvas
CORNER = 112       # tile radius, ~22% — the squircle-ish proportion iOS uses
SEGMENTS = 4

# (ring radius, stroke width, gap in degrees)
LARGE = (150, 60, 24)
SMALL = (148, 72, 15)
SMALL_ABOVE = 48   # sizes at or below this use SMALL


def dasharray(radius, width, gap_degrees):
    """`stroke-dasharray` for SEGMENTS equal arcs with gaps of a given angle."""
    circumference = 2 * math.pi * radius
    gap = circumference * gap_degrees / 360.0
    return f"{circumference / SEGMENTS - gap:.3f} {gap:.3f}"


def mark_svg(geometry):
    radius, width, gap = geometry
    ring = (
        f'<circle cx="{BOX // 2}" cy="{BOX // 2}" r="{radius}" fill="none" '
        f'stroke="#fff" stroke-width="{width}" '
        f'stroke-dasharray="{dasharray(*geometry)}" '
        f'transform="rotate(-90 {BOX // 2} {BOX // 2})"/>'
    )
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {BOX} {BOX}" width="{BOX}" height="{BOX}">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="{ACCENT}"/>
      <stop offset="1" stop-color="{ACCENT2}"/>
    </linearGradient>
  </defs>
  <rect width="{BOX}" height="{BOX}" rx="{CORNER}" fill="url(#g)"/>
  {ring}
</svg>
'''


def render(svg_path, out_path, px):
    out_path.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["rsvg-convert", "-w", str(px), "-h", str(px), "-o", str(out_path), str(svg_path)],
        check=True,
    )


def source_for(px):
    return SMALL_SVG if px <= SMALL_ABOVE else LARGE_SVG


def png(out_path, px):
    render(source_for(px), out_path, px)
    return out_path


def write_icns(pngs, out_path):
    """Pack PNGs into an .icns.

    ImageMagick writes a plain PNG when handed an `.icns` name, so the container
    is assembled here. Every modern icns type takes PNG data verbatim, so this
    is a header, a table of (type, length, bytes), and nothing else.
    """
    types = {
        16: b"icp4", 32: b"icp5", 64: b"ic12", 128: b"ic07",
        256: b"ic08", 512: b"ic09", 1024: b"ic10",
    }
    chunks = b""
    for size, path in sorted(pngs.items()):
        if size not in types:
            continue
        data = path.read_bytes()
        chunks += types[size] + struct.pack(">I", len(data) + 8) + data
    out_path.write_bytes(b"icns" + struct.pack(">I", len(chunks) + 8) + chunks)


LARGE_SVG = HERE / "mark.svg"
SMALL_SVG = HERE / "mark-small.svg"

LARGE_SVG.write_text(mark_svg(LARGE))
SMALL_SVG.write_text(mark_svg(SMALL))

# ── The Tauri bundle set ───────────────────────────────────────────────────
for name, px in {
    "32x32.png": 32,
    "128x128.png": 128,
    "128x128@2x.png": 256,
    "icon.png": 512,
    "Square30x30Logo.png": 30,
    "Square44x44Logo.png": 44,
    "Square71x71Logo.png": 71,
    "Square89x89Logo.png": 89,
    "Square107x107Logo.png": 107,
    "Square142x142Logo.png": 142,
    "Square150x150Logo.png": 150,
    "Square284x284Logo.png": 284,
    "Square310x310Logo.png": 310,
    "StoreLogo.png": 50,
}.items():
    png(HERE / name, px)

# ── .ico and .icns ─────────────────────────────────────────────────────────
scratch = HERE / ".build"
scratch.mkdir(exist_ok=True)
sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
built = {px: png(scratch / f"{px}.png", px) for px in sizes}

subprocess.run(
    ["magick"] + [str(built[px]) for px in (16, 24, 32, 48, 64, 256)]
    + [str(HERE / "icon.ico")],
    check=True,
)
write_icns(built, HERE / "icon.icns")

# ── The browser extension, and the web favicon ─────────────────────────────
for px in (16, 32, 48, 128):
    png(ROOT / "extension" / f"icon{px}.png", px)

(ROOT / "public" / "spool.svg").write_text(mark_svg(LARGE))

for leftover in scratch.iterdir():
    leftover.unlink()
scratch.rmdir()

print("icons regenerated from the spool mark")
