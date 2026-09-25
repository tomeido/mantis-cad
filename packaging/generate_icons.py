#!/usr/bin/env python3
"""Regenerate the small checked-in app icons from mantis-cad.svg.

Authoring only: install CairoSVG and Pillow to run this script. Building or
packaging MantisCAD uses the committed PNG/ICO and needs neither package.
"""

from io import BytesIO
from pathlib import Path
import struct

import cairosvg
from PIL import Image


ROOT = Path(__file__).resolve().parent
SIZES = (16, 24, 32, 48, 64, 128, 256)


def render(size):
    # Render each size independently so even the taskbar sizes stay crisp.
    data = cairosvg.svg2png(
        url=str(ROOT / "mantis-cad.svg"),
        output_width=size * 4,
        output_height=size * 4,
    )
    with Image.open(BytesIO(data)) as image:
        return image.convert("RGBA").resize((size, size), Image.Resampling.LANCZOS)


def main():
    entries = []
    for size in SIZES:
        icon = render(size)
        buffer = BytesIO()
        if size < 128:
            # DIB entries support Windows shell and classic installer dialogs.
            icon.save(buffer, format="ICO", sizes=[(size, size)], bitmap_format="bmp")
            data = buffer.getvalue()
            length, offset = struct.unpack_from("<II", data, 14)
            payload = data[offset:offset + length]
        else:
            # Compress the large entries to keep the executable resource small.
            icon.save(buffer, format="PNG", optimize=True)
            payload = buffer.getvalue()
        entries.append((size, payload))
        if size == 256:
            (ROOT / "mantis-cad.png").write_bytes(payload)

    offset = 6 + 16 * len(entries)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(entries)))
    for size, payload in entries:
        directory.extend(struct.pack(
            "<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(payload), offset,
        ))
        offset += len(payload)
    (ROOT / "mantis-cad.ico").write_bytes(
        directory + b"".join(payload for _, payload in entries)
    )


if __name__ == "__main__":
    main()
