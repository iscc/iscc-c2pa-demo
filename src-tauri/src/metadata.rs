//! Title, description and ISCC metadata embedded in an asset, read with iscc-sdk's rules
//! (`extract_metadata`, `code_meta`), and the Meta-Code inputs derived from them.
//!
//! Images follow iscc-sdk's `IMAGE_META_MAP`: XMP, IPTC and EXIF keys in a fixed order, the first
//! filled key per field wins, values are sanitised. Documents (EPUB, office, SVG) supply a title
//! and a description, sanitised the same way.
//! When nothing names the asset, the title and description this app stored in the manifest's
//! `cawg.metadata` assertion stand in, then the manifest title, then the file name, so a file
//! signed here recomputes the Meta-Code it was signed with.
//!
//! Known deviations from iscc-sdk 0.x: the EXIF Windows title is decoded as text (iscc-sdk takes
//! exiv2's raw byte listing, "87 0 105 0 ..."), IPTC values that are not UTF-8 are skipped
//! (iscc-sdk fails with a UnicodeEncodeError), and an XMP struct in a mapped field has no value
//! (iscc-sdk takes exiv2's placeholder `type="Struct"` as the text).

use std::io::Cursor;
use std::path::Path;

use image::ImageDecoder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{Namespace, ResolveResult};
use quick_xml::{NsReader, XmlVersion};
use serde::Serialize;

pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_ISCC: &str = "http://purl.org/iscc/schema/";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
const NS_XMP: &str = "http://ns.adobe.com/xap/1.0/";
const NS_XMP_DM: &str = "http://ns.adobe.com/xmp/1.0/DynamicMedia/";
const NS_PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
const NS_IPTC_EXT: &str = "http://iptc.org/std/Iptc4xmpExt/2008-02-29/";

/// IPTC-IIM datasets of the application record (2).
const IPTC_OBJECT_NAME: u8 = 5;
const IPTC_BYLINE: u8 = 80;
const IPTC_BYLINE_TITLE: u8 = 85;
const IPTC_HEADLINE: u8 = 105;
/// Photoshop image resource that holds the IPTC-IIM records.
const PHOTOSHOP_IPTC: u16 = 0x0404;
/// EXIF tag of the Windows title, UCS-2 little-endian bytes.
const EXIF_XP_TITLE: u16 = 0x9c9b;
/// TIFF tags that hold an XMP packet and raw IPTC-IIM records.
const TIFF_XMP: u16 = 700;
const TIFF_IPTC: u16 = 33723;
/// iscc-core `meta_trim_name`: names longer than this many bytes are cut.
const META_TRIM_NAME: usize = 128;

/// One metadata key of iscc-sdk's `IMAGE_META_MAP`.
#[derive(Clone, Copy, Debug)]
enum Key {
    Xmp(&'static str, &'static str),
    Iptc(u8),
    XpTitle,
    Artist,
}

/// Name keys in `IMAGE_META_MAP` order.
const NAME_KEYS: &[Key] = &[
    Key::Xmp(NS_ISCC, "name"),
    Key::Xmp(NS_DC, "title"),
    Key::Xmp(NS_XMP, "Nickname"),
    Key::Xmp(NS_XMP_DM, "shotName"),
    Key::Xmp(NS_PHOTOSHOP, "Headline"),
    Key::Xmp(NS_IPTC_EXT, "AOTitle"),
    Key::Iptc(IPTC_HEADLINE),
    Key::Iptc(IPTC_BYLINE_TITLE),
    Key::Iptc(IPTC_OBJECT_NAME),
    Key::XpTitle,
];
const DESCRIPTION_KEYS: &[Key] = &[
    Key::Xmp(NS_ISCC, "description"),
    Key::Xmp(NS_DC, "description"),
];
const META_KEYS: &[Key] = &[Key::Xmp(NS_ISCC, "meta")];
const CREATOR_KEYS: &[Key] = &[
    Key::Xmp(NS_DC, "creator"),
    Key::Iptc(IPTC_BYLINE),
    Key::Artist,
];

/// Meta-Code fields as found in the asset itself, plus its creator.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Embedded {
    pub name: Option<String>,
    pub description: Option<String>,
    /// ISCC metadata (`iscc:meta`, a data URL); replaces the description in the Meta-Code.
    pub meta: Option<String>,
    /// Display only, never part of the Meta-Code.
    pub creator: Option<String>,
}

/// Title, description and ISCC metadata the Meta-Code is computed from.
#[derive(Serialize, Debug, Clone, PartialEq)]
pub struct MetaFields {
    pub name: String,
    pub description: Option<String>,
    pub meta: Option<String>,
    /// Where `name` came from: `metadata` (embedded in the file), `manifest` (the C2PA title) or
    /// `filename`.
    pub name_source: &'static str,
}

/// Raw image metadata: top-level XMP properties, IPTC datasets, the EXIF Windows title and
/// artist.
#[derive(Debug, Default)]
struct ImageTags {
    xmp: Vec<(String, String, String)>,
    iptc: Vec<(u8, String)>,
    xp_title: Option<String>,
    artist: Option<String>,
}

/// Embedded metadata of an image; empty when the format carries none or cannot be read.
pub fn image(bytes: &[u8]) -> Embedded {
    let tags = image_tags(bytes);
    let pick = |keys: &[Key]| {
        keys.iter()
            .find_map(|&key| lookup(&tags, key).filter(|v| !v.is_empty()))
            .map(|v| sanitize(&v))
    };
    Embedded {
        name: pick(NAME_KEYS),
        description: pick(DESCRIPTION_KEYS),
        meta: pick(META_KEYS),
        creator: pick(CREATOR_KEYS),
    }
}

/// Embedded metadata of a document from its title, description and creator, sanitised like
/// iscc-sdk sanitises Tika's values.
pub fn document(title: Option<&str>, description: Option<&str>, creator: Option<&str>) -> Embedded {
    let clean = |s: Option<&str>| s.map(sanitize).filter(|s| !s.is_empty());
    Embedded {
        name: clean(title),
        description: clean(description),
        meta: None,
        creator: clean(creator),
    }
}

/// Meta-Code inputs recorded in the active C2PA manifest.
#[derive(Debug, Default, Clone, Copy)]
pub struct ManifestMeta<'a> {
    /// `dc:title` and `dc:description` of the `cawg.metadata` assertion written at signing.
    pub stored: Option<(&'a str, Option<&'a str>)>,
    /// The manifest's `title`.
    pub title: Option<&'a str>,
}

/// Meta-Code inputs: the embedded name if usable, else the title and description stored in the
/// manifest's `cawg.metadata`, else the manifest title, else the file name. A stored pair brings
/// its own description (none when it has none), because the signed Meta-Code used exactly that;
/// the other fallbacks keep the embedded description. ISCC metadata always comes from the file.
pub fn meta_fields(embedded: Embedded, manifest: ManifestMeta<'_>, path: &Path) -> MetaFields {
    let stored = manifest.stored.filter(|(t, _)| usable_name(t));
    let (name, description, name_source) = match (
        embedded.name.filter(|n| usable_name(n)),
        stored,
        manifest.title.filter(|t| usable_name(t)),
    ) {
        (Some(name), _, _) => (name, embedded.description, "metadata"),
        (None, Some((title, description)), _) => {
            (title.to_owned(), description.map(str::to_owned), "manifest")
        }
        (None, None, Some(title)) => (title.to_owned(), embedded.description, "manifest"),
        (None, None, None) => (name_from_path(path), embedded.description, "filename"),
    };
    MetaFields {
        name,
        description,
        meta: embedded.meta,
        name_source,
    }
}

/// iscc-sdk `text_name_from_uri`: file stem with dashes and underscores as spaces.
pub fn name_from_path(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    stem.replace(['-', '_'], " ")
}

/// iscc-sdk `text_sanitize`: drop script and style blocks, comments and tags, decode HTML
/// entities, collapse whitespace.
pub fn sanitize(text: &str) -> String {
    let text = remove_blocks(&remove_blocks(text, "script"), "style");
    let decoded = html_escape::decode_html_entities(&strip_tags(&text)).into_owned();
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether a name survives iscc-sdk's pre-check (clean, drop newlines, trim to 128 bytes).
fn usable_name(name: &str) -> bool {
    !iscc_lib::text_trim(
        &iscc_lib::text_remove_newlines(&iscc_lib::text_clean(name)),
        META_TRIM_NAME,
    )
    .is_empty()
}

/// Value of one metadata key; XMP and IPTC keys that repeat keep their last value, as exiv2's
/// key-value view does.
fn lookup(tags: &ImageTags, key: Key) -> Option<String> {
    match key {
        Key::Xmp(ns, local) => tags
            .xmp
            .iter()
            .rev()
            .find(|(n, l, _)| n == ns && l == local)
            .map(|(_, _, v)| v.clone()),
        Key::Iptc(dataset) => tags
            .iptc
            .iter()
            .rev()
            .find(|(d, _)| *d == dataset)
            .map(|(_, v)| v.clone()),
        Key::XpTitle => tags.xp_title.clone(),
        Key::Artist => tags.artist.clone(),
    }
}

/// Read the XMP packet, the IPTC block and the EXIF Windows title of an image.
fn image_tags(bytes: &[u8]) -> ImageTags {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok();
    let decoder = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_decoder().ok());
    let (xmp, irb) = match decoder {
        Some(mut d) => (
            d.xmp_metadata().ok().flatten(),
            d.iptc_metadata().ok().flatten(),
        ),
        None => (None, None),
    };
    // TIFF keeps both in tags of its first directory. The image crate's TIFF decoder refuses
    // tag values larger than its buffer limits allow, so the EXIF reader supplies them.
    let tiff_tag = |tag| exif.as_ref().and_then(|e| tiff_bytes(e, tag));
    let xmp = xmp.or_else(|| tiff_tag(TIFF_XMP));
    let iptc = match irb {
        Some(irb) => photoshop_resource(&irb, PHOTOSHOP_IPTC).map(iim_datasets),
        None => tiff_tag(TIFF_IPTC).map(|iim| iim_datasets(&iim)),
    };
    ImageTags {
        xmp: xmp.map(|x| xmp_properties(&x)).unwrap_or_default(),
        iptc: iptc.unwrap_or_default(),
        xp_title: exif.as_ref().and_then(xp_title),
        artist: exif.as_ref().and_then(artist),
    }
}

/// The bytes of a tag in the first TIFF directory; `LONG` values (as Photoshop writes IPTC)
/// are turned back into bytes in file order.
fn tiff_bytes(exif: &exif::Exif, tag: u16) -> Option<Vec<u8>> {
    let field = exif.get_field(exif::Tag(exif::Context::Tiff, tag), exif::In::PRIMARY)?;
    match &field.value {
        exif::Value::Byte(bytes) | exif::Value::Undefined(bytes, _) => Some(bytes.clone()),
        exif::Value::Long(longs) => Some(
            longs
                .iter()
                .flat_map(|l| {
                    if exif.little_endian() {
                        l.to_le_bytes()
                    } else {
                        l.to_be_bytes()
                    }
                })
                .collect(),
        ),
        _ => None,
    }
}

/// A top-level XMP property while its element is open.
#[derive(Default)]
struct Property {
    ns: String,
    local: String,
    depth: usize,
    alt: bool,
    text: String,
    /// Language and text of each `rdf:li`.
    items: Vec<(Option<String>, String)>,
    in_item: bool,
    /// Qualified value (`rdf:value`) of the property or of its open list item; replaces its text.
    value: Option<String>,
    /// Depth of the open `rdf:value` element.
    value_depth: Option<usize>,
    /// Depth of the open qualifier or struct field; its text is ignored.
    skip_depth: Option<usize>,
    /// Holds elements other than rdf containers: qualifiers or struct fields.
    nested: bool,
    /// Carries an `rdf:value`, so nested elements are qualifiers. Without one they are struct
    /// fields, and the property has no text value.
    qualified: bool,
}

impl Property {
    /// Take a qualified value (`rdf:value`), if there is one.
    fn set_value(&mut self, value: Option<String>) {
        if value.is_some() {
            self.value = value;
            self.qualified = true;
        }
    }
}

/// Top-level properties of an XMP packet as (namespace, local name, value), with values
/// formatted like exiv2's `toString` and cleaned like iscc-sdk's `_clean_xmp_value`. Simple
/// properties may be attributes of `rdf:Description` or child elements.
fn xmp_properties(xmp: &[u8]) -> Vec<(String, String, String)> {
    let Ok(text) = std::str::from_utf8(xmp) else {
        return Vec::new();
    };
    let mut reader = NsReader::from_str(text);
    let mut out = Vec::new();
    let (mut depth, mut description) = (0usize, None::<usize>);
    let mut prop: Option<Property> = None;
    loop {
        let (empty, start) = match reader.read_resolved_event() {
            Ok((ns, Event::Start(e))) => (false, Some((bound(&ns), e))),
            Ok((ns, Event::Empty(e))) => (true, Some((bound(&ns), e))),
            Ok((_, Event::End(e))) => {
                end_element(
                    &mut prop,
                    &mut description,
                    depth,
                    e.local_name().as_ref(),
                    &mut out,
                );
                depth -= 1;
                continue;
            }
            Ok((_, Event::Text(t))) => {
                push_text(&mut prop, &t);
                continue;
            }
            Ok((_, Event::GeneralRef(r))) => {
                push_text(
                    &mut prop,
                    &html_escape::decode_html_entities(&format!("&{};", r.xml10_content())),
                );
                continue;
            }
            Ok((_, Event::CData(c))) => {
                push_text(&mut prop, &c.into_inner());
                continue;
            }
            Ok((_, Event::Eof)) | Err(_) => break,
            _ => continue,
        };
        let Some((ns, e)) = start else { continue };
        depth += 1;
        let local = e.local_name().as_ref().to_owned();
        match prop.as_mut() {
            Some(p) => {
                open_in_property(p, ns.as_deref(), &local, &e, depth, rdf_value(&reader, &e))
            }
            None if ns.as_deref() == Some(NS_RDF) && local == "Description" => {
                description = Some(depth);
                out.extend(attribute_properties(&reader, &e));
            }
            None if description == Some(depth - 1) => {
                let mut p = Property {
                    ns: ns.unwrap_or_default(),
                    local,
                    depth,
                    ..Default::default()
                };
                p.set_value(rdf_value(&reader, &e));
                prop = Some(p);
            }
            None => {}
        }
        if empty {
            end_element(
                &mut prop,
                &mut description,
                depth,
                e.local_name().as_ref(),
                &mut out,
            );
            depth -= 1;
        }
    }
    out
}

/// Namespace of a resolved name, if bound.
fn bound(ns: &ResolveResult<'_>) -> Option<String> {
    match ns {
        ResolveResult::Bound(Namespace(n)) => Some((*n).to_owned()),
        _ => None,
    }
}

/// Track rdf containers, list items, qualified values and qualifiers opened inside a property.
/// `value` is the element's `rdf:value` attribute.
fn open_in_property(
    p: &mut Property,
    ns: Option<&str>,
    local: &str,
    e: &BytesStart<'_>,
    depth: usize,
    value: Option<String>,
) {
    if p.skip_depth.is_some() {
        return;
    }
    match (ns == Some(NS_RDF), local) {
        (true, "Alt") => p.alt = true,
        (true, "Bag" | "Seq") => {}
        // A qualified value written as a nested resource.
        (true, "Description") => p.set_value(value),
        (true, "li") => {
            let lang = e
                .attributes()
                .flatten()
                .find(|a| a.key.as_ref() == "xml:lang")
                .and_then(|a| {
                    a.normalized_value(XmlVersion::Implicit1_0)
                        .ok()
                        .map(|v| v.into_owned())
                });
            p.items.push((lang, String::new()));
            p.in_item = true;
            p.set_value(value);
        }
        (true, "value") => {
            p.set_value(Some(String::new()));
            p.value_depth = Some(depth);
        }
        _ => {
            p.nested = true;
            p.skip_depth = Some(depth);
        }
    }
}

/// The `rdf:value` attribute of an element: a qualified value in attribute form.
fn rdf_value(reader: &NsReader<&[u8]>, e: &BytesStart<'_>) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        let (ns, local) = reader.resolver().resolve_attribute(a.key);
        let is_value = bound(&ns).as_deref() == Some(NS_RDF) && local.as_ref() == "value";
        is_value
            .then(|| {
                a.normalized_value(XmlVersion::Implicit1_0)
                    .ok()
                    .map(|v| v.into_owned())
            })
            .flatten()
    })
}

/// Close an element: finish qualifiers, qualified values, list items, properties and descriptions.
fn end_element(
    prop: &mut Option<Property>,
    description: &mut Option<usize>,
    depth: usize,
    local: &str,
    out: &mut Vec<(String, String, String)>,
) {
    match prop.as_mut() {
        Some(p) if p.depth == depth => {
            let mut p = prop.take().expect("property is open");
            if let Some(value) = p.value.take() {
                p.text = value;
            }
            if !p.nested || p.qualified {
                out.push((p.ns.clone(), p.local.clone(), property_value(&p)));
            }
        }
        Some(p) if p.skip_depth == Some(depth) => p.skip_depth = None,
        Some(p) if p.skip_depth.is_some() => {}
        Some(p) if p.value_depth == Some(depth) => p.value_depth = None,
        Some(p) if local == "li" => {
            p.in_item = false;
            if let (Some(value), Some((_, item))) = (p.value.take(), p.items.last_mut()) {
                *item = value;
            }
        }
        _ if *description == Some(depth) => *description = None,
        _ => {}
    }
}

/// Append character data to the open qualified value, list item or simple property; text of
/// qualifiers and struct fields is ignored.
fn push_text(prop: &mut Option<Property>, text: &str) {
    let Some(p) = prop.as_mut() else { return };
    if p.skip_depth.is_some() {
        return;
    }
    if p.value_depth.is_some() {
        p.value.get_or_insert_default().push_str(text);
    } else if p.in_item {
        if let Some((_, item)) = p.items.last_mut() {
            item.push_str(text);
        }
    } else if p.items.is_empty() {
        p.text.push_str(text);
    }
}

/// Simple properties written as attributes of `rdf:Description`.
fn attribute_properties(
    reader: &NsReader<&[u8]>,
    e: &BytesStart<'_>,
) -> Vec<(String, String, String)> {
    e.attributes()
        .flatten()
        .filter(|a| !a.key.as_ref().starts_with("xmlns") && !a.key.as_ref().starts_with("xml:"))
        .filter_map(|a| {
            let (ns, local) = reader.resolver().resolve_attribute(a.key);
            let ns = bound(&ns).filter(|n| n != NS_RDF)?;
            let value = a.normalized_value(XmlVersion::Implicit1_0).ok()?;
            Some((ns, local.as_ref().to_owned(), value.trim().to_owned()))
        })
        .collect()
}

/// exiv2 `toString` of a property (`lang="x-default" text, lang="de" text` for language
/// alternatives, items joined with ", " for arrays), stripped and cleaned like iscc-sdk.
fn property_value(p: &Property) -> String {
    let raw = if p.alt {
        let lang = |l: &Option<String>| l.clone().unwrap_or_else(|| "x-default".to_owned());
        let default = p.items.iter().filter(|(l, _)| lang(l) == "x-default");
        let others = p.items.iter().filter(|(l, _)| lang(l) != "x-default");
        default
            .chain(others)
            .map(|(l, v)| format!("lang=\"{}\" {v}", lang(l)))
            .collect::<Vec<_>>()
            .join(", ")
    } else if !p.items.is_empty() {
        p.items
            .iter()
            .map(|(_, v)| v.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        p.text.clone()
    };
    clean_xmp_value(raw.trim())
}

/// iscc-sdk `_clean_xmp_value`: drop a leading `lang="..." ` qualifier.
fn clean_xmp_value(value: &str) -> String {
    if let Some(rest) = value.strip_prefix("lang=\"") {
        if let Some(end) = rest.find('"') {
            if rest.len() > end + 1 {
                return rest[end + 1..].trim().to_owned();
            }
        }
    }
    value.to_owned()
}

/// Datasets of the application record in IPTC-IIM records. Values that are not UTF-8 are
/// skipped.
fn iim_datasets(iim: &[u8]) -> Vec<(u8, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 <= iim.len() && iim[i] == 0x1c {
        let (record, dataset) = (iim[i + 1], iim[i + 2]);
        let len = u16::from_be_bytes([iim[i + 3], iim[i + 4]]) as usize;
        // Extended lengths (high bit set) are only used for binary datasets.
        let Some(data) = (len & 0x8000 == 0)
            .then(|| iim.get(i + 5..i + 5 + len))
            .flatten()
        else {
            break;
        };
        if record == 2 {
            if let Ok(s) = std::str::from_utf8(data) {
                out.push((dataset, s.trim().to_owned()));
            }
        }
        i += 5 + len;
    }
    out
}

/// Data of the Photoshop image resource `id` in a sequence of `8BIM` blocks.
fn photoshop_resource(irb: &[u8], id: u16) -> Option<&[u8]> {
    let mut i = 0;
    while irb.get(i..i + 4) == Some(b"8BIM") {
        let resource = u16::from_be_bytes(irb.get(i + 4..i + 6)?.try_into().ok()?);
        // Pascal string name: length byte plus name, padded to an even size.
        let name = (usize::from(*irb.get(i + 6)?) + 2) & !1;
        let size_at = i + 6 + name;
        let size = u32::from_be_bytes(irb.get(size_at..size_at + 4)?.try_into().ok()?) as usize;
        let data = irb.get(size_at + 4..size_at + 4 + size)?;
        if resource == id {
            return Some(data);
        }
        i = size_at + 4 + size + (size & 1);
    }
    None
}

/// EXIF Windows title (tag 0x9C9B) decoded from its UCS-2 bytes.
fn xp_title(exif: &exif::Exif) -> Option<String> {
    let field = exif.get_field(
        exif::Tag(exif::Context::Tiff, EXIF_XP_TITLE),
        exif::In::PRIMARY,
    )?;
    let exif::Value::Byte(raw) = &field.value else {
        return None;
    };
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16(&units).ok().map(|s| s.trim().to_owned())
}

/// EXIF artist (tag 0x013B).
fn artist(exif: &exif::Exif) -> Option<String> {
    let field = exif.get_field(exif::Tag::Artist, exif::In::PRIMARY)?;
    let exif::Value::Ascii(parts) = &field.value else {
        return None;
    };
    let text = String::from_utf8_lossy(parts.first()?);
    Some(text.trim_end_matches('\0').trim().to_owned())
}

/// Remove `<tag ...>...</tag>` blocks, case-insensitively, like iscc-sdk's SCRIPT_RE and STYLE_RE.
fn remove_blocks(text: &str, tag: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = String::new();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find(&open).map(|s| s + pos) {
        let Some(end) = lower[start..].find(&close).map(|e| e + start + close.len()) else {
            break;
        };
        out.push_str(&text[pos..start]);
        pos = end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Drop comments and tags the way an HTML tokenizer sees them: `<` starts markup only when
/// followed by a letter, `/`, `!` or `?`; anything else stays text.
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt + 1..];
        let markup = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, '/' | '!' | '?'));
        if !markup {
            out.push('<');
            rest = after;
            continue;
        }
        let end = if after.starts_with("!--") {
            after.find("-->").map(|e| e + 3)
        } else if let Some(end_tag) = after
            .strip_prefix('/')
            .filter(|t| t.starts_with(|c: char| c.is_ascii_alphabetic()))
        {
            tag_end(end_tag).map(|e| e + 1)
        } else if after.starts_with(|c: char| c.is_ascii_alphabetic()) {
            tag_end(after)
        } else {
            // Bogus comments (`<!x`, `<?x`, `</ x`) end at the first `>`.
            after.find('>').map(|e| e + 1)
        };
        match end {
            Some(e) => rest = &after[e..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Where the HTML tokenizer is inside a start or end tag.
#[derive(Clone, Copy)]
enum TagState {
    Name,
    BeforeAttr,
    Attr,
    AfterAttr,
    BeforeValue,
    Quoted(char),
    Unquoted,
}

/// Byte offset just past the `>` that closes a tag, given the text after `<` (or `</`). Follows
/// the tokenizer's attribute states, so a quoted attribute value may contain `>`; a quote
/// anywhere else is an ordinary character.
fn tag_end(tag: &str) -> Option<usize> {
    let mut state = TagState::Name;
    for (i, c) in tag.char_indices() {
        let space = matches!(c, '\t' | '\n' | '\x0c' | '\r' | ' ');
        state = match (state, c) {
            (TagState::Quoted(q), c) if c == q => TagState::BeforeAttr,
            (TagState::Quoted(q), _) => TagState::Quoted(q),
            (_, '>') => return Some(i + 1),
            (TagState::BeforeValue, _) if space => TagState::BeforeValue,
            (TagState::BeforeValue, '"' | '\'') => TagState::Quoted(c),
            (TagState::BeforeValue, _) => TagState::Unquoted,
            (TagState::Unquoted, _) if space => TagState::BeforeAttr,
            (TagState::Unquoted, _) => TagState::Unquoted,
            (TagState::Attr | TagState::AfterAttr, '=') => TagState::BeforeValue,
            (TagState::Attr | TagState::AfterAttr, _) if space => TagState::AfterAttr,
            (TagState::Name, _) if space => TagState::BeforeAttr,
            (_, '/') => TagState::BeforeAttr,
            (TagState::Name, _) => TagState::Name,
            (TagState::BeforeAttr, _) if space => TagState::BeforeAttr,
            (_, _) => TagState::Attr,
        };
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::iscc::{self, MetaInput};

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Meta-Code as iscc-sdk `code_meta` computes it: embedded name, else the file name.
    pub fn sdk_meta_code(embedded: &Embedded, path: &Path) -> String {
        let fields = meta_fields(embedded.clone(), ManifestMeta::default(), path);
        iscc::meta_unit(MetaInput {
            name: Some(&fields.name),
            description: fields.description.as_deref(),
            meta: fields.meta.as_deref(),
        })
        .unwrap()
        .iscc
    }

    #[test]
    fn image_metadata_matches_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_meta.py (iscc-sdk with exiv2).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_meta.json")).unwrap();
        for (file, want) in expected.as_object().unwrap() {
            if file.ends_with(".svg") {
                continue; // svg.rs checks its SVG entries.
            }
            let path = fixture(file);
            let got = image(&std::fs::read(&path).unwrap());
            let mut want_name = want["name"].as_str().map(str::to_owned);
            if file == "meta-exif.jpg" {
                // Deviation: iscc-sdk takes exiv2's raw byte listing of the UCS-2 title.
                assert!(want_name.as_deref().unwrap().starts_with("87 0 105 0"));
                assert_eq!(got.name.as_deref(), Some("Windows Title"));
                continue;
            }
            assert_eq!(got.name, want_name.take(), "{file} name");
            assert_eq!(
                got.description.as_deref(),
                want["description"].as_str(),
                "{file} description"
            );
            assert_eq!(got.meta.as_deref(), want["meta"].as_str(), "{file} meta");
            assert_eq!(
                got.creator.as_deref(),
                want["creator"].as_str(),
                "{file} creator"
            );
            assert_eq!(
                name_from_path(&path),
                want["fallback"].as_str().unwrap(),
                "{file} fallback"
            );
            assert_eq!(
                sdk_meta_code(&got, &path),
                want["meta_code"].as_str().unwrap(),
                "{file} Meta-Code"
            );
        }
    }

    #[test]
    fn name_falls_back_to_stored_pair_then_manifest_title_then_file_name() {
        let path = Path::new("dir/my_photo-final.jpg");
        let described = Embedded {
            description: Some("Embedded description".into()),
            meta: Some("data:application/json;base64,e30=".into()),
            ..Default::default()
        };
        let title_only = ManifestMeta {
            stored: None,
            title: Some("Signed Title"),
        };
        let with_pair = |description| ManifestMeta {
            stored: Some(("Stored Title", description)),
            title: Some("Signed Title"),
        };

        // The stored pair wins over the manifest title and brings its own description.
        let fields = meta_fields(described.clone(), with_pair(Some("Stored")), path);
        assert_eq!(fields.name, "Stored Title");
        assert_eq!(fields.name_source, "manifest");
        assert_eq!(fields.description.as_deref(), Some("Stored"));
        assert_eq!(
            fields.meta, described.meta,
            "ISCC metadata stays the file's"
        );
        // A pair without a description means the Meta-Code was signed without one.
        let fields = meta_fields(described.clone(), with_pair(None), path);
        assert_eq!(fields.description, None);
        // A bare manifest title (other tools) keeps the embedded description.
        let fields = meta_fields(described.clone(), title_only, path);
        assert_eq!(
            (fields.name.as_str(), fields.name_source),
            ("Signed Title", "manifest")
        );
        assert_eq!(fields.description.as_deref(), Some("Embedded description"));
        // Unusable titles fall through to the file name.
        let blank = ManifestMeta {
            stored: Some(("  ", Some("ignored"))),
            title: Some("  "),
        };
        let fields = meta_fields(Embedded::default(), blank, path);
        assert_eq!(
            (fields.name.as_str(), fields.name_source),
            ("my photo final", "filename")
        );
        assert_eq!(fields.description, None);
        // An embedded title outranks everything in the manifest.
        let embedded = Embedded {
            name: Some("Embedded".into()),
            ..described
        };
        let fields = meta_fields(embedded, with_pair(Some("Stored")), path);
        assert_eq!(fields.name_source, "metadata");
        assert_eq!(fields.description.as_deref(), Some("Embedded description"));
    }

    #[test]
    fn sanitize_strips_markup_like_iscc_sdk() {
        assert_eq!(
            sanitize("a <b>bold</b> &amp; <!-- c --> x < y\n z"),
            "a bold & x < y z"
        );
        assert_eq!(
            sanitize("<script>alert(1)</script>Title<STYLE>p{}</style>"),
            "Title"
        );
        assert_eq!(sanitize("&lt;b&gt;literal"), "<b>literal");
    }

    #[test]
    fn sanitize_finds_tag_ends_like_html5lib() {
        // Expected values from iscc-sdk 0.9.5 text_sanitize (bleach on html5lib).
        let cases = [
            (r#"A <b title="1 > 0">bold</b> title"#, "A bold title"),
            ("A <b title='1 > 0'>bold</b> title", "A bold title"),
            (r#"A <b title = "1 > 0">bold</b> title"#, "A bold title"),
            (r#"a</b title="1 > 0">b"#, "ab"),
            ("A <b title=1>0>bold</b> title", "A 0>bold title"),
            (r#"A <b "x>y">bold</b> title"#, r#"A y">bold title"#),
            ("a</>b", "ab"),
            ("a<?x > y?>b", "a y?>b"),
            (r#"a <b title="unterminated>bold"#, "a"),
        ];
        for (input, want) in cases {
            assert_eq!(sanitize(input), want, "{input}");
        }
    }

    /// Value of `ns:local` in an XMP packet whose `rdf:Description` holds `body`.
    fn xmp_value(body: &str, ns: &'static str, local: &'static str) -> Option<String> {
        let packet = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{NS_RDF}"><rdf:Description rdf:about=""
            xmlns:xmp="{NS_XMP}" xmlns:dc="{NS_DC}" xmlns:photoshop="{NS_PHOTOSHOP}" xmlns:ex="http://example.com/ns/">
            {body}</rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let tags = ImageTags {
            xmp: xmp_properties(packet.as_bytes()),
            ..Default::default()
        };
        lookup(&tags, Key::Xmp(ns, local))
    }

    #[test]
    fn xmp_qualified_values_like_exiv2() {
        // Expected values from exiv2 0.28 through iscc-sdk 0.9.5.
        let nickname = [
            (r#"<xmp:Nickname rdf:parseType="Resource"> <rdf:value>Resource</rdf:value> <ex:q>qualifier</ex:q> </xmp:Nickname>"#, "Resource"),
            ("<xmp:Nickname><rdf:Description><ex:q>qualifier</ex:q><rdf:value>Nested</rdf:value></rdf:Description></xmp:Nickname>", "Nested"),
            (r#"<xmp:Nickname rdf:value="Attribute" ex:q="qualifier"/>"#, "Attribute"),
            (r#"<xmp:Nickname><rdf:Description rdf:value="Nested attribute" ex:q="qualifier"/></xmp:Nickname>"#, "Nested attribute"),
        ];
        for (body, want) in nickname {
            assert_eq!(
                xmp_value(body, NS_XMP, "Nickname").as_deref(),
                Some(want),
                "{body}"
            );
        }
        let title = r#"<dc:title><rdf:Alt><rdf:li xml:lang="x-default" rdf:parseType="Resource">
            <rdf:value>Item</rdf:value><ex:q>qualifier</ex:q></rdf:li></rdf:Alt></dc:title>"#;
        assert_eq!(xmp_value(title, NS_DC, "title").as_deref(), Some("Item"));
        // Deviation: a struct has no text value, so the next key takes over. exiv2 reports
        // `type="Struct"`, which iscc-sdk uses as the name.
        let structure = r#"<xmp:Nickname rdf:parseType="Resource"><ex:a>field</ex:a></xmp:Nickname>
            <photoshop:Headline>Headline</photoshop:Headline>"#;
        assert_eq!(xmp_value(structure, NS_XMP, "Nickname"), None);
        assert_eq!(
            xmp_value(structure, NS_PHOTOSHOP, "Headline").as_deref(),
            Some("Headline")
        );
    }
}
