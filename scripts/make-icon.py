#!/usr/bin/env python3
"""Renders the Schatzsuche app icon and writes both forms the program needs.

    python3 scripts/make-icon.py

Produces `assets/AppIcon.icns` for the macOS bundle and `src/icon_data.rs` for
the window icon, which is embedded rather than loaded from the bundle: the icon
cache once held on to a placeholder after a bad build, and a binary that
carries its own icon cannot be caught out that way.

The picture is the one the project started with — a lit chest on a dark tile.
Three things separate this from the first attempt at it:

* The straps run over the lid. A chest's ironwork wraps the whole body, from
  the feet up and across the curve. Stopping it at the rim is what made the lid
  read as a bun sitting on a box.
* One light, from the upper left, applied to every surface in the same
  direction. The dome is shaded by its own curvature and the straps crossing it
  are shaded by the same function, so the metal bends with the wood instead of
  lying flat on top of it.
* Contact shadows. Every piece of metal drops one onto the wood beneath it.

Everything is drawn at 4x and downsampled, so the anti-aliasing comes from the
resampling. The first version was painted straight at 128 pixels and every
rounding showed it.

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


BG_TOP = (32, 36, 48)
BG_BOT = (11, 13, 19)
BORDER = (54, 62, 84)

# Wood, warm rather than muddy.
W_LIT = (146, 96, 56)
W_MID = (104, 65, 38)
W_DARK = (66, 40, 23)
W_DEEP = (40, 24, 14)
W_LINE = (28, 16, 9)

G_LIT = (255, 232, 172)
G_MID = (226, 176, 88)
G_LOW = (163, 113, 43)
G_EDGE = (92, 61, 20)

LIGHT = (255, 247, 226)
GLOW = (255, 205, 132)

CX = 512
LID_L, LID_R = 196, 828
LID_T, LID_B = 272, 480
BAND_TOP = (480, 518)
SLOT = (518, 572)
BAND_BOT = (572, 610)
BODY_L, BODY_R = 232, 792
BODY_T, BODY_B = 610, 818
FOOT = (818, 862)
STRAP_HALF = 21
STRAP_X = (CX - 214, CX + 214)


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


def dome_mask(pad=0.0):
    ry = LID_B - LID_T
    m = Image.new("L", (W, W), 0)
    d = ImageDraw.Draw(m)
    d.ellipse(
        [px(LID_L - pad), px(LID_B - ry - pad), px(LID_R + pad), px(LID_B + ry + pad)], fill=255
    )
    d.rectangle([0, px(LID_B), W, W], fill=0)
    return m


def paint(mask, grad):
    layer = canvas()
    layer.paste(grad, (0, 0), mask)
    return layer


def curvature(base_lit, base_mid, base_dark, base_deep):
    """Shading across the lid, lit from the upper left.

    The same ramp is used for the wood and for the straps that cross it, which
    is what makes the metal look wrapped around the curve.
    """
    return ramp(
        [
            (0.00, base_deep),
            (0.10, base_dark),
            (0.30, base_lit),
            (0.46, base_mid),
            (0.72, base_dark),
            (1.00, base_deep),
        ],
        horizontal=True,
    )


def key_light(mask, strength=54):
    """One diagonal wash: brighter to the upper left, darker to the lower right."""
    layer = canvas()
    d = ImageDraw.Draw(layer)
    steps = 90
    for i in range(steps):
        f = i / (steps - 1)
        v = int(strength * (1 - f) ** 1.4)
        y = int(W * f)
        d.rectangle([0, y, W, y + W // steps + 2], fill=(255, 240, 210, v))
    dark = canvas()
    dd = ImageDraw.Draw(dark)
    for i in range(steps):
        f = i / (steps - 1)
        v = int(70 * f**1.6)
        y = int(W * f)
        dd.rectangle([0, y, W, y + W // steps + 2], fill=(20, 10, 4, v))
    both = Image.alpha_composite(layer, dark)
    both.putalpha(Image.composite(both.split()[3], Image.new("L", (W, W), 0), mask))
    return both


def shadow_under(box, radius, spread=16, alpha=185, drop=10):
    layer = canvas()
    ImageDraw.Draw(layer).rounded_rectangle(
        [px(box[0]), px(box[1] + drop), px(box[2]), px(box[3] + drop)], px(radius),
        fill=(16, 8, 2, alpha),
    )
    return layer.filter(ImageFilter.GaussianBlur(px(spread)))


def background():
    img = paint(mask_round((22, 22, N - 22, N - 22), 228), ramp([(0.0, BG_TOP), (1.0, BG_BOT)]))
    vig = canvas()
    ImageDraw.Draw(vig).rounded_rectangle(
        [px(22), px(22), px(N - 22), px(N - 22)], px(228), outline=(0, 0, 0, 150), width=px(70)
    )
    img = Image.alpha_composite(img, vig.filter(ImageFilter.GaussianBlur(px(50))))
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(22), px(22), px(N - 22), px(N - 22)], px(228), outline=BORDER + (255,), width=px(3)
    )
    return Image.alpha_composite(img, ring)


def beams():
    layer = canvas()
    d = ImageDraw.Draw(layer)
    ox, oy = CX, SLOT[0] + 14
    for ang, half, a in ((-90, 11, 34), (-58, 8, 24), (-122, 8, 24), (-30, 6, 16), (-150, 6, 16)):
        r, s, L = math.radians(ang), math.radians(half), 860
        d.polygon(
            [
                (px(ox), px(oy)),
                (px(ox + math.cos(r - s) * L), px(oy + math.sin(r - s) * L)),
                (px(ox + math.cos(r + s) * L), px(oy + math.sin(r + s) * L)),
            ],
            fill=GLOW + (a,),
        )
    layer = layer.filter(ImageFilter.GaussianBlur(px(40)))
    fade = Image.new("L", (W, W), 0)
    fd = ImageDraw.Draw(fade)
    for i in range(64):
        f = i / 63
        rad = int(500 * (1 - f) + 40)
        fd.ellipse([px(ox - rad), px(oy - rad), px(ox + rad), px(oy + rad)],
                   fill=int(255 * (1 - f) ** 0.8))
    fade = fade.filter(ImageFilter.GaussianBlur(px(34)))
    layer.putalpha(Image.composite(layer.split()[3], Image.new("L", (W, W), 0), fade))
    return layer


def grain(mask, top, bottom, n=13):
    """Faint horizontal grain, alpha varying so it never reads as stripes."""
    layer = canvas()
    d = ImageDraw.Draw(layer)
    for i in range(n):
        y = top + (bottom - top) * (i + 0.5) / n
        a = 40 + int(38 * math.sin(i * 2.3))
        d.line([0, px(y), W, px(y)], fill=W_LINE + (max(a, 12),), width=px(2))
    layer = layer.filter(ImageFilter.GaussianBlur(px(2)))
    layer.putalpha(Image.composite(layer.split()[3], Image.new("L", (W, W), 0), mask))
    return layer


def lid():
    m = dome_mask()
    layer = paint(m, curvature(W_LIT, W_MID, W_DARK, W_DEEP))
    layer = Image.alpha_composite(layer, grain(m, LID_T + 20, LID_B - 8, 9))
    layer = Image.alpha_composite(layer, key_light(m, 46))

    rim = canvas()
    ry = LID_B - LID_T
    rd = ImageDraw.Draw(rim)
    rd.ellipse([px(LID_L + 5), px(LID_B - ry + 5), px(LID_R - 5), px(LID_B + ry - 5)],
               outline=(226, 168, 104, 190), width=px(7))
    rd.rectangle([0, px(LID_T + 128), W, W], fill=(0, 0, 0, 0))
    layer = Image.alpha_composite(layer, rim.filter(ImageFilter.GaussianBlur(px(4))))

    edge = canvas()
    ed = ImageDraw.Draw(edge)
    ed.ellipse([px(LID_L), px(LID_B - ry), px(LID_R), px(LID_B + ry)],
               outline=W_LINE + (240,), width=px(5))
    ed.rectangle([0, px(LID_B), W, W], fill=(0, 0, 0, 0))
    return Image.alpha_composite(layer, edge)


def body():
    box = (BODY_L, BODY_T, BODY_R, BODY_B)
    m = mask_round(box, 14)
    layer = paint(m, ramp([(0.0, W_MID), (0.35, W_DARK), (1.0, W_DEEP)]))
    layer = Image.alpha_composite(layer, grain(m, BODY_T + 10, BODY_B - 10, 8))
    layer = Image.alpha_composite(layer, key_light(m, 40))
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(BODY_L), px(BODY_T), px(BODY_R), px(BODY_B)], px(14), outline=W_LINE + (235,), width=px(4)
    )
    return Image.alpha_composite(layer, ring)


def metal(mask, curved=False):
    """Gold with a lit upper edge, a shadowed lower one and a dark outline.

    On the lid the metal is shaded by the same curvature ramp as the wood, so
    it bends with the surface instead of lying flat on top of it.
    """
    grad = (
        curvature(G_LIT, G_MID, G_LOW, G_EDGE)
        if curved
        else ramp([(0.0, G_LIT), (0.2, G_MID), (0.75, G_LOW), (1.0, G_MID)])
    )
    return paint(mask, grad)


def straps():
    """From the feet, up the body, over the lid. One continuous piece."""
    out = canvas()
    dome = dome_mask()
    for x in STRAP_X:
        over = Image.new("L", (W, W), 0)
        ImageDraw.Draw(over).rectangle(
            [px(x - STRAP_HALF), px(LID_T - 20), px(x + STRAP_HALF), px(LID_B)], fill=255
        )
        over = Image.composite(over, Image.new("L", (W, W), 0), dome)
        out = Image.alpha_composite(out, metal(over, curved=True))

        low = mask_round((x - STRAP_HALF, BAND_TOP[0] - 4, x + STRAP_HALF, FOOT[1]), 8)
        out = Image.alpha_composite(out, shadow_under(
            (x - STRAP_HALF, BAND_TOP[0], x + STRAP_HALF, FOOT[1]), 8, 12, 150, 8))
        out = Image.alpha_composite(out, metal(low))

    edge = canvas()
    ed = ImageDraw.Draw(edge)
    for x in STRAP_X:
        ed.line([px(x - STRAP_HALF), px(LID_T - 20), px(x - STRAP_HALF), px(FOOT[1])],
                fill=G_EDGE + (200,), width=px(3))
        ed.line([px(x + STRAP_HALF), px(LID_T - 20), px(x + STRAP_HALF), px(FOOT[1])],
                fill=G_EDGE + (200,), width=px(3))
    edge.putalpha(
        Image.composite(edge.split()[3], Image.new("L", (W, W), 0),
                        Image.composite(Image.new("L", (W, W), 255), dome,
                                        mask_round((0, LID_B, N, N), 0)))
    )
    return Image.alpha_composite(out, edge)


def band(y0, y1, overhang=8, radius=11):
    box = (LID_L - overhang, y0, LID_R + overhang, y1)
    layer = metal(mask_round(box, radius))
    hi = canvas()
    ImageDraw.Draw(hi).rounded_rectangle(
        [px(box[0] + 6), px(box[1] + 3), px(box[2] - 6), px(box[1] + 10)], px(4),
        fill=G_LIT + (205,)
    )
    layer = Image.alpha_composite(layer, hi)
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(radius),
        outline=G_EDGE + (225,), width=px(3)
    )
    return Image.alpha_composite(shadow_under(box, radius, 14, 165, 9), Image.alpha_composite(layer, ring))


def slot():
    layer = canvas()
    d = ImageDraw.Draw(layer)
    d.rectangle([px(LID_L + 4), px(SLOT[0] - 8), px(LID_R - 4), px(SLOT[1] + 8)],
                fill=(20, 11, 4, 255))
    inner = canvas()
    ImageDraw.Draw(inner).rounded_rectangle(
        [px(LID_L + 22), px(SLOT[0] + 5), px(LID_R - 22), px(SLOT[1] - 5)], px(10),
        fill=LIGHT + (255,)
    )
    layer = Image.alpha_composite(layer, inner.filter(ImageFilter.GaussianBlur(px(4))))

    bulbs = canvas()
    bd = ImageDraw.Draw(bulbs)
    cy = (SLOT[0] + SLOT[1]) / 2
    for x in (LID_L + 8, LID_R - 8):
        for rad, a in ((104, 52), (64, 82), (34, 130)):
            bd.ellipse([px(x - rad), px(cy - rad), px(x + rad), px(cy + rad)], fill=LIGHT + (a,))
    return Image.alpha_composite(layer, bulbs.filter(ImageFilter.GaussianBlur(px(22))))


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


def lock():
    w, h = 132, 158
    cy = 714
    box = (CX - w / 2, cy - h / 2, CX + w / 2, cy + h / 2)
    layer = metal(mask_round(box, 20))
    inner = (box[0] + 13, box[1] + 13, box[2] - 13, box[3] - 13)
    layer = Image.alpha_composite(
        layer, paint(mask_round(inner, 12), ramp([(0.0, W_DARK), (1.0, W_DEEP)]))
    )
    mark = canvas()
    bitcoin(ImageDraw.Draw(mark), CX + 3, cy + 2, 80, G_LIT + (255,))
    layer = Image.alpha_composite(layer, mark)
    ring = canvas()
    ImageDraw.Draw(ring).rounded_rectangle(
        [px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(20), outline=G_EDGE + (215,), width=px(3)
    )
    layer = Image.alpha_composite(layer, ring)
    return Image.alpha_composite(shadow_under(box, 20, 18, 175, 11), layer)


def render():
    img = background()
    img = Image.alpha_composite(img, beams())

    ground = canvas()
    ImageDraw.Draw(ground).ellipse(
        [px(BODY_L - 40), px(FOOT[1] - 30), px(BODY_R + 40), px(FOOT[1] + 46)],
        fill=(0, 0, 0, 185),
    )
    img = Image.alpha_composite(img, ground.filter(ImageFilter.GaussianBlur(px(26))))

    img = Image.alpha_composite(img, lid())
    img = Image.alpha_composite(img, slot())
    img = Image.alpha_composite(img, body())
    img = Image.alpha_composite(img, band(*BAND_TOP))
    img = Image.alpha_composite(img, band(*BAND_BOT))
    img = Image.alpha_composite(img, band(*FOOT))
    img = Image.alpha_composite(img, straps())
    img = Image.alpha_composite(img, lock())

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
