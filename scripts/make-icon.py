#!/usr/bin/env python3
"""Renders the Schatzsuche app icon and writes both forms the program needs.

    python3 scripts/make-icon.py

Produces `assets/AppIcon.icns` for the macOS bundle and `src/icon_data.rs` for
the window icon, which is embedded rather than loaded from the bundle: the icon
cache once held on to a placeholder after a bad build, and a binary that
carries its own icon cannot be caught out that way.

The artwork is geometric on purpose. An earlier version drew wood grain,
rivets and a fan of light beams at 128 pixels; at icon size that reads as
clutter, and every edge was stepped because nothing was drawn larger than it
was shown. Here the shapes carry it — a lid lighter than the body, one cast
shadow where the lid overhangs, a recessed front panel, and a single lit slit —
and everything is rendered at 4x and downsampled, so the anti-aliasing comes
from the resampling rather than from luck.

Needs Pillow (`pip3 install pillow`). The generated files are committed, so
nobody has to run this to build the project.
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFilter


S = 4
N = 1024
W = N * S


def px(v):
    return int(round(v * S))


# Background, from the interface palette.
BG_TOP = (23, 27, 40)
BG_BOT = (11, 13, 19)
BORDER = (48, 56, 78)

# One gold family, two values: the lid catches light, the body does not.
# A metal ramp, not a single fade: highlight, mid, shadow, then a little
# bounce light at the very bottom edge. That last step is what stops gold from
# looking like flat mustard.
LID_HI = (255, 226, 158)
LID_MID = (233, 179, 88)
LID_LO = (183, 126, 48)
BODY_HI = (238, 189, 110)
BODY_MID = (204, 148, 66)
BODY_LO = (138, 92, 32)
BODY_BOUNCE = (176, 122, 48)
RIM_HI = (255, 236, 182)
RIM_LO = (196, 141, 58)
LOCK_HI = (255, 240, 200)
LOCK_LO = (226, 173, 92)
EDGE = (58, 36, 12)
SLIT = (255, 244, 214)
GLOW = (255, 206, 138)
GLYPH = (74, 46, 16)

# Geometry. The lid overhangs the body on both sides, which is what makes a
# box read as a chest rather than a crate.
CX = 512
LID_L, LID_R = 248, 776
LID_T, LID_B = 282, 438
RIM_T, RIM_B = 440, 476
SLIT_T, SLIT_B = 474, 500
BODY_L, BODY_R = 262, 762
BODY_T, BODY_B = 492, 776
R_BODY = 26
R_LID = 86


def canvas():
    return Image.new("RGBA", (W, W), (0, 0, 0, 0))


def gradient(colours, horizontal=False):
    """A linear ramp the size of the canvas, from a list of (pos, rgb)."""
    n = 512
    strip = Image.new("RGB", (1, n))
    for i in range(n):
        f = i / (n - 1)
        for j in range(len(colours) - 1):
            p0, c0 = colours[j]
            p1, c1 = colours[j + 1]
            if p0 <= f <= p1:
                k = (f - p0) / max(p1 - p0, 1e-6)
                strip.putpixel((0, i), tuple(int(c0[m] + (c1[m] - c0[m]) * k) for m in range(3)))
                break
    img = strip.resize((W, W))
    if horizontal:
        img = img.transpose(Image.ROTATE_90)
    return img.convert("RGBA")


def rounded(box, radius, flat_bottom=False):
    """A mask for a rounded rectangle, optionally with square bottom corners."""
    m = Image.new("L", (W, W), 0)
    d = ImageDraw.Draw(m)
    d.rounded_rectangle([px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(radius), fill=255)
    if flat_bottom:
        d.rectangle(
            [px(box[0]), px(box[3] - radius), px(box[2]), px(box[3])],
            fill=255,
        )
    return m


def fill(mask, grad):
    layer = canvas()
    layer.paste(grad, (0, 0), mask)
    return layer


def outline(box, radius, colour, width, flat_bottom=False):
    layer = canvas()
    d = ImageDraw.Draw(layer)
    d.rounded_rectangle(
        [px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(radius), outline=colour, width=px(width)
    )
    if flat_bottom:
        d.rectangle(
            [px(box[0]), px(box[3] - radius), px(box[0] + width), px(box[3])], fill=colour
        )
        d.rectangle(
            [px(box[2] - width), px(box[3] - radius), px(box[2]), px(box[3])], fill=colour
        )
    return layer


def background():
    img = canvas()
    mask = rounded((24, 24, N - 24, N - 24), 226)
    img.paste(gradient([(0.0, BG_TOP), (1.0, BG_BOT)]), (0, 0), mask)

    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(24), px(24), px(N - 24), px(N - 24)], px(226), outline=BORDER + (255,), width=px(3)
    )
    return Image.alpha_composite(img, ring)


def ambient():
    """One soft warm pool behind the chest. No beams."""
    g = canvas()
    d = ImageDraw.Draw(g)
    for r, a in ((430, 16), (330, 22), (230, 30), (150, 40)):
        d.ellipse([px(CX - r), px(SLIT_T - r * 0.72), px(CX + r), px(SLIT_T + r * 0.72)],
                  fill=GLOW + (a,))
    return g.filter(ImageFilter.GaussianBlur(px(60)))


def contact_shadow():
    layer = canvas()
    ImageDraw.Draw(layer).ellipse(
        [px(BODY_L - 6), px(BODY_B - 18), px(BODY_R + 6), px(BODY_B + 40)], fill=(0, 0, 0, 165)
    )
    return layer.filter(ImageFilter.GaussianBlur(px(26)))


def dome_mask():
    """Half an ellipse: the lid of a chest, not the lid of a bread bin."""
    m = Image.new("L", (W, W), 0)
    d = ImageDraw.Draw(m)
    ry = LID_B - LID_T
    d.ellipse([px(LID_L), px(LID_B - ry), px(LID_R), px(LID_B + ry)], fill=255)
    d.rectangle([0, px(LID_B), W, W], fill=0)
    return m


def lid():
    mask = dome_mask()
    layer = fill(mask, gradient([(0.0, LID_HI), (0.42, LID_MID), (1.0, LID_LO)]))

    # A single sheen across the upper curve, not a blob.
    sheen = canvas()
    ImageDraw.Draw(sheen).ellipse(
        [px(LID_L + 96), px(LID_T + 18), px(LID_R - 96), px(LID_T + 86)],
        fill=(255, 250, 232, 96),
    )
    sheen = sheen.filter(ImageFilter.GaussianBlur(px(18)))
    sheen.putalpha(Image.composite(sheen.split()[3], Image.new("L", (W, W), 0), mask))

    out = Image.alpha_composite(layer, sheen)

    edge = canvas()
    ed = ImageDraw.Draw(edge)
    ry = LID_B - LID_T
    ed.ellipse([px(LID_L), px(LID_B - ry), px(LID_R), px(LID_B + ry)],
               outline=EDGE + (140,), width=px(3))
    ed.rectangle([0, px(LID_B), W, W], fill=(0, 0, 0, 0))
    out = Image.alpha_composite(out, edge)

    ry = LID_B - LID_T
    top = canvas()
    td = ImageDraw.Draw(top)
    td.ellipse([px(LID_L + 6), px(LID_B - ry + 5), px(LID_R - 6), px(LID_B + ry - 5)],
               outline=(255, 244, 210, 165), width=px(3))
    td.rectangle([0, px(LID_T + 78), W, W], fill=(0, 0, 0, 0))
    return Image.alpha_composite(out, top)


def slit():
    """The chest is ajar. The opening is dark; the light sits inside it."""
    layer = canvas()
    d = ImageDraw.Draw(layer)
    d.rectangle([px(BODY_L + 4), px(SLIT_T - 4), px(BODY_R - 4), px(SLIT_B + 4)],
                fill=(24, 14, 5, 255))
    inner = canvas()
    ImageDraw.Draw(inner).rounded_rectangle(
        [px(BODY_L + 14), px(SLIT_T + 3), px(BODY_R - 14), px(SLIT_B - 3)], px(9),
        fill=SLIT + (255,),
    )
    inner = inner.filter(ImageFilter.GaussianBlur(px(7)))
    return Image.alpha_composite(layer, inner)


def body():
    box = (BODY_L, BODY_T, BODY_R, BODY_B)
    mask = rounded(box, R_BODY)
    layer = fill(
        mask,
        gradient([(0.0, BODY_HI), (0.30, BODY_MID), (0.86, BODY_LO), (1.0, BODY_BOUNCE)]),
    )

    # The lid overhangs, so it casts onto the top of the body. This one shadow
    # does more for the depth than any amount of surface detail.
    shade = canvas()
    ImageDraw.Draw(shade).rectangle(
        [px(BODY_L), px(BODY_T - 4), px(BODY_R), px(BODY_T + 54)], fill=(40, 24, 8, 170)
    )
    shade = shade.filter(ImageFilter.GaussianBlur(px(16)))
    shade.putalpha(Image.composite(shade.split()[3], Image.new("L", (W, W), 0), mask))
    layer = Image.alpha_composite(layer, shade)

    # Inset panel: a recessed face with light catching its upper edge.
    inset = (BODY_L + 34, BODY_T + 62, BODY_R - 34, BODY_B - 34)
    panel = fill(rounded(inset, 14), gradient([(0.0, BODY_MID), (1.0, BODY_LO)]))
    panel.putalpha(panel.split()[3].point(lambda v: int(v * 0.55)))
    layer = Image.alpha_composite(layer, panel)
    layer = Image.alpha_composite(layer, outline(inset, 14, (255, 226, 168, 70), 2))

    lip = canvas()
    ImageDraw.Draw(lip).rounded_rectangle(
        [px(BODY_L + 6), px(BODY_T + 2), px(BODY_R - 6), px(BODY_T + 8)], px(3),
        fill=(255, 232, 178, 90),
    )
    layer = Image.alpha_composite(layer, lip)
    return Image.alpha_composite(layer, outline(box, R_BODY, EDGE + (170,), 3))


def rim():
    """The lid's front edge: a band of brighter metal that separates lid from
    body and gives the silhouette a straight line to rest on."""
    box = (LID_L, RIM_T, LID_R, RIM_B)
    mask = rounded(box, 10)
    layer = fill(mask, gradient([(0.0, RIM_HI), (0.5, LID_MID), (1.0, RIM_LO)]))
    return Image.alpha_composite(layer, outline(box, 10, EDGE + (150,), 3))


def bitcoin(cx, cy, h, colour):
    layer = canvas()
    d = ImageDraw.Draw(layer)
    t = h * 0.155
    left = cx - h * 0.30
    top, bot = cy - h * 0.40, cy + h * 0.40
    for x in (left + t * 0.30, left + t * 1.45):
        d.rectangle(
            [px(x - t * 0.21), px(top - h * 0.19), px(x + t * 0.21), px(bot + h * 0.19)],
            fill=colour,
        )
    d.rectangle([px(left), px(top), px(left + t), px(bot)], fill=colour)
    for y0, y1, bulge in ((top, cy - t * 0.16, 0.90), (cy + t * 0.16, bot, 1.0)):
        r = (y1 - y0) / 2
        ri = max(r - t, 1.5)
        d.rectangle([px(left), px(y0), px(cx), px(y1)], fill=colour)
        d.ellipse([px(cx - r * bulge), px(y0), px(cx + r * bulge), px(y1)], fill=colour)
        d.ellipse([px(cx - ri * bulge), px(y0 + t), px(cx + ri * bulge), px(y1 - t)],
                  fill=(0, 0, 0, 0))
        d.rectangle([px(left + t), px(y0 + t), px(cx), px(y1 - t)], fill=(0, 0, 0, 0))
    return layer


def lock():
    """The one detail. It sits on the seam, where the eye goes first."""
    size = 112
    cy = RIM_B + 4
    box = (CX - size / 2, cy - size * 0.72, CX + size / 2, cy + size * 0.62)
    mask = rounded(box, 34)
    layer = fill(mask, gradient([(0.0, LOCK_HI), (1.0, LOCK_LO)]))
    layer = Image.alpha_composite(layer, outline(box, 26, EDGE + (120,), 3))
    return Image.alpha_composite(layer, bitcoin(CX + 2, cy + 1, size * 0.56, GLYPH + (255,)))


def render():
    img = background()
    img = Image.alpha_composite(img, ambient())
    img = Image.alpha_composite(img, contact_shadow())
    img = Image.alpha_composite(img, lid())
    img = Image.alpha_composite(img, rim())
    img = Image.alpha_composite(img, slit())
    img = Image.alpha_composite(img, body())
    img = Image.alpha_composite(img, lock())

    out = canvas()
    out.paste(img, (0, 0), rounded((24, 24, N - 24, N - 24), 226))
    return out.resize((N, N), Image.LANCZOS)


ICONSET_SIZES = [16, 32, 64, 128, 256, 512, 1024]

RS_HEADER = """//! The application icon as raw RGBA, generated from the same artwork as
//! `AppIcon.icns`.
//!
//! Carried inside the binary and handed to the window system at startup, so the
//! icon does not depend on macOS resolving the bundle resource — which it
//! failed to do once already, after the bundle had briefly shipped a shell
//! script as its executable and the icon cache remembered a placeholder.
//!
//! Generated by `scripts/make-icon.py`; edit the artwork there, not here.

pub const ICON_W: u32 = {n};
pub const ICON_H: u32 = {n};

#[rustfmt::skip]
pub static ICON_RGBA: [u8; {total}] = [
"""


def write_rust(master, path, n=128):
    small = master.resize((n, n), Image.LANCZOS)
    data = list(small.tobytes())
    out = [RS_HEADER.format(n=n, total=len(data))]
    for i in range(0, len(data), 24):
        out.append("    " + ", ".join(str(b) for b in data[i : i + 24]) + ",\n")
    out.append("];\n")
    with open(path, "w") as f:
        f.write("".join(out))


def write_icns(master, path):
    import shutil
    import subprocess
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "AppIcon.iconset")
        os.makedirs(iconset)
        for size in ICONSET_SIZES:
            img = master.resize((size, size), Image.LANCZOS)
            img.save(os.path.join(iconset, f"icon_{size}x{size}.png"))
            # The @2x variants are what Retina actually draws.
            if size >= 32:
                img.save(os.path.join(iconset, f"icon_{size // 2}x{size // 2}@2x.png"))
        subprocess.run(
            ["iconutil", "-c", "icns", iconset, "-o", os.path.join(tmp, "AppIcon.icns")],
            check=True,
        )
        shutil.copy(os.path.join(tmp, "AppIcon.icns"), path)


if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(here)
    master = render()

    if len(sys.argv) > 1:
        master.save(sys.argv[1])
        sys.exit(0)

    write_rust(master, os.path.join(root, "src", "icon_data.rs"))
    print("wrote src/icon_data.rs", file=sys.stderr)
    write_icns(master, os.path.join(root, "assets", "AppIcon.icns"))
    print("wrote assets/AppIcon.icns", file=sys.stderr)
