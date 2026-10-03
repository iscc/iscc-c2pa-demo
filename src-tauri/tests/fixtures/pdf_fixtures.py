"""Build the generated PDF fixtures: metadata variants, a scan without text and an Arabic document.

Run: uv run --with iscc-sdk pdf_fixtures.py   (rtl.pdf needs LibreOffice; set SOFFICE when
soffice is not on PATH)

The meta-*.pdf files are copies of c2pa-rs's basic-no-xmp.pdf with document information
entries and XMP packets that pin iscc-sdk's (Tika's) metadata rules: ISCC keys before the
standard ones, docinfo before XMP, blank values skipped, the first language alternative in
document order, creators of an rdf:Seq joined, and the text-string encodings. scan.pdf is one
page holding only a picture of text; rtl.pdf is rtl.fodt converted by LibreOffice. Run the
hooks on new files and regenerate expected_pdf.json afterwards.
"""

import os
import shutil
import subprocess
import time
from pathlib import Path

import iscc_sdk as idk
from PIL import Image, ImageDraw
from pypdf import PdfReader, PdfWriter
from pypdf.generic import ByteStringObject, DecodedStreamObject, NameObject

HERE = Path(__file__).parent
BASE = HERE / "basic-no-xmp.pdf"
PACKET = """<?xpacket begin="\ufeff" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">{body}</rdf:Description>
</rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"""
FIXED_DATE = time.strptime("2026-10-03", "%Y-%m-%d")


def alt(tag, items):
    """An rdf:Alt property of (language, text) items."""
    lis = "".join(f'<rdf:li xml:lang="{lang}">{text}</rdf:li>' for lang, text in items)
    return f"<dc:{tag}><rdf:Alt>{lis}</rdf:Alt></dc:{tag}>"


def seq(tag, items):
    """An rdf:Seq property."""
    lis = "".join(f"<rdf:li>{text}</rdf:li>" for text in items)
    return f"<dc:{tag}><rdf:Seq>{lis}</rdf:Seq></dc:{tag}>"


XMP_ALL = (
    alt("title", [("de", "Deutscher Titel"), ("x-default", "English title")])
    + alt("description", [("de", "Deutsche Beschreibung"), ("x-default", "English description")])
    + seq("creator", ["First Creator", "Second Creator"])
)


def write(name, source=BASE, xmp=None, info=None, raw=None):
    """Copy source to name with an XMP packet, docinfo text and raw docinfo byte strings."""
    writer = PdfWriter(clone_from=PdfReader(source))
    if xmp is not None:
        stream = DecodedStreamObject()
        stream.set_data(PACKET.format(body=xmp).encode("utf-8"))
        stream[NameObject("/Type")] = NameObject("/Metadata")
        stream[NameObject("/Subtype")] = NameObject("/XML")
        writer._root_object[NameObject("/Metadata")] = writer._add_object(stream)
    if info:
        writer.add_metadata(info)
    for key, value in (raw or {}).items():
        writer._info[NameObject(key)] = ByteStringObject(value)
    writer.write(HERE / name)


def meta_fixtures():
    """The meta-*.pdf files."""
    meta = idk.IsccMeta(
        name="ISCC <b>name</b>",
        description="ISCC description",
        meta="data:application/json;base64,eyJhIjoxfQ==",
    )
    embedded = Path(idk.pdf_meta_embed(BASE, meta))
    write("meta-iscc.pdf", source=embedded, info={"/Title": "Docinfo title", "/Subject": "Docinfo subject"})
    shutil.rmtree(embedded.parent)
    write("meta-xmp-only.pdf", xmp=XMP_ALL)
    write(
        "meta-conflict.pdf",
        xmp=XMP_ALL,
        info={"/Title": "Docinfo title", "/Subject": "Docinfo subject", "/Author": "Docinfo author"},
    )
    write("meta-blank-title.pdf", xmp=XMP_ALL, info={"/Title": "  ", "/Subject": " ", "/Author": " "})
    write(
        "meta-encodings.pdf",
        raw={
            "/Title": b"Caf\xe9 \x80 bullet \x8d quoted\x8e \x84 dash",
            "/Subject": b"\xfe\xff" + "Größe 日本語 UTF-16BE".encode("utf-16-be"),
            "/Author": b"\xff\xfe" + "Jürgen Müller".encode("utf-16-le"),
        },
    )
    write(
        "meta-utf8.pdf",
        raw={
            "/Title": b"\xef\xbb\xbf" + "Größe 日本語 UTF-8".encode(),
            "/Subject": b"\xef\xbb\xbf" + "Beschreibung – UTF-8".encode(),
        },
    )


def scan_fixture():
    """scan.pdf: one page that is a grey picture of a few lines of text."""
    img = Image.new("L", (850, 1100), 255)
    draw = ImageDraw.Draw(img)
    for i, line in enumerate(["A scanned page", "has only a picture of its text,", "no text layer."]):
        draw.text((80, 100 + i * 40), line, fill=0, font_size=28)
    img.save(
        HERE / "scan.pdf",
        resolution=100,
        quality=60,
        title="Scanned page",
        creationDate=FIXED_DATE,
        modDate=FIXED_DATE,
    )


def rtl_fixture():
    """rtl.pdf: rtl.fodt exported by LibreOffice."""
    soffice = os.environ.get("SOFFICE", "soffice")
    subprocess.run([soffice, "--headless", "--convert-to", "pdf", "--outdir", str(HERE), str(HERE / "rtl.fodt")], check=True)


def main():
    meta_fixtures()
    scan_fixture()
    rtl_fixture()
    for path in sorted(HERE.glob("*.pdf")):
        print(f"{path.name:24} {path.stat().st_size:>7} bytes")


if __name__ == "__main__":
    main()
