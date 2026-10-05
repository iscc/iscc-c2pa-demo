//! External tools installed on first use, never bundled: ffmpeg, which computes the MPEG-7 video
//! signatures behind the Content-Code Video, and the semantic models behind the Semantic-Codes.
//! The app and the CLI ask first, then download each file, check its size and BLAKE3 hash and put
//! it into the tools folder.
//!
//! ffmpeg is the build iscc-sdk uses for the platform, from iscc-binaries, extracted from its zip
//! archive. It is GPL software and runs as a separate program, so the app stays Apache-2.0. There
//! is no PATH lookup: another ffmpeg build may lack the signature filter or compute other codes.
//! Without ffmpeg, and on platforms without a build in iscc-binaries (Linux and Windows on ARM),
//! a video inspects and signs without its Meta-Code and Content-Code.
//!
//! The semantic models are weight-compressed copies of the iscc-sci and iscc-sct models with
//! iscc-sct's tokenizer, plain files in this repository's `models-v1` release, the same on every
//! platform. Each kind of Semantic-Code has its own files: Semantic-Code Image the image model,
//! Semantic-Code Text the text model and the tokenizer.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context as _, Result};
use serde::Serialize;

use crate::semantic::SemanticKind;

/// Tauri app identifier; the tools folder lives in the app's local data folder.
const APP_ID: &str = "codes.iscc.c2pa-demo";
/// Download URL of `file` in the release of iscc-binaries that holds the builds iscc-sdk pins.
macro_rules! release_url {
    ($file:literal) => {
        concat!(
            "https://github.com/iscc/iscc-binaries/releases/download/v1.0.0/",
            $file
        )
    };
}
/// Longest wait for the connection and for the response headers.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest wait for one whole download: the largest file (140 MB) at 40 KB/s. ureq has no per-read
/// timeout.
const BODY_TIMEOUT: Duration = Duration::from_secs(3600);
/// Read size while downloading and hashing.
const CHUNK: usize = 1 << 20;
/// errno of a program built for a CPU the system cannot run (macOS `EBADARCH`).
const BAD_CPU_TYPE: i32 = 86;
/// `CREATE_NO_WINDOW`: no console window flashes up when the app starts a console program.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Download URL of `file` in this repository's release of the semantic models.
macro_rules! models_url {
    ($file:literal) => {
        concat!(
            "https://github.com/iscc/iscc-c2pa-demo/releases/download/models-v1/",
            $file
        )
    };
}

/// A program or data file downloaded on first use.
#[derive(Debug, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub version: &'static str,
    /// Download URL of the zip archive, or of the file itself.
    pub url: &'static str,
    /// BLAKE3 hash of the download, hex.
    pub blake3: &'static str,
    /// Archive member that is the program; `None` when the download is the file itself.
    pub member: Option<&'static str>,
    /// Size of the download in bytes.
    pub bytes: u64,
}

impl Tool {
    /// File name of the installed file: name and version, so another version never stands in.
    fn file_name(&self) -> String {
        let ext = Path::new(self.member.unwrap_or(self.url))
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        format!("{}-{}{ext}", self.name, self.version)
    }
}

const FFMPEG_WINDOWS: Tool = Tool {
    name: "ffmpeg",
    version: "8.1",
    url: release_url!("ffmpeg-8.1-win-64.zip"),
    blake3: "d84c72395b9f52cf34c516fdfab83edfa631165b32ab1674ed0f4686989e1126",
    member: Some("ffmpeg.exe"),
    bytes: 72_252_640,
};
const FFMPEG_LINUX: Tool = Tool {
    name: "ffmpeg",
    version: "8.1",
    url: release_url!("ffmpeg-8.1-linux-64.zip"),
    blake3: "9a49dc5c1d7720acee5e269565f2674d8bb6a08fa9b428cfb173c5ff22188b45",
    member: Some("ffmpeg"),
    bytes: 73_269_447,
};
/// x86_64 only; Macs with Apple chips run it under Rosetta 2.
const FFMPEG_MACOS: Tool = Tool {
    name: "ffmpeg",
    version: "8.1",
    url: release_url!("ffmpeg-8.1-macos-64.zip"),
    blake3: "abc4ddf4f0fa0273ab635cde87cbaa02b71caa0fb77cd93a29e6945a8c17758d",
    member: Some("ffmpeg"),
    bytes: 25_927_146,
};

/// The image model of iscc-sci, its weights stored as fp16 (codes at most 1 of 256 bits off).
pub const SCI_MODEL: Tool = Tool {
    name: "iscc-sci",
    version: "v0.1.0-w16",
    url: models_url!("iscc-sci-v0.1.0-w16.onnx"),
    blake3: "9f24dd3d0440eaa07adf121b21a1cbd8518fe36048b9d8a72b606d8a6416e8a3",
    member: None,
    bytes: 104_790_555,
};
/// The text model of iscc-sct, its word embeddings stored as int8 and the other weights as fp16
/// (codes at most 2 of 256 bits off).
pub const SCT_MODEL: Tool = Tool {
    name: "iscc-sct",
    version: "v0.1.0-emb8-w16",
    url: models_url!("iscc-sct-v0.1.0-emb8-w16.onnx"),
    blake3: "e04671ff5ff9cc325400dd73318af9919322a367986f03a199c769f8143a4679",
    member: None,
    bytes: 140_304_326,
};
/// iscc-sct's tokenizer, unchanged from its tag v0.2.2.
pub const SCT_TOKENIZER: Tool = Tool {
    name: "iscc-sct-tokenizer",
    version: "v0.2.2",
    url: models_url!("tokenizer.json"),
    blake3: "bb64a20ac363c0669830ae3a084406ed2ee1dcaaff3675e07fbc7090e34df060",
    member: None,
    bytes: 9_081_590,
};
/// Everything the Semantic-Codes need.
pub const SEMANTIC_MODELS: [&Tool; 3] = [&SCI_MODEL, &SCT_MODEL, &SCT_TOKENIZER];
/// What Semantic-Code Image needs.
const IMAGE_MODEL_FILES: [&Tool; 1] = [&SCI_MODEL];
/// What Semantic-Code Text needs: the model, then the tokenizer.
const TEXT_MODEL_FILES: [&Tool; 2] = [&SCT_MODEL, &SCT_TOKENIZER];
/// The release that holds the semantic models, with their sources and licences.
pub const MODELS_RELEASE: &str = "https://github.com/iscc/iscc-c2pa-demo/releases/tag/models-v1";
/// Licences of the semantic models: the ISC21 descriptor is MIT, the rest Apache-2.0.
pub const MODELS_LICENCE: &str = "MIT and Apache-2.0";

/// Licence of ffmpeg's builds with the signature filter.
pub const FFMPEG_LICENCE: &str = "GPL-2.0-or-later";
/// Caveat for Macs with Apple chips, which run the x86_64 build under Rosetta 2.
const ROSETTA_NOTE: &str = "Runs under Rosetta 2 on Macs with Apple chips.";
const ROSETTA_MISSING: &str = "ffmpeg needs Rosetta 2 on Macs with Apple chips. Install it \
    with `softwareupdate --install-rosetta --agree-to-license` in Terminal, then try again.";

/// ffmpeg for this platform; `None` where iscc-binaries has no build.
pub fn ffmpeg() -> Option<&'static Tool> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some(&FFMPEG_WINDOWS)
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(&FFMPEG_LINUX)
    } else if cfg!(target_os = "macos") {
        Some(&FFMPEG_MACOS)
    } else {
        None
    }
}

/// A tool that a unit needs is not installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// ffmpeg, which a video needs; `available` tells whether this platform has a build to
    /// install.
    Ffmpeg { available: bool },
    /// The model of a kind of Semantic-Code.
    SemanticModel(SemanticKind),
}

impl Missing {
    /// What is missing, as the reason a unit is not there: a video's Meta-Code and Content-Code,
    /// or a Semantic-Code.
    pub fn reason(&self) -> &'static str {
        match self {
            Missing::Ffmpeg { available: true } => "video needs ffmpeg, which is not installed yet",
            Missing::Ffmpeg { available: false } => {
                "video needs ffmpeg, and there is no build of it for this platform"
            }
            Missing::SemanticModel(SemanticKind::Image) => {
                "the Semantic-Code Image needs its model, which is not installed yet"
            }
            Missing::SemanticModel(SemanticKind::Text) => {
                "the Semantic-Code Text needs its model, which is not installed yet"
            }
        }
    }
}

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.reason())
    }
}

impl std::error::Error for Missing {}

/// The user stopped a download or an analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// The app's local data folder (Tauri's `app_local_data_dir`), which holds the tools and the
/// settings. Local, not roaming, on Windows: no profile sync of 200 MB.
pub fn app_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().ok_or_else(|| anyhow!("no local data folder"))?;
    Ok(base.join(APP_ID))
}

/// Folder the tools are installed in, shared by the app and the CLI.
pub fn tools_dir() -> Result<PathBuf> {
    Ok(app_dir()?.join("tools"))
}

/// Path of `tool` in `dir`, if it is installed there.
pub fn installed(tool: &Tool, dir: &Path) -> Option<PathBuf> {
    let path = dir.join(tool.file_name());
    path.is_file().then_some(path)
}

/// The error for ffmpeg not being installed; `available` as in [`Missing::Ffmpeg`].
fn ffmpeg_missing(available: bool) -> anyhow::Error {
    anyhow::Error::new(Missing::Ffmpeg { available })
}

/// The installed ffmpeg, or a [`Missing`] error.
pub fn ffmpeg_path() -> Result<PathBuf> {
    let tool = ffmpeg().ok_or_else(|| ffmpeg_missing(false))?;
    installed(tool, &tools_dir()?).ok_or_else(|| ffmpeg_missing(true))
}

/// Install ffmpeg into the tools folder unless it is there already, and return its path; a
/// [`Missing`] error where there is no build for this platform. `progress` as for [`install`].
pub fn install_ffmpeg(progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<PathBuf> {
    let tool = ffmpeg().ok_or_else(|| ffmpeg_missing(false))?;
    install(tool, &tools_dir()?, progress)
}

/// The files the Semantic-Code `kind` needs: the image model, or the text model and its
/// tokenizer.
pub fn semantic_files(kind: SemanticKind) -> &'static [&'static Tool] {
    match kind {
        SemanticKind::Image => &IMAGE_MODEL_FILES,
        SemanticKind::Text => &TEXT_MODEL_FILES,
    }
}

/// The installed files of the Semantic-Code `kind`, in the order of [`semantic_files`], or a
/// [`Missing::SemanticModel`] error unless all of them are there.
pub fn semantic_paths(kind: SemanticKind) -> Result<Vec<PathBuf>> {
    let dir = tools_dir()?;
    semantic_files(kind)
        .iter()
        .map(|tool| {
            installed(tool, &dir).ok_or_else(|| anyhow::Error::new(Missing::SemanticModel(kind)))
        })
        .collect()
}

/// Whether every file of the Semantic-Code `kind` is installed.
pub fn semantic_installed(kind: SemanticKind) -> bool {
    semantic_paths(kind).is_ok()
}

/// Install the files of the Semantic-Code `kind` that are not installed yet, one after the
/// other, and return their paths. `progress` gets the bytes received of all of them and their
/// total size, and stops the download by returning false; files installed before stay.
pub fn install_semantic(
    kind: SemanticKind,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<Vec<PathBuf>> {
    let dir = tools_dir()?;
    let total = semantic_bytes(kind);
    let mut done = 0;
    for tool in semantic_files(kind) {
        install(tool, &dir, &mut |received, _| {
            progress(done + received, total)
        })?;
        done += tool.bytes;
    }
    semantic_paths(kind)
}

/// Size of the downloads of the Semantic-Code `kind` together.
fn semantic_bytes(kind: SemanticKind) -> u64 {
    semantic_files(kind).iter().map(|t| t.bytes).sum()
}

/// What the UI and the CLI tell about a tool before and after installing it.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct Status {
    pub name: &'static str,
    pub version: Option<&'static str>,
    /// Whether this platform has a build to install.
    pub available: bool,
    pub installed: bool,
    /// The installed program, or where it will be installed.
    pub path: Option<String>,
    pub url: Option<&'static str>,
    /// Size of the download in bytes.
    pub bytes: Option<u64>,
    pub licence: &'static str,
    /// Caveat for this platform (Rosetta 2 on Macs with Apple chips).
    pub note: Option<&'static str>,
}

/// Whether ffmpeg has a build for this platform, whether it is installed, and where.
pub fn ffmpeg_status() -> Status {
    let tool = ffmpeg();
    let dir = tools_dir().ok();
    let path = tool.zip(dir.as_ref()).map(|(t, d)| d.join(t.file_name()));
    Status {
        name: "ffmpeg",
        version: tool.map(|t| t.version),
        available: tool.is_some(),
        installed: path.as_ref().is_some_and(|p| p.is_file()),
        path: path.map(|p| p.to_string_lossy().into_owned()),
        url: tool.map(|t| t.url),
        bytes: tool.map(|t| t.bytes),
        licence: FFMPEG_LICENCE,
        note: (cfg!(all(target_os = "macos", target_arch = "aarch64"))).then_some(ROSETTA_NOTE),
    }
}

/// Whether the model of the Semantic-Code `kind` is installed, and what installing it means.
/// `path` is the tools folder it goes into.
pub fn semantic_status(kind: SemanticKind) -> Status {
    let model = semantic_files(kind)[0];
    Status {
        name: model.name,
        version: Some(model.version),
        available: true,
        installed: semantic_installed(kind),
        path: tools_dir().ok().map(|d| d.to_string_lossy().into_owned()),
        url: Some(MODELS_RELEASE),
        bytes: Some(semantic_bytes(kind)),
        licence: MODELS_LICENCE,
        note: None,
    }
}

/// Serialises installs within the process, so concurrent requests wait for one download.
static INSTALLING: Mutex<()> = Mutex::new(());

/// Install `tool` into `dir` unless it is there already, and return its path. `progress` gets
/// the bytes received and the download's size, and stops the download by returning false. The
/// download is checked against the tool's size and BLAKE3 hash before anything is extracted or
/// put in place; any failure removes the partial files.
pub fn install(
    tool: &Tool,
    dir: &Path,
    progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<PathBuf> {
    let _guard = INSTALLING.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(path) = installed(tool, dir) {
        return Ok(path);
    }
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let unique = unique_suffix();
    let archive = dir.join(format!("{}.zip.{unique}.part", tool.file_name()));
    let program = dir.join(format!("{}.{unique}.part", tool.file_name()));
    let unpacked = match tool.member {
        Some(member) => {
            download(tool, &archive, progress).and_then(|()| extract(member, &archive, &program))
        }
        None => download(tool, &program, progress),
    };
    let result = unpacked.and_then(|()| put_in_place(&program, &dir.join(tool.file_name())));
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_file(&program);
    result
}

/// A suffix no other install, in this or another process, uses at the same time.
fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos}", std::process::id())
}

/// Download the archive or file of `tool` to `path`, checking its size and BLAKE3 hash.
fn download(tool: &Tool, path: &Path, progress: &mut dyn FnMut(u64, u64) -> bool) -> Result<()> {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .build();
    let response = ureq::Agent::new_with_config(config)
        .get(tool.url)
        .call()
        .map_err(|e| anyhow!("cannot download {}: {e}", tool.name))?;
    let mut body = response.into_body().into_reader();
    let mut file =
        File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut received = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = body
            .read(&mut buf)
            .map_err(|e| anyhow!("the download of {} broke off: {e}", tool.name))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > tool.bytes {
            bail!("the download of {} is larger than expected", tool.name);
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        if !progress(received, tool.bytes) {
            return Err(Cancelled.into());
        }
    }
    file.sync_all()?;
    if hasher.finalize().to_hex().as_str() != tool.blake3 {
        bail!("the download of {} failed its integrity check", tool.name);
    }
    Ok(())
}

/// Extract the program `member` from the archive at `archive` to `path`, executable on Unix.
fn extract(member: &str, archive: &Path, path: &Path) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(archive)?)?;
    let mut member = zip
        .by_name(member)
        .with_context(|| format!("the archive holds no {member}"))?;
    let mut out = File::create(path)?;
    io::copy(&mut member, &mut out)?;
    out.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Move the extracted program or the downloaded file to `target`. Another process may have installed it meanwhile and
/// be running it, which keeps Windows from replacing it; its copy is just as good.
fn put_in_place(program: &Path, target: &Path) -> Result<PathBuf> {
    match fs::rename(program, target) {
        Ok(()) => Ok(target.to_owned()),
        Err(_) if target.is_file() => Ok(target.to_owned()),
        Err(e) => Err(anyhow!(e).context(format!("cannot install {}", target.display()))),
    }
}

/// A command for `program` that opens no console window on Windows.
pub fn command(program: &Path) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Why `program` did not start, in words the user can act on: the x86_64 builds need Rosetta 2
/// on Macs with Apple chips.
pub fn spawn_error(error: io::Error, program: &Path) -> anyhow::Error {
    if cfg!(target_os = "macos") && error.raw_os_error() == Some(BAD_CPU_TYPE) {
        return anyhow!(ROSETTA_MISSING);
    }
    anyhow!(error).context(format!("cannot start {}", program.display()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use std::net::TcpListener;

    /// Install ffmpeg into the user's tools folder if it is missing, for the tests that run it.
    /// The first run on a machine downloads it (72 MB on Windows, 26 MB on macOS).
    pub(crate) fn ensure_ffmpeg() -> PathBuf {
        static ENSURED: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        ENSURED
            .get_or_init(|| install_ffmpeg(&mut |_, _| true).unwrap())
            .clone()
    }

    /// Install the models of both Semantic-Codes into the user's tools folder if they are
    /// missing, for the tests that run them. The first run on a machine downloads them (254 MB).
    pub(crate) fn ensure_semantic() {
        static ENSURED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ENSURED.get_or_init(|| {
            for kind in SemanticKind::ALL {
                install_semantic(kind, &mut |_, _| true).unwrap();
            }
        });
    }

    /// A fresh, empty folder for one test.
    fn fresh_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A zip archive with one member `name` holding `content`.
    fn zip_with(name: &str, content: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content).unwrap();
        zip.finish().unwrap().into_inner()
    }

    /// Read the request headers; a GET has no body. Closing a socket with unread data resets
    /// the connection on Windows.
    fn read_headers(stream: &mut impl Read) {
        let mut request = Vec::new();
        let mut buf = [0u8; 1024];
        while let Ok(n @ 1..) = stream.read(&mut buf) {
            request.extend_from_slice(&buf[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                return;
            }
        }
    }

    /// Local server answering every request with `body`, announced as `length` bytes; a shorter
    /// body breaks the connection off.
    fn serve(body: Vec<u8>, length: usize) -> &'static str {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/tool.zip", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                read_headers(&mut stream);
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\n\r\n");
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        Box::leak(url.into_boxed_str())
    }

    /// A tool served locally as `archive`, expected to hash to `blake3`.
    fn local_tool(archive: &[u8], blake3: &str, length: usize) -> Tool {
        Tool {
            name: "demo-tool",
            version: "1.0",
            url: serve(archive.to_vec(), length),
            blake3: Box::leak(blake3.to_owned().into_boxed_str()),
            member: Some("demo-tool.exe"),
            bytes: archive.len() as u64,
        }
    }

    #[test]
    fn install_checks_and_extracts_the_archive() {
        let archive = zip_with("demo-tool.exe", b"#!/bin/sh\necho demo\n");
        let hash = blake3::hash(&archive).to_hex().to_string();
        let tool = local_tool(&archive, &hash, archive.len());
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-install");
        assert_eq!(installed(&tool, &dir), None);

        let mut reports = Vec::new();
        let path = install(&tool, &dir, &mut |got, total| {
            reports.push((got, total));
            true
        })
        .unwrap();
        assert_eq!(path, dir.join("demo-tool-1.0.exe"));
        assert_eq!(fs::read(&path).unwrap(), b"#!/bin/sh\necho demo\n");
        assert_eq!(reports.last(), Some(&(tool.bytes, tool.bytes)));
        assert_eq!(installed(&tool, &dir), Some(path.clone()));
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "only the program stays");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "executable");
        }
        // Installed already: no second download.
        let again = install(&tool, &dir, &mut |_, _| panic!("downloaded again")).unwrap();
        assert_eq!(again, path);
    }

    #[test]
    fn install_puts_a_plain_file_in_place() {
        let content = b"{\"model\": \"weights\"}";
        let hash = blake3::hash(content).to_hex().to_string();
        let tool = Tool {
            name: "demo-model",
            version: "v1",
            member: None,
            ..local_tool(content, &hash, content.len())
        };
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-plain");
        let path = install(&tool, &dir, &mut |_, _| true).unwrap();
        // The extension comes from the URL, which ends in tool.zip for the local server.
        assert_eq!(path, dir.join("demo-model-v1.zip"));
        assert_eq!(fs::read(&path).unwrap(), content);
        let leftovers: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "only the file stays");

        let wrong = Tool {
            name: "other-model",
            blake3: Box::leak("0".repeat(64).into_boxed_str()),
            ..tool
        };
        let error = install(&wrong, &dir, &mut |_, _| true).unwrap_err();
        assert!(error.to_string().contains("integrity check"), "{error}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "nothing added");
    }

    #[test]
    fn install_rejects_an_archive_with_the_wrong_hash() {
        let archive = zip_with("demo-tool.exe", b"tampered");
        let tool = local_tool(&archive, &"0".repeat(64), archive.len());
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-hash");
        let error = install(&tool, &dir, &mut |_, _| true).unwrap_err();
        assert!(error.to_string().contains("integrity check"), "{error}");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "partial files removed"
        );
    }

    #[test]
    fn install_fails_when_the_connection_breaks_off() {
        let archive = zip_with("demo-tool.exe", b"whole program");
        let hash = blake3::hash(&archive).to_hex().to_string();
        // The server announces more bytes than it sends, then closes.
        let tool = local_tool(&archive[..archive.len() / 2], &hash, archive.len());
        let tool = Tool {
            bytes: archive.len() as u64,
            ..tool
        };
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-broken");
        let error = install(&tool, &dir, &mut |_, _| true).unwrap_err();
        assert!(error.to_string().contains("broke off"), "{error}");
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "partial files removed"
        );
    }

    #[test]
    fn install_refuses_a_download_larger_than_announced() {
        let archive = zip_with("demo-tool.exe", b"a larger program than expected");
        let hash = blake3::hash(&archive).to_hex().to_string();
        let tool = Tool {
            bytes: 10,
            ..local_tool(&archive, &hash, archive.len())
        };
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-large");
        let error = install(&tool, &dir, &mut |_, _| true).unwrap_err();
        assert!(
            error.to_string().contains("larger than expected"),
            "{error}"
        );
    }

    #[test]
    fn install_stops_when_progress_says_so() {
        let archive = zip_with("demo-tool.exe", b"program");
        let hash = blake3::hash(&archive).to_hex().to_string();
        let tool = local_tool(&archive, &hash, archive.len());
        let dir = fresh_dir("iscc-c2pa-demo-test-tools-cancel");
        let error = install(&tool, &dir, &mut |_, _| false).unwrap_err();
        assert!(error.downcast_ref::<Cancelled>().is_some(), "{error}");
        assert_eq!(installed(&tool, &dir), None);
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "partial files removed"
        );
    }

    #[test]
    fn every_platform_build_comes_from_the_pinned_release() {
        for tool in [&FFMPEG_WINDOWS, &FFMPEG_LINUX, &FFMPEG_MACOS] {
            assert!(tool.url.starts_with(release_url!("")), "{}", tool.url);
            assert!(tool.url.ends_with(".zip"));
            assert_eq!(tool.blake3.len(), 64);
            assert!(tool.bytes > 20_000_000);
        }
        assert_eq!(FFMPEG_WINDOWS.file_name(), "ffmpeg-8.1.exe");
        assert_eq!(FFMPEG_LINUX.file_name(), "ffmpeg-8.1");
        let status = ffmpeg_status();
        assert_eq!(status.available, ffmpeg().is_some());
        assert_eq!(status.licence, "GPL-2.0-or-later");
    }

    #[test]
    fn semantic_models_come_from_the_models_release() {
        for tool in SEMANTIC_MODELS {
            assert!(tool.url.starts_with(models_url!("")), "{}", tool.url);
            assert_eq!(tool.member, None, "plain files");
            assert_eq!(tool.blake3.len(), 64);
        }
        assert_eq!(SCI_MODEL.file_name(), "iscc-sci-v0.1.0-w16.onnx");
        assert_eq!(SCT_MODEL.file_name(), "iscc-sct-v0.1.0-emb8-w16.onnx");
        assert_eq!(SCT_TOKENIZER.file_name(), "iscc-sct-tokenizer-v0.2.2.json");
        assert!(MODELS_RELEASE.starts_with("https://github.com/iscc/iscc-c2pa-demo/"));
    }

    #[test]
    fn each_semantic_code_has_its_own_files() {
        assert_eq!(semantic_files(SemanticKind::Image), [&SCI_MODEL]);
        assert_eq!(
            semantic_files(SemanticKind::Text),
            [&SCT_MODEL, &SCT_TOKENIZER]
        );
        let together: Vec<_> = SemanticKind::ALL
            .iter()
            .flat_map(|k| semantic_files(*k).iter().copied())
            .collect();
        assert_eq!(together, SEMANTIC_MODELS, "each file belongs to one kind");
        let image = semantic_status(SemanticKind::Image);
        assert_eq!(
            (image.name, image.version),
            ("iscc-sci", Some("v0.1.0-w16"))
        );
        assert_eq!(image.bytes, Some(104_790_555));
        let text = semantic_status(SemanticKind::Text);
        assert_eq!(text.name, "iscc-sct");
        assert_eq!(text.bytes, Some(149_385_916));
        assert!(text.available);
        assert_eq!(text.licence, "MIT and Apache-2.0");
        for kind in SemanticKind::ALL {
            let status = semantic_status(kind);
            assert_eq!(status.installed, semantic_paths(kind).is_ok());
            assert_eq!(
                status.path,
                tools_dir().ok().map(|d| d.display().to_string())
            );
        }
        assert_eq!(app_dir().unwrap().join("tools"), tools_dir().unwrap());
    }

    #[test]
    fn missing_tool_errors_say_what_to_do() {
        let missing = Missing::Ffmpeg { available: true };
        assert_eq!(
            missing.to_string(),
            "video needs ffmpeg, which is not installed yet"
        );
        assert_eq!(missing.reason(), missing.to_string());
        let unavailable = Missing::Ffmpeg { available: false };
        assert!(unavailable.to_string().contains("no build"));
        assert_eq!(
            Missing::SemanticModel(SemanticKind::Image).to_string(),
            "the Semantic-Code Image needs its model, which is not installed yet"
        );
        assert!(Missing::SemanticModel(SemanticKind::Text)
            .reason()
            .starts_with("the Semantic-Code Text needs"));
        let error = anyhow::Error::new(missing).context("cannot read this MP4 file");
        assert_eq!(error.downcast_ref::<Missing>(), Some(&missing));
    }
}
