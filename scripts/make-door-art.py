#!/usr/bin/env python3
"""Builds the artwork of the opening fork, from the renders in `assets/`.

    python3 scripts/make-door-art.py            # write the assets
    python3 scripts/make-door-art.py preview.png  # see them at door size

Drei Bilder: die zwei Türsymbole und die Seekarte, die hinter ihnen auf dem
Tisch liegt.

`dice-source.png` becomes die Tür „Wallets würfeln" und `key-source.png` die
Tür „Seed retten". Vorher lagen dort von Hand gemalte Münzen und ein flacher
Schlüssel, danach eine Schatzkarte; die Bilder sagen dieselben zwei Dinge —
blind würfeln, die eigene Wallet zurückholen — im Material der Truhe, nach der
das Programm heißt.

**Zwei Wege auf die Kachel, und der Unterschied ist keine Laune.** Der
Schlüssel ist ein *freigestelltes* Motiv: er wurde vor cremefarbenem Grund
gerendert, [`cutout`] flutet den weg, und übrig bleibt ein Gegenstand, der auf
der Kachel schwebt. Die Würfel sind ein *Ausschnitt aus einer Szene* — sie
liegen zwischen Fingern, Laterne und Seekarte, und die Farben liegen zu dicht
beieinander, um sie zu trennen: gemessen kommt das Würfelholz auf einen
Farbton von 20 bis 27 Grad, das Pergament auf 31 bis 33, die Finger auf 16.
Jede Schwelle, die das Pergament wegnimmt, frisst auch den Würfel an, und der
obere ist zudem bewegungsunscharf — eine harte Kante an einem unscharfen Rand
sieht immer nach Fehler aus. Rund beschnitten braucht es die Trennung gar
nicht: die Messingwinkel der Kachel rahmen den Kreis, und das liest sich als
eingelassenes Medaillon.

Both are written as square 256-pixel sheets with the subject centred, and the
window draws each into the same box. That is deliberate: the two subjects have
very different shapes, so if the sizing lived in the Rust it would need a
per-door fudge factor. Here it is one number per picture, next to a preview
that shows what the number does.

Needs Pillow (`pip3 install pillow`). The generated files are committed, so
nobody has to run this to build the project.
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFilter

from cutout import cutout, fit, unmark

N = 256

# How much of the sheet each subject fills. The two are not the same, because
# equal width does not mean equal weight: the map is a solid lit slab and the
# key is a thin diagonal with holes in it. Sized to the same box the map
# crowds its tile and reads as a picture in a frame, while the key still has
# air around it — so the map is held well back and the key only a little.
# Both judged against the preview at door size, not by measurement.
FILL = {
    "map": 0.76,
    "key": 0.90,
}

# Der Ausschnitt aus der Würfelszene, in Bildpunkten der Vorlage (1024 × 1024),
# und quadratisch: das Bullauge schneidet daraus den Kreis.
#
# Gewählt ist der Ausschnitt nach der Diagonale, die die zwei Würfel bilden —
# der fliegende oben links, der liegende unten rechts. Enger geschnitten fällt
# einer aus dem Kreis, weiter geschnitten werden beide zu klein, um bei 68
# Punkten noch als Würfel lesbar zu sein.
DICE_BOX = (437, 470, 767, 800)

# Kantenlänge der Seekarte hinter der Gabelung.
#
# Kleiner als die Vorlage, mit Absicht: Sie wird mit sehr wenig Deckung über
# das Holz gelegt, und was dabei ankommt, sind weiche Flächen und ein paar
# dunkle Linien — Schärfe, die niemand sieht, würde nur das Programm um ein
# Megabyte schwerer machen.
MAP_BG_PX = 768

# Where the render's sparkle has to be painted out, and what to paint over it
# with. Only the map needs this: on the chest and the key the mark sat on the
# backdrop and left with it, and running the search over a picture that does
# not need it would find lit metal instead. The window is the parchment's
# bottom-right corner; the patch comes from 110 pixels straight up, which is
# the same burnt edge a moment earlier along and matches its tone to within a
# value or two. Both were measured against the render, not guessed.
SPARKLE = {"map": ((872, 872, 940, 932), (0, -110))}

# The door tile from `gui.rs`, so the preview shows the real thing.
TILE_BG = (34, 27, 18)
TILE_EDGE = (70, 55, 32)
TILE_PX = 76
ART_PX = 68


def source_of(name):
    here = os.path.dirname(os.path.abspath(__file__))
    return os.path.join(os.path.dirname(here), "assets", f"{name}-source.png")


def build(name):
    """Ein freigestelltes Motiv: Grund wegfluten, Funken übermalen, einpassen."""
    cut = cutout(Image.open(source_of(name)))
    if name in SPARKLE:
        box, offset = SPARKLE[name]
        cut, found = unmark(cut, box, offset)
        print(f"{name}: painted over the sparkle, {found} pixels", file=sys.stderr)
    margin = int(N * (1.0 - FILL[name]) / 2)
    return fit(cut, N, margin)


def build_porthole(name, box):
    """Ein Ausschnitt aus einer Szene, rund beschnitten — siehe Kopf der Datei.

    Der Rand ist um einen Bildpunkt weich gezeichnet. Ohne das steht der Kreis
    bei 68 Punkten als Treppe da, weil er auf ein Achtel seiner Größe verkleinert
    wird und die harte Kante dabei aliast.
    """
    art = Image.open(source_of(name)).convert("RGB").crop(box).resize((N, N), Image.LANCZOS)
    mask = Image.new("L", (N * 4, N * 4), 0)
    ImageDraw.Draw(mask).ellipse([0, 0, N * 4 - 1, N * 4 - 1], fill=255)
    mask = mask.resize((N, N), Image.LANCZOS)
    art = art.convert("RGBA")
    art.putalpha(mask)
    return art


def preview(sheets, path, zoom=4):
    """The two icons in their tile, at the size the window draws them.

    Rendered at a zoom so the small size can actually be judged: what matters
    is whether each still reads at 68 pixels, not whether it is pretty at 256.
    """
    t = TILE_PX * zoom
    sheet = Image.new("RGB", (t * 2 + 30 * zoom, t + 20 * zoom), (13, 15, 22))
    for i, (name, art) in enumerate(sheets):
        tile = Image.new("RGBA", (t, t), (0, 0, 0, 0))
        d = ImageDraw.Draw(tile)
        d.rounded_rectangle(
            [0, 0, t - 1, t - 1],
            radius=18 * zoom,
            fill=TILE_BG + (255,),
            outline=TILE_EDGE + (255,),
            width=zoom,
        )
        # The gold bloom the door paints behind whatever sits on the tile.
        glow = Image.new("RGBA", (t, t), (0, 0, 0, 0))
        gd = ImageDraw.Draw(glow)
        gd.ellipse(
            [t / 2 - 40 * zoom, t / 2 - 34 * zoom, t / 2 + 40 * zoom, t / 2 + 34 * zoom],
            fill=(226, 170, 78, 60),
        )
        tile = Image.alpha_composite(tile, glow.filter(ImageFilter.GaussianBlur(8 * zoom)))

        a = art.resize((ART_PX * zoom, ART_PX * zoom), Image.LANCZOS)
        tile.paste(a, ((t - a.width) // 2, (t - a.height) // 2), a)
        sheet.paste(tile, (10 * zoom + i * (t + 20 * zoom), 10 * zoom), tile)
    sheet.save(path)


if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(here)

    # Nach der Tür benannt und nicht nach dem Gegenstand — genau dafür war das
    # gut: die linke Tür heißt inzwischen „Wallets würfeln" und trägt Würfel
    # statt der Karte, ohne dass im Rust eine Zeile umzieht.
    #
    # Die Karte, die dort lag, ist auf den Tisch gewandert: sie liegt jetzt
    # hinter beiden Türen (`assets/map-bg.png`).
    sheets = [
        ("search", build_porthole("dice", DICE_BOX)),
        ("recover", build("key")),
    ]

    if len(sys.argv) > 1:
        preview(sheets, sys.argv[1])
        print(f"wrote {sys.argv[1]}", file=sys.stderr)
        sys.exit(0)

    for door, art in sheets:
        out = os.path.join(root, "assets", f"door-{door}.png")
        art.save(out)
        print(f"wrote assets/door-{door}.png", file=sys.stderr)

    # Ohne Rand eingepasst: das Fenster legt sie ohnehin nur locker unter die
    # Türen, und ein einkalkulierter Rand wäre dort bloß verschenkte Fläche.
    cut = cutout(Image.open(source_of("map")))
    box, offset = SPARKLE["map"]
    cut, found = unmark(cut, box, offset)
    print(f"map: painted over the sparkle, {found} pixels", file=sys.stderr)
    out = os.path.join(root, "assets", "map-bg.png")
    fit(cut, MAP_BG_PX, 0).save(out)
    print(f"wrote assets/map-bg.png", file=sys.stderr)
