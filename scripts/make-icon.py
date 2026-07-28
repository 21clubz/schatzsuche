#!/usr/bin/env python3
"""Renders the Schatzsuche app icon and writes both forms the program needs.

    python3 scripts/make-icon.py

Produces `assets/AppIcon.icns` for the macOS bundle and `src/icon_data.rs` for
the window icon, which is embedded rather than loaded from the bundle: the icon
cache once held on to a placeholder after a bad build, and a binary that
carries its own icon cannot be caught out that way.

The picture is the one the project started with — a lit chest on a dark tile —
drawn properly. The first version was painted straight at 128 pixels, so every
rounding was stepped, the wood was one flat brown, the gold was one flat
yellow, and the beams stopped dead at the tile edge. Here everything is drawn
at 4x and downsampled, the lid is shaded across its curve, the gold runs
through a metal ramp, and the light falls off smoothly.

Needs Pillow (`pip3 install pillow`). The generated files are committed, so
nobody has to run this to build the project.
"""
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFilter


S = 4
N = 1024
W = N * S


def px(v):
    return int(round(v * S))


def canvas():
    return Image.new("RGBA", (W, W), (0, 0, 0, 0))


BG_TOP = (30, 33, 44)
BG_BOT = (12, 14, 20)
BORDER = (52, 60, 82)

WOOD_LIGHT = (118, 78, 48)
WOOD_MID = (86, 55, 33)
WOOD_DARK = (58, 36, 21)
WOOD_DEEP = (38, 23, 13)
WOOD_LINE = (30, 18, 10)

GOLD_TOP = (252, 226, 160)
GOLD_MID = (222, 172, 84)
GOLD_LOW = (168, 118, 46)
GOLD_EDGE = (104, 70, 24)

LIGHT = (255, 246, 222)
GLOW = (255, 206, 132)

CX = 512
# The chest, top to bottom.
LID_L, LID_R = 214, 810
LID_T, LID_B = 296, 470
BAND1 = (470, 512)          # gold band under the lid
SLOT = (512, 566)           # the lit gap
BAND2 = (566, 606)          # gold band under the light
BODY_L, BODY_R = 246, 778
BODY_T, BODY_B = 606, 800
STRAP_W = 46
FOOT = (800, 846)           # bottom band, doubles as the feet


def ramp(stops, horizontal=False):
    n = 512
    strip = Image.new("RGB", (1, n))
    for i in range(n):
        f = i / (n - 1)
        for j in range(len(stops) - 1):
            p0, c0 = stops[j]
            p1, c1 = stops[j + 1]
            if p0 <= f <= p1:
                k = (f - p0) / max(p1 - p0, 1e-6)
                strip.putpixel((0, i), tuple(int(c0[m] + (c1[m] - c0[m]) * k) for m in range(3)))
                break
    img = strip.resize((W, W))
    if horizontal:
        img = img.transpose(Image.ROTATE_270)
    return img.convert("RGBA")


def mask_round(box, radius):
    m = Image.new("L", (W, W), 0)
    ImageDraw.Draw(m).rounded_rectangle(
        [px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(radius), fill=255
    )
    return m


def paint(mask, grad):
    layer = canvas()
    layer.paste(grad, (0, 0), mask)
    return layer


def background():
    img = paint(mask_round((22, 22, N - 22, N - 22), 228), ramp([(0.0, BG_TOP), (1.0, BG_BOT)]))
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(22), px(22), px(N - 22), px(N - 22)], px(228), outline=BORDER + (255,), width=px(3)
    )
    return Image.alpha_composite(img, ring)


def beams():
    """Light from behind the chest. Wide, warm, and fading to nothing."""
    layer = canvas()
    d = ImageDraw.Draw(layer)
    ox, oy = CX, SLOT[0] + 10
    for ang, half, a in (
        (-90, 8.5, 46), (-66, 5.5, 30), (-114, 5.5, 30),
        (-44, 7.0, 36), (-136, 7.0, 36),
        (-24, 4.5, 24), (-156, 4.5, 24),
        (-8, 5.5, 20), (-172, 5.5, 20),
    ):
        r = math.radians(ang)
        s = math.radians(half)
        L = 900
        d.polygon(
            [
                (px(ox), px(oy)),
                (px(ox + math.cos(r - s) * L), px(oy + math.sin(r - s) * L)),
                (px(ox + math.cos(r + s) * L), px(oy + math.sin(r + s) * L)),
            ],
            fill=GLOW + (a,),
        )
    layer = layer.filter(ImageFilter.GaussianBlur(px(22)))

    # Fade the beams out with distance so they do not reach the tile edge.
    fade = Image.new("L", (W, W), 0)
    fd = ImageDraw.Draw(fade)
    for i in range(64):
        f = i / 63
        rad = int(520 * (1 - f) + 40)
        fd.ellipse(
            [px(ox - rad), px(oy - rad), px(ox + rad), px(oy + rad)],
            fill=int(255 * (1 - f) ** 0.7),
        )
    fade = fade.filter(ImageFilter.GaussianBlur(px(30)))
    layer.putalpha(Image.composite(layer.split()[3], Image.new("L", (W, W), 0), fade))
    return layer


def dome():
    ry = LID_B - LID_T
    m = Image.new("L", (W, W), 0)
    md = ImageDraw.Draw(m)
    md.ellipse([px(LID_L), px(LID_B - ry), px(LID_R), px(LID_B + ry)], fill=255)
    md.rectangle([0, px(LID_B), W, W], fill=0)

    layer = paint(
        m,
        ramp(
            [
                (0.00, WOOD_DEEP),
                (0.16, WOOD_DARK),
                (0.34, WOOD_LIGHT),
                (0.52, WOOD_MID),
                (0.78, WOOD_DARK),
                (1.00, WOOD_DEEP),
            ],
            horizontal=True,
        ),
    )

    # Staves: a chest lid is planks bent over a frame, not one surface.
    stave = canvas()
    sd = ImageDraw.Draw(stave)
    for k in (-0.62, -0.32, 0.0, 0.32, 0.62):
        x = CX + (LID_R - LID_L) / 2 * k
        sd.line([px(x), px(LID_T + 6), px(x), px(LID_B)], fill=WOOD_LINE + (110,), width=px(3))
    stave = stave.filter(ImageFilter.GaussianBlur(px(2)))
    stave.putalpha(Image.composite(stave.split()[3], Image.new("L", (W, W), 0), m))
    layer = Image.alpha_composite(layer, stave)

    # Warm rim where the light licks the top edge.
    rim = canvas()
    rd = ImageDraw.Draw(rim)
    rd.ellipse(
        [px(LID_L + 4), px(LID_B - ry + 4), px(LID_R - 4), px(LID_B + ry - 4)],
        outline=(196, 142, 84, 210),
        width=px(6),
    )
    rd.rectangle([0, px(LID_T + 110), W, W], fill=(0, 0, 0, 0))
    rim = rim.filter(ImageFilter.GaussianBlur(px(3)))
    layer = Image.alpha_composite(layer, rim)

    edge = canvas()
    ed = ImageDraw.Draw(edge)
    ed.ellipse([px(LID_L), px(LID_B - ry), px(LID_R), px(LID_B + ry)],
               outline=WOOD_LINE + (235,), width=px(5))
    ed.rectangle([0, px(LID_B), W, W], fill=(0, 0, 0, 0))
    return Image.alpha_composite(layer, edge)


def gold(box, radius=10, overhang=0):
    b = (box[0] - overhang, box[1], box[2] + overhang, box[3])
    layer = paint(
        mask_round(b, radius),
        ramp([(0.0, GOLD_TOP), (0.22, GOLD_MID), (0.72, GOLD_LOW), (1.0, GOLD_MID)]),
    )
    hi = canvas()
    ImageDraw.Draw(hi).rounded_rectangle(
        [px(b[0] + 5), px(b[1] + 3), px(b[2] - 5), px(b[1] + 9)], px(4), fill=GOLD_TOP + (200,)
    )
    layer = Image.alpha_composite(layer, hi)
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(b[0]), px(b[1]), px(b[2]), px(b[3])], px(radius), outline=GOLD_EDGE + (220,), width=px(3)
    )
    return Image.alpha_composite(layer, ring)


def body():
    layer = paint(
        mask_round((BODY_L, BODY_T, BODY_R, BODY_B), 16),
        ramp([(0.0, WOOD_MID), (0.45, WOOD_DARK), (1.0, WOOD_DEEP)]),
    )
    planks = canvas()
    pd = ImageDraw.Draw(planks)
    for k in (-0.55, -0.2, 0.2, 0.55):
        x = CX + (BODY_R - BODY_L) / 2 * k
        pd.line([px(x), px(BODY_T + 8), px(x), px(BODY_B - 8)], fill=WOOD_LINE + (120,), width=px(3))
    planks = planks.filter(ImageFilter.GaussianBlur(px(2)))
    planks.putalpha(
        Image.composite(planks.split()[3], Image.new("L", (W, W), 0),
                        mask_round((BODY_L, BODY_T, BODY_R, BODY_B), 16))
    )
    layer = Image.alpha_composite(layer, planks)
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(BODY_L), px(BODY_T), px(BODY_R), px(BODY_B)], px(16),
        outline=WOOD_LINE + (220,), width=px(4)
    )
    return Image.alpha_composite(layer, ring)


def slot():
    """The lit gap, with the light spilling out at both ends."""
    layer = canvas()
    d = ImageDraw.Draw(layer)
    d.rectangle([px(LID_L + 10), px(SLOT[0] - 6), px(LID_R - 10), px(SLOT[1] + 6)],
                fill=(22, 13, 6, 255))
    inner = canvas()
    ImageDraw.Draw(inner).rounded_rectangle(
        [px(LID_L + 24), px(SLOT[0] + 4), px(LID_R - 24), px(SLOT[1] - 4)], px(10),
        fill=LIGHT + (255,),
    )
    inner = inner.filter(ImageFilter.GaussianBlur(px(4)))
    layer = Image.alpha_composite(layer, inner)

    bulbs = canvas()
    bd = ImageDraw.Draw(bulbs)
    cy = (SLOT[0] + SLOT[1]) / 2
    for x in (LID_L + 12, LID_R - 12):
        for rad, a in ((92, 60), (58, 90), (30, 140)):
            bd.ellipse([px(x - rad), px(cy - rad), px(x + rad), px(cy + rad)], fill=LIGHT + (a,))
    bulbs = bulbs.filter(ImageFilter.GaussianBlur(px(20)))
    return Image.alpha_composite(layer, bulbs)


def bitcoin(d, cx, cy, h, colour):
    t = h * 0.155
    left = cx - h * 0.30
    top, bot = cy - h * 0.40, cy + h * 0.40
    for x in (left + t * 0.30, left + t * 1.45):
        d.rectangle([px(x - t * 0.21), px(top - h * 0.19), px(x + t * 0.21), px(bot + h * 0.19)],
                    fill=colour)
    d.rectangle([px(left), px(top), px(left + t), px(bot)], fill=colour)
    for y0, y1, bulge in ((top, cy - t * 0.16, 0.90), (cy + t * 0.16, bot, 1.0)):
        r = (y1 - y0) / 2
        ri = max(r - t, 1.5)
        d.rectangle([px(left), px(y0), px(cx), px(y1)], fill=colour)
        d.ellipse([px(cx - r * bulge), px(y0), px(cx + r * bulge), px(y1)], fill=colour)
        d.ellipse([px(cx - ri * bulge), px(y0 + t), px(cx + ri * bulge), px(y1 - t)],
                  fill=(0, 0, 0, 0))
        d.rectangle([px(left + t), px(y0 + t), px(cx), px(y1 - t)], fill=(0, 0, 0, 0))


def plaque():
    w, h = 122, 152
    cy = 700
    box = (CX - w / 2, cy - h / 2, CX + w / 2, cy + h / 2)
    layer = paint(mask_round(box, 18), ramp([(0.0, GOLD_TOP), (0.35, GOLD_MID), (1.0, GOLD_LOW)]))
    inner = (box[0] + 12, box[1] + 12, box[2] - 12, box[3] - 12)
    layer = Image.alpha_composite(
        layer, paint(mask_round(inner, 11), ramp([(0.0, WOOD_DARK), (1.0, WOOD_DEEP)]))
    )
    mark = canvas()
    bitcoin(ImageDraw.Draw(mark), CX + 3, cy + 2, 76, GOLD_TOP + (255,))
    layer = Image.alpha_composite(layer, mark)
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(18),
        outline=GOLD_EDGE + (200,), width=px(3)
    )
    return Image.alpha_composite(layer, ring)


def render():
    img = background()
    img = Image.alpha_composite(img, beams())

    shadow = canvas()
    ImageDraw.Draw(shadow).ellipse(
        [px(BODY_L - 30), px(FOOT[1] - 26), px(BODY_R + 30), px(FOOT[1] + 40)],
        fill=(0, 0, 0, 170),
    )
    img = Image.alpha_composite(img, shadow.filter(ImageFilter.GaussianBlur(px(24))))

    img = Image.alpha_composite(img, dome())
    img = Image.alpha_composite(img, slot())
    img = Image.alpha_composite(img, body())

    # Corner straps run the whole height and carry the feet.
    for x in (LID_L + 6, LID_R - 6 - STRAP_W):
        img = Image.alpha_composite(img, gold((x, BAND1[0] - 6, x + STRAP_W, FOOT[1]), 9))

    for y0, y1 in (BAND1, BAND2, FOOT):
        img = Image.alpha_composite(img, gold((LID_L, y0, LID_R, y1), 11, overhang=6))
    img = Image.alpha_composite(img, plaque())

    out = canvas()
    out.paste(img, (0, 0), mask_round((22, 22, N - 22, N - 22), 228))
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
