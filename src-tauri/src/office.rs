//! Office documents in zip containers: Word, PowerPoint and Excel (Office Open XML) and
//! OpenDocument text, spreadsheet and presentation. Text, core properties and the preview
//! picture the office application saved.
//!
//! iscc-sdk extracts text with Tika. The Content-Code Text only depends on which text is included
//! and in what order (`text_collapse` drops whitespace and punctuation), so these extractors
//! follow Tika's inclusion rules, checked against the fixtures, rather than its formatting.

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{anyhow, Context as _, Result};
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};
use zip::ZipArchive;

use crate::asset::Document;
use crate::formats::{DOCX, ODP, ODS, ODT, PPTX, XLSX};
use crate::metadata::NS_DC;
use crate::numfmt;

type Zip<'a> = ZipArchive<Cursor<&'a [u8]>>;

const NS_ODF_META: &str = "urn:oasis:names:tc:opendocument:xmlns:meta:1.0";
/// Relationship type of the OOXML package thumbnail.
const REL_THUMBNAIL: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail";
const REL_NOTES_SLIDE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide";
/// The preview picture of an OpenDocument file.
const ODF_THUMBNAIL: &str = "Thumbnails/thumbnail.png";

/// Which text an XML walk collects. Names are local (`p`) or, with a colon, qualified
/// (`dc:creator`).
struct Rules {
    /// Elements whose character data is text; empty when all character data is text.
    text: &'static [&'static str],
    /// Elements whose content is left out entirely.
    skip: &'static [&'static str],
    /// Elements after which a line ends.
    block: &'static [&'static str],
    /// Elements that stand for a space (tabs and space runs).
    space: &'static [&'static str],
}

/// WordprocessingML: runs of `w:t`; `mc:Fallback` repeats `mc:Choice` content.
const WORD: Rules = Rules {
    text: &["t"],
    skip: &["Fallback"],
    block: &["p", "br", "cr", "tr"],
    space: &["tab"],
};
/// DrawingML text of slides and notes.
const DRAWING: Rules = Rules {
    text: &["t"],
    skip: &["Fallback"],
    block: &["p", "br"],
    space: &[],
};
/// OpenDocument: all character data outside style definitions (number formats carry currency
/// symbols and literal text), without page numbers and counts and without the author and date
/// of comments, as Tika reads it.
const ODF: Rules = Rules {
    text: &[],
    skip: &[
        "styles",
        "automatic-styles",
        "font-face-decls",
        "scripts",
        "page-number",
        "page-count",
        "dc:creator",
        "dc:date",
    ],
    block: &["p", "h", "line-break", "table-row"],
    space: &["s", "tab", "table-cell"],
};

/// Whether `mime` is one of the office formats read here.
pub fn handles(mime: &str) -> bool {
    [DOCX, PPTX, XLSX, ODT, ODS, ODP].contains(&mime)
}

/// Text, properties and thumbnail of an office document of type `mime`.
pub fn read(bytes: &[u8], mime: &str) -> Result<Document> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).context("cannot read the zip container")?;
    let odf = [ODT, ODS, ODP].contains(&mime);
    let text = match mime {
        DOCX => docx_text(&mut zip)?,
        PPTX => pptx_text(&mut zip)?,
        XLSX => xlsx_text(&mut zip)?,
        _ => odf_text(&mut zip)?,
    };
    let properties = if odf {
        odf_meta(&mut zip)
    } else {
        core_properties(&mut zip)
    };
    Ok(Document {
        cover: thumbnail(&mut zip, odf),
        text,
        ..properties
    })
}

/// Bytes of the zip entry `name`.
fn zip_entry(zip: &mut Zip, name: &str) -> Result<Vec<u8>> {
    let mut file = zip
        .by_name(name)
        .map_err(|e| anyhow!("{name} missing from the container: {e}"))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// The zip entry `name` as text (office XML is UTF-8).
fn zip_text(zip: &mut Zip, name: &str) -> Result<String> {
    Ok(String::from_utf8_lossy(&zip_entry(zip, name)?).into_owned())
}

/// Whether `names` lists an element by its qualified or its local name.
fn listed(names: &[&str], qualified: &str, local: &str) -> bool {
    names.iter().any(|n| *n == local || *n == qualified)
}

/// Text of a character or entity reference such as `amp` or `#10`.
fn reference_text(r: &BytesRef<'_>) -> String {
    html_escape::decode_html_entities(&format!("&{};", r.xml10_content())).into_owned()
}

/// Footnotes, endnotes and comments of a Word document by id.
#[derive(Default)]
struct Notes {
    footnotes: HashMap<String, String>,
    endnotes: HashMap<String, String>,
    comments: HashMap<String, String>,
}

/// Character data collected by an XML walk.
struct Walk<'r> {
    rules: &'r Rules,
    /// Word notes, inserted where they are referenced.
    notes: Option<&'r Notes>,
    /// Comments referenced in the open paragraph; they follow the paragraph.
    pending: Vec<&'r str>,
    out: String,
    /// Depth inside skipped elements.
    skipped: usize,
    /// Depth inside text elements.
    inside: usize,
}

impl<'r> Walk<'r> {
    fn new(rules: &'r Rules) -> Self {
        Self {
            rules,
            notes: None,
            pending: Vec::new(),
            out: String::new(),
            skipped: 0,
            inside: 0,
        }
    }

    /// Feed one parser event.
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(e) => self.open(&e),
            Event::End(e) => self.close(e.name().as_ref(), e.local_name().as_ref()),
            Event::Empty(e) => {
                self.open(&e);
                self.close(e.name().as_ref(), e.local_name().as_ref());
            }
            Event::Text(t) => self.text(&t),
            Event::GeneralRef(r) => self.text(&reference_text(&r)),
            Event::CData(c) => self.text(&c.into_inner()),
            _ => {}
        }
    }

    fn open(&mut self, e: &BytesStart<'_>) {
        let (qualified, local) = (e.name(), e.local_name());
        if self.skipped > 0 || listed(self.rules.skip, qualified.as_ref(), local.as_ref()) {
            self.skipped += 1;
            return;
        }
        if listed(self.rules.text, qualified.as_ref(), local.as_ref()) {
            self.inside += 1;
        }
        if let Some(notes) = self.notes {
            self.note_reference(notes, local.as_ref(), e);
        }
    }

    /// Insert a referenced footnote or endnote, or queue a referenced comment.
    fn note_reference(&mut self, notes: &'r Notes, local: &str, e: &BytesStart<'_>) {
        let found = |map: &'r HashMap<String, String>| attr(e, "id").and_then(|id| map.get(&id));
        match local {
            "footnoteReference" => self.out.extend(found(&notes.footnotes).map(String::as_str)),
            "endnoteReference" => self.out.extend(found(&notes.endnotes).map(String::as_str)),
            "commentReference" => self
                .pending
                .extend(found(&notes.comments).map(String::as_str)),
            _ => {}
        }
    }

    fn close(&mut self, qualified: &str, local: &str) {
        if self.skipped > 0 {
            self.skipped -= 1;
            return;
        }
        if listed(self.rules.text, qualified, local) {
            self.inside = self.inside.saturating_sub(1);
        }
        if listed(self.rules.block, qualified, local) {
            self.out.push('\n');
        } else if listed(self.rules.space, qualified, local) {
            self.out.push(' ');
        }
        if local == "p" {
            for comment in self.pending.drain(..) {
                self.out.push_str(comment);
                self.out.push('\n');
            }
        }
    }

    fn text(&mut self, text: &str) {
        if self.skipped == 0 && (self.rules.text.is_empty() || self.inside > 0) {
            self.out.push_str(text);
        }
    }

    /// Walk a whole XML part. A malformed part yields the text read up to the error.
    fn run(mut self, xml: &str) -> String {
        let mut reader = quick_xml::Reader::from_str(xml);
        loop {
            match reader.read_event() {
                Ok(Event::Eof) | Err(_) => break,
                Ok(event) => self.event(event),
            }
        }
        self.out
    }
}

/// The text of an XML part under `rules`.
fn xml_text(xml: &str, rules: &Rules) -> String {
    Walk::new(rules).run(xml)
}

/// Text of every `element` (`footnote`, `endnote`, `comment`) of a Word part, by its id.
fn note_texts(xml: &str, element: &str) -> HashMap<String, String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = HashMap::new();
    let mut current: Option<(String, Walk)> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if local(&e) == element => {
                current = attr(&e, "id").map(|id| (id, Walk::new(&WORD)));
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == element => {
                out.extend(current.take().map(|(id, walk)| (id, walk.out)));
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(event) => {
                if let Some((_, walk)) = current.as_mut() {
                    walk.event(event);
                }
            }
        }
    }
    out
}

/// Local name of an element.
fn local(e: &BytesStart<'_>) -> String {
    e.local_name().as_ref().to_owned()
}

/// Value of the attribute with local name `name`.
fn attr(e: &BytesStart<'_>, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// The relationship id (`r:id`) of an element; unlike a plain `id`, it has a namespace prefix.
fn rel_id(e: &BytesStart<'_>) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.prefix().is_some() && a.key.local_name().as_ref() == "id")
        .and_then(|a| {
            a.normalized_value(XmlVersion::Implicit1_0)
                .ok()
                .map(|v| v.into_owned())
        })
}

/// Name (if any) and relationship id of every element with local name `element`, in document
/// order.
fn references(xml: &str, element: &str) -> Vec<(Option<String>, String)> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if local(&e) == element => {
                out.extend(rel_id(&e).map(|id| (attr(&e, "name"), id)));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// One OOXML package relationship with its target resolved to a zip entry name.
struct Relationship {
    id: String,
    kind: String,
    target: String,
}

/// Relationships of the package part `part` (empty string: the package itself).
fn relationships(zip: &mut Zip, part: &str) -> Vec<Relationship> {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    let rels_name = if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    };
    let Ok(xml) = zip_text(zip, &rels_name) else {
        return Vec::new();
    };
    let mut reader = quick_xml::Reader::from_str(&xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if local(&e) == "Relationship" => {
                let (Some(id), Some(kind), Some(target)) =
                    (attr(&e, "Id"), attr(&e, "Type"), attr(&e, "Target"))
                else {
                    continue;
                };
                out.push(Relationship {
                    id,
                    kind,
                    target: resolve(dir, &target),
                });
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Zip entry name of a relationship target, relative to `dir` unless it starts with `/`.
fn resolve(dir: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Word document: the body in reading order, footnotes and endnotes where they are referenced,
/// comments after the paragraph that references them. Headers and footers are left out, as
/// Tika leaves them out.
fn docx_text(zip: &mut Zip) -> Result<String> {
    const DOCUMENT: &str = "word/document.xml";
    let rels = relationships(zip, DOCUMENT);
    let mut notes_of = |kind: &str, element: &str| {
        rels.iter()
            .find(|r| r.kind.ends_with(kind))
            .and_then(|r| zip_text(zip, &r.target).ok())
            .map(|xml| note_texts(&xml, element))
            .unwrap_or_default()
    };
    let notes = Notes {
        footnotes: notes_of("/footnotes", "footnote"),
        endnotes: notes_of("/endnotes", "endnote"),
        comments: notes_of("/comments", "comment"),
    };
    let mut walk = Walk::new(&WORD);
    walk.notes = Some(&notes);
    Ok(walk.run(&zip_text(zip, DOCUMENT)?))
}

/// Presentation: each slide in presentation order, followed by its notes.
fn pptx_text(zip: &mut Zip) -> Result<String> {
    const PRESENTATION: &str = "ppt/presentation.xml";
    let order = references(&zip_text(zip, PRESENTATION)?, "sldId");
    let rels = relationships(zip, PRESENTATION);
    let mut out = String::new();
    for (_, id) in order {
        let Some(slide) = rels.iter().find(|r| r.id == id) else {
            continue;
        };
        out.push_str(&xml_text(&zip_text(zip, &slide.target)?, &DRAWING));
        let notes = relationships(zip, &slide.target)
            .into_iter()
            .find(|r| r.kind == REL_NOTES_SLIDE);
        if let Some(notes) = notes {
            out.push_str(&xml_text(&zip_text(zip, &notes.target)?, &DRAWING));
        }
    }
    Ok(out)
}

/// Spreadsheet: each sheet in workbook order, its name followed by its cells row by row.
fn xlsx_text(zip: &mut Zip) -> Result<String> {
    const WORKBOOK: &str = "xl/workbook.xml";
    let workbook = zip_text(zip, WORKBOOK)?;
    let rels = relationships(zip, WORKBOOK);
    let mut part = |kind: &str| {
        rels.iter()
            .find(|r| r.kind.ends_with(kind))
            .and_then(|r| zip_text(zip, &r.target).ok())
            .unwrap_or_default()
    };
    let book = Workbook {
        strings: shared_strings(&part("/sharedStrings")),
        formats: cell_formats(&part("/styles")),
        date1904: attr_values(&workbook, "workbookPr", "date1904")
            .first()
            .is_some_and(|v| v == "1" || v == "true"),
    };
    let mut out = String::new();
    for (name, id) in references(&workbook, "sheet") {
        let Some(sheet) = rels.iter().find(|r| r.id == id) else {
            continue;
        };
        out.push_str(&name.unwrap_or_default());
        out.push('\n');
        out.push_str(&sheet_text(&zip_text(zip, &sheet.target)?, &book));
    }
    Ok(out)
}

/// What worksheet cells refer to.
#[derive(Default)]
struct Workbook {
    strings: Vec<String>,
    /// Number format code of each cell style (`cellXfs` index).
    formats: Vec<String>,
    /// Dates count from 1904 instead of 1900.
    date1904: bool,
}

/// Values of the attribute `name` on every element with local name `element`, in document order.
fn attr_values(xml: &str, element: &str, name: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) if local(&e) == element => {
                out.extend(attr(&e, name));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Number format code of each cell style in `styles.xml` (the `xf` entries of `cellXfs`):
/// custom `numFmt` codes by id, built-in ones from POI's table, General otherwise.
fn cell_formats(xml: &str) -> Vec<String> {
    let custom: HashMap<String, String> = attr_values(xml, "numFmt", "numFmtId")
        .into_iter()
        .zip(attr_values(xml, "numFmt", "formatCode"))
        .collect();
    let code = |id: Option<String>| {
        let id = id.unwrap_or_default();
        custom
            .get(&id)
            .cloned()
            .or_else(|| id.parse().ok().and_then(numfmt::builtin).map(str::to_owned))
            .unwrap_or_else(|| "General".to_owned())
    };
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut out, mut in_cell_xfs) = (Vec::new(), false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if local(&e) == "cellXfs" => in_cell_xfs = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == "cellXfs" => in_cell_xfs = false,
            Ok(Event::Start(e) | Event::Empty(e)) if in_cell_xfs && local(&e) == "xf" => {
                out.push(code(attr(&e, "numFmtId")));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// The shared string table: the text of each `si`, phonetic runs left out.
fn shared_strings(xml: &str) -> Vec<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut out: Vec<String> = Vec::new();
    let (mut in_t, mut in_phonetic) = (false, false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match local(&e).as_str() {
                "si" => out.push(String::new()),
                "t" => in_t = true,
                "rPh" => in_phonetic = true,
                _ => {}
            },
            Ok(Event::Empty(e)) if local(&e) == "si" => out.push(String::new()),
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "t" => in_t = false,
                "rPh" => in_phonetic = false,
                _ => {}
            },
            Ok(Event::Text(t)) if in_t && !in_phonetic => push_last(&mut out, &t),
            Ok(Event::GeneralRef(r)) if in_t && !in_phonetic => {
                push_last(&mut out, &reference_text(&r))
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Append `text` to the last string of `list`, if any.
fn push_last(list: &mut [String], text: &str) {
    if let Some(last) = list.last_mut() {
        last.push_str(text);
    }
}

/// One worksheet cell while it is read.
#[derive(Default)]
struct Cell {
    /// Cell type attribute `t`: `s` shared string, `inlineStr`, `str`, `b`, `e` or number.
    kind: String,
    /// Cell style index `s`, which selects the number format.
    style: usize,
    value: String,
    /// Inside `v` or an inline string's `t`.
    capturing: bool,
}

/// Cells of a worksheet, tab-separated, one row per line; numbers rendered with their format.
fn sheet_text(xml: &str, book: &Workbook) -> String {
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut out, mut row) = (String::new(), Vec::<String>::new());
    let mut cell: Option<Cell> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => open_cell_part(&e, &mut cell),
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                "c" => row.extend(cell.take().and_then(|c| cell_value(c, book))),
                "row" => {
                    out.push_str(&row.join("\t"));
                    out.push('\n');
                    row.clear();
                }
                "v" | "t" => cell.iter_mut().for_each(|c| c.capturing = false),
                _ => {}
            },
            Ok(Event::Text(t)) => capture(&mut cell, &t),
            Ok(Event::GeneralRef(r)) => capture(&mut cell, &reference_text(&r)),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Start a cell, or start capturing its value.
fn open_cell_part(e: &BytesStart<'_>, cell: &mut Option<Cell>) {
    match (local(e).as_str(), cell.as_mut()) {
        ("c", _) => {
            *cell = Some(Cell {
                kind: attr(e, "t").unwrap_or_default(),
                style: attr(e, "s").and_then(|s| s.parse().ok()).unwrap_or(0),
                ..Default::default()
            })
        }
        ("v" | "t", Some(c)) => c.capturing = true,
        _ => {}
    }
}

/// Append character data to the value of the open cell.
fn capture(cell: &mut Option<Cell>, text: &str) {
    if let Some(c) = cell.as_mut().filter(|c| c.capturing) {
        c.value.push_str(text);
    }
}

/// Display text of a finished cell; `None` for an empty one.
fn cell_value(cell: Cell, book: &Workbook) -> Option<String> {
    let text = match cell.kind.as_str() {
        "s" => book
            .strings
            .get(cell.value.trim().parse::<usize>().ok()?)?
            .clone(),
        "b" => if cell.value.trim() == "1" {
            "TRUE"
        } else {
            "FALSE"
        }
        .to_owned(),
        // Without a cell style (no `styles.xml`), POI keeps the stored value.
        "" | "n" => match book.formats.get(cell.style) {
            Some(code) => numfmt::format(&cell.value, code, book.date1904),
            None => cell.value,
        },
        _ => cell.value,
    };
    (!text.is_empty()).then_some(text)
}

/// OpenDocument: the text of `styles.xml` (headers, footers, master pages) and `content.xml`
/// in container order, as Tika reads them.
fn odf_text(zip: &mut Zip) -> Result<String> {
    let mut out = String::new();
    for index in 0..zip.len() {
        let name = zip.by_index(index)?.name().to_owned();
        if name == "content.xml" || name == "styles.xml" {
            out.push_str(&xml_text(&zip_text(zip, &name)?, &ODF));
        }
    }
    Ok(out)
}

/// Title, description and creator from `docProps/core.xml`.
fn core_properties(zip: &mut Zip) -> Document {
    let xml = zip_text(zip, "docProps/core.xml").unwrap_or_default();
    Document {
        title: element_text(&xml, NS_DC, "title"),
        description: element_text(&xml, NS_DC, "description"),
        creator: element_text(&xml, NS_DC, "creator"),
        ..Default::default()
    }
}

/// Title, description and creator from `meta.xml`. Tika takes the creator from
/// `meta:initial-creator`; ODF's `dc:creator` names whoever saved last.
fn odf_meta(zip: &mut Zip) -> Document {
    let xml = zip_text(zip, "meta.xml").unwrap_or_default();
    Document {
        title: element_text(&xml, NS_DC, "title"),
        description: element_text(&xml, NS_DC, "description"),
        creator: element_text(&xml, NS_ODF_META, "initial-creator"),
        ..Default::default()
    }
}

/// Trimmed text of the first element `ns`:`name`; `None` when missing or blank.
fn element_text(xml: &str, ns: &str, name: &str) -> Option<String> {
    let mut reader = NsReader::from_str(xml);
    let mut depth = 0usize;
    let mut out = String::new();
    loop {
        match reader.read_resolved_event() {
            Ok((ResolveResult::Bound(n), Event::Start(e)))
                if depth == 0 && n.as_ref() == ns && local(&e) == name =>
            {
                depth = 1
            }
            Ok((_, Event::Start(_))) if depth > 0 => depth += 1,
            Ok((_, Event::End(_))) if depth > 1 => depth -= 1,
            Ok((_, Event::End(_))) if depth == 1 => break,
            Ok((_, Event::Text(t))) if depth > 0 => out.push_str(&t),
            Ok((_, Event::GeneralRef(r))) if depth > 0 => out.push_str(&reference_text(&r)),
            Ok((_, Event::Eof)) | Err(_) => break,
            _ => {}
        }
    }
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_owned())
}

/// The preview picture the office application saved, if any: ODF `Thumbnails/thumbnail.png`,
/// OOXML the target of the package's thumbnail relationship.
fn thumbnail(zip: &mut Zip, odf: bool) -> Option<Vec<u8>> {
    let name = if odf {
        ODF_THUMBNAIL.to_owned()
    } else {
        relationships(zip, "")
            .into_iter()
            .find(|r| r.kind == REL_THUMBNAIL)?
            .target
    };
    zip_entry(zip, &name).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn saved_thumbnails_are_the_cover() {
        let size = |name: &str, mime: &str| {
            read(&fixture(name), mime)
                .unwrap()
                .cover
                .map(|c| crate::iscc::decode_rgb(&c).unwrap().dimensions())
        };
        assert_eq!(size("demo.odt", ODT), Some((181, 256)));
        assert_eq!(size("demo.pptx", PPTX), Some((256, 144)));
        assert_eq!(size("demo.docx", DOCX), None);
        assert_eq!(size("demo.xlsx", XLSX), None);
    }

    #[test]
    fn broken_container_is_an_error() {
        assert!(read(b"not a zip", DOCX).is_err());
    }

    #[test]
    fn relationship_targets_resolve_against_their_part() {
        assert_eq!(resolve("ppt", "slides/slide1.xml"), "ppt/slides/slide1.xml");
        assert_eq!(
            resolve("ppt/slides", "../notesSlides/notesSlide1.xml"),
            "ppt/notesSlides/notesSlide1.xml"
        );
        assert_eq!(
            resolve("xl", "/xl/worksheets/sheet1.xml"),
            "xl/worksheets/sheet1.xml"
        );
        assert_eq!(
            resolve("", "docProps/thumbnail.jpeg"),
            "docProps/thumbnail.jpeg"
        );
    }

    #[test]
    fn walk_keeps_text_elements_and_skips_fallbacks() {
        let xml = r#"<w:document xmlns:w="w" xmlns:mc="mc"><w:body><w:p><w:r><w:t>One &amp; </w:t><w:tab/><w:t>two</w:t></w:r></w:p>
            <mc:AlternateContent><mc:Choice><w:p><w:r><w:t>box</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>box</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent>
            <w:p><w:r><w:instrText>PAGE</w:instrText><w:t>three</w:t></w:r></w:p></w:body></w:document>"#;
        assert_eq!(xml_text(xml, &WORD), "One &  two\nbox\nthree\n");
    }

    #[test]
    fn odf_walk_leaves_out_page_numbers() {
        let xml = r#"<office:document-styles xmlns:office="o" xmlns:text="t"><text:p>Page <text:page-number>1</text:page-number> of <text:page-count>9</text:page-count></text:p><text:p><text:sheet-name>???</text:sheet-name></text:p></office:document-styles>"#;
        assert_eq!(xml_text(xml, &ODF), "Page  of \n???\n");
    }

    #[test]
    fn sheet_cells_resolve_strings_numbers_and_booleans() {
        let xml = r#"<worksheet><sheetData><row><c t="s"><v>1</v></c><c><v>3.5</v></c><c t="b"><v>1</v></c></row>
            <row><c t="inlineStr"><is><t>inline</t></is></c><c/><c t="str"><f>A1</f><v>formula</v></c></row></sheetData></worksheet>"#;
        let book = Workbook {
            strings: vec!["zero".to_owned(), "one".to_owned()],
            ..Default::default()
        };
        assert_eq!(sheet_text(xml, &book), "one\t3.5\tTRUE\ninline\tformula\n");
    }

    #[test]
    fn numbers_without_a_cell_style_keep_the_stored_value() {
        let xml = r#"<worksheet><sheetData><row><c><v>1.00</v></c><c s="1"><v>1.00</v></c></row></sheetData></worksheet>"#;
        let unstyled = Workbook::default();
        assert_eq!(sheet_text(xml, &unstyled), "1.00\t1.00\n");
        let styled = Workbook {
            formats: vec!["General".to_owned(), "0.0".to_owned()],
            ..Default::default()
        };
        assert_eq!(sheet_text(xml, &styled), "1\t1.0\n");
    }

    #[test]
    fn shared_strings_leave_out_phonetic_runs() {
        let xml = r#"<sst><si><t>plain</t></si><si><r><t>rich </t></r><r><t>text</t></r><rPh><t>fonetik</t></rPh></si><si/></sst>"#;
        assert_eq!(shared_strings(xml), ["plain", "rich text", ""]);
    }
}
