//! PDF assets: text and a picture of the first page from pdfium, title, description, creator and
//! ISCC metadata from the document information dictionary and the XMP packet, and whether the
//! file is encrypted or digitally signed.
//!
//! The text is what iscc-sdk extracts with pypdfium2: per page `FPDFText_GetBoundedText` over
//! the page's bounding box, UTF-16 with lone surrogates dropped, pages joined with a newline.
//! pdfium's own updates change that text, so the bundled build (`scripts/fetch_resources.py`)
//! follows iscc-sdk's. The library is loaded at run time from the app's resources, next to the
//! executable or, in debug builds, from `src-tauri/pdfium`; only its raw C API is used, and one
//! lock serialises every call, because pdfium is not thread-safe.
//!
//! Pages that are scans (a page-filling image with next to no native text) are listed at once,
//! from the page objects alone; with OCR switched on, [`Pdf::recognised`] renders them and puts
//! their recognised text in place of their native text. Every other page keeps iscc-sdk's text.
//!
//! Metadata follows iscc-sdk, which reads it through Tika: name from docinfo `iscc_name`, then
//! `/Title`, then XMP `dc:title`; description from `iscc_description`, `/Subject`, XMP
//! `dc:description`; creator from `/Author`, XMP `dc:creator`; ISCC metadata from `iscc_meta`.
//! A value that sanitises to nothing counts as missing. Deviation: docinfo strings in UTF-8 (PDF
//! 2.0) are decoded as UTF-8, where Tika makes mojibake of them.

use std::ffi::{c_int, c_ulong, c_void};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{anyhow, bail, Result};
use image::RgbImage;
use lopdf::Object;
use pdfium_render::prelude::{
    PdfiumLibraryBindings, FPDF_BITMAP, FPDF_DOCUMENT, FPDF_DWORD, FPDF_FORMFILLINFO, FPDF_PAGE,
    FPDF_PAGEOBJECT, FPDF_TEXTPAGE, FS_RECTF,
};
use serde::Serialize;

use crate::asset::Document;
use crate::thumbnail::PREVIEW_EDGE;
use crate::video::Progress;
use crate::{metadata, ocr, parallel};

/// `FPDF_GetLastError` codes: not a PDF, needs a password, unsupported security handler.
const FPDF_ERR_FORMAT: c_ulong = 3;
const FPDF_ERR_PASSWORD: c_ulong = 4;
const FPDF_ERR_SECURITY: c_ulong = 5;
/// `FPDF_RenderPageBitmap` flag: draw annotations, as viewers do.
const FPDF_ANNOT: i32 = 0x01;
/// `FPDF_GetFormType` result of a document without a form.
const FORMTYPE_NONE: i32 = 0;
/// Opaque white in pdfium's ARGB colour notation.
const WHITE: FPDF_DWORD = 0xFFFF_FFFF;
/// `FPDFPageObj_GetType` results of an image and of a form XObject.
const FPDF_PAGEOBJ_IMAGE: c_int = 3;
const FPDF_PAGEOBJ_FORM: c_int = 5;
/// Levels of nested form XObjects searched for images, as pypdfium2's `get_objects`.
const MAX_FORM_DEPTH: usize = 15;
/// Share of a page its largest image must cover for the page to be a scan.
const SCAN_COVER: f32 = 0.9;
/// Fewest native characters (after `text_collapse`) of a page that is no scan: a stamped page
/// number or a Bates number over a scan stays below it.
const SCAN_TEXT: usize = 50;
/// Most pages recognised at once: detecting a page takes about 220 MB, so eight at once took
/// 2 GB.
const MAX_PAGE_WORKERS: usize = 4;
/// What one compressed stream may inflate to in lopdf, so that a decompression bomb in a file of
/// unknown origin fails instead of exhausting memory; real object streams stay far below it.
const MAX_STREAM_BYTES: usize = 64 << 20;

/// Why a PDF without a text layer has no Content-Code.
const NO_TEXT_LAYER: &str = "no text layer found; a scanned PDF has only pictures of text";
/// Why a PDF whose scanned pages are its only text has none while OCR is off.
pub const NEEDS_OCR: &str = "no text layer found; its scanned pages need OCR, which is off";
/// Why a PDF has none when OCR found nothing on its scanned pages either.
pub const NOTHING_RECOGNISED: &str = "OCR found no text on its scanned pages";
/// Why a PDF that needs a password has neither text nor metadata.
const PASSWORD: &str = "the PDF is password-protected";
/// Why a PDF with a security handler pdfium does not support has neither.
const UNSUPPORTED_ENCRYPTION: &str = "the PDF uses an encryption that cannot be opened";
/// Inspection error when the library is not where the app looks for it.
const MISSING_LIBRARY: &str =
    "PDF support needs the pdfium library, which is missing from this installation";

/// Directory of the bundled library, set by the app at startup.
static LIBRARY_DIR: OnceLock<PathBuf> = OnceLock::new();
/// The bound library, or why it could not be bound; bound on first use.
static PDFIUM: OnceLock<Result<Mutex<Box<dyn PdfiumLibraryBindings>>, String>> = OnceLock::new();

/// Security facts that decide whether and how a PDF may be signed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PdfFlags {
    /// Has a security handler (an empty password included).
    pub encrypted: bool,
    /// Carries at least one digital signature.
    pub digitally_signed: bool,
}

impl PdfFlags {
    /// Why the PDF cannot be signed: c2pa-rs rewrites the file, which silently drops an
    /// encryption with an empty password and fails on one with a user password.
    pub fn sign_block(self) -> Option<&'static str> {
        self.encrypted.then_some(
            "Encrypted PDFs cannot be signed: embedding the manifest would remove the encryption or fail.",
        )
    }

    /// What signing a digitally signed PDF does to it; none for encrypted ones, which are blocked.
    pub fn sign_warning(self) -> Option<&'static str> {
        (self.digitally_signed && !self.encrypted)
            .then_some("Signing rewrites the PDF and breaks its existing digital signature.")
    }
}

/// How many pages of a PDF are scans, and whether OCR reads them.
#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OcrPages {
    /// Pages that are scans.
    pub scanned: usize,
    /// Pages of the document.
    pub pages: usize,
    /// Whether OCR is switched on, so their text is recognised.
    pub on: bool,
}

/// A PDF as [`read`] finds it.
#[derive(Debug, Default)]
pub struct Pdf {
    /// Title, description, creator, ISCC metadata, the picture of the first page, and the native
    /// text as iscc-sdk extracts it.
    pub document: Document,
    pub flags: PdfFlags,
    /// Native text of each page.
    pub pages: Vec<String>,
    /// Indices of the pages that are scans ([`is_scanned`]).
    pub scanned: Vec<usize>,
}

impl Pdf {
    /// What OCR does with this PDF when switched `on`; `None` without scanned pages.
    pub fn ocr_pages(&self, on: bool) -> Option<OcrPages> {
        (!self.scanned.is_empty()).then_some(OcrPages {
            scanned: self.scanned.len(),
            pages: self.pages.len(),
            on,
        })
    }

    /// The document with the text OCR recognises on the scanned pages of this PDF, held in
    /// `bytes`, in place of their native text, so a stamp over a scan is not counted twice.
    /// `progress` hears the share of those pages done and stops the run by returning false.
    pub fn recognised(self, bytes: &[u8], progress: Progress) -> Result<Document> {
        let mut pages = self.pages;
        let texts = recognize_pages(bytes, &self.scanned, progress)?;
        for (index, text) in self.scanned.iter().zip(texts) {
            pages[*index] = text;
        }
        Ok(Document {
            text: pages.join("\n"),
            no_text_reason: Some(NOTHING_RECOGNISED),
            ..self.document
        })
    }
}

/// Tell the loader where the app bundles the library.
pub fn set_library_dir(dir: PathBuf) {
    let _ = LIBRARY_DIR.set(dir);
}

/// Read the PDF held in `bytes`. A PDF that pdfium cannot open without a password reads as one
/// without text and metadata.
pub fn read(bytes: &[u8]) -> Result<Pdf> {
    // lopdf needs no pdfium, so it runs before the lock.
    let fields = fields(bytes);
    let lib = pdfium()?;
    let lib: &dyn PdfiumLibraryBindings = lib.as_ref();
    let handle = unsafe { lib.FPDF_LoadMemDocument64(bytes, None) };
    if handle.is_null() {
        return unopened(unsafe { lib.FPDF_GetLastError() });
    }
    let doc = Closing::new(handle, |h| unsafe { lib.FPDF_CloseDocument(h) });
    let flags = PdfFlags {
        encrypted: unsafe { lib.FPDF_GetSecurityHandlerRevision(doc.0) } != -1,
        digitally_signed: digitally_signed(lib, doc.0),
    };
    let (pages, scanned) = pages(lib, doc.0);
    let no_text_reason = if scanned.is_empty() {
        NO_TEXT_LAYER
    } else {
        NEEDS_OCR
    };
    let document = Document {
        text: pages.join("\n"),
        picture: render(lib, doc.0, 0, PREVIEW_EDGE),
        no_text_reason: Some(no_text_reason),
        ..fields
    };
    Ok(Pdf {
        document,
        flags,
        pages,
        scanned,
    })
}

/// A document pdfium could not open, by its error `code`: one that needs a password or uses an
/// unsupported security handler is encrypted and has neither text nor metadata; anything else
/// is an error.
fn unopened(code: c_ulong) -> Result<Pdf> {
    let reason = match code {
        FPDF_ERR_PASSWORD => PASSWORD,
        FPDF_ERR_SECURITY => UNSUPPORTED_ENCRYPTION,
        FPDF_ERR_FORMAT => bail!("not a valid PDF file"),
        _ => bail!("cannot read PDF (pdfium error {code})"),
    };
    Ok(Pdf {
        document: Document {
            no_text_reason: Some(reason),
            ..Default::default()
        },
        flags: PdfFlags {
            encrypted: true,
            digitally_signed: false,
        },
        ..Default::default()
    })
}

/// Whether a signature field carries a signature value: pdfium counts every `/Sig` form field,
/// the empty placeholder of an unsigned form included.
fn digitally_signed(lib: &dyn PdfiumLibraryBindings, doc: FPDF_DOCUMENT) -> bool {
    let count = unsafe { lib.FPDF_GetSignatureCount(doc) };
    (0..count).any(|i| {
        let signature = unsafe { lib.FPDF_GetSignatureObject(doc, i) };
        !signature.is_null()
            && unsafe { lib.FPDFSignatureObj_GetContents(signature, std::ptr::null_mut(), 0) } > 0
    })
}

/// A pdfium handle that is closed when it goes out of scope.
struct Closing<T: Copy, F: Fn(T)>(T, F);

impl<T: Copy, F: Fn(T)> Closing<T, F> {
    fn new(handle: T, close: F) -> Self {
        Closing(handle, close)
    }
}

impl<T: Copy, F: Fn(T)> Drop for Closing<T, F> {
    fn drop(&mut self) {
        (self.1)(self.0)
    }
}

/// The bound library, locked for the caller; binds it on first use.
fn pdfium() -> Result<MutexGuard<'static, Box<dyn PdfiumLibraryBindings>>> {
    let lib = PDFIUM
        .get_or_init(bind)
        .as_ref()
        .map_err(|e| anyhow!("{e}"))?;
    Ok(lib.lock().unwrap_or_else(|e| e.into_inner()))
}

/// Bind and initialise the first library found in [`library_dirs`].
fn bind() -> Result<Mutex<Box<dyn PdfiumLibraryBindings>>, String> {
    let path = find_library(&library_dirs())?;
    let lib = pdfium_render::prelude::Pdfium::bind_to_library(&path)
        .map_err(|e| format!("cannot load the pdfium library {}: {e}", path.display()))?;
    unsafe { lib.FPDF_InitLibrary() };
    Ok(Mutex::new(lib))
}

/// Path of the platform's pdfium library in the first of `dirs` that has it.
fn find_library(dirs: &[PathBuf]) -> Result<PathBuf, String> {
    dirs.iter()
        .map(pdfium_render::prelude::Pdfium::pdfium_platform_library_name_at_path)
        .find(|p| p.is_file())
        .ok_or_else(|| MISSING_LIBRARY.to_owned())
}

/// Where the library may be, in order: the app's resource directory, next to the executable
/// (the CLI), and in debug builds the directory the fetch script fills.
fn library_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = LIBRARY_DIR.get().cloned().into_iter().collect();
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        dirs.push(exe_dir.join("pdfium"));
        dirs.push(exe_dir);
    }
    if cfg!(debug_assertions) {
        dirs.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("pdfium"));
    }
    dirs
}

/// The native text of every page, as pypdfium2's `get_text_bounded` extracts it, and the indices
/// of the pages that are scans. A page pdfium cannot load has no text and is no scan.
fn pages(lib: &dyn PdfiumLibraryBindings, doc: FPDF_DOCUMENT) -> (Vec<String>, Vec<usize>) {
    let count = unsafe { lib.FPDF_GetPageCount(doc) }.max(0) as usize;
    let mut texts = Vec::with_capacity(count);
    let mut scanned = Vec::new();
    for index in 0..count {
        let Some(page) = load_page(lib, doc, index as i32) else {
            texts.push(String::new());
            continue;
        };
        let bounds = bounding_box(lib, page.0);
        let text = page_text(lib, page.0, &bounds);
        if is_scanned(largest_image(lib, page.0, &bounds), &text) {
            scanned.push(index);
        }
        texts.push(text);
    }
    (texts, scanned)
}

/// Whether a page is a scan: its largest image covers at least [`SCAN_COVER`] of it (`cover`)
/// and its native text has fewer than [`SCAN_TEXT`] characters once collapsed. Measured on 820
/// born-digital pages, the rule picks only an image-only design; a scan with an invisible text
/// layer keeps its text.
pub fn is_scanned(cover: f32, native_text: &str) -> bool {
    cover >= SCAN_COVER && iscc_lib::text_collapse(native_text).chars().count() < SCAN_TEXT
}

/// The page's bounding box: its MediaBox clipped to the CropBox, unrotated.
fn bounding_box(lib: &dyn PdfiumLibraryBindings, page: FPDF_PAGE) -> FS_RECTF {
    let mut rect = FS_RECTF {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    unsafe { lib.FPDF_GetPageBoundingBox(page, &mut rect) };
    rect
}

/// Text of the page inside its `bounds`; empty when it has none.
fn page_text(lib: &dyn PdfiumLibraryBindings, page: FPDF_PAGE, bounds: &FS_RECTF) -> String {
    let text_page: FPDF_TEXTPAGE = unsafe { lib.FPDFText_LoadPage(page) };
    if text_page.is_null() {
        return String::new();
    }
    let text_page = Closing::new(text_page, |h| unsafe { lib.FPDFText_ClosePage(h) });
    let (l, t, r, b) = (
        bounds.left as f64,
        bounds.top as f64,
        bounds.right as f64,
        bounds.bottom as f64,
    );
    let n =
        unsafe { lib.FPDFText_GetBoundedText(text_page.0, l, t, r, b, std::ptr::null_mut(), 0) };
    if n <= 0 {
        return String::new();
    }
    let mut buffer = vec![0u16; n as usize];
    unsafe { lib.FPDFText_GetBoundedText(text_page.0, l, t, r, b, buffer.as_mut_ptr(), n) };
    // Python's `decode("utf-16-le", errors="ignore")`: lone surrogates vanish.
    char::decode_utf16(buffer).filter_map(Result::ok).collect()
}

/// Share of the page's `bounds` that its largest image covers, clipped to them. Images inside
/// form XObjects count too, down to [`MAX_FORM_DEPTH`] levels, with the bounds pdfium gives
/// them, as the measurement behind [`SCAN_COVER`] took them.
fn largest_image(lib: &dyn PdfiumLibraryBindings, page: FPDF_PAGE, bounds: &FS_RECTF) -> f32 {
    let area = (bounds.right - bounds.left) * (bounds.top - bounds.bottom);
    if area <= 0.0 {
        return 0.0;
    }
    let count = unsafe { lib.FPDFPage_CountObjects(page) };
    let mut objects = (0..count).map(|i| unsafe { lib.FPDFPage_GetObject(page, i) });
    largest_image_area(lib, &mut objects, bounds, 0) / area
}

/// Area of the largest image among `objects` and inside the form XObjects among them, which sit
/// `depth` levels deep, clipped to `bounds`.
fn largest_image_area(
    lib: &dyn PdfiumLibraryBindings,
    objects: &mut dyn Iterator<Item = FPDF_PAGEOBJECT>,
    bounds: &FS_RECTF,
    depth: usize,
) -> f32 {
    let mut largest = 0f32;
    for object in objects.filter(|o| !o.is_null()) {
        match unsafe { lib.FPDFPageObj_GetType(object) } {
            FPDF_PAGEOBJ_IMAGE => largest = largest.max(clipped_area(lib, object, bounds)),
            FPDF_PAGEOBJ_FORM if depth + 1 < MAX_FORM_DEPTH => {
                let count = unsafe { lib.FPDFFormObj_CountObjects(object) }.max(0);
                let mut children =
                    (0..count).map(|i| unsafe { lib.FPDFFormObj_GetObject(object, i as c_ulong) });
                let inside = largest_image_area(lib, &mut children, bounds, depth + 1);
                largest = largest.max(inside);
            }
            _ => {}
        }
    }
    largest
}

/// Area of the bounds pdfium gives `object`, clipped to `bounds`.
fn clipped_area(
    lib: &dyn PdfiumLibraryBindings,
    object: FPDF_PAGEOBJECT,
    bounds: &FS_RECTF,
) -> f32 {
    let (mut l, mut b, mut r, mut t) = (0f32, 0f32, 0f32, 0f32);
    if unsafe { lib.FPDFPageObj_GetBounds(object, &mut l, &mut b, &mut r, &mut t) } == 0 {
        return 0.0;
    }
    let width = r.min(bounds.right) - l.max(bounds.left);
    let height = t.min(bounds.top) - b.max(bounds.bottom);
    width.max(0.0) * height.max(0.0)
}

/// Page `index`, closed when dropped; `None` when pdfium cannot load it.
fn load_page(
    lib: &dyn PdfiumLibraryBindings,
    doc: FPDF_DOCUMENT,
    index: i32,
) -> Option<Closing<FPDF_PAGE, impl Fn(FPDF_PAGE) + '_>> {
    let page = unsafe { lib.FPDF_LoadPage(doc, index) };
    (!page.is_null()).then(|| Closing::new(page, move |h| unsafe { lib.FPDF_ClosePage(h) }))
}

/// Page `index` rendered on white with its annotations and form fields, turned as its `/Rotate`
/// says, fitted into a long edge of `edge` pixels; `None` for a page that does not exist or that
/// pdfium cannot render.
fn render(
    lib: &dyn PdfiumLibraryBindings,
    doc: FPDF_DOCUMENT,
    index: i32,
    edge: u32,
) -> Option<RgbImage> {
    let page = load_page(lib, doc, index)?;
    let (w, h) = unsafe {
        (
            lib.FPDF_GetPageWidthF(page.0),
            lib.FPDF_GetPageHeightF(page.0),
        )
    };
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let scale = edge as f32 / w.max(h);
    let (pw, ph) = (
        ((w * scale).round() as i32).max(1),
        ((h * scale).round() as i32).max(1),
    );
    let bitmap: FPDF_BITMAP = unsafe { lib.FPDFBitmap_Create(pw, ph, 0) };
    if bitmap.is_null() {
        return None;
    }
    let bitmap = Closing::new(bitmap, |h| unsafe { lib.FPDFBitmap_Destroy(h) });
    unsafe {
        lib.FPDFBitmap_FillRect(bitmap.0, 0, 0, pw, ph, WHITE);
        lib.FPDF_RenderPageBitmap(bitmap.0, page.0, 0, 0, pw, ph, 0, FPDF_ANNOT);
    }
    draw_form_fields(lib, doc, page.0, bitmap.0, pw, ph);
    let stride = unsafe { lib.FPDFBitmap_GetStride(bitmap.0) } as usize;
    let buffer: *const c_void = unsafe { lib.FPDFBitmap_GetBuffer(bitmap.0) };
    if buffer.is_null() {
        return None;
    }
    // BGRx rows of `stride` bytes, owned by the bitmap.
    let bgrx = unsafe { std::slice::from_raw_parts(buffer as *const u8, stride * ph as usize) };
    Some(RgbImage::from_fn(pw as u32, ph as u32, |x, y| {
        let i = y as usize * stride + x as usize * 4;
        image::Rgb([bgrx[i + 2], bgrx[i + 1], bgrx[i]])
    }))
}

/// The recognised text of the pages `indices` of the PDF in `bytes`, in that order. One thread
/// renders them one by one ([`Rendered`]) while up to [`MAX_PAGE_WORKERS`] workers recognise
/// them, sharing out the logical cores. `progress` hears the share of the pages done.
fn recognize_pages(bytes: &[u8], indices: &[usize], progress: Progress) -> Result<Vec<String>> {
    if indices.is_empty() {
        return Ok(Vec::new());
    }
    let models = ocr::Models::load()?;
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let workers = cores.min(indices.len()).min(MAX_PAGE_WORKERS);
    let threads = (cores / workers).max(1);
    let read = |picture: Option<RgbImage>, _: &_| match picture {
        Some(picture) => ocr::page_text(&models, &picture, threads),
        None => Ok(String::new()),
    };
    let mut done = 0;
    let mut report = |_: &String| {
        done += 1;
        progress(Some(done as f64 / indices.len() as f64))
    };
    let pages = Rendered::new(bytes, indices)?;
    parallel::ordered(workers, 1, pages, read, &mut report)
}

/// Pages of a PDF rendered for OCR, one at a time, at [`ocr::PAGE_EDGE`]; `None` for a page
/// pdfium cannot render. The pdfium lock is held for one page only, so other files are read
/// meanwhile.
struct Rendered<'a> {
    doc: OpenDocument<'a>,
    indices: std::slice::Iter<'a, usize>,
}

impl<'a> Rendered<'a> {
    /// The pages `indices` of the PDF in `bytes`.
    fn new(bytes: &'a [u8], indices: &'a [usize]) -> Result<Self> {
        Ok(Rendered {
            doc: OpenDocument::load(bytes)?,
            indices: indices.iter(),
        })
    }
}

impl Iterator for Rendered<'_> {
    type Item = Option<RgbImage>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = *self.indices.next()?;
        let lib = pdfium().ok()?;
        Some(render(
            lib.as_ref(),
            self.doc.handle,
            index as i32,
            ocr::PAGE_EDGE,
        ))
    }
}

/// The scanned pages of the PDF in `bytes` as OCR sees them.
#[cfg(test)]
pub(crate) fn scanned_pictures(bytes: &[u8]) -> Vec<Option<RgbImage>> {
    let scanned = read(bytes).unwrap().scanned;
    Rendered::new(bytes, &scanned).unwrap().collect()
}

/// A document pdfium keeps open between calls, each made under the lock; closed when dropped.
struct OpenDocument<'a> {
    handle: FPDF_DOCUMENT,
    /// pdfium reads the document from these bytes until it is closed.
    bytes: PhantomData<&'a [u8]>,
}

// SAFETY: pdfium has no thread affinity; it must only never be called from two threads at once,
// and every call on this handle is made under the lock of `pdfium()`.
unsafe impl Send for OpenDocument<'_> {}

impl<'a> OpenDocument<'a> {
    /// The PDF in `bytes`, opened.
    fn load(bytes: &'a [u8]) -> Result<Self> {
        let lib = pdfium()?;
        let handle = unsafe { lib.FPDF_LoadMemDocument64(bytes, None) };
        if handle.is_null() {
            let code = unsafe { lib.FPDF_GetLastError() };
            bail!("cannot open the PDF for OCR (pdfium error {code})");
        }
        Ok(OpenDocument {
            handle,
            bytes: PhantomData,
        })
    }
}

impl Drop for OpenDocument<'_> {
    fn drop(&mut self) {
        if let Ok(lib) = pdfium() {
            unsafe { lib.FPDF_CloseDocument(self.handle) };
        }
    }
}

/// Draw the form fields of `page` onto `bitmap`, over the rendered page. `FPDF_RenderPageBitmap`
/// leaves widget annotations to the form-fill environment, so a filled form would show as a
/// blank page without this.
fn draw_form_fields(
    lib: &dyn PdfiumLibraryBindings,
    doc: FPDF_DOCUMENT,
    page: FPDF_PAGE,
    bitmap: FPDF_BITMAP,
    width: i32,
    height: i32,
) {
    if unsafe { lib.FPDF_GetFormType(doc) } == FORMTYPE_NONE {
        return;
    }
    // Version 1 of the interface, no callbacks; pdfium keeps the pointer until the environment
    // is destroyed.
    let mut info: FPDF_FORMFILLINFO = unsafe { std::mem::zeroed() };
    info.version = 1;
    let form = unsafe { lib.FPDFDOC_InitFormFillEnvironment(doc, &mut info) };
    if form.is_null() {
        return;
    }
    unsafe {
        lib.FORM_OnAfterLoadPage(page, form);
        lib.FPDF_FFLDraw(form, bitmap, page, 0, 0, width, height, 0, FPDF_ANNOT);
        lib.FORM_OnBeforeClosePage(page, form);
        lib.FPDFDOC_ExitFormFillEnvironment(form);
    }
}

/// Title, description, creator and ISCC metadata with iscc-sdk's precedence, as a document
/// without text and picture; none when lopdf cannot parse the file.
fn fields(bytes: &[u8]) -> Document {
    let options = lopdf::LoadOptions::with_max_decompressed_size(MAX_STREAM_BYTES);
    let Ok(pdf) = lopdf::Document::load_mem_with_options(bytes, options) else {
        return Document::default();
    };
    let info = |key: &[u8]| docinfo(&pdf, key);
    let xmp = xmp_packet(&pdf)
        .map(|x| metadata::document_xmp(&x))
        .unwrap_or_default();
    Document {
        title: info(b"iscc_name").or_else(|| info(b"Title")).or(xmp.title),
        description: info(b"iscc_description")
            .or_else(|| info(b"Subject"))
            .or(xmp.description),
        creator: info(b"Author").or(xmp.creator),
        meta: info(b"iscc_meta"),
        ..Default::default()
    }
}

/// A text string of the document information dictionary; `None` when missing, not decodable or
/// nothing after sanitising (iscc-sdk then takes the next source).
fn docinfo(pdf: &lopdf::Document, key: &[u8]) -> Option<String> {
    let info = pdf.trailer.get(b"Info").ok()?;
    let (_, info) = pdf.dereference(info).ok()?;
    let (_, value) = pdf.dereference(info.as_dict().ok()?.get(key).ok()?).ok()?;
    text_string(value).filter(|s| !metadata::sanitize(s).is_empty())
}

/// A PDF text string as Tika and pdfium decode it: UTF-16BE or UTF-8 after their byte order
/// marks, UTF-16LE after `FF FE` (outside the PDF specification), else PDFDocEncoding.
fn text_string(value: &Object) -> Option<String> {
    let bytes = value.as_str().ok()?;
    if let Some(le) = bytes.strip_prefix(b"\xFF\xFE") {
        let units: Vec<u16> = le
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16(&units).ok();
    }
    let text = lopdf::decode_text_string(value).ok()?;
    // lopdf keeps the UTF-8 byte order mark as a leading U+FEFF.
    Some(text.strip_prefix('\u{FEFF}').unwrap_or(&text).to_owned())
}

/// The XMP packet of the Catalog's `/Metadata` stream, decompressed.
fn xmp_packet(pdf: &lopdf::Document) -> Option<Vec<u8>> {
    let (_, metadata) = pdf
        .dereference(pdf.catalog().ok()?.get(b"Metadata").ok()?)
        .ok()?;
    metadata
        .as_stream()
        .ok()?
        .get_plain_content_with_limit(MAX_STREAM_BYTES)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::{self, AssetContent};
    use crate::formats;
    use crate::iscc;
    use crate::metadata::tests::sdk_meta_code;
    use serde_json::Value;

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(fixture_path(name)).unwrap()
    }

    /// The UTF-8 byte order mark read as PDFDocEncoding: iscc-sdk's (Tika's) reading of a UTF-8
    /// docinfo string, which this crate decodes as UTF-8 on purpose.
    const UTF8_MOJIBAKE: &str = "\u{EF}\u{BB}\u{BF}";

    /// Differences between this crate's reading of the PDF at `path` and the iscc-sdk
    /// reference `want` from `expected_pdf.py`. Where iscc-sdk's text collapses to nothing or
    /// cannot be extracted, the Content-Code must be missing instead. Metadata is not compared
    /// where iscc-sdk made mojibake of a UTF-8 docinfo string.
    fn mismatches(path: &Path, want: &Value) -> Vec<String> {
        let bytes = std::fs::read(path).unwrap();
        let asset = match asset::read(path, &bytes, formats::by_path(path).unwrap()) {
            Ok(asset) => asset,
            Err(e) => return vec![format!("cannot read: {e}")],
        };
        let mut out = Vec::new();
        let mut check = |what: &str, got: Option<&str>, want: &Value| {
            if got != want.as_str() {
                out.push(format!("{what}: {got:?} != {want}"));
            }
        };
        let m = &asset.metadata;
        let mojibake = ["title", "description", "creator"].iter().any(|k| {
            want[k]
                .as_str()
                .is_some_and(|v| v.starts_with(UTF8_MOJIBAKE))
        });
        if !mojibake {
            check("title", m.name.as_deref(), &want["title"]);
            check(
                "description",
                m.description.as_deref(),
                &want["description"],
            );
            check("creator", m.creator.as_deref(), &want["creator"]);
            check("Meta-Code", Some(&sdk_meta_code(m, path)), &want["meta"]);
        }
        check("iscc_meta", m.meta.as_deref(), &want["iscc_meta"]);
        let [data, instance] = iscc::bitstream_units(&bytes).unwrap();
        check("Data-Code", Some(&data.iscc), &want["data"]);
        check("Instance-Code", Some(&instance.iscc), &want["instance"]);
        let no_text = want["text"].is_null() || want["collapsed"] == 0;
        match (&asset.content, no_text) {
            (AssetContent::Unavailable(_), true) => {}
            (AssetContent::Text(text), false) => {
                let code = iscc::content_unit(iscc::Content::Text(text)).unwrap().iscc;
                check("Content-Code Text", Some(&code), &want["text"]);
            }
            (content, _) => out.push(format!(
                "content {:?} where iscc-sdk has {} and {} characters",
                std::mem::discriminant(content),
                want["text"],
                want["collapsed"]
            )),
        }
        out
    }

    /// Mismatches of every PDF in `expected` (file name to reference) below `dir`, as lines
    /// naming the file.
    fn all_mismatches(dir: &Path, expected: &Value) -> Vec<String> {
        expected
            .as_object()
            .unwrap()
            .iter()
            .flat_map(|(file, want)| {
                mismatches(&dir.join(file), want)
                    .into_iter()
                    .map(move |m| format!("{file}: {m}"))
            })
            .collect()
    }

    #[test]
    fn units_and_metadata_match_iscc_sdk_reference() {
        let expected: Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_pdf.json")).unwrap();
        let wrong = all_mismatches(&fixture_path(""), &expected);
        assert!(
            wrong.is_empty(),
            "{}",
            wrong.join(
                "
"
            )
        );
    }

    #[test]
    fn utf8_docinfo_strings_are_decoded() {
        // Deviation: iscc-sdk 0.9.5 (Tika) reads these as PDFDocEncoding mojibake. Meta-Code
        // from iscc-lib: gen_meta_code_v0("Größe 日本語 UTF-8", "Beschreibung – UTF-8", bits=256).
        let path = fixture_path("meta-utf8.pdf");
        let asset = asset::read(
            &path,
            &fixture("meta-utf8.pdf"),
            formats::by_path(&path).unwrap(),
        )
        .unwrap();
        assert_eq!(asset.metadata.name.as_deref(), Some("Größe 日本語 UTF-8"));
        assert_eq!(
            asset.metadata.description.as_deref(),
            Some("Beschreibung – UTF-8")
        );
        assert_eq!(
            sdk_meta_code(&asset.metadata, &path),
            "ISCC:AAD7ZWJKTVDS7J2FE5JVLUPCGEN3EYX7ZVNUPLC7J33DRZ74TK7NJ5Q"
        );
    }

    /// Compare every PDF below `PDF_CORPUS_DIR` with the references `expected_pdf.py
    /// PDF_CORPUS_DIR` wrote there.
    #[test]
    #[ignore = "needs a local corpus; set PDF_CORPUS_DIR and run with --ignored"]
    fn pdf_corpus_matches_iscc_sdk_reference() {
        let dir = PathBuf::from(std::env::var("PDF_CORPUS_DIR").expect("PDF_CORPUS_DIR"));
        let json = std::fs::read_to_string(dir.join("expected_pdf.json")).unwrap();
        let expected: Value = serde_json::from_str(&json).unwrap();
        let wrong = all_mismatches(&dir, &expected);
        let files = expected.as_object().unwrap().len();
        assert!(
            wrong.is_empty(),
            "{} mismatches in {files} files:
{}",
            wrong.len(),
            wrong.join(
                "
"
            )
        );
    }

    #[test]
    fn missing_library_is_a_plain_error() {
        let empty = std::env::temp_dir().join("iscc-c2pa-demo-test-no-pdfium");
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(
            find_library(std::slice::from_ref(&empty)).unwrap_err(),
            MISSING_LIBRARY
        );
        let found = find_library(&[empty, fixture_path("../../pdfium")]).unwrap();
        assert!(found.starts_with(fixture_path("../../pdfium")));
    }

    #[test]
    fn library_binds_and_renders_the_first_page() {
        let pdf = read(&fixture("demo.pdf")).unwrap();
        assert_eq!(pdf.flags, PdfFlags::default());
        let picture = pdf.document.picture.expect("first page");
        assert_eq!(picture.width().max(picture.height()), PREVIEW_EDGE);
    }

    #[test]
    fn unsigned_signature_field_is_no_digital_signature() {
        // A form with an empty signature field, as blank official forms have them; pdfium's
        // FPDF_GetSignatureCount counts it.
        use lopdf::{dictionary, Object};
        let mut pdf = lopdf::Document::load_mem(&fixture("basic-no-xmp.pdf")).unwrap();
        let field = pdf.add_object(dictionary! {
            "FT" => "Sig",
            "T" => Object::string_literal("Signature1"),
        });
        let form = pdf.add_object(dictionary! { "Fields" => vec![Object::Reference(field)] });
        pdf.catalog_mut()
            .unwrap()
            .set("AcroForm", Object::Reference(form));
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        assert!(!read(&bytes).unwrap().flags.digitally_signed);
        let signed = read(&fixture("basic-retest.pdf")).unwrap();
        assert!(signed.flags.digitally_signed);
    }

    #[test]
    fn filled_form_fields_are_drawn() {
        // basic-no-xmp.pdf with one filled text field whose appearance paints a 200 x 100 pt
        // black box: FPDF_RenderPageBitmap skips widgets, FPDF_FFLDraw draws them.
        use lopdf::{dictionary, Object, Stream};
        let mut pdf = lopdf::Document::load_mem(&fixture("basic-no-xmp.pdf")).unwrap();
        let page = *pdf.get_pages().get(&1).unwrap();
        let appearance = pdf.add_object(Object::Stream(Stream::new(
            dictionary! {
                "Type" => "XObject",
                "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
            },
            b"0 g 0 0 200 100 re f".to_vec(),
        )));
        let field = pdf.add_object(dictionary! {
            "Type" => "Annot",
            "Subtype" => "Widget",
            "FT" => "Tx",
            "T" => Object::string_literal("name"),
            "V" => Object::string_literal("Jane Doe"),
            "F" => 4,
            "P" => Object::Reference(page),
            "Rect" => vec![100.into(), 100.into(), 300.into(), 200.into()],
            "AP" => dictionary! { "N" => Object::Reference(appearance) },
        });
        pdf.get_object_mut(page)
            .unwrap()
            .as_dict_mut()
            .unwrap()
            .set("Annots", vec![Object::Reference(field)]);
        let form = pdf.add_object(dictionary! { "Fields" => vec![Object::Reference(field)] });
        pdf.catalog_mut()
            .unwrap()
            .set("AcroForm", Object::Reference(form));
        let mut bytes = Vec::new();
        pdf.save_to(&mut bytes).unwrap();
        let picture = read(&bytes).unwrap().document.picture.unwrap();
        let dark = picture.pixels().filter(|p| p.0[0] < 128).count();
        // The box covers 200 x 100 of the 612 x 792 pt page, scaled to the preview edge.
        let scale = f64::from(PREVIEW_EDGE) / 792.0;
        let expected = (200.0 * scale * 100.0 * scale) as usize;
        assert!(
            dark >= expected * 9 / 10,
            "{dark} dark pixels, expected about {expected}"
        );
    }

    #[test]
    fn scans_are_pages_filled_by_an_image_with_next_to_no_text() {
        assert!(is_scanned(1.0, ""));
        assert!(is_scanned(0.9, ""));
        assert!(!is_scanned(0.89, ""), "a figure, or a scan with margins");
        assert!(is_scanned(0.95, "Page 1 of 9"), "a stamped page number");
        assert!(is_scanned(0.95, " . , ; !\n"), "punctuation does not count");
        assert!(is_scanned(1.0, &"x".repeat(49)));
        assert!(!is_scanned(1.0, &"x".repeat(50)));
        let layer = "The invisible text layer a scanner's own OCR puts over the picture";
        assert!(!is_scanned(1.0, layer), "keeps its text, as in iscc-sdk");
    }

    /// The scanned pages of every PDF fixture whose name contains `scan` or `mixed`.
    #[test]
    fn only_the_scans_among_the_fixtures_are_routed() {
        let mut routed = Vec::new();
        for entry in std::fs::read_dir(fixture_path("")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "pdf") {
                let pdf = read(&std::fs::read(&path).unwrap()).unwrap();
                if !pdf.scanned.is_empty() {
                    let name = path.file_name().unwrap().to_string_lossy().into_owned();
                    routed.push((name, pdf.scanned));
                }
            }
        }
        routed.sort();
        let expected = [
            ("mixed.pdf", vec![1]),
            ("scan-blank.pdf", vec![0]),
            ("scan-demo.pdf", vec![0]),
            ("scan-stamped.pdf", vec![0]),
            ("scan.pdf", vec![0]),
        ]
        .map(|(n, p)| (n.to_owned(), p));
        assert_eq!(routed, expected);
        let mixed = read(&fixture("mixed.pdf")).unwrap();
        let pages = OcrPages {
            scanned: 1,
            pages: 2,
            on: true,
        };
        assert_eq!(mixed.ocr_pages(true), Some(pages));
        assert_eq!(mixed.document.no_text_reason, Some(NEEDS_OCR));
        let demo = read(&fixture("demo.pdf")).unwrap();
        assert_eq!(demo.ocr_pages(true), None);
        assert_eq!(demo.document.no_text_reason, Some(NO_TEXT_LAYER));
    }

    /// The pages OCR would recognise in every PDF below `PDF_CORPUS_DIR` (the files named in its
    /// `expected_pdf.json`): only the first page of an image-only Adobe Express design, as
    /// measured when the rule was chosen.
    #[test]
    #[ignore = "needs a local corpus; set PDF_CORPUS_DIR and run with --ignored --nocapture"]
    fn ocr_routing_corpus() {
        let dir = PathBuf::from(std::env::var("PDF_CORPUS_DIR").expect("PDF_CORPUS_DIR"));
        let json = std::fs::read_to_string(dir.join("expected_pdf.json")).unwrap();
        let expected: Value = serde_json::from_str(&json).unwrap();
        let mut routed = Vec::new();
        let mut pages = 0;
        for file in expected.as_object().unwrap().keys() {
            let Ok(pdf) = read(&std::fs::read(dir.join(file)).unwrap()) else {
                continue;
            };
            pages += pdf.pages.len();
            if !pdf.scanned.is_empty() {
                routed.push(format!("{file} pages {:?}", pdf.scanned));
            }
        }
        println!("{pages} pages, routed: {routed:#?}");
        assert_eq!(routed, ["test-suites/c2pa_express-signed.pdf pages [0]"]);
    }

    /// Recognise the scanned pages of the PDF at `OCR_PDF` and print how long it took, a
    /// measuring aid for long scans.
    #[test]
    #[ignore = "a measuring aid; set OCR_PDF and run with --ignored --nocapture"]
    fn ocr_pdf() {
        let bytes = std::fs::read(std::env::var("OCR_PDF").expect("OCR_PDF")).unwrap();
        let started = std::time::Instant::now();
        let pdf = read(&bytes).unwrap();
        let scanned = pdf.scanned.len();
        let doc = pdf.recognised(&bytes, &|_| true).unwrap();
        let seconds = started.elapsed().as_secs_f64();
        let chars = doc.text.chars().count();
        println!("{scanned} scanned pages, {chars} characters: {seconds:.1} s");
    }

    /// The text of the PDF fixture `name` with its scanned pages recognised, cleaned.
    fn recognised(name: &str) -> String {
        let bytes = fixture(name);
        let doc = read(&bytes).unwrap().recognised(&bytes, &|_| true).unwrap();
        iscc_lib::text_clean(&doc.text)
    }

    /// The Content-Code Text of `text`.
    fn text_code(text: &str) -> String {
        iscc::content_unit(iscc::Content::Text(text)).unwrap().iscc
    }

    #[test]
    fn ocr_reads_the_plain_scan_exactly() {
        let text = "A scanned page has only a picture of its text, no text layer.";
        assert_eq!(
            iscc_lib::text_collapse(&recognised("scan.pdf")),
            iscc_lib::text_collapse(text)
        );
    }

    #[test]
    fn a_scan_reads_close_to_its_born_digital_original() {
        let original = read(&fixture("demo.pdf")).unwrap().pages.remove(0);
        let original = text_code(&iscc_lib::text_clean(&original));
        let scan = text_code(&recognised("scan-demo.pdf"));
        let similarity = iscc::similarity(&scan, &original).unwrap().unwrap();
        assert!(similarity >= 0.9, "{similarity}");
    }

    #[test]
    fn a_stamp_over_a_scan_is_not_counted_twice() {
        let stamped = recognised("scan-stamped.pdf");
        let collapsed = iscc_lib::text_collapse(&stamped);
        assert_eq!(collapsed.matches("page1of9").count(), 1, "{stamped}");
        let plain = text_code(&recognised("scan-demo.pdf"));
        let similarity = iscc::similarity(&text_code(&stamped), &plain).unwrap();
        assert!(similarity.unwrap() >= 0.95, "{similarity:?}");
    }

    #[test]
    fn only_the_scanned_page_of_a_mixed_document_is_recognised() {
        let bytes = fixture("mixed.pdf");
        let pdf = read(&bytes).unwrap();
        let native = pdf.pages[0].clone();
        assert!(
            native.starts_with("This first page is born digital"),
            "{native}"
        );
        let reports = std::cell::Cell::new(0);
        let doc = pdf
            .recognised(&bytes, &|share| {
                reports.set(reports.get() + 1);
                assert_eq!(share, Some(1.0), "one page to recognise");
                true
            })
            .unwrap();
        assert_eq!(reports.get(), 1);
        let (first, second) = doc.text.split_at(native.len());
        assert_eq!(first, native, "pdfium's text, untouched");
        assert!(second.starts_with('\n') && second.len() > 1000, "{second}");
        assert_eq!(doc.no_text_reason, Some(NOTHING_RECOGNISED));
    }

    #[test]
    fn a_stopped_recognition_is_cancelled() {
        let bytes = fixture("scan-blank.pdf");
        let error = read(&bytes)
            .unwrap()
            .recognised(&bytes, &|_| false)
            .unwrap_err();
        assert!(
            error.downcast_ref::<crate::tools::Cancelled>().is_some(),
            "{error}"
        );
    }

    /// Most bits of 256 the Content-Code of a scan may differ between machines (Titusz,
    /// 2026-10-03: 4 bits per 64).
    const OCR_BUDGET: u32 = 16;

    /// The Content-Code of `scan-demo.pdf` read by OCR, against `expected_ocr.json`, which this
    /// app wrote on the machine named there. rten's kernels differ per instruction set, so
    /// another CPU may read a character differently; the report shows how far it got. Set
    /// `OCR_WRITE_REFERENCE` to write the reference anew.
    #[test]
    fn scan_code_stays_within_the_budget_of_the_reference() {
        let code = text_code(&recognised("scan-demo.pdf"));
        let set = crate::parallel::instruction_set();
        let path = fixture_path("expected_ocr.json");
        if std::env::var_os("OCR_WRITE_REFERENCE").is_some() {
            let json = serde_json::json!({ "scan-demo.pdf": { "text": code, "machine": set } });
            let text = serde_json::to_string_pretty(&json).unwrap() + "\n";
            std::fs::write(&path, text).unwrap();
        }
        let expected: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let want = &expected["scan-demo.pdf"];
        let similarity = iscc::similarity(&code, want["text"].as_str().unwrap())
            .unwrap()
            .unwrap();
        let distance = ((1.0 - similarity) * 256.0).round() as u32;
        println!(
            "{set}: scan-demo.pdf {distance} of 256 bits off the reference made on {}",
            want["machine"]
        );
        assert!(distance <= OCR_BUDGET, "{distance} bits off");
    }

    #[test]
    fn files_that_are_no_pdf_give_a_plain_error() {
        assert_eq!(
            read(b"not a PDF").unwrap_err().to_string(),
            "not a valid PDF file"
        );
        assert_eq!(read(b"").unwrap_err().to_string(), "not a valid PDF file");
        let pdf = unopened(FPDF_ERR_SECURITY).unwrap();
        assert!(pdf.flags.encrypted);
        assert_eq!(pdf.document.no_text_reason, Some(UNSUPPORTED_ENCRYPTION));
        assert!(pdf.scanned.is_empty());
    }
}
