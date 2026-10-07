//! OCR of scanned pages: the PP-OCRv6 tiny text detector and recogniser (PaddlePaddle,
//! Apache-2.0) in rten, compiled into the binary, with a pipeline of our own around them.
//!
//! A page picture goes through these steps:
//! 1. Detection: the model's probability map, thresholded, gives one contour per text line; its
//!    minimum-area rectangle, kept when the mean probability inside is high enough, is grown as
//!    PaddleOCR's DB post-processing grows it (pyclipper's offset by area · ratio / perimeter).
//! 2. Reading order: a recursive XY-cut (column gutters first, then horizontal gaps, then lines by
//!    their vertical centre) on the line boxes turned back by the page's skew, the median angle
//!    of its long lines; the picture itself is never rotated.
//! 3. Recognition: each line, sampled straight from the page into a 48 px high input (turned a
//!    quarter when it is a vertical one), one line per run because padding lines to a common
//!    width changes the text; greedy CTC decoding through the model's dictionary.
//!
//! The recognised text is bitwise the same on any number of threads, but rten picks its kernels
//! by instruction set (AVX-512, AVX2, NEON or generic), so another CPU may read a character
//! differently now and then. A small memo keyed by the page's pixels recognises each page once
//! per session: a scan, its signed copy and that copy reopened render to the same pixels.

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use image::RgbImage;
use rten::ctc::CtcDecoder;
use rten::{Model, ModelOptions, RunOptions, ThreadPool};
use rten_imageproc::{find_contours, Point, PointF, RetrievalMode, RotatedRect, Vec2};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, NdTensorView};

use crate::iscc::{resize_pillow, Filter};
use crate::memo::Memo;
use crate::parallel;

/// The text detection model, `inference.onnx` of `PaddlePaddle/PP-OCRv6_tiny_det_onnx`.
static DETECTOR: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ocr/PP-OCRv6_tiny_det.onnx"
));
/// The text recognition model, `inference.onnx` of `PaddlePaddle/PP-OCRv6_tiny_rec_onnx`.
static RECOGNIZER: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ocr/PP-OCRv6_tiny_rec.onnx"
));
/// The recogniser's dictionary, one entry per line: label `i` reads as line `i - 1`.
static DICTIONARY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/ocr/PP-OCRv6_tiny_rec.txt"
));

/// Longest side of a page picture, a multiple of 32: pages render at this size, larger
/// pictures are scaled down first. About 180 dpi on A4 and Letter.
pub const PAGE_EDGE: u32 = 1984;
/// Mean and standard deviation per channel of the detector's input, in its channel order (blue,
/// green, red). The detection settings are RapidOCR's: on the quality set they read long
/// documents a little better than the model's own `inference.yml` (ImageNet mean and deviation,
/// thresholds 0.2 and 0.4, ratio 1.4), 0.970 and 0.978 against 0.967 and 0.971 on clean and rough
/// scans.
const DET_MEAN: [f32; 3] = [0.5, 0.5, 0.5];
const DET_STD: [f32; 3] = [0.5, 0.5, 0.5];
/// Probability above which a pixel is text.
const THRESH: f32 = 0.3;
/// Mean probability inside a box below which it is dropped.
const BOX_THRESH: f32 = 0.5;
/// How much a box grows: its area times this, over its perimeter, on every side.
const UNCLIP_RATIO: f32 = 1.6;
/// Most contours looked at per page.
const MAX_CANDIDATES: usize = 3000;
/// Shortest side of a box before it is grown, in pixels; after growing it must be 2 more.
const MIN_SIDE: f32 = 3.0;
/// Height of a line as the recogniser takes it, and the narrowest input it gets (wider lines keep
/// their aspect ratio, narrower ones are padded).
const REC_HEIGHT: usize = 48;
const REC_MIN_WIDTH: usize = 320;
/// A box at least this much taller than wide holds vertical text.
const VERTICAL: f32 = 1.5;
/// A line this many times longer than high counts towards the page's skew.
const SKEW_ASPECT: f32 = 5.0;
/// Narrowest column gutter, as a share of the page's long side (12 px at 300 dpi).
const GUTTER: f32 = 0.004;

/// Text recognised per page, by the hash of its pixels; about 5,000 pages.
static MEMO: Memo<String> = Memo::new(5000);

/// Both models, loaded for one document and dropped after it.
pub struct Models {
    detector: Model,
    recognizer: Model,
    /// Dictionary entries; label `i` is entry `i - 1`, the label after the last one a space.
    labels: Vec<&'static str>,
}

impl Models {
    /// Load both models from the binary.
    pub fn load() -> Result<Models> {
        let mut options = ModelOptions::with_all_ops();
        options.prepack_weights(true);
        let load = |bytes, what| {
            options
                .load_static_slice(bytes)
                .with_context(|| format!("cannot load the OCR {what} model"))
        };
        Ok(Models {
            detector: load(DETECTOR, "detection")?,
            recognizer: load(RECOGNIZER, "recognition")?,
            labels: DICTIONARY.lines().collect(),
        })
    }
}

/// The text of one page picture, its lines in reading order joined with a newline; the memo's
/// when the same pixels were recognised before. Runs on `threads` threads.
pub fn page_text(models: &Models, page: &RgbImage, threads: usize) -> Result<String> {
    let key = key(page);
    if let Some(text) = MEMO.recall(&key) {
        return Ok(text);
    }
    let text = recognize(models, page, threads)?;
    MEMO.keep(key, text.clone());
    Ok(text)
}

/// Memo key of a page picture: its size and pixels.
fn key(page: &RgbImage) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ocr");
    hasher.update(&page.width().to_le_bytes());
    hasher.update(&page.height().to_le_bytes());
    hasher.update(page.as_raw());
    *hasher.finalize().as_bytes()
}

/// The text of one page picture, recognised now (see the module documentation) on `threads`
/// threads: detection runs on all of them, the lines are spread over as many single-threaded
/// workers, since rten spreads one line poorly over many threads.
pub fn recognize(models: &Models, page: &RgbImage, threads: usize) -> Result<String> {
    let page = fit(page);
    let pool = Arc::new(ThreadPool::with_num_threads(threads.max(1)));
    let lines: Vec<Line> = detect(models, &page, &pool)?.iter().map(Line::of).collect();
    let order = reading_order(
        &lines,
        skew(&lines),
        GUTTER * page.width().max(page.height()) as f32,
    );
    let read = |i: usize, pool: &Arc<ThreadPool>| recognize_line(models, &page, &lines[i], pool);
    let texts = parallel::ordered(threads, 1, order.into_iter(), read, &mut |_| true)?;
    let texts: Vec<String> = texts.into_iter().filter(|t| !t.trim().is_empty()).collect();
    Ok(texts.join("\n"))
}

/// `page`, scaled down so that its long side is at most [`PAGE_EDGE`].
fn fit(page: &RgbImage) -> Cow<'_, RgbImage> {
    let (w, h) = page.dimensions();
    let long = w.max(h);
    if long <= PAGE_EDGE {
        return Cow::Borrowed(page);
    }
    let scale = PAGE_EDGE as f32 / long as f32;
    let size = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    let pixels = resize_pillow(page.as_raw(), 3, (w, h), size, Filter::Bilinear);
    Cow::Owned(RgbImage::from_raw(size.0, size.1, pixels).expect("buffer of the size"))
}

/// Run `model` on one input on `threads`; its first output as an f32 tensor.
fn run<const N: usize>(
    model: &Model,
    input: NdTensor<f32, 4>,
    threads: &Arc<ThreadPool>,
) -> Result<NdTensor<f32, N>> {
    let options = RunOptions::default().with_thread_pool(Some(threads.clone()));
    let output = model.run_one(input.into(), Some(options))?;
    Ok(output.try_into()?)
}

/// The text boxes the detector finds on `page`.
fn detect(models: &Models, page: &RgbImage, threads: &Arc<ThreadPool>) -> Result<Vec<RotatedRect>> {
    let map: NdTensor<f32, 4> = run(&models.detector, det_input(page), threads)?;
    let (w, h) = (page.width() as usize, page.height() as usize);
    // The padding is no part of the page.
    Ok(text_boxes(map.slice((0, 0, ..h, ..w))))
}

/// The detector's input: `page` padded with white to multiples of 32, its channels blue, green,
/// red, normalised with [`DET_MEAN`] and [`DET_STD`], NCHW.
fn det_input(page: &RgbImage) -> NdTensor<f32, 4> {
    let pad = |n: u32| (n as usize).div_ceil(32) * 32;
    let (w, h) = (pad(page.width()), pad(page.height()));
    let mut input = NdTensor::zeros([1, 3, h, w]);
    for c in 0..3 {
        let white = (1.0 - DET_MEAN[c]) / DET_STD[c];
        input.slice_mut((0, c)).fill(white);
    }
    for (x, y, pixel) in page.enumerate_pixels() {
        for (c, sample) in pixel.0.iter().rev().enumerate() {
            let value = (f32::from(*sample) / 255.0 - DET_MEAN[c]) / DET_STD[c];
            input[[0, c, y as usize, x as usize]] = value;
        }
    }
    input
}

/// The boxes of the text lines in a probability map, in its pixel coordinates.
fn text_boxes(map: NdTensorView<f32, 2>) -> Vec<RotatedRect> {
    let mask = map.map(|p| *p > THRESH);
    let contours = find_contours(mask.view(), RetrievalMode::External);
    contours
        .iter()
        .take(MAX_CANDIDATES)
        .filter_map(|contour| text_box(map, contour))
        .collect()
}

/// The grown box of one contour; `None` when it is too thin or too faint.
fn text_box(map: NdTensorView<f32, 2>, contour: &[Point]) -> Option<RotatedRect> {
    let rect = min_area_rect(contour)?;
    if rect.width().min(rect.height()) < MIN_SIDE || box_score(map, &rect) < BOX_THRESH {
        return None;
    }
    let grown = grow(&rect);
    (grown.width().min(grown.height()) >= MIN_SIDE + 2.0).then_some(grown)
}

/// The rectangle of least area around `points`, as OpenCV's `minAreaRect`: one side lies on an
/// edge of their convex hull. rten-imageproc's own collapses on long thin lines (a hull from
/// float angles), so this one works on the whole-number points exactly and measures each edge's
/// extent on both sides. `None` without points.
fn min_area_rect(points: &[Point]) -> Option<RotatedRect> {
    let hull = convex_hull(points);
    let first = *hull.first()?;
    let at = |p: Point| Vec2::from_xy(p.x as f32, p.y as f32);
    let mut best: Option<(f32, RotatedRect)> = None;
    for (i, a) in hull.iter().enumerate() {
        let b = hull[(i + 1) % hull.len()];
        let edge = at(b) - at(*a);
        // A hull of one point has no edge; any direction will do.
        let along = if edge.length() > 0.0 {
            edge.normalized()
        } else {
            Vec2::from_xy(1.0, 0.0)
        };
        let across = along.perpendicular();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for p in &hull {
            let d = at(*p) - at(first);
            for (k, axis) in [along, across].iter().enumerate() {
                let t = d.dot(*axis);
                lo[k] = lo[k].min(t);
                hi[k] = hi[k].max(t);
            }
        }
        let (width, height) = (hi[0] - lo[0], hi[1] - lo[1]);
        if best.as_ref().is_none_or(|(area, _)| width * height < *area) {
            let centre =
                at(first) + along * ((lo[0] + hi[0]) / 2.0) + across * ((lo[1] + hi[1]) / 2.0);
            let rect = RotatedRect::new(PointF::from_yx(centre.y, centre.x), across, width, height);
            best = Some((width * height, rect));
        }
    }
    best.map(|(_, rect)| rect)
}

/// The convex hull of `points` in order, by Andrew's monotone chain, in exact integer arithmetic.
fn convex_hull(points: &[Point]) -> Vec<Point> {
    let mut sorted = points.to_vec();
    sorted.sort_by_key(|p| (p.x, p.y));
    sorted.dedup();
    if sorted.len() < 3 {
        return sorted;
    }
    let cross = |o: Point, a: Point, b: Point| {
        i64::from(a.x - o.x) * i64::from(b.y - o.y) - i64::from(a.y - o.y) * i64::from(b.x - o.x)
    };
    // The lower chain from left to right, then the upper one back, each turning one way only;
    // the upper chain starts from the lower one's last point.
    let mut hull: Vec<Point> = Vec::with_capacity(sorted.len() + 1);
    for p in &sorted {
        while hull.len() >= 2 && cross(hull[hull.len() - 2], hull[hull.len() - 1], *p) <= 0 {
            hull.pop();
        }
        hull.push(*p);
    }
    let lower = hull.len();
    for p in sorted.iter().rev().skip(1) {
        while hull.len() > lower && cross(hull[hull.len() - 2], hull[hull.len() - 1], *p) <= 0 {
            hull.pop();
        }
        hull.push(*p);
    }
    // The upper chain ends where the lower one began.
    hull.pop();
    hull
}

/// Mean probability of the map's pixels inside `rect`.
fn box_score(map: NdTensorView<f32, 2>, rect: &RotatedRect) -> f32 {
    let corners = rect.corners();
    let range = |values: [f32; 4], len: usize| {
        let lo = values
            .iter()
            .copied()
            .fold(f32::MAX, f32::min)
            .floor()
            .max(0.0) as usize;
        let hi = values
            .iter()
            .copied()
            .fold(f32::MIN, f32::max)
            .ceil()
            .max(0.0) as usize;
        lo..(hi + 1).min(len)
    };
    let (rows, cols) = (
        range(corners.map(|p| p.y), map.size(0)),
        range(corners.map(|p| p.x), map.size(1)),
    );
    let (mut sum, mut n) = (0.0, 0);
    for y in rows {
        for x in cols.clone() {
            if rect.contains(PointF::from_yx(y as f32, x as f32)) {
                sum += map[[y, x]];
                n += 1;
            }
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f32
    }
}

/// `rect` grown on every side by its area times [`UNCLIP_RATIO`] over its perimeter, as
/// pyclipper's offset of the box grows it (rounded corners aside).
fn grow(rect: &RotatedRect) -> RotatedRect {
    let (w, h) = (rect.width(), rect.height());
    let d = w * h * UNCLIP_RATIO / (2.0 * (w + h));
    rect.expanded(2.0 * d, 2.0 * d)
}

/// A text line: its centre, the unit vectors along the text and down across it, its length and
/// height, in page pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Line {
    centre: Vec2,
    along: Vec2,
    down: Vec2,
    width: f32,
    height: f32,
}

impl Line {
    /// The line in a detected box: read left to right along its edge closer to horizontal, as
    /// PaddleOCR crops boxes; a box much taller than wide is vertical text, turned a quarter
    /// anticlockwise.
    fn of(rect: &RotatedRect) -> Line {
        // Image coordinates grow downwards, so up is negative y.
        let upright = rect.orient_towards(Vec2::from_yx(-1.0, 0.0));
        let up = upright.up_axis();
        let line = Line {
            centre: upright.center().to_vec(),
            along: Vec2::from_xy(-up.y, up.x),
            down: Vec2::from_xy(-up.x, -up.y),
            width: upright.width(),
            height: upright.height(),
        };
        if line.height >= VERTICAL * line.width {
            Line {
                along: line.down,
                down: Vec2::from_xy(-line.along.x, -line.along.y),
                width: line.height,
                height: line.width,
                ..line
            }
        } else {
            line
        }
    }

    /// The angle of the line against the horizontal, in radians.
    fn angle(&self) -> f32 {
        self.along.y.atan2(self.along.x)
    }

    /// Left, top, right and bottom of the line's corners turned by `-angle` about the origin.
    fn bounds(&self, angle: f32) -> [f32; 4] {
        let (sin, cos) = (-angle).sin_cos();
        let (hw, hh) = (self.width / 2.0, self.height / 2.0);
        let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)].map(|(a, d)| {
            let x = self.centre.x + a * self.along.x + d * self.down.x;
            let y = self.centre.y + a * self.along.y + d * self.down.y;
            (x * cos - y * sin, x * sin + y * cos)
        });
        let xs = corners.map(|c| c.0);
        let ys = corners.map(|c| c.1);
        let min = |v: [f32; 4]| v.into_iter().fold(f32::MAX, f32::min);
        let max = |v: [f32; 4]| v.into_iter().fold(f32::MIN, f32::max);
        [min(xs), min(ys), max(xs), max(ys)]
    }
}

/// The page's skew in radians: the median angle of its long lines, 0 without any.
fn skew(lines: &[Line]) -> f32 {
    let mut angles: Vec<f32> = lines
        .iter()
        .filter(|l| l.width >= SKEW_ASPECT * l.height)
        .map(Line::angle)
        .collect();
    if angles.is_empty() {
        return 0.0;
    }
    angles.sort_by(f32::total_cmp);
    angles[angles.len() / 2]
}

/// Indices of `lines` in reading order: a recursive XY-cut of their bounds turned back by the
/// page's `skew`, splitting at column gutters at least `gutter` wide first, then at horizontal
/// gaps, then ordering lines by their vertical centre and each from left to right.
fn reading_order(lines: &[Line], skew: f32, gutter: f32) -> Vec<usize> {
    let bounds: Vec<[f32; 4]> = lines.iter().map(|l| l.bounds(skew)).collect();
    xy_cut((0..lines.len()).collect(), &bounds, gutter)
}

/// `items` (indices into `bounds`) in reading order; see [`reading_order`].
fn xy_cut(items: Vec<usize>, bounds: &[[f32; 4]], gutter: f32) -> Vec<usize> {
    if items.len() <= 1 {
        return items;
    }
    for (axis, gap) in [(0, gutter), (1, 1.0)] {
        let parts = runs(&items, bounds, axis, gap);
        if parts.len() > 1 {
            return parts
                .into_iter()
                .flat_map(|part| xy_cut(part, bounds, gutter))
                .collect();
        }
    }
    rows(items, bounds)
}

/// `items` split into runs along `axis` (0 for x, 1 for y) where an empty gap of at least `gap`
/// separates them, in order along the axis.
fn runs(items: &[usize], bounds: &[[f32; 4]], axis: usize, gap: f32) -> Vec<Vec<usize>> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| bounds[*a][axis].total_cmp(&bounds[*b][axis]));
    let mut runs = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut end = f32::MIN;
    for i in sorted {
        let [lo, hi] = [bounds[i][axis], bounds[i][axis + 2]];
        if !current.is_empty() && lo - end >= gap {
            runs.push(std::mem::take(&mut current));
            end = f32::MIN;
        }
        current.push(i);
        end = end.max(hi);
    }
    runs.push(current);
    runs
}

/// Lines that overlap vertically, grouped into rows by their vertical centre (within half the
/// median height of the first in a row), rows top to bottom, each left to right.
fn rows(mut items: Vec<usize>, bounds: &[[f32; 4]]) -> Vec<usize> {
    let centre = |i: usize| (bounds[i][1] + bounds[i][3]) / 2.0;
    let mut heights: Vec<f32> = items
        .iter()
        .map(|i| bounds[*i][3] - bounds[*i][1])
        .collect();
    heights.sort_by(f32::total_cmp);
    let tolerance = heights[heights.len() / 2] / 2.0;
    items.sort_by(|a, b| centre(*a).total_cmp(&centre(*b)));
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut first = f32::MIN;
    for i in items {
        if rows.is_empty() || centre(i) - first > tolerance {
            rows.push(Vec::new());
            first = centre(i);
        }
        rows.last_mut().expect("a row").push(i);
    }
    for row in &mut rows {
        row.sort_by(|a, b| bounds[*a][0].total_cmp(&bounds[*b][0]));
    }
    rows.concat()
}

/// The text of one line of `page`.
fn recognize_line(
    models: &Models,
    page: &RgbImage,
    line: &Line,
    threads: &Arc<ThreadPool>,
) -> Result<String> {
    let output: NdTensor<f32, 3> = run(&models.recognizer, rec_input(page, line), threads)?;
    let hypothesis = CtcDecoder::new().decode_greedy(output.slice(0));
    Ok(hypothesis
        .steps()
        .iter()
        .map(|step| label(&models.labels, step.label as usize))
        .collect())
}

/// The text of CTC label `label` (0 is the blank): dictionary entry `label - 1`, a space for the
/// label after the last entry.
fn label<'a>(labels: &[&'a str], label: usize) -> &'a str {
    match label.checked_sub(1) {
        Some(i) => labels.get(i).copied().unwrap_or(" "),
        None => "",
    }
}

/// The recogniser's input for `line`: the line sampled bilinearly from `page` into
/// [`REC_HEIGHT`] rows, as wide as its aspect ratio asks, padded with zeros to at least
/// [`REC_MIN_WIDTH`]; channels blue, green, red, each `(x / 255 - 0.5) / 0.5`, NCHW. Samples
/// outside the page take its nearest edge.
fn rec_input(page: &RgbImage, line: &Line) -> NdTensor<f32, 4> {
    let width = ((REC_HEIGHT as f32 * line.width / line.height).ceil() as usize).max(1);
    let mut input = NdTensor::zeros([1, 3, REC_HEIGHT, width.max(REC_MIN_WIDTH)]);
    let (step_x, step_y) = (line.width / width as f32, line.height / REC_HEIGHT as f32);
    let origin = line.centre - line.along * (line.width / 2.0) - line.down * (line.height / 2.0);
    for v in 0..REC_HEIGHT {
        for u in 0..width {
            let p = origin
                + line.along * ((u as f32 + 0.5) * step_x)
                + line.down * ((v as f32 + 0.5) * step_y);
            let rgb = bilinear(page, p.x, p.y);
            for (c, sample) in rgb.iter().rev().enumerate() {
                input[[0, c, v, u]] = (sample / 255.0 - 0.5) / 0.5;
            }
        }
    }
    input
}

/// The colour of `page` at `(x, y)` in pixel coordinates (pixel centres at whole numbers),
/// interpolated between the four nearest pixels; outside the page, the nearest edge.
fn bilinear(page: &RgbImage, x: f32, y: f32) -> [f32; 3] {
    let (w, h) = (page.width() as f32 - 1.0, page.height() as f32 - 1.0);
    let (x, y) = (x.clamp(0.0, w), y.clamp(0.0, h));
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x1, y1) = ((x0 + 1.0).min(w), (y0 + 1.0).min(h));
    let at = |x: f32, y: f32| page.get_pixel(x as u32, y as u32).0.map(f32::from);
    let (a, b, c, d) = (at(x0, y0), at(x1, y0), at(x0, y1), at(x1, y1));
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x: f32, y: f32, width: f32, height: f32) -> Line {
        Line {
            centre: Vec2::from_xy(x, y),
            along: Vec2::from_xy(1.0, 0.0),
            down: Vec2::from_xy(0.0, 1.0),
            width,
            height,
        }
    }

    /// Lines whose left, top, right and bottom are given.
    fn lines(boxes: &[[f32; 4]]) -> Vec<Line> {
        boxes
            .iter()
            .map(|[l, t, r, b]| line((l + r) / 2.0, (t + b) / 2.0, r - l, b - t))
            .collect()
    }

    #[test]
    fn labels_map_to_dictionary_entries_and_a_space() {
        let labels = ["a", "ä", "日", "ab"];
        assert_eq!(label(&labels, 0), "", "blank");
        assert_eq!(label(&labels, 1), "a");
        assert_eq!(label(&labels, 3), "日");
        assert_eq!(
            label(&labels, 4),
            "ab",
            "an entry need not be one character"
        );
        assert_eq!(label(&labels, 5), " ", "the label after the last entry");
        let models = Models::load().unwrap();
        assert_eq!(models.labels.len(), 6904);
        assert_eq!(models.labels[..3], ["!", "\"", "#"]);
        assert!(models.labels.contains(&"ß") && models.labels.contains(&"ü"));
    }

    #[test]
    fn greedy_decoding_collapses_repeats_and_drops_blanks() {
        // Labels per step: a a blank a b b blank, as one-hot rows over blank, a, b.
        let steps = [1, 1, 0, 1, 2, 2, 0];
        let probs = NdTensor::from_fn(
            [steps.len(), 3],
            |[t, c]| {
                if steps[t] == c {
                    0.9
                } else {
                    0.05
                }
            },
        );
        let hypothesis = CtcDecoder::new().decode_greedy(probs.view());
        let text: String = hypothesis
            .steps()
            .iter()
            .map(|s| label(&["a", "b"], s.label as usize))
            .collect();
        assert_eq!(text, "aab");
    }

    /// The border of a filled rectangle of `w` x `h` pixels at `(x, y)`, turned by `degrees`
    /// about its corner and rounded to whole pixels, as a contour lists it.
    fn border(x: i32, y: i32, w: i32, h: i32, degrees: f32) -> Vec<Point> {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let mut points = Vec::new();
        for i in 0..w {
            points.extend([(i, 0), (i, h - 1)]);
        }
        for j in 0..h {
            points.extend([(0, j), (w - 1, j)]);
        }
        points
            .into_iter()
            .map(|(i, j)| {
                let (i, j) = (i as f32, j as f32);
                Point::from_yx(
                    y + (i * sin + j * cos).round() as i32,
                    x + (i * cos - j * sin).round() as i32,
                )
            })
            .collect()
    }

    #[test]
    fn min_area_rect_fits_long_thin_lines() {
        // A text line 777 px long and 10 high: rten-imageproc's rectangle was 1 px high.
        let line = min_area_rect(&border(100, 700, 777, 10, 0.0)).unwrap();
        let (w, h) = (
            line.width().max(line.height()),
            line.width().min(line.height()),
        );
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(w, 776.0) && near(h, 9.0), "{w} x {h}");
        let centre = line.center();
        assert!(near(centre.x, 488.0) && near(centre.y, 704.5), "{centre:?}");
        let turned = min_area_rect(&border(100, 700, 777, 10, 0.8)).unwrap();
        let (w, h) = (
            turned.width().max(turned.height()),
            turned.width().min(turned.height()),
        );
        assert!(
            (775.0..778.0).contains(&w) && (8.0..11.0).contains(&h),
            "{w} x {h}"
        );
        let point = min_area_rect(&[Point::from_yx(5, 7)]).unwrap();
        assert_eq!((point.width(), point.height()), (0.0, 0.0));
        assert!(min_area_rect(&[]).is_none());
    }

    #[test]
    fn convex_hull_keeps_the_corners() {
        let square = border(0, 0, 5, 5, 0.0);
        let hull = convex_hull(&square);
        assert_eq!(hull.len(), 4, "{hull:?}");
        for corner in [(0, 0), (4, 0), (4, 4), (0, 4)] {
            assert!(
                hull.contains(&Point::from_yx(corner.1, corner.0)),
                "{corner:?}"
            );
        }
        let collinear = [(0, 0), (1, 0), (2, 0)].map(|(x, y)| Point::from_yx(y, x));
        assert_eq!(convex_hull(&collinear).len(), 2);
    }

    #[test]
    fn boxes_grow_by_area_over_perimeter() {
        let rect = RotatedRect::new(
            PointF::from_yx(50.0, 100.0),
            Vec2::from_yx(-1.0, 0.0),
            100.0,
            20.0,
        );
        // d = 100 * 20 * 1.6 / 240 = 13.33 on every side.
        let grown = grow(&rect);
        assert!((grown.width() - 126.67).abs() < 0.01, "{}", grown.width());
        assert!((grown.height() - 46.67).abs() < 0.01, "{}", grown.height());
        assert_eq!(grown.center(), rect.center());
    }

    #[test]
    fn text_boxes_cover_the_lines_of_a_probability_map() {
        // Two lines of text and a speck too small to keep.
        let map = NdTensor::from_fn([60, 200], |[y, x]| {
            let first = (10..20).contains(&y) && (20..180).contains(&x);
            let second = (35..45).contains(&y) && (20..100).contains(&x);
            let speck = y == 55 && x == 5;
            if first || second || speck {
                0.9
            } else {
                0.01
            }
        });
        let mut boxes: Vec<[f32; 4]> = text_boxes(map.view())
            .iter()
            .map(|b| {
                let l = Line::of(b);
                l.bounds(0.0)
            })
            .collect();
        boxes.sort_by(|a, b| a[1].total_cmp(&b[1]));
        assert_eq!(boxes.len(), 2, "{boxes:?}");
        // The first line spans x 20..179 and y 10..19 by pixel centres, grown by about 6 px.
        let [l, t, r, b] = boxes[0];
        assert!(
            (13.0..16.0).contains(&l) && (183.0..186.0).contains(&r),
            "{l} {r}"
        );
        assert!(
            (3.0..6.0).contains(&t) && (23.0..26.0).contains(&b),
            "{t} {b}"
        );
    }

    #[test]
    fn detector_input_is_padded_with_white_to_multiples_of_32() {
        let page = RgbImage::from_pixel(40, 70, image::Rgb([0, 128, 255]));
        let input = det_input(&page);
        assert_eq!(input.shape(), [1, 3, 96, 64]);
        // Blue first: the red sample 0 lands in channel 2.
        let red = (0.0 - DET_MEAN[2]) / DET_STD[2];
        let blue = (1.0 - DET_MEAN[0]) / DET_STD[0];
        assert_eq!(input[[0, 2, 0, 0]], red);
        assert_eq!(input[[0, 0, 0, 0]], blue);
        assert_eq!(
            input[[0, 2, 80, 50]],
            (1.0 - DET_MEAN[2]) / DET_STD[2],
            "white"
        );
    }

    #[test]
    fn large_pictures_are_scaled_to_the_page_edge() {
        let page = RgbImage::new(3000, 1500);
        assert_eq!(fit(&page).dimensions(), (PAGE_EDGE, 992));
        let small = RgbImage::new(100, 50);
        assert!(matches!(fit(&small), Cow::Borrowed(_)));
    }

    #[test]
    fn tall_boxes_are_vertical_text() {
        let wide = Line::of(&RotatedRect::new(
            PointF::from_yx(0.0, 0.0),
            Vec2::from_yx(1.0, 0.0),
            90.0,
            30.0,
        ));
        assert_eq!((wide.width, wide.height), (90.0, 30.0));
        assert_eq!(wide.along, Vec2::from_xy(1.0, 0.0), "left to right");
        assert_eq!(wide.down, Vec2::from_xy(0.0, 1.0));
        let tall = Line::of(&RotatedRect::new(
            PointF::from_yx(0.0, 0.0),
            Vec2::from_yx(-1.0, 0.0),
            30.0,
            90.0,
        ));
        assert_eq!((tall.width, tall.height), (90.0, 30.0));
        assert_eq!(tall.along, Vec2::from_xy(0.0, 1.0), "top to bottom");
    }

    #[test]
    fn skew_is_the_median_angle_of_long_lines() {
        let tilted = |degrees: f32, width: f32| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            Line {
                along: Vec2::from_xy(cos, sin),
                down: Vec2::from_xy(-sin, cos),
                ..line(0.0, 0.0, width, 10.0)
            }
        };
        let page = [
            tilted(0.8, 300.0),
            tilted(0.7, 200.0),
            tilted(0.9, 400.0),
            tilted(30.0, 20.0), // short: a word in a figure
        ];
        assert!((skew(&page).to_degrees() - 0.8).abs() < 1e-4);
        assert_eq!(skew(&page[3..]), 0.0, "no long line");
        assert_eq!(skew(&[]), 0.0);
    }

    #[test]
    fn reading_order_takes_columns_under_a_title() {
        // Grown boxes of neighbouring lines overlap, so only a paragraph gap splits a column.
        let page = lines(&[
            [320.0, 102.0, 580.0, 126.0], // 0: right column, first line
            [20.0, 20.0, 580.0, 50.0],    // 1: title over both columns
            [20.0, 120.0, 280.0, 145.0],  // 2: left column, second line
            [20.0, 100.0, 280.0, 124.0],  // 3: left column, first line
            [320.0, 122.0, 580.0, 147.0], // 4: right column, second line
        ]);
        assert_eq!(reading_order(&page, 0.0, 8.0), [1, 3, 2, 0, 4]);
    }

    #[test]
    fn reading_order_keeps_table_rows_together() {
        // Cells of two rows whose gutters do not line up; words of a row sit at slightly
        // different heights.
        let page = lines(&[
            [200.0, 22.0, 300.0, 40.0], // 0: row 1, middle
            [10.0, 60.0, 150.0, 80.0],  // 1: row 2, left
            [10.0, 20.0, 100.0, 40.0],  // 2: row 1, left
            [140.0, 61.0, 300.0, 79.0], // 3: row 2, right
            [105.0, 21.0, 195.0, 41.0], // 4: row 1, between
        ]);
        assert_eq!(reading_order(&page, 0.0, 8.0), [2, 4, 0, 1, 3]);
        // A single column reads top to bottom.
        let column = lines(&[
            [10.0, 50.0, 300.0, 70.0],
            [10.0, 10.0, 300.0, 30.0],
            [10.0, 90.0, 200.0, 110.0],
        ]);
        assert_eq!(reading_order(&column, 0.0, 8.0), [1, 0, 2]);
    }

    #[test]
    fn reading_order_turns_a_skewed_page_back() {
        // Two lines of a page turned by 3 degrees: the right end of the first line sits lower
        // than the left end of the second, so the unturned bounds overlap vertically.
        let (sin, cos) = 3f32.to_radians().sin_cos();
        let turned = |x: f32, y: f32| Line {
            centre: Vec2::from_xy(x * cos - y * sin, x * sin + y * cos),
            along: Vec2::from_xy(cos, sin),
            down: Vec2::from_xy(-sin, cos),
            width: 1000.0,
            height: 20.0,
        };
        let page = [turned(500.0, 60.0), turned(500.0, 20.0)];
        assert_eq!(reading_order(&page, skew(&page), 8.0), [1, 0]);
    }

    /// Documents of the quality set long enough for a stable Content-Code (2,000 to 12,851
    /// collapsed characters).
    const LONG: [&str; 5] = ["iscc_demo", "tracemonkey", "attention", "hep_th", "geotopo"];

    /// Recognise the page images and compare their text with the truth that
    /// `cauldron/samples/ocr/spike-quality/prep.py` writes into `OCR_CORPUS_DIR`: per document
    /// and set (clean, rough), the similarity of the Content-Codes, and the time per page.
    #[test]
    #[ignore = "needs the OCR quality set; set OCR_CORPUS_DIR and run with --ignored --nocapture"]
    fn ocr_corpus() {
        let dir =
            std::path::PathBuf::from(std::env::var("OCR_CORPUS_DIR").expect("OCR_CORPUS_DIR"));
        let truth: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("gt/ground_truth.json")).unwrap())
                .unwrap();
        let models = Models::load().unwrap();
        let workers = std::thread::available_parallelism().map_or(1, |n| n.get());
        for (set, ext) in [("clean", "png"), ("rough", "jpg")] {
            let (mut long, mut all) = (Vec::new(), Vec::new());
            let (mut pages, started) = (0, std::time::Instant::now());
            for doc in truth.as_array().unwrap() {
                let name = doc["doc"].as_str().unwrap();
                let n = doc["pages"].as_u64().unwrap() as usize;
                let paths: Vec<_> = (1..=n)
                    .map(|p| dir.join(format!("images/{name}/{set}_p{p}.{ext}")))
                    .collect();
                let read = |path: &std::path::PathBuf, _: &Arc<ThreadPool>| {
                    recognize(&models, &image::open(path)?.to_rgb8(), 1)
                };
                let texts = crate::parallel::ordered(workers, 1, paths.iter(), read, &mut |_| true)
                    .unwrap();
                pages += n;
                let text = iscc_lib::text_clean(&texts.join("\n"));
                if let Ok(dump) = std::env::var("OCR_DUMP") {
                    let file = std::path::Path::new(&dump).join(format!("{set}_{name}.txt"));
                    std::fs::write(file, &text).unwrap();
                }
                let code = crate::iscc::content_unit(crate::iscc::Content::Text(&text))
                    .map(|u| u.iscc)
                    .unwrap_or_default();
                let similarity = crate::iscc::similarity(&code, doc["code"].as_str().unwrap())
                    .ok()
                    .flatten()
                    .unwrap_or(0.0);
                println!("{set} {name}: {similarity:.3}");
                all.push(similarity);
                if LONG.contains(&name) {
                    long.push(similarity);
                }
            }
            let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
            let min = |v: &[f64]| v.iter().copied().fold(1.0, f64::min);
            println!(
                "{set}: long documents {:.3} (min {:.3}), all {:.3}; {:.2} s per page on {workers} \
                workers",
                mean(&long),
                min(&long),
                mean(&all),
                started.elapsed().as_secs_f64() / pages as f64
            );
        }
    }

    /// Print the text recognised on the picture at `OCR_PAGE`, a debugging aid.
    #[test]
    #[ignore = "a debugging aid; set OCR_PAGE and run with --ignored --nocapture"]
    fn ocr_page() {
        let path = std::env::var("OCR_PAGE").expect("OCR_PAGE");
        let page = image::open(path).unwrap().to_rgb8();
        let models = Models::load().unwrap();
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let n = std::env::var("OCR_THREADS").map_or(cores, |n| n.parse().unwrap());
        let started = std::time::Instant::now();
        let text = recognize(&models, &page, n).unwrap();
        let threads = Arc::new(ThreadPool::with_num_threads(n));
        println!(
            "{text}\n{:.2} s on {n} threads",
            started.elapsed().as_secs_f64()
        );
        if let Ok(out) = std::env::var("OCR_MAP") {
            let page = fit(&page);
            let map: NdTensor<f32, 4> = run(&models.detector, det_input(&page), &threads).unwrap();
            let (w, h) = page.dimensions();
            let grey = image::GrayImage::from_fn(w, h, |x, y| {
                image::Luma([(map[[0, 0, y as usize, x as usize]] * 255.0) as u8])
            });
            grey.save(&out).unwrap();
            let view = map.slice((0, 0, ..h as usize, ..w as usize));
            let mask = view.map(|p| *p > THRESH);
            let contours = find_contours(mask.view(), RetrievalMode::External);
            for c in contours.iter() {
                let rect = min_area_rect(c).unwrap();
                let score = box_score(view, &rect);
                if score < BOX_THRESH || rect.width().min(rect.height()) < MIN_SIDE {
                    println!(
                        "dropped {} points at {:?}: {} x {}, score {score}",
                        c.len(),
                        rect.center(),
                        rect.width(),
                        rect.height()
                    );
                }
            }
            println!("{} contours", contours.len());
            for b in detect(&models, &page, &threads).unwrap() {
                let l = Line::of(&b);
                if l.height > 30.0 {
                    println!("tall box {:?} {} x {}", l.centre, l.width, l.height);
                }
            }
        }
    }

    #[test]
    fn recogniser_input_keeps_the_aspect_ratio() {
        let mut page = RgbImage::from_pixel(400, 100, image::Rgb([255, 255, 255]));
        for x in 100..300 {
            for y in 40..60 {
                page.put_pixel(x, y, image::Rgb([0, 0, 255]));
            }
        }
        let wide = rec_input(&page, &line(199.5, 49.5, 200.0, 20.0));
        assert_eq!(wide.shape(), [1, 3, 48, 480]);
        // Blue first: the blue box gives 1 in channel 0, -1 in the others.
        assert_eq!(wide[[0, 0, 24, 240]], 1.0);
        assert_eq!(wide[[0, 2, 24, 240]], -1.0);
        let short = rec_input(&page, &line(199.5, 49.5, 40.0, 20.0));
        assert_eq!(short.shape(), [1, 3, 48, 320]);
        assert_eq!(short[[0, 1, 24, 100]], 0.0, "padding");
    }
}
