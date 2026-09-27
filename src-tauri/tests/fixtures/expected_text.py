"""Generate reference values for the text fixtures with iscc-sdk (Tika text extraction).

Run: uv run --with iscc-sdk expected_text.py [DUMP_DIR]

For every text document in this folder it records the title, description and creator that
iscc-sdk extracts, the 256-bit Meta-Code (embedded title, else the file name, as `code_meta`
does), and the Content-Code Text, Data-Code and Instance-Code. The Meta-Code is derived here
instead of calling `code_meta`, because iscc-sdk has no media type entry for ODS and ODP.
With DUMP_DIR, the cleaned text Tika extracted is also written there, one file per fixture,
for comparison with the Rust extractors.
"""

import json
import sys
from pathlib import Path

import iscc_lib as il
import iscc_sdk as idk

HERE = Path(__file__).parent
BITS = 256
TEXT_SUFFIXES = {".epub", ".docx", ".pptx", ".xlsx", ".odt", ".ods", ".odp", ".txt", ".md"}


def meta_code(path, meta):
    """Meta-Code like `iscc_sdk.code_meta`: embedded name, else the file name."""
    name = meta.get("name") or ""
    name = il.text_trim(il.text_remove_newlines(il.text_clean(name)), il.core_opts.meta_trim_name)
    if not name:
        meta = {**meta, "name": idk.text_name_from_uri(path)}
    return il.gen_meta_code_v0(meta["name"], meta.get("description"), meta.get("meta"), BITS)["iscc"]


def main():
    dump = Path(sys.argv[1]) if len(sys.argv) > 1 else None
    out = {}
    for path in sorted(p for p in HERE.iterdir() if p.suffix.lower() in TEXT_SUFFIXES):
        data = path.read_bytes()
        meta = idk.text_meta_extract(path)
        text = il.text_clean(idk.text_extract(path))
        if dump:
            dump.mkdir(parents=True, exist_ok=True)
            (dump / f"{path.name}.txt").write_text(text, encoding="utf-8")
        out[path.name] = {
            "title": meta.get("name"),
            "description": meta.get("description"),
            "creator": meta.get("creator"),
            "meta": meta_code(path, meta),
            "text": il.gen_text_code_v0(text, bits=BITS)["iscc"],
            "data": il.gen_data_code_v0(data, bits=BITS)["iscc"],
            "instance": il.gen_instance_code_v0(data, bits=BITS)["iscc"],
        }
    (HERE / "expected_text.json").write_text(json.dumps(out, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps(out, indent=2, ensure_ascii=False))


if __name__ == "__main__":
    main()
