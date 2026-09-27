"""Generate Meta-Code reference values for the image fixtures with iscc-sdk (exiv2 metadata).

Run: uv run --with iscc-sdk expected_meta.py

For every image in this folder it records the name, description, meta and creator that
iscc-sdk extracts, the file-name fallback title, and the 256-bit Meta-Code that `code_meta`
produces (embedded name, else file name). SVGs also get their 256-bit Content-Code Image,
which iscc-sdk computes from a resvg rendering (Pillow cannot open SVG, so expected_iscc.py
leaves them out). The meta-*.* fixtures come from meta_fixtures.sh.
"""

import json
from pathlib import Path

import iscc_sdk as idk

HERE = Path(__file__).parent
BITS = 256
IMAGE_SUFFIXES = {".jpg", ".jpeg", ".png", ".webp", ".gif", ".tif", ".tiff", ".svg"}


def main():
    out = {}
    for path in sorted(p for p in HERE.iterdir() if p.suffix.lower() in IMAGE_SUFFIXES):
        meta = idk.extract_metadata(path).dict()
        out[path.name] = {
            "name": meta.get("name"),
            "description": meta.get("description"),
            "meta": meta.get("meta"),
            "creator": meta.get("creator"),
            "fallback": idk.text_name_from_uri(path),
            "meta_code": idk.code_meta(path, bits=BITS).iscc,
        }
        if path.suffix.lower() == ".svg":
            out[path.name]["image"] = idk.code_image(path, bits=BITS).iscc
    (HERE / "expected_meta.json").write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(out, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
