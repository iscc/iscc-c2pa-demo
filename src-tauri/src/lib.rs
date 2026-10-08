//! Tauri application shell exposing inspection and signing to the web view.

pub mod asset;
pub mod audio;
pub mod audio_tags;
pub mod context;
pub mod epub;
pub mod formats;
pub mod inspect;
pub mod iscc;
pub mod memo;
pub mod metadata;
pub mod numfmt;
pub mod ocr;
pub mod office;
pub mod opus;
pub mod parallel;
pub mod pdf;
pub mod pdf_update;
pub mod plain;
pub mod resample;
pub mod semantic;
pub mod settings;
pub mod sign;
pub mod svg;
pub mod thumbnail;
pub mod timestamp;
pub mod tools;
pub mod video;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

use semantic::SemanticKind;
use settings::{Settings, Switches};

/// Static facts the UI needs once at startup.
#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    c2pa_version: &'static str,
    claim_generator: &'static str,
    soft_binding_alg: &'static str,
    /// Lower-case file extensions accepted by the drop zone.
    extensions: Vec<&'static str>,
    /// The same extensions grouped by kind, for the file dialog and the drop zone.
    kinds: Vec<KindInfo>,
    /// Timestamp services offered in the Sign form; the first is the default.
    tsa_presets: &'static [timestamp::TsaPreset],
    /// Longest wait for a timestamp before signing goes on without one.
    tsa_timeout_secs: u64,
}

/// Formats and extensions of one kind of asset.
#[derive(Serialize)]
struct KindInfo {
    kind: formats::Kind,
    label: &'static str,
    /// Short format names, for the start screen.
    formats: Vec<&'static str>,
    extensions: Vec<&'static str>,
}

#[tauri::command]
fn app_info() -> AppInfo {
    let kinds: Vec<KindInfo> = formats::Kind::ALL
        .into_iter()
        .map(|kind| KindInfo {
            kind,
            label: kind.label(),
            formats: formats::short_names(kind),
            extensions: formats::extensions(kind),
        })
        .collect();
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        c2pa_version: c2pa::VERSION,
        claim_generator: context::CLAIM_GENERATOR_NAME,
        soft_binding_alg: context::ISCC_SOFT_BINDING_ALG,
        extensions: kinds.iter().flat_map(|k| k.extensions.clone()).collect(),
        kinds,
        tsa_presets: &timestamp::TSA_PRESETS,
        tsa_timeout_secs: timestamp::TIMEOUT.as_secs(),
    }
}

/// File path given on the command line (e.g. via "Open with"), consumed once by the UI at startup.
/// Only the Tauri dev CLI doubles backslashes; release builds receive the argument as typed, so
/// UNC and `\\?\` prefixes must survive untouched there.
#[tauri::command]
fn initial_path() -> Option<String> {
    std::env::args()
        .nth(1)
        .filter(|a| !a.starts_with('-'))
        .map(|a| {
            if cfg!(all(windows, debug_assertions)) {
                a.replace("\\\\", "\\")
            } else {
                a
            }
        })
}

/// Meta-Code preview for the sign form, so the user sees the unit before signing. `meta` is the
/// source's embedded ISCC metadata, which takes part in the Meta-Code but is not editable.
#[tauri::command]
fn meta_code(
    title: String,
    description: Option<String>,
    meta: Option<String>,
) -> Result<iscc::IsccUnit, String> {
    iscc::meta_unit(iscc::MetaInput {
        name: Some(&title),
        description: description.as_deref(),
        meta: meta.as_deref(),
    })
    .map_err(|e| format!("{e:#}"))
}

/// How far a slow unit got, for the progress bars.
#[derive(Serialize, Clone)]
struct Progress<S> {
    /// The unit of an inspection: `content` (a video decoding or scanned pages being recognised)
    /// or `semantic` (a text being embedded); or the [`sign::Stage`] of a signing: `source` (the file being signed),
    /// `write` (the signed copy is about to be written, which cannot be stopped) or `output`
    /// (the signed copy, checked after signing).
    stage: S,
    /// Share done; `None` when unknown (a video's duration).
    fraction: Option<f64>,
}

/// How much of a tool's download arrived.
#[derive(Serialize, Clone)]
struct Download {
    received: u64,
    total: u64,
}

/// Generation of the long tasks the UI waits for; [`cancel_tasks`] moves it on, and every task
/// started before stops at its next progress report.
static TASKS: AtomicU64 = AtomicU64::new(0);

/// Whether the UI still waits for a task started in `generation`.
fn still_wanted(generation: u64) -> bool {
    TASKS.load(Ordering::SeqCst) == generation
}

/// Stop the analyses and downloads that run now.
#[tauri::command]
fn cancel_tasks() {
    TASKS.fetch_add(1, Ordering::SeqCst);
}

/// The settings as the app holds them: loaded at startup, written by [`set_semantic`] and
/// [`set_ocr`].
fn settings_of(app: &AppHandle) -> Settings {
    *app.state::<Mutex<Settings>>()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// What the app computes: the kinds of Semantic-Code switched on whose models are installed,
/// and OCR when it is switched on.
fn computed(app: &AppHandle) -> Settings {
    let settings = settings_of(app);
    Settings {
        semantic: settings.semantic.installed(),
        ..settings
    }
}

/// Inspect the file at `path`: at a glance, its slow units pending, or in `full`, reporting the
/// progress of each slow unit; stopped by [`cancel_tasks`]. Semantic-Codes only of the kinds
/// switched on, scanned PDF pages recognised only with OCR on.
#[tauri::command]
async fn inspect_asset(
    app: AppHandle,
    path: String,
    full: bool,
    on_progress: Channel<Progress<&'static str>>,
) -> Result<inspect::Inspection, String> {
    let generation = TASKS.load(Ordering::SeqCst);
    let depth = if full {
        inspect::Depth::FULL
    } else {
        inspect::Depth::GLANCE
    };
    tauri::async_runtime::spawn_blocking(move || {
        let computed = computed(&app);
        let depth = depth
            .with_semantic_kinds(computed.semantic)
            .with_ocr(computed.ocr);
        let progress = |stage, fraction| {
            let _ = on_progress.send(Progress { stage, fraction });
            still_wanted(generation)
        };
        inspect::inspect_with(&PathBuf::from(path), depth, &progress).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Sign as `request` says, reporting the stages and the progress of video analyses; stopped by
/// [`cancel_tasks`] until the signed copy is written. A Semantic-Code of a kind that is off is
/// refused.
#[tauri::command]
async fn sign_asset(
    app: AppHandle,
    request: sign::SignRequest,
    on_progress: Channel<Progress<sign::Stage>>,
) -> Result<sign::SignResult, String> {
    let generation = TASKS.load(Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        let progress = |stage, fraction| {
            let _ = on_progress.send(Progress { stage, fraction });
            still_wanted(generation)
        };
        sign::sign_with(&request, computed(&app), &progress).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether opening the file at `path` needs ffmpeg: a video does, unless it carries sound only.
/// False when the file has no supported format, which the inspection then reports.
#[tauri::command]
async fn needs_ffmpeg(path: String) -> bool {
    tauri::async_runtime::spawn_blocking(move || {
        inspect::asset_format(&PathBuf::from(path)).is_ok_and(|f| f.kind == formats::Kind::Video)
    })
    .await
    .unwrap_or(false)
}

/// Whether ffmpeg, which videos need, is installed, and what installing it means.
#[tauri::command]
fn ffmpeg_status() -> tools::Status {
    tools::ffmpeg_status()
}

/// Download and install ffmpeg, reporting the bytes received; returns its new status.
#[tauri::command]
async fn install_ffmpeg(on_progress: Channel<Download>) -> Result<tools::Status, String> {
    let generation = TASKS.load(Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        install_reporting(&on_progress, generation, |p| {
            tools::install_ffmpeg(p).map(drop)
        })?;
        Ok(tools::ffmpeg_status())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Which Semantic-Codes are on and what switching each on downloads, and whether OCR is on.
#[tauri::command]
fn switches(app: AppHandle) -> Switches {
    settings::switches(&settings_of(&app))
}

/// Switch the Semantic-Code `kind` on or off and save the choice; returns the switches as they
/// are then. Switching on first downloads its model when it is missing, reporting the bytes
/// received, stopped by [`cancel_tasks`]; the switch flips only once the model is there.
/// Switching off keeps the model.
#[tauri::command]
async fn set_semantic(
    app: AppHandle,
    kind: SemanticKind,
    on: bool,
    on_progress: Channel<Download>,
) -> Result<Switches, String> {
    let generation = TASKS.load(Ordering::SeqCst);
    tauri::async_runtime::spawn_blocking(move || {
        if on {
            install_reporting(&on_progress, generation, |p| {
                tools::install_semantic(kind, p).map(drop)
            })?;
        }
        change_settings(&app, |s| Settings {
            semantic: s.semantic.with(kind, on),
            ..s
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Switch OCR of scanned PDF pages on or off and save the choice; returns the switches as they
/// are then. Its models are built in, so nothing is downloaded.
#[tauri::command]
fn set_ocr(app: AppHandle, on: bool) -> Result<Switches, String> {
    change_settings(&app, |s| Settings { ocr: on, ..s })
}

/// Save the settings `change` makes of the ones held and hold them; the switches as they are
/// then. The file comes first: the app never holds a choice it could not save.
fn change_settings(
    app: &AppHandle,
    change: impl FnOnce(Settings) -> Settings,
) -> Result<Switches, String> {
    let state = app.state::<Mutex<Settings>>();
    let mut held = state.lock().unwrap_or_else(|p| p.into_inner());
    let next = change(*held);
    settings::path()
        .and_then(|path| settings::save_to(&path, &next))
        .map_err(|e| format!("{e:#}"))?;
    *held = next;
    Ok(settings::switches(&next))
}

/// Run `install`, sending the bytes received to `on_progress`; the download stops once
/// [`cancel_tasks`] ends `generation`.
fn install_reporting(
    on_progress: &Channel<Download>,
    generation: u64,
    install: impl FnOnce(&mut dyn FnMut(u64, u64) -> bool) -> anyhow::Result<()>,
) -> Result<(), String> {
    // Reads arrive in pieces of a few KB; half a megabyte apart is enough for the bar.
    let mut sent = 0;
    install(&mut |received, total| {
        if received == total || received.saturating_sub(sent) >= 1 << 19 {
            sent = received;
            let _ = on_progress.send(Download { received, total });
        }
        still_wanted(generation)
    })
    .map_err(|e| format!("{e:#}"))
}

/// Size that fits into `room`: each side of `want` capped at the room available.
fn fit(want: LogicalSize<f64>, room: LogicalSize<f64>) -> LogicalSize<f64> {
    LogicalSize::new(want.width.min(room.width), want.height.min(room.height))
}

/// Shrink the window and its minimum size to the monitor's usable area and centre it, so the
/// title bar and the Sign button stay on screen on small or highly scaled displays (a 1080p
/// laptop at 150% has about 670 logical pixels of height, less than the configured window).
fn fit_to_screen(window: &WebviewWindow, min: Option<LogicalSize<f64>>) -> tauri::Result<()> {
    let Some(monitor) = window.current_monitor()?.or(window.primary_monitor()?) else {
        return Ok(());
    };
    let scale = window.scale_factor()?;
    let (outer, inner) = (window.outer_size()?, window.inner_size()?);
    let area = monitor
        .work_area()
        .size
        .to_logical::<f64>(monitor.scale_factor());
    let room = LogicalSize::new(
        area.width - f64::from(outer.width.saturating_sub(inner.width)) / scale,
        area.height - f64::from(outer.height.saturating_sub(inner.height)) / scale,
    );
    if let Some(min) = min {
        window.set_min_size(Some(fit(min, room)))?;
    }
    window.set_size(fit(inner.to_logical(scale), room))?;
    window.center()
}

/// Load the settings, point the PDF reader at the bundled pdfium, fit the main window (created
/// hidden) to the screen, then show it.
fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let loaded = settings::path()
        .map(|path| settings::load_from(&path))
        .unwrap_or_default();
    app.manage(Mutex::new(loaded));
    if let Ok(resources) = app.path().resource_dir() {
        pdf::set_library_dir(resources.join("pdfium"));
    }
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let min = app
        .config()
        .app
        .windows
        .first()
        .and_then(|w| Some(LogicalSize::new(w.min_width?, w.min_height?)));
    if let Err(e) = fit_to_screen(&window, min) {
        eprintln!("could not fit the window to the screen: {e}");
    }
    window.show()?;
    Ok(())
}

/// Start the desktop application.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(setup)
        .invoke_handler(tauri::generate_handler![
            app_info,
            initial_path,
            meta_code,
            inspect_asset,
            sign_asset,
            cancel_tasks,
            needs_ffmpeg,
            ffmpeg_status,
            install_ffmpeg,
            switches,
            set_semantic,
            set_ocr
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_keeps_a_window_that_fits() {
        let want = LogicalSize::new(1280.0, 840.0);
        assert_eq!(fit(want, LogicalSize::new(2560.0, 1400.0)), want);
    }

    #[test]
    fn fit_caps_each_side_to_the_room() {
        // 1920x1080 at 150% with a taskbar and a title bar.
        let room = LogicalSize::new(1280.0, 648.0);
        assert_eq!(fit(LogicalSize::new(1280.0, 840.0), room), room);
        assert_eq!(
            fit(LogicalSize::new(960.0, 640.0), room),
            LogicalSize::new(960.0, 640.0)
        );
        // 1366x768 at 125%: even the minimum height does not fit.
        let small = LogicalSize::new(1092.8, 558.0);
        assert_eq!(
            fit(LogicalSize::new(960.0, 640.0), small),
            LogicalSize::new(960.0, 558.0)
        );
    }
}
