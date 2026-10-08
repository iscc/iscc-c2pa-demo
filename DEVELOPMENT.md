# Developing the ISCC C2PA Demo

Everything behind the [README](README.md): what the app reads and writes, how it is built and
tested, and how a release is made.

## What the app does

Opening a file shows:

- the C2PA manifest store with validation state, signer, actions, ingredients and every assertion;
- the ISCC units of the file (Meta-Code, Semantic-Code Image or Text, Content-Code Image, Text,
  Audio or Video, Data-Code, Instance-Code), computed from the whole file as any ISCC tool computes
  them. Title, description, text, audio fingerprint and video signatures are extracted with the same
  rules as [iscc-sdk](https://github.com/iscc/iscc-sdk), and the test suite checks the units against
  iscc-sdk's output for every fixture. Documents of more than 500,000 characters are the exception:
  iscc-sdk extracts their text (PDF aside) through a Tika wrapper that silently stops there, while
  the app takes the whole text, so their Content-Code Text and Semantic-Code Text differ from
  iscc-sdk's. The experimental Semantic-Codes are off until switched on in Settings; they come
  from compressed copies of the iscc-sci and iscc-sct models and stay within a few bits of their
  codes (see [Semantic-Codes](#semantic-codes)). OCR of scanned PDF pages is off until switched
  on in Settings too; on, a scan gets a Content-Code Text from its recognised text, which iscc-sdk
  does not compute (see [OCR](#ocr)).
  Units that take long (a video's frames and hashes, the Semantic-Code, the text of scanned
  pages) are computed in the background after the file is shown, each with its own progress bar
  (see [Shown at once](#shown-at-once));
- the ISCC soft binding embedded in the manifest, decoded from its ISCC-SEQ value and compared
  unit by unit with the file, and whether the file is source-preserving (see
  [Notes on the soft binding](#notes-on-the-soft-binding));
- the claim thumbnail of the manifest next to the file's own picture, to compare by eye (see
  [Thumbnail](#thumbnail));
- the CAWG training and data mining assertion, if present.

The Sign tab writes a signed copy with:

- a `c2pa.soft-binding` assertion using algorithm `io.iscc.v0`, whose value is the ISCC-SEQ
  defined by [IEP-0020](https://ieps.iscc.codes/iep-0020/) (one or more 256-bit ISCC-UNITs,
  header and body concatenated);
- a `cawg.metadata` assertion with the title and description behind the Meta-Code, so a signed
  copy recomputes the Meta-Code it was signed with; a PDF also gets them written into its own
  metadata when they are not its own, with a `c2pa.edited.metadata` action;
- an optional `cawg.training-mining` assertion (CAWG Training and Data Mining Assertion 1.1);
- a `c2pa.opened` action with the source as parent ingredient, which carries the digital source
  type if one was chosen or, when the source already has Content Credentials, the existing
  manifest;
- a claim thumbnail when the file has a picture (see [Thumbnail](#thumbnail));
- an RFC 3161 timestamp from a time stamping authority, so the manifest proves when it was
  signed (see below).

## Stack

| Layer | What |
|---|---|
| Shell | [Tauri 2](https://tauri.app) (Rust core, system web view) |
| C2PA | [`c2pa`](https://crates.io/crates/c2pa) 0.91 with Rust native crypto, no OpenSSL |
| ISCC | [`iscc-lib`](https://crates.io/crates/iscc-lib) 0.6 plus a Pillow-equivalent image normalisation in `src-tauri/src/iscc.rs` |
| PDF | [pdfium](https://pdfium.googlesource.com/pdfium/) (bblanchon's prebuilt library, bundled with the app) through the raw C API of [`pdfium-render`](https://crates.io/crates/pdfium-render) for text and the first page; [`lopdf`](https://crates.io/crates/lopdf) for docinfo and XMP |
| EPUB | [`rbook`](https://crates.io/crates/rbook) for metadata, cover and reading order; text extracted with `quick-xml` |
| Office | [`zip`](https://crates.io/crates/zip) and `quick-xml`; text follows Tika's inclusion rules, Excel number formats rendered as Apache POI does |
| SVG | [`resvg`](https://crates.io/crates/resvg) 0.48 with the settings iscc-sdk uses through resvg_py |
| Audio | [`symphonia`](https://crates.io/crates/symphonia) 0.6 decodes, a resampler with fpcalc's settings in `src-tauri/src/resample.rs`, [`rusty-chromaprint`](https://crates.io/crates/rusty-chromaprint) fingerprints, [`lofty`](https://crates.io/crates/lofty) reads tags and cover art; [`rusty-opus`](https://crates.io/crates/rusty-opus) decodes Opus tracks of MP4 containers, registered as a symphonia decoder in `src-tauri/src/opus.rs`; no external tools |
| Video | ffmpeg 8.1, the [iscc-binaries](https://github.com/iscc/iscc-binaries) build iscc-sdk uses, downloaded on first use and run as a separate program for the MPEG-7 frame signatures, the tags and a preview frame (see [Video](#video)) |
| Semantic-Codes | [`rten`](https://crates.io/crates/rten) 0.27 runs weight-compressed copies of the iscc-sci and iscc-sct models, downloaded on first use; [`tokenizers`](https://crates.io/crates/tokenizers) 0.23 (pure Rust, no Oniguruma) and [`text-splitter`](https://crates.io/crates/text-splitter) 0.33 tokenize and chunk text as iscc-sct does (see [Semantic-Codes](#semantic-codes)) |
| OCR | PP-OCRv6 tiny (PaddlePaddle) in `rten`, compiled into the binary, with [`rten-imageproc`](https://crates.io/crates/rten-imageproc) for contours and a pipeline of our own in `src-tauri/src/ocr.rs` (see [OCR](#ocr)) |
| CLI | [`clap`](https://crates.io/crates/clap) |
| UI | Vite + TypeScript, no framework; ISCC brand tokens, Readex Pro and JetBrains Mono |
| Content Credentials pin | `src/assets/content_credentials_{icon,logo}.svg`, copied from [c2pa-conformance-tool](https://github.com/contentauth/c2pa-conformance-tool) (Apache 2.0). The icon and the name are C2PA trademarks; shown unmodified as the presence indicator per the [C2PA UX guidance](https://spec.c2pa.org/specifications/specifications/2.2/ux/UX_Recommendations.html) |

Trust lists compiled into the binary: the public C2PA trust list and TSA trust list from
`c2pa-org/conformance-public`, and the c2pa-rs test root bundle (sources and licences in
[`src-tauri/resources/certs/README.md`](src-tauri/resources/certs/README.md)) so files signed with the built-in
demo key validate as trusted inside the app. The demo key is the c2pa-rs ES256 test certificate;
other validators will report its signer as unknown. Use "My own certificate and key" in the Sign
tab to sign with a real credential.

## Build and run

Requirements: Rust 1.96+, Node 22.12+ (CI uses 24), pnpm 10, [uv](https://docs.astral.sh/uv/),
and the [Tauri prerequisites](https://tauri.app/start/prerequisites/) of your system: the WebView2
runtime on Windows (present on Windows 10/11), the Xcode command line tools on macOS, and
WebKitGTK 4.1 with its development packages on Linux (also needed for `cargo test`).

```sh
pnpm install
uv run scripts/fetch_resources.py # once after cloning, and when a pinned file changes
pnpm tauri dev                 # dev build with hot reload, Vite on port 43172
pnpm tauri dev -- -- /full/path/to/image.jpg  # open a file at startup (absolute path)
pnpm tauri build               # installers under src-tauri/target/release/bundle
```

**pdfium and the OCR models are build prerequisites.** `tauri.conf.json` bundles
`src-tauri/pdfium/*` with the app, and the Tauri build script fails on every cargo run (build,
test, clippy, rust-analyzer) when that directory is empty: "glob pattern pdfium/* path not
found"; `ocr.rs` compiles in the files of `src-tauri/ocr/`, and the build fails without them.
`scripts/fetch_resources.py`, the one place that pins both, downloads the pinned build of
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) for your system and the
PP-OCRv6 tiny models, checks their SHA-256 and puts them there; it does nothing when the right
files are present. The pre-push hooks, `mise run lint`, `mise run test`, CI and the release
workflow run it themselves. The app loads the library at run time from its
resource directory; the CLI looks next to its executable, and debug builds also in
`src-tauri/pdfium`.

## Command line

The CLI is not part of the installers yet. Build and run it from source with the `cli` feature:

```sh
cd src-tauri
cargo run --features cli --bin c2pa-iscc -- inspect ../photo.jpg --no-preview          # inspection as JSON
cargo run --features cli --bin c2pa-iscc -- sign ../report.docx --training cawg.ai_training=notAllowed
cargo run --features cli --bin c2pa-iscc -- meta-code --title "A title" --description "Some text"
cargo run --features cli --bin c2pa-iscc -- formats                                    # supported formats
cargo run --features cli --bin c2pa-iscc -- tools install                              # ffmpeg, for video
cargo run --features cli --bin c2pa-iscc -- tools install semantic                     # both semantic models (242 MB)
cargo run --features cli --bin c2pa-iscc -- inspect ../photo.jpg --semantic            # with the Semantic-Code Image
cargo run --features cli --bin c2pa-iscc -- inspect ../scan.pdf --ocr                  # a scan's text read by OCR
cargo run --features cli --bin c2pa-iscc -- sign --help                                # every option
```

The CLI prints the same JSON the desktop app works with (`--compact` for one line,
`--no-preview` to leave out the base64 preview image). `sign` fills in every option you leave
out the way the Sign tab prefills its form: the file's own title and description, every unit
it can compute, the built-in demo certificate, a timestamp from Encypher, and a `-signed` copy
next to the source. `--tsa URL` picks another timestamp service, `--no-timestamp` signs without network
access. Errors print one line on stderr and exit with code 1. A video's Meta-Code and
Content-Code need ffmpeg: without it, `inspect` and `sign` leave both out (`sign` keeps a
Meta-Code you ask for with `--title`) and a note says to run `c2pa-iscc tools install`, which
downloads it with a progress line on stderr. The experimental Semantic-Code of an image or a
document is left out unless `--semantic` asks for it (`sign --units semantic` does too), whatever
the app has switched on: the CLI never reads the app's settings, so its output does not depend on
them. It needs its kind's model: with `--semantic` and the model missing, `inspect` and `sign`
leave it out with a note to run `c2pa-iscc tools install semantic-image` (or `semantic-text`;
`semantic` installs both), and `--units semantic` is an error with the same hint. `tools status`
shows ffmpeg and each model (`semantic_image`, `semantic_text`) and where they are. The scanned
pages of a PDF are recognised only with `--ocr` (on `inspect` and `sign`), for the same reason;
the JSON's `ocr` says how many pages are scans and whether OCR read them. Like a video's
decoding, OCR prints no progress.

## Test

```sh
cd src-tauri
cargo test --tests --all-features   # --all-features includes the CLI tests
```

The tests compare every unit, title and description with reference values produced by the
Python ISCC tools, and sign and re-inspect a file of every format. The reference files live in
`src-tauri/tests/fixtures` with the scripts that generate them:

```sh
cd src-tauri/tests/fixtures
uv run --with pillow --with iscc-core expected_iscc.py   # raster images (Pillow + iscc-core)
uv run --with iscc-sdk expected_meta.py                  # image and SVG metadata (iscc-sdk)
uv run --with iscc-sdk expected_text.py                  # documents (iscc-sdk with Tika)
uv run --with iscc-sdk expected_audio.py                 # audio (iscc-sdk with fpcalc and TagLib)
uv run --with iscc-sdk --with pypdfium2==5.14.0b1 expected_pdf.py   # PDF (iscc-sdk, pdfium 8076)
uv run --with iscc-sdk expected_video.py                 # video (iscc-sdk with ffmpeg 8.1)
uv run --with "iscc-sci[cpu]==0.3.0" --with "iscc-sct[cpu]==0.2.2" --with semantic-text-splitter==0.33.0 --with blake3 expected_semantic.py DUMP_DIR   # Semantic-Codes
```

The video tests run ffmpeg and the Semantic-Code tests the semantic models; both are installed
on the first run (see [Video](#video) and [Semantic-Codes](#semantic-codes)).
`expected_semantic.py` reads the texts our extractors produce from `DUMP_DIR`, written by
`DUMP_TEXT_DIR=DUMP_DIR cargo test --lib dump_texts -- --ignored`.

`expected_pdf.py DIR` writes references for every PDF below a folder of your own, and
`PDF_CORPUS_DIR=DIR cargo test --lib pdf_corpus -- --ignored` compares the app with them. Run it
on a large set of PDFs before changing the pdfium build; `ocr_routing_corpus` (same variable)
lists the pages of that set OCR would read.

Two tests hold model results to a reference within a budget, because rten's kernels differ by
instruction set: the Semantic-Codes against iscc-sci and iscc-sct, and the OCR of
`scan-demo.pdf` against `expected_ocr.json` (rewrite it with `OCR_WRITE_REFERENCE=1`). Their
names contain `budget`; CI runs them in a step of their own with `--nocapture`, so the log shows
how far the runner drifted (`cargo test --tests --all-features budget -- --nocapture`).
`OCR_CORPUS_DIR=DIR cargo test --release --lib ocr_corpus -- --ignored --nocapture` measures OCR
quality and speed on the page images and truth codes of a quality set (see [OCR](#ocr)).

Sources and licences of the fixtures are listed in
[`src-tauri/tests/fixtures/README.md`](src-tauri/tests/fixtures/README.md).

## Timestamps

Signing asks a time stamping authority (TSA) to countersign the signature, so the manifest
proves that it existed at a given time and stays verifiable after the signing certificate
expires. Besides the one-time downloads of ffmpeg for video and of a semantic model when its
Semantic-Code is switched on, which the user starts, this is the only network access of the app: it sends a SHA-256 hash of the signature, nothing of the file. The default service is [Encypher](https://tsa.encypher.com), the only free
service found whose timestamps are on the C2PA TSA trust list; the Sign tab also offers DigiCert
and Sectigo (valid timestamps, but not on that list) and any other RFC 3161 URL. Timestamping is
best effort: when the service fails, does not answer within 10 seconds, or returns a timestamp
that C2PA would not accept (too large, or a certificate that breaks the C2PA rules), the file is
signed without a timestamp and the app says so. Inspecting never goes online.

The Content Credentials tab shows the timestamp as *Verified time* (service on the C2PA trust
list), *Unverified time* (valid, but the service is not on the list), *No timestamp*, or
*Timestamp rejected* (the token is broken or does not belong to the signature). Manifests with a v1 claim get
*Verified time* for any valid timestamp, because c2pa-rs does not check their service against
the trust list; the tab says so.

## Notes on the soft binding

The app follows [IEP-0020](https://ieps.iscc.codes/iep-0020/) in full.

**What is embedded.** Every unit is computed from the file handed to the signer exactly as it is,
including any Content Credentials it already carries. These are the units the left panel shows for
that file, and the ones iscc-sdk computes for it. No ISCC code path knows anything about C2PA.

One exception, so that the units describe the file as it is without its Content Credentials: a PDF
whose title, description or ISCC metadata at signing are not its own (or that has no title of its
own) gets them written into a revision of its own first, with iscc-sdk's keys (`/Title` and
`/iscc_name`, `/Subject` and `/iscc_description`, `/iscc_meta`; `pdf_update::with_metadata`). A
description cleared at signing is also taken out of the XMP packet (`dc:description`, the rest of
the packet byte for byte), because readers fall back to it once the document information has
none; the title and the ISCC metadata need no such step, as the document information wins. That
edited copy is then the source: every unit is computed from it, its Meta-Code from the fields read
back from it, and the manifest records the original as the `parentOf` ingredient with a
`c2pa.edited.metadata` action after `c2pa.opened`. A copy stripped of its Content Credentials
therefore computes every embedded unit, the Meta-Code included. Other formats still take the title
and description of the Sign tab into `cawg.metadata` only, until they get a metadata writer.

**Source preservation.** A signed file is *source-preserving* when its *source view* (the file
without the byte ranges its C2PA data hash excludes) is byte for byte the file that was signed. The
embedded Instance-Code proves it: the app computes the Instance-Code of the source view and
compares. The soft binding card shows one of three results:

| Result | When | Formats signed here (c2pa-rs 0.91) |
|---|---|---|
| Source preserved | Instance-Codes equal | JPEG, PNG, GIF, TXT, Markdown, PDF (appended update section), a re-signed PDF |
| Source not preserved | embedding changed other bytes, or the source already had Content Credentials | WebP, WAV and AVI (RIFF size field), TIFF (new image directory), SVG (namespace declaration), MP3 (ID3 tag rewritten), FLAC (ID3 tag in front); any other re-signed file; a PDF signed by c2pa-rs itself (rewritten as a whole) |
| Not verifiable | no data hash, no Instance-Code, a signature that does not validate or no longer covers the manifest as it is, or the file changed after signing | EPUB, DOCX, PPTX, XLSX, ODT, ODS, ODP (collection hash), M4A, MP4, MOV, M4V (BMFF hash) |

`source_preservation_per_format` in `src-tauri/src/sign.rs` pins the table.

**PDF source view.** The app signs a PDF itself (`src-tauri/src/pdf_update.rs`) instead of letting
c2pa-rs rewrite it: it appends the manifest store as an incremental update (ISO 32000-1, 7.5.6),
an unfiltered embedded file listed in the catalog's `/AF` and `/Names /EmbeddedFiles`, through
c2pa-rs's embeddable API (`Builder::placeholder`, `set_data_hash_exclusions`,
`update_hash_from_stream`, `sign_embeddable`, then the signed store patched into its place). Every
update section starts with a single line feed, whatever the source ends with. The exclusion ranges
of such a file still leave the appended catalog and cross-reference section in the view, so the
app takes a PDF source view in their place, drafted for IEP-0020: the update section begins with
the first object after the last `%%EOF` before the manifest store; when the byte before it is a
line feed and the update section adds nothing but the manifest store (`only_adds_manifest`), the
source view is every byte before that line feed. The app reads the update section's own
cross-reference sections for that, because lopdf's merged table hides free entries and resolves
objects by their headers: every entry must point into the update section at an object header
with its number, the catalog may change only in `/AF` and `/Names /EmbeddedFiles`, where every
entry but earlier manifest stores stays listed, the arrays and dictionaries on those paths keep
their numbers, and every other object the update defines (the file specification and the
embedded file among them) takes a number the source never used; no free entry for a number in
use, no object streams, no `/XRefStm`, no `/Encrypt` (an encrypted PDF has no PDF source view:
changing its encryption changes how every earlier object decodes), and the chain continues at
the source's own cross-reference section, as the parsed trailer says (`/Pr#65v` is `/Prev`, and
a `/Prev` inside a string is none). Without that, the signer's own update could show other pages than the source
it claims to preserve: a page redefined, a content stream replaced by the manifest stream under
its number, an entry pointing a page back at an earlier revision, a trailer skipping a revision,
or a dropped attachment were all found to pass a looser check (reviews, 2026-10-08). A PDF
signed by c2pa-rs (a rewrite) keeps the exclusion-range view and is not source-preserving.
Measured on 112 PDFs (fixtures and the 94-file corpus): every one that is not encrypted
validates, starts with its source byte for byte and recovers it; c2pa-python (c2pa-rs 0.91.0)
validates them too.

**What the embedded units are compared with.** When the file is source-preserving, every unit is
recomputed from the source view, which then is the original. Otherwise Meta-Code and Content-Code
are recomputed from the file itself, because the source view of a file whose embedding changed other
bytes need not decode (TIFF) or decodes wrongly (MP3). Data-Code and Instance-Code always come from
the source view where there is one, so the manifest store never counts as a change. When a file is
not source-preserving but its hash still matches, Data-Code and Instance-Code describe the file as
it was before signing; the card shows them in grey, with "Before signing" in place of "No match".
A hash that does not match the file always means "not verifiable", even when the Instance-Codes
agree.

**Sidecar manifests.** c2pa-rs's `Reader::with_file` loads `<stem>.c2pa` next to a file that embeds
no manifest store; the status card then says "Stored in: Sidecar file …" (`sidecar` in the
inspection). A sidecar is not in the file, so the file itself is its source view, whatever the
hard binding: a file signed into a sidecar (`c2patool --sidecar`) is source-preserving even as
M4A. A sidecar extracted from a copy that embeds the manifest carries data hash exclusions made
for that copy, which would cut real content from this file; the app ignores them, and c2pa-rs
rejects such a hash with `assertion.dataHash.mismatch` (c2pa-rs PR #2643). The `sidecar` tests
in `src-tauri/src/sign.rs` pin both cases.

**Inception.** Signing records the source as the `parentOf` ingredient and a `c2pa.opened`
action with `allActionsIncluded: true`, as the C2PA specification requires for a file that is opened
and saved with Content Credentials but otherwise unchanged. The digital source type chosen in the
Sign tab goes on that ingredient when the source has no Content Credentials of its own. The form
starts at "Not specified", which records none (the specification allows an ingredient without
one): a preselected type would make the signer assert how a file was made that the app cannot
know. The same holds for the CLI without `--source-type`.

The assertion also carries the optional `bindingMetadata` map of the C2PA specification (2.3
and later) with a description of the ISCC, the contact `info@iscc.io` and a link to IEP-0020, so
anyone reading the manifest learns how to interpret the value without consulting the soft binding
algorithm list. Validators ignore the map by spec; the Content Credentials tab shows it for any
soft-binding assertion that has one.

The assertion is built in `src-tauri/src/sign.rs` and verified in `src-tauri/src/inspect.rs`
(`source_view`, `preservation`, `summarize_assertion`); both go through `iscc::encode_seq` /
`iscc::decode_seq`.

## PDF

**Text and metadata as iscc-sdk extracts them.** iscc-sdk reads PDF text with pypdfium2: for every
page the text inside the page's bounding box (`FPDFText_GetBoundedText`), UTF-16 with lone
surrogates dropped, pages joined with a newline. `src-tauri/src/pdf.rs` makes the same pdfium
calls. Metadata comes from Tika in iscc-sdk, and from lopdf here with Tika's rules: the name from
the docinfo key `iscc_name`, then `/Title`, then XMP `dc:title`; the description from
`iscc_description`, `/Subject`, XMP `dc:description`; the creator from `/Author`, XMP
`dc:creator`; the ISCC metadata from `iscc_meta`. A blank value counts as missing. From XMP, Tika
takes the first language alternative of `dc:title` and `dc:description` in document order,
whatever its language, and joins the items of a `dc:creator` array; other forms yield nothing.
(Images follow exiv2, which prefers `x-default`.) Every fixture and a corpus of 94 PDFs match
iscc-sdk bit for bit, apart from the deviations below.

**The pdfium build is pinned, in lockstep with iscc-sdk.** pdfium's own updates change the
extracted text (chromium/8076 differs from 7999 on right-to-left Arabic), so the app and
iscc-sdk must use the same build. `PDFIUM_BUILD` in `scripts/fetch_resources.py` is the only place
that names it, with the SHA-256 of each archive; the reference script names the pypdfium2
version that bundles the same build. The app uses the raw C API through pdfium-render's
`pdfium_7881` bindings, the newest set that binds bblanchon's build (`pdfium_future` expects
XFA, V8 and Skia symbols). The macOS library needs macOS 13 or later, which is therefore the
app's minimum.

**Deviations from iscc-sdk 0.9.5.**

- A document whose text is empty once whitespace and punctuation are removed gets no
  Content-Code Text, in every text format: a scanned PDF without a text layer, an empty DOCX.
  iscc-sdk computes the code of the empty text for PDFs, so every scan would match every other
  scan 100%. The app names the reason in place of the code.
- Docinfo strings in UTF-8 (PDF 2.0) are decoded as UTF-8. Tika reads them as PDFDocEncoding,
  so iscc-sdk's title and Meta-Code for such a file are mojibake (`ï»¿JÃ¼rgen` for `Jürgen`).
  A string that does not decode at all counts as missing. Strings starting with `FF FE` are read
  as UTF-16LE, as Tika and pdfium do, although the PDF specification has no such encoding.
- A PDF lopdf cannot parse has no metadata (none of the 94 corpus files).
- With OCR switched on (off by default), the scanned pages of a PDF get their text from OCR, where
  iscc-sdk has none or only a stamp; see [OCR](#ocr).

**Signing appends to the PDF.** The manifest store comes in an update section of its own (see
[PDF source view](#notes-on-the-soft-binding)); incremental updates, linearisation, object streams
and digital signatures of the source stay as they are. Three cases the app guards against:

- An encrypted PDF cannot be signed: lopdf cannot add a revision to it (C2PA would want the
  manifest's stream left unencrypted through the `Identity` crypt filter). The Sign tab, `sign()`
  and the CLI refuse it with a plain reason (`Inspection.sign_block`). It still inspects; one that
  needs a password has no text and no metadata.
- A certified PDF cannot be signed either. A certifying signature (DocMDP) limits later changes;
  the C2PA specification allows no manifest at its levels 1 and 2, and at level 3 only through a
  `FileAttachment` annotation on a page, which this app does not write (c2pa-rs has a writer for
  it but never calls it). The annotation would change a page, which the PDF source view rules
  out, and whether it keeps a level 3 certification valid is untested: pyHanko's default diff
  policy has no rules for annotations and calls every such update `OTHER`. pdfium's
  `FPDFSignatureObj_GetDocMDPPermission` finds the certification; the block works like the
  encrypted one.
- An existing approval signature (`/ByteRange`) stays valid for what it signed, but PDF viewers
  report the appended revision as a change after signing (pyHanko: intact, modification level
  `OTHER`). The Sign tab shows a warning above the button and the CLI prints it as a `note:`
  (`Inspection.sign_warning`); the empty signature field of a blank form is not a signature. The
  signed copy still carries the signature, so the copy warns again.

## OCR

**Off by default, for parity with iscc-sdk.** iscc-sdk hashes only a PDF's text layer, so a scan
has no text to code (see the deviations above). With OCR switched on in Settings (or `--ocr` in
the CLI), the app recognises the text of the pages that are scans and puts it in place of their
native text, so a scanned document gets a Content-Code Text and a Semantic-Code Text that iscc-sdk
does not compute. Every other page keeps iscc-sdk's text, and a PDF without scanned pages keeps
iscc-sdk's codes whatever the switch says. Off, nothing is rendered for OCR and no model is loaded.
The switch is `ocr` in `settings.json`, off when missing. Nothing in the manifest marks an OCR
code; the unit list says how many pages OCR read.

**Which pages are scans.** A page is a scan when its largest image covers at least 90% of its
bounding box and its native text has fewer than 50 characters after `text_collapse`
(`pdf::is_scanned`); images inside form XObjects count, 15 levels deep, as pypdfium2's
`get_objects` finds them. The rule reads page objects only, so every PDF is routed at a glance.
Measured on the 94-file PDF corpus (844 pages): it picks only the first page of an image-only
Adobe Express design (`ocr_routing_corpus`). A scan with a stamped page number or Bates number
(a few native characters) is still a scan, and its stamp is not counted twice; a scan with an
invisible text layer (scanner software, Acrobat's "Recognize Text") keeps that layer, as in
iscc-sdk. A scan that does not fill its page (below 90%) is not recognised.

**Models, compiled in.** PP-OCRv6 tiny, PaddlePaddle's text detector (1.8 MB) and recogniser
(4.5 MB, 6,904 characters: Latin-script languages with their diacritics, English and Chinese; no
Japanese kana, Cyrillic, Greek, Arabic, Korean or Indic scripts), the official ONNX files from
Hugging Face (`PaddlePaddle/PP-OCRv6_tiny_det_onnx` and `_rec_onnx`). `scripts/fetch_resources.py`
pins them by commit and SHA-256 and writes them to `src-tauri/ocr/` (git-ignored) with the
dictionary taken from the recogniser's `inference.yml`, one entry per line. `src-tauri/src/ocr.rs`
compiles them into the app and the CLI with `include_bytes!` and runs them in rten, which the
Semantic-Codes use too. Each document loads them (tens of milliseconds) and drops them after.

**Pipeline** (`ocr.rs`). Pages render at 1984 px on the long side (about 180 dpi on A4 and
Letter, `/Rotate` applied). The detector's probability map, thresholded, gives a contour per
line, its minimum-area rectangle is scored and grown as PaddleOCR's DB post-processing does, with
RapidOCR's settings (mean and deviation 0.5, thresholds 0.3 and 0.5, growth ratio 1.6), which read
long documents a little better than the model's own `inference.yml`. The minimum-area rectangle is
our own (exact integer hull, rotating calipers): rten-imageproc's collapses on long thin lines, which
dropped whole lines of text. The reading order is a recursive XY-cut (column gutters first, then
horizontal gaps, then lines by their centre) of the boxes turned back by the page's skew, the median
angle of its long lines; the picture itself is never turned. Each line is sampled from the page into
a 48 px high input, one line per run: padding lines to a common width, as batches do, changes the
text. Greedy CTC decoding maps labels through the dictionary.

**Quality.** Measured with `ocr_corpus` on 11 documents of the PDF corpus, three pages each,
rendered clean (300 dpi) and as a rough office scan (200 dpi grey, 0.8 degrees, noise, blur, JPEG
60), each scored as the similarity of its Content-Code Text with the one of its text layer: the
five long documents (2,000 to 12,851 characters) reach 0.970 clean and 0.978 rough on average,
0.957 at least; unrelated documents score about 0.5. Short texts move more bits per misread
character, and maths becomes noise. `scan-demo.pdf` (the first page of `demo.pdf`, scanned)
reads at least 0.90 against its born-digital text, `scan.pdf` exactly.

**Speed.** Release build on a four-core Core i7-7700K: a dense page (two columns, about 85 lines)
takes 1.9 s on all cores, 5.9 s on one. Detection runs on all of a page's threads, and its lines on
as many one-thread workers, since rten spreads one line poorly over many threads. A 20-page scan
takes 16 s, 0.8 s a page: up to four pages are recognised at once (`pdf::MAX_PAGE_WORKERS`),
sharing out the cores. Detecting a page takes about 220 MB, so four at once peak at 1.2 GB; eight
took 2 GB and were no faster. Rendering a page takes about 0.1 s. In dev builds a page takes
several seconds, because the pixel loops of `ocr.rs` are not optimised there.

**Drift.** rten's results are the same on any number of threads, but its kernels depend on the
instruction set (AVX-512, AVX2, NEON or generic), so another CPU may now and then read a character
differently. The OCR Content-Code may differ by up to 16 of 256 bits between machines (Titusz,
2026-10-03); `scan_code_stays_within_the_budget_of_the_reference` holds `scan-demo.pdf` to
`expected_ocr.json`, made on the machine it names, and CI runs it on Windows, Linux and macOS
(NEON) with its drift report in the log.

**Background pass and memo.** At a glance, a PDF with scanned pages and OCR on has its
Content-Code (and Semantic-Code Text) pending (`AssetContent::Unrecognised`); the full pass
recognises the pages, reporting `content` progress per page, then embeds the text. Stop, Resume
and opening another file work as for a video. One thread renders the pages, holding pdfium's lock
for one page at a time so another file can be read meanwhile, and one worker per logical core
recognises them; a single page gets all cores (`parallel::ordered`, shared with the
Semantic-Code Text). The text of each page is remembered by the hash of its pixels for the
session: signing appends to a PDF and leaves its pages alone, so the source, the signed copy and
that copy reopened render to the same pixels and each page is recognised once.

**Switching it** with a scan open opens the file again, so its codes follow the switch at once.
A scan signed with OCR on and checked with OCR off (or by iscc-sdk) has no Content-Code to compare
with the embedded one: the soft-binding card says so and points to Settings.

## Video

**ffmpeg, installed on first use.** The Content-Code Video comes from MPEG-7 frame signatures,
which only ffmpeg's `signature` filter computes (GPL code, no Rust port), and iscc-sdk runs
ffmpeg for them. The app does the same with the very build iscc-sdk pins: ffmpeg 8.1 from the
[iscc-binaries](https://github.com/iscc/iscc-binaries) release v1.0.0 for Windows x64, Linux x64
and macOS (x86_64; Apple chips run it under Rosetta 2). It is never bundled. The first time a
video is opened, the app offers it (a file with sound only does not need it); once agreed, `src-tauri/src/tools.rs` downloads the zip, checks
its size and the BLAKE3 hash iscc-sdk pins, and extracts `ffmpeg-8.1[.exe]` into the app's local
data folder (`%LOCALAPPDATA%\codes.iscc.c2pa-demo\tools`, `~/Library/Application Support/...`,
`~/.local/share/...`), shared by the app and the CLI (`c2pa-iscc tools install`). A failed or
cancelled download leaves nothing behind. There is no `PATH` lookup: another build may lack the
signature filter or compute other codes. On a Mac with an Apple chip but without Rosetta 2,
ffmpeg cannot start and the app says how to install Rosetta. ffmpeg runs as a separate program,
which keeps the app Apache-2.0.

**Without ffmpeg** (declined, or on Linux and Windows on ARM, which have no build) a video is
only hashed (`asset::with_ffmpeg`, `AssetContent::Unread`): it inspects and signs with its
Content Credentials, Data-Code and Instance-Code, and both the Meta-Code and the Content-Code
give `tools::Missing::reason` in their place. The Meta-Code goes too because ffmpeg reads the
tags: one from the fallbacks (manifest title, file name) would differ from the file's own. The
Sign tab starts with it unticked; a title typed in still makes one. An unread video is not kept
for reuse, so it is decoded once ffmpeg is there.

**Four ffmpeg runs per video** (`src-tauri/src/video.rs`), with the arguments iscc-sdk uses. Each
reads the file with the demuxer of its format (`-f mov` for MP4, MOV and M4V, `-f avi` for AVI),
never one ffmpeg guesses from the content: read as a DASH or HLS playlist, a crafted `.mp4` makes
ffmpeg open any `file:` path it names, a network share on Windows included (`a_crafted_playlist_is_not_followed`).

- The ffmetadata dump (`-f ffmetadata -`) gives the tags; its log gives the duration, the streams
  and the frame size, rotated by the display matrix. In a file with several video streams the
  size is that of the stream ffmpeg decodes when left to choose, as the thumbnail and signature
  runs leave it:
  the default stream, else the largest picture, cover art last. iscc-sdk also runs ffprobe, but
  its only Meta-Code input is the global title, which the ffmetadata dump prints too and which
  overrides ffprobe's anyway; `expected_video.py` checks that on every fixture. Skipping ffprobe
  halves the download.
- The `thumbnail` filter picks the preview frame, scaled to fit 640 px first so a large video
  does not hold 100 full frames. Display only.
- The signature: `fps=fps=5,signature=format=binary`, written into a temporary folder ffmpeg runs
  in (so no path needs filter escaping), parsed like iscc-sdk's `read_mp7_signature`. Audio,
  subtitle and data streams are not decoded (`-an -sn -dn`), which leaves the video filter chain
  as it is and saves time. ffmpeg rotates by the display matrix before filtering, so a portrait
  phone video is fingerprinted upright, as in iscc-sdk. `-progress pipe:1` drives the progress
  bar of the Content-Code row; Stop kills ffmpeg. Signing can be cancelled while the source is
  fingerprinted; once the signed copy is being written, the card offers no Cancel, and a signing
  stopped before that point leaves no output.
- The stream fingerprint (see "Decoded once" below), alongside the signature pass and stopped
  with it.

Tags map through iscc-sdk's `VIDEO_META_MAP`, first filled key per field: name from `iscc_name`,
`title`, `track`, `show`, `album`; description from `iscc_description`, `description`,
`synopsis`, `comment`; ISCC metadata from `iscc_meta`; creator from `author`, `composer`,
`artist`, `album_artist`. Values are not sanitised, as iscc-sdk leaves video tags alone. Every
fixture's Content-Code Video, Meta-Code, Data-Code and Instance-Code equal iscc-sdk's
(`video_units_and_metadata_match_iscc_sdk_reference`).

**Deviations from iscc-sdk 0.9.5**, all where it fails or misreads:

- Only the global section of the ffmetadata dump counts. iscc-sdk fails on the `[CHAPTER]`
  sections of a file with chapters.
- A value that spans lines is read whole, and escapes are undone in one pass. iscc-sdk fails on
  the second line, or takes it as a key, and turns a literal `\n` into a line break.
- An MP4, MOV or M4V file with sound and no video is audio. iscc-sdk's `code_iscc` takes every
  MP4 container for a video by its file signature, an M4A too, and raises "No video stream
  detected". The app reads the track list from the movie box (`audio::sound_only`, called by
  `inspect::asset_format`): a sound track and no video track make the file "MP4 audio", read like
  an M4A, with a Content-Code Audio and without ffmpeg. Its MIME type stays, so c2pa signs it as
  the video format it is. iscc-sdk's audio functions give the same Content-Code Audio and tags
  (`no-video.mp4` and `no-video.mov` in `expected_audio.json`; TagLib, like lofty, reads no title
  from the QuickTime user data ffmpeg writes into a MOV). A file with neither sound nor video, and
  an AVI with sound only (the audio reader opens no AVI), still has no Content-Code, with the
  reason in its place.
- Audio the app cannot decode costs only the Content-Code Audio, in an M4A as in a sound-only
  MP4, MOV or M4V: the file inspects and signs with its other units, and the reason stands in
  the code's place (`audio::Undecodable`). That holds for AC-3, E-AC-3, HE-AAC, AAC with more
  than two channels, and track types symphonia's MP4 reader does not know (ALAC or AC-3 in a
  MOV, PCM in an MP4); fpcalc decodes them all through FFmpeg. A file whose container does not
  read stays an error.
- Opus in an MP4 container is decoded by rusty-opus, not libopus (`src-tauri/src/opus.rs`, a
  decoder registered in symphonia's codec registry; a further codec would join the same way).
  The Content-Code Audio may differ from iscc-sdk's in a few bits: 1 of 256 on
  `no-video-opus.mp4`, and the reference test accepts up to 7% (Titusz, 2026-10-04). The header's
  output gain is not applied, and the pre-skip comes from the MP4 edit list.

**Large files.** A video is never read into memory: ffmpeg reads the file, and Data-Code and
Instance-Code are hashed from it in 1 MiB chunks in a second thread meanwhile (`iscc::stream_units`,
`asset::load`). The source view of an embedded manifest is streamed the same way for every
format (`inspect::ViewReader`). ffmpeg's decoding dominates the time; the hashing runs alongside
and adds little.

**Decoded once.** ffmpeg has to decode every frame to pick five per second, so a decode cannot
be made cheaper without changing the code. Signing therefore decodes nothing it has decoded
before:

- The source. The app keeps the analysis of the video it decoded last (`asset::load`). Signing
  hashes the source anyway for its Data-Code and Instance-Code; when the Instance-Code equals the
  one of the kept analysis, the bytes are the same and the analysis is reused. Path, size and
  modification time only decide whether to try. The CLI runs one command per process and always
  decodes the source.
- The signed copy. c2pa-rs adds its manifest without touching the compressed video: in all four
  formats the copy carries the same packets as the source. `video::stream_fingerprint` shows that
  without decoding: a BLAKE3 hash of ffmpeg's `framemd5` listing of a stream copy (every video
  packet's hash, size and timestamps, plus the codec setup) and of the video streams as ffmpeg
  describes them (rotation included, bitrates left out, as ffmpeg estimates them from the file
  size in AVI). The source's fingerprint is taken while it is analysed and kept with its
  signatures (`Video.fingerprint`); when the copy's equals it, the copy decodes to the frames
  analysed and takes the source's signatures and preview; only its tags are read
  (`video::read_copy`). Any difference decodes the copy in full. Comparing with the analysis, not
  with the source file as it is at signing, keeps a source replaced in between from lending the
  copy frames it does not carry (`a_video_replaced_after_its_analysis_lends_its_copy_no_frames`).
  `rotated.mp4` has the packets of `demo.mp4` and another Content-Code, which the fingerprint
  tells apart. Fingerprinting reads the file at disk speed, a few percent of the time a decode
  takes. The UI says when a Content-Code was taken over (`Inspection.content_from_source`);
  opening the copy again decodes it.

What remains of signing a long video is c2pa-rs writing the copy and the hashing.

**Signing.** c2pa-rs signs MP4, MOV and M4V with a BMFF hash, so like M4A they have no source view;
AVI gets a data hash, and embedding rewrites its RIFF size field, so a signed AVI is not
source-preserving. MKV, WebM, MPEG-TS and OGV stay out: c2pa-rs has no handler for them.

**Tests need ffmpeg.** The video tests install it into the tools folder when it is missing
(`tools::tests::ensure_ffmpeg`, `c2pa-iscc tools install` in the CLI test), so the first test run
on a machine downloads it; they never skip. CI caches the folder per OS and installs Rosetta 2
on the arm64 macOS runner.

## Shown at once

Opening a file shows it at once and computes the slow units afterwards, in the background
(`inspect::Depth`). The first inspection is a glance (`Depth::GLANCE`): everything but a video's
frames and hashes, the Semantic-Code and, with OCR on, the text of a PDF's scanned pages. A video at a glance takes two short ffmpeg runs, the
ffmetadata dump and the preview frame (`video::glance`), so its tags, Meta-Code, duration, frame
size and preview show right away; its Content-Code, Data-Code and Instance-Code are pending
(`Inspection.pending`), and so is the preservation verdict of a signed AVI, which needs the hash of
its source view. Its Content Credentials are read but not checked yet (validation state `Pending`,
shown as "Checking…"): c2pa-rs reads and hashes the whole file to check the hard binding, so that
check runs once, in the second pass. The second inspection (`Depth::FULL`) computes everything and
replaces the first; it reports the progress of each slow unit by name (`content` while a video
decodes or scanned pages are recognised, `semantic` while a text is embedded), shown in that unit's
row. Data-Code and Instance-Code
have no share to report and show a moving bar.

The background pass stops when another file is opened or the file is closed; Stop in the unit list
ends it and Resume starts it again, as after a failed pass. The Sign button waits for it, because
signing needs the units, and a video signed afterwards is not decoded again (see "Decoded once" in
[Video](#video)). The full pass repeats the cheap part of the glance (the manifest, the tags, an
image's decoding, a document's text extraction). A signed copy is inspected in full, except a
Semantic-Code that was left out at signing, which the background pass computes as for any file just
opened. While both kinds of Semantic-Code and OCR are off (the default), images and documents have
nothing pending and get no background pass. The CLI always inspects in full.

## Semantic-Codes

The Semantic-Code Image and the Semantic-Code Text are experimental ISCC units: the sign bits of
a neural embedding, so they match what a picture shows or what a text says, across crops,
recolouring and overlays, translations and paraphrases. iscc-sci and iscc-sct compute them with
onnxruntime; the app runs the same models in [rten](https://github.com/robertknight/rten), a pure
Rust engine that gives the same bits as onnxruntime on the original fp32 models (all 16 image and
20 text test inputs). rten appears only in `src-tauri/src/semantic/`.

**Models, downloaded on first use.** The fp32 originals weigh 684 MB. The app uses
weight-compressed copies, published as release
[`models-v1`](https://github.com/iscc/iscc-c2pa-demo/releases/tag/models-v1) of this repository:
`iscc-sci-v0.1.0-w16.onnx` (weights as fp16) and `iscc-sct-v0.1.0-emb8-w16.onnx` (word
embeddings as int8 with a scale per row, other weights as fp16), with iscc-sct's
`tokenizer.json`, 242 MB together. All maths stays fp32. `scripts/compress_models.py` rebuilds
them from the originals. `tools.rs` downloads them like ffmpeg, as plain files checked by size and
BLAKE3, into the same tools folder, per kind (`tools::semantic_files`): the Semantic-Code Image
needs only the image model (100 MB), the Semantic-Code Text the text model and the tokenizer
(142 MB). In the CLI, `c2pa-iscc tools install semantic-image`, `semantic-text`, or `semantic`
for both. A changed model gets a new release (`models-v2`); published files are never replaced.

**Off by default, switched per kind in Settings.** A fresh install computes, offers and
downloads no Semantic-Code. The Settings dialog (top bar) has one box per kind; ticking it agrees
to the download of its model, which runs in the dialog with its progress and Cancel, and the
switch flips only once the model is there. Unticking keeps the model, so switching on again is
instant. One change runs at a time, and while it does the top bar's Open file, Close and Settings
and drag and drop wait: the app stops tasks through one generation counter (`cancel_tasks`), so
anything that cancels would stop the download too. The backend keeps the switches
(`settings.rs`) in `settings.json` next to the tools folder (`%LOCALAPPDATA%\codes.iscc.c2pa-demo`,
`~/Library/Application Support/...`, `~/.local/share/...`), so deleting that folder resets models
and switches together; a missing or unreadable file means both off. The app computes a kind only
when it is switched on and its model installed (`SemanticKinds::installed`), so a model deleted by
hand reads as off. `inspect::Depth::semantic_kinds` carries that set, and an inspection treats a
kind left out like no Semantic-Code at all: no unit, nothing pending, no reason, and an embedded
one is not compared. The views go further and hide what an inspection made before the switch-off
(`semanticShown` in `main.ts`), so switching off recomputes nothing. Signing refuses a
Semantic-Code of a kind that is off before it computes or writes anything. A file that carries a
Semantic-Code of a kind that is off says "not compared" for it, with a link to Settings.

**Drift.** Measured on 2026-10-04 against the fp32 models: at most 1 of 256 bits on the image test
set and 2 on the text set (AVX-512); on the fixtures 0 to 3 bits on x86_64 with AVX2 and AVX-512.
The smaller variants (`w8mix`, `w8`, 56 and 119 MB) drift up to 13 and 4 bits at the same speed,
which is why the larger ones were chosen (Titusz, 2026-10-04). rten's kernels depend on the
instruction set, so codes may differ by a bit or so between machines; the tests allow 16 of 256
bits, and CI checks that on Windows, Linux and macOS (NEON).

**Image.** As iscc-sci 0.3.0: the decoded picture (EXIF-rotated, transparency on white, the same
`AssetContent::Image` the Content-Code uses), its uniform border trimmed, squashed to 512x512 with
Pillow's bilinear filter (`iscc::resize_pillow`, bit for bit), each sample `(x / 255 - 0.5) / 0.5`
in f32. The model input equals iscc-sci's exactly for every PNG, GIF, TIFF and WebP fixture.
JPEGs decode a little differently from Pillow's libjpeg-turbo; photos still come out 0 to 3 bits
apart, but the `meta-*.jpg` fixtures, a 48x32 colour gradient without content, come out 58 bits
apart, and the tests leave them out. SVGs get the unit of their rendered picture; covers, cover
art and video frames get none.

**Text.** As iscc-sct 0.2.2: the cleaned text (whitespace kept, because it moves chunk
boundaries), split into chunks of at most 127 tokens overlapping by 48, with text-splitter 0.33,
the crate behind iscc-sct's `semantic-text-splitter`, kept in lockstep with the version iscc-sct
resolves. Chunk sizes are counted with `tokenizer.json` without truncation, and for text with
long spans between line breaks with iscc-sct's guarded count (`semantic::TokenSizer`). Each chunk
is tokenized as the file declares (truncated to 128 tokens), embedded, mean-pooled and
normalised; the document vector is the mean of the chunk vectors. The chunks equal iscc-sct's for
every test text, the long synthetic cases of its own test suite included.

The chunks are embedded in parallel, one per logical core and each on a single thread, while the
splitter is still cutting the text: rten spreads a single chunk of 128 tokens poorly over many
cores. Each chunk embeds exactly as it does on its own, and the mean adds the chunk vectors in text
order, so the code is the same on any number of cores (test
`the_number_of_workers_changes_nothing`). Measured on 2026-10-04 with the release CLI on a
603,000-character book (1,741 chunks): 4.3 s on a 16-core Ryzen AI Max+ 395 (46 s on one of its
cores) and 63 s on a four-core Core i7-7700K, where one chunk after the other took about 95 s;
iscc-sct takes 11 s for it on the 16 cores. Cutting the text into chunks alone takes 1.3 s and 9 s
on these two.

**Memo.** A session keeps the units it computed, keyed by a BLAKE3 hash of the model input (64 at
most): a file, its source view, its signed copy and that copy reopened usually decode to the same
pixels or text and are embedded once. Each computation loads its model and drops it after, so the
app holds no model while it idles.

**Build profiles.** The app's release build optimises for size; rten's kernels then run at half
speed, so `Cargo.toml` builds the rten crates with `opt-level = 3`. Dev builds compile every
dependency at `opt-level = 3` without debug assertions and overflow checks; with them the
Semantic-Code Text took 1.7 to 2.6 times as long.

## Thumbnail

Each signed file carries one thumbnail, the claim thumbnail (`c2pa.thumbnail.claim`), made by the
app from the same picture the Content-Code or the preview is computed from: the image itself
(EXIF-rotated, transparency flattened on white), the rendered SVG, a PDF's first page, the EPUB
cover, the thumbnail an office file was saved with, the cover art of an audio file, or the frame
of a video that ffmpeg's `thumbnail` filter picks. It is a JPEG at quality 75,
scaled down to 256 px on its long edge and never enlarged, without metadata. Files without a
picture (TXT, Markdown, audio without cover art, office files saved without a thumbnail, EPUBs
without a cover) get none. Its purpose is visual verification: when a file has lost its
manifest and the manifest is found again through its soft binding, the thumbnail shows whether
it belongs to the file at hand.

The parent ingredient gets no thumbnail of its own. The demo signs the file unchanged, so it
would be a second copy of the claim thumbnail. This deviates from the C2PA specification, which
says a thumbnail should be generated for an ingredient that has none. A source that already has
Content Credentials keeps the reference to its own claim thumbnail.

c2pa-rs's own thumbnails are off (`add_thumbnails` feature not enabled, `builder.thumbnail` set
to disabled): they measure 1024 px, enlarge small images and keep lossless formats lossless, so
a 3.7 KB PNG became an 853 KB signed file with the same 422 KB thumbnail twice. The thumbnail is
made in `src-tauri/src/thumbnail.rs`, which also makes the preview.

The Content Credentials tab shows the active manifest's claim thumbnail, whoever signed it, next
to the picture of the file as it is now (`ManifestSummary.thumbnail`, a data URL of any `image/*`
type). There is no similarity figure for the pair: the soft binding table already holds the
computed comparison, and the thumbnail is for the eye. A manifest without a claim thumbnail
says so when the file has a picture; for files without one the card is left out.

## Release

Releases are built by GitHub Actions. To publish version `X.Y.Z`:

1. Set `version` in `src-tauri/Cargo.toml` (the only place that holds the app version) and run
   `cargo check` so `Cargo.lock` follows.
2. Add a `## X.Y.Z - YYYY-MM-DD` section to [CHANGELOG.md](CHANGELOG.md); it becomes the release
   notes.
3. Commit, then tag and push: `git tag vX.Y.Z && git push origin main vX.Y.Z`.

The `release` workflow checks the tag against the version, builds the Windows installer, the
universal macOS disk image and the Linux AppImage, `.deb` and `.rpm`, attaches them with a
`SHA256SUMS` file to a GitHub release, and rebuilds the landing page so its download links point
at the new files. A tag with a hyphen (`v0.2.0-rc.1`) makes a prerelease, which the landing page
skips. The builds are not code-signed.

The landing page lives in `site/`. `node site/build.mjs` writes it to `_site/` with the links of
the latest release; the `pages` workflow deploys it to https://c2pa-demo.iscc.codes.

`site/install-mac.sh` is published with the page and is the recommended way to install on macOS:
`curl -fsSL https://c2pa-demo.iscc.codes/install-mac.sh | bash` downloads the disk image of the
latest release, checks it against `SHA256SUMS`, copies the app to `/Applications` and opens it.
The app is not notarized, so a disk image downloaded in a browser carries the quarantine flag and
Gatekeeper refuses it at the first start; curl sets no such flag. The arm64 slice carries the
linker's ad-hoc signature, which Apple silicon needs to run it at all.

`scripts/shot.ps1` drives and screenshots the running app on Windows, for checking the UI by eye.

## Licences

The code is Apache-2.0, see [LICENSE](LICENSE). The installers bundle the pdfium library from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (pdfium under BSD-3-Clause
and Apache-2.0, the build scripts under MIT); its licence files, those of the third-party code
compiled into it included, sit next to it as `LICENSE-*`. ffmpeg is not bundled: the app
downloads it on request from iscc-binaries, a GPL-2.0-or-later build, and runs it as a separate
program. The semantic models are not bundled either: the ISC21 image descriptor (MIT, exported
through Towhee, Apache-2.0) as iscc-sci packages it (Apache-2.0), and
`paraphrase-multilingual-MiniLM-L12-v2` with its tokenizer (Apache-2.0) as iscc-sct packages it
(Apache-2.0); the `models-v1` release names the sources. The app and the CLI carry the PP-OCRv6
tiny text detection and recognition models of PaddlePaddle (Apache-2.0), compiled in. Test fixtures and certificates from other projects keep
their own licences, listed in [`src-tauri/tests/fixtures/README.md`](src-tauri/tests/fixtures/README.md)
and [`src-tauri/resources/certs/README.md`](src-tauri/resources/certs/README.md). The fonts
(Readex Pro, JetBrains Mono) are under the SIL Open Font License, with the licence texts in
`src/assets/fonts`. The ISCC logos in `src/assets` are marks of the ISCC Foundation and are not
covered by the Apache-2.0 licence.
