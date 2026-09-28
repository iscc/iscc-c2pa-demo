//! Read an asset: file facts, preview, ISCC units and the C2PA manifest store with validation.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use c2pa::assertions::{labels, SoftBinding};
use c2pa::{Context, Manifest, Reader, ValidationState};
use image::codecs::jpeg::JpegEncoder;
use serde::Serialize;
use serde_json::Value;

use crate::asset::{self, AssetContent};
use crate::context::{base_settings, ISCC_SOFT_BINDING_ALG};
use crate::formats::{self, Format, Kind};
use crate::iscc::{self, IsccUnit, MetaInput};
use crate::metadata::{self, ManifestMeta, MetaFields};

/// Longest edge of the preview image sent to the UI.
const PREVIEW_EDGE: u32 = 640;
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
    /// Pixel size of an image (the rendered size of an SVG); 0 for text and audio assets.
    pub width: u32,
    pub height: u32,
    /// JPEG preview as a data URL (the cover or thumbnail of a document); empty when there is
    /// nothing to show.
    pub preview: String,
    /// Characters of extracted text; `None` for images and audio.
    pub characters: Option<usize>,
    /// Length of the decoded audio in seconds; `None` for images and text.
    pub duration_secs: Option<f64>,
    /// Creator named in the asset's own metadata; display only.
    pub creator: Option<String>,
    /// Meta, Content, Data and Instance units of the whole file, as any ISCC tool computes them
    /// and as signing this file would embed them.
    pub iscc: Vec<IsccUnit>,
    /// Title, description and ISCC metadata behind the Meta-Code, and where the title came from.
    pub meta_fields: MetaFields,
    /// Why the Meta-Code could not be computed from `meta_fields`; `iscc` then lacks it.
    pub meta_error: Option<String>,
    /// Why the Content-Code could not be computed (audio too short); `iscc` then lacks it.
    pub content_error: Option<String>,
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
    pub validation_state: String,
    /// Why the manifest is invalid; set exactly when `validation_state` is `Invalid`.
    pub invalid_reason: Option<InvalidReason>,
    /// URI of the trust list the active manifest's signer chains to; `None` when untrusted.
    pub trust_list: Option<String>,
    pub validation: Value,
    /// True when the hard binding is a data hash, so the file has a source view: the file
    /// without the byte ranges the data hash excludes (IEP-0020).
    pub source_view: bool,
    pub signature: Option<SignatureSummary>,
    pub assertions: Vec<AssertionSummary>,
    pub ingredients: Vec<IngredientSummary>,
    pub soft_bindings: Vec<SoftBindingSummary>,
    pub training_mining: Option<Value>,
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

/// Format of the asset at `path`, from its extension.
pub fn asset_format(path: &Path) -> Result<&'static Format> {
    formats::by_path(path).ok_or_else(|| anyhow!("unsupported file type"))
}

/// Inspect the file at `path`. The units in `iscc` are those of the whole file; see
/// [`summarize_assertion`] for what soft bindings are compared with.
pub fn inspect(path: &Path) -> Result<Inspection> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    let format = asset_format(path)?;
    let asset = asset::read(path, &bytes, format)?;

    let (reader, manifest_error) = match read_manifest(path) {
        Ok(reader) => (Some(reader), None),
        Err(c2pa::Error::JumbfNotFound) => (None, None),
        Err(e) => (None, Some(e.to_string())),
    };
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
    // An unusable title or ISCC metadata costs only the Meta-Code, audio too short for a
    // fingerprint only the Content-Code, not the inspection.
    let (meta, meta_error) = unit_or_error(iscc::meta_unit(meta_input(&meta_fields)));
    let (content, content_error) = unit_or_error(iscc::content_unit(asset.content()));
    let bitstream = [iscc::data_unit(&bytes)?, iscc::instance_unit(&bytes)?];
    let units: Vec<IsccUnit> = meta.into_iter().chain(content).chain(bitstream).collect();
    let view_units = reader
        .as_ref()
        .and_then(data_hash_exclusions)
        .map(|ranges| source_view_units(path, format, &source_view(&bytes, &ranges), &units))
        .transpose()?;
    let compared = match &view_units {
        Some(view) => with_bitstream_of(&units, view),
        None => units.clone(),
    };
    let manifest = reader
        .as_ref()
        .zip(json.as_ref())
        .map(|(r, j)| summarize(r, j, &compared, view_units.as_deref()));
    let (width, height) = match &asset.content {
        AssetContent::Image(rgb) => rgb.dimensions(),
        AssetContent::Text(_) | AssetContent::Audio(_) => (0, 0),
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
        size_bytes: bytes.len() as u64,
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
            _ => None,
        },
        creator: asset.metadata.creator.clone(),
        iscc: units,
        meta_fields,
        meta_error,
        content_error,
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

/// Byte ranges `(start, length)` that the data hash of the active manifest excludes; `None`
/// when its hard binding is not a data hash (collection, BMFF and box hashes define no source
/// view).
fn data_hash_exclusions(reader: &Reader) -> Option<Vec<(u64, u64)>> {
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

/// The source view (IEP-0020): `bytes` without the `(start, length)` exclusion ranges, which
/// may come in any order, overlap or reach past the end.
pub fn source_view(bytes: &[u8], exclusions: &[(u64, u64)]) -> Vec<u8> {
    let clamp = |n: u64| usize::try_from(n).unwrap_or(usize::MAX).min(bytes.len());
    let mut ranges = exclusions.to_vec();
    ranges.sort_unstable();
    let mut view = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    for (start, length) in ranges {
        let (start, end) = (clamp(start), clamp(start.saturating_add(length)));
        if start > pos {
            view.extend_from_slice(&bytes[pos..start]);
        }
        pos = pos.max(end);
    }
    view.extend_from_slice(&bytes[pos..]);
    view
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

/// Units of the source view. The Meta-Code is the file's, since its inputs never lie inside the
/// manifest store. The view of a file that is not source-preserving need not decode, so a
/// Content-Code that cannot be computed from it is left out.
fn source_view_units(
    path: &Path,
    format: &'static Format,
    view: &[u8],
    file_units: &[IsccUnit],
) -> Result<Vec<IsccUnit>> {
    let meta = file_units.iter().find(|u| u.unit == "meta").cloned();
    let content = asset::read(path, view, format)
        .ok()
        .and_then(|a| iscc::content_unit(a.content()).ok());
    let bitstream = [iscc::data_unit(view)?, iscc::instance_unit(view)?];
    Ok(meta.into_iter().chain(content).chain(bitstream).collect())
}

/// Open the manifest store with the shared trust settings.
fn read_manifest(path: &Path) -> c2pa::Result<Reader> {
    let context = Context::new().with_settings(base_settings())?;
    Reader::from_context(context).with_file(path)
}

/// Down-scale the flattened image and encode it as a JPEG data URL.
fn preview_data_url(rgb: &image::RgbImage) -> Result<String> {
    let (w, h) = rgb.dimensions();
    let scale = (PREVIEW_EDGE as f64 / w.max(h).max(1) as f64).min(1.0);
    let (pw, ph) = (
        ((w as f64 * scale) as u32).max(1),
        ((h as f64 * scale) as u32).max(1),
    );
    let small = image::imageops::resize(rgb, pw, ph, image::imageops::FilterType::Triangle);
    let mut buf = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buf, 82).encode_image(&small)?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        B64.encode(buf.into_inner())
    ))
}

/// Build the display summary of the active manifest. `view_units` are the units of the source
/// view, when the hard binding is a data hash; see [`summarize_assertion`] for `compared`.
fn summarize(
    reader: &Reader,
    json: &Value,
    compared: &[IsccUnit],
    view_units: Option<&[IsccUnit]>,
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
        validation_state: format!("{:?}", reader.validation_state()),
        invalid_reason: invalid_reason(reader),
        trust_list: signer_trust_list(reader),
        validation: serde_json::to_value(reader.validation_results()).unwrap_or(Value::Null),
        source_view: view_units.is_some(),
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
                let facts = binding_facts(reader, m, view_units.is_some());
                soft_bindings(m, compared, view_units, &facts)
            })
            .unwrap_or_default(),
        training_mining,
        assertions,
    }
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
    Some(reason_for(&failures))
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

/// Plain-language sentence for the failure codes of c2pa-rs 0.91 a reader can act on.
fn plain_reason(code: &str) -> Option<&'static str> {
    Some(match code {
        c if HARD_BINDING_MISMATCH.contains(&c) => {
            "The file changed after signing: its hash no longer matches."
        }
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

/// Decode every soft-binding assertion of a manifest.
fn soft_bindings(
    manifest: &Manifest,
    compared: &[IsccUnit],
    view_units: Option<&[IsccUnit]>,
    facts: &BindingFacts,
) -> Vec<SoftBindingSummary> {
    manifest
        .assertions()
        .iter()
        .filter(|a| is_label(a.label(), labels::SOFT_BINDING))
        .filter_map(|a| a.to_assertion::<SoftBinding>().ok())
        .flat_map(|sb| summarize_assertion(&sb, compared, view_units, facts))
        .collect()
}

/// One summary per block of a soft-binding assertion; the metadata belongs to the assertion and
/// is repeated on each block. IEP-0020, Soft Binding Verification: when the file is
/// source-preserving, the embedded units are compared with `view_units`, because the source view
/// then is the source. Otherwise they are compared with `compared`: Meta-Code and Content-Code
/// of the file, which always decodes, with Data-Code and Instance-Code of the source view where
/// there is one, so that the manifest store does not count as a change.
fn summarize_assertion(
    sb: &SoftBinding,
    compared: &[IsccUnit],
    view_units: Option<&[IsccUnit]>,
    facts: &BindingFacts,
) -> Vec<SoftBindingSummary> {
    let metadata = binding_metadata(sb);
    let summarize = |value: &[u8], units: &[IsccUnit]| {
        summarize_soft_binding(sb.alg.clone(), value, units, metadata.clone())
    };
    sb.blocks
        .iter()
        .map(|block| {
            let on_view = view_units.map(|units| summarize(&block.value, units));
            let instance_equal = on_view.as_ref().and_then(|s| {
                let instance = s.matches.iter().find(|m| m.embedded.unit == "instance")?;
                Some(instance.similarity == Some(1.0))
            });
            let verdict = preservation(facts, instance_equal);
            let mut summary = match on_view {
                Some(s) if verdict == Preservation::Preserved => s,
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
    /// The hard binding is a data hash, so there is a source view.
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
/// source view, `None` without an embedded Instance-Code.
fn preservation(facts: &BindingFacts, instance_equal: Option<bool>) -> Preservation {
    let unverifiable = |reason| {
        if facts.hard_binding_matches {
            reason
        } else {
            Preservation::FileChanged
        }
    };
    if !facts.signature_valid {
        return Preservation::SignatureInvalid;
    }
    if !facts.source_view {
        return unverifiable(Preservation::NoSourceView);
    }
    match instance_equal {
        None => unverifiable(Preservation::NoInstanceCode),
        Some(true) => Preservation::Preserved,
        Some(false) if !facts.hard_binding_matches => Preservation::FileChanged,
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

    /// A validly signed, unchanged file with a data hash, signed from a file without a manifest.
    const FACTS: BindingFacts = BindingFacts {
        source_view: true,
        signature_valid: true,
        hard_binding_matches: true,
        resigned: false,
    };

    #[test]
    fn source_view_cuts_the_exclusion_ranges() {
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
        let summaries = summarize_assertion(&sb, &[], None, &FACTS);
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
