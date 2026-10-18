"""Generate the JPEGs in jpeg/ that libjpeg-turbo-rs 0.8 cannot decode as libjpeg-turbo does.

Run: uv run --with pillow jpeg_fixtures.py

- jpeg/cmyk-420.jpg: CMYK with 4:2:0 subsampling; the decoder panics on 4 components whose
  sampling differs.
- jpeg/rgb-no-marker.jpg: solid red stored as RGB, told apart only by its component IDs R, G, B
  (no JFIF or Adobe marker); the decoder takes it for YCbCr.

They live in a folder of their own, so the reference scripts, which demand Pillow's pixels of
every JPEG in this folder, leave them alone.
"""

from pathlib import Path

from PIL import Image

OUT = Path(__file__).parent / "jpeg"


def without_app14(data):
    """The JPEG without its Adobe APP14 segments."""
    out, i = bytearray(data[:2]), 2
    while data[i + 1] != 0xDA:
        size = int.from_bytes(data[i + 2 : i + 4], "big")
        if data[i + 1] != 0xEE:
            out += data[i : i + 2 + size]
        i += 2 + size
    return bytes(out + data[i:])


def main():
    OUT.mkdir(exist_ok=True)
    gradient = Image.new("RGB", (17, 23))
    gradient.putdata([(x * 15, y * 11, 128) for y in range(23) for x in range(17)])
    gradient.convert("CMYK").save(OUT / "cmyk-420.jpg", quality=85, subsampling=2)
    rgb = OUT / "rgb-no-marker.jpg"
    Image.new("RGB", (16, 16), (255, 0, 0)).save(rgb, keep_rgb=True, subsampling=0, quality=95)
    rgb.write_bytes(without_app14(rgb.read_bytes()))


if __name__ == "__main__":
    main()
