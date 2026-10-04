//! Video assets: the MPEG-7 frame signatures behind the Content-Code Video, the tags behind the
//! Meta-Code, the duration, the frame size and a preview frame, all from the ffmpeg build
//! iscc-sdk uses (installed on first use by `tools`), run the way iscc-sdk runs it.
//!
//! - Signatures: `video_mp7sig_extract`, ffmpeg's `signature` filter at 5 frames per second
//!   after ffmpeg's autorotation, parsed like `read_mp7_signature`. Audio, subtitle and data
//!   streams are not decoded (`-an -sn -dn`), which changes nothing in the video filter chain.
//! - Tags: `video_meta_extract_ffmpeg`, ffmpeg's ffmetadata output mapped through
//!   `VIDEO_META_MAP`, values not sanitised. ffprobe is not used: its only Meta-Code input is the
//!   global title, which ffmetadata prints too (`expected_video.py` checks that).
//! - Preview: ffmpeg's `thumbnail` filter, scaled down first; display only.
//! - Signed copies: a stream fingerprint (every compressed video packet with its timestamps and
//!   the codec setup, plus the description of the video streams), taken while the source is
//!   analysed, shows without decoding that a copy decodes to the frames analysed, so the copy
//!   takes the source's signatures. c2pa-rs leaves the compressed video untouched in all four
//!   formats.
//!
//! ffmpeg reads every file with the demuxer of its format ([`demuxer`]), never one it guesses
//! from the content: as a DASH or HLS playlist, a crafted file would make ffmpeg open the paths
//! it names, network shares included.
//!
//! Known deviations from iscc-sdk 0.x, all where it crashes or misreads: only the global section
//! of the ffmetadata output counts (iscc-sdk fails on the `[CHAPTER]` sections of a file with
//! chapters), a value spanning lines is read whole (iscc-sdk fails or takes the next line as a
//! key), and escapes are undone in one pass (iscc-sdk turns a literal `\n` into a line break). A
//! file without a video stream inspects without a Content-Code (iscc-sdk raises).

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{bail, Result};
use image::RgbImage;

use crate::asset::{Asset, AssetContent};
use crate::formats::{self, Format};
use crate::metadata::Embedded;
use crate::thumbnail::PREVIEW_EDGE;
use crate::tools::{self, Cancelled};

/// Frames per second the signature samples (iscc-sdk `video_fps`).
const FPS: u32 = 5;
/// Values in one MPEG-7 frame signature, each 0, 1 or 2.
pub const SIGNATURE_LEN: usize = 380;
/// Name of the signature file in the folder ffmpeg runs in, so no path needs filter escaping.
const SIGNATURE_FILE: &str = "signature.bin";
/// Bytes of ffmpeg's error output kept for error messages.
const STDERR_TAIL: usize = 8192;
/// Bytes of ffmpeg's log kept from its start, where it describes the input.
const LOG_HEAD: usize = 65536;

/// iscc-sdk's `VIDEO_META_MAP`, field by field in its order: the first filled key wins.
const NAME_KEYS: [&str; 5] = ["iscc_name", "title", "track", "show", "album"];
const DESCRIPTION_KEYS: [&str; 4] = ["iscc_description", "description", "synopsis", "comment"];
const META_KEYS: [&str; 1] = ["iscc_meta"];
const CREATOR_KEYS: [&str; 4] = ["author", "composer", "artist", "album_artist"];

/// The analysed video as far as the ISCC needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    /// Distinct MPEG-7 frame signatures, sorted. The Content-Code Video sums distinct
    /// signatures, so repeated frames add nothing. Empty when there are no frames.
    pub signatures: Vec<Vec<i32>>,
    /// Frames the signature filter wrote, repeated ones included.
    pub frames: usize,
    /// Duration of the input as ffmpeg reports it; display only.
    pub seconds: Option<f64>,
    /// Frame size after rotation; (0, 0) when ffmpeg names none.
    pub width: u32,
    pub height: u32,
    /// The signatures are those of the source of this signed copy, whose compressed video the
    /// copy carries unchanged ([`read_copy`]); false when this file was decoded.
    pub from_source: bool,
    /// [`stream_fingerprint`] of the file the signatures were computed from, taken alongside;
    /// `None` without a video stream or when ffmpeg could not list the packets.
    pub fingerprint: Option<blake3::Hash>,
}

/// Hears how far a video analysis got, as the share of the duration decoded (`None` when the
/// duration is unknown), and stops it by returning false.
pub type Progress<'a> = &'a dyn Fn(Option<f64>) -> bool;

/// Read the video at `path` with the ffmpeg at `ffmpeg`: signatures, tags and a preview frame.
pub fn read(ffmpeg: &Path, path: &Path, progress: Progress) -> Result<Asset> {
    let input = std::path::absolute(path)?;
    let probe = probe(ffmpeg, &input)?;
    let start = probe.seconds.map(|_| 0.0);
    if !progress(start) {
        return Err(Cancelled.into());
    }
    let (preview, frames, fingerprint) = if probe.has_video {
        let preview = thumbnail(ffmpeg, &input);
        let (frames, fingerprint) =
            frames_and_fingerprint(ffmpeg, &input, probe.seconds, progress)?;
        (preview, frames, fingerprint)
    } else {
        (None, Vec::new(), None)
    };
    let count = frames.len();
    let distinct: BTreeSet<[u8; SIGNATURE_LEN]> = frames.into_iter().collect();
    Ok(Asset {
        content: AssetContent::Video(Video {
            signatures: distinct
                .iter()
                .map(|f| f.iter().map(|&v| i32::from(v)).collect())
                .collect(),
            frames: count,
            seconds: probe.seconds,
            width: probe.width,
            height: probe.height,
            from_source: false,
            fingerprint,
        }),
        preview,
        metadata: probe.metadata,
        sign_block: None,
        sign_warning: None,
    })
}

/// [`frame_signatures`], with the [`stream_fingerprint`] of the same file taken alongside
/// (`None` when that fails). A failed or stopped signature pass stops the fingerprint too.
fn frames_and_fingerprint(
    ffmpeg: &Path,
    input: &Path,
    seconds: Option<f64>,
    progress: Progress,
) -> Result<(Vec<[u8; SIGNATURE_LEN]>, Option<blake3::Hash>)> {
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let fingerprint = scope.spawn(|| stream_fingerprint(ffmpeg, input, &stop).ok());
        let frames = frame_signatures(ffmpeg, input, seconds, progress);
        stop.store(frames.is_err(), Ordering::Relaxed);
        let fingerprint = fingerprint.join().ok().flatten();
        Ok((frames?, fingerprint))
    })
}

/// Read the signed copy at `path` of the video `source`. When the copy's stream fingerprint
/// equals the one taken while `source` was analysed, the copy decodes to the frames analysed
/// then: its frame signatures and preview are the source's, and only the copy's tags and facts
/// are read. Otherwise, or when `source` is no video, the copy is read in full. The comparison
/// is with the analysis, not with the source file as it is now, so a source replaced after its
/// analysis cannot lend the copy frames it does not carry.
pub fn read_copy(ffmpeg: &Path, path: &Path, source: &Asset, progress: Progress) -> Result<Asset> {
    let AssetContent::Video(video) = &source.content else {
        return read(ffmpeg, path, progress);
    };
    let copy = stream_fingerprint(ffmpeg, path, &AtomicBool::new(false)).ok();
    if copy.is_none() || copy != video.fingerprint {
        return read(ffmpeg, path, progress);
    }
    let probe = probe(ffmpeg, &std::path::absolute(path)?)?;
    Ok(Asset {
        content: AssetContent::Video(Video {
            seconds: probe.seconds,
            width: probe.width,
            height: probe.height,
            from_source: true,
            ..video.clone()
        }),
        preview: source.preview.clone(),
        metadata: probe.metadata,
        sign_block: None,
        sign_warning: None,
    })
}

/// What fixes the frames a video decodes to, short of decoding it: every compressed packet of
/// its video streams with its timestamps, and the codec setup (ffmpeg's `framemd5` of a stream
/// copy), plus the description of its video streams, rotation included. Files with equal
/// fingerprints decode to the same frames. ffmpeg reads the file at disk speed; a file without
/// a video stream has no fingerprint. Setting `stop` ends the run early with [`Cancelled`].
pub fn stream_fingerprint(ffmpeg: &Path, path: &Path, stop: &AtomicBool) -> Result<blake3::Hash> {
    let input = std::path::absolute(path)?;
    let mut child = tools::command(ffmpeg)
        .args(["-nostdin", "-nostats"])
        .args(input_args(&input))
        .args(["-map", "0:v", "-c", "copy", "-f", "framemd5", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tools::spawn_error(e, ffmpeg))?;
    let stderr = child.stderr.take().expect("stderr is piped");
    let log = std::thread::spawn(move || head(stderr, LOG_HEAD));
    let hashed = hash_packets(child.stdout.take().expect("stdout is piped"), stop);
    if hashed.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let log = log.join().unwrap_or_default();
    let mut hasher = hashed?;
    if !status?.success() {
        bail!("ffmpeg cannot list the video packets of this file");
    }
    hasher.update(video_streams(input_section(&log)).as_bytes());
    Ok(hasher.finalize())
}

/// Hash ffmpeg's `framemd5` lines, until they end or `stop` is set.
fn hash_packets(framemd5: impl Read, stop: &AtomicBool) -> Result<blake3::Hasher> {
    let mut hasher = blake3::Hasher::new();
    for line in BufReader::new(framemd5).lines() {
        if stop.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let line = line?;
        // The muxer names the ffmpeg build, which says nothing about the frames.
        if !line.starts_with("#software") {
            hasher.update(line.as_bytes()).update(b"\n");
        }
    }
    Ok(hasher)
}

/// The video streams of an input as ffmpeg's log describes them, with their side data (the
/// display matrix) and tags, but without bitrates, which ffmpeg estimates from the file size in
/// some containers (AVI).
fn video_streams(input_log: &str) -> String {
    let mut out = String::new();
    let mut in_video = false;
    for line in input_log.lines() {
        if line.trim_start().starts_with("Stream #") {
            in_video = is_video_stream(line);
        } else if !line.starts_with("    ") {
            // Lines of a stream are indented deeper than the stream line.
            in_video = false;
        }
        if in_video {
            let parts: Vec<&str> = line
                .split(", ")
                .filter(|part| !part.ends_with(" kb/s"))
                .collect();
            out.push_str(&parts.join(", "));
            out.push('\n');
        }
    }
    out
}

/// Read a video held in memory (a source view) through a temporary file with its extension.
pub fn read_bytes(ffmpeg: &Path, bytes: &[u8], format: &Format) -> Result<Asset> {
    let file = tempfile::Builder::new()
        .prefix("iscc-c2pa-demo-")
        .suffix(&format!(".{}", format.extensions[0]))
        .tempfile()?;
    std::fs::write(file.path(), bytes)?;
    read(ffmpeg, file.path(), &|_| true)
}

/// What ffmpeg tells about a video before decoding it.
#[derive(Debug, Default, PartialEq)]
struct Probe {
    metadata: Embedded,
    seconds: Option<f64>,
    has_video: bool,
    /// Frame size after rotation of the video stream ffmpeg decodes ([`frame_size`]).
    width: u32,
    height: u32,
}

/// The ffmetadata pass of iscc-sdk's `video_meta_extract_ffmpeg`, whose log also gives the
/// duration and the streams.
fn probe(ffmpeg: &Path, input: &Path) -> Result<Probe> {
    let output = run(
        ffmpeg,
        input,
        &["-movflags", "use_metadata_tags", "-f", "ffmetadata", "-"],
    )?;
    if !output.status.success() {
        bail!(
            "ffmpeg cannot read this file: {}",
            last_line(&output.stderr)
        );
    }
    let tags = parse_ffmetadata(&decode_ignoring_errors(&output.stdout));
    let log = String::from_utf8_lossy(&output.stderr);
    let input_log = input_section(&log);
    let (width, height) = frame_size(input_log).unwrap_or_default();
    Ok(Probe {
        metadata: map_tags(&tags),
        seconds: duration(input_log),
        has_video: input_log.lines().any(is_video_stream),
        width,
        height,
    })
}

/// Run ffmpeg on the file `input` with the output arguments `tail`, capturing its output.
fn run(ffmpeg: &Path, input: &Path, tail: &[&str]) -> Result<Output> {
    tools::command(ffmpeg)
        .arg("-nostdin")
        .args(input_args(input))
        .args(tail)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| tools::spawn_error(e, ffmpeg))
}

/// ffmpeg's arguments to read the file `input` with the demuxer of its format.
fn input_args(input: &Path) -> [&OsStr; 4] {
    [
        "-f".as_ref(),
        demuxer(input).as_ref(),
        "-i".as_ref(),
        input.as_os_str(),
    ]
}

/// ffmpeg's demuxer for the video at `path`, by its extension: `avi` for AVI, `mov` for MP4,
/// MOV and M4V. Left to choose, ffmpeg goes by the content and may read a crafted file as a
/// playlist (DASH, HLS) that makes it open other files, network shares included.
fn demuxer(path: &Path) -> &'static str {
    match formats::by_path(path).map(|f| f.mime) {
        Some(formats::AVI) => "avi",
        _ => "mov",
    }
}

/// A preview frame chosen by ffmpeg's `thumbnail` filter, scaled down to fit the preview first
/// (never up) so a large video does not hold 100 full frames; `None` when ffmpeg finds none. The
/// filter takes the size from the stream it gets, whichever one ffmpeg chooses to decode.
fn thumbnail(ffmpeg: &Path, input: &Path) -> Option<RgbImage> {
    let filter = format!(
        "scale='min({PREVIEW_EDGE},iw)':'min({PREVIEW_EDGE},ih)':\
        force_original_aspect_ratio=decrease,thumbnail"
    );
    let output = run(
        ffmpeg,
        input,
        &[
            "-an",
            "-sn",
            "-dn",
            "-vf",
            &filter,
            "-frames:v",
            "1",
            "-c:v",
            "png",
            "-f",
            "image2pipe",
            "-",
        ],
    )
    .ok()?;
    image::load_from_memory(&output.stdout)
        .ok()
        .map(|img| img.to_rgb8())
}

/// MPEG-7 frame signatures of the video, as iscc-sdk's `video_mp7sig_extract` makes them. The
/// filter writes into a fresh temporary folder that ffmpeg runs in. `progress` hears the share
/// of `seconds` decoded so far and can stop ffmpeg.
fn frame_signatures(
    ffmpeg: &Path,
    input: &Path,
    seconds: Option<f64>,
    progress: Progress,
) -> Result<Vec<[u8; SIGNATURE_LEN]>> {
    let dir = tempfile::Builder::new()
        .prefix("iscc-c2pa-demo-video-")
        .tempdir()?;
    let filter = format!("fps=fps={FPS},signature=format=binary:filename={SIGNATURE_FILE}");
    let mut child = tools::command(ffmpeg)
        .args(["-nostdin", "-nostats", "-progress", "pipe:1"])
        .args(input_args(input))
        .args(["-an", "-sn", "-dn", "-vf", &filter, "-f", "null", "-"])
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| tools::spawn_error(e, ffmpeg))?;
    let stderr = child.stderr.take().expect("stderr is piped");
    let log = std::thread::spawn(move || tail(stderr, STDERR_TAIL));
    let watched = watch_progress(
        child.stdout.take().expect("stdout is piped"),
        seconds,
        progress,
    );
    if watched.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let log = log.join().unwrap_or_default();
    watched?;
    if !status?.success() {
        bail!("ffmpeg failed on this video: {}", last_line(log.as_bytes()));
    }
    match std::fs::read(dir.path().join(SIGNATURE_FILE)) {
        Ok(bytes) => parse_mp7(&bytes),
        // No video frames reached the filter, so it wrote nothing.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

/// Follow ffmpeg's `-progress` reports until it closes its output, or until `progress` says
/// stop ([`Cancelled`]). A report without a time (`N/A` before the first frame) repeats the
/// share last reported, so a known duration never turns into an unknown one.
fn watch_progress(reports: impl Read, seconds: Option<f64>, progress: Progress) -> Result<()> {
    let total = seconds.filter(|s| *s > 0.0);
    let mut fraction = total.map(|_| 0.0);
    for line in BufReader::new(reports).lines() {
        let line = line?;
        let Some(us) = line.strip_prefix("out_time_us=") else {
            continue;
        };
        if let (Some(total), Ok(done)) = (total, us.trim().parse::<f64>()) {
            fraction = Some((done / 1e6 / total).clamp(0.0, 1.0));
        }
        if !progress(fraction) {
            return Err(Cancelled.into());
        }
    }
    Ok(())
}

/// The last `limit` bytes `reader` yields, as text.
fn tail(mut reader: impl Read, limit: usize) -> String {
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = reader.read(&mut buf) {
        kept.extend_from_slice(&buf[..n]);
        if kept.len() > 2 * limit {
            kept.drain(..kept.len() - limit);
        }
    }
    let start = kept.len().saturating_sub(limit);
    String::from_utf8_lossy(&kept[start..]).into_owned()
}

/// The first `limit` bytes `reader` yields, as text; the rest is read and dropped.
fn head(mut reader: impl Read, limit: usize) -> String {
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = reader.read(&mut buf) {
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buf[..n.min(room)]);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The last non-empty line of ffmpeg's log, which names the error.
fn last_line(log: &[u8]) -> String {
    String::from_utf8_lossy(log)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no reason given")
        .to_owned()
}

/// `bytes` as UTF-8 with invalid sequences dropped, as iscc-sdk decodes ffmpeg's output
/// (`errors="ignore"`).
fn decode_ignoring_errors(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace(char::REPLACEMENT_CHARACTER, "")
}

/// Global tags of ffmpeg's ffmetadata output, keys lower-cased, empty values skipped, later
/// keys replacing earlier ones, escapes undone. Reading stops at the first section (`[STREAM]`,
/// `[CHAPTER]`); a value whose line ends in an escaped line break goes on in the next line.
fn parse_ffmetadata(text: &str) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    for line in logical_lines(text) {
        if line.starts_with('[') {
            break;
        }
        if line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !value.is_empty() {
            tags.insert(key.to_lowercase(), unescape(value));
        }
    }
    tags
}

/// Lines of ffmetadata text with escaped line breaks joined back in.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending: Option<String> = None;
    for raw in text.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut line = pending.take().unwrap_or_default();
        line.push_str(raw);
        let trailing = line.chars().rev().take_while(|&c| c == '\\').count();
        if trailing % 2 == 1 {
            line.push('\n');
            pending = Some(line);
        } else {
            lines.push(line);
        }
    }
    lines.extend(pending);
    lines
}

/// Undo ffmetadata escaping: a backslash takes the next character literally.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.extend(chars.next());
        } else {
            out.push(c);
        }
    }
    out
}

/// Meta-Code fields from the tags through `VIDEO_META_MAP`, unsanitised like iscc-sdk's.
fn map_tags(tags: &HashMap<String, String>) -> Embedded {
    let pick = |keys: &[&str]| keys.iter().find_map(|k| tags.get(*k).cloned());
    Embedded {
        name: pick(&NAME_KEYS),
        description: pick(&DESCRIPTION_KEYS),
        meta: pick(&META_KEYS),
        creator: pick(&CREATOR_KEYS),
    }
}

/// Headers in ffmpeg's log that end its description of the input.
const AFTER_INPUT: [&str; 2] = ["Output #", "Stream mapping:"];

/// The part of ffmpeg's log that describes the input, before the output and stream mapping.
/// Their headers start a line; the same words in the file name or in a tag never do, as ffmpeg
/// prints the name after `Input #` and indents every tag line.
fn input_section(log: &str) -> &str {
    let end: usize = log
        .split_inclusive('\n')
        .take_while(|line| !AFTER_INPUT.iter().any(|header| line.starts_with(header)))
        .map(str::len)
        .sum();
    &log[..end]
}

/// Duration from the input's `Duration: HH:MM:SS.ss` line, which ffmpeg indents by two spaces
/// (tags by four, so a tag with these words is not taken for it); `None` for `N/A`.
fn duration(log: &str) -> Option<f64> {
    let line = log.lines().find_map(|l| l.strip_prefix("  Duration: "))?;
    let value = line.split(',').next()?.trim();
    let mut parts = value.split(':').map(|p| p.parse::<f64>().ok());
    let (h, m, s) = (parts.next()??, parts.next()??, parts.next()??);
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Whether a log line describes a video stream of the input.
fn is_video_stream(line: &str) -> bool {
    line.trim_start().starts_with("Stream #") && line.contains(": Video: ")
}

/// Size of the video stream ffmpeg decodes when left to choose, as the thumbnail and signature
/// passes leave it, rotated by its display matrix as ffmpeg's autorotation does. ffmpeg takes
/// the stream it prefers most ([`preference`]), the earlier one of two it prefers alike.
fn frame_size(log: &str) -> Option<(u32, u32)> {
    let lines: Vec<&str> = log.lines().collect();
    let (at, (w, h)) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_video_stream(line))
        .filter_map(|(at, line)| Some((at, stream_size(line)?)))
        .rev()
        .max_by_key(|&(at, size)| preference(lines[at], size))?;
    // Side data of the stream follows in more deeply indented lines, up to the next stream.
    let rotation = lines[at + 1..]
        .iter()
        .take_while(|l| !l.trim_start().starts_with("Stream #"))
        .find_map(|l| {
            let degrees = l.split("rotation of ").nth(1)?.split_whitespace().next()?;
            degrees.parse::<f64>().ok()
        });
    let quarter_turn = rotation.is_some_and(|r| (r.abs().round() as i64) % 180 == 90);
    Some(if quarter_turn { (h, w) } else { (w, h) })
}

/// How much ffmpeg prefers the video stream of `size` that `line` describes when it chooses one
/// to decode (`map_auto_video` in ffmpeg 8.1): the larger picture, the default stream before any
/// other, cover art (`attached pic`) last.
fn preference(line: &str, (w, h): (u32, u32)) -> u64 {
    if line.contains("(attached pic)") {
        return 1;
    }
    let default = if line.contains("(default)") {
        5_000_000
    } else {
        0
    };
    u64::from(w) * u64::from(h) + default
}

/// Frame size named in the log line of a video stream.
fn stream_size(line: &str) -> Option<(u32, u32)> {
    line.split(|c: char| c == ',' || c.is_whitespace())
        .find_map(size_token)
}

/// `176x144` as a size; codec tags such as `0x31637661` are not sizes.
fn size_token(token: &str) -> Option<(u32, u32)> {
    let (w, h) = token.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(w) || !digits(h) || w.starts_with('0') {
        return None;
    }
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Reads an MPEG-7 binary signature bit by bit, most significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    /// Skip `n` bits.
    fn skip(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    /// The next `n` bits (at most 32) as an unsigned number.
    fn take(&mut self, n: usize) -> Result<u32> {
        if self.pos.saturating_add(n) > self.data.len() * 8 {
            bail!("the video signature is truncated");
        }
        let mut value = 0u32;
        for i in self.pos..self.pos + n {
            let bit = (self.data[i / 8] >> (7 - i % 8)) & 1;
            value = (value << 1) | u32::from(bit);
        }
        self.pos += n;
        Ok(value)
    }
}

/// Frame signatures of ffmpeg's binary MPEG-7 video signature, as iscc-sdk's
/// `read_mp7_signature` reads them: the header and the segment signatures are skipped; each
/// frame's 76 bytes hold five base-3 digits apiece, most significant first. Media time and
/// confidence are read and dropped, as the Content-Code does not use them.
pub fn parse_mp7(data: &[u8]) -> Result<Vec<[u8; SIGNATURE_LEN]>> {
    let mut bits = Bits { data, pos: 0 };
    bits.skip(129);
    let count = bits.take(32)? as usize;
    let _media_time_unit = bits.take(16)?;
    bits.skip(1 + 32 + 32);
    let segments = bits.take(32)? as usize;
    bits.skip(segments.saturating_mul(4 * 32 + 1 + 5 * 243));
    bits.skip(1);
    let mut frames = Vec::with_capacity(count.min(data.len() / 76));
    for _ in 0..count {
        bits.skip(1);
        let _media_time = bits.take(32)?;
        let _confidence = bits.take(8)?;
        bits.skip(5 * 8);
        let mut frame = [0u8; SIGNATURE_LEN];
        for chunk in frame.as_chunks_mut::<5>().0 {
            let byte = bits.take(8)?;
            let mut div = 81;
            for digit in chunk {
                *digit = ((byte / div) % 3) as u8;
                div /= 3;
            }
        }
        frames.push(frame);
    }
    Ok(frames)
}

/// Read `path` as a video for the tests, with ffmpeg installed if missing.
#[cfg(test)]
pub(crate) fn read_fixture(path: &Path) -> Result<Asset> {
    use anyhow::Context as _;
    read(&tools::tests::ensure_ffmpeg(), path, &|_| true)
        .with_context(|| format!("cannot read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iscc::{self, Content};
    use crate::metadata::tests::sdk_meta_code;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn expected() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/expected_video.json")).unwrap()
    }

    #[test]
    fn mp7_signature_parses_like_iscc_sdk() {
        let want = &expected()["demo.mp4"]["signature"];
        let frames = parse_mp7(&std::fs::read(fixture("demo.mp4.mp7sig")).unwrap()).unwrap();
        assert_eq!(frames.len() as u64, want["frames"].as_u64().unwrap());
        let first: String = frames[0].iter().map(|d| char::from(b'0' + d)).collect();
        assert_eq!(first, want["first_frame"].as_str().unwrap());
        assert!(frames.iter().flatten().all(|&d| d <= 2));
    }

    #[test]
    fn truncated_mp7_signature_is_an_error() {
        let bytes = std::fs::read(fixture("demo.mp4.mp7sig")).unwrap();
        let error = parse_mp7(&bytes[..bytes.len() / 2]).unwrap_err();
        assert_eq!(error.to_string(), "the video signature is truncated");
        assert!(parse_mp7(&[]).is_err());
    }

    #[test]
    fn ffmetadata_is_parsed_like_iscc_sdk() {
        let text = ";FFMETADATA1\nmajor_brand=isom\nTITLE=Title with \\= and \\; and \\#\n\
                    comment=\nartist=First\nartist=Second\ndescription=one\\\ntwo\n\
                    synopsis=back\\\\slash\n[CHAPTER]\nTIMEBASE=1/1000\ntitle=Chapter one\n";
        let tags = parse_ffmetadata(text);
        assert_eq!(tags["title"], "Title with = and ; and #");
        assert!(!tags.contains_key("comment"), "empty values are skipped");
        assert_eq!(
            tags["artist"], "Second",
            "a later key replaces an earlier one"
        );
        assert_eq!(
            tags["description"], "one\ntwo",
            "an escaped line break continues"
        );
        assert_eq!(tags["synopsis"], "back\\slash");
        assert!(!tags.contains_key("timebase"), "chapters do not count");
        let crlf = parse_ffmetadata(";FFMETADATA1\r\ntitle=Windows\r\n");
        assert_eq!(crlf["title"], "Windows");
    }

    #[test]
    fn tags_map_in_video_meta_map_order() {
        let tags: HashMap<String, String> = [
            ("title", "Title"),
            ("album", "Album"),
            ("comment", "Comment"),
            ("synopsis", "Synopsis"),
            ("artist", "Artist"),
            ("composer", "Composer"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let mapped = map_tags(&tags);
        assert_eq!(mapped.name.as_deref(), Some("Title"));
        assert_eq!(mapped.description.as_deref(), Some("Synopsis"));
        assert_eq!(mapped.creator.as_deref(), Some("Composer"));
        assert_eq!(mapped.meta, None);
        // Values are not sanitised, as iscc-sdk leaves video tags alone.
        let raw: HashMap<String, String> =
            [("title".to_owned(), "  <b>Bold</b>  ".to_owned())].into();
        assert_eq!(map_tags(&raw).name.as_deref(), Some("  <b>Bold</b>  "));
    }

    #[test]
    fn log_gives_duration_streams_and_rotated_size() {
        let log = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'rotated.mp4':\n  Metadata:\n    \
            title           : Kali\n  Duration: 00:01:00.14, start: 0.000000, bitrate: 87 kb/s\n  \
            Stream #0:0[0x1](eng): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), \
            176x144 [SAR 1:1 DAR 11:9], 65 kb/s, 24 fps, 24 tbr, 12288 tbn (default)\n      \
            Display Matrix: rotation of -90.00 degrees\n  \
            Stream #0:1[0x2](eng): Audio: aac (LC) (mp4a / 0x6134706D), 22050 Hz, mono\n\
            Output #0, ffmetadata, to 'pipe:':\n  Stream #0:0: Video: wrapped_avframe, 1x1\n";
        let input = input_section(log);
        assert_eq!(duration(input), Some(60.14));
        assert!(input.lines().any(is_video_stream));
        assert_eq!(frame_size(input), Some((144, 176)));
        let unrotated = input.replace("rotation of -90.00", "rotation of 180.00");
        assert_eq!(frame_size(&unrotated), Some((176, 144)));
        assert_eq!(duration("  Duration: N/A, bitrate: N/A"), None);
        assert_eq!(size_token("0x31637661"), None);
        assert_eq!(size_token("1920x1080"), Some((1920, 1080)));
    }

    #[test]
    fn words_of_the_log_in_a_name_or_a_tag_are_not_its_structure() {
        let log = "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'Output #1 Stream mapping: x.mp4':\n  \
            Metadata:\n    title           : Output #1\n    comment         : Duration: none\n\
            \x20                   : Output #9 second line\n\
            \x20                   : Stream mapping: third\n  \
            Duration: 00:00:08.00, start: 0.000000, bitrate: 94 kb/s\n  \
            Stream #0:0[0x1](eng): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), \
            176x144 [SAR 1:1 DAR 11:9], 65 kb/s, 24 fps, 24 tbr, 12288 tbn (default)\n\
            Stream mapping:\nOutput #0, ffmetadata, to 'pipe:':\n  Metadata:\n    \
            title           : Output #1\n";
        let input = input_section(log);
        assert!(input.ends_with("(default)\n"), "{input}");
        assert_eq!(duration(input), Some(8.0));
        assert!(input.lines().any(is_video_stream));
        assert_eq!(frame_size(input), Some((176, 144)));
        assert_eq!(input_section("no headers at all"), "no headers at all");
    }

    #[test]
    fn frame_size_is_that_of_the_stream_ffmpeg_decodes() {
        let stream = |index: u32, size: &str, flags: &str| {
            format!(
                "  Stream #0:{index}[0x{index}](und): Video: mpeg4 (Simple Profile) \
                (mp4v / 0x7634706D), yuv420p, {size} [SAR 1:1 DAR 1:1], 13 kb/s, 25 fps, 25 tbr, \
                12800 tbn{flags}\n"
            )
        };
        let size_of = |streams: &[String]| frame_size(&streams.concat());
        // Stream choices checked against ffmpeg 8.1 on files with these streams.
        let default_second = [stream(0, "16x16", ""), stream(1, "1280x720", " (default)")];
        assert_eq!(size_of(&default_second), Some((1280, 720)));
        let default_first = [stream(0, "16x16", " (default)"), stream(1, "1280x720", "")];
        assert_eq!(size_of(&default_first), Some((16, 16)));
        let largest = [stream(0, "16x16", ""), stream(1, "1280x720", "")];
        assert_eq!(size_of(&largest), Some((1280, 720)));
        let tie = [stream(0, "320x240", ""), stream(1, "240x320", "")];
        assert_eq!(size_of(&tie), Some((320, 240)));
        let cover = [
            stream(0, "2000x2000", " (attached pic)"),
            stream(1, "176x144", ""),
        ];
        assert_eq!(size_of(&cover), Some((176, 144)));
        // The rotation is the chosen stream's own.
        let rotated = [
            stream(0, "16x16", ""),
            "      Display Matrix: rotation of -90.00 degrees\n".to_owned(),
            stream(1, "1280x720", " (default)"),
        ];
        assert_eq!(size_of(&rotated), Some((1280, 720)));
    }

    #[test]
    fn stream_description_keeps_what_decoding_depends_on() {
        let log = "Input #0, avi, from 'C:\\some\\where\\signed.avi':\n  \
            Duration: 00:00:08.10, start: 0.000000, bitrate: 170 kb/s\n  \
            Stream #0:0: Video: mpeg4 (Simple Profile) (FMP4 / 0x34504D46), yuv420p, 176x144 \
            [SAR 1:1 DAR 11:9], 114 kb/s, 24 fps, 24 tbr, 24 tbn\n      \
            Side data:\n        displaymatrix: rotation of -90.00 degrees\n  \
            Stream #0:1: Audio: mp3 (mp3float) (U[0][0][0] / 0x0055), 22050 Hz, mono, 24 kb/s\n";
        assert_eq!(
            video_streams(log),
            "  Stream #0:0: Video: mpeg4 (Simple Profile) (FMP4 / 0x34504D46), yuv420p, \
            176x144 [SAR 1:1 DAR 11:9], 24 fps, 24 tbr, 24 tbn\n      Side data:\n        \
            displaymatrix: rotation of -90.00 degrees\n"
        );
    }

    #[test]
    fn stream_fingerprint_tells_packets_and_rotation_apart() {
        let ffmpeg = tools::tests::ensure_ffmpeg();
        let go = AtomicBool::new(false);
        let fingerprint = |name: &str| stream_fingerprint(&ffmpeg, &fixture(name), &go);
        let demo = fingerprint("demo.mp4").unwrap();
        assert_eq!(fingerprint("demo.mp4").unwrap(), demo);
        // The same packets with a display matrix decode to other (rotated) frames.
        assert_ne!(fingerprint("rotated.mp4").unwrap(), demo);
        assert_ne!(fingerprint("demo.avi").unwrap(), demo);
        assert!(fingerprint("no-video.mp4").is_err());
        let stopped = stream_fingerprint(&ffmpeg, &fixture("demo.mp4"), &AtomicBool::new(true));
        let error = stopped.unwrap_err();
        assert!(error.downcast_ref::<Cancelled>().is_some(), "{error:#}");
    }

    #[test]
    fn analysis_keeps_the_stream_fingerprint() {
        let ffmpeg = tools::tests::ensure_ffmpeg();
        let path = fixture("demo.mp4");
        let AssetContent::Video(video) = read_fixture(&path).unwrap().content else {
            panic!("a video");
        };
        let want = stream_fingerprint(&ffmpeg, &path, &AtomicBool::new(false)).unwrap();
        assert_eq!(video.fingerprint, Some(want));
        let AssetContent::Video(silent) = read_fixture(&fixture("no-video.mp4")).unwrap().content
        else {
            panic!("a video");
        };
        assert_eq!(silent.fingerprint, None, "no video stream, no fingerprint");
    }

    #[test]
    fn a_copy_that_may_decode_to_other_frames_is_decoded() {
        let ffmpeg = tools::tests::ensure_ffmpeg();
        let source = read_fixture(&fixture("demo.mp4")).unwrap();
        let reports = std::cell::Cell::new(0);
        let copy = read_copy(&ffmpeg, &fixture("rotated.mp4"), &source, &|_| {
            reports.set(reports.get() + 1);
            true
        })
        .unwrap();
        assert!(reports.get() > 0, "decoded");
        assert!(matches!(&copy.content, AssetContent::Video(v) if !v.from_source));
        let code = iscc::content_unit(copy.content()).unwrap().iscc;
        assert_eq!(code, expected()["rotated.mp4"]["video"]);
    }

    #[test]
    fn a_crafted_playlist_is_not_followed() {
        // A DASH manifest named .mp4 whose BaseURL points at another file: left to choose,
        // ffmpeg reads it as DASH and opens that file (a network share, on Windows, by UNC).
        let dir = tempfile::tempdir().unwrap();
        let target = std::path::absolute(fixture("demo.mp4")).unwrap();
        let target = target.to_string_lossy().replace('\\', "/");
        let mpd = format!(
            "<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" \
            type=\"static\" mediaPresentationDuration=\"PT8S\" minBufferTime=\"PT1S\" \
            profiles=\"urn:mpeg:dash:profile:isoff-on-demand:2011\"><Period>\
            <AdaptationSet mimeType=\"video/mp4\"><Representation id=\"1\" bandwidth=\"1\">\
            <BaseURL>file:{target}</BaseURL></Representation></AdaptationSet></Period></MPD>\n"
        );
        let crafted = dir.path().join("crafted.mp4");
        std::fs::write(&crafted, mpd).unwrap();
        let error = read_fixture(&crafted).unwrap_err();
        assert!(
            format!("{error:#}").contains("ffmpeg cannot read"),
            "{error:#}"
        );
        assert_eq!(demuxer(&fixture("demo.avi")), "avi");
        assert_eq!(demuxer(&fixture("demo.m4v")), "mov");
    }

    #[test]
    fn progress_without_a_time_repeats_the_last_share() {
        let reports = "out_time_us=N/A\nprogress=continue\nout_time_us=4000000\n\
                       out_time_us=N/A\nout_time_us=8000000\nprogress=end\n";
        let seen = std::cell::RefCell::new(Vec::new());
        let record = |f: Option<f64>| {
            seen.borrow_mut().push(f);
            true
        };
        watch_progress(reports.as_bytes(), Some(8.0), &record).unwrap();
        assert_eq!(seen.take(), [Some(0.0), Some(0.5), Some(0.5), Some(1.0)]);
        watch_progress(reports.as_bytes(), None, &record).unwrap();
        assert_eq!(seen.take(), [None; 4], "no duration, no share");
        let error = watch_progress(reports.as_bytes(), Some(8.0), &|_| false).unwrap_err();
        assert!(error.downcast_ref::<Cancelled>().is_some(), "{error:#}");
    }

    #[test]
    fn video_units_and_metadata_match_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_video.py (iscc-sdk with ffmpeg).
        for (file, want) in expected().as_object().unwrap() {
            let path = fixture(file);
            let asset = read_fixture(&path).unwrap();
            let metadata = &asset.metadata;
            assert_eq!(
                metadata.name.as_deref(),
                want["name"].as_str(),
                "{file} name"
            );
            assert_eq!(
                metadata.description.as_deref(),
                want["description"].as_str(),
                "{file} description"
            );
            assert_eq!(
                metadata.meta.as_deref(),
                want["meta_field"].as_str(),
                "{file} meta"
            );
            assert_eq!(
                metadata.creator.as_deref(),
                want["creator"].as_str(),
                "{file} creator"
            );
            assert_eq!(
                sdk_meta_code(metadata, &path),
                want["meta"],
                "{file} Meta-Code"
            );
            let code = iscc::content_unit(asset.content()).ok().map(|u| u.iscc);
            assert_eq!(
                code.as_deref(),
                want["video"].as_str(),
                "{file} Content-Code"
            );
            let bytes = std::fs::read(&path).unwrap();
            let [data, instance] = iscc::bitstream_units(&bytes).unwrap();
            assert_eq!(data.iscc, want["data"], "{file} Data-Code");
            assert_eq!(instance.iscc, want["instance"], "{file} Instance-Code");
        }
    }

    #[test]
    fn video_facts_and_preview() {
        let AssetContent::Video(video) = read_fixture(&fixture("demo.mp4")).unwrap().content else {
            panic!("a video");
        };
        assert_eq!(video.frames, 40, "eight seconds at five frames per second");
        assert!(video.signatures.len() <= video.frames);
        assert!((video.seconds.unwrap() - 8.0).abs() < 0.1);
        assert_eq!((video.width, video.height), (176, 144));

        let rotated = read_fixture(&fixture("rotated.mp4")).unwrap();
        let preview = rotated.preview.as_ref().expect("a preview frame");
        assert_eq!(
            preview.dimensions(),
            (144, 176),
            "rotated like ffmpeg plays it"
        );
        let AssetContent::Video(video) = &rotated.content else {
            panic!("a video");
        };
        assert_eq!((video.width, video.height), (144, 176));
    }

    #[test]
    fn video_without_video_stream_has_no_content_code() {
        let asset = read_fixture(&fixture("no-video.mp4")).unwrap();
        assert!(asset.preview.is_none());
        let AssetContent::Video(video) = &asset.content else {
            panic!("a video");
        };
        assert!(video.signatures.is_empty());
        assert!(video.seconds.is_some());
        let error = iscc::content_unit(asset.content()).unwrap_err().to_string();
        assert!(error.contains("no video"), "{error}");
        assert!(matches!(asset.content(), Content::Video([])));
    }

    #[test]
    fn a_file_that_is_no_video_is_an_error() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-no-video");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.mp4");
        std::fs::write(&path, [0x13u8, 0x37, 0x00, 0xff].repeat(64)).unwrap();
        let error = read_fixture(&path).unwrap_err();
        assert!(
            format!("{error:#}").contains("ffmpeg cannot read"),
            "{error:#}"
        );
    }

    #[test]
    fn cancelling_stops_a_running_analysis() {
        let ffmpeg = tools::tests::ensure_ffmpeg();
        let path = fixture("demo.mp4");
        let reports = std::cell::Cell::new(0);
        let error = read(&ffmpeg, &path, &|_| {
            reports.set(reports.get() + 1);
            reports.get() < 2
        })
        .unwrap_err();
        assert!(error.downcast_ref::<Cancelled>().is_some(), "{error:#}");
        assert_eq!(
            reports.get(),
            2,
            "stopped at the first report of the signature pass"
        );
        let at_start = read(&ffmpeg, &path, &|_| false).unwrap_err();
        assert!(
            at_start.downcast_ref::<Cancelled>().is_some(),
            "{at_start:#}"
        );
    }

    #[test]
    fn progress_rises_to_the_end() {
        let fractions = std::cell::RefCell::new(Vec::new());
        read(&tools::tests::ensure_ffmpeg(), &fixture("demo.avi"), &|f| {
            fractions
                .borrow_mut()
                .push(f.expect("the duration is known"));
            true
        })
        .unwrap();
        let fractions = fractions.into_inner();
        assert!(!fractions.is_empty());
        assert!(fractions.windows(2).all(|w| w[0] <= w[1]), "{fractions:?}");
        assert!(*fractions.last().unwrap() > 0.9, "{fractions:?}");
    }
}
