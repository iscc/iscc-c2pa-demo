//! One reader for every supported format: the content behind the Content-Code, a preview picture,
//! the asset's own title and description, and its creator.

use std::path::Path;

use anyhow::{bail, Result};
use image::RgbImage;

use crate::formats::{self, Format, Kind};
use crate::iscc::{self, Content};
use crate::metadata::{self, Embedded};
use crate::{audio, epub, office, plain, svg};

/// The input of the Content-Code.
#[derive(Debug)]
pub enum AssetContent {
    /// Decoded, EXIF-transposed and flattened on white; also the preview.
    Image(RgbImage),
    /// Plain text, cleaned like `iscc_sdk.code_text` does.
    Text(String),
    /// Chromaprint fingerprint and duration of the decoded audio.
    Audio(audio::Audio),
}

/// What an asset contributes to inspection and signing.
#[derive(Debug)]
pub struct Asset {
    pub content: AssetContent,
    /// Picture of a non-image asset: an EPUB cover, an office thumbnail or audio cover art.
    pub preview: Option<RgbImage>,
    /// The asset's own title, description, ISCC metadata and creator.
    pub metadata: Embedded,
}

/// Parts of a text document as its format reader finds them.
#[derive(Debug, Default)]
pub struct Document {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    /// Encoded cover or thumbnail image, in whatever format the document ships.
    pub cover: Option<Vec<u8>>,
    /// Plain text in reading order, not yet cleaned.
    pub text: String,
}

impl Asset {
    /// The content as the ISCC functions take it.
    pub fn content(&self) -> Content<'_> {
        match &self.content {
            AssetContent::Image(rgb) => Content::Image(rgb),
            AssetContent::Text(text) => Content::Text(text),
            AssetContent::Audio(audio) => Content::Audio(&audio.fingerprint),
        }
    }

    /// The picture to show: the image itself, or the preview of a document or audio file.
    pub fn picture(&self) -> Option<&RgbImage> {
        match &self.content {
            AssetContent::Image(rgb) => Some(rgb),
            AssetContent::Text(_) | AssetContent::Audio(_) => self.preview.as_ref(),
        }
    }
}

/// Read the asset held in `bytes`; `path` names the file for formats that resolve resources
/// next to it (SVG).
pub fn read(path: &Path, bytes: &[u8], format: &Format) -> Result<Asset> {
    match format.mime {
        formats::SVG => svg::read(path, bytes),
        _ if format.kind == Kind::Image => read_raster(bytes),
        _ if format.kind == Kind::Audio => audio::read(bytes, format),
        formats::EPUB => Ok(from_document(epub::read(bytes)?)),
        formats::TXT | formats::MARKDOWN => Ok(from_document(plain::read(bytes))),
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
    })
}

/// A text document with its cleaned text and, when the image crate can decode it, its cover.
fn from_document(doc: Document) -> Asset {
    Asset {
        content: AssetContent::Text(iscc_lib::text_clean(&doc.text)),
        preview: doc.cover.and_then(|c| iscc::decode_rgb(&c).ok()),
        metadata: metadata::document(
            doc.title.as_deref(),
            doc.description.as_deref(),
            doc.creator.as_deref(),
        ),
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
            let asset = read(&path, &bytes, formats::by_path(&path).unwrap()).unwrap();
            if let AssetContent::Text(text) = asset.content {
                std::fs::write(Path::new(&dir).join(format!("{file}.txt")), text).unwrap();
            }
        }
    }

    #[test]
    fn text_units_and_metadata_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_text.py (iscc-sdk with Tika).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_text.json")).unwrap();
        let selection = UnitSelection {
            meta: false,
            content: true,
            data: true,
            instance: true,
        };
        for (file, want) in expected.as_object().unwrap() {
            let path = fixture(file);
            let bytes = std::fs::read(&path).unwrap();
            let asset = read(&path, &bytes, formats::by_path(&path).unwrap()).unwrap();
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
            let units =
                iscc::units_for(&bytes, asset.content(), Default::default(), &selection).unwrap();
            assert_eq!(units[0].iscc, want["text"], "{file} Content-Code Text");
            assert_eq!(units[1].iscc, want["data"], "{file} Data-Code");
            assert_eq!(units[2].iscc, want["instance"], "{file} Instance-Code");
        }
    }
}
