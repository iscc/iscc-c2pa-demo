//! Command-line interface to the same core as the desktop app: inspect and sign files, compute a
//! Meta-Code, list the supported formats, install ffmpeg for video and the semantic models for
//! the Semantic-Codes. Prints the JSON the UI consumes; errors go to stderr as one line with exit
//! code 1 (usage errors exit with 2).
//!
//! The experimental Semantic-Codes are left out unless `--semantic` asks for them (or `sign
//! --units` names them), whatever the app has switched on: the CLI never reads its settings.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{anyhow, bail, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use iscc_c2pa_demo_lib::formats::{self, Kind};
use iscc_c2pa_demo_lib::inspect::{self, Depth, Inspection};
use iscc_c2pa_demo_lib::iscc::{self, MetaInput};
use iscc_c2pa_demo_lib::semantic::{SemanticKind, SemanticKinds};
use iscc_c2pa_demo_lib::sign::{self, Credentials, SignRequest, TimestampOutcome, TrainingEntry};
use iscc_c2pa_demo_lib::{timestamp, tools};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "c2pa-iscc",
    version,
    about = "Inspect and sign C2PA manifests with ISCC soft bindings"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Print JSON on one line instead of indented.
    #[arg(long, global = true)]
    compact: bool,
    /// Leave the preview image (a base64 data URL) out of inspections.
    #[arg(long, global = true)]
    no_preview: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Show a file's Content Credentials and ISCC units.
    Inspect {
        /// File to inspect.
        file: PathBuf,
        /// Also compute the experimental Semantic-Code of an image or a text, and compare an
        /// embedded one; its model must be installed.
        #[arg(long)]
        semantic: bool,
    },
    /// Write a signed copy with an ISCC soft binding; defaults mirror the Sign tab.
    Sign(Box<SignArgs>),
    /// Compute the Meta-Code of a title, description and ISCC metadata.
    MetaCode {
        #[arg(long)]
        title: String,
        #[arg(long)]
        description: Option<String>,
        /// ISCC metadata as a data URL (`iscc:meta`).
        #[arg(long)]
        meta: Option<String>,
    },
    /// List the supported file formats.
    Formats,
    /// Manage ffmpeg, which video files need, and the semantic models, which the Semantic-Codes
    /// need; each is downloaded once, on request.
    Tools {
        #[command(subcommand)]
        action: ToolsAction,
    },
}

#[derive(Subcommand)]
enum ToolsAction {
    /// Show whether ffmpeg and the semantic models are installed, where, and what installing them
    /// downloads.
    Status,
    /// Download a tool, check it and install it for the app and the CLI: ffmpeg (GPL) from
    /// iscc-binaries, or semantic models (MIT and Apache-2.0) from this app's model release.
    Install {
        #[arg(value_enum, default_value_t = Tool::Ffmpeg)]
        tool: Tool,
    },
}

/// A tool `tools install` installs.
#[derive(Clone, Copy, ValueEnum)]
enum Tool {
    Ffmpeg,
    /// The models of both Semantic-Codes.
    Semantic,
    /// The model of the Semantic-Code Image.
    SemanticImage,
    /// The model and tokenizer of the Semantic-Code Text.
    SemanticText,
}

/// The models of both Semantic-Codes, as `tools status` and `tools install semantic` print them.
#[derive(Serialize)]
struct SemanticStatus {
    semantic_image: tools::Status,
    semantic_text: tools::Status,
}

impl SemanticStatus {
    fn now() -> Self {
        SemanticStatus {
            semantic_image: tools::semantic_status(SemanticKind::Image),
            semantic_text: tools::semantic_status(SemanticKind::Text),
        }
    }
}

/// What `tools status` prints.
#[derive(Serialize)]
struct ToolsStatus {
    ffmpeg: tools::Status,
    #[serde(flatten)]
    semantic: SemanticStatus,
}

#[derive(Args)]
struct SignArgs {
    /// File to sign; it is not modified.
    file: PathBuf,
    /// Signed copy to write [default: the first free `-signed` sibling].
    #[arg(long)]
    output: Option<PathBuf>,
    /// Title for the manifest and the Meta-Code [default: the file's own title, else the
    /// stored or manifest title, else the file name].
    #[arg(long)]
    title: Option<String>,
    /// Description for the Meta-Code [default: the one the title came with].
    #[arg(long)]
    description: Option<String>,
    /// ISCC units to embed: meta, semantic, image, text, audio or video, data, instance [default:
    /// all of them but the Semantic-Code, without a unit that cannot be computed].
    #[arg(long, value_delimiter = ',', value_parser = ["meta", "semantic", "image", "text", "audio", "video", "data", "instance"])]
    units: Option<Vec<String>>,
    /// Also embed the experimental Semantic-Code of an image or a text; its model must be
    /// installed. Naming `semantic` in --units does the same.
    #[arg(long)]
    semantic: bool,
    /// Digital source type URI, recorded on the parent ingredient when the file has no Content
    /// Credentials yet [default: none recorded].
    #[arg(long)]
    source_type: Option<String>,
    /// CAWG training and data mining entry, repeatable, e.g. `cawg.ai_training=notAllowed` or
    /// `cawg.data_mining=constrained:research only`.
    #[arg(long, value_name = "KEY=USE[:CONSTRAINT]")]
    training: Vec<String>,
    /// Certificate chain (PEM) [default: the built-in demo certificate].
    #[arg(long, requires = "key")]
    cert: Option<PathBuf>,
    /// Private key (PEM) matching --cert.
    #[arg(long, requires = "cert")]
    key: Option<PathBuf>,
    /// Signature algorithm of --cert and --key.
    #[arg(long, default_value = "es256")]
    alg: String,
    /// Timestamp service (RFC 3161) that countersigns the signature; if it fails, the copy is
    /// signed without a timestamp and a note goes to stderr.
    #[arg(long, value_name = "URL", default_value = timestamp::DEFAULT_TSA_URL)]
    tsa: String,
    /// Sign without a timestamp, without network access.
    #[arg(long, conflicts_with = "tsa")]
    no_timestamp: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// How to get the Meta-Code and Content-Code of a video of `kind` read without ffmpeg; `None`
/// when it is no video, ffmpeg is `installed`, or there is no build to install.
fn ffmpeg_note(kind: Kind, installed: bool) -> Option<String> {
    let tool = tools::ffmpeg().filter(|_| kind == Kind::Video && !installed)?;
    Some(format!(
        "a video's Meta-Code and Content-Code need ffmpeg ({} download); run `c2pa-iscc tools \
        install`",
        megabytes(tool.bytes)
    ))
}

/// The `tools install` argument that installs the model of the Semantic-Code `kind`.
fn install_name(kind: SemanticKind) -> &'static str {
    match kind {
        SemanticKind::Image => "semantic-image",
        SemanticKind::Text => "semantic-text",
    }
}

/// How to get the Semantic-Code `kind`; `None` when its model is `installed`.
fn semantic_note(kind: SemanticKind, installed: bool) -> Option<String> {
    (!installed).then(|| {
        format!(
            "the {} needs its model ({} download); run `c2pa-iscc tools install {}`",
            kind.unit_name(),
            megabytes(tools::semantic_status(kind).bytes.unwrap_or_default()),
            install_name(kind)
        )
    })
}

/// Print [`ffmpeg_note`] for `inspection` on stderr, and [`semantic_note`] when the
/// Semantic-Code was asked for (`semantic`).
fn note_missing_tools(inspection: &Inspection, semantic: bool) {
    let ffmpeg = ffmpeg_note(inspection.kind, tools::ffmpeg_status().installed);
    let semantic = SemanticKind::of(inspection.kind)
        .filter(|_| semantic)
        .and_then(|kind| semantic_note(kind, tools::semantic_installed(kind)));
    for note in ffmpeg.into_iter().chain(semantic) {
        eprintln!("note: {note}");
    }
}

/// `error` with the command that installs a semantic model when it is what is missing.
fn with_install_hint(error: anyhow::Error) -> anyhow::Error {
    if let Some(tools::Missing::SemanticModel(kind)) = error.downcast_ref::<tools::Missing>() {
        let name = install_name(*kind);
        return anyhow!("{error}; run `c2pa-iscc tools install {name}`");
    }
    error
}

/// The kinds of Semantic-Code a command computes: both when asked for, else none.
fn semantic_kinds(asked: bool) -> SemanticKinds {
    if asked {
        SemanticKinds::ALL
    } else {
        SemanticKinds::NONE
    }
}

/// A byte count in megabytes (MiB, as the app counts them), as "69 MB".
fn megabytes(bytes: u64) -> String {
    format!("{} MB", (bytes as f64 / 1_048_576.0).round())
}

/// Execute the parsed command and print its JSON.
fn run(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Inspect { file, semantic } => {
            let depth = Depth::FULL.with_semantic_kinds(semantic_kinds(*semantic));
            let mut inspection = inspect::inspect_with(file, depth, &|_, _| true)?;
            note_missing_tools(&inspection, *semantic);
            strip_preview(&mut inspection, cli.no_preview);
            print(&inspection, cli.compact)
        }
        Command::Sign(args) => {
            let named = args
                .units
                .as_ref()
                .is_some_and(|u| u.iter().any(|u| u == "semantic"));
            let kinds = semantic_kinds(args.semantic || named);
            let request = sign_request(args, kinds)?;
            let mut result =
                sign::sign_with(&request, kinds, &|_, _| true).map_err(with_install_hint)?;
            if let TimestampOutcome::Failed { url, reason } = &result.timestamp {
                eprintln!("note: signed without a timestamp: {url} failed ({reason})");
            }
            strip_preview(&mut result.inspection, cli.no_preview);
            print(&result, cli.compact)
        }
        Command::MetaCode {
            title,
            description,
            meta,
        } => print(
            &iscc::meta_unit(MetaInput {
                name: Some(title),
                description: description.as_deref(),
                meta: meta.as_deref(),
            })?,
            cli.compact,
        ),
        Command::Formats => print(&formats::FORMATS, cli.compact),
        Command::Tools { action } => match action {
            ToolsAction::Status => print(
                &ToolsStatus {
                    ffmpeg: tools::ffmpeg_status(),
                    semantic: SemanticStatus::now(),
                },
                cli.compact,
            ),
            ToolsAction::Install { tool } => install_tool(*tool, cli.compact),
        },
    }
}

/// Install `tool` unless it is there, and print its status.
fn install_tool(tool: Tool, compact: bool) -> Result<()> {
    let kind = match tool {
        Tool::Ffmpeg => {
            install_ffmpeg()?;
            return print(&tools::ffmpeg_status(), compact);
        }
        Tool::Semantic => {
            for kind in SemanticKind::ALL {
                install_semantic(kind)?;
            }
            return print(&SemanticStatus::now(), compact);
        }
        Tool::SemanticImage => SemanticKind::Image,
        Tool::SemanticText => SemanticKind::Text,
    };
    install_semantic(kind)?;
    print(&tools::semantic_status(kind), compact)
}

/// A progress line on stderr every tenth of a download.
fn tenths() -> impl FnMut(u64, u64) -> bool {
    let mut shown = 0;
    move |received, total| {
        let tenth = received * 10 / total.max(1);
        if tenth > shown {
            shown = tenth;
            eprintln!("{}%", tenth * 10);
        }
        true
    }
}

/// Install the model of the Semantic-Code `kind` unless it is there, with progress lines on
/// stderr.
fn install_semantic(kind: SemanticKind) -> Result<()> {
    let status = tools::semantic_status(kind);
    if status.installed {
        return Ok(());
    }
    eprintln!(
        "downloading the model of the {} ({}, {}) from {}",
        kind.unit_name(),
        megabytes(status.bytes.unwrap_or_default()),
        status.licence,
        status.url.unwrap_or_default()
    );
    tools::install_semantic(kind, &mut tenths())?;
    Ok(())
}

/// Install ffmpeg unless it is there, with a progress line on stderr every tenth of the download.
fn install_ffmpeg() -> Result<()> {
    if tools::ffmpeg_status().installed {
        return Ok(());
    }
    if let Some(tool) = tools::ffmpeg() {
        eprintln!(
            "downloading ffmpeg {} ({}, {}) from {}",
            tool.version,
            megabytes(tool.bytes),
            tools::FFMPEG_LICENCE,
            tool.url
        );
    }
    tools::install_ffmpeg(&mut tenths())?;
    Ok(())
}

/// Write `value` as JSON to stdout.
fn print(value: &impl Serialize, compact: bool) -> Result<()> {
    let json = if compact {
        serde_json::to_string(value)?
    } else {
        serde_json::to_string_pretty(value)?
    };
    println!("{json}");
    Ok(())
}

/// Blank the preview data URL when asked to.
fn strip_preview(inspection: &mut Inspection, strip: bool) {
    if strip {
        inspection.preview.clear();
    }
}

/// The signing request, with every option the user left out filled in the way the Sign tab
/// prefills its form, the Semantic-Code of `kinds` only. A file that cannot be signed is an
/// error; what signing does to it that its owner may not want is a note.
fn sign_request(args: &SignArgs, kinds: SemanticKinds) -> Result<SignRequest> {
    let depth = Depth::FULL.with_semantic_kinds(kinds);
    let source = inspect::inspect_with(&args.file, depth, &|_, _| true)?;
    note_missing_tools(&source, kinds != SemanticKinds::NONE);
    if let Some(block) = source.sign_block {
        bail!("{block}");
    }
    if let Some(warning) = source.sign_warning {
        eprintln!("note: {warning}");
    }
    let fields = &source.meta_fields;
    let title = args.title.clone().unwrap_or_else(|| fields.name.clone());
    let description = args
        .description
        .clone()
        .or_else(|| fields.description.clone());
    let units = match &args.units {
        Some(units) => units.clone(),
        None => default_units(
            &source,
            args.title.is_some(),
            &title,
            description.as_deref(),
        ),
    };
    Ok(SignRequest {
        source: args.file.to_string_lossy().into_owned(),
        output: args.output.as_ref().map_or_else(
            || source.suggested_output.clone(),
            |o| o.to_string_lossy().into_owned(),
        ),
        title,
        description,
        meta: fields.meta.clone(),
        source_type: args.source_type.clone(),
        units,
        training: training_entries(&args.training)?,
        credentials: credentials(args),
        tsa_url: (!args.no_timestamp).then(|| args.tsa.clone()),
    })
}

/// Every unit for the asset's kind; the Meta-Code only when it can be computed from the title,
/// description and the file's ISCC metadata, and, when the inspection computed none (a video's
/// tags unread without ffmpeg), only for a title `given`; the Semantic-Code and the Content-Code
/// only when the inspection computed them (audio can be too short, the Semantic-Code not asked
/// for or its model missing); each with a note on stderr otherwise, unless
/// [`note_missing_tools`] told already or the Semantic-Code was not asked for.
fn default_units(
    source: &Inspection,
    given: bool,
    title: &str,
    description: Option<&str>,
) -> Vec<String> {
    let meta = match &source.meta_error {
        Some(e) if !given => Err(anyhow!("{e}")),
        _ => iscc::meta_unit(MetaInput {
            name: Some(title),
            description,
            meta: source.meta_fields.meta.as_deref(),
        }),
    };
    let content = source.kind.slug();
    let mut units = vec!["meta", "semantic", content, "data", "instance"];
    if !source.iscc.iter().any(|u| u.unit == "semantic") {
        units.retain(|u| *u != "semantic");
        let missing =
            SemanticKind::of(source.kind).map(|k| tools::Missing::SemanticModel(k).reason());
        if let Some(e) = source
            .semantic_error
            .as_deref()
            .filter(|e| Some(*e) != missing)
        {
            eprintln!("note: Semantic-Code left out: {e}");
        }
    }
    if let Some(e) = &source.content_error {
        eprintln!("note: Content-Code left out: {e}");
        units.retain(|u| *u != content);
    }
    if let Err(e) = meta {
        eprintln!("note: Meta-Code left out: {e:#}");
        units.retain(|u| *u != "meta");
    }
    units.into_iter().map(str::to_owned).collect()
}

/// Parse `KEY=USE[:CONSTRAINT]` entries; a constraint is required for `constrained`.
fn training_entries(entries: &[String]) -> Result<BTreeMap<String, TrainingEntry>> {
    let mut out = BTreeMap::new();
    for entry in entries {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("training entry {entry:?} is not KEY=USE"))?;
        let (use_, constraint) = match value.split_once(':') {
            Some((use_, constraint)) => (use_, Some(constraint.trim().to_owned())),
            None => (value, None),
        };
        if !["allowed", "notAllowed", "constrained"].contains(&use_) {
            bail!("training use {use_:?} is not allowed, notAllowed or constrained");
        }
        if use_ == "constrained" && constraint.as_deref().is_none_or(str::is_empty) {
            bail!("training entry {key} is constrained but names no constraint");
        }
        out.insert(
            key.trim().to_owned(),
            TrainingEntry {
                use_: use_.to_owned(),
                constraint_info: constraint.filter(|_| use_ == "constrained"),
            },
        );
    }
    Ok(out)
}

/// The demo certificate, or the PEM files given.
fn credentials(args: &SignArgs) -> Credentials {
    match (&args.cert, &args.key) {
        (Some(cert), Some(key)) => Credentials::Custom {
            cert_path: cert.to_string_lossy().into_owned(),
            key_path: key.to_string_lossy().into_owned(),
            alg: args.alg.clone(),
        },
        _ => Credentials::Demo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_ffmpeg_says_how_to_install_it() {
        match ffmpeg_note(Kind::Video, false) {
            Some(note) => {
                assert!(note.ends_with("run `c2pa-iscc tools install`"), "{note}");
                assert!(note.contains(" MB download"), "{note}");
            }
            None => assert!(tools::ffmpeg().is_none(), "no build to install"),
        }
        assert_eq!(ffmpeg_note(Kind::Video, true), None);
        assert_eq!(ffmpeg_note(Kind::Audio, false), None);
        assert_eq!(megabytes(72_252_640), "69 MB");
    }

    #[test]
    fn missing_models_say_how_to_install_them() {
        let note = semantic_note(SemanticKind::Text, false).unwrap();
        assert_eq!(
            note,
            "the Semantic-Code Text needs its model (142 MB download); run `c2pa-iscc tools \
            install semantic-text`"
        );
        let image = semantic_note(SemanticKind::Image, false).unwrap();
        assert!(image.contains("(100 MB download)"), "{image}");
        assert!(image.ends_with("install semantic-image`"), "{image}");
        assert_eq!(semantic_note(SemanticKind::Image, true), None);
        let missing = tools::Missing::SemanticModel(SemanticKind::Image);
        let hinted = with_install_hint(missing.into());
        assert!(hinted
            .to_string()
            .ends_with("run `c2pa-iscc tools install semantic-image`"));
        let other = with_install_hint(anyhow!("something else"));
        assert_eq!(other.to_string(), "something else");
        assert_eq!(semantic_kinds(false), SemanticKinds::NONE);
        assert_eq!(semantic_kinds(true), SemanticKinds::ALL);
    }
}
