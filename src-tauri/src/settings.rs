//! The app's settings, kept in `settings.json` in the app's local data folder next to the tools,
//! so deleting that folder resets the models and their switches together. Only the desktop app
//! reads and writes them; the CLI takes its choices from its arguments.
//!
//! A missing or unreadable file gives the defaults, with everything experimental off.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::semantic::{SemanticKind, SemanticKinds};
use crate::tools;

/// Everything the app remembers between runs.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// The kinds of Semantic-Code switched on.
    pub semantic: SemanticKinds,
}

/// Where the settings live: `settings.json` in the app's local data folder.
pub fn path() -> Result<PathBuf> {
    Ok(tools::app_dir()?.join("settings.json"))
}

/// The settings in the file at `path`; the defaults when it is missing or unreadable.
pub fn load_from(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Write `settings` to `path`: into a temporary sibling first, then renamed over the file, so a
/// failure never leaves half a file.
pub fn save_to(path: &Path, settings: &Settings) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(settings)?)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("cannot write {}", path.display()))
}

/// One kind of Semantic-Code as the Settings dialog shows it.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct SemanticSwitch {
    /// Switched on and its model installed: whether the app computes it.
    pub on: bool,
    pub installed: bool,
    /// Size of its download in bytes.
    pub bytes: u64,
    /// Name and version of its model.
    pub model: String,
}

/// Everything the Settings dialog shows about the Semantic-Codes.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct SemanticSettings {
    pub image: SemanticSwitch,
    pub text: SemanticSwitch,
    /// The tools folder the models are stored in.
    pub folder: Option<String>,
    /// The release the models come from.
    pub url: &'static str,
    pub licence: &'static str,
}

/// The Semantic-Codes of `settings` as the Settings dialog shows them: a kind is on only while
/// its model is installed, so a model deleted by hand reads as off.
pub fn semantic_settings(settings: &Settings) -> SemanticSettings {
    let switch = |kind| {
        let status = tools::semantic_status(kind);
        SemanticSwitch {
            on: settings.semantic.has(kind) && status.installed,
            installed: status.installed,
            bytes: status.bytes.unwrap_or_default(),
            model: format!("{} {}", status.name, status.version.unwrap_or_default()),
        }
    };
    SemanticSettings {
        image: switch(SemanticKind::Image),
        text: switch(SemanticKind::Text),
        folder: tools::tools_dir()
            .ok()
            .map(|d| d.to_string_lossy().into_owned()),
        url: tools::MODELS_RELEASE,
        licence: tools::MODELS_LICENCE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_gives_everything_off() {
        let dir = tempfile::tempdir().unwrap();
        let settings = load_from(&dir.path().join("settings.json"));
        assert_eq!(settings, Settings::default());
        assert_eq!(settings.semantic, SemanticKinds::NONE);
    }

    #[test]
    fn a_corrupt_file_gives_everything_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for content in ["{\"semantic\": {\"image\": tr", "[]", "\u{feff}garbage", ""] {
            fs::write(&path, content).unwrap();
            assert_eq!(load_from(&path), Settings::default(), "{content:?}");
        }
        fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        assert_eq!(load_from(&path), Settings::default());
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        // The folder does not exist yet on a fresh install.
        let path = dir.path().join("app").join("settings.json");
        let settings = Settings {
            semantic: SemanticKinds::NONE.with(SemanticKind::Text, true),
        };
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path), settings);
        save_to(&path, &Settings::default()).unwrap();
        assert_eq!(load_from(&path), Settings::default());
        let files: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(files.len(), 1, "no temporary file left");
    }

    #[test]
    fn unknown_and_missing_fields_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let content = r#"{"semantic": {"image": true, "audio": true}, "theme": "dark"}"#;
        fs::write(&path, content).unwrap();
        let settings = load_from(&path);
        assert!(settings.semantic.image);
        assert!(!settings.semantic.text, "missing: off");
        fs::write(&path, "{}").unwrap();
        assert_eq!(load_from(&path), Settings::default());
    }

    #[test]
    fn a_switch_is_on_only_with_its_model() {
        let off = semantic_settings(&Settings::default());
        assert!(!off.image.on && !off.text.on);
        assert_eq!(off.image.model, "iscc-sci v0.1.0-w16");
        assert_eq!(off.text.bytes, 149_385_916);
        let on = semantic_settings(&Settings {
            semantic: SemanticKinds::ALL,
        });
        for (kind, switch) in [
            (SemanticKind::Image, &on.image),
            (SemanticKind::Text, &on.text),
        ] {
            let installed = tools::semantic_installed(kind);
            assert_eq!((switch.on, switch.installed), (installed, installed));
        }
        assert_eq!(on.licence, "MIT and Apache-2.0");
        assert!(path()
            .unwrap()
            .ends_with("codes.iscc.c2pa-demo/settings.json"));
    }
}
