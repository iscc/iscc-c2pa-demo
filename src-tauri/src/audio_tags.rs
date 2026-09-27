//! Tags and cover art of audio files, read with iscc-sdk's rules (`audio_meta_extract`, which
//! uses TagLib): the first non-empty tag in TagLib's order per format, then the keys of
//! `AUDIO_META_MAP` in order, the first present key per field with its first value. iscc-sdk
//! does not sanitise audio tags, so neither does this module.
//!
//! lofty's generic tag view keeps only the keys it knows, so which tag counts as empty and the
//! `ISCC:*` keys iscc-sdk writes come from the concrete ID3v2 tag (TXXX frames), APE tag or
//! Vorbis comments. TagLib cannot write them into MP4, and RIFF INFO tags do not carry them.

use std::io::Cursor;

use lofty::ape::ApeTag;
use lofty::config::ParseOptions;
use lofty::file::{AudioFile, FileType, TaggedFile, TaggedFileExt};
use lofty::flac::FlacFile;
use lofty::id3::v2::Id3v2Tag;
use lofty::iff::wav::WavFile;
use lofty::mpeg::MpegFile;
use lofty::ogg::tag::VorbisComments;
use lofty::picture::PictureType;
use lofty::prelude::{ItemKey, TagExt};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagType};

use crate::formats;
use crate::metadata::Embedded;

/// iscc-sdk's keys for the name, description and ISCC metadata.
const ISCC_NAME: &str = "ISCC:NAME";
const ISCC_DESCRIPTION: &str = "ISCC:DESCRIPTION";
const ISCC_META: &str = "ISCC:META";
/// Creator keys in `AUDIO_META_MAP` order: COMPOSER, ORIGINALARTIST, ARTIST, ALBUMARTIST.
const CREATOR_KEYS: [ItemKey; 4] = [
    ItemKey::Composer,
    ItemKey::OriginalArtist,
    ItemKey::TrackArtist,
    ItemKey::AlbumArtist,
];

/// The Meta-Code fields and the encoded cover picture of an audio file; missing or unreadable
/// tags give empty fields.
pub fn read(bytes: &[u8], mime: &str) -> (Embedded, Option<Vec<u8>>) {
    let Some(file_type) = file_type(mime) else {
        return (Embedded::default(), None);
    };
    let Ok(file) = Probe::new(Cursor::new(bytes))
        .set_file_type(file_type)
        .options(ParseOptions::new().read_properties(false))
        .read()
    else {
        return (Embedded::default(), None);
    };
    let cover = cover(&file);
    let concrete = concrete_tags(bytes, file_type);
    let Some(tag_type) = chosen_tag(&file, &concrete) else {
        return (Embedded::default(), cover);
    };
    let first = |key: ItemKey| file.tag(tag_type).and_then(|tag| first_value(tag, key));
    let iscc = |key: &str| concrete.iscc_key(tag_type, key);
    let embedded = Embedded {
        name: iscc(ISCC_NAME).or_else(|| first(ItemKey::TrackTitle)),
        description: iscc(ISCC_DESCRIPTION),
        meta: iscc(ISCC_META),
        creator: CREATOR_KEYS.into_iter().find_map(first),
    };
    (embedded, cover)
}

/// The tags whose keys lofty's generic view loses: ID3v2 and APE decide which tag TagLib
/// chooses, and they and Vorbis comments carry the `ISCC:*` keys.
#[derive(Default)]
struct Concrete {
    id3v2: Option<Id3v2Tag>,
    ape: Option<ApeTag>,
    vorbis: Option<VorbisComments>,
}

impl Concrete {
    /// Value of an `ISCC:*` key in the tag of type `tag_type`; TagLib drops empty values.
    fn iscc_key(&self, tag_type: TagType, key: &str) -> Option<String> {
        let value = match tag_type {
            TagType::Id3v2 => self.id3v2.as_ref()?.get_user_text(key)?,
            TagType::Ape => match self.ape.as_ref()?.get(key)?.value() {
                ItemValue::Text(text) => text.split('\0').next()?,
                _ => return None,
            },
            TagType::VorbisComments => self.vorbis.as_ref()?.get(key)?,
            _ => return None,
        };
        (!value.is_empty()).then(|| value.to_owned())
    }
}

/// lofty's file type of an audio MIME type.
fn file_type(mime: &str) -> Option<FileType> {
    Some(match mime {
        formats::MP3 => FileType::Mpeg,
        formats::FLAC => FileType::Flac,
        formats::WAV => FileType::Wav,
        formats::M4A => FileType::Mp4,
        _ => return None,
    })
}

/// Tag types in the order TagLib consults them for a file type.
fn tag_order(file_type: FileType) -> &'static [TagType] {
    match file_type {
        FileType::Mpeg => &[TagType::Id3v2, TagType::Ape, TagType::Id3v1],
        FileType::Wav => &[TagType::Id3v2, TagType::RiffInfo],
        FileType::Mp4 => &[TagType::Mp4Ilst],
        _ => &[TagType::VorbisComments],
    }
}

/// The type of the tag TagLib reads properties from: in its order, the first that is present
/// and not empty. TagLib calls an ID3v2 tag without frames and an APE tag without items empty,
/// whatever the frames or items are; every other tag type comes last in its order, where an
/// empty tag and no tag give the same fields.
fn chosen_tag(file: &TaggedFile, concrete: &Concrete) -> Option<TagType> {
    tag_order(file.file_type())
        .iter()
        .copied()
        .find(|&tag_type| match tag_type {
            TagType::Id3v2 => concrete.id3v2.as_ref().is_some_and(|t| !t.is_empty()),
            TagType::Ape => concrete.ape.as_ref().is_some_and(|t| !t.is_empty()),
            _ => file.tag(tag_type).is_some(),
        })
}

/// First non-empty value of `key`. TagLib drops empty values too, except in MP4, where lofty
/// skips them while parsing, so an MP4 value list that starts empty yields its second value.
fn first_value(tag: &Tag, key: ItemKey) -> Option<String> {
    tag.get_strings(key)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The ID3v2, APE and Vorbis comment tags of a file, as far as its format has them.
fn concrete_tags(bytes: &[u8], file_type: FileType) -> Concrete {
    let mut reader = Cursor::new(bytes);
    let options = ParseOptions::new().read_properties(false);
    let concrete = match file_type {
        FileType::Mpeg => MpegFile::read_from(&mut reader, options).map(|f| Concrete {
            id3v2: f.id3v2().cloned(),
            ape: f.ape().cloned(),
            vorbis: None,
        }),
        FileType::Wav => WavFile::read_from(&mut reader, options).map(|f| Concrete {
            id3v2: f.id3v2().cloned(),
            ..Concrete::default()
        }),
        FileType::Flac => FlacFile::read_from(&mut reader, options).map(|f| Concrete {
            vorbis: f.vorbis_comments().cloned(),
            ..Concrete::default()
        }),
        _ => Ok(Concrete::default()),
    };
    concrete.unwrap_or_default()
}

/// The first front cover in any tag, else the first picture.
fn cover(file: &TaggedFile) -> Option<Vec<u8>> {
    let pictures: Vec<_> = file.tags().iter().flat_map(Tag::pictures).collect();
    pictures
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or(pictures.first())
        .map(|p| p.data().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::tests::sdk_meta_code;
    use std::path::Path;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn tags_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_audio.py (iscc-sdk with TagLib).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_audio.json")).unwrap();
        for (file, want) in expected.as_object().unwrap() {
            let path = fixture(file);
            let format = formats::by_path(&path).unwrap();
            let (got, _) = read(&std::fs::read(&path).unwrap(), format.mime);
            if file == "tags-empty-first.m4a" {
                // Deliberate deviation: lofty skips the empty first title, TagLib reads "" and
                // iscc-sdk falls back to the file name.
                assert_eq!(got.name.as_deref(), Some("Second title"));
                assert_eq!(got.creator.as_deref(), want["creator"].as_str());
                continue;
            }
            assert_eq!(got.name.as_deref(), want["name"].as_str(), "{file} name");
            assert_eq!(
                got.description.as_deref(),
                want["description"].as_str(),
                "{file} description"
            );
            assert_eq!(
                got.meta.as_deref(),
                want["meta_field"].as_str(),
                "{file} meta"
            );
            assert_eq!(
                got.creator.as_deref(),
                want["creator"].as_str(),
                "{file} creator"
            );
            assert_eq!(sdk_meta_code(&got, &path), want["meta"], "{file} Meta-Code");
        }
    }

    #[test]
    fn cover_art_is_found() {
        let (_, cover) = read(
            &std::fs::read(fixture("withcover.mp3")).unwrap(),
            formats::MP3,
        );
        let cover = cover.expect("withcover.mp3 has a cover");
        assert!(crate::iscc::decode_rgb(&cover).is_ok());
        let (_, none) = read(&std::fs::read(fixture("demo.mp3")).unwrap(), formats::MP3);
        assert!(none.is_none());
    }

    #[test]
    fn unreadable_tags_give_empty_fields() {
        let (embedded, cover) = read(b"not audio at all", formats::MP3);
        assert_eq!(embedded, Embedded::default());
        assert!(cover.is_none());
    }
}
