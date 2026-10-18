//! Real paths of files dropped on or pasted into the page (Windows only).
//!
//! Tauri's drop handler takes only drags that carry file paths and turns everything else away,
//! so on Windows it is off and the page receives every drop itself, an image dragged from a web
//! browser included. The page's `File` objects have no path, so the page posts the first one with
//! WebView2's `postMessageWithAdditionalObjects`. WebView2 refuses a file that is not on disk (a
//! browser's image, a pasted picture), and the page saves those bytes instead; for a file on disk
//! this handler hands its path back as the [`EVENT`] event. Tauri's own IPC handler sees the
//! message too and logs a JSON error to the web view's console, which is harmless.

use tauri::{Emitter, WebviewWindow};
use webview2_com::Microsoft::Web::WebView2::Win32::{
    ICoreWebView2File, ICoreWebView2ObjectCollectionView, ICoreWebView2WebMessageReceivedEventArgs,
    ICoreWebView2WebMessageReceivedEventArgs2,
};
use webview2_com::{take_pwstr, WebMessageReceivedEventHandler};
use windows_core::{Interface, PWSTR};

/// Event carrying the path of the file the page posted.
pub const EVENT: &str = "dropped-path";

/// Path of the first file among a message's additional objects, if it is a file on disk.
fn first_path(args: &ICoreWebView2WebMessageReceivedEventArgs) -> Option<String> {
    // SAFETY: COM calls on live interfaces handed to the event handler; `take_pwstr` frees the
    // string WebView2 allocated.
    unsafe {
        let objects: ICoreWebView2ObjectCollectionView = args
            .cast::<ICoreWebView2WebMessageReceivedEventArgs2>()
            .ok()?
            .AdditionalObjects()
            .ok()?;
        let mut count = 0;
        objects.Count(&mut count).ok()?;
        if count == 0 {
            return None;
        }
        let file = objects
            .GetValueAtIndex(0)
            .ok()?
            .cast::<ICoreWebView2File>()
            .ok()?;
        let mut path = PWSTR::null();
        file.Path(&mut path).ok()?;
        Some(take_pwstr(path))
    }
}

/// Listen for files the page posts and emit each one's path to the page.
pub fn watch(window: &WebviewWindow) {
    let emitter = window.clone();
    let attached = window.with_webview(move |webview| {
        let handler = WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
            if let Some(path) = args.as_ref().and_then(first_path) {
                let _ = emitter.emit(EVENT, path);
            }
            Ok(())
        }));
        let mut token = 0i64;
        // SAFETY: the controller belongs to this window's live web view.
        let added = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.add_WebMessageReceived(&handler, &mut token))
        };
        if let Err(e) = added {
            eprintln!("could not watch dropped files: {e}");
        }
    });
    if let Err(e) = attached {
        eprintln!("could not watch dropped files: {e}");
    }
}
