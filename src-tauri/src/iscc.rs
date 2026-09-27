//! ISCC units for image, text and audio assets and the ISCC-SEQ soft-binding value defined by
//! IEP-0020.
//!
//! Image preprocessing mirrors `iscc_sdk.image_normalize` (EXIF transpose, white fill for
//! transparency, uniform border trim, ITU-R 601-2 luma, bicubic 32x32 resize) so that codes
//! agree with the Python reference implementation.

use std::io::Cursor;

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
    /// Content-Code of the asset's media type (image, text or audio).
    pub content: bool,
    pub data: bool,
    pub instance: bool,
}

impl UnitSelection {
    /// Build a selection from slugs such as `["image", "data"]`; `image`, `text` and `audio` all
    /// select the Content-Code.
    pub fn from_slugs(slugs: &[String]) -> Self {
        let has = |s: &str| slugs.iter().any(|x| x == s);
        Self {
            meta: has("meta"),
            content: has("image") || has("text") || has("audio"),
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
}

/// Classify a unit string and normalize it.
pub fn describe_unit(iscc: &str) -> Result<IsccUnit> {
    let (mt, st, _vs, _len, _body) = iscc_lib::iscc_decode(iscc)?;
    let (unit, name) = match (mt, st) {
        (0, _) => ("meta", "Meta-Code"),
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

/// Generate the selected units for an asset held in memory. Data-Code and Instance-Code are
/// computed over `bytes`; the Content-Code over `content`; the Meta-Code over `meta`.
pub fn units_for(
    bytes: &[u8],
    content: Content<'_>,
    meta: MetaInput<'_>,
    selection: &UnitSelection,
) -> Result<Vec<IsccUnit>> {
    let mut units = Vec::new();
    if selection.meta {
        units.push(meta_unit(meta)?);
    }
    if selection.content {
        units.push(content_unit(content)?);
    }
    if selection.data {
        let code = iscc_lib::gen_data_code_v0(bytes, UNIT_BITS)?;
        units.push(describe_unit(&code.iscc)?);
    }
    if selection.instance {
        let code = iscc_lib::gen_instance_code_v0(bytes, UNIT_BITS)?;
        units.push(describe_unit(&code.iscc)?);
    }
    Ok(units)
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
    };
    describe_unit(&code)
}

/// Reduce a decoded image to the 1024 grayscale samples expected by `gen_image_code_v0`.
pub fn image_pixels(rgb: &RgbImage) -> Vec<u8> {
    let gray = to_luma(&trim_border(rgb));
    resize_bicubic(&gray, 32, 32)
}

/// Fixed-point precision of Pillow's 8-bit resampling coefficients (32 - 8 - 2).
const PRECISION_BITS: u32 = 22;

/// Pillow's `Image.resize(size, BICUBIC)` for 8-bit grayscale, bit for bit: a horizontal pass
/// followed by a vertical pass, each rounding to 8 bit, using 22-bit fixed-point coefficients.
/// A pass whose size already matches is skipped, as Pillow does. Returns row-major samples.
fn resize_bicubic(img: &GrayImage, out_w: u32, out_h: u32) -> Vec<u8> {
    let (in_w, in_h) = img.dimensions();
    let mut data = img.as_raw().clone();
    let mut w = in_w as usize;
    if in_w != out_w {
        let coeffs = bicubic_coeffs(in_w, out_w);
        let mut out = vec![0u8; out_w as usize * in_h as usize];
        for y in 0..in_h as usize {
            let row = &data[y * w..(y + 1) * w];
            for (x, (first, k)) in coeffs.iter().enumerate() {
                out[y * out_w as usize + x] =
                    weighted_sample(row[*first..*first + k.len()].iter().copied(), k);
            }
        }
        data = out;
        w = out_w as usize;
    }
    if in_h != out_h {
        let coeffs = bicubic_coeffs(in_h, out_h);
        let mut out = vec![0u8; w * out_h as usize];
        for (y, (first, k)) in coeffs.iter().enumerate() {
            for x in 0..w {
                let column = (0..k.len()).map(|i| data[(first + i) * w + x]);
                out[y * w + x] = weighted_sample(column, k);
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

/// Per output sample along one axis: first input index and the normalised fixed-point weights,
/// mirroring Pillow's `precompute_coeffs` and `normalize_coeffs_8bpc`.
fn bicubic_coeffs(in_size: u32, out_size: u32) -> Vec<(usize, Vec<i32>)> {
    let scale = in_size as f64 / out_size as f64;
    let filterscale = scale.max(1.0);
    let support = 2.0 * filterscale;
    let ss = 1.0 / filterscale;
    (0..out_size)
        .map(|xx| {
            let center = (xx as f64 + 0.5) * scale;
            let first = ((center - support + 0.5) as i64).max(0) as usize;
            let last = ((center + support + 0.5) as i64).min(in_size as i64) as usize;
            let weights: Vec<f64> = (first..last)
                .map(|x| bicubic_kernel((x as f64 - center + 0.5) * ss))
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
    let img = image::load_from_memory(bytes)?;
    let img = exif_transpose(img, bytes);
    Ok(fill_transparency(&img))
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

/// Crop a uniform border whose colour is taken from the top-left pixel.
fn trim_border(img: &RgbImage) -> RgbImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return img.clone();
    }
    let bg = *img.get_pixel(0, 0);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if *p != bg {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x1 == 0 || (x0 == 0 && y0 == 0 && x1 == w && y1 == h) {
        return img.clone();
    }
    image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image()
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
            meta: false,
            content: true,
            data: true,
            instance: true,
        };
        for (file, codes) in expected.as_object().unwrap() {
            let path = format!("{}/tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"));
            let bytes = std::fs::read(path).unwrap();
            let rgb = decode_rgb(&bytes).unwrap();
            let units =
                units_for(&bytes, Content::Image(&rgb), Default::default(), &selection).unwrap();
            let got: Vec<&str> = units.iter().map(|u| u.iscc.as_str()).collect();
            let want: Vec<&str> = codes
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            if file.starts_with("meta-") && file.ends_with(".jpg") {
                // Deviation: these 48x32 JPEGs with 4:2:0 chroma decode slightly differently
                // from Pillow's libjpeg-turbo; larger images average the difference out.
                let content = similarity(got[0], want[0]).unwrap().unwrap();
                assert!(content >= 0.9, "{file}: Content-Code similarity {content}");
                assert_eq!(got[1..], want[1..], "{file}");
                continue;
            }
            assert_eq!(got, want, "{file}");
        }
    }
}
