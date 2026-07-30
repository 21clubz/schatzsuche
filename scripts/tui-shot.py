#!/usr/bin/env python3
"""Fotografiert die Terminal-Oberfläche als Text.

    python3 scripts/tui-shot.py target/release/schatzsuche --duration 6

Warum es das braucht: Die Oberfläche in `tui.rs` verlangt ein echtes Terminal —
`enable_raw_mode` scheitert an einer Pipe. Wer sie also aus einem Skript heraus
starten will, bekommt gar nichts zu sehen, und `script -q /dev/null …` versagt,
sobald die eigene Standardeingabe kein Terminal ist. Dieses Skript baut mit
`os.forkpty` selbst eines, setzt eine Fenstergröße (ohne die hat die Oberfläche
keine Fläche und zeichnet nichts), sammelt die Steuersequenzen und spielt sie
auf ein Raster zurück. Heraus kommt das letzte Bild als Text — lesbar, in ein
Diff kopierbar, ohne Screenshot.

Das Gegenstück zu `raw2png.py`, das dasselbe für das Fenster tut. Ohne dieses
Skript war die Terminal-Oberfläche die eine Ansicht des Programms, die niemand
ansehen konnte, ohne sie von Hand zu starten — und genau deshalb ist ihr jahrelang
nicht aufgefallen, dass kein Schalter sie überhaupt erreicht.

Die Wiedergabe versteht nur, was diese Oberfläche benutzt: Cursor setzen,
Zeile und Bild löschen, Text schreiben. Farben werden verworfen — geprüft wird
Aufbau und Inhalt, nicht die Palette.
"""
import fcntl
import os
import re
import struct
import sys
import termios
import time

ROWS, COLS = 45, 150
TIMEOUT = 30.0


def capture(argv: list[str]) -> bytes:
    """Startet `argv` in einem Pseudo-Terminal und gibt alles zurück, was es schreibt."""
    pid, fd = os.forkpty()
    if pid == 0:
        os.execv(argv[0], argv)
    # Muss nach dem Fork geschehen: vorher gibt es das Terminal noch nicht.
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))

    data = bytearray()
    deadline = time.time() + TIMEOUT
    while time.time() < deadline:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break  # Das Kind hat das Terminal geschlossen.
        if not chunk:
            break
        data += chunk
    try:
        os.close(fd)
    except OSError:
        pass
    os.waitpid(pid, 0)
    return bytes(data)


def replay(raw: str) -> list[str]:
    """Spielt die Ausgabe auf ein Raster zurück und gibt dessen Zeilen aus."""
    grid = [[" "] * COLS for _ in range(ROWS)]
    row = col = 0
    i = 0
    while i < len(raw):
        ch = raw[i]
        if ch == "\x1b":
            m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z])", raw[i:])
            if m:
                params, cmd = m.group(1), m.group(2)
                nums = [int(x) for x in params.split(";") if x.isdigit()]
                if cmd == "H":
                    row = (nums[0] - 1) if nums else 0
                    col = (nums[1] - 1) if len(nums) > 1 else 0
                elif cmd == "J":
                    grid = [[" "] * COLS for _ in range(ROWS)]
                    row = col = 0
                elif cmd == "K":
                    for x in range(col, COLS):
                        grid[row][x] = " "
                i += m.end()
                continue
            # Fenstertitel und Ähnliches: bis zum Abschluss überspringen.
            m = re.match(r"\x1b\][^\x07\x1b]*(\x07|\x1b\\)", raw[i:])
            if m:
                i += m.end()
                continue
            i += 1
            continue
        if ch == "\n":
            row, col = row + 1, 0
        elif ch == "\r":
            col = 0
        elif ch in ("\x0e", "\x0f"):
            pass  # Zeichensatz-Umschaltung, für den Rahmen belanglos.
        else:
            if 0 <= row < ROWS and 0 <= col < COLS:
                grid[row][col] = ch
            col += 1
        i += 1

    lines = ["".join(r).rstrip() for r in grid]
    while lines and not lines[-1]:
        lines.pop()
    return lines


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    raw = capture(sys.argv[1:])
    if not raw:
        print("Das Programm hat nichts geschrieben.", file=sys.stderr)
        return 1

    # Zwei Zusicherungen, die man an der Textausgabe nicht sieht: dass der
    # Alternativbildschirm überhaupt betreten und dass er wieder verlassen
    # wurde. Bleibt das Verlassen aus, ist die Shell des Lesers hinterher
    # unbenutzbar — der schlimmste Fehler, den eine Terminal-Oberfläche machen
    # kann, und der einzige, den er nicht selbst beheben kann.
    entered = raw.find(b"\x1b[?1049h")
    left = raw.rfind(b"\x1b[?1049l")
    print("".join(l + "\n" for l in replay(raw.decode("utf-8", "replace"))), end="")
    print("─" * COLS)
    print(f"betreten: Byte {entered}   verlassen: Byte {left}   gesamt: {len(raw)} Bytes")
    if entered < 0:
        print("WARNUNG: Alternativbildschirm nie betreten — lief die Oberfläche?")
        return 1
    if left < entered:
        print("FEHLER: Alternativbildschirm nicht verlassen — das Terminal bleibt kaputt.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
