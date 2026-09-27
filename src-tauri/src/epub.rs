//! EPUB assets: title, description, creators, cover image and plain text in reading order, read
//! with rbook.
//!
//! The text is what a reader sees: the text nodes of every spine document in reading order,
//! cleaned like `iscc_sdk.text_extract`. Tika is not involved, yet the Content-Code Text of the
//! fixture agrees with the iscc-sdk reference bit for bit; other books may differ slightly.

use std::io::Cursor;

use anyhow::{anyhow, Result};
use quick_xml::events::Event;
use rbook::Epub;

use crate::asset::Document;

/// Elements whose content is not text a reader sees.
const SKIPPED_ELEMENTS: &[&str] = &["head", "script", "style"];

/// Elements whose end marks a line break in the extracted text.
const BLOCK_ELEMENTS: &[&str] = &[
    "p",
    "div",
    "br",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "tr",
    "blockquote",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "aside",
    "figure",
    "figcaption",
    "dt",
    "dd",
    "hr",
    "table",
];

/// Parse an EPUB held in memory. The cover comes in whatever format the book ships (PNG, JPEG,
/// SVG, ...); several creators are joined with ", ", as iscc-sdk does.
pub fn read(bytes: &[u8]) -> Result<Document> {
    let epub =
        Epub::read(Cursor::new(bytes.to_vec())).map_err(|e| anyhow!("cannot read EPUB: {e}"))?;
    let metadata = epub.metadata();
    let title = metadata.title().and_then(|t| non_empty(t.value()));
    let description = metadata.description().and_then(|d| non_empty(d.value()));
    let creators: Vec<String> = metadata
        .creators()
        .filter_map(|c| non_empty(c.value()))
        .collect();
    let cover = epub
        .manifest()
        .cover_image()
        .and_then(|c| c.read_bytes().ok());
    let mut text = String::new();
    let mut reader = epub.reader();
    while let Some(content) = reader.read_next() {
        // Spine items that cannot be read (missing or non-XML resources) contribute nothing.
        if let Ok(content) = content {
            text.push_str(&xhtml_text(content.content()));
            text.push('\n');
        }
    }
    Ok(Document {
        title,
        description,
        creator: (!creators.is_empty()).then(|| creators.join(", ")),
        cover,
        text,
    })
}

/// Trimmed copy of `s`, or `None` when nothing is left.
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

/// Text nodes of an XHTML document outside head, script and style, with a line break after each
/// block element. References are decoded with the HTML entity table, which covers the EPUB 2 DTD.
/// A malformed document yields the text read up to the error.
fn xhtml_text(xhtml: &str) -> String {
    let mut reader = quick_xml::Reader::from_str(xhtml);
    let mut out = String::new();
    let mut skipped = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if is_skipped(e.local_name().as_ref()) => skipped += 1,
            Ok(Event::End(e)) if is_skipped(e.local_name().as_ref()) => {
                skipped = skipped.saturating_sub(1)
            }
            Ok(Event::End(e)) if is_block(e.local_name().as_ref()) => out.push('\n'),
            Ok(Event::Empty(e)) if is_block(e.local_name().as_ref()) => out.push('\n'),
            Ok(Event::Text(t)) if skipped == 0 => out.push_str(&t),
            Ok(Event::GeneralRef(r)) if skipped == 0 => out.push_str(
                &html_escape::decode_html_entities(&format!("&{};", r.xml10_content())),
            ),
            Ok(Event::CData(c)) if skipped == 0 => out.push_str(&c.into_inner()),
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

fn is_skipped(name: &str) -> bool {
    SKIPPED_ELEMENTS.contains(&name)
}

fn is_block(name: &str) -> bool {
    BLOCK_ELEMENTS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iscc;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn xhtml_text_keeps_visible_text_only() {
        let doc = r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>Chapter</title>
            <style>p { color: red }</style></head><body><h1>One &amp; two</h1><p>Caf&eacute; <em>au</em> lait<br/>next</p>
            <script>alert(1)</script><p><![CDATA[raw <text>]]></p></body></html>"#;
        assert_eq!(
            xhtml_text(doc),
            "One & two\nCafé au lait\nnext\n\n            raw <text>\n"
        );
    }

    #[test]
    fn epub_metadata_and_cover() {
        let doc = read(&fixture("demo.epub")).unwrap();
        assert_eq!(doc.title.as_deref(), Some("title from metadata"));
        assert!(doc.description.is_none());
        assert_eq!(
            doc.creator.as_deref(),
            Some("Charles Madison Curry, Erle Elsworth Clippinger")
        );
        let cover = doc.cover.expect("cover image");
        assert!(iscc::decode_rgb(&cover).unwrap().dimensions().0 > 0);
        assert!(doc.text.contains("FAIRY STORIES"), "{}", &doc.text[..200]);
    }
}
