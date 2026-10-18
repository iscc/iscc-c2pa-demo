"""Generate reference pixels for a folder of JPEGs with Pillow (libjpeg-turbo).

Run: uv run --with pillow --with blake3 expected_jpeg.py DIR

For every JPEG in DIR it records the size and the BLAKE3 of the RGB pixels Pillow decodes
(`Image.convert("RGB")`, before any EXIF rotation), which the app's JPEG decoder must reproduce
byte for byte. A file Pillow cannot decode is left out. Writes expected_jpeg.json into DIR.
"""

import json
import sys
from pathlib import Path

import blake3
import PIL
from PIL import Image, features


def reference(path):
    """Width, height and pixel hash of one JPEG, or None when Pillow cannot decode it."""
    try:
        with Image.open(path) as im:
            if im.format != "JPEG":
                return None
            rgb = im.convert("RGB")
    except Exception:  # noqa: BLE001 - a file Pillow refuses has no reference
        return None
    return {"width": rgb.width, "height": rgb.height, "rgb_blake3": blake3.blake3(rgb.tobytes()).hexdigest()}


def main():
    folder = Path(sys.argv[1])
    out = {}
    for path in sorted(folder.iterdir()):
        if path.suffix.lower() in (".jpg", ".jpeg"):
            ref = reference(path)
            if ref:
                out[path.name] = ref
    meta = {"pillow": PIL.__version__, "libjpeg_turbo": features.version("libjpeg_turbo")}
    data = {"_decoder": meta, **out}
    (folder / "expected_jpeg.json").write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(out)} JPEGs, Pillow {meta['pillow']}, libjpeg-turbo {meta['libjpeg_turbo']}")


if __name__ == "__main__":
    main()
