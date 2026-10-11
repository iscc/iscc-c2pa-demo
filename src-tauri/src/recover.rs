//! What a manifest store found by search says about the file in hand, in three checks that the
//! UI keeps apart:
//!
//! - **C2PA**: the store validated with the file as its asset (C2PA, "Validating a Match").
//!   Credentials published apart from their file were made for the copy that embedded them, so
//!   their hard binding normally does not match, and the specification has them rejected for it.
//! - **ISCC** (this app's check, IEP-0020): the units of every `io.iscc.v0` block compared with
//!   the file's. An equal 256-bit Instance-Code (BLAKE3-256) means the file has exactly the bytes
//!   the soft binding was computed from; it ties none of the other statements to the file.
//! - **Rebuild** (this app's check, for those exact matches): the store put back into the file
//!   the way c2pa-rs embeds it, the result validated as an ordinary embedded asset. When it passes
//!   and the hash covers the whole file, the file with these credentials validates as the signed
//!   file, and Restore writes it. It shows no more than the hard binding covers.
//!
//! A store is first screened without validation, which hashes nothing ([`screen`]), and only the
//! candidates worth it are examined in full ([`examine`]). Found stores are validated with the
//! app's trust settings, which hold no c2pa-rs test root: a signer with such a public test key,
//! the demo key among them, is reported as one, never as trusted (`ManifestSummary::test_key`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use c2pa::assertions::{labels, SoftBinding};
use c2pa::{Context, Manifest, Reader, ValidationState};
use serde::Serialize;
use serde_json::{json, Value};

use crate::context::{base_settings, ISCC_SOFT_BINDING_ALG};
use crate::formats::{self, Format};
use crate::inspect::{self, InvalidReason, ManifestSummary, HARD_BINDING_MISMATCH};
use crate::iscc::{self, IsccUnit};
use crate::memo::Memo;
use crate::{pdf_update, sign};

/// Lowest similarity of the Content-Code for a near-identical match: 236 of 256 bits equal (C2PA
/// UX Recommendations, Fingerprinting: matches must be nearly identical). Measured on JPEG photos
/// and the demo picture only, and used for every kind of content: re-encoding, resizing,
/// greyscale, mild changes of colour, brightness and contrast and borders of a few percent stay at
/// 92% or above; a different picture of the same scene came to 89.8%, a crop of 2% a side to 88%.
pub const NEAR_IDENTICAL: f64 = 0.92;

/// The file a found manifest store is examined against.
pub struct InHand<'a> {
    pub path: &'a Path,
    pub format: &'static Format,
    /// Its units as the app shows them, which the soft bindings are compared with.
    pub units: &'a [IsccUnit],
}

/// What a found manifest store says about the file in hand.
#[derive(Serialize, Debug)]
pub struct Examined {
    /// The C2PA verdict for this file as it is.
    pub for_file: ForFile,
    pub iscc: IsccMatch,
    /// The specification attributes the ISCC soft binding to the signer: the claim lists each
    /// assertion that holds it in `created_assertions` (or is a v1 claim). A gathered one is not
    /// the signer's statement.
    pub attributed: bool,
    /// The rebuild check, for exact matches only.
    pub rebuild: Option<Rebuild>,
    /// The active manifest as validated with this file as its asset, its soft bindings compared
    /// with this file's units.
    pub manifest: ManifestSummary,
}

/// The C2PA verdict on a found manifest store for the file in hand as it is: the store validated
/// with the file as its asset.
#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ForFile {
    /// Its data hash matches the whole file: a manifest store published apart from exactly these
    /// bytes. `state` is c2pa's `Valid` or `Trusted`.
    Valid { state: String },
    /// Rejected for this file; `hash_mismatch` when because its hash does not match (C2PA,
    /// "Validating a Match"): it was made for another file, usually the copy that embedded it.
    Rejected {
        reason: InvalidReason,
        hash_mismatch: bool,
    },
    /// c2pa-rs calls it valid, but not through a data hash of its own claim, which is the only
    /// hash this app relies on for a file as it is.
    Unchecked { reason: String },
}

/// How the ISCC soft binding of a found manifest compares with the file in hand (IEP-0020).
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct IsccMatch {
    /// The soft binding lists this file's Instance-Code, in a block for the whole asset: the file
    /// has exactly the bytes it was computed from.
    pub exact: bool,
    /// Lowest similarity, from 0 to 1, of the Content-Codes the soft binding lists to this file's;
    /// `None` when it lists none of this file's kind.
    pub content: Option<f64>,
    /// Exact, or a Content-Code at least [`NEAR_IDENTICAL`] similar.
    pub near: bool,
    /// Soft bindings this app does not check: other algorithms, and ISCC blocks for part of the
    /// asset (a region or a timespan). The specification wants every soft binding of a located
    /// manifest to match, so with any of them the match is not complete.
    pub unchecked: Vec<String>,
}

/// The rebuild check of an exact match: the found manifest store put back into this file.
#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Rebuild {
    /// The rebuilt file validates and its hash covers the whole file; `state` is c2pa's `Valid`
    /// or `Trusted`.
    Passed { state: String },
    /// The rebuilt file validates, but its hash leaves parts of this file out besides the
    /// credentials: `uncovered` bytes of a data hash (an MP3's whole ID3 tag, as c2pa-rs signs
    /// it), or `None` for BMFF boxes or ZIP members it does not list.
    Partial {
        state: String,
        uncovered: Option<u64>,
    },
    /// The rebuilt file does not validate; `hash_mismatch` when because its hash does not match.
    Failed {
        reason: InvalidReason,
        hash_mismatch: bool,
    },
    /// The check cannot be made for this manifest store, or was not made.
    Unavailable { reason: String },
}

/// Hard-binding mismatch of a manifest store put back into the file in hand.
const REBUILT_MISMATCH: &str =
    "Put back into this file, the hash in these credentials does not match it.";

/// Manifest stores of the latest search, by the BLAKE3 hash of their bytes, for Restore.
static FOUND: Memo<Arc<Vec<u8>>> = Memo::new(64);

/// Forget the manifest stores kept by earlier searches.
pub fn forget_found() {
    FOUND.clear();
}

/// Keep the found manifest store `store` for Restore; returns its id, the hex BLAKE3 hash.
pub fn keep(store: Vec<u8>) -> String {
    let hash = blake3::hash(&store);
    FOUND.keep(*hash.as_bytes(), Arc::new(store));
    hash.to_hex().to_string()
}

/// Screen the manifest store `store`, which a service named manifest `manifest_id`, against the
/// file in hand without validating it, which hashes nothing: its ISCC match. `Err` gives the
/// reason to leave it out: it does not read, holds another manifest, or its ISCC soft binding is
/// malformed, missing or names several files.
pub fn screen(file: &InHand, manifest_id: &str, store: &[u8]) -> Result<IsccMatch, String> {
    let reader =
        read(store, file, false).map_err(|e| format!("its manifest store does not read: {e:#}"))?;
    let label = reader.active_label().unwrap_or_default();
    if label != manifest_id {
        return Err(format!(
            "its manifest store holds {label}, not {manifest_id}"
        ));
    }
    let manifest = reader
        .active_manifest()
        .ok_or("its manifest store has no active manifest")?;
    iscc_match(&soft_bindings_of(manifest)?, file.units)
}

/// Examine the manifest store `store`, screened as `iscc`, against the file in hand: validate it
/// with the file as its asset and, when `rebuild` and the match is exact, put it back into the
/// file. `Err` gives the reason to leave it out: it does not read, or its signature does not
/// validate.
pub fn examine(
    file: &InHand,
    store: &[u8],
    iscc: IsccMatch,
    rebuild: bool,
) -> Result<Examined, String> {
    let reader =
        read(store, file, true).map_err(|e| format!("its manifest store does not read: {e:#}"))?;
    if let Some(code) = inspect::signature_failure(&reader) {
        return Err(format!("its signature does not validate ({code})"));
    }
    let binding = Binding::of(&reader);
    let rebuild = match (iscc.exact, rebuild) {
        (false, _) => None,
        (true, true) => Some(rebuild_check(file, store, &binding)),
        (true, false) => Some(Rebuild::Unavailable {
            reason: "it was not made, as this search checks only its first exact matches"
                .to_owned(),
        }),
    };
    let manifest = inspect::found_summary(&reader, file.units);
    Ok(Examined {
        for_file: for_file(&binding, &manifest),
        attributed: iscc_attributed(&reader),
        rebuild,
        manifest,
        iscc,
    })
}

/// The manifest store `store` read with the file in hand as its asset; validated when `check`.
fn read(store: &[u8], file: &InHand, check: bool) -> Result<Reader> {
    let mut settings = base_settings();
    settings["verify"]["verify_after_reading"] = Value::Bool(check);
    let context = Context::new().with_settings(settings)?;
    let asset =
        File::open(file.path).with_context(|| format!("cannot read {}", file.path.display()))?;
    Ok(Reader::from_context(context).with_manifest_data_and_stream(
        store,
        file.format.mime,
        asset,
    )?)
}

/// Whether the claim lists every `c2pa.soft-binding` assertion with ISCC soft bindings among
/// those the specification attributes to the signer.
fn iscc_attributed(reader: &Reader) -> bool {
    let Some(manifest) = reader.active_manifest() else {
        return false;
    };
    let attributed = inspect::attributed_labels(reader);
    manifest
        .assertions()
        .iter()
        .filter(|a| labels::parse_label(a.label()).0 == labels::SOFT_BINDING)
        .filter(|a| {
            a.to_assertion::<SoftBinding>()
                .is_ok_and(|sb| sb.alg.as_deref() == Some(ISCC_SOFT_BINDING_ALG))
        })
        .all(|a| attributed.contains(&a.label_with_instance()))
}

/// The hard binding of a found manifest's own claim, as far as this app tells kinds apart.
enum Binding {
    /// A data hash: byte ranges of the file.
    Data,
    /// A BMFF hash, with its assertion.
    Bmff(Value),
    /// A collection hash, as c2pa-rs binds ZIP formats (EPUB, OOXML, ODF), with its assertion.
    Collection(Value),
    /// A box hash.
    Boxes,
    /// None of its own (an update manifest), or one this app does not know.
    Other,
}

impl Binding {
    /// The hard binding of the active manifest of `reader`.
    fn of(reader: &Reader) -> Binding {
        let bindings = inspect::hard_bindings(reader);
        let find = |base: &str| {
            bindings
                .iter()
                .find(|(label, _)| label == base)
                .map(|(_, a)| a.clone())
        };
        if find(labels::DATA_HASH).is_some() {
            Binding::Data
        } else if let Some(bmff) = find(labels::BMFF_HASH) {
            Binding::Bmff(bmff)
        } else if let Some(collection) = find(labels::COLLECTION_HASH) {
            Binding::Collection(collection)
        } else if find(labels::BOX_HASH).is_some() {
            Binding::Boxes
        } else {
            Binding::Other
        }
    }

    /// Why a verdict through this binding is not relied on; `None` for a data hash.
    fn doubt(&self) -> Option<&'static str> {
        Some(match self {
            Binding::Data => return None,
            Binding::Bmff(_) => {
                "their hash is a BMFF hash, which this app relies on only with the credentials put back"
            }
            Binding::Collection(_) => {
                "their hash is a collection hash, which this app relies on only with the credentials put back"
            }
            Binding::Boxes => BOX_HASH_UNCHECKED,
            Binding::Other => "their own claim carries no hash of a file this app checks",
        })
    }
}

/// Why a box hash is never relied on: c2pa-rs 0.91 passes one for a file without the box of the
/// manifest store, and does not check that its box list is complete (fixed upstream in #2668, not
/// released).
const BOX_HASH_UNCHECKED: &str =
    "their hash is a box hash, which c2pa-rs 0.91 does not check closely enough";

/// The C2PA verdict for the file in hand of the manifest store whose own hard binding is
/// `binding`, summarised as `manifest`. Valid only through a data hash: c2pa-rs (since #2643)
/// then hashes the whole file, as a detached store leaves nothing out. A rejection always stands.
fn for_file(binding: &Binding, manifest: &ManifestSummary) -> ForFile {
    if let Some(reason) = &manifest.invalid_reason {
        return ForFile::Rejected {
            reason: reason.clone(),
            hash_mismatch: is_hash_mismatch(reason),
        };
    }
    match binding.doubt() {
        Some(reason) => ForFile::Unchecked {
            reason: reason.to_owned(),
        },
        None => ForFile::Valid {
            state: manifest.validation_state.clone(),
        },
    }
}

/// Whether `reason` is a hard binding that does not match.
fn is_hash_mismatch(reason: &InvalidReason) -> bool {
    reason
        .code
        .as_deref()
        .is_some_and(|code| HARD_BINDING_MISMATCH.contains(&code))
}

/// The soft-binding assertions of `manifest`, or why one does not read.
fn soft_bindings_of(manifest: &Manifest) -> Result<Vec<SoftBinding>, String> {
    manifest
        .assertions()
        .iter()
        .filter(|a| labels::parse_label(a.label()).0 == labels::SOFT_BINDING)
        .map(|a| {
            a.to_assertion()
                .map_err(|e| format!("its soft binding is malformed: {e}"))
        })
        .collect()
}

/// The units of the `io.iscc.v0` blocks of a manifest's soft bindings, and what is not checked.
#[derive(Default)]
struct IsccBlocks {
    /// Blocks for the whole asset.
    whole: Vec<Vec<IsccUnit>>,
    /// Blocks for part of the asset (a region or a timespan).
    parts: Vec<Vec<IsccUnit>>,
    /// Soft bindings not checked here: other algorithms, ISCC blocks for part of the asset.
    unchecked: Vec<String>,
    /// ISCC-UNITs ignored for matching: shorter than 256 bits.
    ignored: usize,
}

/// Sort the blocks of `bindings`; `Err` when an `io.iscc.v0` block is malformed.
fn iscc_blocks(bindings: &[SoftBinding]) -> Result<IsccBlocks, String> {
    let mut blocks = IsccBlocks::default();
    for sb in bindings {
        let alg = sb.alg.as_deref().unwrap_or("no algorithm");
        if alg != ISCC_SOFT_BINDING_ALG {
            blocks.unchecked.push(alg.to_owned());
            continue;
        }
        for block in &sb.blocks {
            let (listed, ignored) = block_units(&block.value)
                .map_err(|e| format!("its ISCC soft binding is malformed: {e:#}"))?;
            blocks.ignored += ignored;
            if inspect::is_part(block) {
                blocks.parts.push(listed);
                blocks
                    .unchecked
                    .push(format!("{ISCC_SOFT_BINDING_ALG} for part of the asset"));
            } else {
                blocks.whole.push(listed);
            }
        }
    }
    if blocks.ignored > 0 {
        let units = if blocks.ignored == 1 {
            "ISCC-UNIT"
        } else {
            "ISCC-UNITs"
        };
        blocks
            .unchecked
            .push(format!("{} {units} shorter than 256 bits", blocks.ignored));
    }
    Ok(blocks)
}

/// The ISCC match of the soft bindings `bindings` with `units`, the units of the file in hand
/// (IEP-0020, Matching: only equal 256-bit Instance-Codes are an exact match), or why their
/// manifest is left out: it needs an `io.iscc.v0` block, every block must be an ISCC-SEQ, and all
/// of them together may list at most one Instance-Code, or the manifest claims more than one file
/// (IEP-0020 says it should not; this app leaves such a manifest out).
fn iscc_match(bindings: &[SoftBinding], units: &[IsccUnit]) -> Result<IsccMatch, String> {
    let blocks = iscc_blocks(bindings)?;
    if blocks.whole.is_empty() && blocks.parts.is_empty() {
        return Err("its active manifest has no ISCC soft binding".to_owned());
    }
    let instances = [
        codes(&blocks.whole, "instance"),
        codes(&blocks.parts, "instance"),
    ]
    .concat();
    if instances.iter().any(|i| *i != instances[0]) {
        return Err("it lists several Instance-Codes, so it claims more than one file".to_owned());
    }
    let own = |slug: &str| units.iter().find(|u| u.unit == slug);
    let exact =
        own("instance").is_some_and(|mine| codes(&blocks.whole, "instance").contains(&mine.iscc));
    let content = own("content").and_then(|mine| {
        codes(&blocks.whole, "content")
            .iter()
            .filter_map(|code| iscc::similarity(code, &mine.iscc).ok().flatten())
            .reduce(f64::min)
    });
    Ok(IsccMatch {
        exact,
        content,
        near: exact || content.is_some_and(|c| c >= NEAR_IDENTICAL),
        unchecked: blocks.unchecked,
    })
}

/// The codes of the units of kind `slug` in `blocks`.
fn codes(blocks: &[Vec<IsccUnit>], slug: &str) -> Vec<String> {
    blocks
        .iter()
        .flatten()
        .filter(|u| u.unit == slug)
        .map(|u| u.iscc.clone())
        .collect()
}

/// The units of an `io.iscc.v0` block that IEP-0020 matches with, and how many it ignores, those
/// shorter than 256 bits. A block that is no ISCC-SEQ of at least one unit is malformed. The
/// decoder (iscc-lib) also rejects a unit of another Version or an unknown SubType, which IEP-0020
/// says to ignore; no such unit exists yet.
fn block_units(value: &[u8]) -> Result<(Vec<IsccUnit>, usize)> {
    let codes = iscc::decode_seq(value)?;
    ensure!(!codes.is_empty(), "it lists no unit");
    let mut units = Vec::new();
    for code in &codes {
        let (_, _, _, _, body) = iscc_lib::iscc_decode(code)?;
        if body.len() * 8 == iscc::UNIT_BITS as usize {
            units.push(iscc::describe_unit(code)?);
        }
    }
    let ignored = codes.len() - units.len();
    Ok((units, ignored))
}

/// The rebuild check of the found manifest store `store`, whose own hard binding is `binding`.
fn rebuild_check(file: &InHand, store: &[u8], binding: &Binding) -> Rebuild {
    if let Some(reason) = unsupported(binding) {
        return Rebuild::Unavailable {
            reason: reason.to_owned(),
        };
    }
    let checked = tempfile::tempfile()
        .map_err(anyhow::Error::from)
        .and_then(|mut out| rebuilt(file, store, binding, &mut out));
    checked.unwrap_or_else(|e| Rebuild::Unavailable {
        reason: format!("the credentials could not be put back into this file: {e:#}"),
    })
}

/// Why the rebuild check is not made through `binding`; `None` for a data hash, a collection
/// hash, and a BMFF hash of the whole file: one with a hash value and no Merkle tree of
/// fragments, which this app does not use.
fn unsupported(binding: &Binding) -> Option<&'static str> {
    match binding {
        Binding::Data | Binding::Collection(_) => None,
        Binding::Bmff(bmff) if has_value(&bmff["hash"]) && !has_value(&bmff["merkle"]) => None,
        Binding::Bmff(_) => Some("their BMFF hash holds no hash of the whole file"),
        Binding::Boxes => Some(BOX_HASH_UNCHECKED),
        Binding::Other => Some("their own claim carries no hash of a file this app checks"),
    }
}

/// Whether a JSON value holds anything: a non-empty string, array or object.
fn has_value(value: &Value) -> bool {
    match value {
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => false,
    }
}

/// Put `store` back into the file in hand, writing to `out`, and judge the result
/// ([`verdict`]).
fn rebuilt(file: &InHand, store: &[u8], binding: &Binding, out: &mut File) -> Result<Rebuild> {
    let source_len = std::fs::metadata(file.path)?.len();
    write_rebuilt(file, store, out)?;
    let added = out.metadata()?.len().saturating_sub(source_len);
    out.rewind()?;
    let context = Context::new().with_settings(base_settings())?;
    let reader = Reader::from_context(context).with_stream(file.format.mime, &mut *out)?;
    let uncovered = match binding {
        Binding::Data => {
            let tiff_count = if file.format.mime == formats::TIFF {
                tiff_count_field(out)
            } else {
                None
            };
            Coverage::Bytes(data_hash_gap(&reader, added, tiff_count))
        }
        Binding::Bmff(bmff) => Coverage::Whole(!bmff_leaves_out_content(bmff)),
        Binding::Collection(collection) => {
            out.rewind()?;
            Coverage::Whole(lists_every_member(collection, &mut *out))
        }
        Binding::Boxes | Binding::Other => Coverage::Whole(false),
    };
    Ok(verdict(&reader, uncovered))
}

/// Write the file in hand with `store` put back into it to `out`: by c2pa-rs's writer for the
/// format, or for a PDF as an update section, the way this app signs PDFs.
fn write_rebuilt(file: &InHand, store: &[u8], out: &mut File) -> Result<()> {
    if file.format.mime == formats::PDF {
        let source = std::fs::read(file.path)?;
        out.write_all(&pdf_update::with_store(&source, store)?)?;
        return Ok(());
    }
    let mut source = File::open(file.path)?;
    c2pa::jumbf_io::save_jumbf_to_stream(file.format.mime, &mut source, out, store)?;
    Ok(())
}

/// How much of a rebuilt file its hash covers besides the credentials.
enum Coverage {
    /// Bytes a data hash leaves out besides the manifest store ([`data_hash_gap`]).
    Bytes(u64),
    /// Whether a BMFF or collection hash covers the whole file.
    Whole(bool),
}

/// Bytes the data hash of the rebuilt file read as `reader` leaves out besides the manifest store.
/// c2pa-rs 0.91 checks that one exclusion is exactly the store and lets any further one pass with
/// an informational code, so every further exclusion leaves out content, however small; only
/// `tiff_count`, the count field c2pa-rs itself leaves out of a TIFF, belongs to the store. The
/// store's own range counts beyond the `added` bytes of putting it back: an MP3's whole ID3 tag,
/// as c2pa-rs signs it, holds the file's own frames too.
fn data_hash_gap(reader: &Reader, added: u64, tiff_count: Option<(u64, u64)>) -> u64 {
    let mut lengths: Vec<u64> = inspect::data_hash_exclusions(reader)
        .unwrap_or_default()
        .into_iter()
        .filter(|&range| range.1 > 0 && Some(range) != tiff_count)
        .map(|(_, length)| length)
        .collect();
    lengths.sort_unstable();
    let store = lengths.pop().unwrap_or(0);
    let others = lengths.into_iter().fold(0, u64::saturating_add);
    others.saturating_add(store.saturating_sub(added))
}

/// Tag of the IFD entry that points at the manifest store in a TIFF.
const TIFF_C2PA_TAG: u16 = 0xcd41;
/// Most IFDs of a TIFF followed, against a chain that loops.
const MAX_IFDS: usize = 1024;

/// Offset and length of the count field of the C2PA tag's IFD entry in the classic TIFF `file`,
/// which c2pa-rs leaves out of the data hash next to the manifest store. The tag sits in the last
/// IFD of the main chain, in older files in the first. `None` for any other file, BigTIFF
/// included.
fn tiff_count_field(file: &mut File) -> Option<(u64, u64)> {
    let header = bytes_at(file, 0, 8)?;
    let little = match &header[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_of = |b: &[u8]| {
        let b = [b[0], b[1]];
        if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    };
    let u32_of = |b: &[u8]| {
        let b = [b[0], b[1], b[2], b[3]];
        if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        }
    };
    if u16_of(&header[2..4]) != 42 {
        return None;
    }
    // Where each IFD's entries start, and how many there are.
    let mut ifds = Vec::new();
    let mut next = u64::from(u32_of(&header[4..8]));
    while next != 0 && ifds.len() < MAX_IFDS {
        let count = u16_of(&bytes_at(file, next, 2)?);
        ifds.push((next + 2, count));
        next = u64::from(u32_of(&bytes_at(
            file,
            next + 2 + 12 * u64::from(count),
            4,
        )?));
    }
    [*ifds.last()?, ifds[0]]
        .into_iter()
        .find_map(|(start, count)| {
            let entries = bytes_at(file, start, 12 * usize::from(count))?;
            let i = entries
                .as_chunks::<12>()
                .0
                .iter()
                .position(|entry| u16_of(entry) == TIFF_C2PA_TAG)?;
            Some((start + 12 * i as u64 + 4, 4))
        })
}

/// `len` bytes of `file` from `offset`; `None` past its end.
fn bytes_at(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0; len];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// The UUID (base64) of the box that holds the manifest store in a BMFF file.
const C2PA_BOX_UUID: &str = "2P7D1hsOSDySl1goh37EgQ==";

/// Whether the BMFF hash `bmff` leaves out a box that may hold content: any but the manifest
/// store's and those c2pa-rs leaves out itself, `ftyp`, `mfra` and the padding of `free` and
/// `skip`.
fn bmff_leaves_out_content(bmff: &Value) -> bool {
    let store_box = json!([{ "offset": 8, "value": C2PA_BOX_UUID }]);
    bmff["exclusions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|e| match e["xpath"].as_str() {
            Some("/ftyp" | "/mfra" | "/free" | "/skip") => false,
            Some("/uuid") => e["data"] != store_box,
            _ => true,
        })
}

/// Name of the ZIP member that holds the manifest store, as c2pa-rs writes it.
const ZIP_MANIFEST: &str = "META-INF/content_credential.c2pa";

/// Whether the collection hash `collection` lists every file of the ZIP archive `zip` but the
/// manifest store: c2pa-rs checks the listed members and the central directory, where any other
/// member is bound only by its CRC-32.
fn lists_every_member(collection: &Value, zip: &mut File) -> bool {
    let Ok(mut archive) = zip::ZipArchive::new(zip) else {
        return false;
    };
    let listed = &collection["uris"];
    (0..archive.len()).all(|i| {
        archive.by_index(i).is_ok_and(|member| {
            let name = member.name();
            member.is_dir() || name == ZIP_MANIFEST || listed.get(name).is_some()
        })
    })
}

/// The rebuild check's verdict on the rebuilt file read as `reader`, whose hash covers it as
/// `coverage` says.
fn verdict(reader: &Reader, coverage: Coverage) -> Rebuild {
    if reader.validation_state() == ValidationState::Invalid {
        let reason = inspect::invalid_reason(reader, REBUILT_MISMATCH).unwrap_or(InvalidReason {
            text: REBUILT_MISMATCH.to_owned(),
            code: None,
            explanation: None,
            more: 0,
        });
        return Rebuild::Failed {
            hash_mismatch: is_hash_mismatch(&reason),
            reason,
        };
    }
    let state = format!("{:?}", reader.validation_state());
    match coverage {
        Coverage::Bytes(0) | Coverage::Whole(true) => Rebuild::Passed { state },
        Coverage::Bytes(bytes) => Rebuild::Partial {
            state,
            uncovered: Some(bytes),
        },
        Coverage::Whole(false) => Rebuild::Partial {
            state,
            uncovered: None,
        },
    }
}

/// Write the file at `path` with the found manifest store `store_id` put back into it to
/// `output`, once the rebuild check passes on exactly what is written. The file at `path` stays
/// as it is; a failure leaves no output.
pub fn restore(path: &Path, store_id: &str, output: &Path) -> Result<()> {
    let key = blake3::Hash::from_hex(store_id).map_err(|_| anyhow!("unknown credentials"))?;
    let store = FOUND
        .recall(key.as_bytes())
        .ok_or_else(|| anyhow!("these credentials are no longer at hand; search again"))?;
    if sign::same_file(path, output) {
        bail!("choose an output path different from this file");
    }
    let format = inspect::asset_format(path)?;
    let file = InHand {
        path,
        format,
        units: &[],
    };
    let binding = Binding::of(&read(&store, &file, false)?);
    if let Some(reason) = unsupported(&binding) {
        bail!("the rebuild check is not available: {reason}");
    }
    let dir = output
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new()
        .prefix("~restore")
        .tempfile_in(dir)
        .with_context(|| format!("cannot write into {}", dir.display()))?;
    match rebuilt(&file, &store, &binding, tmp.as_file_mut())? {
        Rebuild::Passed { .. } => {
            tmp.persist(output)
                .with_context(|| format!("cannot write {}", output.display()))?;
            Ok(())
        }
        Rebuild::Partial { .. } => {
            bail!("put back into this file, their hash leaves parts of it out")
        }
        Rebuild::Failed { reason, .. } => {
            bail!("the rebuilt file does not validate: {}", reason.text)
        }
        Rebuild::Unavailable { reason } => bail!("{reason}"),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use c2pa::{create_signer, Builder, BuilderIntent, SigningAlg};
    use serde_json::{json, Value};

    use super::*;
    use crate::context::{DEMO_SIGN_CERT, DEMO_SIGN_KEY};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// An empty temporary directory `name`.
    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Sign `source` into `output` as the Sign tab does by default, with the file's own title and
    /// description, so that a PDF gets no metadata revision and its source stays the file itself.
    fn sign_as_is(source: &Path, output: &Path) {
        let inspection = inspect::inspect(source).unwrap();
        let mut req = sign::tests::request("no_manifest.jpg", output);
        req.source = source.to_string_lossy().into_owned();
        req.title = inspection.meta_fields.name.clone();
        req.description = inspection.meta_fields.description.clone();
        req.meta = inspection.meta_fields.meta.clone();
        req.units = vec!["data".into(), "instance".into()];
        sign::sign(&req).unwrap();
    }

    /// The manifest store embedded in `signed` and the label of its active manifest.
    fn store_of(signed: &Path) -> (Vec<u8>, String) {
        let mime = inspect::asset_format(signed).unwrap().mime;
        let mut file = File::open(signed).unwrap();
        let store = c2pa::jumbf_io::load_jumbf_from_stream(mime, &mut file)
            .unwrap_or_else(|e| panic!("{}: {e}", signed.display()));
        let context = Context::new().with_settings(base_settings()).unwrap();
        let reader = Reader::from_context(context).with_file(signed).unwrap();
        let label = reader.active_label().unwrap().to_owned();
        (store, label)
    }

    /// `store`, named `id`, screened and examined against the file at `path` with its units.
    fn examined(path: &Path, id: &str, store: &[u8]) -> Result<Examined, String> {
        let units = inspect::inspect(path).unwrap().iscc;
        let file = InHand {
            path,
            format: inspect::asset_format(path).unwrap(),
            units: &units,
        };
        let iscc = screen(&file, id, store)?;
        examine(&file, store, iscc, true)
    }

    /// The file at `path` in hand, without units.
    fn in_hand(path: &Path) -> InHand<'_> {
        InHand {
            path,
            format: inspect::asset_format(path).unwrap(),
            units: &[],
        }
    }

    const FORMATS: [&str; 24] = [
        "no_manifest.jpg",
        "synthetic-640x480.png",
        "demo.gif",
        "meta-xmp.webp",
        "demo.tif",
        "demo.svg",
        "demo.txt",
        "demo.md",
        "demo.pdf",
        "demo.epub",
        "demo.docx",
        "demo.pptx",
        "demo.xlsx",
        "demo.odt",
        "demo.ods",
        "demo.odp",
        "demo.wav",
        "demo.mp3",
        "demo.flac",
        "demo.m4a",
        "demo.mp4",
        "demo.mov",
        "demo.m4v",
        "demo.avi",
    ];

    /// The unsigned original of a signed file, in every format: its Instance-Code equals the one
    /// in the published credentials, which C2PA rejects for it as it is, and put back into it they
    /// validate (the demo key is not trusted here) and Restore writes the signed file again, byte
    /// for byte. An MP3 is the exception: c2pa-rs leaves its whole ID3 tag out of the hash, the
    /// file's own frames included, so the rebuild check passes only in part and Restore refuses.
    #[test]
    fn credentials_put_back_into_the_original_rebuild_the_signed_file() {
        crate::tools::tests::ensure_ffmpeg();
        let dir = fresh_dir("iscc-c2pa-demo-test-rebuild");
        for file in FORMATS {
            let source = fixture(file);
            let signed = dir.join(file);
            sign_as_is(&source, &signed);
            let (store, label) = store_of(&signed);
            let found = examined(&source, &label, &store).unwrap();
            assert!(found.iscc.exact && found.iscc.near, "{file}");
            assert!(
                matches!(
                    found.for_file,
                    ForFile::Rejected {
                        hash_mismatch: true,
                        ..
                    }
                ),
                "{file}: {:?}",
                found.for_file
            );
            assert!(found.manifest.test_key && found.attributed, "{file}");
            let restored = dir.join(format!("restored-{file}"));
            let restoring = restore(&source, &keep(store), &restored);
            if file == "demo.mp3" {
                let Some(Rebuild::Partial { state, uncovered }) = &found.rebuild else {
                    panic!("{file}: {:?}", found.rebuild);
                };
                assert_eq!(state, "Valid");
                assert!(uncovered.is_some_and(|bytes| bytes > 0), "{uncovered:?}");
                assert!(restoring.is_err() && !restored.exists());
                continue;
            }
            assert_eq!(
                found.rebuild,
                Some(Rebuild::Passed {
                    state: "Valid".into()
                }),
                "{file}"
            );
            restoring.unwrap();
            assert!(
                std::fs::read(&restored).unwrap() == std::fs::read(&signed).unwrap(),
                "{file}: the restored file is not the signed file"
            );
        }
    }

    /// `source` with `extra` appended: other bytes, the same picture.
    fn changed_copy(source: &Path, output: &Path, extra: &[u8]) {
        let mut bytes = std::fs::read(source).unwrap();
        bytes.extend_from_slice(extra);
        std::fs::write(output, bytes).unwrap();
    }

    #[test]
    fn a_changed_copy_is_similar_and_never_rebuilt() {
        let dir = fresh_dir("iscc-c2pa-demo-test-changed-copy");
        let source = fixture("no_manifest.jpg");
        let signed = dir.join("signed.jpg");
        let mut req = sign::tests::request("no_manifest.jpg", &signed);
        req.units = vec!["image".into(), "data".into(), "instance".into()];
        sign::sign(&req).unwrap();
        let (store, label) = store_of(&signed);
        let changed = dir.join("changed.jpg");
        changed_copy(&source, &changed, b"trailing bytes");
        let found = examined(&changed, &label, &store).unwrap();
        assert!(!found.iscc.exact);
        assert_eq!(found.iscc.content, Some(1.0));
        assert!(found.iscc.near);
        assert_eq!(found.rebuild, None);
        // Put back by force, the hash does not match.
        let file = in_hand(&changed);
        let binding = Binding::of(&read(&store, &file, false).unwrap());
        let Rebuild::Failed {
            reason,
            hash_mismatch: true,
        } = rebuild_check(&file, &store, &binding)
        else {
            panic!("a changed file must not rebuild");
        };
        assert_eq!(reason.code.as_deref(), Some("assertion.dataHash.mismatch"));
        assert_eq!(reason.text, REBUILT_MISMATCH);
        assert!(restore(&changed, &keep(store), &dir.join("restored.jpg")).is_err());
        assert!(!dir.join("restored.jpg").exists());
    }

    #[test]
    fn a_store_holding_another_manifest_is_left_out() {
        let signed = sign::tests::sign_fixture("no_manifest.jpg", "iscc-c2pa-demo-test-other-id");
        let (store, _) = store_of(&signed);
        let reason = examined(&fixture("no_manifest.jpg"), "urn:c2pa:other", &store).unwrap_err();
        assert!(reason.contains("not urn:c2pa:other"), "{reason}");
    }

    /// A manifest store kept apart from exactly these bytes (signed into a sidecar) validates for
    /// the file as it is; this app relies on that verdict only through a data hash.
    #[test]
    fn a_store_published_apart_from_its_file_validates_for_it() {
        let dir = fresh_dir("iscc-c2pa-demo-test-found-sidecar");
        for file in ["no_manifest.jpg", "demo.m4a"] {
            let copy = sign::tests::sign_into_sidecar(file, &dir);
            let store = std::fs::read(copy.with_extension("c2pa")).unwrap();
            std::fs::remove_file(copy.with_extension("c2pa")).unwrap();
            let file_in_hand = in_hand(&copy);
            let reader = read(&store, &file_in_hand, false).unwrap();
            let label = reader.active_label().unwrap().to_owned();
            let found = examined(&copy, &label, &store).unwrap();
            assert!(found.iscc.exact, "{file}");
            let expected = if file == "demo.m4a" {
                ForFile::Unchecked {
                    reason: Binding::Bmff(Value::Null).doubt().unwrap().into(),
                }
            } else {
                ForFile::Valid {
                    state: "Valid".into(),
                }
            };
            assert_eq!(found.for_file, expected, "{file}");
        }
    }

    /// A store whose assertion was changed after signing is left out.
    #[test]
    fn a_store_changed_after_signing_is_left_out() {
        let signed = sign::tests::sign_fixture("no_manifest.jpg", "iscc-c2pa-demo-test-tampered");
        let (mut store, label) = store_of(&signed);
        let at = store
            .windows(13)
            .position(|w| w == b"research only")
            .expect("the training assertion's constraint");
        store[at + 12] = b'z';
        let reason = examined(&fixture("no_manifest.jpg"), &label, &store).unwrap_err();
        assert!(reason.contains("signature does not validate"), "{reason}");
    }

    /// Restore writes through a temporary file of its own, so a file in hand named like the old
    /// temporary sibling (`~photo.jpg`) stays.
    #[test]
    fn restore_never_touches_the_file_in_hand() {
        let dir = fresh_dir("iscc-c2pa-demo-test-restore-tilde");
        let source = dir.join("~photo.jpg");
        std::fs::copy(fixture("no_manifest.jpg"), &source).unwrap();
        let signed = dir.join("signed.jpg");
        sign_as_is(&source, &signed);
        let (store, _) = store_of(&signed);
        let original = std::fs::read(&source).unwrap();
        restore(&source, &keep(store), &dir.join("photo.jpg")).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(
            std::fs::read(dir.join("photo.jpg")).unwrap(),
            std::fs::read(&signed).unwrap()
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 3, "no temporary file is left: {names:?}");
    }

    /// The rebuild check takes only a BMFF hash that hashes the whole file and leaves out no box
    /// that may hold content.
    #[test]
    fn a_bmff_hash_counts_only_with_a_value_for_the_whole_file() {
        let store_box =
            json!({ "xpath": "/uuid", "data": [{ "offset": 8, "value": C2PA_BOX_UUID }] });
        let whole = json!({ "exclusions": [store_box, { "xpath": "/ftyp" }, { "xpath": "/free" }], "hash": "Z2l+" });
        assert_eq!(unsupported(&Binding::Bmff(whole.clone())), None);
        assert!(!bmff_leaves_out_content(&whole));
        let hashless = json!({ "exclusions": [store_box] });
        assert!(unsupported(&Binding::Bmff(hashless)).is_some());
        let merkle =
            json!({ "exclusions": [store_box], "hash": "Z2l+", "merkle": [{ "count": 1 }] });
        assert!(unsupported(&Binding::Bmff(merkle)).is_some());
        let media = json!({ "exclusions": [store_box, { "xpath": "/mdat" }], "hash": "Z2l+" });
        assert!(bmff_leaves_out_content(&media));
        let other_uuid = json!({ "exclusions": [{ "xpath": "/uuid", "data": [{ "offset": 8, "value": "AAAA" }] }] });
        assert!(bmff_leaves_out_content(&other_uuid));
    }

    /// A ZIP member the collection hash does not list is bound only by its CRC-32.
    #[test]
    fn a_collection_hash_must_list_every_member() {
        let dir = fresh_dir("iscc-c2pa-demo-test-zip-members");
        let path = dir.join("members.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        for name in ["a.txt", "b.txt", ZIP_MANIFEST] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(name.as_bytes()).unwrap();
        }
        zip.add_directory("META-INF/", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
        let listed = |uris: Value| {
            lists_every_member(&json!({ "uris": uris }), &mut File::open(&path).unwrap())
        };
        assert!(listed(json!({ "a.txt": {}, "b.txt": {} })));
        assert!(!listed(json!({ "a.txt": {} })));
    }

    #[test]
    fn restore_refuses_its_own_file_and_unknown_credentials() {
        let source = fixture("no_manifest.jpg");
        let signed = sign::tests::sign_fixture("no_manifest.jpg", "iscc-c2pa-demo-test-restore");
        let id = keep(store_of(&signed).0);
        assert!(restore(&source, &id, &source).is_err());
        let output = signed.with_file_name("restored.jpg");
        assert!(restore(&source, &"0".repeat(64), &output).is_err());
        assert!(restore(&source, "not hex", &output).is_err());
        assert!(!output.exists());
    }

    /// `source`, a JPEG, signed with the demo key under `settings`, its soft binding listing its
    /// Data-Code and Instance-Code; a box hash is computed over the source first.
    fn signed_with(source: &Path, settings: Value) -> Vec<u8> {
        let box_hash = settings["builder"]["prefer_box_hash"] == json!(true);
        let bytes = std::fs::read(source).unwrap();
        let mut builder = demo_builder(settings, "image/jpeg", &bytes);
        if box_hash {
            builder
                .update_hash_from_stream("image/jpeg", &mut Cursor::new(&bytes))
                .unwrap();
        }
        let mut output = Cursor::new(Vec::new());
        builder
            .save_to_stream("image/jpeg", &mut Cursor::new(&bytes), &mut output)
            .unwrap();
        output.into_inner()
    }

    /// A builder of a manifest for `bytes` of type `mime`, signed with the demo key under
    /// `settings`, its soft binding listing their Data-Code and Instance-Code.
    fn demo_builder(settings: Value, mime: &str, bytes: &[u8]) -> Builder {
        let signer = create_signer::from_keys(
            DEMO_SIGN_CERT.as_bytes(),
            DEMO_SIGN_KEY.as_bytes(),
            SigningAlg::Es256,
            None,
        )
        .unwrap();
        let context = Context::new()
            .with_settings(settings)
            .unwrap()
            .with_signer(signer);
        let mut builder = Builder::from_context(context)
            .with_definition(json!({ "title": "test", "format": mime }))
            .unwrap();
        builder.set_intent(BuilderIntent::Edit);
        let parent = json!({ "title": "source", "relationship": "parentOf" });
        builder
            .add_ingredient_from_stream(parent.to_string(), mime, &mut Cursor::new(bytes))
            .unwrap();
        let units: Vec<String> = iscc::bitstream_units(bytes)
            .unwrap()
            .map(|u| u.iscc)
            .to_vec();
        let sb: SoftBinding = serde_json::from_value(json!({
            "alg": ISCC_SOFT_BINDING_ALG,
            "blocks": [{ "scope": {}, "value": iscc::encode_seq(&units).unwrap() }],
        }))
        .unwrap();
        builder.add_assertion(labels::SOFT_BINDING, &sb).unwrap();
        builder
    }

    /// Credentials whose data hash leaves out content besides the manifest store cover only part
    /// of the file, however little they leave out: here four bytes of a PDF, changed in the file
    /// in hand, where putting the store back adds hundreds of hashed bytes around it.
    #[test]
    fn a_data_hash_leaving_out_content_covers_only_part() {
        let original = std::fs::read(fixture("demo.pdf")).unwrap();
        // Inside the comment on the second line, so that the changed file is still a PDF.
        let at = 10;
        let mut changed = original.clone();
        changed[at..at + 4].copy_from_slice(b"$900");
        let mut builder = demo_builder(base_settings(), formats::PDF, &changed);
        let extra = c2pa::HashRange::new(at as u64, 4);
        let signed = pdf_update::signed_leaving_out(&mut builder, &original, &[extra]).unwrap();
        let store = c2pa::jumbf_io::load_jumbf_from_memory(formats::PDF, &signed).unwrap();
        let dir = fresh_dir("iscc-c2pa-demo-test-leaving-out");
        let path = dir.join("changed.pdf");
        std::fs::write(&path, &changed).unwrap();
        assert_eq!(
            rebuild_check(&in_hand(&path), &store, &Binding::Data),
            Rebuild::Partial {
                state: "Valid".into(),
                uncovered: Some(4)
            }
        );
        let restored = dir.join("restored.pdf");
        assert!(restore(&path, &keep(store), &restored).is_err());
        assert!(!restored.exists());
    }

    /// The manifest store embedded in the JPEG `signed`, and its active label.
    fn jpeg_store(signed: &[u8]) -> (Vec<u8>, String) {
        let store = c2pa::jumbf_io::load_jumbf_from_memory("image/jpeg", signed).unwrap();
        let context = Context::new().with_settings(base_settings()).unwrap();
        let reader = Reader::from_context(context)
            .with_stream("image/jpeg", Cursor::new(signed))
            .unwrap();
        (store, reader.active_label().unwrap().to_owned())
    }

    /// c2pa-rs 0.91 passes a box hash for a file that lacks the box of the manifest store, so
    /// neither the C2PA verdict nor the rebuild check relies on one.
    #[test]
    fn a_box_hash_is_neither_judged_nor_rebuilt() {
        let source = fixture("no_manifest.jpg");
        let mut settings = base_settings();
        settings["builder"]["prefer_box_hash"] = json!(true);
        let (store, label) = jpeg_store(&signed_with(&source, settings));
        let found = examined(&source, &label, &store).unwrap();
        assert!(found.iscc.exact);
        // c2pa-rs itself calls it valid for the bare file; if this fails, upstream checks the box
        // list now and the rule below may be dropped.
        assert_eq!(found.manifest.validation_state, "Valid");
        assert_eq!(
            found.for_file,
            ForFile::Unchecked {
                reason: BOX_HASH_UNCHECKED.into()
            }
        );
        assert_eq!(
            found.rebuild,
            Some(Rebuild::Unavailable {
                reason: BOX_HASH_UNCHECKED.into()
            })
        );
    }

    /// A soft binding c2pa-rs lists as gathered by default is not the signer's statement.
    #[test]
    fn a_gathered_soft_binding_is_not_attributed() {
        let source = fixture("no_manifest.jpg");
        let mut settings = base_settings();
        settings["builder"]
            .as_object_mut()
            .unwrap()
            .remove("created_assertion_labels");
        let (store, label) = jpeg_store(&signed_with(&source, settings));
        assert!(!examined(&source, &label, &store).unwrap().attributed);
        let (store, label) = jpeg_store(&signed_with(&source, base_settings()));
        assert!(examined(&source, &label, &store).unwrap().attributed);
    }

    /// A soft binding of `alg` with one block per entry of `blocks`: its units, and whether the
    /// block is for part of the asset (a timespan).
    fn binding(alg: &str, blocks: &[(&[&str], bool)]) -> SoftBinding {
        let blocks: Vec<Value> = blocks
            .iter()
            .map(|(units, part)| {
                let units: Vec<String> = units.iter().map(|u| u.to_string()).collect();
                let scope = if *part {
                    json!({ "timespan": { "start": 0, "end": 1000 } })
                } else {
                    json!({})
                };
                json!({ "scope": scope, "value": iscc::encode_seq(&units).unwrap() })
            })
            .collect();
        serde_json::from_value(json!({ "alg": alg, "blocks": blocks })).unwrap()
    }

    // Units of the demo picture and of its copy cropped 3% a side, halved and saved at quality 60.
    const CONTENT: &str = "ISCC:EED6WDMQ2DGHONLM24NCBIEZV5V5SDMQ2DGXONLM5ENCBIE355V5TUQ";
    const CONTENT_MODIFIED: &str = "ISCC:EED6WDMQ6LHRGHKM24NCBZM5EY5ZSDOQ6LHRGHKMVUNKBZM7EY5ZSWQ";
    const DATA: &str = "ISCC:GADWYSEQ44YZMECUTBZO2GWBOISZNBVO24KF4UTHRNPRQI74E6UKRYY";
    const INSTANCE: &str = "ISCC:IADZUC27FXMNE4P7YLACBSKWIMZSVGZWYMSCN4QMV6MEEKABNLIBAPQ";
    const INSTANCE_MODIFIED: &str = "ISCC:IADZDU6WDNVGT4GDNLDUZVIAPOYWCIH73324ZFCPHURBLIMVPCSUF5Q";

    fn units(codes: &[&str]) -> Vec<IsccUnit> {
        codes
            .iter()
            .map(|c| iscc::describe_unit(c).unwrap())
            .collect()
    }

    #[test]
    fn an_equal_instance_code_is_an_exact_match() {
        let original = units(&[CONTENT, DATA, INSTANCE]);
        let sb = binding(
            ISCC_SOFT_BINDING_ALG,
            &[(&[CONTENT, DATA, INSTANCE], false)],
        );
        assert_eq!(
            iscc_match(&[sb], &original).unwrap(),
            IsccMatch {
                exact: true,
                content: Some(1.0),
                near: true,
                unchecked: vec![]
            }
        );
    }

    #[test]
    fn a_cropped_copy_is_not_near_identical() {
        let modified = units(&[CONTENT_MODIFIED, INSTANCE_MODIFIED]);
        let sb = binding(
            ISCC_SOFT_BINDING_ALG,
            &[(&[CONTENT, DATA, INSTANCE], false)],
        );
        let found = iscc_match(&[sb], &modified).unwrap();
        assert!(!found.exact && !found.near);
        assert_eq!(found.content, Some(1.0 - 46.0 / 256.0));
    }

    #[test]
    fn every_iscc_block_must_match_and_others_are_not_checked() {
        let original = units(&[CONTENT, INSTANCE]);
        let bindings = [
            binding(ISCC_SOFT_BINDING_ALG, &[(&[CONTENT, INSTANCE], false)]),
            binding(ISCC_SOFT_BINDING_ALG, &[(&[CONTENT_MODIFIED], false)]),
            binding("com.example.watermark", &[(&[CONTENT], false)]),
            binding(ISCC_SOFT_BINDING_ALG, &[(&[CONTENT], true)]),
        ];
        let found = iscc_match(&bindings, &original).unwrap();
        assert!(found.exact);
        assert_eq!(found.content, Some(1.0 - 46.0 / 256.0));
        assert_eq!(
            found.unchecked,
            ["com.example.watermark", "io.iscc.v0 for part of the asset"]
        );
    }

    /// IEP-0020 lets a block repeat a kind and has validators ignore units shorter than 256 bits.
    #[test]
    fn repeated_kinds_are_compared_and_short_units_ignored() {
        let original = units(&[CONTENT, INSTANCE]);
        let sb = binding(
            ISCC_SOFT_BINDING_ALG,
            &[(
                &[CONTENT, CONTENT_MODIFIED, "ISCC:EEA4GQZQTY6J5DTH", INSTANCE],
                false,
            )],
        );
        let found = iscc_match(&[sb], &original).unwrap();
        assert!(found.exact);
        assert_eq!(found.content, Some(1.0 - 46.0 / 256.0));
        assert_eq!(found.unchecked, ["1 ISCC-UNIT shorter than 256 bits"]);
    }

    #[test]
    fn a_soft_binding_that_claims_several_files_or_is_malformed_is_left_out() {
        let original = units(&[INSTANCE]);
        let check = |bindings: &[SoftBinding], wanted: &str| {
            let reason = iscc_match(bindings, &original).unwrap_err();
            assert!(reason.contains(wanted), "{reason}");
        };
        let two_files = [
            binding(ISCC_SOFT_BINDING_ALG, &[(&[INSTANCE], false)]),
            binding(ISCC_SOFT_BINDING_ALG, &[(&[INSTANCE_MODIFIED], true)]),
        ];
        check(&two_files, "several Instance-Codes");
        let mut empty = binding(ISCC_SOFT_BINDING_ALG, &[(&[], false)]);
        check(std::slice::from_ref(&empty), "lists no unit");
        empty.blocks.clear();
        check(&[empty], "no ISCC soft binding");
        check(
            &[binding("com.example.watermark", &[(&[INSTANCE], false)])],
            "no ISCC soft binding",
        );
    }
}
