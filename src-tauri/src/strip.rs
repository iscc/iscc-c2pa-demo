//! A copy of a file without its embedded Content Credentials, for tests and demos of their
//! recovery. The app keeps it out of sight (an Alt-click on the verdict of the Content Credentials
//! card, the hidden CLI command `strip`): it is no tool to remove credentials.
//!
//! A PDF signed by this app's incremental update loses that update section, which gives back the
//! PDF before signing. Every other file goes through c2pa-rs's own removal. Whether the copy is the
//! file that was signed is computed, never assumed: its Instance-Code against the one the removed
//! ISCC soft binding lists.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{anyhow, ensure, Context as _, Result};
use c2pa::{Context, Reader};
use serde::Serialize;
use serde_json::Value;

use crate::context::base_settings;
use crate::formats::{self, Format};
use crate::inspect::{self, ViewReader};
use crate::{iscc, recover, sign};

/// The copy written without Content Credentials.
#[derive(Serialize, Debug)]
pub struct Stripped {
    pub output: String,
    /// Whether the copy is exactly the file that was signed: its Instance-Code equals the one the
    /// removed ISCC soft binding lists. `None` when the credentials carry no ISCC soft binding.
    pub original: Option<bool>,
}

/// Write the file at `path` without its embedded Content Credentials to the first free
/// `<stem>-stripped<ext>` next to it. The file itself stays as it is; a failure leaves no copy.
pub fn strip(path: &Path) -> Result<Stripped> {
    let format = inspect::asset_format(path)?;
    ensure!(
        sign::embeds_manifest(path),
        "this file has no embedded Content Credentials"
    );
    let reader = read(path)?;
    let output = inspect::free_sibling(path, "stripped");
    let name = output
        .file_name()
        .ok_or_else(|| anyhow!("output path has no file name"))?;
    let dir = output
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // c2pa-rs removes in place and finds the format by the extension, so the copy keeps the name.
    let staging = tempfile::Builder::new()
        .prefix("~stripping")
        .tempdir_in(dir)
        .with_context(|| format!("cannot write into {}", dir.display()))?;
    let copy = staging.path().join(name);
    write_stripped(path, format, &reader, &copy)?;
    ensure!(
        !sign::embeds_manifest(&copy),
        "Content Credentials are left in the copy of this file"
    );
    let original = is_original(&reader, &copy)?;
    std::fs::rename(&copy, &output)
        .with_context(|| format!("cannot write {}", output.display()))?;
    Ok(Stripped {
        output: output.to_string_lossy().into_owned(),
        original,
    })
}

/// The embedded manifest store of the file at `path`, read without validation (which would hash
/// the file for nothing).
fn read(path: &Path) -> Result<Reader> {
    let mut settings = base_settings();
    settings["verify"]["verify_after_reading"] = Value::Bool(false);
    let context = Context::new().with_settings(settings)?;
    Ok(Reader::from_context(context).with_file(path)?)
}

/// Write the file at `path` without its manifest store to `copy`: a PDF whose store came in an
/// update section that adds nothing else as the PDF before it, a RIFF file (WebP, WAV, AVI) as its
/// source view with the RIFF size put right (c2pa-rs 0.91 removes nothing from those), anything
/// else through c2pa-rs.
fn write_stripped(path: &Path, format: &Format, reader: &Reader, copy: &Path) -> Result<()> {
    if let Some(ranges) = pdf_before_update(path, format, reader) {
        return write_view(path, &ranges, copy);
    }
    if RIFF_MIMES.contains(&format.mime) {
        let ranges = inspect::data_hash_exclusions(reader)
            .ok_or_else(|| anyhow!("its hard binding is no data hash"))?;
        write_view(path, &ranges, copy)?;
        return shrink_riff(copy, file_len(path)? - file_len(copy)?);
    }
    std::fs::copy(path, copy).with_context(|| format!("cannot write {}", copy.display()))?;
    c2pa::jumbf_io::remove_jumbf_from_file(copy)
        .context("c2pa-rs cannot remove Content Credentials from this file")
}

/// Formats c2pa-rs embeds into a RIFF chunk.
const RIFF_MIMES: [&str; 3] = [formats::WEBP, formats::WAV, formats::AVI];

/// Write the file at `path` without the `(start, length)` ranges `exclusions` to `copy`.
fn write_view(path: &Path, exclusions: &[(u64, u64)], copy: &Path) -> Result<()> {
    let mut view = ViewReader::open(path, exclusions)?;
    let mut out = File::create(copy).with_context(|| format!("cannot write {}", copy.display()))?;
    std::io::copy(&mut view, &mut out)?;
    Ok(())
}

/// Length of the file at `path`.
fn file_len(path: &Path) -> Result<u64> {
    Ok(std::fs::metadata(path)
        .with_context(|| format!("cannot read {}", path.display()))?
        .len())
}

/// Take `removed` bytes off the size of the RIFF chunk that the file at `path` starts with.
fn shrink_riff(path: &Path, removed: u64) -> Result<()> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    let mut header = [0; 8];
    file.read_exact(&mut header)?;
    ensure!(
        &header[..4] == b"RIFF",
        "the file does not start with a RIFF chunk"
    );
    let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let size = u32::try_from(removed)
        .ok()
        .and_then(|removed| size.checked_sub(removed))
        .ok_or_else(|| anyhow!("its RIFF size is smaller than the Content Credentials"))?;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&size.to_le_bytes())?;
    Ok(())
}

/// The ranges that cut a PDF back to the revision before the update section holding its manifest
/// store, when that section adds nothing else; `None` for any other file.
fn pdf_before_update(path: &Path, format: &Format, reader: &Reader) -> Option<Vec<(u64, u64)>> {
    if format.mime != formats::PDF {
        return None;
    }
    let ranges = inspect::data_hash_exclusions(reader)?;
    let view = inspect::pdf_view_ranges(path, format, ranges.clone());
    (view != ranges).then_some(view)
}

/// Whether the file at `copy` is the one the ISCC soft binding of the active manifest was computed
/// from, by its Instance-Code; `None` without such a soft binding.
fn is_original(reader: &Reader, copy: &Path) -> Result<Option<bool>> {
    let Some(bindings) = reader
        .active_manifest()
        .and_then(|m| recover::soft_bindings_of(m).ok())
    else {
        return Ok(None);
    };
    let file = File::open(copy).with_context(|| format!("cannot read {}", copy.display()))?;
    let ([_, instance], _) = iscc::stream_units(file)?;
    Ok(recover::iscc_match(&bindings, &[instance])
        .ok()
        .map(|m| m.exact))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::recover::tests::sign_as_is;
    use crate::sign::tests::ONE_OF_EACH;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Formats whose stripped copy is the file that was signed, byte for byte.
    const GIVEN_BACK: &[&str] = &[
        "image/jpeg",
        "image/png",
        formats::WEBP,
        "image/gif",
        "text/plain",
        "text/markdown",
        formats::WAV,
        "audio/mp4",
        formats::PDF,
        "video/mp4",
        "video/quicktime",
        "video/x-m4v",
    ];

    #[test]
    fn stripping_leaves_no_credentials_and_says_when_it_gives_back_the_signed_file() {
        crate::tools::tests::ensure_ffmpeg();
        let dir = tempfile::tempdir().unwrap();
        let mut wrong = Vec::new();
        for name in ONE_OF_EACH {
            let source = dir.path().join(name);
            std::fs::copy(fixture(name), &source).unwrap();
            let signed = inspect::suggested_output(&source);
            sign_as_is(&source, &signed);
            let before = std::fs::read(&signed).unwrap();
            let stripped = strip(&signed).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            let output = PathBuf::from(&stripped.output);
            let (stem, ext) = name.rsplit_once('.').unwrap();
            assert_eq!(
                output,
                dir.path().join(format!("{stem}-signed-stripped.{ext}"))
            );
            assert_eq!(
                std::fs::read(&signed).unwrap(),
                before,
                "{name}: signed file changed"
            );
            assert!(!sign::embeds_manifest(&output), "{name}: credentials left");
            let same = std::fs::read(&output).unwrap() == std::fs::read(&source).unwrap();
            assert_eq!(stripped.original, Some(same), "{name}");
            let mime = inspect::asset_format(&source).unwrap().mime;
            if same != GIVEN_BACK.contains(&mime) {
                wrong.push(*name);
            }
        }
        assert!(wrong.is_empty(), "GIVEN_BACK is wrong for {wrong:?}");
    }

    #[test]
    fn stripping_again_writes_the_next_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let signed = dir.path().join("photo.jpg");
        sign_as_is(&fixture("no_manifest.jpg"), &signed);
        let first = strip(&signed).unwrap();
        let second = strip(&signed).unwrap();
        assert_eq!(
            PathBuf::from(first.output),
            dir.path().join("photo-stripped.jpg")
        );
        assert_eq!(
            PathBuf::from(second.output),
            dir.path().join("photo-stripped-2.jpg")
        );
    }

    #[test]
    fn a_file_without_embedded_credentials_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let error = strip(&fixture("no_manifest.jpg")).unwrap_err().to_string();
        assert_eq!(error, "this file has no embedded Content Credentials");
        // Credentials in a sidecar are not in the file.
        let source = dir.path().join("photo.jpg");
        std::fs::copy(fixture("no_manifest.jpg"), &source).unwrap();
        let mut req = sign::tests::request("no_manifest.jpg", &dir.path().join("unused.jpg"));
        req.source = source.to_string_lossy().into_owned();
        req.sidecar = true;
        sign::sign(&req).unwrap();
        assert!(strip(&source).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
