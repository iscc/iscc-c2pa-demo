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
//! Known deviations from fpcalc: more than two channels are averaged (swresample uses a downmix
//! matrix; 0 to 2 bits per 64 on 5.1), and packets that fail to decode are skipped.

use std::io::Cursor;

use anyhow::{anyhow, Context as _, Result};
use rusty_chromaprint::{Configuration, Fingerprinter};
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::packet::Packet;

use crate::asset::{Asset, AssetContent};
use crate::formats::{self, Format};
use crate::resample::{Resampler, OUTPUT_RATE};
use crate::{audio_tags, iscc};

/// The decoded audio as far as the ISCC needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    /// Chromaprint TEST2 fingerprint, as fpcalc prints it with `-signed`; empty when the audio
    /// is too short.
    pub fingerprint: Vec<i32>,
    /// Duration of the decoded audio after trimming.
    pub seconds: f64,
}

/// Read the audio asset held in `bytes`: fingerprint, tags and cover art.
pub fn read(bytes: &[u8], format: &Format) -> Result<Asset> {
    let audio = analyse(bytes, format)?;
    let (metadata, cover) = audio_tags::read(bytes, format.mime);
    Ok(Asset {
        content: AssetContent::Audio(audio),
        preview: cover.and_then(|c| iscc::decode_rgb(&c).ok()),
        metadata,
    })
}

/// Decode `bytes` and fingerprint the audio of its default track.
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
    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| anyhow!("the file has no audio track"))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| anyhow!("the audio track has no codec parameters"))?
        .clone();
    let skip = match format.mime {
        formats::M4A => mp4_priming(bytes, params.sample_rate.unwrap_or(0)),
        _ => 0,
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
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .context("this audio codec is not supported")?;
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
    if child(mdia, b"hdlr")?.get(8..12)? != b"soun" {
        return None;
    }
    let timescale = mdhd_timescale(child(mdia, b"mdhd")?).filter(|&t| t > 0)?;
    let media_time = child(trak, b"edts")
        .and_then(|edts| child(edts, b"elst"))
        .and_then(first_media_time)
        .unwrap_or(0);
    Some((media_time, timescale))
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
    use crate::iscc::{self, Content, UnitSelection};
    use std::path::Path;

    fn fixture(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn audio_units_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_audio.py (iscc-sdk with fpcalc).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_audio.json")).unwrap();
        let selection = UnitSelection {
            meta: false,
            content: false,
            data: true,
            instance: true,
        };
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
                Some(code) => assert_eq!(unit.unwrap().iscc, code, "{file} Content-Code Audio"),
            }
            let units = iscc::units_for(
                &bytes,
                Content::Audio(&audio.fingerprint),
                Default::default(),
                &selection,
            )
            .unwrap();
            assert_eq!(units[0].iscc, want["data"], "{file} Data-Code");
            assert_eq!(units[1].iscc, want["instance"], "{file} Instance-Code");
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
        assert!(analyse(b"not audio at all", format).is_err());
    }
}
