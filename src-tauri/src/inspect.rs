//! Read an asset: file facts, preview, ISCC units and the C2PA manifest store with validation.

use std::cell::OnceCell;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use c2pa::assertions::{labels, SoftBinding};
use c2pa::{Context, Manifest, Reader, ValidationState};
use serde::Serialize;
use serde_json::Value;

use crate::asset::{self, Asset, AssetContent, Loaded};
use crate::context::{base_settings, ISCC_SOFT_BINDING_ALG};
use crate::formats::{self, Format, Kind};
use crate::iscc::{self, Content, IsccUnit, MetaInput};
use crate::metadata::{self, ManifestMeta, MetaFields};
use crate::pdf::OcrPages;
use crate::semantic::{SemanticKind, SemanticKinds};
use crate::tools::{self, Cancelled};
use crate::{audio, pdf_update, semantic, thumbnail};

/// JPEG quality of the preview image.
const PREVIEW_QUALITY: u8 = 82;
/// Label of the CAWG training and data mining assertion.
pub const TRAINING_MINING_LABEL: &str = "cawg.training-mining";

/// Everything the UI shows for a dropped file.
#[derive(Serialize, Debug)]
pub struct Inspection {
    pub path: String,
    pub file_name: String,
    /// Default output path for signing: a `-signed` sibling that does not exist yet.
    pub suggested_output: String,
    pub mime: &'static str,
    /// Display name of the format.
    pub format_label: &'static str,
    /// Decides which Content-Code applies and how the asset is shown.
    pub kind: Kind,
    pub size_bytes: u64,
    /// Pixel size of an image (the rendered size of an SVG) or of a video's frames; 0 for text and
    /// audio assets.
    pub width: u32,
    pub height: u32,
    /// JPEG preview as a data URL (the cover or thumbnail of a document); empty when there is
    /// nothing to show.
    pub preview: String,
    /// Characters of extracted text; `None` for images, audio and video.
    pub characters: Option<usize>,
    /// Length of the decoded audio or of the video in seconds; `None` for images and text, and
    /// for a video whose duration ffmpeg does not know.
    pub duration_secs: Option<f64>,
    /// Creator named in the asset's own metadata; display only.
    pub creator: Option<String>,
    /// Meta, Semantic, Content, Data and Instance units of the whole file, as any ISCC tool
    /// computes them and as signing this file would embed them.
    pub iscc: Vec<IsccUnit>,
    /// Units a later pass still computes ([`Depth::GLANCE`]): `semantic`, a video's `content`,
    /// `data` and `instance`, and a scan's `content` when OCR is on.
    pub pending: Vec<&'static str>,
    /// Title, description and ISCC metadata behind the Meta-Code, and where the title came from.
    pub meta_fields: MetaFields,
    /// Why the Meta-Code could not be computed from `meta_fields`, or why the file's own tags
    /// could not be read (a video without ffmpeg); `iscc` then lacks it.
    pub meta_error: Option<String>,
    /// Why the Content-Code could not be computed (audio too short, a document without text, a
    /// video without frames or without ffmpeg); `iscc` then lacks it.
    pub content_error: Option<String>,
    /// Why an image or a text has no Semantic-Code (its model is not installed, a document
    /// without text); `iscc` then lacks it. `None` for audio and video, which have none, and for a
    /// kind of Semantic-Code left out ([`Depth::semantic_kinds`]).
    pub semantic_error: Option<String>,
    /// The Content-Code is that of the source just signed, whose compressed video this signed
    /// copy carries unchanged, so it decodes to the same frames; false when it was computed from
    /// this file's own frames.
    pub content_from_source: bool,
    /// How many pages of a PDF are scans and whether OCR read them, its Content-Code then coming
    /// from their recognised text; `None` for a file without scanned pages.
    pub ocr: Option<OcrPages>,
    /// Why this file cannot be signed (an encrypted PDF); `None` when it can.
    pub sign_block: Option<&'static str>,
    /// What signing does to this file that its owner may not want (breaking a PDF's digital
    /// signature).
    pub sign_warning: Option<&'static str>,
    pub manifest: Option<ManifestSummary>,
    /// Full manifest store as produced by `Reader::json`, for the raw view.
    pub manifest_json: Option<Value>,
    /// Set when the file carries a manifest store that could not be read at all.
    pub manifest_error: Option<String>,
}

/// Active manifest, condensed for display.
#[derive(Serialize, Debug)]
pub struct ManifestSummary {
    pub label: String,
    pub title: Option<String>,
    pub claim_generator: Option<String>,
    pub manifest_count: usize,
    /// c2pa's `Trusted`, `Valid` or `Invalid`; `Pending` while a video at a glance waits for the
    /// check that hashes it.
    pub validation_state: String,
    /// Why the manifest is invalid; set exactly when `validation_state` is `Invalid`.
    pub invalid_reason: Option<InvalidReason>,
    /// URI of the trust list the active manifest's signer chains to; `None` when untrusted.
    pub trust_list: Option<String>,
    pub validation: Value,
    /// True when the file has a source view (IEP-0020): the file without the byte ranges the
    /// data hash of an embedded manifest excludes, or the file itself for a sidecar manifest.
    pub source_view: bool,
    /// File name of the sidecar the manifest store was read from; `None` when it is embedded.
    pub sidecar: Option<String>,
    pub signature: Option<SignatureSummary>,
    pub assertions: Vec<AssertionSummary>,
    pub ingredients: Vec<IngredientSummary>,
    pub soft_bindings: Vec<SoftBindingSummary>,
    pub training_mining: Option<Value>,
    /// Claim thumbnail of the active manifest as a data URL, for comparing by eye with the
    /// file; empty when the manifest has none in an image format.
    pub thumbnail: String,
}

/// The failure that makes a manifest invalid, in plain language, so the verdict is never bare.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct InvalidReason {
    /// Plain-language sentence; c2pa's own explanation for codes without one.
    pub text: String,
    /// Validation code of that failure; `None` when c2pa names no failure.
    pub code: Option<String>,
    /// c2pa's explanation of that failure.
    pub explanation: Option<String>,
    /// Further failures that make the manifest invalid.
    pub more: usize,
}

#[derive(Serialize, Debug)]
pub struct SignatureSummary {
    pub alg: Option<String>,
    pub issuer: Option<String>,
    pub common_name: Option<String>,
    pub time: Option<String>,
    pub cert_serial_number: Option<String>,
    pub timestamp: TimestampSummary,
}

/// Timestamp of the claim signature, judged by the validator's `timeStamp.*` codes. The raw
/// signing time is set for untrusted timestamps too, so it never decides the status.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct TimestampSummary {
    /// `trusted`, `untrusted`, `rejected` or `none`.
    pub status: &'static str,
    /// Trusted because v1 claims skip the trust check of the timestamp service.
    pub legacy: bool,
    pub time: Option<String>,
    /// Organisation of the timestamp service's certificate, else its common name.
    pub tsa: Option<String>,
    /// Common name, organisation and issuer of that certificate.
    pub tsa_detail: Option<String>,
    /// Explanation of the code that decided the status.
    pub reason: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct AssertionSummary {
    pub label: String,
    pub data: Value,
}

#[derive(Serialize, Debug)]
pub struct IngredientSummary {
    pub title: Option<String>,
    pub relationship: String,
    pub format: Option<String>,
    /// How the content of an ingredient without Content Credentials was made (URI).
    pub digital_source_type: Option<String>,
    pub validation_state: Option<String>,
}

/// One soft-binding assertion, decoded when it uses the ISCC algorithm.
#[derive(Serialize, Debug)]
pub struct SoftBindingSummary {
    pub alg: Option<String>,
    pub supported: bool,
    pub value_base64: String,
    pub units: Vec<IsccUnit>,
    /// Comparison of each embedded unit against the freshly computed unit of the same kind
    /// (IEP-0020, Soft Binding Verification).
    pub matches: Vec<UnitMatch>,
    pub error: Option<String>,
    /// Informational `bindingMetadata` of the assertion, shared by all its blocks.
    pub metadata: Option<BindingMetadata>,
    /// Whether the file is source-preserving; `None` unless the block decoded as ISCC.
    pub preservation: Option<Preservation>,
}

/// Whether a signed file is source-preserving (IEP-0020, Source Preservation), with the reason
/// when it is not or cannot be told.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Preservation {
    /// The source view equals the source: the Instance-Codes match.
    Preserved,
    /// Not preserved: embedding changed bytes outside the manifest store.
    Changed,
    /// Not preserved: the source already carried a manifest store, which embedding replaced.
    Resigned,
    /// Not verifiable: the hard binding is not a data hash, so there is no source view.
    NoSourceView,
    /// Not verifiable: the soft binding has no Instance-Code.
    NoInstanceCode,
    /// Not verifiable: the claim signature does not validate, or an assertion no longer matches
    /// its hash in the claim, so the signature does not vouch for the soft binding as it is.
    SignatureInvalid,
    /// Not verifiable: the file changed after signing; its hard binding no longer matches.
    FileChanged,
}

/// Informational `bindingMetadata` of a soft-binding assertion (spec 2.3+).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct BindingMetadata {
    pub description: Option<String>,
    pub contact: Option<String>,
    pub informational_url: Option<String>,
}

#[derive(Serialize, Debug)]
pub struct UnitMatch {
    pub embedded: IsccUnit,
    pub computed: Option<String>,
    /// Fraction of equal bits, 1.0 for identical units.
    pub similarity: Option<f64>,
}

/// First `<stem>-signed<ext>`, `<stem>-signed-2<ext>`, ... next to `path` that does not exist,
/// so the default output never silently overwrites an earlier result.
pub fn suggested_output(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let candidate = |n: u32| {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        path.with_file_name(format!("{stem}-signed{suffix}{ext}"))
    };
    (1..)
        .map(candidate)
        .find(|p| !p.exists())
        .expect("some suffix is free")
}

/// Format of the asset at `path`, from its extension; an MP4, MOV or M4V file with sound but no
/// video is audio ([`audio::sound_only`]), so it gets a Content-Code Audio and needs no ffmpeg.
pub fn asset_format(path: &Path) -> Result<&'static Format> {
    let format = formats::by_path(path).ok_or_else(|| anyhow!("unsupported file type"))?;
    Ok(match formats::sound_only(format) {
        Some(audio) if audio::sound_only(path) => audio,
        _ => format,
    })
}

/// What an inspection computes now, and what it leaves pending for a later pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Depth {
    /// Decode and hash a video now and, with `ocr`, recognise the scanned pages of a PDF; false
    /// shows the file at a glance ([`asset::glance`]), a video's Content-Code, Data-Code and
    /// Instance-Code and a scan's Content-Code pending.
    pub decode: bool,
    /// Recognise the scanned pages of a PDF, whose Content-Code then comes from their text; off,
    /// a PDF has iscc-sdk's Content-Code.
    pub ocr: bool,
    pub semantic: Semantic,
    /// The kinds of Semantic-Code `semantic` applies to; a file of another kind is inspected as
    /// with [`Semantic::Never`].
    pub semantic_kinds: SemanticKinds,
}

/// When the Semantic-Code of an image or a text is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Semantic {
    /// Now: the unit, or why there is none (its model is not installed).
    Now,
    /// In a later pass: pending when its model is installed, else why there is none.
    Later,
    /// Not at all, and not mentioned.
    Never,
}

impl Depth {
    /// What shows at once: every unit but the slow ones, which stay pending.
    pub const GLANCE: Depth = Depth {
        decode: false,
        ocr: false,
        semantic: Semantic::Later,
        semantic_kinds: SemanticKinds::ALL,
    };
    /// Every unit.
    pub const FULL: Depth = Depth {
        decode: true,
        ocr: false,
        semantic: Semantic::Now,
        semantic_kinds: SemanticKinds::ALL,
    };
    /// Every unit but the Semantic-Code, which needs the models.
    pub const NO_SEMANTIC: Depth = Depth {
        decode: true,
        ocr: false,
        semantic: Semantic::Never,
        semantic_kinds: SemanticKinds::ALL,
    };

    /// This depth for the Semantic-Codes of `kinds` only.
    pub fn with_semantic_kinds(self, kinds: SemanticKinds) -> Depth {
        Depth {
            semantic_kinds: kinds,
            ..self
        }
    }

    /// This depth with OCR of scanned PDF pages switched `on` or off.
    pub fn with_ocr(self, on: bool) -> Depth {
        Depth { ocr: on, ..self }
    }

    /// When the Semantic-Code of a file of `kind` is computed: never for a kind left out.
    fn semantic_for(self, kind: Kind) -> Semantic {
        if self.semantic_kinds.includes(kind) {
            self.semantic
        } else {
            Semantic::Never
        }
    }
}

/// Hears how far a slow unit got: `content` while a video decodes or scanned pages are
/// recognised, `semantic` while a text is embedded, with the share done (`None` when unknown);
/// false stops the inspection.
pub type Progress<'a> = &'a dyn Fn(&'static str, Option<f64>) -> bool;

/// Inspect the file at `path`, every unit but the Semantic-Code. The units in `iscc` are those
/// of the whole file; see [`summarize_assertion`] for what soft bindings are compared with.
pub fn inspect(path: &Path) -> Result<Inspection> {
    inspect_with(path, Depth::NO_SEMANTIC, &|_, _| true)
}

/// [`inspect`] to `depth`, with `progress` following the slow units.
pub fn inspect_with(path: &Path, depth: Depth, progress: Progress) -> Result<Inspection> {
    let format = asset_format(path)?;
    let loaded = if depth.decode {
        asset::load(path, format, depth.ocr, &|f| progress("content", f))?
    } else {
        asset::glance(path, format, depth.ocr)?
    };
    inspect_loaded(path, format, loaded, depth, progress)
}

/// The units of a file, the reasons for those it lacks and the ones still to come.
struct FileUnits {
    units: Vec<IsccUnit>,
    meta_error: Option<String>,
    semantic_error: Option<String>,
    content_error: Option<String>,
    pending: Vec<&'static str>,
}

/// The units of `loaded`, its Semantic-Code as `semantic` says. An unusable title or ISCC
/// metadata costs only the Meta-Code, audio too short for a fingerprint only the Content-Code, a
/// missing model only the Semantic-Code, not the inspection. Unread tags give no Meta-Code: one
/// from the fallbacks would differ from the file's own.
fn file_units(
    loaded: &Loaded,
    kind: Kind,
    meta_fields: &MetaFields,
    semantic: Semantic,
    progress: Progress,
) -> Result<FileUnits> {
    let asset = &loaded.asset;
    let (meta, meta_error) = match &asset.content {
        AssetContent::Unread(reason) => (None, Some((*reason).to_owned())),
        _ => unit_or_error(iscc::meta_unit(meta_input(meta_fields))),
    };
    let semantic = semantic_unit(kind, asset, semantic, progress)?;
    let undecoded = asset.content_pending();
    let (content, content_error) = if undecoded {
        (None, None)
    } else {
        unit_or_error(iscc::content_unit(asset.content()))
    };
    let mut pending = Vec::new();
    if semantic == Slow::Pending {
        pending.push("semantic");
    }
    if undecoded {
        pending.push("content");
    }
    if loaded.bitstream.is_none() {
        pending.extend(["data", "instance"]);
    }
    let (semantic, semantic_error) = semantic.split();
    let units = meta
        .into_iter()
        .chain(semantic)
        .chain(content)
        .chain(loaded.bitstream.clone().into_iter().flatten())
        .collect();
    Ok(FileUnits {
        units,
        meta_error,
        semantic_error,
        content_error,
        pending,
    })
}

/// A unit that may take long: computed, left for later, failed with a reason, or not one this
/// file has.
#[derive(Debug, PartialEq)]
enum Slow {
    Done(IsccUnit),
    Pending,
    Failed(String),
    Absent,
}

impl Slow {
    /// The unit and the reason it failed.
    fn split(self) -> (Option<IsccUnit>, Option<String>) {
        match self {
            Slow::Done(unit) => (Some(unit), None),
            Slow::Failed(reason) => (None, Some(reason)),
            Slow::Pending | Slow::Absent => (None, None),
        }
    }
}

/// The Semantic-Code of an image or a text as `when` asks; audio and video have none. A
/// document without text has none for the reason it has no Content-Code; one whose text is still
/// to come (a scan not recognised yet) leaves it for later too. Only a cancelled run fails the
/// inspection.
fn semantic_unit(kind: Kind, asset: &Asset, when: Semantic, progress: Progress) -> Result<Slow> {
    let Some(semantic_kind) = SemanticKind::of(kind).filter(|_| when != Semantic::Never) else {
        return Ok(Slow::Absent);
    };
    let content = asset.content();
    let pending = asset.content_pending();
    if let (Content::Unavailable(reason), false) = (content, pending) {
        return Ok(Slow::Failed(reason.to_owned()));
    }
    if when == Semantic::Later || pending {
        return Ok(match tools::semantic_paths(semantic_kind) {
            Ok(_) => Slow::Pending,
            Err(e) => Slow::Failed(e.to_string()),
        });
    }
    match semantic::unit(content, &|f| progress("semantic", f)) {
        Ok(unit) => Ok(Slow::Done(unit)),
        Err(e) if e.downcast_ref::<Cancelled>().is_some() => Err(e),
        Err(e) => Ok(Slow::Failed(e.to_string())),
    }
}

/// The manifest store of the file at `path`, or why it could not be read; `(None, None)` when
/// the file has none. `check` validates it, which hashes the whole file for its hard binding.
fn manifest_store(path: &Path, check: bool) -> (Option<Reader>, Option<String>) {
    match read_manifest(path, check) {
        Ok(reader) => (Some(reader), None),
        Err(c2pa::Error::JumbfNotFound) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// [`inspect`] to `depth` of the file at `path`, of `format`, loaded as `loaded`.
pub fn inspect_loaded(
    path: &Path,
    format: &'static Format,
    loaded: Loaded,
    depth: Depth,
    progress: Progress,
) -> Result<Inspection> {
    let asset = &loaded.asset;
    // An unhashed file (a video at a glance) has no source view yet, and its manifest store is
    // checked with its hashes: c2pa reads the whole file for the hard binding.
    let hashed = loaded.bitstream.is_some();
    let (reader, manifest_error) = manifest_store(path, hashed);
    let json = reader
        .as_ref()
        .map(|r| serde_json::from_str::<Value>(&r.json()).unwrap_or(Value::Null));
    let manifest_meta = ManifestMeta {
        stored: reader
            .as_ref()
            .zip(json.as_ref())
            .and_then(|(r, j)| stored_meta(j, r.active_label()?)),
        title: reader
            .as_ref()
            .and_then(Reader::active_manifest)
            .and_then(Manifest::title),
    };
    let meta_fields = metadata::meta_fields(asset.metadata.clone(), manifest_meta, path);
    // A kind of Semantic-Code left out is never mentioned: no unit, nothing pending, no reason,
    // and nothing to compare an embedded one with.
    let semantic = depth.semantic_for(format.kind);
    let file = file_units(&loaded, format.kind, &meta_fields, semantic, progress)?;
    let units = &file.units;
    let view = reader
        .as_ref()
        .filter(|_| hashed)
        .and_then(source_view_ranges)
        .map(|ranges| pdf_view_ranges(path, format, ranges))
        .map(|ranges| {
            if ranges.is_empty() {
                // Nothing to cut: the source view is the file itself.
                Ok(SourceView::of_file(path, format, units))
            } else {
                SourceView::new(
                    path,
                    format,
                    ranges,
                    units,
                    semantic,
                    depth.decode && depth.ocr,
                )
            }
        })
        .transpose()?;
    let compared = match &view {
        Some(view) => with_bitstream_of(units, view.units()),
        None => units.clone(),
    };
    let manifest = reader
        .as_ref()
        .zip(json.as_ref())
        .map(|(r, j)| summarize(r, j, path, &compared, view.as_ref(), hashed));
    let (width, height) = match &asset.content {
        AssetContent::Image(rgb) => rgb.dimensions(),
        AssetContent::Video(video) | AssetContent::Undecoded(video) => (video.width, video.height),
        _ => (0, 0),
    };

    Ok(Inspection {
        path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        suggested_output: suggested_output(path).to_string_lossy().into_owned(),
        mime: format.mime,
        format_label: format.label,
        kind: format.kind,
        size_bytes: loaded.size,
        width,
        height,
        preview: asset
            .picture()
            .map(preview_data_url)
            .transpose()?
            .unwrap_or_default(),
        characters: match &asset.content {
            AssetContent::Text(text) => Some(text.chars().count()),
            _ => None,
        },
        duration_secs: match &asset.content {
            AssetContent::Audio(audio) => Some(audio.seconds),
            AssetContent::Video(video) | AssetContent::Undecoded(video) => video.seconds,
            _ => None,
        },
        creator: asset.metadata.creator.clone(),
        iscc: file.units,
        pending: file.pending,
        meta_fields,
        meta_error: file.meta_error,
        semantic_error: file.semantic_error,
        content_error: file.content_error,
        content_from_source: matches!(&asset.content, AssetContent::Video(v) if v.from_source),
        ocr: asset.ocr,
        sign_block: asset.sign_block,
        sign_warning: asset.sign_warning,
        manifest,
        manifest_json: json,
        manifest_error,
    })
}

/// A unit, or the reason it could not be computed.
fn unit_or_error(unit: Result<IsccUnit>) -> (Option<IsccUnit>, Option<String>) {
    match unit {
        Ok(unit) => (Some(unit), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

/// Whether an assertion label names `base`, possibly with an instance suffix (`__2`).
fn is_label(label: &str, base: &str) -> bool {
    label
        .strip_prefix(base)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("__"))
}

/// `dc:title` and `dc:description` of the first `cawg.metadata` assertion of the manifest
/// `label` in the Reader JSON, as written at signing; `None` without a string title.
fn stored_meta<'a>(json: &'a Value, label: &str) -> Option<(&'a str, Option<&'a str>)> {
    let data = json
        .get("manifests")?
        .get(label)?
        .get("assertions")?
        .as_array()?
        .iter()
        .find(|a| {
            a.get("label")
                .and_then(Value::as_str)
                .is_some_and(|l| is_label(l, labels::CAWG_METADATA))
        })?
        .get("data")?;
    let title = data.get("dc:title")?.as_str()?;
    Some((title, data.get("dc:description").and_then(Value::as_str)))
}

/// Meta-Code inputs of the resolved fields.
pub fn meta_input(fields: &MetaFields) -> MetaInput<'_> {
    MetaInput {
        name: Some(&fields.name),
        description: fields.description.as_deref(),
        meta: fields.meta.as_deref(),
    }
}

/// Byte ranges `(start, length)` that separate the file from its Content Credentials, for the
/// source view (IEP-0020): the data hash exclusions of an embedded manifest store. A store read
/// from a sidecar is not in the file, so nothing is cut, whatever its hard binding: exclusions
/// made for a copy with the store embedded would cut real content from this file. `None` when an
/// embedded store's hard binding is not a data hash.
fn source_view_ranges(reader: &Reader) -> Option<Vec<(u64, u64)>> {
    if reader.is_embedded() {
        data_hash_exclusions(reader)
    } else {
        Some(Vec::new())
    }
}

/// The ranges cut from a file for its source view, given its exclusion ranges `ranges`: for a
/// PDF whose manifest store came in an update section that adds nothing else, everything from
/// that update section on, so that the source view is the PDF before it (IEP-0020); else `ranges`.
fn pdf_view_ranges(path: &Path, format: &Format, ranges: Vec<(u64, u64)>) -> Vec<(u64, u64)> {
    let [(start, _)] = ranges[..] else {
        return ranges;
    };
    if format.mime != formats::PDF {
        return ranges;
    }
    let end = usize::try_from(start)
        .ok()
        .zip(std::fs::read(path).ok())
        .and_then(|(start, bytes)| pdf_update::source_end(&bytes, start));
    match end {
        Some(end) => vec![(end as u64, u64::MAX - end as u64)],
        None => ranges,
    }
}

/// File name of the sidecar the manifest store was read from: c2pa-rs loads `<stem>.c2pa` next
/// to a file that embeds none. `None` for an embedded store.
fn sidecar_name(reader: &Reader, path: &Path) -> Option<String> {
    if reader.is_embedded() {
        return None;
    }
    let sidecar = path.with_extension("c2pa");
    sidecar
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Byte ranges `(start, length)` that the data hash of the active manifest excludes; `None`
/// when its hard binding is not a data hash (collection, BMFF and box hashes define no source
/// view).
pub(crate) fn data_hash_exclusions(reader: &Reader) -> Option<Vec<(u64, u64)>> {
    let json: Value = serde_json::from_str(&reader.detailed_json()).ok()?;
    let store = json
        .get("manifests")?
        .get(reader.active_label()?)?
        .get("assertion_store")?
        .as_object()?;
    let (_, data_hash) = store
        .iter()
        .find(|(label, _)| is_label(label, labels::DATA_HASH))?;
    let ranges = data_hash
        .get("exclusions")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|r| Some((r.get("start")?.as_u64()?, r.get("length")?.as_u64()?)))
                .collect()
        })
        .unwrap_or_default();
    Some(ranges)
}

/// The parts of a file of `len` bytes that its source view (IEP-0020) keeps, as `(start, end)`
/// pairs in order: everything but the `(start, length)` exclusion ranges, which may come in any
/// order, overlap or reach past the end.
pub fn kept_ranges(len: u64, exclusions: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut ranges = exclusions.to_vec();
    ranges.sort_unstable();
    let mut kept = Vec::new();
    let mut pos = 0;
    for (start, length) in ranges {
        let (start, end) = (start.min(len), start.saturating_add(length).min(len));
        if start > pos {
            kept.push((pos, start));
        }
        pos = pos.max(end);
    }
    if pos < len {
        kept.push((pos, len));
    }
    kept
}

/// The source view of a file as a reader: the file's bytes within `kept` ranges, in order.
struct ViewReader {
    file: File,
    kept: Vec<(u64, u64)>,
    /// Index of the range being read, and the position in the file.
    index: usize,
    pos: u64,
}

impl ViewReader {
    /// The source view of the file at `path` without the `(start, length)` exclusion ranges.
    fn open(path: &Path, exclusions: &[(u64, u64)]) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
        let kept = kept_ranges(file.metadata()?.len(), exclusions);
        Ok(ViewReader {
            file,
            kept,
            index: 0,
            pos: 0,
        })
    }
}

impl Read for ViewReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while let Some(&(start, end)) = self.kept.get(self.index) {
            if self.pos >= end {
                self.index += 1;
                continue;
            }
            if self.pos < start {
                self.pos = self.file.seek(SeekFrom::Start(start))?;
            }
            let want = usize::try_from(end - self.pos).map_or(buf.len(), |n| n.min(buf.len()));
            let n = self.file.read(&mut buf[..want])?;
            if n == 0 {
                // The file got shorter since its length was taken.
                return Ok(0);
            }
            self.pos += n as u64;
            return Ok(n);
        }
        Ok(0)
    }
}

/// `units` with the Data-Code and Instance-Code of `view` in place of their own.
fn with_bitstream_of(units: &[IsccUnit], view: &[IsccUnit]) -> Vec<IsccUnit> {
    let bitstream = |u: &&IsccUnit| u.unit == "data" || u.unit == "instance";
    let content = units.iter().filter(|u| !bitstream(u));
    content
        .chain(view.iter().filter(bitstream))
        .cloned()
        .collect()
}

/// The source view (IEP-0020) with its units. The Meta-Code is the file's, since its inputs
/// never lie inside the manifest store. The view is streamed from the file, never held whole for
/// its Data-Code and Instance-Code. The Content-Code and the Semantic-Code are computed on first
/// use, because they only matter once the view is proven to be the source: the view of a file
/// that is not source-preserving need not decode (then they are left out), and decoding it costs
/// as much as the file did (a signed PDF would run pdfium a second time, a video ffmpeg). The
/// Semantic-Code of a view that decodes like the file comes from the memo of `semantic`.
struct SourceView<'a> {
    path: &'a Path,
    format: &'static Format,
    /// `(start, length)` ranges cut from the file.
    exclusions: Vec<(u64, u64)>,
    /// Meta-Code of the file, Data-Code and Instance-Code of the view.
    units: Vec<IsccUnit>,
    /// Whether the Semantic-Code is computed now, as for the file.
    semantic: Semantic,
    /// Whether the scanned pages of a PDF are recognised now, as for the file: a signed scan is
    /// source-preserving, and its view's text then comes from OCR too.
    ocr: bool,
    /// Content-Code and Semantic-Code of the decoded view.
    decoded: OnceCell<Vec<IsccUnit>>,
}

impl<'a> SourceView<'a> {
    /// The view of the file at `path` without the `(start, length)` ranges in `exclusions`; the
    /// file's units are `file_units`.
    fn new(
        path: &'a Path,
        format: &'static Format,
        exclusions: Vec<(u64, u64)>,
        file_units: &[IsccUnit],
        semantic: Semantic,
        ocr: bool,
    ) -> Result<Self> {
        let meta = file_units.iter().find(|u| u.unit == "meta").cloned();
        let (bitstream, _) = iscc::stream_units(ViewReader::open(path, &exclusions)?)?;
        Ok(SourceView {
            path,
            format,
            exclusions,
            units: meta.into_iter().chain(bitstream).collect(),
            semantic,
            ocr,
            decoded: OnceCell::new(),
        })
    }

    /// The file itself as its source view (a sidecar manifest, or a data hash that excludes
    /// nothing): every unit is known already.
    fn of_file(path: &'a Path, format: &'static Format, file_units: &[IsccUnit]) -> Self {
        let (decoded, units): (Vec<IsccUnit>, Vec<IsccUnit>) = file_units
            .iter()
            .cloned()
            .partition(|u| u.unit == "content" || u.unit == "semantic");
        SourceView {
            path,
            format,
            exclusions: Vec::new(),
            units,
            semantic: Semantic::Never,
            ocr: false,
            decoded: OnceCell::from(decoded),
        }
    }

    /// Content-Code and, when computed now, Semantic-Code of the view, where the view decodes.
    fn decoded_units(&self) -> Vec<IsccUnit> {
        let mut bytes = Vec::new();
        let read = ViewReader::open(self.path, &self.exclusions)
            .and_then(|mut view| Ok(view.read_to_end(&mut bytes)?));
        let Some(asset) = read
            .ok()
            .and_then(|_| asset::read(self.path, &bytes, self.format, self.ocr).ok())
        else {
            return Vec::new();
        };
        let content = iscc::content_unit(asset.content()).ok();
        let semantic = (self.semantic == Semantic::Now)
            .then(|| semantic::unit(asset.content(), &|_| true).ok())
            .flatten();
        content.into_iter().chain(semantic).collect()
    }

    /// Meta-Code of the file, Data-Code and Instance-Code of the view.
    fn units(&self) -> &[IsccUnit] {
        &self.units
    }

    /// Every unit, the view's Content-Code and Semantic-Code included where the view decodes.
    fn all_units(&self) -> Vec<IsccUnit> {
        let decoded = self.decoded.get_or_init(|| self.decoded_units());
        self.units.iter().chain(decoded).cloned().collect()
    }
}

/// Open the manifest store with the shared trust settings.
/// The manifest store of the file at `path`, validated when `check` is set.
fn read_manifest(path: &Path, check: bool) -> c2pa::Result<Reader> {
    let mut settings = base_settings();
    settings["verify"]["verify_after_reading"] = Value::Bool(check);
    let context = Context::new().with_settings(settings)?;
    Reader::from_context(context).with_file(path)
}

/// Down-scale the flattened image and encode it as a JPEG data URL.
fn preview_data_url(rgb: &image::RgbImage) -> Result<String> {
    let jpeg = thumbnail::scaled_jpeg(rgb, thumbnail::PREVIEW_EDGE, PREVIEW_QUALITY)?;
    Ok(format!("data:image/jpeg;base64,{}", B64.encode(jpeg)))
}

/// Build the display summary of the active manifest of the file at `path`. `view` is the
/// source view, when there is one (see [`source_view_ranges`]); see [`summarize_assertion`] for
/// `compared`. `hashed` is false while the file's Data-Code and Instance-Code are pending, which
/// leaves its source view, the preservation verdict and the validation for later: the store was
/// read unchecked, and its validation state is `Pending`.
fn summarize(
    reader: &Reader,
    json: &Value,
    path: &Path,
    compared: &[IsccUnit],
    view: Option<&SourceView>,
    hashed: bool,
) -> ManifestSummary {
    let label = reader.active_label().unwrap_or_default().to_owned();
    let manifest = reader.active_manifest();
    let assertions: Vec<AssertionSummary> = json
        .get("manifests")
        .and_then(|m| m.get(&label))
        .and_then(|m| m.get("assertions"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .map(|a| AssertionSummary {
                    label: a
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    data: a.get("data").cloned().unwrap_or(Value::Null),
                })
                .collect()
        })
        .unwrap_or_default();
    let claim_generator = manifest
        .and_then(|m| m.claim_generator().map(str::to_owned))
        .or_else(|| claim_generator_from_json(json, &label));
    let training_mining = assertions
        .iter()
        .find(|a| is_label(&a.label, TRAINING_MINING_LABEL))
        .map(|a| a.data.clone());

    ManifestSummary {
        label,
        title: manifest.and_then(|m| m.title().map(str::to_owned)),
        claim_generator,
        manifest_count: reader.manifests().len(),
        validation_state: if hashed {
            format!("{:?}", reader.validation_state())
        } else {
            "Pending".to_owned()
        },
        invalid_reason: invalid_reason(reader).filter(|_| hashed),
        trust_list: signer_trust_list(reader),
        validation: serde_json::to_value(reader.validation_results()).unwrap_or(Value::Null),
        source_view: view.is_some() || (!hashed && source_view_ranges(reader).is_some()),
        sidecar: sidecar_name(reader, path),
        signature: manifest
            .and_then(Manifest::signature_info)
            .map(|s| SignatureSummary {
                alg: s.alg.map(|a| a.to_string()),
                issuer: s.issuer.clone(),
                common_name: s.common_name.clone(),
                time: s.time.clone(),
                cert_serial_number: s.cert_serial_number.clone(),
                timestamp: timestamp_summary(reader, s.time.clone()),
            }),
        ingredients: manifest
            .map(|m| {
                m.ingredients()
                    .iter()
                    .map(|i| IngredientSummary {
                        title: i.title().map(str::to_owned),
                        relationship: format!("{:?}", i.relationship()),
                        format: i.format().map(str::to_owned),
                        digital_source_type: i
                            .digital_source_type()
                            .and_then(|d| serde_json::to_value(d).ok())
                            .and_then(|v| v.as_str().map(str::to_owned)),
                        validation_state: i
                            .validation_results()
                            .map(|r| format!("{:?}", r.validation_state())),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        soft_bindings: manifest
            .map(|m| {
                let facts = binding_facts(reader, m, view.is_some());
                soft_bindings(m, compared, view, hashed.then_some(&facts))
            })
            .unwrap_or_default(),
        training_mining,
        thumbnail: manifest.map(thumbnail_data_url).unwrap_or_default(),
        assertions,
    }
}

/// Claim thumbnail of `manifest` as a data URL; empty without one.
fn thumbnail_data_url(manifest: &Manifest) -> String {
    manifest
        .thumbnail()
        .and_then(|(format, bytes)| image_data_url(format, &bytes))
        .unwrap_or_default()
}

/// `bytes` as a data URL of the media type `format`, which comes from the manifest and must be
/// a plain `image/*` type to end up in the URL.
fn image_data_url(format: &str, bytes: &[u8]) -> Option<String> {
    let subtype = format.strip_prefix("image/")?;
    let plain = !subtype.is_empty()
        && subtype
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'));
    plain.then(|| format!("data:{format};base64,{}", B64.encode(bytes)))
}

/// A failure reported by the validator: code, explanation, and whether it belongs to an
/// ingredient's manifest rather than the active one.
type Failure<'a> = (&'a str, Option<&'a str>, bool);

/// Trust list of the active manifest's signer, from its own `signingCredential.trusted` status.
/// `ValidationResults::trust_list_uri` is not used: c2pa-rs fills it from whichever manifest's
/// signer it logged last, ingredients included, in no stable order.
pub(crate) fn signer_trust_list(reader: &Reader) -> Option<String> {
    let signature = format!("self#jumbf=/c2pa/{}/c2pa.signature", reader.active_label()?);
    reader
        .validation_results()?
        .active_manifest()?
        .success()
        .iter()
        .find(|s| s.code() == "signingCredential.trusted" && s.url() == Some(signature.as_str()))
        .and_then(|s| s.trust_list_uri())
        .map(str::to_owned)
}

/// Reason of an invalid manifest; `None` for any other validation state.
fn invalid_reason(reader: &Reader) -> Option<InvalidReason> {
    if reader.validation_state() != ValidationState::Invalid {
        return None;
    }
    let results = reader.validation_results();
    let active = results
        .and_then(|r| r.active_manifest())
        .into_iter()
        .flat_map(|c| c.failure())
        .map(|v| (v.code(), v.explanation(), false));
    let ingredients = results
        .and_then(|r| r.ingredient_deltas())
        .into_iter()
        .flatten()
        .flat_map(|d| d.validation_deltas().failure())
        .map(|v| (v.code(), v.explanation(), true));
    let failures: Vec<Failure> = active.chain(ingredients).collect();
    let mut reason = reason_for(&failures);
    if !reader.is_embedded() && reason.text == FILE_CHANGED {
        reason.text = SIDECAR_MISMATCH.to_owned();
    }
    Some(reason)
}

/// Failures c2pa does not count against the manifest (`is_tolerated_manifest_failure_code`
/// in c2pa-rs): an untrusted signer and CAWG identity assertion failures.
fn tolerated(code: &str) -> bool {
    code == "signingCredential.untrusted"
        || code.starts_with("cawg.x509.")
        || code.starts_with("cawg.identity.")
}

/// Reason from the failures in validator order: the first one that counts leads, the others
/// are counted. Without such a failure, c2pa found the claim signature unconfirmed.
fn reason_for(failures: &[Failure]) -> InvalidReason {
    let counted: Vec<&Failure> = failures.iter().filter(|f| !tolerated(f.0)).collect();
    let Some(&&(code, explanation, ingredient)) = counted.first() else {
        return InvalidReason {
            text: "The signature could not be confirmed.".to_owned(),
            code: None,
            explanation: None,
            more: 0,
        };
    };
    let plain = if ingredient {
        Some(INGREDIENT_INVALID)
    } else {
        plain_reason(code)
    };
    let text = plain
        .map(str::to_owned)
        .or_else(|| explanation.filter(|e| !e.trim().is_empty()).map(sentence))
        .unwrap_or_else(|| "Validation failed.".to_owned());
    InvalidReason {
        text,
        code: Some(code.to_owned()),
        explanation: explanation.map(str::to_owned),
        more: counted.len() - 1,
    }
}

const INGREDIENT_INVALID: &str =
    "A source this file was made from (an ingredient) does not validate.";

const FILE_CHANGED: &str = "The file changed after signing: its hash no longer matches.";

/// Hard-binding mismatch of a sidecar manifest, which also describes an unchanged file: a sidecar
/// extracted from a copy that embeds it never matches the file without it.
const SIDECAR_MISMATCH: &str = "The hash in the sidecar does not match this file.";

/// Plain-language sentence for the failure codes of c2pa-rs 0.91 a reader can act on.
fn plain_reason(code: &str) -> Option<&'static str> {
    Some(match code {
        c if HARD_BINDING_MISMATCH.contains(&c) => FILE_CHANGED,
        "claim.hardBindings.missing"
        | "assertion.multipleHardBindings"
        | "assertion.dataHash.malformed"
        | "assertion.bmffHash.malformed"
        | "assertion.boxesHash.malformed"
        | "assertion.collectionHash.malformed"
        | "assertion.collectionHash.invalidURI"
        | "assertion.dataHash.redacted"
        | "assertion.hardBinding.redacted" => {
            "The manifest has no usable hash of the file, so the file cannot be checked against it."
        }
        "claimSignature.mismatch" => {
            "The signature does not match the manifest: the manifest changed after signing."
        }
        "claimSignature.missing" => "The manifest carries no signature.",
        "claimSignature.outsideValidity" => {
            "The file was signed outside the validity period of the signing certificate."
        }
        "signingCredential.expired" => "The signing certificate has expired.",
        "signingCredential.invalid" => {
            "The signing certificate does not meet the C2PA certificate requirements."
        }
        "signingCredential.ocsp.revoked" => "The signing certificate has been revoked.",
        "assertion.hashedURI.mismatch" | "hashedURI.mismatch" => {
            "Part of the manifest changed after signing."
        }
        "assertion.missing"
        | "assertion.inaccessible"
        | "assertion.required.missing"
        | "claim.required.missing"
        | "hashedURI.missing" => "Part of the manifest that the signature covers is missing.",
        "assertion.undeclared" => "The manifest holds a part the signature does not cover.",
        "claim.missing"
        | "claim.multiple"
        | "claim.malformed"
        | "claim.cbor.invalid"
        | "assertion.json.invalid"
        | "assertion.cbor.invalid"
        | "manifest.compressed.invalid" => "The manifest is malformed.",
        "timeStamp.mismatch" | "timeStamp.malformed" | "timeStamp.outsideValidity" => {
            "The timestamp is invalid or does not belong to this signature."
        }
        "algorithm.unsupported" => "The manifest uses an algorithm this app cannot check.",
        "manifest.inaccessible" => "A manifest this file refers to could not be found.",
        c if c.starts_with("assertion.action.") => {
            "The recorded edit history (actions) is inconsistent."
        }
        c if c.starts_with("ingredient.") => INGREDIENT_INVALID,
        _ => return None,
    })
}

/// c2pa's explanation as a sentence: capital first letter, closing full stop.
fn sentence(explanation: &str) -> String {
    let mut chars = explanation.trim().chars();
    let first: String = chars
        .next()
        .map(char::to_uppercase)
        .into_iter()
        .flatten()
        .collect();
    let text = format!("{first}{}", chars.as_str());
    if text.ends_with(['.', '!', '?', ')']) {
        text
    } else {
        format!("{text}.")
    }
}

/// Codes that mean the timestamp token is invalid or does not belong to the signature.
const TIMESTAMP_REJECTED: [&str; 3] = [
    "timeStamp.mismatch",
    "timeStamp.malformed",
    "timeStamp.outsideValidity",
];

/// Timestamp status of the active manifest's signature, whose raw time is `time`.
fn timestamp_summary(reader: &Reader, time: Option<String>) -> TimestampSummary {
    let codes: Vec<(&str, &str)> = reader
        .validation_results()
        .and_then(|r| r.active_manifest())
        .map(|c| {
            c.success()
                .iter()
                .chain(c.informational())
                .chain(c.failure())
                .map(|v| (v.code(), v.explanation().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    let (status, legacy, reason) = timestamp_status(&codes, time.is_some());
    let (tsa, tsa_detail) = if status == "none" {
        (None, None)
    } else {
        tsa_names(reader)
    };
    TimestampSummary {
        status,
        legacy,
        time,
        tsa,
        tsa_detail,
        reason: reason.filter(|r| !r.is_empty()).map(str::to_owned),
    }
}

/// Status, legacy flag and deciding explanation from `(code, explanation)` pairs: a token that
/// is invalid or does not fit is rejected, then the trust verdict counts; a time without any
/// verdict is treated as untrusted. c2pa-rs also reports a token it cannot check (broken
/// signature, no certificate, unknown digest) as `timeStamp.untrusted`; only the explanation
/// "timestamp cert untrusted" means a valid token from an untrusted service.
fn timestamp_status<'a>(
    codes: &[(&str, &'a str)],
    has_time: bool,
) -> (&'static str, bool, Option<&'a str>) {
    let find = |wanted: &[&str]| {
        codes
            .iter()
            .find(|(code, _)| wanted.contains(code))
            .map(|(_, explanation)| *explanation)
    };
    if let Some(e) = find(&TIMESTAMP_REJECTED) {
        return ("rejected", false, Some(e));
    }
    if let Some(e) = find(&["timeStamp.trusted"]) {
        return ("trusted", e.starts_with("legacy"), Some(e));
    }
    if let Some(e) = find(&["timeStamp.untrusted"]) {
        let valid = e.starts_with("timestamp cert untrusted");
        return (if valid { "untrusted" } else { "rejected" }, false, Some(e));
    }
    (if has_time { "untrusted" } else { "none" }, false, None)
}

/// Display name and detail line of the active manifest's timestamp certificate (crJSON
/// `signature.timeStampInfo`, present whatever the trust verdict).
fn tsa_names(reader: &Reader) -> (Option<String>, Option<String>) {
    let Ok(crjson) = reader.to_crjson_value() else {
        return (None, None);
    };
    let label = reader.active_label();
    let Some(cert) = crjson["manifests"]
        .as_array()
        .and_then(|list| list.iter().find(|m| m["label"].as_str() == label))
        .map(|m| &m["signature"]["timeStampInfo"]["certificateInfo"])
    else {
        return (None, None);
    };
    let subject = |key: &str| cert["subject"][key].as_str();
    let issuer = cert["issuer"]["CN"]
        .as_str()
        .map(|i| format!("issued by {i}"));
    let detail: Vec<String> = [subject("CN"), subject("O")]
        .into_iter()
        .flatten()
        .map(str::to_owned)
        .chain(issuer)
        .collect();
    (
        subject("O").or(subject("CN")).map(str::to_owned),
        (!detail.is_empty()).then(|| detail.join(", ")),
    )
}

/// Claim generator of a v2 claim: first entry of `claim_generator_info` as "name version".
fn claim_generator_from_json(json: &Value, label: &str) -> Option<String> {
    let info = json
        .get("manifests")?
        .get(label)?
        .get("claim_generator_info")?
        .as_array()?
        .first()?;
    let name = info.get("name")?.as_str()?;
    Some(match info.get("version").and_then(Value::as_str) {
        Some(version) => format!("{name} {version}"),
        None => name.to_owned(),
    })
}

/// Decode every soft-binding assertion of a manifest; `facts` is `None` while the file is not
/// hashed yet, which leaves the preservation verdict open.
fn soft_bindings(
    manifest: &Manifest,
    compared: &[IsccUnit],
    view: Option<&SourceView>,
    facts: Option<&BindingFacts>,
) -> Vec<SoftBindingSummary> {
    manifest
        .assertions()
        .iter()
        .filter(|a| is_label(a.label(), labels::SOFT_BINDING))
        .filter_map(|a| a.to_assertion::<SoftBinding>().ok())
        .flat_map(|sb| summarize_assertion(&sb, compared, view, facts))
        .collect()
}

/// One summary per block of a soft-binding assertion; the metadata belongs to the assertion and
/// is repeated on each block. IEP-0020, Soft Binding Verification: when the file is
/// source-preserving, the embedded units are compared with those of `view`, because the source
/// view then is the source. Otherwise they are compared with `compared`: Meta-Code and Content-Code
/// of the file, which always decodes, with Data-Code and Instance-Code of the source view where
/// there is one, so that the manifest store does not count as a change. Without `facts` (the
/// file not hashed yet) there is no verdict, and the units known so far are compared.
fn summarize_assertion(
    sb: &SoftBinding,
    compared: &[IsccUnit],
    view: Option<&SourceView>,
    facts: Option<&BindingFacts>,
) -> Vec<SoftBindingSummary> {
    let metadata = binding_metadata(sb);
    let summarize = |value: &[u8], units: &[IsccUnit]| {
        summarize_soft_binding(sb.alg.clone(), value, units, metadata.clone())
    };
    let Some(facts) = facts else {
        return sb
            .blocks
            .iter()
            .map(|block| summarize(&block.value, compared))
            .collect();
    };
    sb.blocks
        .iter()
        .map(|block| {
            let instance_equal = view.and_then(|v| {
                let on_view = summarize(&block.value, v.units());
                let instance = on_view
                    .matches
                    .iter()
                    .find(|m| m.embedded.unit == "instance")?;
                Some(instance.similarity == Some(1.0))
            });
            let verdict = preservation(facts, instance_equal);
            let mut summary = match (view, verdict) {
                (Some(v), Preservation::Preserved) => summarize(&block.value, &v.all_units()),
                _ => summarize(&block.value, compared),
            };
            if summary.supported && summary.error.is_none() {
                summary.preservation = Some(verdict);
            }
            summary
        })
        .collect()
}

/// What the preservation check needs to know about the active manifest besides the units.
#[derive(Debug, Clone, Copy)]
struct BindingFacts {
    /// There is a source view: an embedded store's hard binding is a data hash, or the store
    /// comes from a sidecar.
    source_view: bool,
    /// The claim signature validates and covers every assertion as it is.
    signature_valid: bool,
    hard_binding_matches: bool,
    /// The parent ingredient carried a manifest store: the source had Content Credentials.
    resigned: bool,
}

/// Codes of a hard binding that no longer matches the file.
const HARD_BINDING_MISMATCH: [&str; 5] = [
    "assertion.dataHash.mismatch",
    "assertion.bmffHash.mismatch",
    "assertion.boxesHash.mismatch",
    "assertion.boxesHash.unknownBox",
    "assertion.collectionHash.mismatch",
];

/// Codes of an assertion that does not match its hashed URI in the claim.
const ASSERTION_MISMATCH: [&str; 3] = [
    "assertion.hashedURI.mismatch",
    "assertion.missing",
    "assertion.inaccessible",
];

/// Facts about the active manifest `manifest` from the validator's failure codes and its
/// ingredients.
fn binding_facts(reader: &Reader, manifest: &Manifest, source_view: bool) -> BindingFacts {
    let failures: Vec<&str> = reader
        .validation_results()
        .and_then(|r| r.active_manifest())
        .map(|c| c.failure().iter().map(|v| v.code()).collect())
        .unwrap_or_default();
    BindingFacts {
        source_view,
        signature_valid: !failures.iter().any(|c| {
            ((c.starts_with("claimSignature.") || c.starts_with("signingCredential."))
                && !tolerated(c))
                || ASSERTION_MISMATCH.contains(c)
        }),
        hard_binding_matches: !failures.iter().any(|c| HARD_BINDING_MISMATCH.contains(c)),
        resigned: manifest
            .ingredients()
            .iter()
            .any(|i| i.is_parent() && i.active_manifest().is_some()),
    }
}

/// The Source Preservation check of IEP-0020, with the reason for every result other than
/// preserved. `instance_equal` tells whether the embedded Instance-Code equals the one of the
/// source view, `None` without an embedded Instance-Code. A hard binding that does not match
/// means the file is not the one the manifest was made for, whatever the Instance-Codes say.
fn preservation(facts: &BindingFacts, instance_equal: Option<bool>) -> Preservation {
    if !facts.signature_valid {
        return Preservation::SignatureInvalid;
    }
    if !facts.hard_binding_matches {
        return Preservation::FileChanged;
    }
    if !facts.source_view {
        return Preservation::NoSourceView;
    }
    match instance_equal {
        None => Preservation::NoInstanceCode,
        Some(true) => Preservation::Preserved,
        Some(false) if facts.resigned => Preservation::Resigned,
        Some(false) => Preservation::Changed,
    }
}

/// Known fields of the assertion's `bindingMetadata`; `None` when absent or none of them is set.
fn binding_metadata(sb: &SoftBinding) -> Option<BindingMetadata> {
    let m = sb.binding_metadata.as_ref()?;
    let filled = |s: &Option<String>| {
        s.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let metadata = BindingMetadata {
        description: filled(&m.description),
        contact: filled(&m.contact),
        informational_url: filled(&m.informational_url),
    };
    (metadata.description.is_some()
        || metadata.contact.is_some()
        || metadata.informational_url.is_some())
    .then_some(metadata)
}

/// Decode one soft-binding block and compare it against the computed units.
fn summarize_soft_binding(
    alg: Option<String>,
    value: &[u8],
    computed: &[IsccUnit],
    metadata: Option<BindingMetadata>,
) -> SoftBindingSummary {
    let supported = alg.as_deref() == Some(ISCC_SOFT_BINDING_ALG);
    let decoded = if supported {
        iscc::decode_seq(value)
    } else {
        Err(anyhow!("algorithm not supported by this app"))
    };
    let (units, error) = match decoded {
        Ok(list) => {
            let units: Vec<IsccUnit> = list
                .iter()
                .filter_map(|u| iscc::describe_unit(u).ok())
                .collect();
            (units, None)
        }
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let matches = units
        .iter()
        .map(|embedded| {
            let peer = computed.iter().find(|c| c.name == embedded.name);
            let similarity =
                peer.and_then(|c| iscc::similarity(&embedded.iscc, &c.iscc).ok().flatten());
            UnitMatch {
                embedded: embedded.clone(),
                computed: peer.map(|c| c.iscc.clone()),
                similarity,
            }
        })
        .collect();
    SoftBindingSummary {
        alg,
        supported,
        value_base64: B64.encode(value),
        units,
        matches,
        error,
        metadata,
        preservation: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::jpeg::JpegEncoder;
    use std::io::Cursor;

    #[test]
    fn timestamp_status_follows_the_codes() {
        let trusted = (
            "timeStamp.trusted",
            "timestamp cert trusted: TSA, trust list: x",
        );
        let legacy = ("timeStamp.trusted", "legacy timestamp cert trusted: TSA");
        let untrusted = ("timeStamp.untrusted", "timestamp cert untrusted: TSA");
        let validated = ("timeStamp.validated", "timestamp message digest matched");
        assert_eq!(
            timestamp_status(&[validated, trusted], true),
            ("trusted", false, Some(trusted.1))
        );
        assert_eq!(timestamp_status(&[legacy], true).0, "trusted");
        assert!(timestamp_status(&[legacy], true).1, "legacy");
        assert_eq!(
            timestamp_status(&[validated, untrusted], true),
            ("untrusted", false, Some(untrusted.1))
        );
        for code in TIMESTAMP_REJECTED {
            let status = timestamp_status(&[trusted, (code, "why")], true);
            assert_eq!(status, ("rejected", false, Some("why")), "{code}");
        }
        let broken = (
            "timeStamp.untrusted",
            "timestamp signed data did not match signature",
        );
        assert_eq!(
            timestamp_status(&[broken], false),
            ("rejected", false, Some(broken.1))
        );
        assert_eq!(timestamp_status(&[], false), ("none", false, None));
        // A time without any verdict is not taken on trust.
        assert_eq!(
            timestamp_status(&[validated], true),
            ("untrusted", false, None)
        );
    }

    #[test]
    fn invalid_reason_leads_with_the_first_counted_failure() {
        let hash = (
            "assertion.dataHash.mismatch",
            Some("hashes do not match"),
            false,
        );
        let untrusted = ("signingCredential.untrusted", Some("not on a list"), false);
        let identity = ("cawg.identity.well-formed", None, false);
        let reason = reason_for(&[untrusted, identity, hash]);
        assert_eq!(
            reason,
            InvalidReason {
                text: "The file changed after signing: its hash no longer matches.".to_owned(),
                code: Some(hash.0.to_owned()),
                explanation: Some("hashes do not match".to_owned()),
                more: 0,
            }
        );
        let signature = ("claimSignature.mismatch", None, false);
        let two = reason_for(&[signature, untrusted, hash]);
        assert_eq!(two.code.as_deref(), Some(signature.0));
        assert!(two.text.starts_with("The signature does not match"));
        assert_eq!(two.more, 1);
    }

    #[test]
    fn invalid_reason_maps_codes_and_falls_back_to_the_explanation() {
        for code in [
            "assertion.bmffHash.mismatch",
            "assertion.boxesHash.mismatch",
            "assertion.collectionHash.mismatch",
        ] {
            assert_eq!(
                plain_reason(code),
                plain_reason("assertion.dataHash.mismatch"),
                "{code}"
            );
        }
        assert!(plain_reason("assertion.action.malformed").is_some());
        assert_eq!(
            plain_reason("ingredient.manifest.mismatch"),
            Some(INGREDIENT_INVALID)
        );
        assert_eq!(plain_reason("general.error"), None);

        let unmapped = reason_for(&[("general.error", Some("something broke"), false)]);
        assert_eq!(unmapped.text, "Something broke.");
        assert_eq!(unmapped.code.as_deref(), Some("general.error"));
        let bare = reason_for(&[("general.error", None, false)]);
        assert_eq!(bare.text, "Validation failed.");

        // An ingredient's own hash failure is not this file's hash failure.
        let delta = reason_for(&[("assertion.dataHash.mismatch", None, true)]);
        assert_eq!(delta.text, INGREDIENT_INVALID);

        let unconfirmed = reason_for(&[("signingCredential.untrusted", None, false)]);
        assert_eq!(unconfirmed.code, None);
        assert_eq!(unconfirmed.text, "The signature could not be confirmed.");
    }

    #[test]
    fn changed_pixels_give_a_hash_mismatch_reason() {
        let signed = inspect(Path::new(&fixture("tsa/encypher.jpg"))).unwrap();
        let manifest = signed.manifest.unwrap();
        assert_ne!(manifest.validation_state, "Invalid");
        assert_eq!(manifest.invalid_reason, None);

        let mut file = std::fs::read(fixture("tsa/encypher.jpg")).unwrap();
        // A byte of entropy-coded data just before the end-of-image marker, off any 0xFF.
        let at = (2..file.len() - 2)
            .rev()
            .find(|&i| file[i - 1] < 0xFE && file[i] < 0xFE && file[i + 1] < 0xFE)
            .unwrap();
        file[at] ^= 1;
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-changed-pixels");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("changed.jpg");
        std::fs::write(&path, &file).unwrap();
        let manifest = inspect(&path).unwrap().manifest.unwrap();
        assert_eq!(manifest.validation_state, "Invalid");
        let reason = manifest
            .invalid_reason
            .expect("an invalid manifest has a reason");
        assert_eq!(reason.code.as_deref(), Some("assertion.dataHash.mismatch"));
        assert_eq!(
            reason.text,
            "The file changed after signing: its hash no longer matches."
        );
    }

    /// Path of a test fixture.
    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn claim_thumbnail_of_any_signer_is_shown() {
        let manifest = inspect(Path::new(&fixture("CA.jpg")))
            .unwrap()
            .manifest
            .unwrap();
        let b64 = manifest
            .thumbnail
            .strip_prefix("data:image/jpeg;base64,")
            .expect("JPEG claim thumbnail");
        let jpeg = B64.decode(b64).unwrap();
        image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
    }

    #[test]
    fn thumbnail_format_must_be_a_plain_image_type() {
        assert_eq!(
            image_data_url("image/svg+xml", b"x").as_deref(),
            Some("data:image/svg+xml;base64,eA==")
        );
        for format in [
            "text/html",
            "image/",
            "image/png;x=\"\"",
            "image/png onerror",
        ] {
            assert_eq!(image_data_url(format, b"x"), None, "{format}");
        }
    }

    /// Timestamp summary of the active manifest of the file at `path`.
    fn timestamp_of(path: &str) -> TimestampSummary {
        inspect(Path::new(path))
            .unwrap()
            .manifest
            .and_then(|m| m.signature)
            .expect("signed file")
            .timestamp
    }

    /// Timestamp summary of a fixture's active manifest.
    fn fixture_timestamp(name: &str) -> TimestampSummary {
        timestamp_of(&fixture(name))
    }

    #[test]
    fn timestamp_states_of_real_files() {
        let encypher = fixture_timestamp("tsa/encypher.jpg");
        assert_eq!((encypher.status, encypher.legacy), ("trusted", false));
        assert_eq!(encypher.tsa.as_deref(), Some("Encypher Corp."));
        assert!(encypher.time.is_some());
        assert!(encypher
            .tsa_detail
            .unwrap()
            .contains("Encypher C2PA TSA Signer"));

        let digicert = fixture_timestamp("tsa/digicert.jpg");
        assert_eq!(digicert.status, "untrusted");
        assert_eq!(digicert.tsa.as_deref(), Some("DigiCert, Inc."));
        assert!(
            digicert.time.is_some(),
            "untrusted timestamps carry a time too"
        );

        // v1 claims: c2pa-rs calls any valid token trusted without checking the trust list.
        let legacy = fixture_timestamp("CA.jpg");
        assert_eq!((legacy.status, legacy.legacy), ("trusted", true));
        assert_eq!(legacy.tsa.as_deref(), Some("DigiCert, Inc."));
    }

    #[test]
    fn timestamp_with_broken_signature_is_rejected() {
        let mut file = std::fs::read(fixture("tsa/encypher.jpg")).unwrap();
        // The token ends with its signer's signature.
        let token = crate::timestamp::tests::token_span(&file);
        file[token.end - 1] ^= 1;
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-broken-timestamp");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.jpg");
        std::fs::write(&path, &file).unwrap();
        let broken = timestamp_of(path.to_str().unwrap());
        assert_eq!(broken.status, "rejected");
        assert_eq!(
            broken.reason.as_deref(),
            Some("timestamp signed data did not match signature")
        );
        assert!(broken.time.is_none());
        assert_eq!(broken.tsa.as_deref(), Some("Encypher Corp."));
    }

    #[test]
    fn suggested_output_skips_existing_siblings() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-suggest");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("photo.jpg");
        std::fs::write(&source, b"x").unwrap();
        for name in ["photo-signed.jpg", "photo-signed-2.jpg"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        assert_eq!(suggested_output(&source), dir.join("photo-signed.jpg"));
        std::fs::write(dir.join("photo-signed.jpg"), b"x").unwrap();
        assert_eq!(suggested_output(&source), dir.join("photo-signed-2.jpg"));
    }

    /// A small JPEG carrying `xmp` as its XMP packet.
    fn jpeg_with_xmp(xmp: &str) -> Vec<u8> {
        let mut jpeg = Cursor::new(Vec::new());
        JpegEncoder::new(&mut jpeg)
            .encode_image(&image::RgbImage::from_pixel(
                16,
                16,
                image::Rgb([120, 30, 60]),
            ))
            .unwrap();
        let jpeg = jpeg.into_inner();
        let payload = [b"http://ns.adobe.com/xap/1.0/\0".as_slice(), xmp.as_bytes()].concat();
        let length = u16::try_from(payload.len() + 2).unwrap().to_be_bytes();
        [&jpeg[..2], &[0xff, 0xe1], &length, &payload, &jpeg[2..]].concat()
    }

    #[test]
    fn inspect_without_computable_meta_code() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-meta-error");
        std::fs::create_dir_all(&dir).unwrap();
        let bad_meta = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
            <rdf:Description rdf:about="" xmlns:iscc="http://purl.org/iscc/schema/" iscc:name="Bad Meta" iscc:meta="not a data url"/>
            </rdf:RDF></x:xmpmeta>"#;
        let cases = [
            // The file name is the title and normalises to nothing.
            (
                "--.jpg",
                std::fs::read(format!(
                    "{}/tests/fixtures/no_manifest.jpg",
                    env!("CARGO_MANIFEST_DIR")
                ))
                .unwrap(),
                "  ",
            ),
            // ISCC metadata that is neither JSON nor a data URL.
            ("bad-meta.jpg", jpeg_with_xmp(bad_meta), "Bad Meta"),
        ];
        for (name, bytes, title) in cases {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let inspection = inspect(&path).unwrap();
            assert_eq!(inspection.meta_fields.name, title, "{name}");
            assert!(inspection.meta_error.is_some(), "{name}");
            let names: Vec<_> = inspection.iscc.iter().map(|u| u.name).collect();
            assert_eq!(
                names,
                ["Content-Code Image", "Data-Code", "Instance-Code"],
                "{name}"
            );
        }
    }

    #[test]
    fn inspect_text_that_is_not_utf8() {
        // The units are computed from a lossy decoding; c2pa refuses the file and says why.
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-latin1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latin-1.txt");
        std::fs::write(&path, b"Caf\xe9 au lait, cr\xe8me br\xfbl\xe9e").unwrap();
        let inspection = inspect(&path).unwrap();
        assert_eq!(inspection.kind, Kind::Text);
        let error = inspection.manifest_error.expect("c2pa rejects the file");
        assert!(error.contains("UTF-8"), "{error}");
        let names: Vec<_> = inspection.iscc.iter().map(|u| u.name).collect();
        assert_eq!(
            names,
            [
                "Meta-Code",
                "Content-Code Text",
                "Data-Code",
                "Instance-Code"
            ]
        );
    }

    /// Unit names of an inspection.
    fn unit_names(inspection: &Inspection) -> Vec<&'static str> {
        inspection.iscc.iter().map(|u| u.name).collect()
    }

    /// Progress that is never stopped.
    fn unstopped(_: &str, _: Option<f64>) -> bool {
        true
    }

    #[test]
    fn a_glance_leaves_the_slow_units_pending() {
        crate::tools::tests::ensure_semantic();
        crate::tools::tests::ensure_ffmpeg();
        let image = fixture("no_manifest.jpg");
        let glanced = inspect_with(Path::new(&image), Depth::GLANCE, &unstopped).unwrap();
        assert_eq!(glanced.pending, ["semantic"]);
        let quick = [
            "Meta-Code",
            "Content-Code Image",
            "Data-Code",
            "Instance-Code",
        ];
        assert_eq!(unit_names(&glanced), quick);
        assert_eq!(glanced.semantic_error, None);
        let full = inspect_with(Path::new(&image), Depth::FULL, &unstopped).unwrap();
        assert!(full.pending.is_empty());
        assert_eq!(unit_names(&full)[1], "Semantic-Code Image");

        let video = fixture("demo.mp4");
        let glanced = inspect_with(Path::new(&video), Depth::GLANCE, &unstopped).unwrap();
        assert_eq!(glanced.pending, ["content", "data", "instance"]);
        assert_eq!(unit_names(&glanced), ["Meta-Code"]);
        assert_eq!(
            (glanced.content_error, glanced.semantic_error),
            (None, None)
        );
        assert_eq!((glanced.width, glanced.height), (176, 144));
        assert!(glanced.duration_secs.is_some_and(|s| (s - 8.0).abs() < 0.2));
        assert!(glanced.preview.starts_with("data:image/jpeg;base64,"));
        let full = inspect_with(Path::new(&video), Depth::FULL, &unstopped).unwrap();
        assert!(full.pending.is_empty());
        let units = [
            "Meta-Code",
            "Content-Code Video",
            "Data-Code",
            "Instance-Code",
        ];
        assert_eq!(unit_names(&full), units);
    }

    #[test]
    fn ocr_leaves_a_scan_pending_at_a_glance() {
        crate::tools::tests::ensure_semantic();
        let scan = fixture("scan-demo.pdf");
        let scan = Path::new(&scan);
        let pages = OcrPages {
            scanned: 1,
            pages: 1,
            on: true,
        };
        let glanced = inspect_with(scan, Depth::GLANCE.with_ocr(true), &unstopped).unwrap();
        assert_eq!(glanced.pending, ["semantic", "content"]);
        assert_eq!(glanced.ocr, Some(pages));
        assert_eq!(
            (&glanced.content_error, &glanced.semantic_error),
            (&None, &None)
        );
        assert_eq!(
            unit_names(&glanced),
            ["Meta-Code", "Data-Code", "Instance-Code"]
        );

        let reports = std::cell::RefCell::new(Vec::new());
        let full = inspect_with(scan, Depth::FULL.with_ocr(true), &|unit, f| {
            reports.borrow_mut().push((unit, f));
            true
        })
        .unwrap();
        assert!(full.pending.is_empty());
        assert_eq!(full.ocr, Some(pages));
        let units = [
            "Meta-Code",
            "Semantic-Code Text",
            "Content-Code Text",
            "Data-Code",
            "Instance-Code",
        ];
        assert_eq!(unit_names(&full), units);
        let reports = reports.into_inner();
        assert_eq!(reports.first(), Some(&("content", Some(1.0))), "the page");
        // Then its text is embedded, unless another test embedded it before (the memo).
        let mut embedding = reports.iter().skip_while(|(unit, _)| *unit == "content");
        assert!(
            embedding.all(|(unit, _)| *unit == "semantic"),
            "{reports:?}"
        );
        let stopped = inspect_with(scan, Depth::FULL.with_ocr(true), &|_, _| false);
        assert!(stopped.unwrap_err().downcast_ref::<Cancelled>().is_some());
    }

    #[test]
    fn a_scan_without_ocr_says_why_it_has_no_text() {
        crate::tools::tests::ensure_semantic();
        let scan = fixture("scan-demo.pdf");
        let pages = OcrPages {
            scanned: 1,
            pages: 1,
            on: false,
        };
        let off = inspect_with(Path::new(&scan), Depth::GLANCE, &unstopped).unwrap();
        assert!(off.pending.is_empty(), "nothing left for later");
        assert_eq!(off.ocr, Some(pages));
        assert_eq!(off.content_error.as_deref(), Some(crate::pdf::NEEDS_OCR));
        assert_eq!(off.semantic_error, off.content_error);
        let born_digital = inspect_with(Path::new(&fixture("demo.pdf")), Depth::GLANCE, &unstopped);
        assert_eq!(born_digital.unwrap().ocr, None);
    }

    #[test]
    fn a_blank_scan_has_no_text_to_code() {
        let blank = fixture("scan-blank.pdf");
        let full = inspect_with(Path::new(&blank), Depth::FULL.with_ocr(true), &unstopped).unwrap();
        let reason = Some(crate::pdf::NOTHING_RECOGNISED);
        assert_eq!(full.content_error.as_deref(), reason);
        assert_eq!(full.semantic_error.as_deref(), reason);
        assert!(full.pending.is_empty());
    }

    #[test]
    fn a_signed_video_at_a_glance_leaves_its_verdict_open() {
        crate::tools::tests::ensure_ffmpeg();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("demo-signed.avi");
        let request = crate::sign::SignRequest {
            source: fixture("demo.avi"),
            output: output.to_string_lossy().into_owned(),
            title: "Demo video".into(),
            description: None,
            meta: None,
            source_type: None,
            units: ["meta", "video", "data", "instance"]
                .map(String::from)
                .to_vec(),
            training: Default::default(),
            credentials: crate::sign::Credentials::Demo,
            tsa_url: None,
        };
        crate::sign::sign(&request).unwrap();
        let glanced = inspect_with(&output, Depth::GLANCE, &unstopped).unwrap();
        let manifest = glanced.manifest.unwrap();
        assert!(manifest.source_view, "an AVI has a byte-range hash");
        assert_eq!(
            manifest.validation_state, "Pending",
            "checked with the hashes"
        );
        assert_eq!(manifest.invalid_reason, None);
        let sb = &manifest.soft_bindings[0];
        assert_eq!(sb.preservation, None, "decided once the file is hashed");
        let compared: Vec<bool> = sb.matches.iter().map(|m| m.similarity.is_some()).collect();
        assert_eq!(
            compared,
            [true, false, false, false],
            "only the Meta-Code is known"
        );
        let full = inspect_with(&output, Depth::FULL, &unstopped).unwrap();
        let manifest = full.manifest.unwrap();
        assert_eq!(manifest.validation_state, "Trusted");
        let sb = &manifest.soft_bindings[0];
        assert_eq!(sb.preservation, Some(Preservation::Changed));
    }

    #[test]
    fn only_images_and_texts_have_a_semantic_code() {
        crate::tools::tests::ensure_semantic();
        let full = |name: &str| inspect_with(Path::new(&fixture(name)), Depth::FULL, &unstopped);
        let audio = full("demo.mp3").unwrap();
        assert!(audio.iscc.iter().all(|u| u.unit != "semantic"));
        assert_eq!(audio.semantic_error, None);
        let scan = full("scan.pdf").unwrap();
        assert!(scan.semantic_error.is_some());
        assert_eq!(
            scan.semantic_error, scan.content_error,
            "no text, for the same reason"
        );

        let reports = std::cell::RefCell::new(Vec::new());
        let text = inspect_with(Path::new(&fixture("demo.txt")), Depth::FULL, &|unit, f| {
            reports.borrow_mut().push((unit, f));
            true
        })
        .unwrap();
        assert_eq!(unit_names(&text)[1], "Semantic-Code Text");
        let reports = reports.into_inner();
        assert!(
            reports.iter().all(|(unit, _)| *unit == "semantic"),
            "{reports:?}"
        );
        assert_eq!(reports.last(), Some(&("semantic", Some(1.0))));
        let stopped = inspect_with(Path::new(&fixture("demo.md")), Depth::FULL, &|_, _| false);
        assert!(stopped.unwrap_err().downcast_ref::<Cancelled>().is_some());
    }

    #[test]
    fn a_kind_of_semantic_code_left_out_is_never_mentioned() {
        crate::tools::tests::ensure_semantic();
        let image = fixture("no_manifest.jpg");
        let text = fixture("demo.txt");
        let image_only = SemanticKinds::NONE.with(SemanticKind::Image, true);
        let glance = |path: &str, kinds| {
            let depth = Depth::GLANCE.with_semantic_kinds(kinds);
            inspect_with(Path::new(path), depth, &unstopped).unwrap()
        };
        for (path, kinds) in [(&image, SemanticKinds::NONE), (&text, image_only)] {
            let inspection = glance(path, kinds);
            assert!(inspection.pending.is_empty(), "{path}");
            assert_eq!(inspection.semantic_error, None, "{path}");
            assert!(inspection.iscc.iter().all(|u| u.unit != "semantic"));
        }
        assert_eq!(glance(&image, image_only).pending, ["semantic"]);
        let depth = Depth::FULL.with_semantic_kinds(SemanticKinds::NONE);
        let full = inspect_with(Path::new(&image), depth, &|unit, _| {
            assert_ne!(unit, "semantic", "nothing embedded");
            true
        })
        .unwrap();
        assert_eq!(unit_names(&full).len(), 4);
    }

    #[test]
    fn an_embedded_semantic_code_of_a_kind_left_out_is_not_compared() {
        crate::tools::tests::ensure_semantic();
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("semantic-signed.jpg");
        let request = crate::sign::SignRequest {
            source: fixture("no_manifest.jpg"),
            output: output.to_string_lossy().into_owned(),
            title: "With a Semantic-Code".into(),
            description: None,
            meta: None,
            source_type: None,
            units: ["meta", "semantic", "image", "data", "instance"]
                .map(String::from)
                .to_vec(),
            training: Default::default(),
            credentials: crate::sign::Credentials::Demo,
            tsa_url: None,
        };
        crate::sign::sign(&request).unwrap();
        let depth = Depth::FULL.with_semantic_kinds(SemanticKinds::NONE);
        let inspection = inspect_with(&output, depth, &unstopped).unwrap();
        let sb = &inspection.manifest.unwrap().soft_bindings[0];
        assert_eq!(sb.preservation, Some(Preservation::Preserved));
        for m in &sb.matches {
            if m.embedded.unit == "semantic" {
                assert_eq!((&m.computed, m.similarity), (&None, None));
            } else {
                assert_eq!(m.similarity, Some(1.0), "{}", m.embedded.name);
            }
        }
        assert_eq!(sb.matches.len(), 5);
    }

    #[test]
    fn inspect_audio_without_manifest() {
        for file in ["demo.mp3", "demo.flac", "demo.wav", "demo.m4a"] {
            let inspection = inspect(Path::new(&fixture(file))).unwrap();
            assert_eq!(inspection.kind, Kind::Audio, "{file}");
            assert!(inspection.manifest.is_none(), "{file}");
            assert!(inspection.manifest_error.is_none(), "{file}");
            assert!(inspection.content_error.is_none(), "{file}");
            assert_eq!(
                unit_names(&inspection),
                [
                    "Meta-Code",
                    "Content-Code Audio",
                    "Data-Code",
                    "Instance-Code"
                ],
                "{file}"
            );
            assert_eq!(inspection.meta_fields.name_source, "metadata", "{file}");
            assert!(inspection.duration_secs.unwrap() >= 4.0, "{file}");
            assert_eq!((inspection.width, inspection.height), (0, 0), "{file}");
        }
    }

    #[test]
    fn inspect_audio_too_short_for_a_fingerprint() {
        let inspection = inspect(Path::new(&fixture("short.wav"))).unwrap();
        let error = inspection.content_error.as_deref().unwrap();
        assert!(error.contains("too short"), "{error}");
        assert_eq!(
            unit_names(&inspection),
            ["Meta-Code", "Data-Code", "Instance-Code"]
        );
        assert_eq!(inspection.meta_fields.name, "short");
    }

    #[test]
    fn inspect_audio_with_cover_art() {
        let inspection = inspect(Path::new(&fixture("withcover.mp3"))).unwrap();
        assert!(inspection.preview.starts_with("data:image/jpeg;base64,"));
        assert_eq!(inspection.creator.as_deref(), Some("Test Artist"));
        assert_eq!(inspection.characters, None);
        assert!((inspection.duration_secs.unwrap() - 15.5).abs() < 0.01);
        assert_eq!(inspection.iscc[1].name, "Content-Code Audio");
    }

    #[test]
    fn inspect_video_without_manifest() {
        crate::tools::tests::ensure_ffmpeg();
        for file in ["demo.mp4", "demo.mov", "demo.m4v", "demo.avi"] {
            let inspection = inspect(Path::new(&fixture(file))).unwrap();
            assert_eq!(inspection.kind, Kind::Video, "{file}");
            assert!(inspection.manifest.is_none(), "{file}");
            assert!(inspection.content_error.is_none(), "{file}");
            assert!(!inspection.content_from_source, "{file}: decoded");
            assert_eq!(
                unit_names(&inspection),
                [
                    "Meta-Code",
                    "Content-Code Video",
                    "Data-Code",
                    "Instance-Code"
                ],
                "{file}"
            );
            assert_eq!(inspection.meta_fields.name_source, "metadata", "{file}");
            let seconds = inspection.duration_secs.unwrap();
            assert!((seconds - 8.0).abs() < 0.2, "{file}: {seconds}");
            assert_eq!((inspection.width, inspection.height), (176, 144), "{file}");
            assert!(inspection.preview.starts_with("data:image/jpeg;base64,"));
            let size = std::fs::metadata(fixture(file)).unwrap().len();
            assert_eq!(inspection.size_bytes, size, "{file}");
        }
    }

    #[test]
    fn inspect_video_without_ffmpeg() {
        // As `asset::load` leaves a video without ffmpeg: hashed, content and tags unread.
        let path = fixture("demo.mp4");
        let path = Path::new(&path);
        let reason = crate::tools::Missing::Ffmpeg { available: true }.reason();
        let bytes = std::fs::read(path).unwrap();
        let loaded = Loaded {
            asset: asset::Asset {
                content: AssetContent::Unread(reason),
                preview: None,
                metadata: Default::default(),
                sign_block: None,
                sign_warning: None,
                ocr: None,
            },
            bitstream: Some(iscc::bitstream_units(&bytes).unwrap()),
            size: bytes.len() as u64,
        };
        let format = asset_format(path).unwrap();
        let inspection =
            inspect_loaded(path, format, loaded, Depth::NO_SEMANTIC, &|_, _| true).unwrap();
        assert_eq!(inspection.kind, Kind::Video);
        assert_eq!(unit_names(&inspection), ["Data-Code", "Instance-Code"]);
        assert_eq!(inspection.meta_error.as_deref(), Some(reason));
        assert_eq!(inspection.content_error.as_deref(), Some(reason));
        assert_eq!(inspection.preview, "");
        assert_eq!(inspection.duration_secs, None);
    }

    #[test]
    fn inspect_video_with_sound_only_as_audio() {
        // Read like an M4A, without ffmpeg; the reference comes from iscc-sdk's audio functions.
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_audio.json")).unwrap();
        for (file, label) in [
            ("no-video.mp4", "MP4 audio"),
            ("no-video.mov", "QuickTime audio (MOV)"),
        ] {
            let inspection = inspect(Path::new(&fixture(file))).unwrap();
            assert_eq!(inspection.kind, Kind::Audio, "{file}");
            assert_eq!(inspection.format_label, label);
            let video = formats::by_path(Path::new(file)).unwrap();
            assert_eq!(
                inspection.mime, video.mime,
                "{file} is signed as what it is"
            );
            assert!(inspection.content_error.is_none(), "{file}");
            assert_eq!(
                unit_names(&inspection),
                [
                    "Meta-Code",
                    "Content-Code Audio",
                    "Data-Code",
                    "Instance-Code"
                ],
                "{file}"
            );
            let want = &expected[file];
            let codes: Vec<&str> = inspection.iscc.iter().map(|u| u.iscc.as_str()).collect();
            assert_eq!(
                codes,
                [
                    &want["meta"],
                    &want["audio"],
                    &want["data"],
                    &want["instance"]
                ],
                "{file}"
            );
            let seconds = inspection.duration_secs.unwrap();
            assert!((seconds - 8.0).abs() < 0.2, "{file}: {seconds}");
            assert_eq!(inspection.preview, "", "{file} has no cover art");
        }
    }

    #[test]
    fn inspect_audio_that_does_not_decode() {
        // A sound-only MP4 whose track is AC-3, which no decoder here takes.
        let inspection = inspect(Path::new(&fixture("no-video-ac3.mp4"))).unwrap();
        assert_eq!(inspection.kind, Kind::Audio);
        assert_eq!(inspection.format_label, "MP4 audio");
        assert_eq!(
            inspection.content_error.as_deref(),
            Some("this audio codec is not supported")
        );
        assert_eq!(
            unit_names(&inspection),
            ["Meta-Code", "Data-Code", "Instance-Code"]
        );
        assert_eq!(inspection.duration_secs, None);
        assert_eq!(inspection.meta_fields.name_source, "metadata");
    }

    /// A validly signed, unchanged file with a data hash, signed from a file without a manifest.
    const FACTS: BindingFacts = BindingFacts {
        source_view: true,
        signature_valid: true,
        hard_binding_matches: true,
        resigned: false,
    };

    /// The source view of `bytes`, written to a file and read back through [`ViewReader`].
    fn source_view(bytes: &[u8], exclusions: &[(u64, u64)]) -> Vec<u8> {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-view");
        std::fs::create_dir_all(&dir).unwrap();
        let name: String = format!("{exclusions:?}")
            .chars()
            .map(|c| if c.is_ascii_digit() { c } else { '_' })
            .collect();
        let path = dir.join(format!("view{name}.bin"));
        std::fs::write(&path, bytes).unwrap();
        let mut view = Vec::new();
        ViewReader::open(&path, exclusions)
            .unwrap()
            .read_to_end(&mut view)
            .unwrap();
        view
    }

    #[test]
    fn source_view_cuts_the_exclusion_ranges() {
        assert_eq!(kept_ranges(10, &[(2, 3)]), [(0, 2), (5, 10)]);
        assert_eq!(kept_ranges(0, &[]), []);
        let bytes: Vec<u8> = (0..10).collect();
        assert_eq!(source_view(&bytes, &[]), bytes);
        assert_eq!(source_view(&bytes, &[(2, 3)]), [0, 1, 5, 6, 7, 8, 9]);
        // Unsorted, adjacent and trailing ranges.
        assert_eq!(
            source_view(&bytes, &[(8, 2), (2, 1), (3, 2)]),
            [0, 1, 5, 6, 7]
        );
        // Overlapping ranges and ranges past the end.
        assert_eq!(source_view(&bytes, &[(1, 4), (3, 4), (9, 100)]), [0, 7, 8]);
        assert_eq!(source_view(&bytes, &[(20, 5), (0, 0)]), bytes);
        assert_eq!(source_view(&bytes, &[(0, u64::MAX)]), Vec::<u8>::new());
    }

    #[test]
    fn preservation_follows_iep_0020() {
        use Preservation::*;
        let with = |f: fn(&mut BindingFacts)| {
            let mut facts = FACTS;
            f(&mut facts);
            facts
        };
        assert_eq!(preservation(&FACTS, Some(true)), Preserved);
        assert_eq!(preservation(&FACTS, Some(false)), Changed);
        let resigned = with(|f| f.resigned = true);
        assert_eq!(preservation(&resigned, Some(false)), Resigned);
        assert_eq!(preservation(&FACTS, None), NoInstanceCode);
        let no_view = with(|f| f.source_view = false);
        assert_eq!(preservation(&no_view, Some(false)), NoSourceView);
        let changed = with(|f| f.hard_binding_matches = false);
        assert_eq!(preservation(&changed, Some(false)), FileChanged);
        assert_eq!(preservation(&changed, None), FileChanged);
        // Equal Instance-Codes do not outweigh a hard binding that does not match the file.
        assert_eq!(preservation(&changed, Some(true)), FileChanged);
        let changed_zip = with(|f| {
            f.source_view = false;
            f.hard_binding_matches = false;
        });
        assert_eq!(preservation(&changed_zip, Some(false)), FileChanged);
        let unsigned = with(|f| f.signature_valid = false);
        assert_eq!(preservation(&unsigned, Some(true)), SignatureInvalid);
    }

    #[test]
    fn inspect_soft_binding_without_binding_metadata() {
        // Files signed by other tools, or by this demo before it wrote the map, carry no metadata.
        let sb: SoftBinding = serde_json::from_value(serde_json::json!({
            "alg": ISCC_SOFT_BINDING_ALG,
            "blocks": [{ "scope": {}, "value": [1, 2, 3] }],
        }))
        .unwrap();
        let summaries = summarize_assertion(&sb, &[], None, Some(&FACTS));
        assert_eq!(summaries.len(), 1);
        assert!(summaries[0].metadata.is_none());

        // A map without any of the known fields is treated the same.
        let sb: SoftBinding = serde_json::from_value(serde_json::json!({
            "alg": ISCC_SOFT_BINDING_ALG,
            "blocks": [{ "scope": {}, "value": [1, 2, 3] }],
            "bindingMetadata": { "io.example.custom": 1, "contact": "  " },
        }))
        .unwrap();
        assert!(binding_metadata(&sb).is_none());
    }
}
