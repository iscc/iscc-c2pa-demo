//! ISCC units for image, text, audio and video assets and the ISCC-SEQ soft-binding value
//! defined by IEP-0020. The Semantic-Codes come from `semantic`; this module only places them.
//!
//! Image preprocessing mirrors `iscc_sdk.image_normalize` (EXIF transpose, white fill for
//! transparency, tolerant border trim, ITU-R 601-2 luma, bicubic 32x32 resize) so that codes
//! agree with the Python reference implementation. JPEGs decode with libjpeg-turbo-rs, a port of
//! the libjpeg-turbo that Pillow uses, to the same pixels. The same Pillow resize, with the bilinear
//! filter, prepares the Semantic-Code Image.

use std::io::{Cursor, Read};

use anyhow::{anyhow, bail, Result};
use image::{DynamicImage, GrayImage, RgbImage, RgbaImage};
use iscc_lib::codec::{self, MainType};
use serde::Serialize;

/// Bit length of every ISCC-UNIT produced and accepted by this app (IEP-0020 requires 256).
pub const UNIT_BITS: u32 = 256;

/// One ISCC-UNIT with its human-readable classification.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct IsccUnit {
    /// Colour/family slug: meta, semantic, content, data or instance.
    pub unit: &'static str,
    /// Display name such as "Content-Code Image".
    pub name: &'static str,
    /// Canonical unit string with `ISCC:` prefix.
    pub iscc: String,
}

/// Which units to generate for an asset.
#[derive(Clone, Debug, Default)]
pub struct UnitSelection {
    pub meta: bool,
    /// Semantic-Code of an image or a text document.
    pub semantic: bool,
    /// Content-Code of the asset's media type (image, text, audio or video).
    pub content: bool,
    pub data: bool,
    pub instance: bool,
}

impl UnitSelection {
    /// Build a selection from slugs such as `["image", "data"]`; `image`, `text`, `audio` and
    /// `video` all select the Content-Code.
    pub fn from_slugs(slugs: &[String]) -> Self {
        let has = |s: &str| slugs.iter().any(|x| x == s);
        Self {
            meta: has("meta"),
            semantic: has("semantic"),
            content: has("image") || has("text") || has("audio") || has("video"),
            data: has("data"),
            instance: has("instance"),
        }
    }
}

/// Media content of an asset, the input of its Content-Code.
#[derive(Clone, Copy, Debug)]
pub enum Content<'a> {
    /// Decoded image, EXIF-transposed and flattened onto white (see [`decode_rgb`]).
    Image(&'a RgbImage),
    /// Plain text extracted from the asset, cleaned like `iscc_sdk.text_extract`.
    Text(&'a str),
    /// Chromaprint fingerprint of the decoded audio, as fpcalc prints it with `-signed`.
    Audio(&'a [i32]),
    /// Distinct MPEG-7 frame signatures of the video, 380 values in 0..=2 each.
    Video(&'a [Vec<i32>]),
    /// No input for a Content-Code, with the reason.
    Unavailable(&'a str),
}

/// Classify a unit string and normalize it.
pub fn describe_unit(iscc: &str) -> Result<IsccUnit> {
    let (mt, st, _vs, _len, _body) = iscc_lib::iscc_decode(iscc)?;
    let (unit, name) = match (mt, st) {
        (0, _) => ("meta", "Meta-Code"),
        (1, 0) => ("semantic", "Semantic-Code Text"),
        (1, 1) => ("semantic", "Semantic-Code Image"),
        (1, _) => ("semantic", "Semantic-Code"),
        (2, 0) => ("content", "Content-Code Text"),
        (2, 1) => ("content", "Content-Code Image"),
        (2, 2) => ("content", "Content-Code Audio"),
        (2, 3) => ("content", "Content-Code Video"),
        (2, 4) => ("content", "Content-Code Mixed"),
        (2, _) => ("content", "Content-Code"),
        (3, _) => ("data", "Data-Code"),
        (4, _) => ("instance", "Instance-Code"),
        _ => bail!("{iscc} is a composite code or ID, not an ISCC-UNIT"),
    };
    Ok(IsccUnit {
        unit,
        name,
        iscc: format!("ISCC:{}", clean(iscc)),
    })
}

/// Strip prefix, dashes and whitespace and upper-case the base32 payload.
fn clean(iscc: &str) -> String {
    let s = iscc.trim();
    let s = s
        .strip_prefix("ISCC:")
        .or_else(|| s.strip_prefix("iscc:"))
        .unwrap_or(s);
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase()
}

/// Inputs of the Meta-Code.
#[derive(Clone, Copy, Debug, Default)]
pub struct MetaInput<'a> {
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    /// ISCC metadata (`iscc:meta`, a data URL); replaces the description in the Meta-Code.
    pub meta: Option<&'a str>,
}

/// The Meta-Code of a title, an optional description and optional ISCC metadata.
pub fn meta_unit(meta: MetaInput<'_>) -> Result<IsccUnit> {
    let name = meta
        .name
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| anyhow!("a title is required for the Meta-Code"))?;
    let description = meta.description.map(str::trim).filter(|d| !d.is_empty());
    let code = iscc_lib::gen_meta_code_v0(name, description, meta.meta, UNIT_BITS)?;
    describe_unit(&code.iscc)
}

/// The selected units of an asset, in the order of their MainType: the Meta-Code over `meta`,
/// the Semantic-Code computed by the caller (`semantic`, which keeps the models out of this
/// module), the Content-Code over `content`, and the Data-Code and Instance-Code from
/// `bitstream`, those of the asset's bytes.
pub fn units_for(
    bitstream: &[IsccUnit; 2],
    content: Content<'_>,
    meta: MetaInput<'_>,
    semantic: Option<IsccUnit>,
    selection: &UnitSelection,
) -> Result<Vec<IsccUnit>> {
    let mut units = Vec::new();
    if selection.meta {
        units.push(meta_unit(meta)?);
    }
    if selection.semantic {
        units.push(semantic.ok_or_else(|| anyhow!("the Semantic-Code is not available"))?);
    }
    if selection.content {
        units.push(content_unit(content)?);
    }
    if selection.data {
        units.push(bitstream[0].clone());
    }
    if selection.instance {
        units.push(bitstream[1].clone());
    }
    Ok(units)
}

/// Data-Code and Instance-Code of `bytes`.
pub fn bitstream_units(bytes: &[u8]) -> Result<[IsccUnit; 2]> {
    Ok([data_unit(bytes)?, instance_unit(bytes)?])
}

/// Read size when hashing a stream.
const STREAM_CHUNK: usize = 1 << 20;

/// Data-Code and Instance-Code of everything `reader` yields, read in 1 MiB chunks so a large
/// file never sits in memory, and the number of bytes read.
pub fn stream_units(mut reader: impl Read) -> Result<([IsccUnit; 2], u64)> {
    let mut data = iscc_lib::DataHasher::new();
    let mut instance = iscc_lib::InstanceHasher::new();
    let mut buf = vec![0u8; STREAM_CHUNK];
    let mut size = 0u64;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        data.update(&buf[..n]);
        instance.update(&buf[..n]);
        size += n as u64;
    }
    let units = [
        describe_unit(&data.finalize(UNIT_BITS)?.iscc)?,
        describe_unit(&instance.finalize(UNIT_BITS)?.iscc)?,
    ];
    Ok((units, size))
}

/// The Data-Code of `bytes`.
pub fn data_unit(bytes: &[u8]) -> Result<IsccUnit> {
    describe_unit(&iscc_lib::gen_data_code_v0(bytes, UNIT_BITS)?.iscc)
}

/// The Instance-Code of `bytes`.
pub fn instance_unit(bytes: &[u8]) -> Result<IsccUnit> {
    describe_unit(&iscc_lib::gen_instance_code_v0(bytes, UNIT_BITS)?.iscc)
}

/// The Content-Code of `content`. An empty fingerprint means the audio was too short for
/// Chromaprint, where fpcalc fails with "Empty fingerprint".
pub fn content_unit(content: Content<'_>) -> Result<IsccUnit> {
    let code = match content {
        Content::Image(rgb) => iscc_lib::gen_image_code_v0(&image_pixels(rgb), UNIT_BITS)?.iscc,
        Content::Text(text) => iscc_lib::gen_text_code_v0(text, UNIT_BITS)?.iscc,
        Content::Audio([]) => {
            bail!("audio too short for a Content-Code Audio (Chromaprint needs about 3 seconds)")
        }
        Content::Audio(fingerprint) => iscc_lib::gen_audio_code_v0(fingerprint, UNIT_BITS)?.iscc,
        Content::Video([]) => bail!("no video frames found; the file may have no video stream"),
        Content::Video(signatures) => iscc_lib::gen_video_code_v0(signatures, UNIT_BITS)?.iscc,
        Content::Unavailable(reason) => bail!("{reason}"),
    };
    describe_unit(&code)
}

/// Reduce a decoded image to the 1024 grayscale samples expected by `gen_image_code_v0`.
pub fn image_pixels(rgb: &RgbImage) -> Vec<u8> {
    let gray = to_luma(&trim_border(rgb));
    resize_pillow(
        gray.as_raw(),
        1,
        gray.dimensions(),
        (32, 32),
        Filter::Bicubic,
    )
}

/// Fixed-point precision of Pillow's 8-bit resampling coefficients (32 - 8 - 2).
const PRECISION_BITS: u32 = 22;

/// Pillow's resampling filters, as far as this app resizes with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Filter {
    /// Triangle filter, support 1 (the Semantic-Code Image).
    Bilinear,
    /// Cubic convolution with a = -0.5, support 2 (the Content-Code Image).
    Bicubic,
}

impl Filter {
    /// Half-width of the kernel at scale 1.
    fn support(self) -> f64 {
        match self {
            Filter::Bilinear => 1.0,
            Filter::Bicubic => 2.0,
        }
    }

    /// Weight of a sample at distance `x`.
    fn kernel(self, x: f64) -> f64 {
        match self {
            Filter::Bilinear => (1.0 - x.abs()).max(0.0),
            Filter::Bicubic => bicubic_kernel(x),
        }
    }
}

/// Pillow's `Image.resize(size, filter)` for 8-bit images, bit for bit: `data` holds rows of
/// `channels` interleaved samples per pixel at `in_size`; a horizontal pass, then a vertical
/// pass, each rounding to 8 bit, using 22-bit fixed-point coefficients. A pass whose size
/// already matches is skipped, as Pillow does. Returns the samples at `out_size`, row-major.
pub(crate) fn resize_pillow(
    data: &[u8],
    channels: usize,
    (in_w, in_h): (u32, u32),
    (out_w, out_h): (u32, u32),
    filter: Filter,
) -> Vec<u8> {
    let mut data = data.to_vec();
    let mut row_len = in_w as usize * channels;
    if in_w != out_w {
        let coeffs = resample_coeffs(filter, in_w, out_w);
        let out_len = out_w as usize * channels;
        let mut out = vec![0u8; out_len * in_h as usize];
        for y in 0..in_h as usize {
            let row = &data[y * row_len..(y + 1) * row_len];
            for (x, (first, k)) in coeffs.iter().enumerate() {
                for c in 0..channels {
                    let samples = (0..k.len()).map(|i| row[(first + i) * channels + c]);
                    out[y * out_len + x * channels + c] = weighted_sample(samples, k);
                }
            }
        }
        data = out;
        row_len = out_len;
    }
    if in_h != out_h {
        let coeffs = resample_coeffs(filter, in_h, out_h);
        let mut out = vec![0u8; row_len * out_h as usize];
        for (y, (first, k)) in coeffs.iter().enumerate() {
            for x in 0..row_len {
                let column = (0..k.len()).map(|i| data[(first + i) * row_len + x]);
                out[y * row_len + x] = weighted_sample(column, k);
            }
        }
        data = out;
    }
    data
}

/// Pillow's bicubic convolution kernel (a = -0.5), support 2.
fn bicubic_kernel(x: f64) -> f64 {
    const A: f64 = -0.5;
    let x = x.abs();
    if x < 1.0 {
        ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
    } else if x < 2.0 {
        (((x - 5.0) * x + 8.0) * x - 4.0) * A
    } else {
        0.0
    }
}

/// Per output sample along one axis: first input index and the normalised fixed-point weights
/// of `filter`, mirroring Pillow's `precompute_coeffs` and `normalize_coeffs_8bpc`.
fn resample_coeffs(filter: Filter, in_size: u32, out_size: u32) -> Vec<(usize, Vec<i32>)> {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = filter.support() * filterscale;
    let ss = 1.0 / filterscale;
    (0..out_size)
        .map(|xx| {
            let center = (xx as f64 + 0.5) * scale;
            let first = ((center - support + 0.5) as i64).max(0) as usize;
            let last = ((center + support + 0.5) as i64).min(in_size as i64) as usize;
            let weights: Vec<f64> = (first..last)
                .map(|x| filter.kernel((x as f64 - center + 0.5) * ss))
                .collect();
            let total: f64 = weights.iter().sum();
            let fixed = weights
                .iter()
                .map(|w| if total != 0.0 { w / total } else { *w })
                .map(|w| {
                    let scaled = w * (1u32 << PRECISION_BITS) as f64;
                    (if w < 0.0 { scaled - 0.5 } else { scaled + 0.5 }) as i32
                })
                .collect();
            (first, fixed)
        })
        .collect()
}

/// Fixed-point weighted sum with Pillow's rounding and clipping to 8 bit.
fn weighted_sample(samples: impl Iterator<Item = u8>, weights: &[i32]) -> u8 {
    let mut acc: i64 = 1 << (PRECISION_BITS - 1);
    for (s, w) in samples.zip(weights) {
        acc += s as i64 * *w as i64;
    }
    if acc >= (255i64 << PRECISION_BITS) {
        255
    } else if acc <= 0 {
        0
    } else {
        (acc >> PRECISION_BITS) as u8
    }
}

/// Decode an image, honour EXIF orientation and flatten transparency onto white.
pub fn decode_rgb(bytes: &[u8]) -> Result<RgbImage> {
    let img = match decode_jpeg(bytes) {
        Some(rgb) => DynamicImage::ImageRgb8(rgb),
        None => image::load_from_memory(bytes)?,
    };
    let img = exif_transpose(img, bytes);
    Ok(fill_transparency(&img))
}

/// Memory a JPEG may take to decode: the `image` crate's default allocation limit.
const JPEG_MAX_MEMORY: u64 = 512 * 1024 * 1024;

/// Decode a JPEG to the pixels Pillow's libjpeg-turbo gives. None for any other format, for a
/// JPEG the decoder refuses, such as one with stray bytes between segments (libjpeg-turbo skips
/// them with a warning), and for a header it would misread (`reads_like_libjpeg`); the `image`
/// crate then decodes it, a few pixels one shade off.
fn decode_jpeg(bytes: &[u8]) -> Option<RgbImage> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let limits = libjpeg_turbo_rs::DecodeLimits {
        max_memory: Some(JPEG_MAX_MEMORY),
        ..Default::default()
    };
    let mut decoder = libjpeg_turbo_rs::Decoder::new_with_limits(bytes, limits).ok()?;
    if !reads_like_libjpeg(&decoder) {
        return None;
    }
    decoder.set_output_format(libjpeg_turbo_rs::PixelFormat::Rgb);
    let img = decoder.decode_image().ok()?;
    RgbImage::from_raw(
        img.width.try_into().ok()?,
        img.height.try_into().ok()?,
        img.data,
    )
}

/// Whether libjpeg-turbo-rs 0.8 decodes this header as libjpeg-turbo does. Not for CMYK or YCCK
/// whose components differ in sampling: it panics on those, and the release build aborts on a
/// panic. Not for components named R, G, B without a JFIF or Adobe marker: libjpeg-turbo reads
/// them as RGB, the port as YCbCr.
fn reads_like_libjpeg(decoder: &libjpeg_turbo_rs::Decoder) -> bool {
    let components = &decoder.header().components;
    let sampling =
        |c: &libjpeg_turbo_rs::ComponentInfo| (c.horizontal_sampling, c.vertical_sampling);
    match components.len() {
        4 => components
            .iter()
            .all(|c| sampling(c) == sampling(&components[0])),
        3 => {
            let named_rgb = components.iter().map(|c| c.id).eq(*b"RGB");
            !named_rgb
                || decoder.saw_jfif_marker()
                || decoder.jpeg_color_space() == libjpeg_turbo_rs::ColorSpace::Rgb
        }
        _ => true,
    }
}

/// Apply the EXIF Orientation tag the way `PIL.ImageOps.exif_transpose` does.
fn exif_transpose(img: DynamicImage, bytes: &[u8]) -> DynamicImage {
    let orientation = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()
        .and_then(|e| {
            e.get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| f.value.get_uint(0))
        });
    match orientation {
        Some(2) => img.fliph(),
        Some(3) => img.rotate180(),
        Some(4) => img.flipv(),
        Some(5) => img.rotate90().fliph(),
        Some(6) => img.rotate90(),
        Some(7) => img.rotate90().flipv(),
        Some(8) => img.rotate270(),
        _ => img,
    }
}

/// Composite alpha over white using Pillow's rounded division, or plain RGB conversion.
fn fill_transparency(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }
    flatten_on_white(&img.to_rgba8())
}

/// Composite straight (not premultiplied) RGBA over white with Pillow's rounded division.
pub fn flatten_on_white(rgba: &RgbaImage) -> RgbImage {
    let (w, h) = rgba.dimensions();
    let mut out = RgbImage::new(w, h);
    for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
        let a = src[3] as u32;
        for c in 0..3 {
            let v = 255 * (255 - a) + src[c] as u32 * a;
            let tmp = v + 128;
            dst[c] = (((tmp >> 8) + tmp) >> 8) as u8;
        }
    }
    out
}

/// Largest per-channel difference of a border pixel to the border colour (iscc-sdk's and
/// iscc-sci's `tolerance`).
const TRIM_TOLERANCE: u8 = 16;

/// Upper bound of trim passes (iscc-sdk's `TRIM_MAX_PASSES`, against crafted nested borders).
const TRIM_MAX_PASSES: usize = 16;

/// A box of pixels as Pillow writes it: left, top, right and bottom, the last two exclusive.
type Area = (u32, u32, u32, u32);

/// Crop near-uniform borders as iscc-sdk's `image_trim_tolerant` and iscc-sci's
/// `trim_border_tolerant` do (see `trim_area`).
pub(crate) fn trim_border(img: &RgbImage) -> RgbImage {
    let (left, top, right, bottom) = trim_area(img);
    image::imageops::crop_imm(img, left, top, right - left, bottom - top).to_image()
}

/// The area the tolerant trim keeps: pass after pass, the box of the pixels that differ from the
/// area's top-left pixel by more than `TRIM_TOLERANCE` in any channel, until a pass crops nothing
/// or `TRIM_MAX_PASSES` have cropped.
fn trim_area(img: &RgbImage) -> Area {
    let mut area = (0, 0, img.width(), img.height());
    for _ in 0..TRIM_MAX_PASSES {
        match trim_pass(img, area) {
            Some(next) => area = next,
            None => break,
        }
    }
    area
}

/// The area one pass crops `area` to, or None when it crops nothing. A pass that cuts one side
/// only must cut a flat band (no pixel there beyond half the tolerance), so smooth one-sided
/// gradients such as a sky stay.
fn trim_pass(img: &RgbImage, area: Area) -> Option<Area> {
    let [content, flat] = content_bounds(img, area);
    let content = content.filter(|b| *b != area)?;
    let (left, top, right, bottom) = content;
    let sides = [left > area.0, top > area.1, right < area.2, bottom < area.3];
    let one_side = sides.iter().filter(|cut| **cut).count() == 1;
    (!one_side || flat == Some(content)).then_some(content)
}

/// Bounding boxes, within `area`, of the pixels whose largest channel difference to the area's
/// top-left pixel exceeds `TRIM_TOLERANCE` and half of it; None where no pixel does.
fn content_bounds(img: &RgbImage, area: Area) -> [Option<Area>; 2] {
    let (x0, y0, x1, y1) = area;
    let mut found = [None, None];
    if x0 >= x1 || y0 >= y1 {
        return found;
    }
    let reference = *img.get_pixel(x0, y0);
    for y in y0..y1 {
        for x in x0..x1 {
            let pixel = img.get_pixel(x, y);
            let distance = (0..3)
                .map(|c| pixel[c].abs_diff(reference[c]))
                .max()
                .unwrap_or(0);
            for (bounds, limit) in found.iter_mut().zip([TRIM_TOLERANCE, TRIM_TOLERANCE / 2]) {
                if distance > limit {
                    *bounds = Some(grown(*bounds, x, y));
                }
            }
        }
    }
    found
}

/// `bounds` grown to take in the pixel at `x`, `y`.
fn grown(bounds: Option<Area>, x: u32, y: u32) -> Area {
    match bounds {
        None => (x, y, x + 1, y + 1),
        Some((left, top, right, bottom)) => {
            (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1))
        }
    }
}

/// ITU-R 601-2 luma with Pillow's fixed-point rounding.
fn to_luma(img: &RgbImage) -> GrayImage {
    let (w, h) = img.dimensions();
    let mut out = GrayImage::new(w, h);
    for (dst, src) in out.pixels_mut().zip(img.pixels()) {
        let l =
            (src[0] as u32 * 19595 + src[1] as u32 * 38470 + src[2] as u32 * 7471 + 0x8000) >> 16;
        dst[0] = l as u8;
    }
    out
}

/// Concatenate unit headers and bodies into the ISCC-SEQ byte string of IEP-0020.
pub fn encode_seq(units: &[String]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for unit in units {
        let described = describe_unit(unit)?;
        let raw = codec::decode_base32(described.iscc.trim_start_matches("ISCC:"))?;
        out.extend(raw);
    }
    Ok(out)
}

/// Split an ISCC-SEQ byte string back into unit strings, rejecting composite codes and IDs.
pub fn decode_seq(bytes: &[u8]) -> Result<Vec<String>> {
    let mut units = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (mt, st, vs, len_index, tail) = codec::decode_header(rest)?;
        if mt > MainType::Instance {
            bail!("ISCC-SEQ contains a composite code or ID");
        }
        // IEP-0020, Decoding: a Length field above 7 is rejected.
        if len_index > 7 {
            bail!("ISCC-SEQ has a Length field above 7");
        }
        let bits = codec::decode_length(mt, len_index, st);
        let nbytes = (bits / 8) as usize;
        if tail.len() < nbytes {
            bail!("ISCC-SEQ is truncated");
        }
        let unit = codec::encode_component(mt, st, vs, bits, &tail[..nbytes])?;
        units.push(format!("ISCC:{unit}"));
        let consumed = rest.len() - tail.len() + nbytes;
        rest = &rest[consumed..];
    }
    Ok(units)
}

/// Similarity of two units of the same kind as the fraction of equal bits; None when kinds differ.
pub fn similarity(a: &str, b: &str) -> Result<Option<f64>> {
    let (mt_a, st_a, _, _, body_a) = iscc_lib::iscc_decode(a)?;
    let (mt_b, st_b, _, _, body_b) = iscc_lib::iscc_decode(b)?;
    if mt_a != mt_b || st_a != st_b || body_a.len() != body_b.len() || body_a.is_empty() {
        return Ok(None);
    }
    let differing: u32 = body_a
        .iter()
        .zip(&body_b)
        .map(|(x, y)| (x ^ y).count_ones())
        .sum();
    let bits = (body_a.len() * 8) as f64;
    Ok(Some(1.0 - differing as f64 / bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IEP_UNITS: [&str; 4] = [
        "ISCC:AADVPDD4R6733NMPFH3D57VTQ4KVH6VHL74XIWGUT37V5ZG56NV3NSI",
        "ISCC:EAD2RASIYU5IKLENP2OFI4CHZGRWYQCSW2WKX3Y6FJGOCXSYNYGLGBI",
        "ISCC:GADQLNA7GRZESMRF2J7NZPNWGI3II2ST5YUN5SS6GVQ2ZQGJXPPYDNI",
        "ISCC:IAD2KIVPJIWJZP3KQCESJL6SVT5APEZUPOJWM6HVTAXCF7OT3VFA4NY",
    ];

    #[test]
    fn seq_roundtrip_matches_iep_0020_example() {
        let units: Vec<String> = IEP_UNITS.iter().map(|s| s.to_string()).collect();
        let seq = encode_seq(&units).unwrap();
        assert_eq!(seq.len(), 4 * (2 + 32));
        assert_eq!(decode_seq(&seq).unwrap(), units);
    }

    /// IEP-0020, Decoding: a Length field above 7 is rejected (a Data-Code header with Length 8).
    #[test]
    fn seq_rejects_a_length_above_seven() {
        let mut seq = vec![0x30, 0x08, 0x00];
        seq.extend([0u8; 36]);
        assert!(decode_seq(&seq).is_err());
    }

    #[test]
    fn seq_rejects_composite_code() {
        let composite = iscc_lib::gen_iscc_code_v0(&IEP_UNITS, false).unwrap().iscc;
        assert!(encode_seq(&[composite]).is_err());
    }

    #[test]
    fn describe_classifies_units() {
        let names: Vec<&str> = IEP_UNITS
            .iter()
            .map(|u| describe_unit(u).unwrap().name)
            .collect();
        assert_eq!(
            names,
            [
                "Meta-Code",
                "Content-Code Text",
                "Data-Code",
                "Instance-Code"
            ]
        );
        // Semantic-Codes of the iscc-sct README and of iscc-samples' demo.bmp.
        let text = describe_unit("ISCC:CADV3GG6JH3XEVRNSVYGCLJ7AAV3BOT5J7EHEZKPFXEGRJ2CTWACGZI");
        let image = describe_unit("ISCC:CEDQ2WTPK2QPZTK47HLOYUUTO3TCA35K5UQZODFMMY42S6O6RDQWFEA");
        assert_eq!(text.unwrap().name, "Semantic-Code Text");
        assert_eq!(image.unwrap().name, "Semantic-Code Image");
    }

    #[test]
    fn selection_takes_the_semantic_unit_from_the_caller() {
        let slugs = ["meta", "semantic", "image", "data", "instance"].map(String::from);
        let selection = UnitSelection::from_slugs(&slugs);
        assert!(selection.meta && selection.semantic && selection.content);
        let bitstream = bitstream_units(b"some bytes").unwrap();
        let semantic =
            describe_unit("ISCC:CEDQ2WTPK2QPZTK47HLOYUUTO3TCA35K5UQZODFMMY42S6O6RDQWFEA").unwrap();
        let rgb = RgbImage::from_pixel(8, 8, image::Rgb([200, 30, 30]));
        let meta = MetaInput {
            name: Some("A title"),
            ..Default::default()
        };
        let units = units_for(
            &bitstream,
            Content::Image(&rgb),
            meta,
            Some(semantic.clone()),
            &selection,
        )
        .unwrap();
        let order: Vec<&str> = units.iter().map(|u| u.unit).collect();
        assert_eq!(order, ["meta", "semantic", "content", "data", "instance"]);
        assert_eq!(units[1], semantic);
        let missing = units_for(&bitstream, Content::Image(&rgb), meta, None, &selection);
        assert!(missing.unwrap_err().to_string().contains("not available"));
    }

    #[test]
    fn bilinear_resize_follows_pillow() {
        // Pillow 12.3: Image.frombytes("RGB", (4, 1), bytes([0,0,0, 255,0,0, 0,255,0, 0,0,255]))
        // .resize((2, 1), BILINEAR) and the 1x2 upscale of a two-pixel gray column.
        let row = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
        let down = resize_pillow(&row, 3, (4, 1), (2, 1), Filter::Bilinear);
        assert_eq!(down, [109, 36, 0, 36, 109, 109]);
        let up = resize_pillow(&[0, 255], 1, (1, 2), (1, 4), Filter::Bilinear);
        assert_eq!(up, [0, 64, 191, 255]);
        // Sizes that match pass the samples through.
        assert_eq!(
            resize_pillow(&row, 3, (4, 1), (4, 1), Filter::Bilinear),
            row
        );
    }

    #[test]
    fn similarity_is_one_for_identical_units_and_none_across_kinds() {
        assert_eq!(similarity(IEP_UNITS[1], IEP_UNITS[1]).unwrap(), Some(1.0));
        assert_eq!(similarity(IEP_UNITS[1], IEP_UNITS[2]).unwrap(), None);
    }

    #[test]
    fn image_units_match_python_reference() {
        // Expected values produced by tests/fixtures/expected_iscc.py (Pillow + iscc-core).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_iscc.json")).unwrap();
        let selection = UnitSelection {
            content: true,
            data: true,
            instance: true,
            ..Default::default()
        };
        for (file, codes) in expected.as_object().unwrap() {
            let bytes = std::fs::read(fixture(file)).unwrap();
            let rgb = decode_rgb(&bytes).unwrap();
            let bitstream = bitstream_units(&bytes).unwrap();
            let units = units_for(
                &bitstream,
                Content::Image(&rgb),
                Default::default(),
                None,
                &selection,
            )
            .unwrap();
            let got: Vec<&str> = units.iter().map(|u| u.iscc.as_str()).collect();
            let want: Vec<&str> = codes
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert_eq!(got, want, "{file}");
        }
    }

    /// IEP-0020's sample vectors 0008 to 0011 (iscc-sdk's `c2pa_vectors.json`): the area the
    /// tolerant trim keeps (`image_crop`) and the Content-Code Image of each embedded picture.
    #[test]
    fn tolerant_trim_follows_the_iep_0020_vectors() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/c2pa_vectors.json")).unwrap();
        let mut checked = 0;
        for (name, vector) in vectors["soft_binding"].as_object().unwrap() {
            let Some(crop) = vector["outputs"].get("image_crop") else {
                continue;
            };
            let png = STANDARD
                .decode(vector["inputs"]["bytes_b64"].as_str().unwrap())
                .unwrap();
            let rgb = decode_rgb(&png).unwrap();
            let (left, top, right, bottom) = trim_area(&rgb);
            assert_eq!(
                *crop,
                serde_json::json!([left, top, right, bottom]),
                "{name}"
            );
            let code = content_unit(Content::Image(&rgb)).unwrap().iscc;
            let units = vector["outputs"]["units"].as_array().unwrap();
            assert!(units.iter().any(|u| *u == code), "{name}: {code}");
            checked += 1;
        }
        assert_eq!(checked, 4, "vectors 0008 to 0011");
    }

    /// Twenty-one nested rings of alternating black and white: each pass removes one ring, and
    /// the trim stops after 16 passes as iscc-sdk's does (checked with iscc-sdk 0.9.7).
    #[test]
    fn tolerant_trim_stops_after_sixteen_passes() {
        let side = 42;
        let img = RgbImage::from_fn(side, side, |x, y| {
            let ring = x.min(y).min(side - 1 - x).min(side - 1 - y);
            image::Rgb([if ring % 2 == 0 { 0 } else { 255 }; 3])
        });
        assert_eq!(trim_area(&img), (16, 16, side - 16, side - 16));
    }

    /// A picture without any pixel beyond the tolerance keeps its size.
    #[test]
    fn tolerant_trim_keeps_a_near_uniform_picture() {
        let img = RgbImage::from_fn(5, 3, |x, _| image::Rgb([100 + 4 * x as u8; 3]));
        assert_eq!(trim_area(&img), (0, 0, 5, 3));
        assert_eq!(trim_area(&RgbImage::new(1, 1)), (0, 0, 1, 1));
    }

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    /// Offset of the first JPEG header segment with `marker`, walking the segments from SOI.
    fn segment(bytes: &[u8], marker: u8) -> usize {
        let mut pos = 2;
        while bytes[pos + 1] != marker {
            pos += 2 + usize::from(u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]));
        }
        pos
    }

    /// `bytes` with two zero bytes before its quantisation table, as in some camera files.
    fn with_stray_bytes(bytes: &[u8]) -> Vec<u8> {
        let pos = segment(bytes, 0xDB);
        [&bytes[..pos], &[0, 0], &bytes[pos..]].concat()
    }

    #[test]
    fn a_jpeg_the_decoder_refuses_falls_back_to_the_image_crate() {
        let bytes = std::fs::read(fixture("IMG_0003.jpg")).unwrap();
        let stray = with_stray_bytes(&bytes);
        assert!(
            decode_jpeg(&stray).is_none(),
            "refused; once accepted, drop this test"
        );
        let rgb = decode_rgb(&stray).unwrap();
        assert_eq!(rgb.dimensions(), decode_rgb(&bytes).unwrap().dimensions());
    }

    #[test]
    fn subsampled_cmyk_falls_back_to_the_image_crate() {
        let bytes = std::fs::read(fixture("jpeg/cmyk-420.jpg")).unwrap();
        assert!(decode_jpeg(&bytes).is_none());
        assert_eq!(decode_rgb(&bytes).unwrap().dimensions(), (17, 23));
    }

    #[test]
    fn rgb_named_components_without_markers_decode_as_rgb() {
        let bytes = std::fs::read(fixture("jpeg/rgb-no-marker.jpg")).unwrap();
        assert!(decode_jpeg(&bytes).is_none());
        let rgb = decode_rgb(&bytes).unwrap();
        assert!(
            rgb.pixels().all(|p| p.0 == [255, 0, 0]),
            "{:?}",
            rgb.get_pixel(0, 0)
        );
    }

    #[test]
    fn a_jpeg_header_asking_for_gigabytes_is_refused() {
        let mut bytes = std::fs::read(fixture("CA.jpg")).unwrap();
        let sof = segment(&bytes, 0xC0);
        // 40000 x 40000: under the decoder's own pixel limit, far over JPEG_MAX_MEMORY.
        bytes[sof + 5..sof + 9].copy_from_slice(&[0x9C, 0x40, 0x9C, 0x40]);
        assert!(decode_jpeg(&bytes).is_none());
        assert!(decode_rgb(&bytes).is_err());
    }

    /// Every JPEG of `JPEG_CORPUS_DIR` decodes to the pixels Pillow gives, as recorded there by
    /// `expected_jpeg.py`; files the decoder refuses are listed, not failed.
    #[test]
    #[ignore = "needs a local corpus; set JPEG_CORPUS_DIR and run with --ignored"]
    fn jpeg_corpus_decodes_like_pillow() {
        let dir =
            std::path::PathBuf::from(std::env::var("JPEG_CORPUS_DIR").expect("JPEG_CORPUS_DIR"));
        let json = std::fs::read_to_string(dir.join("expected_jpeg.json")).unwrap();
        let expected: serde_json::Value = serde_json::from_str(&json).unwrap();
        let files = expected
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, _)| !name.starts_with('_'));
        let (mut refused, mut wrong, mut count) = (Vec::new(), Vec::new(), 0);
        for (name, want) in files {
            count += 1;
            match decode_jpeg(&std::fs::read(dir.join(name)).unwrap()) {
                None => refused.push(name.as_str()),
                Some(rgb) if blake3::hash(rgb.as_raw()).to_hex().as_str() != want["rgb_blake3"] => {
                    wrong.push(name.as_str())
                }
                Some(_) => {}
            }
        }
        println!(
            "{count} JPEGs, {} refused (the image crate decodes them): {refused:?}",
            refused.len()
        );
        assert!(
            wrong.is_empty(),
            "{} not as Pillow decodes them: {wrong:?}",
            wrong.len()
        );
    }

    /// A reader that hands out at most 7777 bytes per read, so chunk borders fall anywhere.
    struct Pieces<'a>(&'a [u8]);

    impl Read for Pieces<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.0.len()).min(7777);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn streamed_units_equal_the_in_memory_ones() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let mut files = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if !entry.file_type().unwrap().is_file() {
                continue;
            }
            let bytes = std::fs::read(entry.path()).unwrap();
            let (streamed, size) = stream_units(Pieces(&bytes)).unwrap();
            assert_eq!(size, bytes.len() as u64);
            assert_eq!(
                streamed,
                bitstream_units(&bytes).unwrap(),
                "{:?}",
                entry.path()
            );
            files += 1;
        }
        assert!(files > 50, "{files} fixtures");
        let (empty, size) = stream_units(&[][..]).unwrap();
        assert_eq!((empty, size), (bitstream_units(&[]).unwrap(), 0));
    }
}
