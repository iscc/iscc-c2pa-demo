# ISCC C2PA Demo

A small native desktop app that shows what is inside a file's Content Credentials and signs
images, documents and audio with a C2PA manifest carrying an ISCC soft binding. A command-line tool,
`c2pa-iscc`, does the same from a terminal or a script.

**Download** the app for Windows, macOS and Linux from
[c2pa-demo.iscc.codes](https://c2pa-demo.iscc.codes) or the
[releases page](https://github.com/iscc/iscc-c2pa-demo/releases). The builds are not code-signed
yet; the download page says how to open them the first time.

Supported formats:

| Kind | Formats | Content-Code |
|---|---|---|
| Images | JPEG, PNG, WebP, GIF, TIFF, SVG | Content-Code Image from the pixels (SVG: rendered with resvg) |
| Documents | EPUB, Word (DOCX), PowerPoint (PPTX), Excel (XLSX), OpenDocument text, spreadsheet and presentation (ODT, ODS, ODP), plain text, Markdown | Content-Code Text from the text in reading order |
| Audio | MP3, FLAC, WAV, M4A (AAC or ALAC) | Content-Code Audio from a Chromaprint fingerprint of the decoded audio |

These are the formats c2pa-rs can embed a manifest into, so every file the app opens it can also
sign.

Drop a file onto the window to see:

- the C2PA manifest store with validation state, signer, actions, ingredients and every assertion;
- the ISCC units of the file (Meta-Code, Content-Code Image, Text or Audio, Data-Code,
  Instance-Code), computed with any embedded manifest store stripped. Title, description, text
  and audio fingerprint are extracted with the same rules as
  [iscc-sdk](https://github.com/iscc/iscc-sdk), and the test suite checks the units against
  iscc-sdk's output for every fixture;
- the ISCC soft binding embedded in the manifest, decoded from its ISCC-SEQ value and compared
  unit by unit with the file;
- the CAWG training and data mining assertion, if present.

The Sign tab writes a signed copy with:

- a `c2pa.soft-binding` assertion using algorithm `io.iscc.v0`, whose value is the ISCC-SEQ
  defined by [IEP-0020](https://ieps.iscc.codes/iep-0020/) (one or more 256-bit ISCC-UNITs,
  header and body concatenated);
- a `cawg.metadata` assertion with the title and description behind the Meta-Code, so a signed
  copy recomputes the Meta-Code it was signed with;
- an optional `cawg.training-mining` assertion (CAWG Training and Data Mining Assertion 1.1);
- a `c2pa.created` action with the chosen digital source type, or, when the source already has
  Content Credentials, a new manifest with the existing one as parent ingredient;
- an RFC 3161 timestamp from a time stamping authority, so the manifest proves when it was
  signed (see below).

## Stack

| Layer | What |
|---|---|
| Shell | [Tauri 2](https://tauri.app) (Rust core, system web view) |
| C2PA | [`c2pa`](https://crates.io/crates/c2pa) 0.91 with Rust native crypto, no OpenSSL |
| ISCC | [`iscc-lib`](https://crates.io/crates/iscc-lib) 0.6 plus a Pillow-equivalent image normalisation in `src-tauri/src/iscc.rs` |
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

## Run

Requirements: Rust 1.96+, Node 22.12+ (CI uses 24), pnpm 10, and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/) of your system: the WebView2
runtime on Windows (present on Windows 10/11), the Xcode command line tools on macOS, and
WebKitGTK 4.1 with its development packages on Linux (also needed for `cargo test`).

```sh
pnpm install
pnpm tauri dev                 # dev build with hot reload, Vite on port 43172
pnpm tauri dev -- -- /full/path/to/image.jpg  # open a file at startup (absolute path)
pnpm tauri build               # installers under src-tauri/target/release/bundle
```

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
```

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

Data-Code and Instance-Code are computed over the asset with its C2PA manifest store stripped, on
both the signing and the verifying side. Embedding a manifest therefore does not break them: a
signed file matches all embedded units exactly until its content bytes change. IEP-0020 leaves the
inputs of the units to the implementer; this is the convention used here. Content-Code Image and
Content-Code Audio also survive re-encoding.

EPUB and the office formats are ZIP containers. c2pa-rs embeds the manifest as
`META-INF/content_credential.c2pa` with a collection data hash as hard binding, and removing it
again rewrites the central directory, so the stripped bytes differ from the original. A signed
document therefore shows a Data-Code that stays close and a failed Instance-Code; Content-Code
Text, computed from the text in reading order, is the unit that identifies the document. SVG,
WebP, TIFF, MP3 (c2pa-rs rewrites its ID3 tag), FLAC and WAV keep similar traces of a removed
manifest; plain text, Markdown, JPEG, PNG, GIF and M4A come back byte for byte, so all four
units match exactly. Content-Code Audio, like Content-Code Image, survives re-encoding.

The assertion also carries the optional `bindingMetadata` map of the C2PA specification (2.3
and later) with a description of the ISCC, the contact `info@iscc.io` and a link to IEP-0020, so
anyone reading the manifest learns how to interpret the value without consulting the soft binding
algorithm list. Validators ignore the map by spec; the Content Credentials tab shows it for any
soft-binding assertion that has one.

IEP-0020 is a draft. The assertion is built in `src-tauri/src/sign.rs` and decoded in
`src-tauri/src/inspect.rs`; both go through `iscc::encode_seq` / `iscc::decode_seq`.

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

`scripts/shot.ps1` drives and screenshots the running app on Windows, for checking the UI by eye.

## Licence

Apache-2.0, see [LICENSE](LICENSE). Test fixtures and certificates from other projects keep
their own licences, listed in [`src-tauri/tests/fixtures/README.md`](src-tauri/tests/fixtures/README.md)
and [`src-tauri/resources/certs/README.md`](src-tauri/resources/certs/README.md). The fonts
(Readex Pro, JetBrains Mono) are under the SIL Open Font License, with the licence texts in
`src/assets/fonts`. The ISCC logos in `src/assets` are marks of the ISCC Foundation and are not
covered by the Apache-2.0 licence.
