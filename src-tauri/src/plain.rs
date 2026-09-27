//! Plain text and Markdown assets: the text is the file itself. Tika, which iscc-sdk uses, reads
//! Markdown as plain text too, so there is no Markdown parsing. Neither format carries a title.

use crate::asset::Document;

/// UTF-8 byte order mark.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// The text of a UTF-8 file without its byte order mark. Invalid UTF-8 is decoded lossily; c2pa
/// refuses such files, so their inspection reports the reason under Content Credentials.
pub fn read(bytes: &[u8]) -> Document {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    Document {
        text: String::from_utf8_lossy(bytes).into_owned(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bom_is_dropped_and_line_endings_are_kept() {
        assert_eq!(read(b"\xEF\xBB\xBFone\r\ntwo\n").text, "one\r\ntwo\n");
        assert_eq!(read("Zürich".as_bytes()).text, "Zürich");
        assert!(read(b"text").title.is_none());
    }

    #[test]
    fn invalid_utf8_is_replaced() {
        assert_eq!(read(b"caf\xE9 au lait").text, "caf\u{FFFD} au lait");
    }
}
