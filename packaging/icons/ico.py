#!/usr/bin/env python3
"""Build a Windows .ico from PNG files. PNG-compressed entries are valid for
Vista and later. Usage: ico.py PNG_DIR OUT.ico"""
import os, struct, sys

SIZES = [16, 24, 32, 48, 64, 128, 256]

def main(png_dir, out):
    images = []
    for s in SIZES:
        path = os.path.join(png_dir, f"apex-{s}.png")
        if os.path.exists(path):
            with open(path, "rb") as f:
                images.append((s, f.read()))
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    directory = b""
    body = b""
    for s, data in images:
        dim = 0 if s >= 256 else s
        directory += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset + len(body))
        body += data
    with open(out, "wb") as f:
        f.write(header + directory + body)

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
