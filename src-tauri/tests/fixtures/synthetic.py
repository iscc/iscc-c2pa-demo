"""Generate the synthetic RGB fixtures: gradients plus a deterministic pattern, with no uniform
border. They exercise the resampler at sizes where an approximate bicubic drifts from Pillow.

Run: uv run --with pillow --with numpy synthetic.py
"""

from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).parent
SIZES = [(64, 64), (123, 57), (640, 480)]


def main():
    rng = np.random.default_rng(7)
    for w, h in SIZES:
        y, x = np.mgrid[0:h, 0:w]
        r = x * 255 / (w - 1)
        g = y * 255 / (h - 1)
        b = rng.integers(0, 256, size=(h, w)) if w * h <= 10000 else (x * y // 97) % 256
        arr = np.stack([r, g, b], axis=-1).astype(np.uint8)
        Image.fromarray(arr, "RGB").save(HERE / f"synthetic-{w}x{h}.png")


if __name__ == "__main__":
    main()
