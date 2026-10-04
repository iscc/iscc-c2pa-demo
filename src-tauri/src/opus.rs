//! Opus for the audio reader. symphonia reads Opus tracks out of MP4 containers but has no
//! decoder for them, so this module supplies one for its codec registry (`audio::CODECS`),
//! backed by the pure-Rust `rusty-opus`. A further codec joins the same way: a decoder type
//! in a module of its own, registered there.
//!
//! Mono and stereo streams decode at 48 kHz. Known deviations from FFmpeg's libopus, as fpcalc
//! uses it, all small against what a Chromaprint fingerprint tolerates: the decoded samples
//! differ by rounding, the header's output gain is not applied (the fingerprint compares band
//! energies, not loudness), and the pre-skip is left to the caller (an MP4 carries it in the
//! edit list, see `audio::mp4_priming`).

use symphonia::core::audio::{
    AsGenericAudioBufferRef, Audio, AudioBuffer, AudioMut, AudioSpec, GenericAudioBufferRef,
};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, FinalizeResult,
};
use symphonia::core::codecs::registry::{RegisterableAudioDecoder, SupportedAudioCodec};
use symphonia::core::codecs::CodecInfo;
use symphonia::core::errors::{decode_error, unsupported_error, Result};
use symphonia::core::packet::PacketRef;

/// Rate every Opus stream decodes at.
const RATE: u32 = 48_000;
/// Samples per channel of the longest packet, 120 ms (RFC 6716, section 3.2.5).
const MAX_PACKET_SAMPLES: usize = 5760;

const OPUS: SupportedAudioCodec = SupportedAudioCodec {
    id: CODEC_ID_OPUS,
    info: CodecInfo {
        short_name: "opus",
        long_name: "Opus",
        profiles: &[],
    },
};

/// Opus decoder for symphonia's codec registry.
pub struct OpusDecoder {
    params: AudioCodecParameters,
    decoder: rusty_opus::OpusDecoder,
    /// Interleaved samples of the packet decoded last.
    pcm: Vec<f32>,
    buf: AudioBuffer<f32>,
}

impl OpusDecoder {
    /// A decoder for the mono or stereo Opus stream that `params` describe.
    fn try_new(params: &AudioCodecParameters) -> Result<Self> {
        let Some(channels) = params.channels.clone() else {
            return unsupported_error("opus: the channels are required");
        };
        let Ok(decoder) = rusty_opus::OpusDecoder::new(RATE as i32, channels.count()) else {
            return unsupported_error("opus: only mono and stereo streams");
        };
        Ok(Self {
            params: params.clone(),
            decoder,
            pcm: Vec::new(),
            buf: AudioBuffer::new(AudioSpec::new(RATE, channels), MAX_PACKET_SAMPLES),
        })
    }

    /// Decode the Opus packet in `data` into the buffer.
    fn decode_packet(&mut self, data: &[u8]) -> Result<()> {
        let Some(samples) = packet_samples(data) else {
            return decode_error("opus: malformed packet");
        };
        let channels = self.buf.spec().channels().count();
        self.pcm.resize(samples * channels, 0.0);
        let Ok(decoded) = self.decoder.decode(data, samples, &mut self.pcm) else {
            return decode_error("opus: packet does not decode");
        };
        let frames = decoded.min(samples);
        let pcm = &self.pcm[..frames * channels];
        self.buf.render_uninit(Some(frames));
        self.buf.copy_from_slice_interleaved(&pcm);
        Ok(())
    }
}

impl AudioDecoder for OpusDecoder {
    fn reset(&mut self) {
        // Seeking is not used: the audio reader decodes a track once, start to end.
    }

    fn codec_info(&self) -> &CodecInfo {
        &OPUS.info
    }

    fn codec_params(&self) -> &AudioCodecParameters {
        &self.params
    }

    fn decode_ref(&mut self, packet: &PacketRef<'_>) -> Result<GenericAudioBufferRef<'_>> {
        self.buf.clear();
        self.decode_packet(packet.data)?;
        Ok(self.buf.as_generic_audio_buffer_ref())
    }

    fn finalize(&mut self) -> FinalizeResult {
        FinalizeResult::default()
    }

    fn last_decoded(&self) -> GenericAudioBufferRef<'_> {
        self.buf.as_generic_audio_buffer_ref()
    }
}

impl RegisterableAudioDecoder for OpusDecoder {
    fn try_registry_new(
        params: &AudioCodecParameters,
        _opts: &AudioDecoderOptions,
    ) -> Result<Box<dyn AudioDecoder>> {
        Ok(Box::new(Self::try_new(params)?))
    }

    fn supported_codecs() -> &'static [SupportedAudioCodec] {
        &[OPUS]
    }
}

/// Samples per channel at 48 kHz that an Opus packet decodes to, from the configuration in its
/// first byte and its frame count (RFC 6716, section 3.1); `None` for an empty packet or one
/// that claims no frame or more than 120 ms.
fn packet_samples(packet: &[u8]) -> Option<usize> {
    let toc = *packet.first()?;
    let config = usize::from(toc >> 3);
    let frame = match config {
        // SILK: 10, 20, 40 or 60 ms.
        0..=11 => [480, 960, 1920, 2880][config & 3],
        // Hybrid: 10 or 20 ms.
        12..=15 => [480, 960][config & 1],
        // CELT: 2.5, 5, 10 or 20 ms.
        _ => 120 << (config & 3),
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => usize::from(*packet.get(1)? & 0x3f),
    };
    Some(frame * frames).filter(|samples| (1..=MAX_PACKET_SAMPLES).contains(samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_samples_follow_the_first_byte_and_frame_count() {
        // Configuration in the upper five bits, frame packing in the lower two.
        let toc = |config: u8, code: u8| (config << 3) | code;
        assert_eq!(packet_samples(&[toc(1, 0)]), Some(960), "SILK, 20 ms");
        assert_eq!(packet_samples(&[toc(3, 0)]), Some(2880), "SILK, 60 ms");
        assert_eq!(packet_samples(&[toc(12, 0)]), Some(480), "hybrid, 10 ms");
        assert_eq!(packet_samples(&[toc(16, 0)]), Some(120), "CELT, 2.5 ms");
        assert_eq!(packet_samples(&[toc(31, 0)]), Some(960), "CELT, 20 ms");
        assert_eq!(packet_samples(&[toc(31, 1)]), Some(1920), "two frames");
        assert_eq!(packet_samples(&[toc(31, 2)]), Some(1920), "two frames");
        assert_eq!(packet_samples(&[toc(31, 3), 6]), Some(5760), "six frames");
        assert_eq!(packet_samples(&[toc(31, 3), 0x80 | 3]), Some(2880));
        assert_eq!(packet_samples(&[toc(31, 3), 7]), None, "over 120 ms");
        assert_eq!(packet_samples(&[toc(31, 3), 0]), None, "no frame");
        assert_eq!(packet_samples(&[toc(31, 3)]), None, "no frame count");
        assert_eq!(packet_samples(&[]), None);
    }
}
