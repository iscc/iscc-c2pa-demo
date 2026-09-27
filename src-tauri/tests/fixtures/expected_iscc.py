"""Generate reference ISCC units for the fixture images with Pillow and iscc-core.

Run: uv run --with pillow --with iscc-core expected_iscc.py
"""

import io
import json
from pathlib import Path

import iscc_core as ic
from PIL import Image, ImageChops, ImageOps

HERE = Path(__file__).parent
IMAGE_SUFFIXES = (".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff")


def normalize(img):
    """Replicate iscc_sdk.image_normalize and return 1024 grayscale samples."""
    img = ImageOps.exif_transpose(img)
    if img.mode in ("RGBA", "LA") or (img.mode == "P" and "transparency" in img.info):
        rgba = img.convert("RGBA")
        bg = Image.new("RGB", img.size, (255, 255, 255))
        bg.paste(rgba, mask=rgba.getchannel("A"))
        img = bg
    else:
        img = img.convert("RGB")
    bg = Image.new(img.mode, img.size, img.getpixel((0, 0)))
    diff = ImageChops.difference(img, bg)
    diff = ImageChops.add(diff, diff)
    bbox = diff.getbbox()
    if bbox != (0, 0) + img.size:
        img = img.crop(bbox)
    img = img.convert("L").resize((32, 32), Image.BICUBIC)
    return list(img.get_flattened_data() if hasattr(img, "get_flattened_data") else img.getdata())


def main():
    out = {}
    for path in sorted(HERE.iterdir()):
        if path.suffix.lower() not in IMAGE_SUFFIXES:
            continue
        data = path.read_bytes()
        pixels = normalize(Image.open(path))
        out[path.name] = [
            ic.gen_image_code_v0(pixels, bits=256)["iscc"],
            ic.gen_data_code_v0(io.BytesIO(data), bits=256)["iscc"],
            ic.gen_instance_code_v0(io.BytesIO(data), bits=256)["iscc"],
        ]
    (HERE / "expected_iscc.json").write_text(json.dumps(out, indent=2) + "\n")
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
