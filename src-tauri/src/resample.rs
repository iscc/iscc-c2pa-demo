//! Streaming resampler from any rate to 11025 Hz mono s16, with the settings fpcalc gives
//! FFmpeg's swresample: a Kaiser-windowed sinc filter (16 taps at full bandwidth, cutoff 0.8,
//! beta 9) with at most 256 phases, interpolating linearly between phases when the rate ratio
//! does not fall on one. Written from that behaviour's specification, so
//! Chromaprint sees the same samples as in fpcalc and the fingerprints agree bit for bit.

/// Rate Chromaprint's TEST2 algorithm works at.
pub const OUTPUT_RATE: u32 = 11025;
/// Taps of the filter at full bandwidth; downsampling widens it by the inverse cutoff factor.
const FILTER_SIZE: f64 = 16.0;
/// Most filter phases per input sample.
const MAX_PHASES: u64 = 256;
/// Filter cutoff relative to the output Nyquist frequency.
const CUTOFF: f64 = 0.8;
/// Kaiser window shape.
const KAISER_BETA: f64 = 9.0;
/// Step fractions are scaled up until one of their terms reaches this.
const STEP_SCALE_LIMIT: u64 = 1 << 20;

/// Resampler state: the filter bank, the step between outputs and the pending input window.
#[derive(Debug)]
pub struct Resampler {
    /// Input rate equals the output rate: samples are only converted.
    passthrough: bool,
    /// `phases + 1` rows of `taps` coefficients; the last row is row 0 shifted by one tap.
    bank: Vec<f32>,
    taps: usize,
    phases: usize,
    /// Step per output: `div` phases plus `rem / src` of a phase.
    src: u64,
    div: usize,
    rem: u64,
    /// Input window: mirrored start, then the input not yet passed by `pos`.
    window: Vec<f32>,
    started: bool,
    /// First input sample under the filter, its phase and the fractional phase.
    pos: usize,
    phase: usize,
    frac: u64,
}

impl Resampler {
    /// A resampler from `in_rate` to [`OUTPUT_RATE`].
    pub fn new(in_rate: u32) -> Self {
        let (r, out) = (u64::from(in_rate.max(1)), u64::from(OUTPUT_RATE));
        let factor = (out as f64 * CUTOFF / r as f64).min(1.0);
        let mut taps = ((FILTER_SIZE / factor).ceil() as usize).max(1);
        if taps > 1 {
            taps += taps % 2;
        }
        let phases = (out / gcd(r, out)).min(MAX_PHASES);
        let (mut a, mut b) = reduce(out, r * phases);
        while a < STEP_SCALE_LIMIT && b < STEP_SCALE_LIMIT {
            a *= 2;
            b *= 2;
        }
        Self {
            passthrough: r == out,
            bank: filter_bank(factor, taps, phases as usize),
            taps,
            phases: phases as usize,
            src: a,
            div: (b / a) as usize,
            rem: b % a,
            window: Vec::new(),
            started: false,
            pos: 0,
            phase: 0,
            frac: 0,
        }
    }

    /// Resample the next mono samples, appending what can be computed so far to `out`.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<i16>) {
        if self.passthrough {
            out.extend(mono.iter().map(|&y| to_s16(y)));
            return;
        }
        self.window.extend_from_slice(mono);
        if !self.started {
            if self.window.len() <= self.taps {
                return;
            }
            self.start();
        }
        self.drain(out);
    }

    /// Flush the end of the stream, mirrored like its start.
    pub fn finish(&mut self, out: &mut Vec<i16>) {
        if self.passthrough || !self.started {
            return;
        }
        let n = self.window.len();
        let tail = n.saturating_sub(self.pos).min(self.taps).div_ceil(2);
        for j in 0..tail.min(n) {
            self.window.push(self.window[n - 1 - j]);
        }
        self.drain(out);
    }

    /// Put the first `taps` samples mirrored in front, so the first output is centred on the
    /// first input sample.
    fn start(&mut self) {
        let mirrored = self.window[1..=self.taps].iter().rev().copied();
        self.window = mirrored.chain(self.window.iter().copied()).collect();
        self.pos = self.taps - (self.taps - 1) / 2;
        self.started = true;
    }

    /// Emit every output whose filter fits into the window, then drop the input passed.
    fn drain(&mut self, out: &mut Vec<i16>) {
        while self.pos + self.taps <= self.window.len() {
            out.push(to_s16(self.sample()));
            self.frac += self.rem;
            self.phase += self.div;
            if self.frac >= self.src {
                self.frac -= self.src;
                self.phase += 1;
            }
            self.pos += self.phase / self.phases;
            self.phase %= self.phases;
        }
        self.window.drain(..self.pos);
        self.pos = 0;
    }

    /// The output at the current position and phase.
    fn sample(&self) -> f32 {
        let input = &self.window[self.pos..self.pos + self.taps];
        let y = dot(input, self.row(self.phase));
        if self.rem == 0 {
            return y;
        }
        let y2 = dot(input, self.row(self.phase + 1));
        let t = f64::from(y2 - y) * (1.0 / self.src as f64) * self.frac as f64;
        (f64::from(y) + t) as f32
    }

    /// Coefficients of filter phase `p`.
    fn row(&self, p: usize) -> &[f32] {
        &self.bank[p * self.taps..(p + 1) * self.taps]
    }
}

/// Sum of the products of two equally long slices, in f32.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// A sample in [-1, 1] as s16, rounded half to even and clipped.
fn to_s16(y: f32) -> i16 {
    (y * 32768.0).round_ties_even().clamp(-32768.0, 32767.0) as i16
}

/// Greatest common divisor.
fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// The fraction `a / b` in lowest terms.
fn reduce(a: u64, b: u64) -> (u64, u64) {
    let g = gcd(a, b);
    (a / g, b / g)
}

/// `phases + 1` rows of `taps` Kaiser-windowed sinc coefficients, normalised by the sum of
/// row 0. Row `phases` is row 0 shifted right by one tap.
fn filter_bank(factor: f64, taps: usize, phases: usize) -> Vec<f32> {
    let center = (taps as i64 - 1) / 2;
    let mut rows: Vec<f64> = Vec::with_capacity(taps * phases);
    for p in 0..phases {
        for i in 0..taps {
            let offset = (i as i64 - center) as f64 - p as f64 / phases as f64;
            rows.push(kaiser_sinc(
                std::f64::consts::PI * offset * factor,
                factor,
                taps,
            ));
        }
    }
    let norm: f64 = rows[..taps].iter().sum();
    let mut bank: Vec<f32> = rows.iter().map(|c| (c / norm) as f32).collect();
    bank.push(0.0);
    bank.extend_from_within(..taps - 1);
    bank
}

/// Windowed sinc at `x`, the Kaiser window spanning the filter's `taps`.
fn kaiser_sinc(x: f64, factor: f64, taps: usize) -> f64 {
    let sinc = if x == 0.0 { 1.0 } else { x.sin() / x };
    let w = 2.0 * x / (factor * taps as f64 * std::f64::consts::PI);
    sinc * bessel_i0(KAISER_BETA * (1.0 - w * w).max(0.0).sqrt())
}

/// Modified Bessel function of the first kind, order 0, by its power series.
fn bessel_i0(x: f64) -> f64 {
    let q = x * x / 4.0;
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..500 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resample all of `input` at once.
    fn run(rate: u32, input: &[f32]) -> Vec<i16> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        r.push(input, &mut out);
        r.finish(&mut out);
        out
    }

    /// `seconds` of a sine of `freq` Hz and amplitude `amp` at `rate`.
    fn sine(rate: u32, freq: f64, amp: f64, seconds: f64) -> Vec<f32> {
        let n = (f64::from(rate) * seconds) as usize;
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                (amp * (2.0 * std::f64::consts::PI * freq * t).sin()) as f32
            })
            .collect()
    }

    #[test]
    fn output_rate_passes_through_exactly() {
        let input = [
            0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            1.5,
            0.25 / 32768.0,
            0.75 / 32768.0,
        ];
        assert_eq!(
            run(OUTPUT_RATE, &input),
            [0, 16384, -16384, 32767, -32768, 32767, 0, 1]
        );
    }

    #[test]
    fn filter_settings_follow_the_rates() {
        // 44.1 kHz: a quarter of the rate, one phase, no interpolation.
        let r = Resampler::new(44100);
        assert_eq!((r.taps, r.phases, r.rem), (80, 1, 0));
        assert_eq!(r.div, 4);
        // 32 kHz: 441 phases would be needed, so 256 with interpolation.
        let r = Resampler::new(32000);
        assert_eq!((r.taps, r.phases), (60, 256));
        assert_ne!(r.rem, 0);
        // 48 kHz: 147 phases, no interpolation.
        let r = Resampler::new(48000);
        assert_eq!((r.phases, r.rem), (147, 0));
        // Row 0 sums to one; the extra row is row 0 shifted by one tap.
        let r = Resampler::new(22050);
        let sum: f32 = r.row(0).iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "{sum}");
        assert_eq!(r.row(1)[0], 0.0);
        assert_eq!(r.row(1)[1..], r.row(0)[..r.taps - 1]);
    }

    #[test]
    fn constant_stays_constant_after_the_filter_settles() {
        for rate in [8000, 16000, 22050, 32000, 44100, 48000, 96000] {
            let taps = Resampler::new(rate).taps;
            let out = run(rate, &vec![0.25; rate as usize]);
            for (i, &s) in out.iter().enumerate().skip(taps) {
                assert!(
                    (i32::from(s) - 8192).abs() <= 1,
                    "{rate} Hz, output {i}: {s}"
                );
            }
        }
    }

    #[test]
    fn output_length_follows_the_rate_ratio() {
        for rate in [8000, 16000, 22050, 32000, 44100, 48000, 96000] {
            let n = rate as usize * 3;
            let out = run(rate, &vec![0.0; n]);
            let want = n as f64 * f64::from(OUTPUT_RATE) / f64::from(rate);
            assert!(
                (out.len() as f64 - want).abs() <= 2.0,
                "{rate} Hz: {}",
                out.len()
            );
        }
    }

    #[test]
    fn sine_keeps_its_amplitude() {
        for rate in [16000, 32000, 44100, 48000] {
            let out = run(rate, &sine(rate, 1000.0, 0.5, 1.0));
            let peak = out[200..out.len() - 200]
                .iter()
                .map(|s| i32::from(*s).abs())
                .max()
                .unwrap();
            let want = 0.5 * 32768.0;
            assert!(
                (f64::from(peak) - want).abs() < want * 0.01,
                "{rate} Hz peak {peak}"
            );
        }
    }

    #[test]
    fn streaming_in_chunks_equals_one_push() {
        let input = sine(32000, 440.0, 0.8, 0.5);
        let mut r = Resampler::new(32000);
        let mut chunked = Vec::new();
        for chunk in input.chunks(37) {
            r.push(chunk, &mut chunked);
        }
        r.finish(&mut chunked);
        assert_eq!(chunked, run(32000, &input));
    }

    #[test]
    fn input_shorter_than_the_filter_gives_nothing() {
        assert!(run(44100, &[0.5; 40]).is_empty());
    }
}
