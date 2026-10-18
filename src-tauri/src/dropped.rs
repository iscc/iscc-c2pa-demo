//! Files that reach the app without a path of their own, such as an image dragged from a web
//! browser or a picture pasted from the clipboard: saved into a folder among the user's pictures,
//! then opened like any other file, so signing writes its copy next to them.

use crate::formats;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Longest file name kept, in characters, extension included.
const MAX_NAME: usize = 120;

/// Names Windows reserves for devices, in any case and with any extension.
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Folder for such files: `ISCC C2PA Demo` in the user's pictures, else in the downloads folder.
pub fn folder() -> Option<PathBuf> {
    dirs::picture_dir()
        .or_else(dirs::download_dir)
        .map(|dir| dir.join("ISCC C2PA Demo"))
}

/// A safe file name from a name a web page or the clipboard suggested: its last path segment,
/// characters Windows refuses replaced by `_`, no leading dots, no trailing dots or spaces, no
/// device name, at most `MAX_NAME` characters with the extension kept.
pub fn file_name(suggested: &str) -> String {
    let last = suggested.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = last
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    let (stem, ext) = match trimmed.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, Some(ext)),
        _ => (trimmed, None),
    };
    let stem = if stem.is_empty() { "dropped" } else { stem };
    let reserved = RESERVED
        .iter()
        .any(|r| r.eq_ignore_ascii_case(stem.trim_end()));
    let stem = if reserved {
        format!("_{stem}")
    } else {
        stem.to_string()
    };
    let room = MAX_NAME.saturating_sub(ext.map_or(0, |e| e.chars().count() + 1));
    let stem: String = stem.chars().take(room.max(1)).collect();
    match ext {
        Some(ext) => format!("{}.{ext}", stem.trim_end()),
        None => stem.trim_end().to_string(),
    }
}

/// The `n`th candidate for `name`: the name itself, then `stem-2.ext`, `stem-3.ext`, ...
fn candidate(name: &str, n: u32) -> String {
    if n == 1 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-{n}.{ext}"),
        None => format!("{name}-{n}"),
    }
}

/// Write `bytes` to a file in `dir` that did not exist before (`create_new`), named after
/// `suggested`; a name already taken gets `-2`, `-3`, ... before its extension. Refuses a name
/// whose extension is not one of the formats the app reads, before anything is written.
pub fn save(dir: &Path, suggested: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let name = file_name(suggested);
    if formats::by_path(Path::new(&name)).is_none() {
        return Err(format!("{name} is not in a format this app reads"));
    }
    fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    for n in 1..=1000 {
        let path = dir.join(candidate(&name, n));
        let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("could not create {}: {e}", path.display())),
        };
        return match file.write_all(bytes).and_then(|()| file.sync_all()) {
            Ok(()) => Ok(path),
            Err(e) => {
                drop(file);
                let _ = fs::remove_file(&path);
                Err(format!("could not write {}: {e}", path.display()))
            }
        };
    }
    Err(format!("no free name for {name} in {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_keeps_plain_names() {
        assert_eq!(
            file_name("shutterstock_1935960685.webp"),
            "shutterstock_1935960685.webp"
        );
        assert_eq!(file_name("Grüße aus Köln.jpg"), "Grüße aus Köln.jpg");
    }

    #[test]
    fn file_name_drops_folders_and_refused_characters() {
        assert_eq!(file_name("../../evil.png"), "evil.png");
        assert_eq!(file_name("C:\\Users\\x\\a.png"), "a.png");
        assert_eq!(file_name("what?<is>this*.jpg"), "what__is_this_.jpg");
        assert_eq!(file_name("tab\there.png"), "tab_here.png");
    }

    #[test]
    fn file_name_trims_dots_and_spaces() {
        assert_eq!(file_name(".hidden.png"), "hidden.png");
        assert_eq!(file_name("photo.jpg. . "), "photo.jpg");
        assert_eq!(file_name(""), "dropped");
        assert_eq!(file_name(".png"), "png");
    }

    #[test]
    fn file_name_avoids_device_names() {
        assert_eq!(file_name("con.png"), "_con.png");
        assert_eq!(file_name("NUL"), "_NUL");
        assert_eq!(file_name("console.png"), "console.png");
    }

    #[test]
    fn file_name_shortens_long_names_and_keeps_the_extension() {
        let name = file_name(&format!("{}.jpeg", "a".repeat(300)));
        assert_eq!(name.chars().count(), MAX_NAME);
        assert!(name.ends_with("a.jpeg"));
    }

    #[test]
    fn save_writes_the_bytes_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = save(dir.path(), "image.png", b"one").unwrap();
        let second = save(dir.path(), "image.png", b"two").unwrap();
        assert_eq!(first, dir.path().join("image.png"));
        assert_eq!(second, dir.path().join("image-2.png"));
        assert_eq!(fs::read(&first).unwrap(), b"one");
        assert_eq!(fs::read(&second).unwrap(), b"two");
    }

    #[test]
    fn save_creates_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("ISCC C2PA Demo");
        let path = save(&nested, "a.jpg", b"x").unwrap();
        assert_eq!(path, nested.join("a.jpg"));
    }

    #[test]
    fn save_refuses_formats_the_app_does_not_read() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["picture.avif", "run.exe", "no-extension"] {
            let err = save(dir.path(), name, b"x").unwrap_err();
            assert!(
                err.contains("not in a format this app reads"),
                "{name}: {err}"
            );
        }
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn folder_is_named_after_the_app() {
        if let Some(dir) = folder() {
            assert!(dir.ends_with("ISCC C2PA Demo"));
        }
    }
}
