//! Semantic-Code Image as iscc-sci 0.3.0 computes it: the decoded picture (EXIF-transposed and
//! flattened on white, as for the Content-Code), its uniform border trimmed, squashed to 512x512
//! with Pillow's bilinear filter, scaled to [-1, 1] and embedded by the ISC21 descriptor model.

use std::path::Path;

use anyhow::Result;
use image::RgbImage;
use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, Tensor};

use crate::iscc::{resize_pillow, trim_border, Filter};

/// Side of the square model input.
const SIDE: u32 = 512;

/// Memo key of the picture: its size and pixels.
pub(super) fn key(rgb: &RgbImage) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"image");
    hasher.update(&rgb.width().to_le_bytes());
    hasher.update(&rgb.height().to_le_bytes());
    hasher.update(rgb.as_raw());
    *hasher.finalize().as_bytes()
}

/// The 256-d embedding of the picture by the image model at `model`.
pub(super) fn embedding(model: &Path, rgb: &RgbImage) -> Result<Vec<f32>> {
    let model = Model::load_file(model)?;
    let input = NdTensor::from_data([1, 3, SIDE as usize, SIDE as usize], preprocess(rgb));
    let (input_id, output_id) = (model.node_id("input_0")?, model.node_id("output_0")?);
    let mut outputs = model.run(vec![(input_id, input.into())], &[output_id], None)?;
    let output: Tensor<f32> = outputs.remove(0).try_into()?;
    Ok(output.to_vec())
}

/// The model input of the picture, NCHW: border trimmed, resized like
/// `Image.resize((512, 512), BILINEAR)` and each sample `(x / 255 - 0.5) / 0.5` in f32, the
/// operations iscc-sci runs in numpy.
pub(super) fn preprocess(rgb: &RgbImage) -> Vec<f32> {
    let trimmed = trim_border(rgb);
    let pixels = resize_pillow(
        trimmed.as_raw(),
        3,
        trimmed.dimensions(),
        (SIDE, SIDE),
        Filter::Bilinear,
    );
    let plane = (SIDE * SIDE) as usize;
    let mut tensor = vec![0f32; 3 * plane];
    for (i, pixel) in pixels.as_chunks::<3>().0.iter().enumerate() {
        for (c, sample) in pixel.iter().enumerate() {
            tensor[c * plane + i] = (f32::from(*sample) / 255.0 - 0.5) / 0.5;
        }
    }
    tensor
}
