"""Generate reference values for PDFs with iscc-sdk (pypdfium2 text, Tika metadata).

Run: uv run --with iscc-sdk --with pypdfium2==5.14.0b1 expected_pdf.py [DIR]

pypdfium2 5.14.0b1 bundles pdfium 156.0.8076.0, the build the app ships; iscc-sdk 0.9.5 itself
locks an older one. For every PDF in this folder, or below DIR, it records the title,
description, creator and ISCC metadata iscc-sdk extracts, the 256-bit Meta-Code (embedded
title, else the file name, as `code_meta` does), the Content-Code Text with the length of the
collapsed text (0 means no text: the app leaves the Content-Code out there), Data-Code and
Instance-Code. A PDF iscc-sdk cannot read gets `text_error` in place of the Content-Code.
Writes expected_pdf.json into the folder it read.
"""

import json
import sys
from pathlib import Path

import iscc_lib as il
import iscc_sdk as idk

HERE = Path(__file__).parent
BITS = 256


def meta_code(path, meta):
    """Meta-Code like `iscc_sdk.code_meta`: embedded name, else the file name."""
    name = meta.get("name") or ""
    name = il.text_trim(il.text_remove_newlines(il.text_clean(name)), il.core_opts.meta_trim_name)
    if not name:
        meta = {**meta, "name": idk.text_name_from_uri(path)}
    return il.gen_meta_code_v0(meta["name"], meta.get("description"), meta.get("meta"), BITS)["iscc"]


def text_fields(path):
    """Content-Code Text and collapsed length, or the reason iscc-sdk could not extract text."""
    try:
        text = il.text_clean(idk.pdf_text_extract(path))
    except Exception as e:  # noqa: BLE001 - any extraction failure is a reference value
        return {"text": None, "collapsed": None, "text_error": f"{type(e).__name__}: {e}"}
    return {"text": il.gen_text_code_v0(text, bits=BITS)["iscc"], "collapsed": len(il.text_collapse(text))}


def reference(path):
    """All reference values of one PDF."""
    data = path.read_bytes()
    meta = idk.text_meta_extract(path)
    return {
        "title": meta.get("name"),
        "description": meta.get("description"),
        "creator": meta.get("creator"),
        "iscc_meta": meta.get("meta"),
        "meta": meta_code(path, meta),
        **text_fields(path),
        "data": il.gen_data_code_v0(data, bits=BITS)["iscc"],
        "instance": il.gen_instance_code_v0(data, bits=BITS)["iscc"],
    }


def main():
    folder = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE
    out = {}
    for path in sorted(folder.rglob("*.pdf")):
        key = path.relative_to(folder).as_posix()
        out[key] = reference(path)
        print(key, out[key]["text"], flush=True)
    text = json.dumps(out, indent=2, ensure_ascii=False) + "\n"
    (folder / "expected_pdf.json").write_text(text, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
