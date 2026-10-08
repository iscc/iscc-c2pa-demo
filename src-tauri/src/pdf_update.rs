//! Incremental updates of a PDF (ISO 32000-1, 7.5.6): revisions appended to the file, every
//! earlier byte kept. Signing appends the C2PA Manifest Store as such an update section and,
//! before it, when the title, description or ISCC metadata change, a revision with the new
//! document information. The source of a PDF signed this way is the first part of the signed
//! file, which [`source_end`] finds again (the PDF source view of IEP-0020).
//!
//! Every update section starts with a single line feed, whatever the previous revision ends with,
//! so the end of the source is known to the byte.

use std::collections::HashSet;
use std::io::Cursor;

use anyhow::{anyhow, bail, ensure, Context as _, Result};
use c2pa::{Builder, HashRange};
use lopdf::content::Content;
use lopdf::xref::XrefEntry;
use lopdf::{
    dictionary, Dictionary, Document, IncrementalDocument, LoadOptions, Object, ObjectId, Stream,
    StringFormat,
};

use crate::formats;
use crate::iscc::MetaInput;
use crate::metadata;
use crate::pdf::MAX_STREAM_BYTES;

/// Name of the manifest store's embedded file, as c2pa-rs names it.
const CONTENT_CREDENTIALS: &str = "Content Credentials";
/// `AFRelationship` of the file specification of a C2PA Manifest Store.
const C2PA_MANIFEST: &[u8] = b"C2PA_Manifest";
/// Media type of a C2PA Manifest Store, the `Subtype` of its embedded file.
const C2PA_MEDIA_TYPE: &[u8] = b"application/c2pa";
/// End-of-file marker that closes every revision.
const EOF_MARKER: &[u8] = b"%%EOF";

/// `source` with a revision whose document information carries `fields` as iscc-sdk's
/// `pdf_meta_embed` writes them: the title as `/Title` and `/iscc_name`, the description as
/// `/Subject` and `/iscc_description`, the ISCC metadata as `/iscc_meta`. A missing (or blank)
/// description or ISCC metadata removes its keys; a missing description also leaves the XMP
/// packet, where readers look next. An `/Info` that is no dictionary (null, a reference to no
/// object) is missing, as readers take it, and gets a new one.
pub fn with_metadata(source: &[u8], fields: MetaInput<'_>) -> Result<Vec<u8>> {
    let title = fields.name.ok_or_else(|| anyhow!("a title is required"))?;
    let description = given(fields.description);
    let mut update = update_of(source)?;
    let slot = (update.new_document.trailer.get(b"Info").ok().cloned())
        .filter(|slot| matches!(resolved(&update, slot), Ok(Object::Dictionary(_))));
    let mut info = match &slot {
        Some(slot) => resolved(&update, slot)?.as_dict()?.clone(),
        None => Dictionary::new(),
    };
    set_text(&mut info, &[b"Title", b"iscc_name"], Some(title));
    set_text(&mut info, &[b"Subject", b"iscc_description"], description);
    set_text(&mut info, &[b"iscc_meta"], given(fields.meta));
    let info = match slot {
        Some(slot) => store(&mut update, &slot, Object::Dictionary(info)),
        None => Object::Reference(update.new_document.add_object(info)),
    };
    update.new_document.trailer.set("Info", info);
    if description.is_none() {
        without_xmp_description(&mut update)?;
    }
    saved(update)
}

/// `value` unless it sanitises to nothing, which a reader takes as missing.
fn given(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !metadata::sanitize(v).is_empty())
}

/// Take `dc:description` out of the XMP packet in the catalog's `/Metadata` stream of `update`,
/// as a revision of that stream (unfiltered, the rest of the packet byte for byte). Nothing
/// happens without such a packet or property.
fn without_xmp_description(update: &mut IncrementalDocument) -> Result<()> {
    let root = update.new_document.trailer.get(b"Root")?.clone();
    let catalog = resolved(update, &root)?.as_dict()?.clone();
    let Ok(slot) = catalog.get(b"Metadata").cloned() else {
        return Ok(());
    };
    let object = resolved(update, &slot)?;
    let stream = object.as_stream()?;
    let packet = stream.get_plain_content_with_limit(MAX_STREAM_BYTES)?;
    let Some(stripped) = metadata::xmp_without(&packet, metadata::NS_DC, "description") else {
        return Ok(());
    };
    let dict = without(&stream.dict, &[b"Filter", b"DecodeParms", b"Length"]);
    let mut revised = Stream::new(dict, stripped);
    revised.allows_compression = false;
    ensure!(
        matches!(slot, Object::Reference(_)),
        "the XMP packet is not an indirect object"
    );
    store(update, &slot, Object::Stream(revised));
    Ok(())
}

/// `source` signed with the manifest of `builder`, appended as an update section: the space for
/// the manifest store is reserved first, the file hashed around it, then the signed store
/// written into its place.
pub fn signed(builder: &mut Builder, source: &[u8]) -> Result<Vec<u8>> {
    let placeholder = builder.placeholder(formats::PDF)?;
    let mut update = update_of(source)?;
    add_manifest(&mut update, &placeholder)?;
    let mut out = saved(update)?;
    let start = find(&out, &placeholder, source.len())
        .ok_or_else(|| anyhow!("the manifest store is missing from the update"))?;
    builder
        .set_data_hash_exclusions(vec![HashRange::new(start as u64, placeholder.len() as u64)])?;
    builder.update_hash_from_stream(formats::PDF, &mut Cursor::new(&out))?;
    let manifest = builder.sign_embeddable(formats::PDF)?;
    ensure!(
        manifest.len() == placeholder.len(),
        "the signed manifest store does not fit its reserved space"
    );
    out[start..start + manifest.len()].copy_from_slice(&manifest);
    Ok(out)
}

/// Where the source of a PDF signed by an update section ends: before the line feed that starts
/// the update section holding the manifest store at `manifest_start`, provided that update adds
/// nothing but the manifest store. `None` for any other PDF.
pub fn source_end(bytes: &[u8], manifest_start: usize) -> Option<usize> {
    let before = bytes.get(..manifest_start)?;
    let eof = before
        .windows(EOF_MARKER.len())
        .rposition(|w| w == EOF_MARKER)?
        + EOF_MARKER.len();
    let first = eof + before[eof..].iter().position(|&b| !is_white(b))?;
    let end = first - 1;
    let separated = end >= eof && bytes[end] == b'\n';
    (separated && object_header(&bytes[first..]).is_some() && only_adds_manifest(bytes, end))
        .then_some(end)
}

/// A new revision of the PDF in `source`, appended after a single line feed.
fn update_of(source: &[u8]) -> Result<IncrementalDocument> {
    let document = load(source).context("cannot read the PDF")?;
    ensure!(
        !encrypted(&document),
        "cannot add a revision to an encrypted PDF"
    );
    let mut previous = Vec::with_capacity(source.len() + 1);
    previous.extend_from_slice(source);
    previous.push(b'\n');
    let mut update = IncrementalDocument::create_from(previous, document);
    // A hybrid file's pointer to its cross-reference stream belongs to the earlier revision.
    update.new_document.trailer.remove(b"XRefStm");
    Ok(update)
}

/// Whether `doc` is encrypted: lopdf drops `/Encrypt` from the trailer of a PDF it decrypted
/// with the empty password.
fn encrypted(doc: &Document) -> bool {
    doc.was_encrypted() || doc.is_encrypted()
}

/// lopdf's view of the PDF in `bytes`, each stream capped at what it may inflate to.
fn load(bytes: &[u8]) -> lopdf::Result<Document> {
    Document::load_mem_with_options(
        bytes,
        LoadOptions::with_max_decompressed_size(MAX_STREAM_BYTES),
    )
}

/// The bytes of the PDF with `update` appended.
fn saved(mut update: IncrementalDocument) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    update.save_to(&mut out)?;
    Ok(out)
}

/// Add `manifest` to `update` as the C2PA Manifest Store: an unfiltered embedded file, its file
/// specification, and the catalog listing it in `/AF` and `/Names /EmbeddedFiles`, where it
/// replaces any earlier manifest store; other associated and embedded files stay.
fn add_manifest(update: &mut IncrementalDocument, manifest: &[u8]) -> Result<()> {
    let mut stream = Stream::new(
        dictionary! {
            "Type" => Object::Name(b"EmbeddedFile".to_vec()),
            "Subtype" => Object::Name(C2PA_MEDIA_TYPE.to_vec()),
        },
        manifest.to_vec(),
    );
    stream.allows_compression = false;
    let stream_id = update.new_document.add_object(stream);
    let spec = update.new_document.add_object(dictionary! {
        "Type" => Object::Name(b"Filespec".to_vec()),
        "F" => Object::string_literal(CONTENT_CREDENTIALS),
        "UF" => Object::string_literal(CONTENT_CREDENTIALS),
        "Desc" => Object::string_literal(CONTENT_CREDENTIALS),
        "AFRelationship" => Object::Name(C2PA_MANIFEST.to_vec()),
        "Subtype" => Object::Name(C2PA_MEDIA_TYPE.to_vec()),
        "EF" => dictionary! { "F" => Object::Reference(stream_id) },
    });
    let root = update.new_document.trailer.get(b"Root")?.clone();
    ensure!(
        matches!(root, Object::Reference(_)),
        "the PDF catalog is not an indirect object"
    );
    let mut catalog = resolved(update, &root)?.as_dict()?.clone();
    let af = with_associated_file(update, &catalog, spec)?;
    catalog.set("AF", af);
    let names = with_embedded_file(update, &catalog, spec)?;
    catalog.set("Names", names);
    store(update, &root, Object::Dictionary(catalog));
    Ok(())
}

/// The catalog's `/AF` with `spec` in place of any C2PA file specification, kept where it was.
fn with_associated_file(
    update: &mut IncrementalDocument,
    catalog: &Dictionary,
    spec: ObjectId,
) -> Result<Object> {
    let slot = catalog
        .get(b"AF")
        .cloned()
        .unwrap_or(Object::Array(Vec::new()));
    let mut files = resolved(update, &slot)?.as_array()?.clone();
    files.retain(|f| !is_c2pa_spec(update.get_prev_documents(), f));
    files.push(Object::Reference(spec));
    Ok(store(update, &slot, Object::Array(files)))
}

/// The catalog's `/Names` with the manifest store `spec` listed in `/EmbeddedFiles` in place of
/// any earlier one; each dictionary and array kept where it was.
fn with_embedded_file(
    update: &mut IncrementalDocument,
    catalog: &Dictionary,
    spec: ObjectId,
) -> Result<Object> {
    let empty = || Object::Dictionary(Dictionary::new());
    let names_slot = catalog.get(b"Names").cloned().unwrap_or_else(|_| empty());
    let mut names = resolved(update, &names_slot)?.as_dict()?.clone();
    let files_slot = names
        .get(b"EmbeddedFiles")
        .cloned()
        .unwrap_or_else(|_| empty());
    let mut files = resolved(update, &files_slot)?.as_dict()?.clone();
    if files.has(b"Kids") {
        bail!("embedded files listed in a name tree with intermediate nodes are not supported");
    }
    let list_slot = files
        .get(b"Names")
        .cloned()
        .unwrap_or(Object::Array(Vec::new()));
    let list = resolved(update, &list_slot)?.as_array()?.clone();
    let previous = update.get_prev_documents();
    let mut pairs: Vec<Object> = list
        .chunks(2)
        .filter(|pair| pair.len() == 2 && !is_c2pa_spec(previous, &pair[1]))
        .flatten()
        .cloned()
        .collect();
    pairs.push(Object::string_literal(CONTENT_CREDENTIALS));
    pairs.push(Object::Reference(spec));
    files.set("Names", store(update, &list_slot, Object::Array(pairs)));
    names.set(
        "EmbeddedFiles",
        store(update, &files_slot, Object::Dictionary(files)),
    );
    Ok(store(update, &names_slot, Object::Dictionary(names)))
}

/// A copy of `object`, followed through a reference to the newest revision that defines it.
fn resolved(update: &IncrementalDocument, object: &Object) -> Result<Object> {
    let Object::Reference(id) = object else {
        return Ok(object.clone());
    };
    let found = match update.new_document.get_object(*id) {
        Ok(found) => found,
        Err(_) => update.get_prev_documents().get_object(*id)?,
    };
    Ok(found.clone())
}

/// `value` where `slot` had its value: under the same object number in the update when `slot` is
/// a reference, else in place. Returns what the parent holds.
fn store(update: &mut IncrementalDocument, slot: &Object, value: Object) -> Object {
    match slot {
        Object::Reference(id) => {
            update.new_document.set_object(*id, value);
            Object::Reference(*id)
        }
        _ => value,
    }
}

/// Set each of `keys` to the text `value` as a PDF text string, or remove them without a value.
fn set_text(dict: &mut Dictionary, keys: &[&[u8]], value: Option<&str>) {
    for key in keys {
        match value {
            Some(text) => dict.set(key.to_vec(), text_string(text)),
            None => {
                dict.remove(key);
            }
        }
    }
}

/// A PDF text string: plain ASCII as it is, anything else UTF-16BE after its byte order mark.
fn text_string(text: &str) -> Object {
    if text.is_ascii() {
        return Object::string_literal(text);
    }
    let utf16 = text.encode_utf16().flat_map(u16::to_be_bytes);
    let bytes = [0xFE, 0xFF].into_iter().chain(utf16).collect();
    Object::String(bytes, StringFormat::Hexadecimal)
}

/// Offset of the first `needle` in `hay` at or after `from`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// PDF white space (ISO 32000-1, Table 1).
fn is_white(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0C | b'\r' | b' ')
}

/// The object number and generation of the object header that `bytes` start with (number,
/// generation, `obj`), if they start with one.
fn object_header(bytes: &[u8]) -> Option<ObjectId> {
    let mut tokens = Tokens { bytes, pos: 0 };
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let number = u32::try_from(tokens.int()?).ok()?;
    let generation = u16::try_from(tokens.int()?).ok()?;
    tokens
        .next()?
        .starts_with(b"obj")
        .then_some((number, generation))
}

/// Whether `before` has object `number` in use: defined, in a stream or not.
fn in_use(doc: &Document, number: u32) -> bool {
    matches!(
        doc.reference_table.get(number),
        Some(XrefEntry::Normal { .. } | XrefEntry::Compressed { .. })
    )
}

/// White-space separated tokens of `bytes` from `pos` on.
struct Tokens<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Tokens<'a> {
    /// The next token; `None` at the end.
    fn next(&mut self) -> Option<&'a [u8]> {
        let rest = self.bytes.get(self.pos..)?;
        let start = rest.iter().position(|&b| !is_white(b))?;
        let len = rest[start..]
            .iter()
            .position(|&b| is_white(b))
            .unwrap_or(rest.len() - start);
        self.pos += start + len;
        Some(&rest[start..start + len])
    }

    /// The next token as an unsigned decimal number.
    fn int(&mut self) -> Option<u64> {
        parse_int(self.next()?)
    }
}

/// Whether `object` is the file specification of a C2PA Manifest Store in `doc`.
fn is_c2pa_spec(doc: &Document, object: &Object) -> bool {
    doc.dereference(object)
        .ok()
        .and_then(|(_, o)| o.as_dict().ok())
        .and_then(|d| d.get(b"AFRelationship").ok())
        .and_then(|r| r.as_name().ok())
        .is_some_and(|name| name == C2PA_MANIFEST)
}

/// Whether the revisions after the first `end` bytes of `bytes` add nothing but a C2PA Manifest
/// Store, so that the file renders as its first part did. Never for an encrypted PDF: a revision
/// that adds, drops or changes the encryption changes how every earlier object decodes.
fn only_adds_manifest(bytes: &[u8], end: usize) -> bool {
    match (load(&bytes[..end]), load(bytes)) {
        (Ok(before), Ok(after)) if !encrypted(&before) && !encrypted(&after) => {
            adds_only_manifest(&before, &after, bytes, end).unwrap_or(false)
        }
        _ => false,
    }
}

/// Whether `after`, the PDF `before` with the revisions from byte `end` of `bytes` on, changes
/// no object but those that list the manifest store (the catalog in `/AF` and `/Names
/// /EmbeddedFiles` only, and the arrays and dictionaries on those paths, each kept under its
/// number) and defines every other object, the store's file specification and embedded file
/// among them, under a number `before` never used. The cross-reference sections of those
/// revisions must say so themselves ([`update_defines`]): what lopdf resolves is only trusted
/// for the content of the objects they define. Anything else, malformed revisions included, is
/// `false` or an error.
fn adds_only_manifest(
    before: &Document,
    after: &Document,
    bytes: &[u8],
    end: usize,
) -> Result<bool> {
    let root = after.trailer.get(b"Root")?.as_reference()?;
    let same_trailer = before.trailer.get(b"Root")?.as_reference()? == root
        && before.trailer.get(b"Info").ok() == after.trailer.get(b"Info").ok();
    let catalog = (before.get_dictionary(root)?, after.get_dictionary(root)?);
    if !same_trailer
        || without(catalog.0, &[b"AF", b"Names"]) != without(catalog.1, &[b"AF", b"Names"])
    {
        return Ok(false);
    }
    let mut listed = HashSet::from([root]);
    if !(associated_files_listed(before, after, catalog, &mut listed)?
        && embedded_files_listed(before, after, catalog, &mut listed)?)
    {
        return Ok(false);
    }
    let Some(defined) = update_defines(bytes, end, before, after) else {
        return Ok(false);
    };
    Ok(defined
        .iter()
        .all(|id| listed.contains(id) || (is_xref_stream(after, *id) && !in_use(before, id.0))))
}

/// The objects that the cross-reference sections of the revisions from byte `end` of `bytes`
/// on define, with their generation. `None` unless those sections, walked from the file's
/// `startxref` through `/Prev`, lie in those revisions and point only at object headers in
/// them that carry the entry's number, free only numbers `before` never used, hold no
/// compressed objects, no `/XRefStm` and no `/Encrypt`, and continue at the cross-reference section `before`
/// starts with: every other object then resolves as it does in `before`.
fn update_defines(
    bytes: &[u8],
    end: usize,
    before: &Document,
    after: &Document,
) -> Option<Vec<ObjectId>> {
    let mut pending = vec![after.xref_start];
    let mut seen = HashSet::new();
    let mut defined = Vec::new();
    let mut continued = false;
    while let Some(at) = pending.pop() {
        if at < end {
            (at == before.xref_start && at > 0).then_some(())?;
            continued = true;
            continue;
        }
        if !seen.insert(at) {
            continue;
        }
        let section = section_at(bytes, at, after)?;
        pending.extend(section.prev);
        for entry in section.entries {
            match entry {
                Entry::InUse { id, offset } => {
                    (offset >= end && object_header(bytes.get(offset..)?) == Some(id))
                        .then_some(())?;
                    defined.push(id);
                }
                Entry::Free(number) => (!in_use(before, number)).then_some(())?,
                Entry::Compressed => return None,
            }
        }
    }
    (continued && after.xref_start >= end).then_some(defined)
}

/// A cross-reference section as read from the file: its entries and the offset its trailer
/// names in `/Prev`.
struct Section {
    entries: Vec<Entry>,
    prev: Option<usize>,
}

/// An entry of a cross-reference section.
enum Entry {
    /// An object defined at `offset`.
    InUse { id: ObjectId, offset: usize },
    /// A free object number.
    Free(u32),
    /// An object in an object stream.
    Compressed,
}

/// The cross-reference section at byte `at` of `bytes`: a table or a stream, the stream read
/// through `after`. `None` when neither is there or the trailer names an `/XRefStm` or an
/// `/Encrypt`.
fn section_at(bytes: &[u8], at: usize, after: &Document) -> Option<Section> {
    let rest = bytes.get(at..)?;
    if rest.starts_with(b"xref") {
        table_section(rest)
    } else {
        stream_section(bytes, at, after)
    }
}

/// The cross-reference table that `rest` starts with (ISO 32000-1, 7.5.4) and its trailer.
fn table_section(rest: &[u8]) -> Option<Section> {
    let mut tokens = Tokens {
        bytes: rest,
        pos: 0,
    };
    (tokens.next()? == b"xref").then_some(())?;
    let mut entries = Vec::new();
    let trailer = loop {
        let token = tokens.next()?;
        if token.starts_with(b"trailer") {
            break tokens.pos - token.len() + b"trailer".len();
        }
        let start = u32::try_from(parse_int(token)?).ok()?;
        for i in 0..tokens.int()? {
            let number = start.checked_add(u32::try_from(i).ok()?)?;
            let offset = usize::try_from(tokens.int()?).ok()?;
            let generation = u16::try_from(tokens.int()?).ok()?;
            entries.push(match tokens.next()? {
                b"n" => Entry::InUse {
                    id: (number, generation),
                    offset,
                },
                b"f" => Entry::Free(number),
                _ => return None,
            });
        }
    };
    let dict = trailer_dict(&rest[trailer..])?;
    if dict.has(b"XRefStm") || dict.has(b"Encrypt") {
        return None;
    }
    let prev = match dict.get(b"Prev") {
        Ok(prev) => Some(usize::try_from(prev.as_i64().ok()?).ok()?),
        Err(_) => None,
    };
    Some(Section { entries, prev })
}

/// The trailer dictionary that `bytes` start with, up to `startxref`, parsed as lopdf parses a
/// trailer (escaped names decoded, strings skipped, the last of repeated keys); lopdf exposes
/// its dictionary parser only through its content parser.
fn trailer_dict(bytes: &[u8]) -> Option<Dictionary> {
    let end = find(bytes, b"startxref", 0)? + b"startxref".len();
    let content = Content::decode_strict(&bytes[..end]).ok()?;
    match content.operations.as_slice() {
        [operation] if operation.operator == "startxref" => match operation.operands.as_slice() {
            [Object::Dictionary(dict)] => Some(dict.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The cross-reference stream at byte `at` of `bytes` (ISO 32000-1, 7.5.8), which must list
/// itself there so that `after` resolves it.
fn stream_section(bytes: &[u8], at: usize, after: &Document) -> Option<Section> {
    let id = object_header(bytes.get(at..)?)?;
    let listed_there = matches!(
        after.reference_table.get(id.0),
        Some(XrefEntry::Normal { offset, generation })
            if *offset as usize == at && *generation == id.1
    );
    let stream = after.get_object(id).ok()?.as_stream().ok()?;
    let dict = &stream.dict;
    let is_xref = dict.get(b"Type").ok()?.as_name().ok()? == b"XRef";
    let plain = !dict.has(b"XRefStm") && !dict.has(b"Encrypt");
    (listed_there && is_xref && plain).then_some(())?;
    let prev = dict
        .get(b"Prev")
        .ok()
        .and_then(|p| usize::try_from(p.as_i64().ok()?).ok());
    let widths: Vec<usize> = dict
        .get(b"W")
        .ok()?
        .as_array()
        .ok()?
        .iter()
        .map(|w| usize::try_from(w.as_i64().ok()?).ok())
        .collect::<Option<_>>()?;
    let index: Vec<u64> = match dict.get(b"Index") {
        Ok(index) => index
            .as_array()
            .ok()?
            .iter()
            .map(|n| u64::try_from(n.as_i64().ok()?).ok())
            .collect::<Option<_>>()?,
        Err(_) => vec![
            0,
            u64::try_from(dict.get(b"Size").ok()?.as_i64().ok()?).ok()?,
        ],
    };
    let data = stream
        .decompressed_content_with_limit(MAX_STREAM_BYTES)
        .ok()?;
    let entries = stream_entries(&data, &widths, &index)?;
    Some(Section { entries, prev })
}

/// The entries of a cross-reference stream's `data`, fields `widths` wide, for the object
/// number ranges in `index`; `None` when `data` has not exactly those.
fn stream_entries(data: &[u8], widths: &[usize], index: &[u64]) -> Option<Vec<Entry>> {
    let [w0, w1, w2] = widths.try_into().ok()?;
    let mut entries = Vec::new();
    let mut pos = 0;
    let mut field = |width: usize| -> Option<u64> {
        let bytes = data.get(pos..pos + width)?;
        pos += width;
        Some(bytes.iter().fold(0, |n, &b| (n << 8) | u64::from(b)))
    };
    for range in index.chunks(2) {
        let [start, count] = range.try_into().ok()?;
        for i in 0..count {
            let number = u32::try_from(start.checked_add(i)?).ok()?;
            let kind = if w0 == 0 { 1 } else { field(w0)? };
            let second = field(w1)?;
            let third = field(w2)?;
            entries.push(match kind {
                0 => Entry::Free(number),
                1 => Entry::InUse {
                    id: (number, u16::try_from(third).ok()?),
                    offset: usize::try_from(second).ok()?,
                },
                2 => Entry::Compressed,
                _ => return None,
            });
        }
    }
    (pos == data.len()).then_some(entries)
}

/// `token` as an unsigned decimal number.
fn parse_int(token: &[u8]) -> Option<u64> {
    (!token.is_empty() && token.iter().all(u8::is_ascii_digit))
        .then(|| std::str::from_utf8(token).ok()?.parse().ok())
        .flatten()
}

/// Whether the catalog's `/AF` in `after` holds what it held in `before`, earlier C2PA file
/// specifications aside, plus C2PA file specifications, which go into `listed` with the array
/// and their embedded files.
fn associated_files_listed(
    before: &Document,
    after: &Document,
    catalog: (&Dictionary, &Dictionary),
    listed: &mut HashSet<ObjectId>,
) -> Result<bool> {
    let files: Vec<&[Object]> = match catalog.1.get(b"AF") {
        Ok(slot) => {
            list_slot(before, slot, catalog.0.get(b"AF").ok(), listed)?;
            after.dereference(slot)?.1.as_array()?.chunks(1).collect()
        }
        Err(_) => Vec::new(),
    };
    let earlier: Vec<&[Object]> = match catalog.0.get(b"AF") {
        Ok(slot) => before.dereference(slot)?.1.as_array()?.chunks(1).collect(),
        Err(_) => Vec::new(),
    };
    Ok(others_kept(before, &earlier, &files)
        && files
            .iter()
            .all(|f| earlier.contains(f) || list_manifest(before, after, &f[0], listed)))
}

/// Whether every entry of `earlier` (a file specification, or a name and a file specification)
/// whose file specification is not a C2PA one in `before` is still in `now`.
fn others_kept(before: &Document, earlier: &[&[Object]], now: &[&[Object]]) -> bool {
    earlier
        .iter()
        .all(|entry| entry.last().is_some_and(|f| is_c2pa_spec(before, f)) || now.contains(entry))
}

/// Whether `/Names /EmbeddedFiles` in `after` lists what it listed in `before`, earlier C2PA
/// file specifications aside, plus C2PA file specifications, which go into `listed` with the
/// dictionaries and the array on the way.
fn embedded_files_listed(
    before: &Document,
    after: &Document,
    catalog: (&Dictionary, &Dictionary),
    listed: &mut HashSet<ObjectId>,
) -> Result<bool> {
    let Some((names, earlier_names)) = level(before, after, catalog, b"Names", listed)? else {
        return Ok(true);
    };
    if without(&earlier_names, &[b"EmbeddedFiles"]) != without(&names, &[b"EmbeddedFiles"]) {
        return Ok(false);
    }
    let pair = (&earlier_names, &names);
    let Some((files, earlier_files)) = level(before, after, pair, b"EmbeddedFiles", listed)? else {
        return Ok(true);
    };
    if without(&earlier_files, &[b"Names"]) != without(&files, &[b"Names"]) {
        return Ok(false);
    }
    let list: Vec<&[Object]> = match files.get(b"Names") {
        Ok(slot) => {
            list_slot(before, slot, earlier_files.get(b"Names").ok(), listed)?;
            after.dereference(slot)?.1.as_array()?.chunks(2).collect()
        }
        Err(_) => Vec::new(),
    };
    let earlier: Vec<&[Object]> = match earlier_files.get(b"Names") {
        Ok(slot) => before.dereference(slot)?.1.as_array()?.chunks(2).collect(),
        Err(_) => Vec::new(),
    };
    Ok(others_kept(before, &earlier, &list)
        && list.iter().all(|pair| {
            earlier.contains(pair)
                || (pair.len() == 2 && list_manifest(before, after, &pair[1], listed))
        }))
}

/// The dictionary under `key` in the parents `(before, after)`, in both documents (empty where
/// missing); its object number goes into `listed`. `None` when neither has one.
fn level(
    before: &Document,
    after: &Document,
    parents: (&Dictionary, &Dictionary),
    key: &[u8],
    listed: &mut HashSet<ObjectId>,
) -> Result<Option<(Dictionary, Dictionary)>> {
    let now = match parents.1.get(key) {
        Ok(slot) => {
            list_slot(before, slot, parents.0.get(key).ok(), listed)?;
            after.dereference(slot)?.1.as_dict()?.clone()
        }
        Err(_) if parents.0.has(key) => Dictionary::new(),
        Err(_) => return Ok(None),
    };
    let earlier = match parents.0.get(key) {
        Ok(slot) => before.dereference(slot)?.1.as_dict()?.clone(),
        Err(_) => Dictionary::new(),
    };
    Ok(Some((now, earlier)))
}

/// Note the reference `slot` on a path to the manifest store in `listed`. It may be the same
/// reference as `earlier`, the slot's value in `before`, or a number `before` never used; an
/// object of `before` taken over this way is an error.
fn list_slot(
    before: &Document,
    slot: &Object,
    earlier: Option<&Object>,
    listed: &mut HashSet<ObjectId>,
) -> Result<()> {
    if let Some(id) = reference(slot) {
        ensure!(
            earlier == Some(slot) || !in_use(before, id.0),
            "a path to the manifest store takes over object {}",
            id.0
        );
        listed.insert(id);
    }
    Ok(())
}

/// Whether `object` is a C2PA file specification in `after` under a number `before` never
/// used, with its embedded file under another such number; if so, both go into `listed`.
fn list_manifest(
    before: &Document,
    after: &Document,
    object: &Object,
    listed: &mut HashSet<ObjectId>,
) -> bool {
    let unused = |id: &ObjectId| !in_use(before, id.0);
    let Some(spec) = reference(object).filter(unused) else {
        return false;
    };
    if !is_c2pa_spec(after, object) {
        return false;
    }
    let file = after
        .dereference(object)
        .ok()
        .and_then(|(_, o)| o.as_dict().ok())
        .and_then(|d| d.get(b"EF").ok())
        .and_then(|ef| after.dereference(ef).ok())
        .and_then(|(_, ef)| ef.as_dict().ok()?.get(b"F").ok().cloned());
    let Some(stream) = file.as_ref().and_then(reference).filter(unused) else {
        return false;
    };
    listed.extend([spec, stream]);
    true
}

/// The object number `object` refers to, if it is a reference.
fn reference(object: &Object) -> Option<ObjectId> {
    object.as_reference().ok()
}

/// `dict` without `keys`.
fn without(dict: &Dictionary, keys: &[&[u8]]) -> Dictionary {
    let mut copy = dict.clone();
    for key in keys {
        copy.remove(key);
    }
    copy
}

/// Whether object `id` of `doc` is a cross-reference stream, which an update section may add.
fn is_xref_stream(doc: &Document, id: ObjectId) -> bool {
    doc.get_object(id)
        .and_then(Object::as_stream)
        .ok()
        .and_then(|s| s.dict.get(b"Type").ok())
        .and_then(|t| t.as_name().ok())
        .is_some_and(|name| name == b"XRef")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{base_settings, DEMO_SIGN_CERT, DEMO_SIGN_KEY};
    use crate::{inspect, pdf};
    use c2pa::{create_signer, BuilderIntent, Context, Reader, SigningAlg, ValidationState};
    use serde_json::json;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        std::fs::read(dir.join(name)).unwrap()
    }

    /// A builder with the demo signer, a title and `source` as parent ingredient, as the app
    /// signs.
    fn builder(source: &[u8]) -> Builder {
        let signer = create_signer::from_keys(
            DEMO_SIGN_CERT.as_bytes(),
            DEMO_SIGN_KEY.as_bytes(),
            SigningAlg::Es256,
            None,
        )
        .unwrap();
        let context = Context::new()
            .with_settings(base_settings())
            .unwrap()
            .with_signer(signer);
        let mut builder = Builder::from_context(context)
            .with_definition(json!({ "title": "test", "format": formats::PDF }))
            .unwrap();
        builder.set_intent(BuilderIntent::Edit);
        let parent = json!({ "title": "source.pdf", "relationship": "parentOf" });
        builder
            .add_ingredient_from_stream(
                parent.to_string(),
                formats::PDF,
                &mut Cursor::new(source.to_vec()),
            )
            .unwrap();
        builder
    }

    /// The manifest store of the PDF in `bytes`, validated.
    fn reader(bytes: &[u8]) -> Reader {
        let context = Context::new().with_settings(base_settings()).unwrap();
        Reader::from_context(context)
            .with_stream(formats::PDF, Cursor::new(bytes.to_vec()))
            .unwrap()
    }

    /// `source` signed, checked to validate, with where its manifest store starts.
    fn sign_checked(source: &[u8]) -> (Vec<u8>, usize) {
        let out = signed(&mut builder(source), source).unwrap();
        let reader = reader(&out);
        assert_eq!(reader.validation_state(), ValidationState::Trusted);
        let exclusions = inspect::data_hash_exclusions(&reader).unwrap();
        assert_eq!(exclusions.len(), 1);
        (out, exclusions[0].0 as usize)
    }

    #[test]
    fn signed_pdfs_start_with_their_source() {
        // Endings: CR LF, a bare CR (with cross-reference streams), LF, none; a digital signature.
        for file in [
            "demo.pdf",
            "basic.pdf",
            "rtl.pdf",
            "scan.pdf",
            "basic-retest.pdf",
        ] {
            let source = fixture(file);
            let (out, start) = sign_checked(&source);
            assert!(out.starts_with(&source), "{file}");
            assert_eq!(out[source.len()], b'\n', "{file}");
            assert_eq!(source_end(&out, start), Some(source.len()), "{file}");
            assert_eq!(source_end(&source, source.len()), None, "{file} unsigned");
        }
    }

    #[test]
    fn a_signed_pdf_signed_again_is_the_source_of_the_second() {
        let (once, _) = sign_checked(&fixture("demo.pdf"));
        let (twice, start) = sign_checked(&once);
        assert!(twice.starts_with(&once));
        assert_eq!(source_end(&twice, start), Some(once.len()));
        let doc = load(&twice).unwrap();
        let catalog = doc.catalog().unwrap();
        let af = doc.dereference(catalog.get(b"AF").unwrap()).unwrap().1;
        assert_eq!(af.as_array().unwrap().len(), 1, "only the active store");
    }

    #[test]
    fn metadata_revision_is_read_back() {
        let source = fixture("demo.pdf");
        let fields = MetaInput {
            name: Some("Neuer Titel für Zürich"),
            description: Some("A description (with parentheses)"),
            meta: Some("data:application/json;base64,e30="),
        };
        let edited = with_metadata(&source, fields).unwrap();
        assert!(edited.starts_with(&source));
        let read = pdf::read(&edited).unwrap().document;
        assert_eq!(read.title.as_deref(), fields.name);
        assert_eq!(read.description.as_deref(), fields.description);
        assert_eq!(read.meta.as_deref(), fields.meta);
        // The text is the source's.
        assert_eq!(read.text, pdf::read(&source).unwrap().document.text);

        let cleared = MetaInput {
            description: None,
            meta: None,
            ..fields
        };
        let again = with_metadata(&edited, cleared).unwrap();
        let read = pdf::read(&again).unwrap().document;
        assert_eq!(read.title.as_deref(), fields.name);
        assert_eq!((read.description, read.meta), (None, None));
    }

    #[test]
    fn a_removed_description_leaves_the_xmp_packet_too() {
        // meta-xmp-only.pdf carries its fields in XMP alone, where readers look after the
        // document information.
        let source = fixture("meta-xmp-only.pdf");
        let before = pdf::read(&source).unwrap().document;
        assert_eq!(before.description.as_deref(), Some("Deutsche Beschreibung"));
        let fields = MetaInput {
            name: Some("Title"),
            description: Some(" "),
            meta: None,
        };
        let edited = with_metadata(&source, fields).unwrap();
        let read = pdf::read(&edited).unwrap().document;
        assert_eq!(read.title.as_deref(), Some("Title"));
        assert_eq!(read.description, None);
        assert_eq!(read.creator, before.creator, "the rest of the packet stays");
        assert_eq!(read.text, before.text);

        // With a description given, the document information wins and the packet is left alone.
        let described = MetaInput {
            description: Some("New"),
            ..fields
        };
        let described = with_metadata(&source, described).unwrap();
        let read = pdf::read(&described).unwrap().document;
        assert_eq!(read.description.as_deref(), Some("New"));
        assert_eq!(find(&described, b"xpacket", source.len()), None);
    }

    /// `source` with an ordinary attachment listed in `/AF` and `/Names /EmbeddedFiles`.
    fn with_attachment(source: &[u8]) -> Vec<u8> {
        let mut update = update_of(source).unwrap();
        let file = update
            .new_document
            .add_object(Stream::new(dictionary! {}, b"notes".to_vec()));
        let spec = update.new_document.add_object(dictionary! {
            "Type" => Object::Name(b"Filespec".to_vec()),
            "F" => Object::string_literal("notes.txt"),
            "AFRelationship" => Object::Name(b"Supplement".to_vec()),
            "EF" => dictionary! { "F" => Object::Reference(file) },
        });
        let root = update.new_document.trailer.get(b"Root").unwrap().clone();
        let mut catalog = resolved(&update, &root).unwrap().as_dict().unwrap().clone();
        let list = vec![Object::string_literal("notes.txt"), Object::Reference(spec)];
        let names = update.new_document.add_object(dictionary! {
            "EmbeddedFiles" => dictionary! { "Names" => list },
        });
        catalog.set("AF", vec![Object::Reference(spec)]);
        catalog.set("Names", Object::Reference(names));
        store(&mut update, &root, Object::Dictionary(catalog));
        saved(update).unwrap()
    }

    #[test]
    fn other_attachments_stay_listed() {
        let source = with_attachment(&fixture("demo.pdf"));
        let (once, _) = sign_checked(&source);
        let (twice, start) = sign_checked(&once);
        assert_eq!(source_end(&twice, start), Some(once.len()));
        let doc = load(&twice).unwrap();
        let catalog = doc.catalog().unwrap();
        let af = doc.dereference(catalog.get(b"AF").unwrap()).unwrap().1;
        assert_eq!(
            af.as_array().unwrap().len(),
            2,
            "attachment and active store"
        );
        let names = doc.dereference(catalog.get(b"Names").unwrap()).unwrap().1;
        let files = names.as_dict().unwrap().get(b"EmbeddedFiles").unwrap();
        let list = files.as_dict().unwrap().get(b"Names").unwrap();
        assert_eq!(list.as_array().unwrap().len(), 4, "two name-file pairs");
    }

    /// Stands in for a manifest store in updates built by hand.
    const STAND_IN: &[u8] = b"stand-in for a manifest store";

    /// `source` with the stand-in manifest appended, the update changed by `tamper` first, and
    /// where the stand-in starts.
    fn tampered(source: &[u8], tamper: impl FnOnce(&mut IncrementalDocument)) -> (Vec<u8>, usize) {
        let mut update = update_of(source).unwrap();
        add_manifest(&mut update, STAND_IN).unwrap();
        tamper(&mut update);
        let out = saved(update).unwrap();
        let start = find(&out, STAND_IN, source.len()).unwrap();
        (out, start)
    }

    /// The first page of `doc` and its content stream.
    fn first_page(doc: &Document) -> (ObjectId, ObjectId) {
        let page = doc.page_iter().next().unwrap();
        let contents = match doc.get_dictionary(page).unwrap().get(b"Contents").unwrap() {
            Object::Reference(id) => *id,
            Object::Array(streams) => streams[0].as_reference().unwrap(),
            other => panic!("contents {other:?}"),
        };
        (page, contents)
    }

    /// `source` with a revision that rotates its first page.
    fn rotated(source: &[u8]) -> Vec<u8> {
        let mut update = update_of(source).unwrap();
        rotate_first_page(&mut update);
        saved(update).unwrap()
    }

    fn rotate_first_page(update: &mut IncrementalDocument) {
        let page = update.get_prev_documents().page_iter().next().unwrap();
        update.opt_clone_object_to_new_document(page).unwrap();
        let dict = update.new_document.get_dictionary_mut(page).unwrap();
        dict.set("Rotate", 90);
    }

    /// The file specification of the manifest store that `doc` adds, and its stream.
    fn manifest_objects(doc: &Document) -> (ObjectId, ObjectId) {
        let (spec, dict) = doc
            .objects
            .iter()
            .filter_map(|(id, o)| Some((*id, o.as_dict().ok()?)))
            .find(|(_, d)| d.has(b"AFRelationship"))
            .unwrap();
        let ef = dict.get(b"EF").unwrap().as_dict().unwrap();
        (spec, ef.get(b"F").unwrap().as_reference().unwrap())
    }

    /// `out` with `entry` for object `number` added to its last cross-reference table.
    fn with_xref_entry(out: &[u8], number: u32, entry: &str) -> Vec<u8> {
        let table = out.windows(6).rposition(|w| w == b"\nxref\n").unwrap() + 6;
        let mut patched = out[..table].to_vec();
        patched.extend_from_slice(format!("{number} 1\n{entry}\n").as_bytes());
        patched.extend_from_slice(&out[table..]);
        patched
    }

    #[test]
    fn an_update_that_redefines_a_page_object_has_no_source_view() {
        let source = fixture("demo.pdf");
        let (_, contents) = first_page(&load(&source).unwrap());
        let text = pdf::read(&source).unwrap().document.text;
        let (out, start) = tampered(&source, |_| {});
        assert_eq!(source_end(&out, start), Some(source.len()));

        let (out, start) = tampered(&source, rotate_first_page);
        assert_eq!(source_end(&out, start), None, "page rotated");

        // The manifest stream under the number of the page's content stream.
        let (out, start) = tampered(&source, |update| {
            let doc = &mut update.new_document;
            let (spec, stream) = manifest_objects(doc);
            let stream = doc.objects.remove(&stream).unwrap();
            doc.set_object(contents, stream);
            let ef = dictionary! { "F" => Object::Reference(contents) };
            doc.get_dictionary_mut(spec).unwrap().set("EF", ef);
        });
        assert_ne!(pdf::read(&out).unwrap().document.text, text);
        assert_eq!(source_end(&out, start), None, "content stream overwritten");

        // The catalog's `/AF` under that number.
        let (out, start) = tampered(&source, |update| {
            let doc = &mut update.new_document;
            let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
            let af = doc
                .get_dictionary(root)
                .unwrap()
                .get(b"AF")
                .unwrap()
                .clone();
            doc.set_object(contents, af);
            let catalog = doc.get_dictionary_mut(root).unwrap();
            catalog.set("AF", Object::Reference(contents));
        });
        assert_ne!(pdf::read(&out).unwrap().document.text, text);
        assert_eq!(source_end(&out, start), None, "content stream taken by /AF");
    }

    #[test]
    fn an_update_that_retargets_or_frees_an_object_has_no_source_view() {
        let source = fixture("demo.pdf");
        let (page, contents) = first_page(&load(&source).unwrap());

        // An entry pointing the page back at the revision before the rotated one.
        let earlier = rotated(&source);
        let (out, _) = tampered(&earlier, |_| {});
        let original = load(&source).unwrap();
        let Some(XrefEntry::Normal { offset, .. }) = original.reference_table.get(page.0) else {
            panic!("page in a stream")
        };
        let out = with_xref_entry(&out, page.0, &format!("{offset:010} 00000 n "));
        let start = find(&out, STAND_IN, earlier.len()).unwrap();
        let page_size = |bytes: &[u8]| {
            pdf::read(bytes)
                .unwrap()
                .document
                .picture
                .unwrap()
                .dimensions()
        };
        assert_eq!(
            page_size(&out),
            page_size(&source),
            "pdfium shows the page before its rotation"
        );
        assert_ne!(page_size(&out), page_size(&earlier));
        assert_eq!(source_end(&out, start), None, "page retargeted");

        // The content stream freed (pdfium keeps the older entry, other viewers drop the object).
        let (out, _) = tampered(&source, |_| {});
        let out = with_xref_entry(&out, contents.0, "0000000000 00001 f ");
        let start = find(&out, STAND_IN, source.len()).unwrap();
        assert_eq!(source_end(&out, start), None, "content stream freed");
    }

    #[test]
    fn an_update_that_skips_a_revision_has_no_source_view() {
        // The update's trailer continues before the rotating revision through an escaped
        // `/Prev`, while a string holds the expected pointer.
        let source = fixture("demo.pdf");
        let earlier = rotated(&source);
        let (out, start) = tampered(&earlier, |_| {});
        let trailer = out.windows(7).rposition(|w| w == b"trailer").unwrap();
        let prev = trailer + find(&out[trailer..], b"/Prev", 0).unwrap();
        let digits = out[prev + 5..]
            .iter()
            .position(|b| !is_white(*b) && !b.is_ascii_digit())
            .unwrap();
        let expected = String::from_utf8_lossy(&out[prev..prev + 5 + digits]).into_owned();
        let skip = load(&source).unwrap().xref_start;
        let mut patched = out[..prev].to_vec();
        patched.extend_from_slice(format!("/Pr#65v {skip}/Note({expected})").as_bytes());
        patched.extend_from_slice(&out[prev + 5 + digits..]);
        let page_size = |bytes: &[u8]| pdf::read(bytes).unwrap().document.picture.unwrap();
        let shown = page_size(&patched).dimensions();
        assert_eq!(shown, page_size(&source).dimensions(), "rotation skipped");
        assert_ne!(shown, page_size(&earlier).dimensions());
        assert_eq!(source_end(&patched, start), None);
    }

    /// The catalog of the revision `update` appends.
    fn catalog(update: &mut IncrementalDocument) -> &mut Dictionary {
        let root = update.new_document.trailer.get(b"Root").unwrap();
        let root = root.as_reference().unwrap();
        update.new_document.get_dictionary_mut(root).unwrap()
    }

    #[test]
    fn an_update_that_drops_an_attachment_has_no_source_view() {
        let source = with_attachment(&fixture("demo.pdf"));
        let from_af: fn(&mut IncrementalDocument) = |update| {
            let af = catalog(update).get_mut(b"AF").unwrap();
            af.as_array_mut().unwrap().remove(0);
        };
        let from_names: fn(&mut IncrementalDocument) = |update| {
            let names = catalog(update)
                .get(b"Names")
                .unwrap()
                .as_reference()
                .unwrap();
            let names = update.new_document.get_dictionary_mut(names).unwrap();
            let files = names.get_mut(b"EmbeddedFiles").unwrap();
            let list = files.as_dict_mut().unwrap().get_mut(b"Names").unwrap();
            list.as_array_mut().unwrap().drain(..2);
        };
        let names_gone: fn(&mut IncrementalDocument) = |update| {
            catalog(update).remove(b"Names");
        };
        for (tamper, what) in [
            (from_af, "/AF"),
            (from_names, "names"),
            (names_gone, "/Names"),
        ] {
            let (out, start) = tampered(&source, tamper);
            assert_eq!(
                source_end(&out, start),
                None,
                "notes.txt dropped from {what}"
            );
        }
    }

    #[test]
    fn a_document_information_that_is_no_dictionary_gets_one() {
        // Null, a reference to no object, and a value of the wrong type all read as missing.
        for value in [
            Object::Null,
            Object::Reference((9999, 0)),
            Object::Integer(1),
        ] {
            let mut doc = load(&fixture("demo.pdf")).unwrap();
            doc.trailer.set("Info", value.clone());
            let mut source = Vec::new();
            doc.save_to(&mut source).unwrap();
            let fields = MetaInput {
                name: Some("Title"),
                description: None,
                meta: None,
            };
            let edited = with_metadata(&source, fields).unwrap();
            let title = pdf::read(&edited).unwrap().document.title;
            assert_eq!(title.as_deref(), Some("Title"), "{value:?}");
            let info = load(&edited).unwrap().trailer.get(b"Info").unwrap().clone();
            assert!(info.as_reference().is_ok_and(|id| id.0 != 9999), "{info:?}");
        }
    }

    #[test]
    fn an_encrypted_pdf_has_no_source_view() {
        // An update appended to a PDF encrypted with an empty password, as lopdf writes it:
        // the trailer repeats `/Encrypt` and the new strings are encrypted.
        let source = fixture("basic-signed.pdf");
        let mut previous = source.clone();
        previous.push(b'\n');
        let mut update = IncrementalDocument::create_from(previous, load(&source).unwrap());
        add_manifest(&mut update, STAND_IN).unwrap();
        let out = saved(update).unwrap();
        assert!(load(&out).unwrap().was_encrypted());
        assert_eq!(source_end(&out, source.len() + 2), None);
        // Signing refuses such a PDF itself.
        assert!(update_of(&source).is_err());
    }

    #[test]
    fn object_headers_and_white_space() {
        assert_eq!(object_header(b"12 0 obj\n<<"), Some((12, 0)));
        assert_eq!(object_header(b"7\r\n0\tobj"), Some((7, 0)));
        assert_eq!(object_header(b"3 0 obj<</A 1>>"), Some((3, 0)));
        assert_eq!(object_header(b"12 0 R"), None);
        assert_eq!(object_header(b"xref\n0 1"), None);
        assert_eq!(object_header(b"12obj"), None);
        assert_eq!(object_header(b" 12 0 obj"), None);
    }

    /// Every PDF below `PDF_CORPUS_DIR` (the files named in its `expected_pdf.json`) that the
    /// app lets through to signing signs to a Trusted copy whose source is recovered.
    #[test]
    #[ignore = "needs a local corpus; set PDF_CORPUS_DIR and run with --ignored --nocapture"]
    fn pdf_corpus_sources_are_recovered() {
        let dir = Path::new(&std::env::var("PDF_CORPUS_DIR").expect("PDF_CORPUS_DIR")).to_owned();
        let json = std::fs::read_to_string(dir.join("expected_pdf.json")).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut signed = 0;
        let mut wrong = Vec::new();
        for file in expected.as_object().unwrap().keys() {
            let source = std::fs::read(dir.join(file)).unwrap();
            let blocked = pdf::read(&source).is_ok_and(|p| p.flags.sign_block().is_some());
            if blocked || update_of(&source).is_err() {
                continue;
            }
            signed += 1;
            let out = match self::signed(&mut builder(&source), &source) {
                Ok(out) => out,
                Err(e) => {
                    wrong.push(format!("{file}: {e:#}"));
                    continue;
                }
            };
            let reader = reader(&out);
            let exclusions = inspect::data_hash_exclusions(&reader).unwrap_or_default();
            let start = exclusions.first().map(|e| e.0 as usize);
            let state = reader.validation_state();
            let end = start.and_then(|start| source_end(&out, start));
            if state != ValidationState::Trusted || end != Some(source.len()) {
                wrong.push(format!("{file}: {state:?}, source end {end:?}"));
            }
        }
        eprintln!("{signed} PDFs signed");
        assert!(signed > 0);
        assert!(wrong.is_empty(), "{} of {signed}: {wrong:?}", wrong.len());
    }
}
