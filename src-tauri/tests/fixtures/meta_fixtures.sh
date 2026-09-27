#!/usr/bin/env bash
# Build the meta-*.* fixtures: tiny images, each carrying one kind of embedded metadata, so the
# Rust port of iscc-sdk's metadata extraction can be checked field by field against
# expected_meta.json (see expected_meta.py).
#
# Run from this directory: bash meta_fixtures.sh   (needs exiftool on PATH and uv)
set -euo pipefail
cd "$(dirname "$0")"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

uv run --quiet --with pillow python - "$tmp" <<'EOF'
import sys
from PIL import Image
img = Image.new("RGB", (48, 32))
img.putdata([(x * 5, y * 8, (x + y) * 3) for y in range(32) for x in range(48)])
img.save(f"{sys.argv[1]}/base.jpg", quality=90)
img.save(f"{sys.argv[1]}/base.png")
# exiftool cannot write WebP (RIFF); Pillow embeds the XMP packet as a WebP XMP chunk.
xmp = """<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">WebP Title</rdf:li></rdf:Alt></dc:title>
<dc:description><rdf:Alt><rdf:li xml:lang="x-default">Described in XMP</rdf:li></rdf:Alt></dc:description>
</rdf:Description></rdf:RDF></x:xmpmeta>"""
img.save("meta-xmp.webp", quality=90, xmp=xmp.encode())
EOF

x() { rm -f "$2"; exiftool -q -o "$2" "${@:3}" "$1"; }
# Non-ASCII values go through UTF-8 files: exiftool on Windows misreads command-line encodings.
printf '%s' "IPTC Headline Zürich" > "$tmp/headline.txt"
printf '%s' "Nachtzug nach Zürich" > "$tmp/title.txt"

# XMP lang-alt title and a description with markup, an entity and a line break (sanitising).
x "$tmp/base.jpg" meta-xmp.jpg -XMP-dc:Title="Harbour at Dawn" \
  -XMP-dc:Description="Fishing boats <b>leaving</b> the harbour &amp; gulls
   overhead"
# IPTC only: Headline outranks ObjectName; UTF-8 declared through the coded character set.
x "$tmp/base.jpg" meta-iptc.jpg -IPTC:CodedCharacterSet=UTF8 -IPTC:ObjectName="Object Name" \
  "-IPTC:Headline<=$tmp/headline.txt"
# EXIF XPTitle only (a UCS-2 byte array).
x "$tmp/base.jpg" meta-exif.jpg -EXIF:XPTitle="Windows Title"
# PNG iTXt XMP: dc:title outranks photoshop:Headline.
x "$tmp/base.png" meta-xmp.png "-XMP-dc:Title<=$tmp/title.txt" -XMP-photoshop:Headline="Ignored Headline"
# XMP photoshop:Headline outranks IPTC ObjectName.
x "$tmp/base.jpg" meta-headline.jpg -XMP-photoshop:Headline="Photoshop Headline" -IPTC:ObjectName="Object Name"

# ISCC namespace (iscc:name, iscc:description, iscc:meta) as written by iscc-sdk itself.
cp "$tmp/base.jpg" "$tmp/iscc.jpg"
uv run --quiet --with iscc-sdk python - "$tmp/iscc.jpg" meta-iscc.jpg <<'EOF'
import shutil, sys
import iscc_sdk as idk
meta = idk.IsccMeta(
    name="ISCC Name",
    description="ISCC description",
    meta="data:application/json;base64,eyJrZXkiOiAidmFsdWUifQ==",
)
shutil.move(idk.image_meta_embed(sys.argv[1], meta), sys.argv[2])
EOF

ls -l meta-*.*
