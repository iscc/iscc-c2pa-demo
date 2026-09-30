//! A picture scaled down to a long edge and encoded as JPEG, for the preview and the claim
//! thumbnail.

use std::io::Cursor;

use anyhow::Result;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::RgbImage;

/// Long edge of the claim thumbnail in pixels.
pub const THUMBNAIL_EDGE: u32 = 256;
/// JPEG quality of the claim thumbnail.
pub const THUMBNAIL_QUALITY: u8 = 75;
/// Media type of the claim thumbnail.
pub const THUMBNAIL_MIME: &str = "image/jpeg";

/// JPEG of `rgb` at `quality`, scaled down so that its long edge is at most `edge`; a smaller
/// picture keeps its size.
pub fn scaled_jpeg(rgb: &RgbImage, edge: u32, quality: u8) -> Result<Vec<u8>> {
    let (w, h) = rgb.dimensions();
    let scale = (edge as f64 / w.max(h).max(1) as f64).min(1.0);
    let (sw, sh) = (
        ((w as f64 * scale) as u32).max(1),
        ((h as f64 * scale) as u32).max(1),
    );
    let small = imageops::resize(rgb, sw, sh, FilterType::Triangle);
    let mut buf = Cursor::new(Vec::new());
    JpegEncoder::new_with_quality(&mut buf, quality).encode_image(&small)?;
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{asset, inspect};
    use std::path::Path;

    /// Size of `rgb` after a round trip through `scaled_jpeg` at the thumbnail settings.
    fn thumbnail_size(rgb: &RgbImage) -> (u32, u32) {
        let jpeg = scaled_jpeg(rgb, THUMBNAIL_EDGE, THUMBNAIL_QUALITY).unwrap();
        let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
        (decoded.width(), decoded.height())
    }

    #[test]
    fn scaled_jpeg_never_enlarges() {
        assert_eq!(thumbnail_size(&RgbImage::new(48, 32)), (48, 32));
        assert_eq!(thumbnail_size(&RgbImage::new(2100, 1500)), (256, 182));
        assert_eq!(thumbnail_size(&RgbImage::new(300, 600)), (128, 256));
        let (w, h) = thumbnail_size(&RgbImage::new(5000, 10));
        assert_eq!(w, 256);
        assert!(h >= 1);
    }

    #[test]
    fn claim_thumbnail_is_small() {
        for name in ["no_manifest.jpg", "synthetic-640x480.png", "meta-xmp.webp"] {
            let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
            let path = Path::new(&path);
            let bytes = std::fs::read(path).unwrap();
            let asset = asset::read(path, &bytes, inspect::asset_format(path).unwrap()).unwrap();
            let picture = asset.picture().expect("raster image");
            let jpeg = scaled_jpeg(picture, THUMBNAIL_EDGE, THUMBNAIL_QUALITY).unwrap();
            assert!(jpeg.len() < 12_000, "{name}: {} bytes", jpeg.len());
            let decoded =
                image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
            assert!(
                decoded.width().max(decoded.height()) <= THUMBNAIL_EDGE,
                "{name}"
            );
        }
    }
}
