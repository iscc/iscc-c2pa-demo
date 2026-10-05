//! Semantic-Code Image and Semantic-Code Text, the experimental ISCC units: the sign bits of a
//! neural embedding. rten runs weight-compressed copies of the iscc-sci and iscc-sct models
//! in-process; tools.rs installs them on first use. All maths stays fp32, only the stored
//! weights are smaller, so codes differ from iscc-sci and iscc-sct by a few bits at most (1 and 2
//! of 256 measured on the test sets).
//!
//! Each kind has a model of its own, installed and switched on separately ([`SemanticKinds`]).
//! Each computation loads its model, runs it and drops it, so the app holds no model while it
//! idles. A small memo keyed by the model input remembers the units of the session: a file, its
//! source view, its signed copy and that copy reopened usually decode to the same pixels or text,
//! so each is embedded once.

mod image;
mod text;

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{bail, Result};
use iscc_lib::codec::{self, MainType, SubType, Version};
use serde::{Deserialize, Serialize};

use crate::formats::Kind;
use crate::iscc::{self, Content, IsccUnit, UNIT_BITS};
use crate::tools;
use crate::video::Progress;

pub use text::{char_offsets, chunks, TokenSizer};

/// Most units the memo keeps before it starts over.
const MEMO_CAPACITY: usize = 64;

/// Units computed in this session, by the BLAKE3 hash of their model input.
static MEMO: Mutex<Option<HashMap<[u8; 32], IsccUnit>>> = Mutex::new(None);

/// A kind of Semantic-Code, each computed by a model of its own.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SemanticKind {
    /// Semantic-Code Image, from the ISC21 descriptor model of iscc-sci.
    Image,
    /// Semantic-Code Text, from the multilingual MiniLM model of iscc-sct.
    Text,
}

impl SemanticKind {
    /// Both kinds, in display order.
    pub const ALL: [SemanticKind; 2] = [SemanticKind::Image, SemanticKind::Text];

    /// The kind of Semantic-Code an asset of `kind` has; audio and video have none.
    pub fn of(kind: Kind) -> Option<SemanticKind> {
        match kind {
            Kind::Image => Some(SemanticKind::Image),
            Kind::Text => Some(SemanticKind::Text),
            Kind::Audio | Kind::Video => None,
        }
    }

    /// Name of the unit, as the unit list shows it.
    pub fn unit_name(self) -> &'static str {
        match self {
            SemanticKind::Image => "Semantic-Code Image",
            SemanticKind::Text => "Semantic-Code Text",
        }
    }
}

/// Which kinds of Semantic-Code are switched on; both off by default, because the Semantic-Codes
/// are experimental and each model is a large download.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SemanticKinds {
    pub image: bool,
    pub text: bool,
}

impl SemanticKinds {
    /// No Semantic-Code at all.
    pub const NONE: SemanticKinds = SemanticKinds {
        image: false,
        text: false,
    };
    /// Both kinds.
    pub const ALL: SemanticKinds = SemanticKinds {
        image: true,
        text: true,
    };

    /// Whether `kind` is switched on.
    pub fn has(self, kind: SemanticKind) -> bool {
        match kind {
            SemanticKind::Image => self.image,
            SemanticKind::Text => self.text,
        }
    }

    /// Whether an asset of `kind` gets its Semantic-Code; audio and video never do.
    pub fn includes(self, kind: Kind) -> bool {
        SemanticKind::of(kind).is_some_and(|k| self.has(k))
    }

    /// These kinds with `kind` switched on or off.
    pub fn with(self, kind: SemanticKind, on: bool) -> SemanticKinds {
        match kind {
            SemanticKind::Image => SemanticKinds { image: on, ..self },
            SemanticKind::Text => SemanticKinds { text: on, ..self },
        }
    }

    /// The kinds switched on here whose models are installed: what the app computes.
    pub fn installed(self) -> SemanticKinds {
        let on = |kind| self.has(kind) && tools::semantic_installed(kind);
        SemanticKinds {
            image: on(SemanticKind::Image),
            text: on(SemanticKind::Text),
        }
    }
}

/// Whether `content` can have a Semantic-Code: an image or a text.
pub fn applies(content: &Content<'_>) -> bool {
    matches!(content, Content::Image(_) | Content::Text(_))
}

/// The Semantic-Code of `content`: Semantic-Code Image of an image, Semantic-Code Text of a
/// text. `progress` hears the share of a text's chunks embedded and stops the run by returning
/// false. Without its kind's model the error is [`tools::Missing::SemanticModel`]; content
/// without text or of another kind has none.
pub fn unit(content: Content<'_>, progress: Progress) -> Result<IsccUnit> {
    let (kind, key) = match content {
        Content::Image(rgb) => (SemanticKind::Image, image::key(rgb)),
        Content::Text(text) => (SemanticKind::Text, text::key(text)),
        Content::Unavailable(reason) => bail!("{reason}"),
        Content::Audio(_) | Content::Video(_) => bail!("audio and video have no Semantic-Code"),
    };
    // In the order of `tools::semantic_files`: the model, then the tokenizer of a text.
    let files = tools::semantic_paths(kind)?;
    if let Some(unit) = recall(&key) {
        return Ok(unit);
    }
    let unit = match content {
        Content::Image(rgb) => encode(SubType::Image, &image::embedding(&files[0], rgb)?)?,
        Content::Text(text) => encode(
            SubType::TEXT,
            &text::embedding(&files[0], &files[1], text, progress)?,
        )?,
        _ => unreachable!("rejected above"),
    };
    keep(key, &unit);
    Ok(unit)
}

/// The unit of `subtype` from the first 256 components of `embedding`.
fn encode(subtype: SubType, embedding: &[f32]) -> Result<IsccUnit> {
    let digest = binarize(embedding);
    let code =
        codec::encode_component(MainType::Semantic, subtype, Version::V0, UNIT_BITS, &digest)?;
    iscc::describe_unit(&code)
}

/// Sign bits of the first 256 components, most significant bit first: a component of zero or
/// more gives 1, as `vec >= 0` does in iscc-sci and iscc-sct.
fn binarize(embedding: &[f32]) -> [u8; 32] {
    let mut digest = [0u8; 32];
    for (i, v) in embedding.iter().take(256).enumerate() {
        if *v >= 0.0 {
            digest[i / 8] |= 0x80 >> (i % 8);
        }
    }
    digest
}

/// The unit kept for the model input `key`.
fn recall(key: &[u8; 32]) -> Option<IsccUnit> {
    let memo = MEMO.lock().unwrap_or_else(|p| p.into_inner());
    memo.as_ref()?.get(key).cloned()
}

/// Keep `unit` for the model input `key`; a full memo starts over.
fn keep(key: [u8; 32], unit: &IsccUnit) {
    let mut memo = MEMO.lock().unwrap_or_else(|p| p.into_inner());
    let map = memo.get_or_insert_with(HashMap::new);
    if map.len() >= MEMO_CAPACITY {
        map.clear();
    }
    map.insert(key, unit.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_kinds_switch_each_kind_on_its_own() {
        assert_eq!(SemanticKinds::default(), SemanticKinds::NONE);
        let image = SemanticKinds::NONE.with(SemanticKind::Image, true);
        assert!(image.includes(Kind::Image));
        assert!(!image.includes(Kind::Text));
        assert!(!SemanticKinds::ALL.includes(Kind::Audio));
        assert!(!SemanticKinds::ALL.includes(Kind::Video));
        assert_eq!(image.with(SemanticKind::Text, true), SemanticKinds::ALL);
        let text = SemanticKinds::ALL.with(SemanticKind::Image, false);
        assert!(!text.has(SemanticKind::Image) && text.has(SemanticKind::Text));
        assert_eq!(SemanticKinds::NONE.installed(), SemanticKinds::NONE);
        assert_eq!(SemanticKind::of(Kind::Text), Some(SemanticKind::Text));
        assert_eq!(SemanticKind::of(Kind::Video), None);
        let kind: SemanticKind = serde_json::from_str("\"image\"").unwrap();
        assert_eq!(kind, SemanticKind::Image);
    }

    #[test]
    fn binarize_takes_signs_msb_first() {
        let mut v = vec![-1.0f32; 384];
        v[0] = 0.5;
        v[9] = 0.0; // zero counts as positive
        v[10] = -0.0; // so does negative zero, as in numpy
        v[255] = 1e-9;
        v[256] = 1.0; // beyond the 256 bits of the code
        let digest = binarize(&v);
        assert_eq!(digest[0], 0b1000_0000);
        assert_eq!(digest[1], 0b0110_0000);
        assert_eq!(digest[31], 0b0000_0001);
        assert!(digest[2..31].iter().all(|b| *b == 0));
    }

    #[test]
    fn semantic_units_carry_their_subtype() {
        let image = encode(SubType::Image, &[1.0; 256]).unwrap();
        assert_eq!(image.name, "Semantic-Code Image");
        assert!(image.iscc.starts_with("ISCC:CED"), "{}", image.iscc);
        let text = encode(SubType::TEXT, &[-1.0; 384]).unwrap();
        assert_eq!(text.name, "Semantic-Code Text");
        assert!(text.iscc.starts_with("ISCC:CAD"), "{}", text.iscc);
    }

    #[test]
    fn memo_remembers_and_starts_over_when_full() {
        let unit = encode(SubType::TEXT, &[1.0; 256]).unwrap();
        let first = *blake3::hash(b"memo test first").as_bytes();
        keep(first, &unit);
        assert_eq!(recall(&first), Some(unit.clone()));
        for i in 0..MEMO_CAPACITY {
            keep(
                *blake3::hash(format!("memo test {i}").as_bytes()).as_bytes(),
                &unit,
            );
        }
        assert_eq!(recall(&first), None, "the memo started over");
    }

    /// The reference values `tests/fixtures/expected_semantic.py` wrote with iscc-sci and
    /// iscc-sct and their fp32 models.
    fn reference() -> serde_json::Value {
        serde_json::from_str(include_str!("../../tests/fixtures/expected_semantic.json")).unwrap()
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    /// Most bits of 256 a code of the compressed models may differ from the fp32 reference.
    const BUDGET: u32 = 16;

    /// Bits in which two units differ.
    fn distance(a: &str, b: &str) -> u32 {
        let similarity = iscc::similarity(a, b).unwrap().expect("units of one kind");
        ((1.0 - similarity) * 256.0).round() as u32
    }

    /// The instruction set rten picks on this CPU, for the drift report.
    fn instruction_set() -> &'static str {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx512f") {
                return "x86_64 AVX-512";
            }
            if std::arch::is_x86_feature_detected!("avx2") {
                return "x86_64 AVX2";
            }
            "x86_64 generic"
        }
        #[cfg(target_arch = "aarch64")]
        {
            "aarch64 NEON"
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            "generic"
        }
    }

    /// The text this app extracts from the text fixture `name`.
    fn fixture_text(name: &str) -> String {
        let path = fixture(name);
        let bytes = std::fs::read(&path).unwrap();
        let format = crate::formats::by_path(&path).unwrap();
        match crate::asset::read(&path, &bytes, format).unwrap().content {
            crate::asset::AssetContent::Text(text) => text,
            other => panic!("{name}: no text, {other:?}"),
        }
    }

    /// iscc-sct's synthetic chunking cases, built as `expected_semantic.py` builds them.
    fn generated(name: &str) -> String {
        let level3 = format!(
            "{}\n\n",
            "Ein kurzer Absatz über die Dinge des Lebens. ".repeat(5)
        )
        .repeat(100)
            + "\n\nEnde.";
        match name {
            "cjk-pathological" => {
                "数据是新的石油它推动着现代经济的发展与变革。".repeat(500) + "\n\n完"
            }
            "long-word-pathological" => "hypermodularization".repeat(600) + " Ende\n\nEnde.",
            "unk-runs-pathological" => format!("{} ", "𓀀".repeat(100)).repeat(120) + "\n\nEnde.",
            "nbsp-pathological" => vec!["Inhalt"; 4000].join("\u{a0}") + "\n\nEnde.",
            "mixed-level-pathological" => level3,
            "crlf" => level3.replace('\n', "\r\n"),
            "whitespace-only" => "  \t \n\n     ".to_owned(),
            other => panic!("unknown case {other}"),
        }
    }

    /// The embedding tokenizer, from the installed models.
    fn encoder() -> tokenizers::Tokenizer {
        crate::tools::tests::ensure_semantic();
        let files = tools::semantic_paths(SemanticKind::Text).unwrap();
        tokenizers::Tokenizer::from_file(&files[1]).unwrap()
    }

    /// Check the chunks of `text` against the reference entry `want`: code point offsets,
    /// lengths and the hash of the joined chunks; and that the chunks cover the text.
    fn assert_chunks(name: &str, text: &str, want: &serde_json::Value) {
        let options = &want["options"];
        let max_tokens = options["max_tokens"]
            .as_u64()
            .map_or(text::MAX_TOKENS, |n| n as usize);
        let overlap = options["overlap"]
            .as_u64()
            .map_or(text::OVERLAP, |n| n as usize);
        let sizer = TokenSizer::new(&encoder(), text, max_tokens).unwrap();
        let got = chunks(&sizer, text, max_tokens, overlap).unwrap();
        let bytes: Vec<usize> = got.iter().map(|(b, _)| *b).collect();
        let join = |v: Vec<String>| v.join(" ");
        let offsets = join(
            char_offsets(text, &bytes)
                .iter()
                .map(usize::to_string)
                .collect(),
        );
        let sizes = join(
            got.iter()
                .map(|(_, c)| c.chars().count().to_string())
                .collect(),
        );
        let joined: Vec<&str> = got.iter().map(|(_, c)| *c).collect();
        let hash = blake3::hash(joined.join("\u{1f}").as_bytes())
            .to_hex()
            .to_string();
        assert_eq!(offsets, want["offsets"].as_str().unwrap(), "{name} offsets");
        assert_eq!(sizes, want["sizes"].as_str().unwrap(), "{name} sizes");
        assert_eq!(
            hash,
            want["chunks_blake3"].as_str().unwrap(),
            "{name} chunks"
        );
        // With whitespace kept, the chunks cover the text, overlapping or adjoining.
        let mut end = 0;
        for (offset, chunk) in &got {
            assert!(*offset <= end, "{name}: gap before byte {offset}");
            assert_eq!(&text[*offset..*offset + chunk.len()], *chunk);
            end = end.max(offset + chunk.len());
        }
        assert_eq!(end, text.len(), "{name}: the chunks reach the end");
    }

    #[test]
    fn tokenizer_counts_and_truncates_like_iscc_sct() {
        let encoder = encoder();
        let hello = encoder.encode("Hello World", true).unwrap();
        assert_eq!(hello.get_ids(), [0, 35378, 6661, 2]);
        let long = "word ".repeat(300);
        let encoded = encoder.encode(long.as_str(), true).unwrap();
        assert_eq!(
            encoded.get_ids().len(),
            128,
            "truncated as tokenizer.json declares"
        );
        let sizer = TokenSizer::new(&encoder, &long, text::MAX_TOKENS).unwrap();
        assert_eq!(
            text_splitter::ChunkSizer::size(&sizer, &long),
            300,
            "never truncated"
        );
    }

    #[test]
    fn chunks_match_iscc_sct() {
        let reference = reference();
        for (name, want) in reference["texts"].as_object().unwrap() {
            assert_chunks(name, want["text"].as_str().unwrap(), want);
        }
        for (name, want) in reference["generated"].as_object().unwrap() {
            let text = generated(name);
            let hash = blake3::hash(text.as_bytes()).to_hex().to_string();
            assert_eq!(hash, want["text_blake3"].as_str().unwrap(), "{name} text");
            assert_chunks(name, &text, want);
        }
        for (name, want) in reference["documents"].as_object().unwrap() {
            assert_chunks(name, &fixture_text(name), want);
        }
    }

    /// Whether the preprocessed image equals iscc-sci's model input bit for bit.
    fn same_input(name: &str, want: &serde_json::Value) -> bool {
        let rgb = iscc::decode_rgb(&std::fs::read(fixture(name)).unwrap()).unwrap();
        let tensor = image::preprocess(&rgb);
        let plane = 512 * 512;
        let samples = [
            tensor[0],
            tensor[plane + 255 * 512 + 255],
            tensor[2 * plane + 511 * 512 + 511],
            tensor[256 * 512],
        ];
        let want_samples: Vec<f32> = want["samples"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        let min = tensor.iter().copied().fold(f32::MAX, f32::min);
        let max = tensor.iter().copied().fold(f32::MIN, f32::max);
        let bytes: Vec<u8> = tensor.iter().flat_map(|v| v.to_le_bytes()).collect();
        samples.to_vec() == want_samples
            && min == want["min"].as_f64().unwrap() as f32
            && max == want["max"].as_f64().unwrap() as f32
            && blake3::hash(&bytes).to_hex().as_str() == want["tensor_blake3"].as_str().unwrap()
    }

    /// Whether `name` is a JPEG: the image crate's decoder is not libjpeg-turbo, which Pillow
    /// uses, so its pixels differ slightly (see [`is_gradient_jpeg`]).
    fn is_jpeg(name: &str) -> bool {
        name.ends_with(".jpg")
    }

    /// The `meta-*.jpg` fixtures: one 48x32 colour gradient with 4:2:0 chroma under different
    /// metadata. On a picture without content many components of the embedding sit near zero,
    /// so the decoder's few differing pixels move 58 bits; fed Pillow's pixels, the model is 0
    /// bits off. Photos decoded by both decoders are 0 to 3 bits apart.
    fn is_gradient_jpeg(name: &str) -> bool {
        name.starts_with("meta-") && is_jpeg(name)
    }

    #[test]
    fn image_input_equals_iscc_sci_preprocessing() {
        let reference = reference();
        let images = reference["images"].as_object().unwrap();
        let exact: Vec<&String> = images.keys().filter(|n| !is_jpeg(n)).collect();
        assert!(exact.len() >= 8, "PNG, GIF, TIFF and WebP fixtures");
        for name in exact {
            assert!(
                same_input(name, &images[name]),
                "{name}: model input differs from iscc-sci's"
            );
        }
    }

    #[test]
    fn codes_stay_within_the_budget_of_the_fp32_reference() {
        crate::tools::tests::ensure_semantic();
        let reference = reference();
        let never = |_: Option<f64>| true;
        let mut report = Vec::new();
        for (name, want) in reference["images"].as_object().unwrap() {
            if is_gradient_jpeg(name) {
                continue;
            }
            let rgb = iscc::decode_rgb(&std::fs::read(fixture(name)).unwrap()).unwrap();
            let got = unit(Content::Image(&rgb), &never).unwrap();
            report.push((
                name.clone(),
                distance(&got.iscc, want["code"].as_str().unwrap()),
            ));
        }
        for (name, want) in reference["texts"].as_object().unwrap() {
            let got = unit(Content::Text(want["text"].as_str().unwrap()), &never).unwrap();
            report.push((
                name.clone(),
                distance(&got.iscc, want["code"].as_str().unwrap()),
            ));
        }
        for (name, want) in reference["documents"].as_object().unwrap() {
            let text = fixture_text(name);
            if text.chars().count() > 50_000 {
                continue; // a thousand chunks; the chunking test covers it
            }
            let got = unit(Content::Text(&text), &never).unwrap();
            report.push((
                name.clone(),
                distance(&got.iscc, want["code"].as_str().unwrap()),
            ));
        }
        let worst = report.iter().map(|(_, d)| *d).max().unwrap();
        println!(
            "{} ({} codes, worst {worst} of 256 bits): {report:?}",
            instruction_set(),
            report.len()
        );
        for (name, d) in &report {
            assert!(
                *d <= BUDGET,
                "{name}: {d} of 256 bits off the fp32 reference"
            );
        }
    }

    #[test]
    fn text_embedding_reports_progress_and_stops() {
        crate::tools::tests::ensure_semantic();
        let text = "A sentence about the weather in spring. ".repeat(80);
        let shares = std::cell::RefCell::new(Vec::new());
        let embedded = unit(Content::Text(&text), &|share| {
            shares.borrow_mut().push(share.unwrap());
            true
        });
        assert!(embedded.is_ok());
        let shares = shares.into_inner();
        assert!(shares.len() > 1, "several chunks");
        assert_eq!(shares.last(), Some(&1.0));
        let other = "Another sentence about the weather in autumn. ".repeat(80);
        let stopped = unit(Content::Text(&other), &|_| false).unwrap_err();
        assert!(
            stopped.downcast_ref::<crate::tools::Cancelled>().is_some(),
            "{stopped}"
        );
    }

    #[test]
    fn only_images_and_texts_have_a_semantic_code() {
        let none = |_: Option<f64>| -> bool { panic!("nothing to embed") };
        let audio = unit(Content::Audio(&[1, 2, 3]), &none).unwrap_err();
        assert!(audio.to_string().contains("no Semantic-Code"), "{audio}");
        let reason = "no text found in this document";
        let empty = unit(Content::Unavailable(reason), &none).unwrap_err();
        assert_eq!(empty.to_string(), reason);
    }
}
