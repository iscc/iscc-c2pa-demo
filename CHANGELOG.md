# Changelog

## Unreleased

- OCR for scanned PDF pages (experimental, off by default): a page that is a scan, filled by one
  image with next to no text on it, has its text recognised, so a scanned document gets a
  Content-Code Text and a Semantic-Code Text. Every other page keeps the text iscc-sdk extracts,
  and with OCR off every PDF keeps iscc-sdk's codes. The PP-OCRv6 tiny models (PaddlePaddle,
  Apache-2.0) are built into the app, so nothing is downloaded; they read Latin script and
  Chinese. Settings switches OCR on and off, the unit list shows the recognition's progress and
  how many pages it read, and a scan without OCR points to Settings. The CLI reads scans with
  `--ocr`.

## 0.3.0 - 2026-10-05

- Semantic-Codes (experimental, off by default): images and documents can get a Semantic-Code
  Image or Semantic-Code Text, which matches what a picture shows or what a text says across
  crops, recolouring, translations and paraphrases. Compressed copies of the iscc-sci and
  iscc-sct models compute them on this computer, within a few bits of the codes iscc-sci and
  iscc-sct make. Settings, in the top bar, switches each kind on and off; switching one on
  downloads its model once (100 MB for images, 142 MB for text). Once a kind is on, the Sign
  tab embeds its Semantic-Code and the soft-binding card compares it. The CLI computes them with
  `--semantic` and installs the models with `c2pa-iscc tools install semantic-image`,
  `semantic-text` or `semantic`.
- A file shows at once, and the slow units follow, each with its own progress bar: a video's
  Content-Code, Data-Code and Instance-Code, and the Semantic-Code. Stop ends the analysis and
  Resume starts it again; the Sign button waits for it.

## 0.2.0 - 2026-10-04

- PDF: inspect and sign PDF files. Text and metadata come from pdfium, the library iscc-sdk
  uses, bundled with the app, and match iscc-sdk bit for bit. The first page is the preview and
  the claim thumbnail. Encrypted PDFs cannot be signed; for a digitally signed PDF the Sign tab
  warns that signing breaks that signature.
- Video: inspect and sign MP4, MOV, M4V and AVI files. The Content-Code Video and the Meta-Code
  match iscc-sdk bit for bit. They need ffmpeg, which the app offers to download the first time
  a video is opened: the build iscc-sdk uses, checked against its hash (about 70 MB on Windows
  and Linux, 25 MB on macOS, where Apple chips run it under Rosetta 2). Without ffmpeg a video
  still opens and signs with its Content Credentials, Data-Code and Instance-Code. Long videos
  show a progress bar with Cancel. The CLI installs ffmpeg with `c2pa-iscc tools install`.
- Audio: an MP4, MOV or M4V file with sound only is read like an M4A, with a Content-Code Audio
  and without ffmpeg. Opus tracks in MP4 get a Content-Code Audio, within a few bits of
  iscc-sdk's. A file whose audio cannot be decoded (AC-3, HE-AAC and others) opens and signs
  with its other units and says why it has no Content-Code Audio.
- A document without text, such as a scan without a text layer, gets no Content-Code Text: the
  code of empty text would match every other empty document. The reason shows in its place.
- The Sign tab records a digital source type only when one is chosen; it no longer claims
  "Digital capture" for every file. The list follows the current IPTC terms. The same holds for
  the CLI's `--source-type`.
- A manifest read from a sidecar file (`photo.c2pa` next to `photo.jpg`) is marked as such, and
  its soft binding is compared with the whole file.
- A file whose hash no longer matches its manifest never shows as "Source preserved".
- Large files are hashed in chunks instead of being read into memory whole.
- macOS 13 or later is required.

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
