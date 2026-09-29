#!/usr/bin/env python3
"""Build an Apple .icns from PNG files (icon_<size>.png) without iconutil.

Modern icns readers accept PNG payloads for the ic07..ic13 entries, which is
what iconutil produces too. Usage: icns.py PNG_DIR OUT.icns
"""
import os, struct, sys

# icns type → (pixel size, scale). @2x variants share the pixel size of the next size up.
TYPES = [
    (b"icp4", 16), (b"icp5", 32), (b"icp6", 64), (b"ic07", 128), (b"ic08", 256),
    (b"ic09", 512), (b"ic10", 1024), (b"ic11", 32), (b"ic12", 64), (b"ic13", 256), (b"ic14", 512),
]

def main(png_dir, out):
    entries = b""
    for typ, px in TYPES:
        path = os.path.join(png_dir, f"apex-{px}.png")
        if not os.path.exists(path):
            continue
        with open(path, "rb") as f:
            data = f.read()
        if data[:8] != b"\x89PNG\r\n\x1a\n":
            raise SystemExit(f"{path} is not a PNG")
        entries += typ + struct.pack(">I", 8 + len(data)) + data
    with open(out, "wb") as f:
        f.write(b"icns" + struct.pack(">I", 8 + len(entries)) + entries)

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
