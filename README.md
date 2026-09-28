<p align="center">
  <a href="https://iscc.io">
    <picture>
      <source media="(prefers-color-scheme: dark)" srcset="src/assets/iscc-logo-white-coral.svg">
      <img src="src/assets/iscc-logo-black-coral.svg" alt="ISCC home" height="56">
    </picture>
  </a>
</p>

<h1 align="center">ISCC C2PA Demo</h1>

<p align="center">
  <b>Bind Content Credentials to the content, not just the bytes.</b><br>
  A tech demo of the International Standard Content Code (ISCC)<br>
  as a soft binding in C2PA Content Credentials, for images, documents and audio.
</p>

<p align="center">
  <a href="https://c2pa-demo.iscc.codes"><b>Download</b></a> ·
  <a href="#why-we-built-it">Why</a> ·
  <a href="#what-goes-into-the-manifest">The assertion</a> ·
  <a href="https://ieps.iscc.codes/iep-0020/">IEP-0020</a>
</p>

![A signed image, re-encoded at half size with its Content Credentials copied back in. C2PA reports the credentials as invalid because the file changed after signing; the ISCC soft binding still matches the Content-Code at 100 percent, while the byte-based Data-Code and Instance-Code no longer match.](site/assets/changed.webp)

<p align="center"><sub>To show what each binding sees, we re-encoded a signed image at half size and copied its
Content Credentials back in. C2PA says <b>Invalid</b> (<code>assertion.dataHash.mismatch</code>):
the hash no longer matches the bytes. The ISCC Content-Code still matches the pixels at
<b>100%</b>, and at 81% after a crop; unrelated images score around 50%. The credentials stay
invalid, but the ISCC shows which content they were made for.</sub></p>

C2PA Content Credentials record where a file comes from. They are tied to the file by a hash of
its exact bytes, so a resized or re-encoded copy no longer matches them, and a copy whose manifest
was stripped has lost them. A **soft binding** connects credentials and content through the content
itself, so a service can find the credentials of a copy again. This demo uses **ISCC**
(ISO 24138:2024), a code that anyone can calculate from the content:

1. **Inspect** any image, document or audio file: its Content Credentials (who signed it, when, and
   whether the signature holds) and its ISCC.
2. **Sign** a copy whose Content Credentials carry the ISCC and, if you choose, whether AI training
   and data mining is allowed.
3. **Check** a signed file, changed or not: the app calculates the ISCC again and compares it with
   the one in the Content Credentials, unit by unit.

## Why we built it

**To show that it runs.** ISCC has been on the C2PA soft binding algorithm list as
`io.iscc.v0` since 2024. This demo runs it end to end with
[c2pa-rs](https://github.com/contentauth/c2pa-rs), the open source C2PA SDK of the Content
Authenticity Initiative, on 19 file formats.

**To show what a computed soft binding does.** C2PA soft bindings come in two kinds. A watermark is
put into the content and read back with a decoder. A fingerprint such as ISCC is calculated from
the content as it is, so anyone with software that implements the open standard can calculate it
again from a copy and compare.

**To build it together.** [IEP-0020](https://ieps.iscc.codes/iep-0020/) defines how an ISCC is
stored in a C2PA manifest, and it is still a draft. Try the demo on files you know, reuse the code
and tell us what works and what does not, in the
[issues](https://github.com/iscc/iscc-c2pa-demo/issues) or at info@iscc.io.

## What goes into the manifest

A `c2pa.soft-binding` assertion whose value is an ISCC-SEQ: the ISCC units of the file (here
Meta-Code, Content-Code Image, Data-Code and Instance-Code), each a header and a 256-bit body,
concatenated. In CBOR the value is a byte string; the JSON view shows it in base64.

```json
{
  "label": "c2pa.soft-binding",
  "data": {
    "alg": "io.iscc.v0",
    "blocks": [{ "scope": {}, "value": "AAfn6v8X9v/rPP+/F/v37m7/53f/42Tq+9/rfi59/X//vyEHw0Mw…" }],
    "bindingMetadata": {
      "description": "International Standard Content Code (ISCC - ISO 24138:2024) - Open Source Content Identification",
      "contact": "info@iscc.io",
      "informationalUrl": "https://ieps.iscc.codes/iep-0020/"
    }
  }
}
```

Next to it: a `cawg.metadata` assertion with the title and description behind the Meta-Code, an
optional `cawg.training-mining` assertion, and an RFC 3161 timestamp.
[DEVELOPMENT.md](DEVELOPMENT.md#notes-on-the-soft-binding) explains how each unit is calculated
and checked, including whether signing left the original file intact byte for byte.

## Download

Installers for Windows, macOS and Linux are on
**[c2pa-demo.iscc.codes](https://c2pa-demo.iscc.codes)** and the
[releases page](https://github.com/iscc/iscc-c2pa-demo/releases). The builds are not code-signed
yet; the download page says how to open them the first time.

| Kind | Formats |
|---|---|
| Images | JPEG, PNG, WebP, GIF, TIFF, SVG |
| Documents | EPUB, DOCX, PPTX, XLSX, ODT, ODS, ODP, TXT, Markdown |
| Audio | MP3, FLAC, WAV, M4A |

## Good to know

- **It does not look credentials up yet.** Finding the credentials of a stripped copy by its ISCC
  needs a lookup service, which C2PA specifies as the Soft Binding Resolution API. That is the next
  step.
- **The built-in certificate is a test certificate.** Anyone can check the ISCC, but only this app
  trusts the demo signature. Sign with your own certificate and key for anything real.
- **A matching ISCC is a strong signal, not proof.** It suggests that two files hold the same or
  similar content. It says nothing about who made them or who holds the rights.
- **Your files stay on your computer.** Signing sends only a hash of the signature to a timestamp
  service, never the file. Inspecting stays offline.

## For developers

Rust on [Tauri 2](https://tauri.app): [c2pa-rs](https://crates.io/crates/c2pa) 0.91 with Rust
native crypto (no OpenSSL) and [iscc-lib](https://crates.io/crates/iscc-lib), with pure Rust readers
for every format. No external tools, no Python. The tests check every ISCC unit against iscc-core,
the ISCC reference implementation, and iscc-sdk. The same core also builds as `c2pa-iscc`, a
command-line tool that prints JSON.

```sh
pnpm install
pnpm tauri dev                                                               # the app
cd src-tauri && cargo run --features cli --bin c2pa-iscc -- sign photo.jpg   # the CLI
```

The assertion is built in [`src-tauri/src/sign.rs`](src-tauri/src/sign.rs) and read in
[`src-tauri/src/inspect.rs`](src-tauri/src/inspect.rs). [DEVELOPMENT.md](DEVELOPMENT.md) covers
requirements, the CLI, tests, timestamps and releases.

## Licence

A technology demonstration, provided as is without warranty (see LICENSE sections 7 and 8).
Apache-2.0, see [LICENSE](LICENSE). Made by the [ISCC Foundation](https://iscc.io). The ISCC logos
are marks of the ISCC Foundation and are not covered by the licence. Third-party fixtures,
certificates and fonts keep their own licences, listed in [DEVELOPMENT.md](DEVELOPMENT.md#licences).
