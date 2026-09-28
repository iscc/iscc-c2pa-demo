# Changelog

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
