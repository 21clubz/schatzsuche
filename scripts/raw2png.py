#!/usr/bin/env python3
"""Turns the raw dump from `--screenshot` into a PNG.

    python3 scripts/raw2png.py fenster.raw fenster.png

`--screenshot` writes raw pixels rather than a PNG on purpose: the encoder would
be one more thing to go wrong inside the program, and the point of that flag is
to photograph the window without a human dragging a corner. The cost is that the
result cannot be looked at until it has been through here.

This script existed only in scratch directories for weeks while the notes
claimed it was part of the project. It is committed now because every visual
change to the window has to be reviewed through it.

The format, as written by `GuiApp::save_screenshot`:

    u32 little-endian  width
    u32 little-endian  height
    width * height * 4 bytes, RGBA8, top row first

Deliberately no Pillow, unlike the other scripts here: the whole job is a zlib
call and three chunk headers, and a review tool that needs an install first is a
review tool that does not get used.
"""
import struct
import sys
import zlib


def chunk(kind: bytes, data: bytes) -> bytes:
    """One PNG chunk: length, type, payload, CRC over type and payload."""
    return (
        struct.pack(">I", len(data))
        + kind
        + data
        + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)
    )


def main(src: str, dst: str) -> None:
    with open(src, "rb") as f:
        blob = f.read()

    if len(blob) < 8:
        raise SystemExit(f"{src}: kürzer als der 8-Byte-Kopf — kein Rohbild")

    width, height = struct.unpack_from("<II", blob, 0)
    pixels = blob[8:]
    expected = width * height * 4
    if len(pixels) < expected:
        raise SystemExit(
            f"{src}: {width}x{height} braucht {expected} Byte, da sind {len(pixels)}"
        )

    # PNG wants a filter byte in front of every row; 0 means "no filter", which
    # zlib then compresses about as well as anything more clever would here.
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        raw += pixels[y * width * 4 : (y + 1) * width * 4]

    png = b"\x89PNG\r\n\x1a\n"
    # Bit depth 8, colour type 6 (RGBA), no interlacing.
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")

    with open(dst, "wb") as f:
        f.write(png)
    print(f"{dst}: {width}x{height}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("Aufruf: raw2png.py <bild.raw> <bild.png>")
    main(sys.argv[1], sys.argv[2])
