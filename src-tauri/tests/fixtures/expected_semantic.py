"""Generate reference values for the Semantic-Codes with iscc-sci and iscc-sct (fp32 models).

Run: uv run --with "iscc-sci[cpu]==0.3.0" --with "iscc-sct[cpu]==0.2.2"
     --with semantic-text-splitter==0.33.0 --with blake3 expected_semantic.py DUMP_DIR

DUMP_DIR holds the texts this app extracts from the text fixtures, written by
`DUMP_TEXT_DIR=DUMP_DIR cargo test --lib dump_texts -- --ignored`: the Semantic-Code Text depends
on whitespace, and our extractors agree with Tika only after `text_collapse`.

For every raster image fixture it records iscc-sci's model input (a BLAKE3 hash of the
float32 NCHW tensor plus four samples, minimum and maximum) and its 256-bit code. For short
texts, synthetic chunking cases (from iscc-sct's `tests/test_chunking_vectors.py`) and the text
fixtures it records iscc-sct's chunks (code point offsets, chunk lengths, a BLAKE3 hash of the
chunks joined with U+001F) and, except for the long synthetic cases, the 256-bit code. The text
fixtures are those of `expected_text.json`. The app's
compressed models stay within a few bits of these codes; the tests allow 16 of 256.
"""

import json
import sys
from pathlib import Path

import blake3
import iscc_sci
import iscc_sct
import numpy as np
from iscc_sct.code_semantic_text import split_text
from PIL import Image

HERE = Path(__file__).parent
BITS = 256
IMAGE_SUFFIXES = {".jpg", ".png", ".gif", ".webp", ".tif"}
GRANULAR = "Try some very small and granular text splitting with Iñtërnâtiônàlizætiøn☃. Use options override for it."
SHORT = {
    "hello": ("Hello World", {}),
    "readme": ("This is some sample text. It can be a longer document or even an entire book.", {}),
    "tiny": ("Hello, World! Schöne Grüße aus München. 😀", {}),
    "unicode-mix": ("Café ‍naïve 😀🎉 سلام z̧álgo\n\nEnde.", {}),
    "granular": (GRANULAR, {"max_tokens": 8, "overlap": 4}),
}


def synthetic():
    """iscc-sct's synthetic chunking cases that need no corpus text, plus a CRLF variant."""
    level3 = ("Ein kurzer Absatz über die Dinge des Lebens. " * 5 + "\n\n") * 100 + "\n\nEnde."
    return {
        "cjk-pathological": "数据是新的石油它推动着现代经济的发展与变革。" * 500 + "\n\n完",
        "long-word-pathological": "hypermodularization" * 600 + " Ende\n\nEnde.",
        "unk-runs-pathological": ("𓀀" * 100 + " ") * 120 + "\n\nEnde.",
        "nbsp-pathological": chr(0xA0).join(["Inhalt"] * 4000) + "\n\nEnde.",
        "mixed-level-pathological": level3,
        "crlf": level3.replace("\n", "\r\n"),
        "whitespace-only": "  \t \n\n     ",
    }


def chunking(text, options):
    """iscc-sct's chunks of text: code point offsets, chunk lengths, hash of the joined chunks."""
    result = split_text(text, **options)
    chunks = [chunk for _, chunk in result]
    return {
        "offsets": " ".join(str(offset) for offset, _ in result),
        "sizes": " ".join(str(len(chunk)) for chunk in chunks),
        "chunks_blake3": blake3.blake3("\x1f".join(chunks).encode("utf-8")).hexdigest(),
    }


def text_code(text):
    """256-bit Semantic-Code Text with iscc-sct's default chunking."""
    return iscc_sct.gen_text_code_semantic(text, bits=BITS)["iscc"]


def image_entry(path):
    """iscc-sci's model input checkpoints and code for one image."""
    arr = iscc_sci.preprocess_image(Image.open(path))
    return {
        "code": iscc_sci.code_image_semantic(path, bits=BITS)["iscc"],
        "tensor_blake3": blake3.blake3(arr.astype("<f4").tobytes()).hexdigest(),
        "samples": [float(arr[0, 0, 0, 0]), float(arr[0, 1, 255, 255]), float(arr[0, 2, 511, 511]), float(arr[0, 0, 256, 0])],
        "min": float(arr.min()),
        "max": float(arr.max()),
    }


def main():
    dump = Path(sys.argv[1])
    images = {p.name: image_entry(p) for p in sorted(HERE.iterdir()) if p.suffix.lower() in IMAGE_SUFFIXES}
    texts = {name: {"text": text, "options": options, **chunking(text, options)} for name, (text, options) in SHORT.items()}
    for name, (text, _) in SHORT.items():
        texts[name]["code"] = text_code(text)
    generated = {}
    for name, text in synthetic().items():
        generated[name] = {"text_blake3": blake3.blake3(text.encode("utf-8")).hexdigest(), **chunking(text, {})}
    documents = {}
    for name in json.loads((HERE / "expected_text.json").read_text(encoding="utf-8")):
        # Bytes, not read_text: universal newlines would turn CRLF into LF.
        text = (dump / f"{name}.txt").read_bytes().decode("utf-8")
        documents[name] = {"code": text_code(text), **chunking(text, {})}
        print(name, len(text), "characters")
    out = {"images": images, "texts": texts, "generated": generated, "documents": documents}
    with open(HERE / "expected_semantic.json", "w", encoding="utf-8", newline="\n") as f:
        json.dump(out, f, indent=1, ensure_ascii=False)
        f.write("\n")
    print(len(images), "images,", len(texts), "texts,", len(generated), "generated,", len(documents), "documents")


if __name__ == "__main__":
    np.set_printoptions(precision=8)
    main()
