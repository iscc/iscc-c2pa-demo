//! The file formats this app inspects and signs, in one table: extensions, MIME type, the kind
//! of content (which decides the Content-Code) and a display label.

use std::path::Path;

use serde::Serialize;

/// Kind of content an asset carries; decides which Content-Code applies.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Text,
    Audio,
}

impl Kind {
    /// Every kind, in display order.
    pub const ALL: [Kind; 3] = [Kind::Image, Kind::Text, Kind::Audio];

    /// Plural display name, as used in file dialogs.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Image => "Images",
            Kind::Text => "Documents",
            Kind::Audio => "Audio",
        }
    }

    /// The serialised kind, which is also the slug of its Content-Code in unit selections.
    pub fn slug(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::Text => "text",
            Kind::Audio => "audio",
        }
    }
}

/// One supported file format.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Format {
    /// Lower-case file extensions, the preferred one first.
    pub extensions: &'static [&'static str],
    /// MIME type, as c2pa's asset handlers accept it.
    pub mime: &'static str,
    pub kind: Kind,
    /// Display name.
    pub label: &'static str,
    /// Name in format lists, at most five characters.
    pub short: &'static str,
}

/// Shorthand for a signable table row.
const fn signable(
    extensions: &'static [&'static str],
    mime: &'static str,
    kind: Kind,
    label: &'static str,
    short: &'static str,
) -> Format {
    Format {
        extensions,
        mime,
        kind,
        label,
        short,
    }
}

pub const JPEG: &str = "image/jpeg";
pub const PNG: &str = "image/png";
pub const WEBP: &str = "image/webp";
pub const GIF: &str = "image/gif";
pub const TIFF: &str = "image/tiff";
pub const SVG: &str = "image/svg+xml";
pub const EPUB: &str = "application/epub+zip";
pub const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
pub const PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
pub const XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
pub const ODT: &str = "application/vnd.oasis.opendocument.text";
pub const ODS: &str = "application/vnd.oasis.opendocument.spreadsheet";
pub const ODP: &str = "application/vnd.oasis.opendocument.presentation";
pub const TXT: &str = "text/plain";
pub const MARKDOWN: &str = "text/markdown";
pub const MP3: &str = "audio/mpeg";
pub const FLAC: &str = "audio/flac";
pub const WAV: &str = "audio/wav";
pub const M4A: &str = "audio/mp4";

/// Every supported format. c2pa-rs 0.91 embeds manifests into all of them; TXT and Markdown need
/// its `unstable_plain_text` and `unstable_structured_text` features.
pub const FORMATS: &[Format] = &[
    signable(&["jpg", "jpeg"], JPEG, Kind::Image, "JPEG", "JPEG"),
    signable(&["png"], PNG, Kind::Image, "PNG", "PNG"),
    signable(&["webp"], WEBP, Kind::Image, "WebP", "WebP"),
    signable(&["gif"], GIF, Kind::Image, "GIF", "GIF"),
    signable(&["tif", "tiff"], TIFF, Kind::Image, "TIFF", "TIFF"),
    signable(&["svg"], SVG, Kind::Image, "SVG", "SVG"),
    signable(&["epub"], EPUB, Kind::Text, "EPUB", "EPUB"),
    signable(&["docx"], DOCX, Kind::Text, "Word (DOCX)", "DOCX"),
    signable(&["pptx"], PPTX, Kind::Text, "PowerPoint (PPTX)", "PPTX"),
    signable(&["xlsx"], XLSX, Kind::Text, "Excel (XLSX)", "XLSX"),
    signable(&["odt"], ODT, Kind::Text, "OpenDocument Text", "ODT"),
    signable(&["ods"], ODS, Kind::Text, "OpenDocument Spreadsheet", "ODS"),
    signable(
        &["odp"],
        ODP,
        Kind::Text,
        "OpenDocument Presentation",
        "ODP",
    ),
    signable(&["txt"], TXT, Kind::Text, "Plain text", "TXT"),
    signable(&["md", "markdown"], MARKDOWN, Kind::Text, "Markdown", "MD"),
    signable(&["mp3"], MP3, Kind::Audio, "MP3", "MP3"),
    signable(&["flac"], FLAC, Kind::Audio, "FLAC", "FLAC"),
    signable(&["wav"], WAV, Kind::Audio, "WAV", "WAV"),
    signable(&["m4a"], M4A, Kind::Audio, "M4A", "M4A"),
];

/// Format of the file at `path`, from its extension (case-insensitive).
pub fn by_path(path: &Path) -> Option<&'static Format> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    FORMATS
        .iter()
        .find(|f| f.extensions.contains(&ext.as_str()))
}

/// Short names of every format of `kind`, in table order.
pub fn short_names(kind: Kind) -> Vec<&'static str> {
    FORMATS
        .iter()
        .filter(|f| f.kind == kind)
        .map(|f| f.short)
        .collect()
}

/// Extensions of every format of `kind`, in table order.
pub fn extensions(kind: Kind) -> Vec<&'static str> {
    FORMATS
        .iter()
        .filter(|f| f.kind == kind)
        .flat_map(|f| f.extensions.iter().copied())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_by_extension_ignores_case() {
        assert_eq!(by_path(Path::new("a/B.JPEG")).unwrap().mime, JPEG);
        assert_eq!(by_path(Path::new("notes.Md")).unwrap().kind, Kind::Text);
        assert!(by_path(Path::new("page.html")).is_none());
        assert!(by_path(Path::new("README")).is_none());
    }

    #[test]
    fn c2pa_accepts_every_mime_type() {
        // Every format must be one c2pa can embed into and read from.
        for format in FORMATS {
            assert!(
                c2pa::Builder::supported_mime_types()
                    .iter()
                    .any(|m| m == format.mime),
                "{}",
                format.mime
            );
        }
    }

    #[test]
    fn kinds_serialize_to_the_ui_strings() {
        assert_eq!(serde_json::to_value(Kind::Image).unwrap(), "image");
        assert_eq!(serde_json::to_value(Kind::Text).unwrap(), "text");
        for kind in Kind::ALL {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.slug());
        }
        assert!(extensions(Kind::Text).contains(&"epub"));
        assert!(extensions(Kind::Audio).contains(&"m4a"));
    }

    #[test]
    fn short_names_are_unique_and_short() {
        let mut names: Vec<_> = FORMATS.iter().map(|f| f.short).collect();
        assert!(names.iter().all(|n| !n.is_empty() && n.len() <= 5));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), FORMATS.len());
        assert_eq!(short_names(Kind::Text).last(), Some(&"MD"));
    }
}
