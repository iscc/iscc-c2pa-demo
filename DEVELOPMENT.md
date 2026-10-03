# Developing the ISCC C2PA Demo

Everything behind the [README](README.md): what the app reads and writes, how it is built and
tested, and how a release is made.

## What the app does

Opening a file shows:

- the C2PA manifest store with validation state, signer, actions, ingredients and every assertion;
- the ISCC units of the file (Meta-Code, Content-Code Image, Text or Audio, Data-Code,
  Instance-Code), computed from the whole file as any ISCC tool computes them. Title, description, text
  and audio fingerprint are extracted with the same rules as
  [iscc-sdk](https://github.com/iscc/iscc-sdk), and the test suite checks the units against
  iscc-sdk's output for every fixture;
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
  copy recomputes the Meta-Code it was signed with;
- an optional `cawg.training-mining` assertion (CAWG Training and Data Mining Assertion 1.1);
- a `c2pa.opened` action with the source as parent ingredient, which carries the chosen digital
  source type or, when the source already has Content Credentials, the existing manifest;
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
| Audio | [`symphonia`](https://crates.io/crates/symphonia) 0.6 decodes, a resampler with fpcalc's settings in `src-tauri/src/resample.rs`, [`rusty-chromaprint`](https://crates.io/crates/rusty-chromaprint) fingerprints, [`lofty`](https://crates.io/crates/lofty) reads tags and cover art; no external tools |
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
uv run scripts/fetch_pdfium.py # once after cloning, and when the pinned pdfium build changes
pnpm tauri dev                 # dev build with hot reload, Vite on port 43172
pnpm tauri dev -- -- /full/path/to/image.jpg  # open a file at startup (absolute path)
pnpm tauri build               # installers under src-tauri/target/release/bundle
```

**pdfium is a build prerequisite.** `tauri.conf.json` bundles `src-tauri/pdfium/*` with the app,
and the Tauri build script fails on every cargo run (build, test, clippy, rust-analyzer) when
that directory is empty: "glob pattern pdfium/* path not found". `scripts/fetch_pdfium.py`
downloads the pinned build of [bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries)
for your system, checks its SHA-256 and extracts the library and its licence files there; it does
nothing when the right build is present. The pre-push hooks, `mise run lint`, `mise run test`,
CI and the release workflow run it themselves. The app loads the library at run time from its
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
cargo run --features cli --bin c2pa-iscc -- sign --help                                # every option
```

The CLI prints the same JSON the desktop app works with (`--compact` for one line,
`--no-preview` to leave out the base64 preview image). `sign` fills in every option you leave
out the way the Sign tab prefills its form: the file's own title and description, all four
units, the built-in demo certificate, a timestamp from Encypher, and a `-signed` copy next to
the source. `--tsa URL` picks another timestamp service, `--no-timestamp` signs without network
access. Errors print one line on stderr and exit with code 1.

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
```

`expected_pdf.py DIR` writes references for every PDF below a folder of your own, and
`PDF_CORPUS_DIR=DIR cargo test --lib pdf_corpus -- --ignored` compares the app with them. Run it
on a large set of PDFs before changing the pdfium build.

Sources and licences of the fixtures are listed in
[`src-tauri/tests/fixtures/README.md`](src-tauri/tests/fixtures/README.md).

## Timestamps

Signing asks a time stamping authority (TSA) to countersign the signature, so the manifest
proves that it existed at a given time and stays verifiable after the signing certificate
expires. This is the only network access of the app: it sends a SHA-256 hash of the signature,
nothing of the file. The default service is [Encypher](https://tsa.encypher.com), the only free
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

**Source preservation.** A signed file is *source-preserving* when its *source view* (the file
without the byte ranges its C2PA data hash excludes) is byte for byte the file that was signed. The
embedded Instance-Code proves it: the app computes the Instance-Code of the source view and
compares. The soft binding card shows one of three results:

| Result | When | Formats signed here (c2pa-rs 0.91) |
|---|---|---|
| Source preserved | Instance-Codes equal | JPEG, PNG, GIF, TXT, Markdown |
| Source not preserved | embedding changed other bytes, or the source already had Content Credentials | WebP, WAV (RIFF size field), TIFF (new image directory), SVG (namespace declaration), MP3 (ID3 tag rewritten), FLAC (ID3 tag in front), PDF (rewritten as a whole); any re-signed file |
| Not verifiable | no data hash, no Instance-Code, a signature that does not validate or no longer covers the manifest as it is, or the file changed after signing | EPUB, DOCX, PPTX, XLSX, ODT, ODS, ODP (collection hash), M4A (BMFF hash) |

`source_preservation_per_format` in `src-tauri/src/sign.rs` pins the table.

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
Sign tab goes on that ingredient when the source has no Content Credentials of its own.

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
iscc-sdk must use the same build. `PDFIUM_BUILD` in `scripts/fetch_pdfium.py` is the only place
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

**Signing rewrites the PDF.** c2pa-rs (feature `pdf`, lopdf) loads the whole document and saves a
new file with the manifest as an embedded file; incremental updates, linearisation and object
streams are flattened, so a signed PDF is never source-preserving. Two consequences the app
guards against:

- An encrypted PDF cannot be signed: one with an empty user password comes out silently
  decrypted, one with a user password fails. The Sign tab, `sign()` and the CLI refuse both with
  a plain reason (`Inspection.sign_block`). Both still inspect; one that needs a password has no
  text and no metadata.
- An existing digital signature (`/ByteRange`) breaks. The Sign tab shows a warning above the
  button and the CLI prints it as a `note:` (`Inspection.sign_warning`); the empty signature
  field of a blank form is not a signature. pdfium still counts the broken signature in the
  signed copy, so the copy warns again.

## Thumbnail

Each signed file carries one thumbnail, the claim thumbnail (`c2pa.thumbnail.claim`), made by the
app from the same picture the Content-Code or the preview is computed from: the image itself
(EXIF-rotated, transparency flattened on white), the rendered SVG, a PDF's first page, the EPUB
cover, the thumbnail an office file was saved with, or the cover art of an audio file. It is a JPEG at quality 75,
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
compiled into it included, sit next to it as `LICENSE-*`. Test fixtures and certificates from other projects keep
their own licences, listed in [`src-tauri/tests/fixtures/README.md`](src-tauri/tests/fixtures/README.md)
and [`src-tauri/resources/certs/README.md`](src-tauri/resources/certs/README.md). The fonts
(Readex Pro, JetBrains Mono) are under the SIL Open Font License, with the licence texts in
`src/assets/fonts`. The ISCC logos in `src/assets` are marks of the ISCC Foundation and are not
covered by the Apache-2.0 licence.
