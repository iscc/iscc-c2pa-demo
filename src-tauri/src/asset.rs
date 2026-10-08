//! One reader for every supported format: the content behind the Content-Code, a preview picture,
//! the asset's own title and description, and its creator; [`load`] adds the Data-Code and
//! Instance-Code of the file. [`glance`] leaves the slow parts for later: decoding a video, and
//! recognising the scanned pages of a PDF when OCR is on.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context as _, Result};
use image::RgbImage;

use crate::formats::{self, Format, Kind};
use crate::iscc::{self, Content, IsccUnit};
use crate::metadata::{self, Embedded};
use crate::pdf::OcrPages;
use crate::video::{self, Progress};
use crate::{audio, epub, office, pdf, plain, svg, tools};

/// Why a text document without text has no Content-Code.
const NO_TEXT: &str = "no text found in this document";
/// Why a video at a glance has no Content-Code yet.
const NOT_DECODED: &str = "the video is not decoded yet";
/// Why a PDF at a glance with OCR on has no Content-Code yet.
const NOT_RECOGNISED: &str = "the scanned pages are not recognised yet";

/// The input of the Content-Code.
#[derive(Debug, Clone)]
pub enum AssetContent {
    /// Decoded, EXIF-transposed and flattened on white; also the preview.
    Image(RgbImage),
    /// Plain text, cleaned like `iscc_sdk.code_text` does.
    Text(String),
    /// Chromaprint fingerprint and duration of the decoded audio.
    Audio(audio::Audio),
    /// MPEG-7 frame signatures, duration and frame size of the video.
    Video(video::Video),
    /// Nothing to compute a Content-Code from, with the reason (a document without text).
    Unavailable(&'static str),
    /// Not read at all, with the reason (a video without ffmpeg): neither the content nor the
    /// tags are known, so there is neither a Content-Code nor a Meta-Code.
    Unread(&'static str),
    /// A video at a [`glance`]: its tags, facts and a frame are read, its frames not decoded yet,
    /// so its Content-Code is still to come. The signatures of the [`video::Video`] are empty.
    Undecoded(video::Video),
    /// A PDF at a [`glance`] with OCR on: its scanned pages are not recognised yet, so its
    /// Content-Code is still to come.
    Unrecognised,
}

/// What an asset contributes to inspection and signing.
#[derive(Debug, Clone)]
pub struct Asset {
    pub content: AssetContent,
    /// Picture of a non-image asset: an EPUB cover, an office thumbnail, audio cover art or a
    /// video frame.
    pub preview: Option<RgbImage>,
    /// The asset's own title, description, ISCC metadata and creator.
    pub metadata: Embedded,
    /// Why this asset cannot be signed (an encrypted PDF); `None` when it can.
    pub sign_block: Option<&'static str>,
    /// What signing does to this asset that its owner may not want (breaking a PDF's digital
    /// signature); `None` when there is nothing to warn about.
    pub sign_warning: Option<&'static str>,
    /// The scanned pages of a PDF and whether OCR reads them; `None` without scanned pages.
    pub ocr: Option<OcrPages>,
}

/// Parts of a text document as its format reader finds them.
#[derive(Debug, Default)]
pub struct Document {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    /// ISCC metadata (a data URL) stored in the document.
    pub meta: Option<String>,
    /// Encoded cover or thumbnail image, in whatever format the document ships.
    pub cover: Option<Vec<u8>>,
    /// Rendered picture of the document (a PDF's first page); preferred over `cover`.
    pub picture: Option<RgbImage>,
    /// Plain text in reading order, not yet cleaned.
    pub text: String,
    /// Why there is no Content-Code when the text is empty; a generic reason when `None`.
    pub no_text_reason: Option<&'static str>,
}

impl Asset {
    /// The content as the ISCC functions take it.
    pub fn content(&self) -> Content<'_> {
        match &self.content {
            AssetContent::Image(rgb) => Content::Image(rgb),
            AssetContent::Text(text) => Content::Text(text),
            AssetContent::Audio(audio) => Content::Audio(&audio.fingerprint),
            AssetContent::Video(video) => Content::Video(&video.signatures),
            AssetContent::Unavailable(reason) | AssetContent::Unread(reason) => {
                Content::Unavailable(reason)
            }
            AssetContent::Undecoded(_) => Content::Unavailable(NOT_DECODED),
            AssetContent::Unrecognised => Content::Unavailable(NOT_RECOGNISED),
        }
    }

    /// Whether the Content-Code is still to come: a video not decoded or a scan not recognised.
    pub fn content_pending(&self) -> bool {
        matches!(
            self.content,
            AssetContent::Undecoded(_) | AssetContent::Unrecognised
        )
    }

    /// The picture to show: the image itself, or the preview of a document, audio or video file.
    pub fn picture(&self) -> Option<&RgbImage> {
        match &self.content {
            AssetContent::Image(rgb) => Some(rgb),
            _ => self.preview.as_ref(),
        }
    }
}

/// An asset as inspection and signing load it from its file.
#[derive(Debug)]
pub struct Loaded {
    pub asset: Asset,
    /// Data-Code and Instance-Code of the whole file; `None` for a video at a [`glance`], which
    /// is not hashed yet.
    pub bitstream: Option<[IsccUnit; 2]>,
    pub size: u64,
}

/// The last video decoded, so that signing the video just opened does not decode it again.
static DECODED: Memory = Memory(Mutex::new(None));

/// When the scanned pages of a PDF are recognised: never (OCR is off), in a later pass, or now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ocr {
    Off,
    Later,
    Now,
}

/// Load the asset at `path` with the Data-Code and Instance-Code of the file. A video stays on
/// disk, as it can take gigabytes: ffmpeg reads it while the file is hashed alongside, and
/// `progress` follows ffmpeg; the same bytes are decoded only once in a row; without ffmpeg the
/// video is only hashed ([`AssetContent::Unread`]). Every other format is read into memory; with
/// `ocr`, the scanned pages of a PDF are recognised, `progress` following them.
pub fn load(path: &Path, format: &Format, ocr: bool, progress: Progress) -> Result<Loaded> {
    if format.kind == Kind::Video {
        return load_video(path, tools::ffmpeg_path(), progress, &DECODED);
    }
    let ocr = if ocr { Ocr::Now } else { Ocr::Off };
    load_bytes(path, format, ocr, progress)
}

/// [`load`] of a file read into memory, its scanned pages as `ocr` says.
fn load_bytes(path: &Path, format: &Format, ocr: Ocr, progress: Progress) -> Result<Loaded> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    loaded_from(path, &bytes, format, ocr, progress)
}

/// [`load`] of an asset other than a video held in `bytes` (a PDF edited for signing); `path`
/// names the file it comes from.
pub fn load_in_memory(
    path: &Path,
    bytes: &[u8],
    format: &Format,
    ocr: bool,
    progress: Progress,
) -> Result<Loaded> {
    let ocr = if ocr { Ocr::Now } else { Ocr::Off };
    loaded_from(path, bytes, format, ocr, progress)
}

/// The asset held in `bytes` with their Data-Code and Instance-Code.
fn loaded_from(
    path: &Path,
    bytes: &[u8],
    format: &Format,
    ocr: Ocr,
    progress: Progress,
) -> Result<Loaded> {
    Ok(Loaded {
        asset: read_with(path, bytes, format, ocr, progress)?,
        bitstream: Some(iscc::bitstream_units(bytes)?),
        size: bytes.len() as u64,
    })
}

/// Load the asset at `path` so that it shows at once: a video's tags, facts and a preview frame
/// only, neither its frames decoded ([`AssetContent::Undecoded`]) nor the file hashed; without
/// ffmpeg it stays unread. With `ocr`, the scanned pages of a PDF are left for later
/// ([`AssetContent::Unrecognised`]). Every other format loads as with [`load`].
pub fn glance(path: &Path, format: &Format, ocr: bool) -> Result<Loaded> {
    if format.kind != Kind::Video {
        let ocr = if ocr { Ocr::Later } else { Ocr::Off };
        return load_bytes(path, format, ocr, &|_| true);
    }
    let size = std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len();
    let asset = match tools::ffmpeg_path() {
        Ok(ffmpeg) => video::glance(&ffmpeg, path)?,
        Err(e) => unread(missing_reason(e)?),
    };
    Ok(Loaded {
        asset,
        bitstream: None,
        size,
    })
}

/// Load the signed copy at `path` of the asset `source`. A video copy that provably decodes to
/// the frames analysed in the source is not decoded again ([`video::read_copy`]); any other
/// copy loads like any file, its scanned pages recognised with `ocr`.
pub fn load_signed_copy(
    path: &Path,
    format: &Format,
    source: &Asset,
    ocr: bool,
    progress: Progress,
) -> Result<Loaded> {
    if format.kind != Kind::Video {
        return load(path, format, ocr, progress);
    }
    with_ffmpeg(tools::ffmpeg_path(), path, |ffmpeg| {
        video::read_copy(ffmpeg, path, source, progress)
    })
}

/// A video, decoded by the ffmpeg at `ffmpeg` unless `memory` holds the analysis of these very
/// bytes. Only an analysis is kept: a video left unread for lack of ffmpeg is read once it is
/// there.
fn load_video(
    path: &Path,
    ffmpeg: Result<PathBuf>,
    progress: Progress,
    memory: &Memory,
) -> Result<Loaded> {
    let stamp = Stamp::of(path);
    if let Some(loaded) = memory.recall(stamp.as_ref(), path)? {
        return Ok(loaded);
    }
    let loaded = with_ffmpeg(ffmpeg, path, |ffmpeg| video::read(ffmpeg, path, progress))?;
    if matches!(loaded.asset.content, AssetContent::Video(_)) {
        memory.keep(stamp, &loaded);
    }
    Ok(loaded)
}

/// The asset `analyse` makes of the video at `path` with the ffmpeg at `ffmpeg`, the file hashed
/// alongside. When `ffmpeg` is a [`tools::Missing`] error, the file is only hashed and stays
/// unread, which costs the Meta-Code and the Content-Code, not the inspection.
fn with_ffmpeg(
    ffmpeg: Result<PathBuf>,
    path: &Path,
    analyse: impl FnOnce(&Path) -> Result<Asset>,
) -> Result<Loaded> {
    match ffmpeg {
        Ok(ffmpeg) => hashed_alongside(path, || analyse(&ffmpeg)),
        Err(e) => {
            let reason = missing_reason(e)?;
            hashed_alongside(path, || Ok(unread(reason)))
        }
    }
}

/// The reason of a [`tools::Missing`] error; any other error stays one.
fn missing_reason(e: anyhow::Error) -> Result<&'static str> {
    let reason = e
        .downcast_ref::<tools::Missing>()
        .map(tools::Missing::reason);
    reason.ok_or(e)
}

/// A video not read, for `reason`.
fn unread(reason: &'static str) -> Asset {
    Asset {
        content: AssetContent::Unread(reason),
        preview: None,
        metadata: Embedded::default(),
        sign_block: None,
        sign_warning: None,
        ocr: None,
    }
}

/// The analysis of the video decoded last, and the file it was made from.
struct Memory(Mutex<Option<Decoded>>);

struct Decoded {
    stamp: Stamp,
    /// Instance-Code of the file, which proves the bytes are the same.
    instance: String,
    asset: Asset,
}

/// Where a file is, its size and when it was modified: a hint that it is unchanged, which only
/// the Instance-Code confirms.
#[derive(PartialEq)]
struct Stamp {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

impl Stamp {
    /// The stamp of the file at `path`; `None` when the file system gives no modification time.
    fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        Some(Stamp {
            path: std::fs::canonicalize(path).ok()?,
            size: meta.len(),
            modified: meta.modified().ok()?,
        })
    }
}

impl Memory {
    /// The kept analysis when the file at `path` still has its stamp and, hashed again (its
    /// Data-Code and Instance-Code are needed anyway), its Instance-Code.
    fn recall(&self, stamp: Option<&Stamp>, path: &Path) -> Result<Option<Loaded>> {
        let kept = self
            .lock()
            .as_ref()
            .and_then(|d| (Some(&d.stamp) == stamp).then(|| (d.instance.clone(), d.asset.clone())));
        let Some((instance, asset)) = kept else {
            return Ok(None);
        };
        let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let (bitstream, size) = iscc::stream_units(file)?;
        Ok((bitstream[1].iscc == instance).then_some(Loaded {
            asset,
            bitstream: Some(bitstream),
            size,
        }))
    }

    /// Keep the analysis of the file with `stamp` in place of the one before.
    fn keep(&self, stamp: Option<Stamp>, loaded: &Loaded) {
        let instance = loaded.bitstream.as_ref().map(|b| b[1].iscc.clone());
        *self.lock() = stamp.zip(instance).map(|(stamp, instance)| Decoded {
            stamp,
            instance,
            asset: loaded.asset.clone(),
        });
    }

    /// The kept analysis, locked; a panic elsewhere while it was locked leaves it usable.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Decoded>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The asset `read` makes of the video at `path`, the file hashed in a second thread meanwhile.
/// When `read` fails or is cancelled, the hashing stops too, rather than read a large file to
/// its end for nothing.
fn hashed_alongside(path: &Path, read: impl FnOnce() -> Result<Asset>) -> Result<Loaded> {
    let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let stop = AtomicBool::new(false);
    let (asset, hashed) = std::thread::scope(|scope| {
        let hashing = scope.spawn(|| iscc::stream_units(Stoppable { file, stop: &stop }));
        let asset = read();
        stop.store(asset.is_err(), Ordering::Relaxed);
        (asset, hashing.join())
    });
    let asset = asset?;
    let (bitstream, size) = hashed.map_err(|_| anyhow!("hashing the file failed"))??;
    Ok(Loaded {
        asset,
        bitstream: Some(bitstream),
        size,
    })
}

/// A file whose reads fail once `stop` is set.
struct Stoppable<'a> {
    file: File,
    stop: &'a AtomicBool,
}

impl Read for Stoppable<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("stopped"));
        }
        self.file.read(buf)
    }
}

/// Read the asset held in `bytes`; `path` names the file for formats that resolve resources
/// next to it (SVG). A video goes through a temporary file, as ffmpeg reads files. With `ocr`, the
/// scanned pages of a PDF are recognised (from the memo of `ocr` when seen before).
pub fn read(path: &Path, bytes: &[u8], format: &Format, ocr: bool) -> Result<Asset> {
    let ocr = if ocr { Ocr::Now } else { Ocr::Off };
    read_with(path, bytes, format, ocr, &|_| true)
}

/// [`read`], the scanned pages of a PDF recognised as `ocr` says, `progress` following them.
fn read_with(
    path: &Path,
    bytes: &[u8],
    format: &Format,
    ocr: Ocr,
    progress: Progress,
) -> Result<Asset> {
    match format.mime {
        formats::SVG => svg::read(path, bytes),
        _ if format.kind == Kind::Image => read_raster(bytes),
        _ if format.kind == Kind::Audio => audio::read(bytes, format),
        _ if format.kind == Kind::Video => video::read_bytes(&tools::ffmpeg_path()?, bytes, format),
        formats::EPUB => Ok(from_document(epub::read(bytes)?)),
        formats::TXT | formats::MARKDOWN => Ok(from_document(plain::read(bytes))),
        formats::PDF => read_pdf(bytes, ocr, progress),
        mime if office::handles(mime) => Ok(from_document(office::read(bytes, mime)?)),
        mime => bail!("no reader for {mime}"),
    }
}

/// A raster image and the metadata embedded in it.
fn read_raster(bytes: &[u8]) -> Result<Asset> {
    Ok(Asset {
        content: AssetContent::Image(iscc::decode_rgb(bytes)?),
        preview: None,
        metadata: metadata::image(bytes),
        sign_block: None,
        sign_warning: None,
        ocr: None,
    })
}

/// A PDF, with the reason it cannot be signed or the warning that signing it deserves, and its
/// scanned pages recognised as `ocr` says.
fn read_pdf(bytes: &[u8], ocr: Ocr, progress: Progress) -> Result<Asset> {
    let pdf = pdf::read(bytes)?;
    let flags = pdf.flags;
    let pages = pdf.ocr_pages(ocr != Ocr::Off);
    let asset = match (ocr, pages) {
        (Ocr::Now, Some(_)) => from_document(pdf.recognised(bytes, progress)?),
        (Ocr::Later, Some(_)) => Asset {
            content: AssetContent::Unrecognised,
            ..from_document(pdf.document)
        },
        _ => from_document(pdf.document),
    };
    Ok(Asset {
        sign_block: flags.sign_block(),
        sign_warning: flags.sign_warning(),
        ocr: pages,
        ..asset
    })
}

/// A text document with its cleaned text, or the reason it has none, and its rendered picture or,
/// when the image crate can decode it, its cover.
fn from_document(doc: Document) -> Asset {
    let text = iscc_lib::text_clean(&doc.text);
    let content = if iscc_lib::text_collapse(&text).is_empty() {
        AssetContent::Unavailable(doc.no_text_reason.unwrap_or(NO_TEXT))
    } else {
        AssetContent::Text(text)
    };
    Asset {
        content,
        preview: doc
            .picture
            .or_else(|| doc.cover.and_then(|c| iscc::decode_rgb(&c).ok())),
        metadata: metadata::document(
            doc.title.as_deref(),
            doc.description.as_deref(),
            doc.meta.as_deref(),
            doc.creator.as_deref(),
        ),
        sign_block: None,
        sign_warning: None,
        ocr: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iscc::UnitSelection;
    use crate::metadata::tests::sdk_meta_code;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Write the extracted text of every text fixture to `DUMP_TEXT_DIR`, for comparison with
    /// the texts `expected_text.py DUMP_DIR` writes from Tika.
    #[test]
    #[ignore = "a debugging aid; set DUMP_TEXT_DIR and run with --ignored"]
    fn dump_texts_for_comparison() {
        let dir = std::env::var("DUMP_TEXT_DIR").expect("DUMP_TEXT_DIR names a directory");
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_text.json")).unwrap();
        for file in expected.as_object().unwrap().keys() {
            let path = fixture(file);
            let bytes = std::fs::read(&path).unwrap();
            let asset = read(&path, &bytes, formats::by_path(&path).unwrap(), false).unwrap();
            if let AssetContent::Text(text) = asset.content {
                std::fs::write(Path::new(&dir).join(format!("{file}.txt")), text).unwrap();
            }
        }
    }

    /// An empty memory of decoded videos, apart from the one other tests share.
    fn memory() -> Memory {
        Memory(Mutex::new(None))
    }

    #[test]
    fn cancelled_video_load_stops_hashing_and_reports_it() {
        crate::tools::tests::ensure_ffmpeg();
        let path = fixture("demo.mp4");
        let error = load_video(&path, tools::ffmpeg_path(), &|_| false, &memory()).unwrap_err();
        assert!(
            error.downcast_ref::<tools::Cancelled>().is_some(),
            "{error:#}"
        );
        let loaded = load(&path, formats::by_path(&path).unwrap(), false, &|_| true).unwrap();
        assert_eq!(loaded.size, std::fs::metadata(&path).unwrap().len());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            loaded.bitstream,
            Some(iscc::bitstream_units(&bytes).unwrap())
        );
    }

    #[test]
    fn a_video_at_a_glance_is_neither_decoded_nor_hashed() {
        crate::tools::tests::ensure_ffmpeg();
        let path = fixture("demo.mp4");
        let glanced = glance(&path, formats::by_path(&path).unwrap(), false).unwrap();
        let AssetContent::Undecoded(video) = &glanced.asset.content else {
            panic!("{:?}", glanced.asset.content);
        };
        assert!(video.signatures.is_empty());
        assert_eq!((video.width, video.height), (176, 144));
        assert!(glanced.asset.preview.is_some(), "a frame to show");
        assert_eq!(glanced.bitstream, None);
        assert_eq!(glanced.size, std::fs::metadata(&path).unwrap().len());
        assert!(iscc::content_unit(glanced.asset.content()).is_err());
        // Any other format loads in full.
        let image = fixture("CA.jpg");
        let loaded = glance(&image, formats::by_path(&image).unwrap(), true).unwrap();
        assert!(loaded.bitstream.is_some());
    }

    #[test]
    fn a_video_is_decoded_once_until_its_bytes_change() {
        crate::tools::tests::ensure_ffmpeg();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo.mp4");
        std::fs::copy(fixture("demo.mp4"), &path).unwrap();
        let memory = memory();
        let reports = std::cell::Cell::new(0);
        let counting = |_: Option<f64>| {
            reports.set(reports.get() + 1);
            true
        };
        let code = |loaded: &Loaded| iscc::content_unit(loaded.asset.content()).unwrap();

        let first = load_video(&path, tools::ffmpeg_path(), &counting, &memory).unwrap();
        assert!(reports.get() > 0, "decoded");
        reports.set(0);
        let again = load_video(&path, tools::ffmpeg_path(), &counting, &memory).unwrap();
        assert_eq!(reports.get(), 0, "the same bytes are not decoded again");
        assert_eq!(again.bitstream, first.bitstream);
        assert_eq!(code(&again), code(&first));

        // Same path, size and modification time, other bytes: the Instance-Code tells.
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[15] ^= 1; // the ftyp box's minor version, which decoding ignores
        std::fs::write(&path, &bytes).unwrap();
        let file = File::options().write(true).open(&path).unwrap();
        file.set_modified(modified).unwrap();
        drop(file);
        let changed = load_video(&path, tools::ffmpeg_path(), &counting, &memory).unwrap();
        assert!(reports.get() > 0, "changed bytes are decoded again");
        assert_eq!(
            changed.bitstream,
            Some(iscc::bitstream_units(&bytes).unwrap())
        );
        assert_ne!(changed.bitstream, first.bitstream);
    }

    #[test]
    fn a_video_without_ffmpeg_is_only_hashed_until_ffmpeg_is_there() {
        crate::tools::tests::ensure_ffmpeg();
        let path = fixture("demo.mp4");
        let memory = memory();
        let missing = tools::Missing::Ffmpeg { available: true };
        let never = |_: Option<f64>| -> bool { panic!("nothing is decoded") };
        let unread = load_video(&path, Err(missing.into()), &never, &memory).unwrap();
        assert!(matches!(unread.asset.content, AssetContent::Unread(r) if r == missing.reason()));
        assert_eq!(unread.asset.metadata, Embedded::default(), "tags unread");
        assert!(unread.asset.preview.is_none());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            unread.bitstream,
            Some(iscc::bitstream_units(&bytes).unwrap())
        );

        // Once ffmpeg is installed the same file is decoded, not recalled unread.
        let read = load_video(&path, tools::ffmpeg_path(), &|_| true, &memory).unwrap();
        assert!(matches!(read.asset.content, AssetContent::Video(_)));

        let other = with_ffmpeg(Err(anyhow!("no local data folder")), &path, |_| {
            unreachable!("no ffmpeg to run")
        });
        assert_eq!(other.unwrap_err().to_string(), "no local data folder");
    }

    #[test]
    fn text_units_and_metadata_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_text.py (iscc-sdk with Tika).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_text.json")).unwrap();
        let selection = UnitSelection {
            content: true,
            data: true,
            instance: true,
            ..Default::default()
        };
        for (file, want) in expected.as_object().unwrap() {
            let path = fixture(file);
            let bytes = std::fs::read(&path).unwrap();
            let asset = read(&path, &bytes, formats::by_path(&path).unwrap(), false).unwrap();
            assert_eq!(
                asset.metadata.name.as_deref(),
                want["title"].as_str(),
                "{file} title"
            );
            assert_eq!(
                asset.metadata.description.as_deref(),
                want["description"].as_str(),
                "{file} description"
            );
            assert_eq!(
                asset.metadata.creator.as_deref(),
                want["creator"].as_str(),
                "{file} creator"
            );
            assert_eq!(
                sdk_meta_code(&asset.metadata, &path),
                want["meta"],
                "{file} Meta-Code"
            );
            let bitstream = iscc::bitstream_units(&bytes).unwrap();
            let units = iscc::units_for(
                &bitstream,
                asset.content(),
                Default::default(),
                None,
                &selection,
            )
            .unwrap();
            assert_eq!(units[0].iscc, want["text"], "{file} Content-Code Text");
            assert_eq!(units[1].iscc, want["data"], "{file} Data-Code");
            assert_eq!(units[2].iscc, want["instance"], "{file} Instance-Code");
        }
    }
}
