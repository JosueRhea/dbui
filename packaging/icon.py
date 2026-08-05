#!/usr/bin/env python3
"""Generate the dbui app icon — a 1024x1024 master PNG.

Draws a macOS-style rounded "squircle" with a slate->indigo vertical gradient
and the one glyph a database client can only be: three stacked discs. Output
feeds the .icns pipeline (see `make icon`).

Written rather than drawn so the icon is reproducible from source and so a
colour change is a one-line diff instead of a binary blob nobody can review.
"""
import sys

from PIL import Image, ImageDraw, ImageFilter

S = 1024                      # master canvas
MARGIN = 100                  # transparent padding around the squircle
BOX = S - 2 * MARGIN          # squircle side
RADIUS = 200                  # corner radius
TOP = (32, 36, 52)            # gradient top    (slate)
BOTTOM = (67, 74, 142)        # gradient bottom (indigo)

# The stack of discs. An ellipse this flat reads as a cylinder seen from
# slightly above, which is the shape every database logo has agreed on.
DISC_W = 470                  # cylinder width
DISC_H = 150                  # cap height (the ellipse's minor axis)
SEGMENTS = 3                  # discs in the stack
GAP = 116                     # height of one segment's wall
CAP = (245, 246, 252)         # the lit top cap
BAND = (214, 218, 238)        # the cylinder wall
SHADE = (150, 157, 196)       # the seam under each disc
SEAM_W = 16                   # seam stroke width


def lerp(a, b, t):
    return tuple(round(a[i] + (b[i] - a[i]) * t) for i in range(3))


def rounded_mask(size, box, margin, radius):
    m = Image.new("L", (size, size), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [margin, margin, margin + box, margin + box], radius=radius, fill=255
    )
    return m


def gradient(size, top, bottom):
    g = Image.new("RGB", (size, size))
    px = g.load()
    for y in range(size):
        c = lerp(top, bottom, y / (size - 1))
        for x in range(size):
            px[x, y] = c
    return g


def disc_box(cx, cy):
    return [cx - DISC_W / 2, cy - DISC_H / 2, cx + DISC_W / 2, cy + DISC_H / 2]


def main(out):
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    mask = rounded_mask(S, BOX, MARGIN, RADIUS)
    icon.paste(gradient(S, TOP, BOTTOM).convert("RGBA"), (0, 0), mask)

    overlay = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    od = ImageDraw.Draw(overlay)
    cx = S / 2
    # The stack, centred as a group: a cap at the top, `SEGMENTS` walls below.
    top = S / 2 - (SEGMENTS * GAP) / 2
    bottom = top + SEGMENTS * GAP

    # Wall first, then the bottom cap closes it off, then the seams are drawn on
    # top. Drawing the seams last is the whole trick -- filling each disc in turn
    # just paints over the one above and the stack reads as a blank cylinder.
    od.rectangle([cx - DISC_W / 2, top, cx + DISC_W / 2, bottom], fill=BAND)
    od.ellipse(disc_box(cx, bottom), fill=BAND)

    # A seam is the *front* half of the ellipse at each segment boundary: the
    # rim of the disc below, seen from above.
    for i in range(1, SEGMENTS + 1):
        od.arc(disc_box(cx, top + i * GAP), start=0, end=180, fill=SHADE, width=SEAM_W)

    od.ellipse(disc_box(cx, top), fill=CAP)

    # Soften the edges very slightly; at 16x16 a hard ellipse edge aliases into
    # a visible stair-step.
    overlay = overlay.filter(ImageFilter.GaussianBlur(1.2))
    icon.paste(overlay, (0, 0), Image.composite(overlay.split()[3], Image.new("L", (S, S), 0), mask))

    icon.save(out)
    print(f"wrote {out}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "packaging/icon-master.png")
