#!/usr/bin/env python3
"""Builds the tiling plank wood in `assets/wood-256.png`.

    python3 scripts/make-wood.py

Der Dateiname ist historisch: die Kachel misst inzwischen 1024 Pixel (siehe
`SIZE`), aber der Pfad steht in `icon_data.rs`, und die Datei dort bleibt
unangetastet — das Fenster fragt die Kachelgröße ohnehin zur Laufzeit ab.

Das hier war einmal ein Furnier: eine gleichmäßige, sehr leise Faser über der
ganzen Fläche. Der Auftrag wurde ein anderer — gealtertes **Plankenholz** wie
auf einem handgemalten Piratenschild: einzelne waagerechte Bretter, dunkle
Fugen dazwischen, Stoßfugen, wo ein Brett endet, unregelmäßige Maserung, Risse,
Astlöcher, und je Brett ein eigener Ton. Drei Entscheidungen tragen das:

**Deckend, nicht Lasur — das ist die wichtigste Entscheidung hier.** Die
Kachel war zweimal ein Alpha-Overlay über dem Palettengrund, und beide Male
kam ein blasses Linienmuster heraus. Der Grund ist keine Geschmacksfrage,
sondern Arithmetik: der Grund liegt bei RGB um 15, bis Schwarz sind es zwölf
Stufen — eine „fast schwarze Fuge" kann auf so einem Grund gar nicht dunkler
aussehen als er selbst. Dazu staucht der Renderer dunkle Lasuren zusätzlich
und verstärkt helle um ein Mehrfaches (Kalibrierkachel durch `--screenshot`
fotografiert: hell Alpha 1 → +11 Stufen, dunkel Alpha 24 → −2). Übrig blieben
allein die hellen Maserungskämme. Deckend gemalt gilt dagegen: was in der
Kachel steht, kommt an — der Brettkörper liegt im Mittelton, und Fugen, Risse
und Astkerne liegen sichtbar **darunter**.

**Nahtlos durch Konstruktion, nicht durch Nachbearbeitung.** Alles, was von x
abhängt, ist aus Sinustermen mit ganzzahliger Periodenzahl gebaut (Fugenwege,
Fugenbreite, Verwerfung, Schwellung); alles Punktförmige rechnet mit
toroidalem Abstand oder stempelt mit Modulo. Die Bretter versetzen um eine
halbe Höhe, damit keine Fuge auf den Zeilen liegt, die der Naht-Test misst.
`the_wood_tile_has_no_seam` prüft es nach.

**Die Farbe kommt weiter aus der Palette.** Die Kachel ist praktisch
grau — Helligkeit plus ein leiser Wärme-Stich je Brett — und wird beim Malen
mit `Palette::wood` **multipliziert** (`draw_grain` in `gui.rs`). So dient
eine Kachel weiter Walnuss und Mahagoni gleichermaßen; echte Brauntöne hier
hineinzumalen hieße, beim nächsten Palettenwechsel alles neu zu zeichnen.

No Pillow, unlike `make-icon.py`: the whole job is arithmetic and a zlib call,
and the generated file is committed anyway.
"""
import math
import random
import struct
import zlib

TAU = 2.0 * math.pi

#: 1024, nicht mehr 512: die Kachel wiederholt sich im Fenster praktisch nicht
#: mehr (1,2-mal in der Breite), und ein Astloch oder Riss fällt als
#: Wiederholung erst auf, wenn man ihn doppelt sieht.
SIZE = 1024

#: Brettreihen je Kachel. Sechs ergeben rund 170 Punkte je Brett — grob genug,
#: dass ein Fenster vier bis fünf Planken zeigt statt einer Tapete aus Latten.
PLANKS = 6

# --- Helligkeiten der deckenden Kachel ----------------------------------------
#
# Alles in Grauwerten von 0 bis 255, vor der Tönung durch die Palette.

#: Mittlere Helligkeit des Brettkörpers. `draw_grain` multipliziert die Kachel
#: mit `Palette::wood` (≈ `panel` · 1,3); bei zwei Dritteln Vollaussteuerung
#: landet der Brettkörper damit knapp **unter** der Panel-Helligkeit — Karten
#: bleiben heller als das Holz, und jede Textfarbe, die auf `panel` besteht,
#: besteht auf dem Holz erst recht.
BODY_LUMA = 170.0
#: Die Fuge. Fast schwarz, aber nicht null: ganz Schwarz säuft ab, eine
#: Restspur des Palettentons hält die Fuge im selben Braun wie das Brett.
SEAM_LUMA = 22.0
#: Risse und Stoßfugen — dunkler als jede Maserung, heller als die Fuge.
CRACK_LUMA = 34.0
#: Der Kern eines Astlochs.
KNOT_LUMA = 38.0

#: Wie stark Kamm und Tal der Maserung den Brettkörper anheben und absenken.
#: Die Kämme sind Glanzlicht, nicht Hauptdarsteller — dominiert die Maserung,
#: ist es wieder das alte Wellenmuster.
RIDGE_GAIN = 0.30
VALLEY_GAIN = 0.24

#: Je Brett: Helligkeitsstreuung und Wärme-Stich (Rot rauf, Blau runter) —
#: mehrere unterschiedlich gealterte Bretter statt einer bedruckten Fläche.
TONE_SPREAD = 0.14
WARM_MAX = 0.10

#: Unterhalb dieser Anteile der Vollaussteuerung bleibt Maserung unsichtbar;
#: nur Kämme leuchten, nur Täler dunkeln. Ohne die Schwellen ist die Faser
#: ein Streifenstoff.
LIGHT_GATE = 0.55
DARK_GATE = 0.38

#: Womit die Stoßfugen in die Überlagerung gestempelt werden; Risse stempeln
#: schwächer und laufen an den Enden aus.
STAMP_FULL = 185.0

rng = random.Random(21)


def periodic_wave(source: random.Random, terms: int, max_amp: float):
    """Eine in x nahtlose Welle: Summe von Sinustermen mit **ganzzahliger**
    Periodenzahl und verstreuten Phasen, auf `max_amp` normiert."""
    parts = [
        (source.randint(1, 4), source.uniform(0.35, 1.0), source.uniform(0.0, TAU))
        for _ in range(terms)
    ]
    total = sum(a for _, a, _ in parts) or 1.0

    def wave(x: float) -> float:
        return sum(a * math.sin(TAU * k * x / SIZE + ph) for k, a, ph in parts) / total * max_amp

    return wave


# --- Fugen ---------------------------------------------------------------------
#
# Je Brettgrenze ein gewellter Weg (Amplitude klein gegen die Bretthöhe, damit
# sich Fugen nie kreuzen) und eine entlang x schwankende Breite — eine Fuge,
# die überall gleich dick ist, sieht gezogen aus, nicht gewachsen.

seam_y: list[list[float]] = []
seam_hw: list[list[float]] = []
for i in range(PLANKS):
    # Um eine halbe Bretthöhe versetzt: so liegt keine Fuge auf dem
    # Kachelrand. Das unterste Brett läuft mittendrin über den Umlauf — was
    # der Naht-Test misst, sind dann zwei benachbarte Zeilen ruhigen Holzes
    # und nicht das Innere einer Fuge, wo jeder Pixelschritt steil ist.
    base = (i + 0.5) * SIZE / PLANKS
    path = periodic_wave(rng, 3, rng.uniform(4.0, 8.0))
    width = periodic_wave(rng, 2, 1.5)
    seam_y.append([base + path(x) for x in range(SIZE)])
    seam_hw.append([max(1.2, 3.2 + width(x)) for x in range(SIZE)])

# --- Bretter -------------------------------------------------------------------
#
# Jedes Brett bekommt seine eigene Maserung: Liniendichte, Phase, Verwerfung
# entlang der Länge, Schwellung (mal dichter, mal ruhiger) und einen
# Alterston. Zwei benachbarte Bretter aus derselben Formel wären sofort wieder
# Furnier.

planks = []
for _ in range(PLANKS):
    warp = periodic_wave(rng, 4, rng.uniform(1.6, 3.2))
    swell = periodic_wave(rng, 2, 0.45)
    planks.append(
        dict(
            freq=rng.uniform(5.0, 9.0),
            phase=rng.uniform(0.0, TAU),
            sharp=rng.uniform(1.3, 1.9),
            tone=rng.uniform(1.0 - TONE_SPREAD, 1.0 + TONE_SPREAD),
            warm=rng.uniform(-0.05, WARM_MAX),
            warp_x=[warp(x) for x in range(SIZE)],
            swell_x=[1.0 + swell(x) for x in range(SIZE)],
        )
    )

# --- Astlöcher -------------------------------------------------------------------
#
# `(brett, x, y-anteil im brett, radius, stärke)`. Mitten im Brett, nie in der
# Fuge; der x-Abstand rechnet toroidal, damit ein Ast am Kachelrand nahtlos
# weitergeht.

#
# Das unterste Brett — das über den Kachelrand läuft — bekommt keinen Ast:
# ein Astkern auf den Zeilen, die der Naht-Test vergleicht, sähe in der
# Statistik aus wie eine Naht (dasselbe Argument wie `X_MARGIN` unten).
KNOTS = [
    (p, rng.uniform(0.08, 0.92) * SIZE, rng.uniform(0.32, 0.68), rng.uniform(14.0, 26.0), rng.uniform(0.7, 1.0))
    for p in rng.sample(range(PLANKS - 1), 4)
]

# --- Risse und Stoßfugen: vorab gestempelt --------------------------------------
#
# Beides sind Wanderwege, keine Wellen — sie passen nicht in die
# Perioden-Konstruktion. Stattdessen werden sie mit Modulo in eine
# Überlagerungsschicht gestempelt: der Rand wickelt sich von selbst um.

overlay = bytearray(SIZE * SIZE)


def stamp(x: float, y: float, alpha: float) -> None:
    if alpha <= 0.0:
        return
    # Erst das Float-Modulo, dann der Ganzzahlschnitt: `int()` schneidet
    # Richtung Null, und ein Riss, der bei x = -0,5 über den Rand läuft,
    # landete damit in Spalte 0 statt 1023 — eine senkrechte Naht, die der
    # Test prompt gefunden hat.
    o = int(y % SIZE) * SIZE + int(x % SIZE)
    if overlay[o] < int(alpha):
        overlay[o] = int(alpha)


def plank_bounds(p: int, x: int) -> tuple[float, float]:
    """Ober- und Unterkante von Brett `p` an dieser Stelle."""
    top = seam_y[p][x]
    if p + 1 < PLANKS:
        return top, seam_y[p + 1][x]
    return top, seam_y[0][x] + SIZE


# Stoßfugen und Risse bleiben den äußersten Spalten fern (`X_MARGIN`). Nicht,
# weil der Umlauf dort bräche — das Modulo wickelt korrekt —, sondern weil der
# Naht-Test genau die Randspalten gegeneinander misst: eine harte Kante, die
# zufällig auf der Messspalte liegt, bläht deren Mittel auf und sieht in der
# Statistik aus wie eine Naht, die es im Bild nicht gibt. Die wellenbasierten
# Inhalte (Fugen, Maserung, Äste) laufen weiter bis an den Rand und darüber.
X_MARGIN = 40.0

# Stoßfugen: wo ein Brett endet und das nächste ansetzt. Ein bis zwei je
# Reihe, leicht schräg — das Versetzen der Stöße ist es, was eine Wand aus
# Brettern von einer linierten Fläche unterscheidet.
for p in range(PLANKS):
    for _ in range(rng.randint(1, 2)):
        x0 = rng.uniform(X_MARGIN, SIZE - X_MARGIN)
        lean = rng.uniform(-0.12, 0.12)
        top, bot = plank_bounds(p, int(x0) % SIZE)
        y = top
        while y < bot:
            x_here = x0 + (y - top) * lean
            stamp(x_here, y, STAMP_FULL)
            stamp(x_here + 1.0, y, STAMP_FULL * 0.65)
            y += 1.0

# Risse: laufen mit der Faser, also fast waagerecht, wandern leicht und laufen
# an beiden Enden aus. Bleiben in ihrem Brett — ein Riss, der durch eine Fuge
# hindurchgeht, wäre ein Riss im Bild, nicht im Holz.
for _ in range(14):
    # Wie bei den Ästen: nicht ins unterste, über den Rand laufende Brett.
    p = rng.randrange(PLANKS - 1)
    step = rng.choice([-1.0, 1.0])
    slope = rng.uniform(-0.16, 0.16)
    peak = rng.uniform(0.6, 1.0) * STAMP_FULL * 0.85
    length = int(rng.uniform(90.0, 340.0))
    # Startpunkt so, dass auch das andere Ende diesseits des Randstreifens
    # bleibt — ein Riss endet im Brett, nicht in der Statistik.
    if step > 0.0:
        x = rng.uniform(X_MARGIN, SIZE - X_MARGIN - length)
    else:
        x = rng.uniform(X_MARGIN + length, SIZE - X_MARGIN)
    top, bot = plank_bounds(p, int(x) % SIZE)
    y = top + (bot - top) * rng.uniform(0.25, 0.75)
    for s in range(length):
        a = peak * math.sin(math.pi * s / length)
        x += step
        y += slope + rng.uniform(-0.35, 0.35)
        stamp(x, y, a)
        if rng.random() < 0.5:
            stamp(x, y + 1.0, a * 0.5)


def toroidal_dx(a: float, b: float) -> float:
    d = (a - b) % SIZE
    return d if d <= SIZE / 2 else d - SIZE


def main() -> None:
    # Eigener, fest gesäter Strom für das Korn: die Datei ist eingecheckt,
    # zwei Läufe müssen dieselben Bytes ergeben.
    speckle = random.Random(1021)

    rows = bytearray()
    for y in range(SIZE):
        rows.append(0)  # PNG filter byte: none.
        for x in range(SIZE):
            # Welches Brett? Die Fugen kreuzen sich nicht, also reicht die
            # erste, die unter y liegt. Oberhalb der obersten Fuge läuft das
            # unterste Brett von der anderen Kachelkante herein.
            first = seam_y[0][x]
            if y < first:
                p_idx = PLANKS - 1
                top, bot = seam_y[p_idx][x] - SIZE, first
            else:
                p_idx = PLANKS - 1
                top, bot = plank_bounds(p_idx, x)
                for i in range(PLANKS - 1):
                    if seam_y[i][x] <= y < seam_y[i + 1][x]:
                        p_idx, top, bot = i, seam_y[i][x], seam_y[i + 1][x]
                        break
            pl = planks[p_idx]
            luma = BODY_LUMA * pl["tone"]

            d_top = y - top
            d_bot = bot - y
            hw_top = seam_hw[p_idx][x]
            hw_bot = seam_hw[(p_idx + 1) % PLANKS][x]

            # Das Brett fällt zu seinen Kanten hin leicht ab — dieses weiche
            # Gefälle macht es rund und massiv statt aufgedruckt.
            luma *= 1.0 - 0.12 * math.exp(-d_top / 9.0) - 0.10 * math.exp(-d_bot / 9.0)

            # Maserung: Linien entlang des Bretts, verworfen entlang der
            # Länge, um die Äste herumgebogen. `sharp` macht aus dem Sinus
            # schmale Rücken mit breiten Tälern — Holz, kein Wellblech.
            v = d_top / (bot - top)
            arg = TAU * pl["freq"] * v + pl["phase"] + pl["warp_x"][x]
            knot_drop = 0.0
            knot_shine = 0.0
            for kp, kx, kyf, kr, ks in KNOTS:
                if kp != p_idx:
                    continue
                dx = toroidal_dx(x, kx)
                dy = y - (top + (bot - top) * kyf)
                d = math.hypot(dx, dy)
                if d < kr * 3.0:
                    arg += 3.0 * ks * math.tanh(dy / (0.35 * kr)) * math.exp(-((d / (1.4 * kr)) ** 2))
                    core = math.exp(-((d / (0.42 * kr)) ** 2))
                    rings = math.cos(TAU * d / (kr * 0.62)) * math.exp(-((d / kr) ** 2))
                    # Kern und dunkle Ringe ziehen zum Astdunkel, der helle
                    # Ringanteil legt einen feinen Lichtring darum.
                    knot_drop = max(knot_drop, min(1.0, ks * (core + 0.55 * max(0.0, -rings))))
                    knot_shine = max(knot_shine, 0.10 * ks * max(0.0, rings) * (1.0 - core))
            g = math.sin(arg)
            g = math.copysign(abs(g) ** pl["sharp"], g) * pl["swell_x"][x]
            g += speckle.uniform(-0.10, 0.10)
            if g > LIGHT_GATE:
                t = min(1.0, (g - LIGHT_GATE) / (1.0 - LIGHT_GATE))
                luma *= 1.0 + RIDGE_GAIN * t
            elif g < -DARK_GATE:
                t = min(1.0, (-g - DARK_GATE) / (1.0 - DARK_GATE))
                luma *= 1.0 - VALLEY_GAIN * t

            # Die Lichtkante an der Oberkante des Bretts, direkt unter der
            # Fuge — sie macht aus „Linie auf Fläche" ein „Brett über Brett".
            if hw_top < d_top <= hw_top + 2.0:
                luma *= 1.18
            if knot_shine > 0.0:
                luma *= 1.0 + knot_shine

            # Dunkles zieht nach unten, in fester Rangfolge: erst der Ast,
            # dann Risse und Stoßfugen, zuletzt — und am tiefsten — die Fuge.
            if knot_drop > 0.0:
                luma += (KNOT_LUMA - luma) * knot_drop
            c = overlay[y * SIZE + x] / STAMP_FULL
            if c > 0.0:
                luma += (CRACK_LUMA - luma) * min(1.0, c)
            s = 0.0
            if d_top <= hw_top:
                s = math.cos(0.5 * math.pi * d_top / hw_top)
            if d_bot <= hw_bot:
                s = max(s, math.cos(0.5 * math.pi * d_bot / hw_bot))
            if s > 0.0:
                luma += (SEAM_LUMA - luma) * s

            # Poren.
            luma *= 1.0 + speckle.uniform(-0.05, 0.05)
            luma = max(12.0, min(250.0, luma))

            # Der Wärme-Stich des Bretts: Rot rauf, Blau runter. Die Tönung
            # zur Farbwelt macht später `draw_grain` mit `Palette::wood`.
            warm = pl["warm"]
            r_ = int(min(255.0, max(0.0, luma * (1.0 + warm))))
            g_ = int(luma)
            b_ = int(min(255.0, max(0.0, luma * (1.0 - 1.4 * warm))))
            rows += bytes((r_, g_, b_, 255))

    def chunk(kind: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + kind
            + data
            + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(rows), 9))
    png += chunk(b"IEND", b"")

    path = "assets/wood-256.png"
    with open(path, "wb") as f:
        f.write(png)
    print(
        f"{path}: {SIZE}x{SIZE} deckend, {PLANKS} Planken, {len(KNOTS)} Astlöcher, "
        f"Körper um {BODY_LUMA:.0f}, Fugen bei {SEAM_LUMA:.0f} von 255, {len(png)} Bytes"
    )


if __name__ == "__main__":
    main()
