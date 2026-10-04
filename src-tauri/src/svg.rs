//! SVG assets, rasterised the way iscc-sdk does it (resvg through resvg_py 0.5, at the SVG's
//! native size, capped at 4096 pixels a side, then flattened on white), with title, description,
//! ISCC metadata and creator read like `iscc_sdk.svg_meta_extract`.
//!
//! resvg_py's settings are kept on purpose: 0 dpi (absolute units inside the drawing collapse,
//! which is why iscc-sdk converts the root size to pixels first), a 16 px default font size, no
//! preferred languages, and system fonts with platform default families, loaded only for SVGs
//! that contain text. Text renders with whatever fonts the machine has, so the Content-Code of a
//! text-heavy SVG can differ between machines, in iscc-sdk as here.

use std::path::{Component, Path};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Context as _, Result};
use image::{Rgba, RgbaImage};
use resvg::tiny_skia::{IntSize, Pixmap, Transform};
use resvg::usvg::{self, fontdb, roxmltree};

use crate::asset::{Asset, AssetContent};
use crate::iscc;
use crate::metadata::{self, NS_DC, NS_ISCC, NS_RDF};

const NS_SVG: &str = "http://www.w3.org/2000/svg";
const NS_CC: &str = "http://creativecommons.org/ns#";
/// Largest render size on either side.
const MAX_SIZE: u32 = 4096;
/// Pixels per absolute CSS unit at 96 dpi.
const UNITS: [(&str, f64); 5] = [
    ("cm", 96.0 / 2.54),
    ("mm", 96.0 / 25.4),
    ("in", 96.0),
    ("pt", 96.0 / 72.0),
    ("pc", 96.0 / 6.0),
];

/// Default font families resvg_py sets: default, serif, sans-serif, cursive, fantasy, monospace.
#[cfg(any(target_os = "windows", target_os = "macos"))]
const FAMILIES: [&str; 6] = [
    "Times New Roman",
    "Times New Roman",
    "Arial",
    "Comic Sans MS",
    "Impact",
    "Courier New",
];
#[cfg(target_os = "linux")]
const FAMILIES: [&str; 6] = [
    "Liberation Serif",
    "Liberation Serif",
    "Liberation Sans",
    "Comic Neue",
    "Anton",
    "Liberation Mono",
];
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const FAMILIES: [&str; 6] = [
    "serif",
    "serif",
    "sans-serif",
    "cursive",
    "fantasy",
    "monospace",
];

/// Rasterise the SVG in `bytes` and read its metadata; `path` locates referenced resources.
pub fn read(path: &Path, bytes: &[u8]) -> Result<Asset> {
    let text = std::str::from_utf8(bytes)
        .context("the SVG is not UTF-8")?
        .trim();
    let doc = parse(text)?;
    let root = doc.root_element();
    let rdf = containers(root);
    // iscc-sdk's order: the ISCC property, else the SVG element, else Dublin Core.
    let field = |iscc: &str, svg: &str, dc: &str| {
        rdf_text(&rdf, NS_ISCC, iscc)
            .or_else(|| child_text(root, NS_SVG, svg))
            .or_else(|| rdf_text(&rdf, NS_DC, dc))
    };
    let name = field("name", "title", "title");
    let description = field("description", "desc", "description");
    let meta = rdf_text(&rdf, NS_ISCC, "meta");
    let image = rasterize(&pixel_size(text, root), path.parent(), render_size(root))?;
    Ok(Asset {
        content: AssetContent::Image(image),
        preview: None,
        metadata: metadata::document(
            name.as_deref(),
            description.as_deref(),
            meta.as_deref(),
            creator(&rdf).as_deref(),
        ),
        sign_block: None,
        sign_warning: None,
    })
}

/// Parse SVG markup as resvg_py does (DTDs allowed).
fn parse(text: &str) -> Result<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..Default::default()
    };
    roxmltree::Document::parse_with_options(text, options).map_err(|e| anyhow!("invalid SVG: {e}"))
}

/// iscc-sdk `_parse_svg_dimension`: a number, optionally with `px` or an absolute CSS unit, as
/// whole pixels at 96 dpi; `None` for relative units and anything else.
fn dimension(value: Option<&str>) -> Option<i64> {
    let value = value?.trim();
    let (number, factor) = UNITS
        .iter()
        .chain(&[("px", 1.0)])
        .find_map(|(unit, factor)| value.strip_suffix(unit).map(|n| (n.trim_end(), *factor)))
        .unwrap_or((value, 1.0));
    let plain = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit() || c == '.');
    let valid = match number.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => {
            let digits = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            plain(mantissa) && !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
        }
        None => plain(number),
    };
    let parsed: f64 = number.parse().ok().filter(|_| valid)?;
    Some((parsed * factor) as i64)
}

/// iscc-sdk `_svg_native_size`: width and height attributes, else the viewBox (completing one
/// missing side from its aspect ratio). Zero counts as missing.
fn native_size(root: roxmltree::Node) -> (Option<i64>, Option<i64>) {
    let set = |v: Option<i64>| v.filter(|&v| v != 0);
    let (w, h) = (
        set(dimension(root.attribute("width"))),
        set(dimension(root.attribute("height"))),
    );
    if w.is_some() && h.is_some() {
        return (w, h);
    }
    let view_box: Vec<f64> = root
        .attribute("viewBox")
        .unwrap_or_default()
        .replace(',', " ")
        .split_whitespace()
        .map_while(|p| p.parse().ok())
        .collect();
    let [_, _, vb_w, vb_h] = view_box[..] else {
        return (w, h);
    };
    match (w, h) {
        (Some(w), None) if vb_w > 0.0 => (Some(w), Some((w as f64 * vb_h / vb_w) as i64)),
        (None, Some(h)) if vb_h > 0.0 => (Some((h as f64 * vb_w / vb_h) as i64), Some(h)),
        _ => (Some(vb_w as i64), Some(vb_h as i64)),
    }
}

/// The size iscc-sdk renders at: the native size when it fits, else 4096 x 4096.
fn render_size(root: roxmltree::Node) -> (u32, u32) {
    let fits = |v: Option<i64>| {
        v.and_then(|v| u32::try_from(v).ok())
            .filter(|&v| v > 0 && v <= MAX_SIZE)
    };
    match native_size(root) {
        (w, h) if fits(w).is_some() && fits(h).is_some() => {
            (fits(w).unwrap_or(MAX_SIZE), fits(h).unwrap_or(MAX_SIZE))
        }
        _ => (MAX_SIZE, MAX_SIZE),
    }
}

/// iscc-sdk `_svg_normalize_units`: root width and height in absolute CSS units rewritten as
/// whole pixels, because resvg_py renders at 0 dpi.
fn pixel_size(text: &str, root: roxmltree::Node) -> String {
    let mut edits: Vec<(std::ops::Range<usize>, String)> = root
        .attributes()
        .filter(|a| a.namespace().is_none() && matches!(a.name(), "width" | "height"))
        .filter(|a| {
            UNITS
                .iter()
                .any(|(unit, _)| a.value().trim().ends_with(unit))
        })
        .filter_map(|a| Some((a.range_value(), dimension(Some(a.value()))?.to_string())))
        .collect();
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut out = text.to_owned();
    for (range, pixels) in edits {
        out.replace_range(range, &pixels);
    }
    out
}

/// Render `svg` at `size` (fitted with the drawing's aspect ratio, as resvg_py fits it) and
/// flatten it on white.
fn rasterize(svg: &str, resources: Option<&Path>, size: (u32, u32)) -> Result<image::RgbImage> {
    let doc = parse(svg)?;
    let has_text = doc.descendants().any(|n| n.has_tag_name((NS_SVG, "text")));
    let mut options = usvg::Options {
        resources_dir: resources.and_then(|p| std::fs::canonicalize(p).ok()),
        dpi: 0.0,
        font_family: FAMILIES[0].to_owned(),
        font_size: 16.0,
        languages: Vec::new(),
        default_size: usvg::Size::from_wh(size.0 as f32, size.1 as f32)
            .ok_or_else(|| anyhow!("empty SVG size"))?,
        style_sheet: Some(String::new()),
        image_href_resolver: relative_images(),
        ..Default::default()
    };
    if has_text {
        options.fontdb = Arc::new(system_fonts().clone());
    }
    let tree = usvg::Tree::from_xmltree(&doc, &options)
        .map_err(|e| anyhow!("cannot render the SVG: {e}"))?;
    let original = tree.size().to_int_size();
    let fitted = original
        .scale_to(IntSize::from_wh(size.0, size.1).ok_or_else(|| anyhow!("empty SVG size"))?);
    let mut pixmap = Pixmap::new(fitted.width(), fitted.height()).ok_or_else(|| {
        anyhow!(
            "cannot allocate {}x{} pixels",
            fitted.width(),
            fitted.height()
        )
    })?;
    let transform = Transform::from_scale(
        fitted.width() as f32 / original.width() as f32,
        fitted.height() as f32 / original.height() as f32,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(iscc::flatten_on_white(&straight_rgba(&pixmap)))
}

/// usvg's image loading, limited to files linked relative to the SVG's folder: an untrusted
/// SVG must not make the app open absolute, device or network paths (on Windows,
/// `\\host\share\x.png` contacts that host and can leak the user's NTLM hash).
fn relative_images() -> usvg::ImageHrefResolver<'static> {
    let load = usvg::ImageHrefResolver::default_string_resolver();
    usvg::ImageHrefResolver {
        resolve_string: Box::new(move |href, options| {
            (options.resources_dir.is_some() && is_relative(href))
                .then(|| load(href, options))
                .flatten()
        }),
        ..Default::default()
    }
}

/// Whether a link is a plain relative path: no root, drive, UNC or device prefix.
fn is_relative(href: &str) -> bool {
    Path::new(href).components().all(|c| {
        matches!(
            c,
            Component::Normal(_) | Component::CurDir | Component::ParentDir
        )
    })
}

/// The pixmap with alpha demultiplied, as tiny-skia's PNG encoder writes it.
fn straight_rgba(pixmap: &Pixmap) -> RgbaImage {
    let mut out = RgbaImage::new(pixmap.width(), pixmap.height());
    for (dst, src) in out.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    out
}

/// System fonts with resvg_py's default families, loaded once.
fn system_fonts() -> &'static fontdb::Database {
    static FONTS: OnceLock<fontdb::Database> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db.set_serif_family(FAMILIES[1]);
        db.set_sans_serif_family(FAMILIES[2]);
        db.set_cursive_family(FAMILIES[3]);
        db.set_fantasy_family(FAMILIES[4]);
        db.set_monospace_family(FAMILIES[5]);
        db
    })
}

/// RDF containers (`rdf:Description`, `cc:Work`) inside the root's `<metadata>`.
fn containers<'a, 'i>(root: roxmltree::Node<'a, 'i>) -> Vec<roxmltree::Node<'a, 'i>> {
    let Some(metadata) = root
        .children()
        .find(|n| n.has_tag_name((NS_SVG, "metadata")))
    else {
        return Vec::new();
    };
    let named = |ns: &'static str, name: &'static str| {
        metadata
            .descendants()
            .filter(move |n| n.has_tag_name((ns, name)))
    };
    named(NS_RDF, "Description")
        .chain(named(NS_CC, "Work"))
        .collect()
}

/// The text before the first child of the element's first child `ns`:`name`, trimmed, like
/// lxml's `find(...).text.strip()`; `None` when empty.
fn child_text(node: roxmltree::Node, ns: &str, name: &str) -> Option<String> {
    let child = node.children().find(|n| n.has_tag_name((ns, name)))?;
    let text = child.first_child().filter(|c| c.is_text())?.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// `ns`:`name` from the first RDF container that has it.
fn rdf_text(containers: &[roxmltree::Node], ns: &str, name: &str) -> Option<String> {
    containers.iter().find_map(|c| child_text(*c, ns, name))
}

/// iscc-sdk `_rdf_text_or_bag` for `dc:creator`: the first `rdf:li` inside it, else its text.
fn creator(containers: &[roxmltree::Node]) -> Option<String> {
    containers.iter().find_map(|c| {
        let el = c.children().find(|n| n.has_tag_name((NS_DC, "creator")))?;
        let item = el
            .descendants()
            .find(|n| n.has_tag_name((NS_RDF, "li")))
            .and_then(|li| li.first_child().filter(|t| t.is_text())?.text())
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let own = el
            .first_child()
            .filter(|t| t.is_text())
            .and_then(|t| t.text())
            .map(str::trim);
        item.or(own).filter(|t| !t.is_empty()).map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_matches_iscc_sdk_reference() {
        // Expected values produced by tests/fixtures/expected_meta.py (iscc-sdk with resvg_py).
        let expected: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/expected_meta.json")).unwrap();
        let svgs = expected
            .as_object()
            .unwrap()
            .iter()
            .filter(|(file, _)| file.ends_with(".svg"));
        for (file, want) in svgs {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(file);
            let asset = read(&path, &std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                asset.metadata.name.as_deref(),
                want["name"].as_str(),
                "{file}"
            );
            assert_eq!(
                asset.metadata.description.as_deref(),
                want["description"].as_str(),
                "{file}"
            );
            assert_eq!(
                asset.metadata.meta.as_deref(),
                want["meta"].as_str(),
                "{file}"
            );
            assert_eq!(
                asset.metadata.creator.as_deref(),
                want["creator"].as_str(),
                "{file}"
            );
            assert_eq!(
                metadata::tests::sdk_meta_code(&asset.metadata, &path),
                want["meta_code"],
                "{file} Meta-Code"
            );
            let unit = iscc::content_unit(asset.content()).unwrap();
            assert_eq!(unit.iscc, want["image"], "{file} Content-Code Image");
        }
    }

    #[test]
    fn native_size_like_iscc_sdk() {
        let size = |attrs: &str| {
            let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" {attrs}/>"#);
            native_size(parse(&svg).unwrap().root_element())
        };
        assert_eq!(size(r#"width="120" height="80px""#), (Some(120), Some(80)));
        assert_eq!(size(r#"width="80mm" height="1in""#), (Some(302), Some(96)));
        assert_eq!(size(r#"viewBox="0 0 64.9 32""#), (Some(64), Some(32)));
        assert_eq!(
            size(r#"width="200" viewBox="0,0,100,50""#),
            (Some(200), Some(100))
        );
        assert_eq!(size(r#"width="50%" height="2em""#), (None, None));
        assert_eq!(size(r#"width="1e2" height="0""#), (Some(100), None));
    }

    #[test]
    fn absolute_units_become_pixels() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width = "80mm" height='1in' viewBox="0 0 8 6"/>"#;
        let doc = parse(svg).unwrap();
        assert_eq!(
            pixel_size(svg, doc.root_element()),
            r#"<svg xmlns="http://www.w3.org/2000/svg" width = "302" height='96' viewBox="0 0 8 6"/>"#
        );
        assert_eq!(render_size(doc.root_element()), (302, 96));
    }

    #[test]
    fn metadata_priority_like_iscc_sdk() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
            xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:cc="http://creativecommons.org/ns#" width="4" height="4">
            <title> SVG title </title>
            <metadata><rdf:RDF><cc:Work><dc:title>DC title</dc:title><dc:description>DC &amp; description</dc:description>
            <dc:creator><rdf:Bag><rdf:li>First</rdf:li><rdf:li>Second</rdf:li></rdf:Bag></dc:creator></cc:Work></rdf:RDF></metadata>
            <rect width="4" height="4" fill="red"/></svg>"#;
        let asset = read(Path::new("x.svg"), svg.as_bytes()).unwrap();
        assert_eq!(asset.metadata.name.as_deref(), Some("SVG title"));
        assert_eq!(
            asset.metadata.description.as_deref(),
            Some("DC & description")
        );
        assert_eq!(asset.metadata.creator.as_deref(), Some("First"));
        let AssetContent::Image(rgb) = asset.content else {
            panic!("an SVG is an image")
        };
        assert_eq!(rgb.dimensions(), (4, 4));
        assert_eq!(rgb.get_pixel(1, 1).0, [255, 0, 0]);
    }

    #[test]
    fn transparent_areas_are_white() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><rect width="5" height="10" fill="#00f" fill-opacity="0.5"/></svg>"##;
        let asset = read(Path::new("x.svg"), svg.as_bytes()).unwrap();
        let AssetContent::Image(rgb) = asset.content else {
            panic!("an SVG is an image")
        };
        assert_eq!(rgb.get_pixel(8, 5).0, [255, 255, 255]);
        let half = rgb.get_pixel(2, 5).0;
        assert!(half[0] > 120 && half[0] < 135 && half[2] == 255, "{half:?}");
    }

    #[test]
    fn only_relative_image_links_load() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-svg-links");
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("red.png");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save(&png)
            .unwrap();
        let center = |href: &str| {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><image href="{href}" width="4" height="4"/></svg>"#
            );
            let asset = read(&dir.join("x.svg"), svg.as_bytes()).unwrap();
            let AssetContent::Image(rgb) = asset.content else {
                panic!("an SVG is an image")
            };
            rgb.get_pixel(2, 2).0
        };
        assert_eq!(center("red.png"), [255, 0, 0]);
        assert_eq!(center("./red.png"), [255, 0, 0]);
        assert_eq!(center(png.to_str().unwrap()), [255, 255, 255]);

        assert!(is_relative("../images/x.png"));
        assert!(!is_relative("//host/share/x.png"));
        assert!(!is_relative("/etc/x.png"));
        if cfg!(windows) {
            for href in [
                r"\\host\share\x.png",
                r"\\?\UNC\host\share\x.png",
                r"\\.\pipe\x",
                r"\x.png",
                "C:x.png",
            ] {
                assert!(!is_relative(href), "{href}");
            }
        }
    }
}
