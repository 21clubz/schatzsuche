#!/usr/bin/env python3
"""Renders the Schatzsuche app icon and writes both forms the program needs.

    python3 scripts/make-icon.py

Produces `assets/AppIcon.icns` for the macOS bundle and `src/icon_data.rs` for
the window icon, which is embedded rather than loaded from the bundle: the icon
cache once held on to a placeholder after a bad build, and a binary that
carries its own icon cannot be caught out that way.

Everything is drawn at 4x and downsampled, which is where the anti-aliasing
comes from. The first version of this artwork was drawn straight at 128 pixels
and every edge showed it.

Needs Pillow (`pip3 install pillow`). The generated files are committed, so
nobody has to run this to build the project.
""" 
import math
import os
import sys

from PIL import Image, ImageDraw, ImageFilter

S = 4  # supersampling
N = 1024  # logical size


def px(v):
    return int(round(v * S))


W = N * S

# Palette, taken from the interface so the icon belongs to the same program.
BG_TOP = (26, 31, 46)
BG_BOT = (11, 13, 19)
BORDER = (46, 54, 76)
GOLD_HI = (250, 224, 158)
GOLD = (226, 172, 82)
GOLD_MID = (198, 143, 60)
GOLD_LO = (140, 95, 34)
WOOD_HI = (92, 60, 37)
WOOD = (60, 38, 23)
WOOD_LO = (36, 23, 14)
WOOD_EDGE = (24, 15, 9)
LIGHT = (255, 214, 150)

# Chest geometry in logical units.
CX = 512
BODY_L, BODY_R = 222, 802
BODY_B = 772
# The lid is held ajar, and the gap between it and the body is where the light
# gets out. Three lines instead of one seam: bottom of the lid, top of the
# body, and the lit slit between them.
LID_T = 278
LID_B = 452
BODY_T = 494
SEAM = (LID_B + BODY_T) / 2


def rr(d, box, radius, **kw):
    d.rounded_rectangle([px(box[0]), px(box[1]), px(box[2]), px(box[3])], px(radius), **kw)


def vertical_gradient(size, top, bottom):
    """A one-pixel-wide column stretched out; cheaper than shading per pixel."""
    col = Image.new("RGB", (1, size))
    for y in range(size):
        f = y / max(size - 1, 1)
        col.putpixel(
            (0, y),
            tuple(int(top[i] + (bottom[i] - top[i]) * f) for i in range(3)),
        )
    return col.resize((size, size))


def background():
    img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    grad = vertical_gradient(W, BG_TOP, BG_BOT).convert("RGBA")

    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [px(24), px(24), px(N - 24), px(N - 24)], px(226), fill=255
    )
    img.paste(grad, (0, 0), mask)

    # Hairline, brighter along the top edge where light would catch it.
    ring = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    rd = ImageDraw.Draw(ring)
    rd.rounded_rectangle(
        [px(24), px(24), px(N - 24), px(N - 24)],
        px(226),
        outline=BORDER + (255,),
        width=px(3),
    )
    top_fade = Image.new("L", (W, W))
    for y in range(W):
        top_fade.paste(int(255 * max(0.0, 1 - (y / W) * 2.4)), (0, y, W, y + 1))
    highlight = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    ImageDraw.Draw(highlight).rounded_rectangle(
        [px(24), px(24), px(N - 24), px(N - 24)],
        px(226),
        outline=(96, 110, 148, 255),
        width=px(3),
    )
    ring.paste(highlight, (0, 0), top_fade)
    return Image.alpha_composite(img, ring)


def glow():
    """Warm light escaping the lid: a soft pool plus a few wide, faint rays.

    The old icon used hard straight beams that read as a clip-art starburst.
    These are drawn wide, at low opacity, and blurred until the edges are gone.
    """
    g = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(g)

    for i, ang in enumerate(range(-160, -19, 35)):
        a = math.radians(ang)
        spread = math.radians(11.0)
        length = 470
        x0, y0 = CX, SEAM
        p1 = (x0 + math.cos(a - spread) * length, y0 + math.sin(a - spread) * length)
        p2 = (x0 + math.cos(a + spread) * length, y0 + math.sin(a + spread) * length)
        alpha = 20 if i % 2 == 0 else 13
        d.polygon(
            [(px(x0), px(y0)), (px(p1[0]), px(p1[1])), (px(p2[0]), px(p2[1]))],
            fill=LIGHT + (alpha,),
        )
    g = g.filter(ImageFilter.GaussianBlur(px(52)))

    pool = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    pd = ImageDraw.Draw(pool)
    for r, a in ((330, 20), (230, 30), (140, 44), (76, 62)):
        pd.ellipse(
            [px(CX - r), px(SEAM - r * 0.50), px(CX + r), px(SEAM + r * 0.50)],
            fill=LIGHT + (a,),
        )
    for r, a in ((300, 16), (200, 22)):
        pd.ellipse(
            [px(CX - r), px(LID_T - r * 0.55), px(CX + r), px(LID_T + r * 0.55)],
            fill=LIGHT + (a,),
        )
    pool = pool.filter(ImageFilter.GaussianBlur(px(40)))
    return Image.alpha_composite(g, pool)


def horizontal_gradient(size, stops):
    """`stops` is [(position 0..1, colour)], interpolated across the width."""
    row = Image.new("RGB", (size, 1))
    for x in range(size):
        f = x / max(size - 1, 1)
        for i in range(len(stops) - 1):
            p0, c0 = stops[i]
            p1, c1 = stops[i + 1]
            if p0 <= f <= p1:
                k = (f - p0) / max(p1 - p0, 1e-6)
                row.putpixel(
                    (x, 0),
                    tuple(int(c0[j] + (c1[j] - c0[j]) * k) for j in range(3)),
                )
                break
    return row.resize((size, size))


def lid():
    """The dome, shaded across its curve with a highlight where light lands."""
    mask = Image.new("L", (W, W), 0)
    md = ImageDraw.Draw(mask)
    ry = LID_B - LID_T
    md.ellipse([px(BODY_L - 14), px(LID_B - ry), px(BODY_R + 14), px(LID_B + ry)], fill=255)
    md.rectangle([0, px(LID_B), W, W], fill=0)

    shade = horizontal_gradient(
        W,
        [
            (0.0, WOOD_EDGE),
            (0.18, WOOD_LO),
            (0.36, WOOD_HI),
            (0.55, WOOD),
            (0.82, WOOD_LO),
            (1.0, WOOD_EDGE),
        ],
    ).convert("RGBA")

    out = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    out.paste(shade, (0, 0), mask)

    # Specular: the light inside the chest catches the curve near the top left.
    spec = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    ImageDraw.Draw(spec).ellipse(
        [px(CX - 210), px(LID_T + 12), px(CX - 20), px(LID_T + 96)],
        fill=(255, 226, 176, 70),
    )
    spec = spec.filter(ImageFilter.GaussianBlur(px(26)))
    spec.putalpha(Image.composite(spec.split()[3], Image.new("L", (W, W), 0), mask))
    out = Image.alpha_composite(out, spec)

    edge = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    ed = ImageDraw.Draw(edge)
    ed.ellipse(
        [px(BODY_L - 14), px(LID_B - ry), px(BODY_R + 14), px(LID_B + ry)],
        outline=WOOD_EDGE + (255,),
        width=px(4),
    )
    ed.rectangle([0, px(LID_B), W, W], fill=(0, 0, 0, 0))
    out = Image.alpha_composite(out, edge)

    warm = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    wd = ImageDraw.Draw(warm)
    wd.ellipse(
        [px(BODY_L - 14), px(LID_B - ry), px(BODY_R + 14), px(LID_B + ry)],
        outline=(216, 160, 94, 175),
        width=px(6),
    )
    wd.rectangle([0, px(LID_B - 40), W, W], fill=(0, 0, 0, 0))
    warm = warm.filter(ImageFilter.GaussianBlur(px(4)))
    return Image.alpha_composite(out, warm)


def body():
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    rr(d, (BODY_L, BODY_T, BODY_R, BODY_B), 22, fill=WOOD + (255,))

    grad = vertical_gradient(W, WOOD_HI, WOOD_LO).convert("RGBA")
    mask = layer.split()[3]
    layer.paste(grad, (0, 0), mask)

    # Plank seams.
    pl = ImageDraw.Draw(layer)
    for x in (BODY_L + 138, CX, BODY_R - 138):
        pl.rectangle(
            [px(x - 2), px(BODY_T + 10), px(x + 2), px(BODY_B - 10)],
            fill=WOOD_EDGE + (120,),
        )

    edge = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    rr(ImageDraw.Draw(edge), (BODY_L, BODY_T, BODY_R, BODY_B), 22,
       outline=WOOD_EDGE + (255,), width=px(5))
    return Image.alpha_composite(layer, edge)


def gold_bar(box, radius=10):
    """A band with a lit top edge and a shadowed bottom, not a flat rectangle."""
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    rr(d, box, radius, fill=GOLD + (255,))
    grad = vertical_gradient(W, GOLD_HI, GOLD_LO).convert("RGBA")
    layer.paste(grad, (0, 0), layer.split()[3])

    hi = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    rr(ImageDraw.Draw(hi), (box[0] + 3, box[1] + 2, box[2] - 3, box[1] + 7), 3,
       fill=GOLD_HI + (190,))
    out = Image.alpha_composite(layer, hi)
    ring = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    rr(ImageDraw.Draw(ring), box, radius, outline=GOLD_LO + (220,), width=px(2))
    return Image.alpha_composite(out, ring)


def rivets(xs, y):
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    for x in xs:
        d.ellipse([px(x - 7), px(y - 7), px(x + 7), px(y + 7)], fill=GOLD_LO + (255,))
        d.ellipse([px(x - 4.5), px(y - 5.5), px(x + 3), px(y + 1)], fill=GOLD_HI + (230,))
    return layer


def bitcoin_glyph(cx, cy, h):
    """A ₿ built from primitives: stem, two bowls, two strokes through them."""
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    dark = (58, 36, 16, 255)
    t = h * 0.155  # stroke thickness; thicker and the counters close up
    left = cx - h * 0.30
    right = cx + h * 0.26
    top = cy - h * 0.42
    bot = cy + h * 0.42
    mid = cy

    # Vertical strokes above and below, the mark that makes it a ₿.
    for x in (left + t * 0.35, left + t * 1.5):
        d.rectangle([px(x - t * 0.22), px(top - h * 0.17), px(x + t * 0.22), px(bot + h * 0.17)],
                    fill=dark)

    # Stem.
    d.rectangle([px(left), px(top), px(left + t), px(bot)], fill=dark)

    # Upper and lower bowl: a filled D, then the counter punched back out.
    for y0, y1, bulge in ((top, mid - t * 0.18, 0.92), (mid + t * 0.18, bot, 1.0)):
        r = (y1 - y0) / 2
        d.rectangle([px(left), px(y0), px(cx), px(y1)], fill=dark)
        d.ellipse([px(cx - r * bulge), px(y0), px(cx + r * bulge), px(y1)], fill=dark)
        # counter
        ri = max(r - t, 1.5)
        d.ellipse(
            [px(cx - ri * bulge), px(y0 + t), px(cx + ri * bulge), px(y1 - t)],
            fill=(0, 0, 0, 0),
        )
        d.rectangle([px(left + t), px(y0 + t), px(cx), px(y1 - t)], fill=(0, 0, 0, 0))
    return layer


def plaque():
    cy = 672
    size = 150
    layer = gold_bar((CX - size / 2, cy - size / 2, CX + size / 2, cy + size / 2), radius=20)
    return Image.alpha_composite(layer, bitcoin_glyph(CX + 4, cy, size * 0.62))


def seam_light():
    """The lit gap under the raised lid, brightest at the middle."""
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    # Dark opening first, so the light has something to sit in.
    d.rectangle([px(BODY_L + 6), px(LID_B - 12), px(BODY_R - 6), px(BODY_T + 6)],
                fill=(18, 11, 6, 255))
    steps = 60
    for i in range(steps):
        f = i / (steps - 1)
        x0 = BODY_L + 14 + (BODY_R - BODY_L - 28) * f
        x1 = x0 + (BODY_R - BODY_L) / steps + 2
        # Cosine falloff: bright centre, nothing at the corners.
        k = math.cos((f - 0.5) * math.pi) ** 1.6
        d.rectangle(
            [px(x0), px(LID_B - 4), px(x1), px(BODY_T - 2)],
            fill=(255, int(238 - 30 * (1 - k)), int(198 - 70 * (1 - k)), int(255 * k)),
        )
    return layer.filter(ImageFilter.GaussianBlur(px(5)))


def drop_shadow():
    layer = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    ImageDraw.Draw(layer).ellipse(
        [px(BODY_L - 10), px(BODY_B - 26), px(BODY_R + 10), px(BODY_B + 46)],
        fill=(0, 0, 0, 150),
    )
    return layer.filter(ImageFilter.GaussianBlur(px(22)))


def render():
    img = background()
    img = Image.alpha_composite(img, glow())
    img = Image.alpha_composite(img, drop_shadow())
    img = Image.alpha_composite(img, lid())
    img = Image.alpha_composite(img, body())

    # Vertical straps, then the two horizontal bands over them.
    img = Image.alpha_composite(img, seam_light())
    img = Image.alpha_composite(img, gold_bar((BODY_L - 10, BODY_T - 10, BODY_R + 10, BODY_T + 30), 10))
    img = Image.alpha_composite(img, gold_bar((BODY_L - 10, BODY_B - 42, BODY_R + 10, BODY_B + 8), 10))
    img = Image.alpha_composite(img, rivets((BODY_L + 60, BODY_R - 60), BODY_T + 10))
    img = Image.alpha_composite(img, rivets((BODY_L + 60, BODY_R - 60), BODY_B - 17))
    img = Image.alpha_composite(img, plaque())

    # Clip everything to the rounded square.
    mask = Image.new("L", (W, W), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [px(24), px(24), px(N - 24), px(N - 24)], px(226), fill=255
    )
    out = Image.new("RGBA", (W, W), (0, 0, 0, 0))
    out.paste(img, (0, 0), mask)
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
