//! PDF assets: text and a picture of the first page from pdfium, title, description, creator and
//! ISCC metadata from the document information dictionary and the XMP packet, and whether the
//! file is encrypted or digitally signed.
//!
//! The text is what iscc-sdk extracts with pypdfium2: per page `FPDFText_GetBoundedText` over
//! the page's bounding box, UTF-16 with lone surrogates dropped, pages joined with a newline.
//! pdfium's own updates change that text, so the bundled build (`scripts/fetch_pdfium.py`)
//! follows iscc-sdk's. The library is loaded at run time from the app's resources, next to the
//! executable or, in debug builds, from `src-tauri/pdfium`; only its raw C API is used, and one
//! lock serialises every call, because pdfium is not thread-safe.
//!
//! Metadata follows iscc-sdk, which reads it through Tika: name from docinfo `iscc_name`, then
//! `/Title`, then XMP `dc:title`; description from `iscc_description`, `/Subject`, XMP
//! `dc:description`; creator from `/Author`, XMP `dc:creator`; ISCC metadata from `iscc_meta`.
//! A value that sanitises to nothing counts as missing. Deviation: docinfo strings in UTF-8 (PDF
//! 2.0) are decoded as UTF-8, where Tika makes mojibake of them.

use std::ffi::{c_ulong, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{anyhow, bail, Result};
use image::RgbImage;
use lopdf::Object;
use pdfium_render::prelude::{
    PdfiumLibraryBindings, FPDF_BITMAP, FPDF_DOCUMENT, FPDF_DWORD, FPDF_FORMFILLINFO, FPDF_PAGE,
    FPDF_TEXTPAGE, FS_RECTF,
};

use crate::asset::Document;
use crate::metadata;
use crate::thumbnail::PREVIEW_EDGE;

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
/// What one compressed stream may inflate to in lopdf, so that a decompression bomb in a file of
/// unknown origin fails instead of exhausting memory; real object streams stay far below it.
const MAX_STREAM_BYTES: usize = 64 << 20;

/// Why a PDF without a text layer has no Content-Code.
const NO_TEXT_LAYER: &str = "no text layer found; a scanned PDF has only pictures of text";
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

/// Tell the loader where the app bundles the library.
pub fn set_library_dir(dir: PathBuf) {
    let _ = LIBRARY_DIR.set(dir);
}

/// Read the PDF held in `bytes`. A PDF that pdfium cannot open without a password reads as one
/// without text and metadata.
pub fn read(bytes: &[u8]) -> Result<(Document, PdfFlags)> {
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
    let document = Document {
        text: text(lib, doc.0),
        picture: first_page(lib, doc.0),
        no_text_reason: Some(NO_TEXT_LAYER),
        ..fields
    };
    Ok((document, flags))
}

/// A document pdfium could not open, by its error `code`: one that needs a password or uses an
/// unsupported security handler is encrypted and has neither text nor metadata; anything else
/// is an error.
fn unopened(code: c_ulong) -> Result<(Document, PdfFlags)> {
    let reason = match code {
        FPDF_ERR_PASSWORD => PASSWORD,
        FPDF_ERR_SECURITY => UNSUPPORTED_ENCRYPTION,
        FPDF_ERR_FORMAT => bail!("not a valid PDF file"),
        _ => bail!("cannot read PDF (pdfium error {code})"),
    };
    let doc = Document {
        no_text_reason: Some(reason),
        ..Default::default()
    };
    let flags = PdfFlags {
        encrypted: true,
        digitally_signed: false,
    };
    Ok((doc, flags))
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

/// Text of every page, joined with a newline, as pypdfium2's `get_text_bounded` extracts it.
fn text(lib: &dyn PdfiumLibraryBindings, doc: FPDF_DOCUMENT) -> String {
    let pages = unsafe { lib.FPDF_GetPageCount(doc) };
    (0..pages)
        .map(|i| page_text(lib, doc, i))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Text inside the bounding box of page `index` (its MediaBox clipped to the CropBox); empty
/// when the page cannot be loaded.
fn page_text(lib: &dyn PdfiumLibraryBindings, doc: FPDF_DOCUMENT, index: i32) -> String {
    let Some(page) = load_page(lib, doc, index) else {
        return String::new();
    };
    let mut rect = FS_RECTF {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };
    unsafe { lib.FPDF_GetPageBoundingBox(page.0, &mut rect) };
    let text_page: FPDF_TEXTPAGE = unsafe { lib.FPDFText_LoadPage(page.0) };
    if text_page.is_null() {
        return String::new();
    }
    let text_page = Closing::new(text_page, |h| unsafe { lib.FPDFText_ClosePage(h) });
    let (l, t, r, b) = (
        rect.left as f64,
        rect.top as f64,
        rect.right as f64,
        rect.bottom as f64,
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

/// Page `index`, closed when dropped; `None` when pdfium cannot load it.
fn load_page(
    lib: &dyn PdfiumLibraryBindings,
    doc: FPDF_DOCUMENT,
    index: i32,
) -> Option<Closing<FPDF_PAGE, impl Fn(FPDF_PAGE) + '_>> {
    let page = unsafe { lib.FPDF_LoadPage(doc, index) };
    (!page.is_null()).then(|| Closing::new(page, move |h| unsafe { lib.FPDF_ClosePage(h) }))
}

/// The first page rendered on white with its annotations and form fields, fitted into the
/// preview's long edge; `None` for a document without pages or a page pdfium cannot render.
fn first_page(lib: &dyn PdfiumLibraryBindings, doc: FPDF_DOCUMENT) -> Option<RgbImage> {
    let page = load_page(lib, doc, 0)?;
    let (w, h) = unsafe {
        (
            lib.FPDF_GetPageWidthF(page.0),
            lib.FPDF_GetPageHeightF(page.0),
        )
    };
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let scale = PREVIEW_EDGE as f32 / w.max(h);
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
        let (doc, flags) = read(&fixture("demo.pdf")).unwrap();
        assert_eq!(flags, PdfFlags::default());
        let picture = doc.picture.expect("first page");
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
        let (_, flags) = read(&bytes).unwrap();
        assert!(!flags.digitally_signed);
        let (_, flags) = read(&fixture("basic-retest.pdf")).unwrap();
        assert!(flags.digitally_signed);
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
        let (doc, _) = read(&bytes).unwrap();
        let picture = doc.picture.unwrap();
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
    fn files_that_are_no_pdf_give_a_plain_error() {
        assert_eq!(
            read(b"not a PDF").unwrap_err().to_string(),
            "not a valid PDF file"
        );
        assert_eq!(read(b"").unwrap_err().to_string(), "not a valid PDF file");
        let (doc, flags) = unopened(FPDF_ERR_SECURITY).unwrap();
        assert!(flags.encrypted);
        assert_eq!(doc.no_text_reason, Some(UNSUPPORTED_ENCRYPTION));
    }
}
