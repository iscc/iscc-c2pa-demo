//! Audio assets: the Chromaprint fingerprint behind the Content-Code Audio, computed like
//! iscc-sdk's `audio_features_extract` (fpcalc `-raw -json -signed -length 0`), plus the tags and
//! cover art from `audio_tags`.
//!
//! fpcalc lets FFmpeg decode, swresample convert to 11025 Hz mono s16 and Chromaprint run its
//! TEST2 algorithm. Here symphonia decodes, [`Resampler`] converts and rusty-chromaprint
//! fingerprints, streaming packet by packet. Samples FFmpeg drops are dropped too: MP3 encoder
//! delay and padding (symphonia's gapless mode) and the MP4 edit list's priming (symphonia
//! ignores edit lists).
//!
//! MP4, MOV and M4V files with a sound track and no video track ([`sound_only`]) are read like
//! an M4A.
//!
//! The decoders are symphonia's plus the ones this app registers ([`CODECS`]): Opus, which
//! symphonia reads out of an MP4 but cannot decode. A file whose container reads but whose
//! audio does not decode (AC-3, HE-AAC, more than two AAC channels, a track type the MP4 reader
//! does not know) still inspects and signs: it has no Content-Code Audio, with the reason in
//! its place ([`Undecodable`]).
//!
//! Known deviations from fpcalc: more than two channels are averaged (swresample uses a downmix
//! matrix; 0 to 2 bits per 64 on 5.1), packets that fail to decode are skipped, and Opus is
//! decoded by another decoder (`opus`), which costs a few bits of the code.

use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{anyhow, Context as _, Result};
use rusty_chromaprint::{Configuration, Fingerprinter};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;

use crate::asset::{Asset, AssetContent};
use crate::formats::{self, Format};
use crate::resample::{Resampler, OUTPUT_RATE};
use crate::{audio_tags, iscc, opus};

/// The decoded audio as far as the ISCC needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    /// Chromaprint TEST2 fingerprint, as fpcalc prints it with `-signed`; empty when the audio
    /// is too short.
    pub fingerprint: Vec<i32>,
    /// Duration of the decoded audio after trimming.
    pub seconds: f64,
}

/// Why a file that reads as its format has no audio to fingerprint; the reason takes the place
/// of the Content-Code Audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Undecodable(pub &'static str);

impl fmt::Display for Undecodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Undecodable {}

/// The file has no audio track, or one of a type symphonia's reader does not know.
const NO_TRACK: Undecodable = Undecodable("no audio track found that this app can read");
/// No registered decoder takes the track's codec, or its flavour of the codec.
const NO_DECODER: Undecodable = Undecodable("this audio codec is not supported");

/// symphonia's decoders and the ones this app adds; a further codec joins by registering here.
static CODECS: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry.register_audio_decoder::<opus::OpusDecoder>();
    registry
});

/// Read the audio asset held in `bytes`: fingerprint, tags and cover art. Audio that does not
/// decode leaves the asset without content, with the reason ([`Undecodable`]).
pub fn read(bytes: &[u8], format: &Format) -> Result<Asset> {
    let content = match analyse(bytes, format) {
        Ok(audio) => AssetContent::Audio(audio),
        Err(error) => AssetContent::Unavailable(error.downcast::<Undecodable>()?.0),
    };
    let (metadata, cover) = audio_tags::read(bytes, format.mime);
    Ok(Asset {
        content,
        preview: cover.and_then(|c| iscc::decode_rgb(&c).ok()),
        metadata,
        sign_block: None,
        sign_warning: None,
        ocr: None,
    })
}

/// Decode `bytes` and fingerprint the audio of its default track; an [`Undecodable`] error when
/// the file reads but has no track or no decoder for it.
pub fn analyse(bytes: &[u8], format: &Format) -> Result<Audio> {
    let source = MediaSourceStream::new(Box::new(Cursor::new(bytes.to_vec())), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(format.extensions[0])
        .mime_type(format.mime);
    let mut reader = symphonia::default::get_probe()
        .probe(
            &hint,
            source,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .with_context(|| format!("cannot read this {} file", format.label))?;
    let track = reader.default_track(TrackType::Audio).ok_or(NO_TRACK)?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or(NO_TRACK)?
        .clone();
    let skip = if formats::is_bmff(format.mime) {
        mp4_priming(bytes, params.sample_rate.unwrap_or(0))
    } else {
        0
    };
    decode(reader.as_mut(), track_id, &params, skip)
}

/// Fingerprint a track symphonia decodes, dropping its first `skip` frames.
fn decode(
    reader: &mut dyn FormatReader,
    track_id: u32,
    params: &AudioCodecParameters,
    skip: u64,
) -> Result<Audio> {
    let mut decoder = CODECS
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|_| NO_DECODER)?;
    let mut sink = Chromaprint::new(skip)?;
    let (mut interleaved, mut mono) = (Vec::new(), Vec::new());
    while let Some(packet) = next_packet(reader, track_id)? {
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        interleaved.resize(buffer.samples_interleaved(), 0f32);
        buffer.copy_to_slice_interleaved(&mut interleaved);
        downmix(&interleaved, buffer.spec().channels().count(), &mut mono);
        sink.push(&mono, buffer.spec().rate());
    }
    Ok(sink.finish())
}

/// The next packet of track `track_id`; `None` at the end of the stream, including a stream
/// that ends early.
fn next_packet(reader: &mut dyn FormatReader, track_id: u32) -> Result<Option<Packet>> {
    loop {
        match reader.next_packet() {
            Ok(Some(packet)) if packet.track_id == track_id => return Ok(Some(packet)),
            Ok(Some(_)) => continue,
            Ok(None) => return Ok(None),
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None)
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Downmix interleaved frames to mono: one channel as is, two as their f32 average, more as the
/// mean of all channels.
fn downmix(interleaved: &[f32], channels: usize, mono: &mut Vec<f32>) {
    mono.clear();
    match channels {
        0 => {}
        1 => mono.extend_from_slice(interleaved),
        2 => mono.extend(
            interleaved
                .as_chunks::<2>()
                .0
                .iter()
                .map(|[l, r]| l * 0.5 + r * 0.5),
        ),
        n => mono.extend(
            interleaved
                .chunks_exact(n)
                .map(|f| f.iter().sum::<f32>() / n as f32),
        ),
    }
}

/// Where the mono samples go: trimming, resampling to 11025 Hz and Chromaprint.
struct Chromaprint {
    /// Frames still to drop from the start.
    skip: u64,
    /// Frames kept, and their rate.
    frames: u64,
    rate: u32,
    resampler: Option<Resampler>,
    fingerprinter: Fingerprinter,
    pcm: Vec<i16>,
}

impl Chromaprint {
    /// A sink that drops the first `skip` frames.
    fn new(skip: u64) -> Result<Self> {
        let mut fingerprinter = Fingerprinter::new(&Configuration::preset_test2());
        fingerprinter
            .start(OUTPUT_RATE, 1)
            .map_err(|e| anyhow!("cannot start Chromaprint: {e:?}"))?;
        Ok(Self {
            skip,
            frames: 0,
            rate: 0,
            resampler: None,
            fingerprinter,
            pcm: Vec::new(),
        })
    }

    /// Take the next mono frames at `rate`; the first call fixes the rate.
    fn push(&mut self, mono: &[f32], rate: u32) {
        let dropped = usize::try_from(self.skip).map_or(mono.len(), |s| s.min(mono.len()));
        self.skip -= dropped as u64;
        let mono = &mono[dropped..];
        self.frames += mono.len() as u64;
        let resampler = self.resampler.get_or_insert_with(|| Resampler::new(rate));
        self.rate = rate;
        self.pcm.clear();
        resampler.push(mono, &mut self.pcm);
        self.fingerprinter.consume(&self.pcm);
    }

    /// Flush the resampler and read the fingerprint.
    fn finish(mut self) -> Audio {
        if let Some(resampler) = self.resampler.as_mut() {
            self.pcm.clear();
            resampler.finish(&mut self.pcm);
            self.fingerprinter.consume(&self.pcm);
        }
        self.fingerprinter.finish();
        Audio {
            fingerprint: self
                .fingerprinter
                .fingerprint()
                .iter()
                .map(|&x| x as i32)
                .collect(),
            seconds: match self.rate {
                0 => 0.0,
                rate => self.frames as f64 / f64::from(rate),
            },
        }
    }
}

/// Frames at `rate` that FFmpeg skips at the start of an MP4 file: the first edit list entry
/// of the sound track that starts inside the media (AAC encoder priming), converted from the
/// media timescale. 0 without such an entry or when the file cannot be parsed.
pub fn mp4_priming(bytes: &[u8], rate: u32) -> u64 {
    child(bytes, b"moov")
        .and_then(|moov| {
            boxes(moov)
                .filter(|(kind, _)| kind == b"trak")
                .find_map(|(_, trak)| sound_track_priming(trak))
        })
        .and_then(|(media_time, timescale)| {
            let frames = u128::from(media_time) * u128::from(rate) / u128::from(timescale);
            u64::try_from(frames).ok()
        })
        .unwrap_or(0)
}

/// Media time of the first edit starting inside the media and the media timescale, for a sound
/// track; `None` for other tracks. A sound track without an edit list starts at 0.
fn sound_track_priming(trak: &[u8]) -> Option<(u64, u32)> {
    let mdia = child(trak, b"mdia")?;
    if handler(mdia)? != b"soun" {
        return None;
    }
    let timescale = mdhd_timescale(child(mdia, b"mdhd")?).filter(|&t| t > 0)?;
    let media_time = child(trak, b"edts")
        .and_then(|edts| child(edts, b"elst"))
        .and_then(first_media_time)
        .unwrap_or(0);
    Some((media_time, timescale))
}

/// Handler type of a media box: `soun` for sound, `vide` for video, and so on.
fn handler(mdia: &[u8]) -> Option<&[u8]> {
    child(mdia, b"hdlr")?.get(8..12)
}

/// Largest movie box read to tell whether a file carries sound only; a larger one belongs to a
/// video (64 MiB of sample tables is days of sound).
const MAX_MOOV: u64 = 64 << 20;

/// Whether the ISO BMFF file at `path` (MP4, MOV, M4V) has a sound track and no video track.
/// False when the file cannot be read or its movie box cannot be found.
pub fn sound_only(path: &Path) -> bool {
    let Some(moov) = File::open(path).ok().and_then(movie_box) else {
        return false;
    };
    let handlers: Vec<&[u8]> = boxes(&moov)
        .filter(|(kind, _)| kind == b"trak")
        .filter_map(|(_, trak)| child(trak, b"mdia").and_then(handler))
        .collect();
    handlers.contains(&&b"soun"[..]) && !handlers.contains(&&b"vide"[..])
}

/// Body of the top-level movie box (`moov`), which may come before or after the media data;
/// every other box is skipped without reading it.
fn movie_box(mut file: File) -> Option<Vec<u8>> {
    loop {
        let (kind, length) = box_header(&mut file)?;
        if &kind == b"moov" {
            let limit = length.unwrap_or(u64::MAX).min(MAX_MOOV + 1);
            let mut body = Vec::new();
            (&mut file).take(limit).read_to_end(&mut body).ok()?;
            let complete = length.is_none_or(|n| n == body.len() as u64);
            return (complete && body.len() as u64 <= MAX_MOOV).then_some(body);
        }
        file.seek(SeekFrom::Current(i64::try_from(length?).ok()?))
            .ok()?;
    }
}

/// Type and body length of the box at the reader's position, the reader left at its body; the
/// length is `None` for a box that runs to the end of the file.
fn box_header(reader: &mut impl Read) -> Option<([u8; 4], Option<u64>)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let kind = header[4..].try_into().ok()?;
    let length = match u32::from_be_bytes(header[..4].try_into().ok()?) {
        0 => None,
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large).ok()?;
            Some(u64::from_be_bytes(large).checked_sub(16)?)
        }
        n => Some(u64::from(n).checked_sub(8)?),
    };
    Some((kind, length))
}

/// Timescale of a media header box.
fn mdhd_timescale(mdhd: &[u8]) -> Option<u32> {
    let at = if *mdhd.first()? == 1 { 20 } else { 12 };
    Some(u32::from_be_bytes(mdhd.get(at..at + 4)?.try_into().ok()?))
}

/// Media time of the first edit list entry that is not an empty edit (media time -1).
fn first_media_time(elst: &[u8]) -> Option<u64> {
    let long = *elst.first()? == 1;
    let count = u32::from_be_bytes(elst.get(4..8)?.try_into().ok()?) as usize;
    let size = if long { 20 } else { 12 };
    (0..count).find_map(|i| {
        let entry = elst.get(8 + i * size..8 + (i + 1) * size)?;
        let media_time = if long {
            i64::from_be_bytes(entry[8..16].try_into().ok()?)
        } else {
            i64::from(i32::from_be_bytes(entry[4..8].try_into().ok()?))
        };
        u64::try_from(media_time).ok()
    })
}

/// Body of the first child box of type `kind`.
fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(k, _)| k == kind).map(|(_, body)| body)
}

/// ISO BMFF boxes in `data` as (type, body); stops at the first malformed header.
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut rest = data;
    std::iter::from_fn(move || {
        let (kind, body, next) = split_box(rest)?;
        rest = next;
        Some((kind, body))
    })
}

/// The first box of `data`: its type, its body and the data after it.
fn split_box(data: &[u8]) -> Option<([u8; 4], &[u8], &[u8])> {
    let size = u32::from_be_bytes(data.get(..4)?.try_into().ok()?);
    let kind: [u8; 4] = data.get(4..8)?.try_into().ok()?;
    let (header, total) = match size {
        0 => (8, data.len()),
        1 => (
            16,
            usize::try_from(u64::from_be_bytes(data.get(8..16)?.try_into().ok()?)).ok()?,
        ),
        n => (8, n as usize),
    };
    if total < header || total > data.len() {
        return None;
    }
    Some((kind, &data[header..total], &data[total..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iscc::{self, Content};
    use std::path::Path;

    /// Least share of equal bits between an Opus fixture's Content-Code Audio and fpcalc's
    /// (Titusz, 2026-10-04: up to 7% apart is fine; `no-video-opus.mp4` measured 1 bit of 256).
    const OPUS_SIMILARITY: f64 = 0.93;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// `demo.m4a` with its sound track declared as a sample entry type no reader knows.
    fn m4a_with_unknown_track() -> Vec<u8> {
        let mut bytes = std::fs::read(fixture("demo.m4a")).unwrap();
        let at = bytes.windows(4).position(|w| w == b"mp4a").unwrap();
        bytes[at..at + 4].copy_from_slice(b"zzzz");
        bytes
    }

    #[test]
    fn audio_units_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_audio.py (iscc-sdk with fpcalc).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_audio.json")).unwrap();
        for (file, want) in expected.as_object().unwrap() {
            let path = fixture(file);
            let bytes = std::fs::read(&path).unwrap();
            let audio = analyse(&bytes, formats::by_path(&path).unwrap()).unwrap();
            assert_eq!(
                audio.seconds.round(),
                want["duration"].as_f64().unwrap(),
                "{file} duration {}",
                audio.seconds
            );
            let unit = iscc::content_unit(Content::Audio(&audio.fingerprint));
            match want["audio"].as_str() {
                None => assert!(unit.is_err(), "{file} is too short"),
                // Opus goes through another decoder than fpcalc's.
                Some(code) if file.contains("opus") => {
                    let similar = iscc::similarity(&unit.unwrap().iscc, code).unwrap();
                    assert!(similar >= Some(OPUS_SIMILARITY), "{file}: {similar:?}");
                }
                Some(code) => assert_eq!(unit.unwrap().iscc, code, "{file} Content-Code Audio"),
            }
            let [data, instance] = iscc::bitstream_units(&bytes).unwrap();
            assert_eq!(data.iscc, want["data"], "{file} Data-Code");
            assert_eq!(instance.iscc, want["instance"], "{file} Instance-Code");
        }
    }

    #[test]
    fn too_short_audio_has_no_content_code() {
        let path = fixture("short.wav");
        let audio = analyse(
            &std::fs::read(&path).unwrap(),
            formats::by_path(&path).unwrap(),
        )
        .unwrap();
        assert!(audio.fingerprint.is_empty());
        assert!((audio.seconds - 1.35).abs() < 0.01, "{}", audio.seconds);
        let error = iscc::content_unit(Content::Audio(&audio.fingerprint)).unwrap_err();
        assert!(error.to_string().contains("too short"), "{error}");
    }

    #[test]
    fn mp4_priming_comes_from_the_edit_list() {
        let bytes = std::fs::read(fixture("demo.m4a")).unwrap();
        assert_eq!(mp4_priming(&bytes, 44100), 1024);
        assert_eq!(mp4_priming(&bytes, 22050), 512);
        assert_eq!(mp4_priming(b"not an mp4 file", 44100), 0);
        let mp3 = std::fs::read(fixture("demo.mp3")).unwrap();
        assert_eq!(mp4_priming(&mp3, 44100), 0);
    }

    /// An ISO BMFF box of `kind` around `body`.
    fn bmff_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let size = u32::try_from(body.len() + 8).unwrap();
        [&size.to_be_bytes()[..], kind, body].concat()
    }

    /// A movie box with one track per media handler type.
    fn movie(handlers: &[&[u8; 4]]) -> Vec<u8> {
        let tracks: Vec<u8> = handlers
            .iter()
            .flat_map(|handler| {
                let hdlr = bmff_box(b"hdlr", &[&[0; 8][..], &handler[..], &[0; 12]].concat());
                bmff_box(b"trak", &bmff_box(b"mdia", &hdlr))
            })
            .collect();
        bmff_box(b"moov", &tracks)
    }

    #[test]
    fn sound_only_means_a_sound_track_and_no_video_track() {
        for file in ["no-video.mp4", "no-video.mov", "demo.m4a"] {
            assert!(
                sound_only(&fixture(file)),
                "{file}: movie box after the media data"
            );
        }
        let videos = [
            "demo.mp4",
            "demo.mov",
            "demo.m4v",
            "rotated.mp4",
            "demo.avi",
        ];
        for file in videos.into_iter().chain(["demo.mp3", "missing.mp4"]) {
            assert!(!sound_only(&fixture(file)), "{file}");
        }

        let dir = tempfile::tempdir().unwrap();
        let ftyp = bmff_box(b"ftyp", b"isom\0\0\0\0isom");
        let mut large_mdat = [&1u32.to_be_bytes()[..], b"mdat", &20u64.to_be_bytes()].concat();
        large_mdat.extend([0; 4]);
        let sound = movie(&[b"soun", b"text"]);
        let cases: [(&str, Vec<u8>, bool); 6] = [
            (
                "movie first",
                [&ftyp[..], &sound, &bmff_box(b"mdat", &[0; 9])].concat(),
                true,
            ),
            (
                "64-bit media data size",
                [&ftyp[..], &large_mdat, &sound].concat(),
                true,
            ),
            (
                "sound and video",
                [&ftyp[..], &movie(&[b"soun", b"vide"])].concat(),
                false,
            ),
            ("no sound", [&ftyp[..], &movie(&[b"text"])].concat(), false),
            (
                "media data to the end",
                [&ftyp[..], &[0, 0, 0, 0], b"mdat", &sound].concat(),
                false,
            ),
            (
                "truncated movie",
                [&ftyp[..], &sound[..sound.len() - 1]].concat(),
                false,
            ),
        ];
        for (case, bytes, want) in cases {
            let path = dir.path().join("case.mp4");
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(sound_only(&path), want, "{case}");
        }
    }

    #[test]
    fn downmix_averages_the_channels() {
        let mut mono = Vec::new();
        downmix(&[0.5, -0.5, 1.0, 0.0], 2, &mut mono);
        assert_eq!(mono, [0.0, 0.5]);
        downmix(&[0.3, 0.6, 0.9], 3, &mut mono);
        assert!((mono[0] - 0.6).abs() < 1e-6);
        downmix(&[0.25, 0.75], 1, &mut mono);
        assert_eq!(mono, [0.25, 0.75]);
    }

    #[test]
    fn unreadable_audio_is_an_error() {
        let format = formats::by_path(Path::new("x.mp3")).unwrap();
        let error = analyse(b"not audio at all", format).unwrap_err();
        assert!(error.downcast_ref::<Undecodable>().is_none(), "{error:#}");
        assert!(read(b"not audio at all", format).is_err());
    }

    #[test]
    fn audio_that_does_not_decode_has_a_reason_in_place_of_its_content() {
        let m4a = formats::by_path(Path::new("x.m4a")).unwrap();
        let mp4 = formats::sound_only(formats::by_path(Path::new("x.mp4")).unwrap()).unwrap();
        // An AC-3 track has no decoder; the other file has no track type the MP4 reader knows.
        let ac3 = std::fs::read(fixture("no-video-ac3.mp4")).unwrap();
        let cases = [
            (ac3, mp4, NO_DECODER),
            (m4a_with_unknown_track(), m4a, NO_TRACK),
        ];
        for (bytes, format, reason) in cases {
            let error = analyse(&bytes, format).unwrap_err();
            assert_eq!(error.downcast_ref::<Undecodable>(), Some(&reason));
            let asset = read(&bytes, format).unwrap();
            assert!(matches!(asset.content, AssetContent::Unavailable(r) if r == reason.0));
            assert!(
                asset.metadata.name.is_some(),
                "the tags are read all the same"
            );
            let error = iscc::content_unit(asset.content()).unwrap_err();
            assert_eq!(error.to_string(), reason.0);
        }
    }

    #[test]
    fn opus_in_an_mp4_decodes_through_the_registered_decoder() {
        let path = fixture("no-video-opus.mp4");
        let bytes = std::fs::read(&path).unwrap();
        let audio = analyse(&bytes, formats::by_path(&path).unwrap()).unwrap();
        assert!(!audio.fingerprint.is_empty());
        assert!((audio.seconds - 8.0).abs() < 0.1, "{}", audio.seconds);
        assert_eq!(mp4_priming(&bytes, 48000), 312, "the pre-skip, as an edit");
    }
}
