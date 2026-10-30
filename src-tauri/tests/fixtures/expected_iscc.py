"""Generate reference ISCC units for the fixture images with iscc-sdk and iscc-core.

Run: uv run --with "iscc-sdk>=0.9.7" --with "iscc-core>=1.5" expected_iscc.py

iscc-sdk's `image_normalize` prepares the 1024 grayscale samples (EXIF transpose, white fill,
the tolerant border trim of 0.9.7, luma, bicubic 32x32), iscc-core turns them into the code.
"""

import io
import json
from pathlib import Path

import iscc_core as ic
import iscc_sdk as idk
from PIL import Image

HERE = Path(__file__).parent
IMAGE_SUFFIXES = (".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff")


def main():
    out = {}
    for path in sorted(HERE.iterdir()):
        if path.suffix.lower() not in IMAGE_SUFFIXES:
            continue
        data = path.read_bytes()
        pixels = list(idk.image_normalize(Image.open(path)))
        out[path.name] = [
            ic.gen_image_code_v0(pixels, bits=256)["iscc"],
            ic.gen_data_code_v0(io.BytesIO(data), bits=256)["iscc"],
            ic.gen_instance_code_v0(io.BytesIO(data), bits=256)["iscc"],
        ]
    (HERE / "expected_iscc.json").write_text(json.dumps(out, indent=2) + "\n")
    print(json.dumps(out, indent=2))


if __name__ == "__main__":
    main()
