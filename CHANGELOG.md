# Changelog

## Unreleased

- "How was this file made?" in the Sign tab starts at "Not specified" and records no digital
  source type, instead of claiming "Digital capture (camera)" for every file. The CLI's
  `--source-type` has no default either. The list drops the terms IPTC retired (Digital art,
  Software rendered image) and adds Composite of captures and Composite with AI generated
  elements.
- PDF: inspect and sign PDF files. The text comes from pdfium, the library iscc-sdk uses, bundled
  with the app (build chromium/8076), and matches iscc-sdk bit for bit; title, description,
  creator and ISCC metadata follow iscc-sdk's rules for docinfo and XMP. The first page is the
  preview and the claim thumbnail. Signing rewrites the whole PDF, so a signed PDF is not
  source-preserving. Encrypted PDFs inspect but cannot be signed (c2pa-rs would remove the
  encryption or fail); for a digitally signed PDF the Sign tab warns that signing breaks that
  signature.
- A document without text (a scanned PDF without a text layer, an empty DOCX) gets no
  Content-Code Text, with the reason in its place: the code of empty text would match every other
  empty document.
- macOS 13 or later is required (the bundled pdfium library needs it).
- A manifest read from a sidecar file (`photo.c2pa` next to `photo.jpg`, which c2pa-rs loads when
  the file embeds none) is marked as such in the status card, and its ISCC soft binding is
  compared with the file itself. The app used to cut the sidecar's data hash exclusions out of
  the file: an exact copy of the source showed an Instance-Code of about 50%, and a file with
  other bytes in exactly those ranges showed as "Source preserved". A file signed into a sidecar
  (`c2patool --sidecar`) now shows as source-preserving in any format, M4A included.
- A hash binding that does not match the file rules out "Source preserved", even when the
  Instance-Codes agree.

## 0.1.1 - 2026-09-30

- Signing writes one thumbnail per manifest, made by the app: a JPEG of at most 256 px on its
  long edge, never enlarged, from the image, the SVG, the EPUB cover, an office file's saved
  thumbnail or audio cover art. SVG, EPUB, office and audio files had none before. The parent
  ingredient no longer gets a second copy. Signed files shrink accordingly: `no_manifest.jpg`
  (98 KB) grows by 10.5 KB instead of 98 KB, and a 3.7 KB PNG signs to 11 KB instead of
  853 KB.
- A "Visual Thumbnail Verification" card in the Content Credentials tab shows the thumbnail
  stored in the manifest next to the file, so a manifest can be checked by eye against the file
  it claims to describe. It works for any signer's claim thumbnail.

## 0.1.0 - 2026-09-28

First public release: installers for Windows, macOS and Linux.

- Inspect the C2PA manifest store of a file: validation state, signer and trust list, timestamp,
  actions, ingredients and every assertion. An invalid manifest always shows why, in plain
  language with the failure code, for example "The file changed after signing: its hash no
  longer matches."
- Compute the ISCC units of the file (Meta-Code, Content-Code, Data-Code, Instance-Code) with
  the rules of iscc-sdk, from the whole file.
- Verify an embedded ISCC soft binding as IEP-0020 specifies: compare each unit with the file
  and tell whether the file is source-preserving, that is, byte for byte the file that was
  signed once its Content Credentials are left out.
- Sign a copy with a C2PA manifest carrying an ISCC soft binding, a `cawg.metadata` assertion,
  an optional CAWG training and data mining assertion and an RFC 3161 timestamp. The manifest
  records the source as opened (`c2pa.opened`, parent ingredient, `allActionsIncluded`), with
  the chosen digital source type on the ingredient.
- Formats: JPEG, PNG, WebP, GIF, TIFF, SVG, EPUB, DOCX, PPTX, XLSX, ODT, ODS, ODP, TXT,
  Markdown, MP3, FLAC, WAV and M4A.
