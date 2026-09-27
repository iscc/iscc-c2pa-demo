//! Create, sign and embed a C2PA manifest carrying an ISCC soft binding and, optionally,
//! a CAWG training and data mining assertion and an RFC 3161 timestamp.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use c2pa::assertions::{labels, DigitalSourceType, Metadata, SoftBinding};
use c2pa::{create_signer, BoxedSigner, Builder, BuilderIntent, Context, Reader, SigningAlg};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::asset;
use crate::context::{self, base_settings, ISCC_SOFT_BINDING_ALG};
use crate::inspect::{self, Inspection, TRAINING_MINING_LABEL};
use crate::iscc::{self, IsccUnit, MetaInput, UnitSelection};
use crate::metadata;
use crate::timestamp::{BestEffortTsa, FailureSlot};

/// Signing request as sent by the UI.
#[derive(Deserialize, Debug)]
pub struct SignRequest {
    pub source: String,
    pub output: String,
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// ISCC metadata embedded in the source (`iscc:meta`), passed through unchanged so the signed
    /// Meta-Code equals the one shown for the source.
    #[serde(default)]
    pub meta: Option<String>,
    /// Digital source type URI used for the `c2pa.created` action of new manifests.
    pub source_type: String,
    /// ISCC unit slugs to embed: meta, the Content-Code (image, text or audio), data, instance.
    pub units: Vec<String>,
    /// CAWG training-mining entries keyed by use case label (e.g. `cawg.ai_training`).
    #[serde(default)]
    pub training: BTreeMap<String, TrainingEntry>,
    pub credentials: Credentials,
    /// Timestamp service (RFC 3161) to countersign the signature; none when absent.
    #[serde(default)]
    pub tsa_url: Option<String>,
}

/// One entry of the CAWG training and data mining assertion.
#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct TrainingEntry {
    #[serde(rename = "use")]
    pub use_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub constraint_info: Option<String>,
}

/// Which signing key to use.
#[derive(Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Credentials {
    /// Bundled c2pa-rs test certificate.
    Demo,
    /// User supplied PEM files.
    Custom {
        cert_path: String,
        key_path: String,
        alg: String,
    },
}

/// Outcome of signing, including a fresh inspection of the output file.
#[derive(Serialize, Debug)]
pub struct SignResult {
    pub output: String,
    pub units: Vec<IsccUnit>,
    pub iscc_seq_base64: String,
    pub timestamp: TimestampOutcome,
    pub inspection: Inspection,
}

/// Whether the timestamp service countersigned the signature. The inspection of the output
/// tells whether the timestamp is trusted and who issued it.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum TimestampOutcome {
    /// No timestamp was requested.
    Off,
    /// The service's token is part of the signature.
    Added,
    /// The service failed, so the file was signed without a timestamp.
    Failed { url: String, reason: String },
}

/// Sign `request.source` into `request.output`.
pub fn sign(request: &SignRequest) -> Result<SignResult> {
    let source = Path::new(&request.source);
    let output = Path::new(&request.output);
    if same_file(source, output) {
        bail!("choose an output path different from the source file");
    }
    let bytes =
        std::fs::read(source).with_context(|| format!("cannot read {}", source.display()))?;
    let format = inspect::asset_format(source)?;
    let mime = format.mime;

    let selection = UnitSelection::from_slugs(&request.units);
    let (content_bytes, _) = inspect::content_bytes(source, &bytes, mime);
    let asset = asset::read(source, &content_bytes, format)?;
    let meta = MetaInput {
        name: Some(&request.title),
        description: request.description.as_deref(),
        meta: request.meta.as_deref(),
    };
    let units = iscc::units_for(&content_bytes, asset.content(), meta, &selection)?;
    if units.is_empty() {
        bail!("select at least one ISCC unit for the soft binding");
    }
    let unit_strings: Vec<String> = units.iter().map(|u| u.iscc.clone()).collect();
    let seq = iscc::encode_seq(&unit_strings)?;

    let tsa_url = tsa_url(request)?;
    let (signer, tsa_failure) = claim_signer(&request.credentials, tsa_url)?;
    let context = Context::new()
        .with_settings(base_settings())?
        .with_signer(signer);
    let mut builder = Builder::from_context(context).with_definition(json!({
        "title": request.title,
        "format": mime,
    }))?;
    builder.set_intent(intent_for(source, &request.source_type)?);

    let soft_binding: SoftBinding = serde_json::from_value(json!({
        "alg": ISCC_SOFT_BINDING_ALG,
        "blocks": [{ "scope": {}, "value": seq }],
        "bindingMetadata": {
            "description": context::ISCC_BINDING_DESCRIPTION,
            "contact": context::ISCC_BINDING_CONTACT,
            "informationalUrl": context::ISCC_BINDING_INFO_URL,
        },
    }))?;
    builder.add_assertion(labels::SOFT_BINDING, &soft_binding)?;

    if selection.meta {
        builder.add_assertion_json(labels::CAWG_METADATA, &meta_assertion(meta)?)?;
    }
    if !request.training.is_empty() {
        builder.add_assertion(
            TRAINING_MINING_LABEL,
            &json!({ "entries": request.training }),
        )?;
    }

    // c2pa refuses to write onto an existing file, so sign into a sibling temp file and move it
    // over the output only once signing succeeded; a failure leaves any previous output intact.
    let tmp = temp_sibling(output)?;
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = builder.save_to_file(source, &tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!(e).context("signing failed"));
    }
    std::fs::rename(&tmp, output)
        .with_context(|| format!("cannot replace {}", output.display()))?;
    let inspection = inspect::inspect(output)?;
    Ok(SignResult {
        output: output.to_string_lossy().into_owned(),
        units,
        iscc_seq_base64: B64.encode(&seq),
        timestamp: timestamp_outcome(tsa_url, tsa_failure),
        inspection,
    })
}

/// The requested timestamp service URL, which must be http or https.
fn tsa_url(request: &SignRequest) -> Result<Option<&str>> {
    let Some(url) = request.tsa_url.as_deref() else {
        return Ok(None);
    };
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("the timestamp service URL must start with http:// or https://: {url}");
    }
    Ok(Some(url))
}

/// The claim signer and, with a timestamp service, the slot its failure is reported in. The
/// signer then timestamps best effort: a failing service leaves the signature without one.
fn claim_signer(
    credentials: &Credentials,
    tsa_url: Option<&str>,
) -> Result<(BoxedSigner, Option<FailureSlot>)> {
    let (alg, cert, key) = load_credentials(credentials)?;
    let alg: SigningAlg = alg
        .parse()
        .map_err(|_| anyhow!("unknown signature algorithm {alg}"))?;
    // With a URL c2pa reserves room for the token in the signature.
    let signer = create_signer::from_keys(
        cert.as_bytes(),
        key.as_bytes(),
        alg,
        tsa_url.map(str::to_owned),
    )
    .context("cannot use the signing certificate and key")?;
    let Some(url) = tsa_url else {
        return Ok((signer, None));
    };
    let tsa = BestEffortTsa::new(signer, url);
    let failure = tsa.failure_handle();
    Ok((Box::new(tsa), Some(failure)))
}

/// What became of the requested timestamp, once signing is done.
fn timestamp_outcome(url: Option<&str>, failure: Option<FailureSlot>) -> TimestampOutcome {
    let Some(url) = url else {
        return TimestampOutcome::Off;
    };
    let reason = failure.and_then(|f| f.lock().unwrap_or_else(|p| p.into_inner()).take());
    match reason {
        Some(reason) => TimestampOutcome::Failed {
            url: url.to_owned(),
            reason,
        },
        None => TimestampOutcome::Added,
    }
}

/// CAWG metadata assertion (JSON-LD, Dublin Core) with the title and description the Meta-Code
/// was computed from, trimmed as the Meta-Code trims them; an empty description is left out.
/// `c2pa.metadata` does not allow `dc:title` or `dc:description`, `cawg.metadata` does.
fn meta_assertion(meta: MetaInput<'_>) -> Result<Metadata> {
    let mut jsonld = json!({
        "@context": { "dc": metadata::NS_DC },
        "dc:title": meta.name.unwrap_or_default().trim(),
    });
    if let Some(description) = meta.description.map(str::trim).filter(|d| !d.is_empty()) {
        jsonld["dc:description"] = json!(description);
    }
    Ok(Metadata::new(labels::CAWG_METADATA, &jsonld.to_string())?)
}

/// True when both paths name the same file, including paths that differ only in case or via links.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

/// Temporary sibling of `output` with the same extension, so c2pa still detects the format.
fn temp_sibling(output: &Path) -> Result<std::path::PathBuf> {
    let name = output
        .file_name()
        .ok_or_else(|| anyhow!("output path has no file name"))?;
    Ok(output.with_file_name(format!("~{}", name.to_string_lossy())))
}

/// Edit intent when the source already carries a manifest store, otherwise a creation.
fn intent_for(source: &Path, source_type: &str) -> Result<BuilderIntent> {
    let has_manifest = match Reader::from_context(Context::new().with_settings(base_settings())?)
        .with_file(source)
    {
        Ok(_) => true,
        Err(c2pa::Error::JumbfNotFound) => false,
        Err(e) => bail!("existing manifest cannot be read: {e}"),
    };
    if has_manifest {
        return Ok(BuilderIntent::Edit);
    }
    let dst: DigitalSourceType = serde_json::from_value(json!(source_type))
        .with_context(|| format!("unknown digital source type {source_type}"))?;
    Ok(BuilderIntent::Create(dst))
}

/// Resolve credentials to (algorithm, certificate chain PEM, private key PEM).
fn load_credentials(credentials: &Credentials) -> Result<(String, String, String)> {
    match credentials {
        Credentials::Demo => Ok((
            context::DEMO_SIGN_ALG.to_owned(),
            context::DEMO_SIGN_CERT.to_owned(),
            context::DEMO_SIGN_KEY.to_owned(),
        )),
        Credentials::Custom {
            cert_path,
            key_path,
            alg,
        } => {
            let cert = std::fs::read_to_string(cert_path)
                .with_context(|| format!("cannot read {cert_path}"))?;
            let key = std::fs::read_to_string(key_path)
                .with_context(|| format!("cannot read {key_path}"))?;
            Ok((alg.to_lowercase(), cert, key))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn expected_metadata() -> inspect::BindingMetadata {
        inspect::BindingMetadata {
            description: Some(context::ISCC_BINDING_DESCRIPTION.into()),
            contact: Some(context::ISCC_BINDING_CONTACT.into()),
            informational_url: Some(context::ISCC_BINDING_INFO_URL.into()),
        }
    }

    fn request(source: &str, output: &Path) -> SignRequest {
        let mut training = BTreeMap::new();
        training.insert(
            "cawg.ai_training".to_owned(),
            TrainingEntry {
                use_: "notAllowed".into(),
                constraint_info: None,
            },
        );
        training.insert(
            "cawg.data_mining".to_owned(),
            TrainingEntry {
                use_: "constrained".into(),
                constraint_info: Some("research only".into()),
            },
        );
        SignRequest {
            source: fixture(source),
            output: output.to_string_lossy().into_owned(),
            title: "Demo asset".into(),
            description: Some("Signed in a unit test".into()),
            meta: None,
            source_type: "http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture".into(),
            units: ["meta", "image", "data", "instance"]
                .map(String::from)
                .to_vec(),
            training,
            credentials: Credentials::Demo,
            tsa_url: None,
        }
    }

    /// The match row of the embedded unit `name`.
    fn unit_match<'a>(sb: &'a inspect::SoftBindingSummary, name: &str) -> &'a inspect::UnitMatch {
        sb.matches
            .iter()
            .find(|m| m.embedded.name == name)
            .expect("unit is embedded")
    }

    /// Sign `no_manifest.jpg` with the default request into the temporary directory `name`.
    fn sign_no_manifest(name: &str) -> SignResult {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        sign(&request(
            "no_manifest.jpg",
            &dir.join("no_manifest-signed.jpg"),
        ))
        .unwrap()
    }

    #[test]
    fn signed_jpeg_matches_its_embedded_units() {
        let result = sign_no_manifest("iscc-c2pa-demo-test-match");
        assert_eq!(result.units.len(), 4);
        assert_eq!(result.timestamp, TimestampOutcome::Off);
        // Bitstream units are computed without the manifest store on both sides, so they match.
        assert!(result.inspection.iscc_excludes_manifest);
        let sb = &result
            .inspection
            .manifest
            .as_ref()
            .expect("manifest present")
            .soft_bindings[0];
        assert_eq!(unit_match(sb, "Content-Code Image").similarity, Some(1.0));
        // Title and description typed at signing are stored in cawg.metadata and recomputed.
        assert_eq!(unit_match(sb, "Meta-Code").similarity, Some(1.0));
        assert_eq!(unit_match(sb, "Data-Code").similarity, Some(1.0));
        assert_eq!(unit_match(sb, "Instance-Code").similarity, Some(1.0));
    }

    #[test]
    fn sign_new_manifest_and_read_it_back() {
        let result = sign_no_manifest("iscc-c2pa-demo-test-create");
        let manifest = result.inspection.manifest.expect("manifest present");
        assert_eq!(
            manifest.validation_state, "Trusted",
            "{:?}",
            manifest.validation
        );
        assert_eq!(manifest.title.as_deref(), Some("Demo asset"));
        assert!(manifest
            .assertions
            .iter()
            .any(|a| a.label == "c2pa.actions" || a.label.starts_with("c2pa.actions")));

        let sb = &manifest.soft_bindings[0];
        assert_eq!(sb.alg.as_deref(), Some("io.iscc.v0"));
        assert_eq!(sb.units, result.units);
        assert_eq!(sb.metadata, Some(expected_metadata()));
        // The map must be in the signed file itself, not only in the summary.
        let assertion = manifest
            .assertions
            .iter()
            .find(|a| a.label.starts_with(labels::SOFT_BINDING))
            .unwrap();
        assert_eq!(
            assertion.data["bindingMetadata"]["informationalUrl"],
            context::ISCC_BINDING_INFO_URL
        );

        let training = manifest.training_mining.expect("training-mining assertion");
        assert_eq!(training["entries"]["cawg.ai_training"]["use"], "notAllowed");
        assert_eq!(
            training["entries"]["cawg.data_mining"]["constraint_info"],
            "research only"
        );

        let stored = manifest
            .assertions
            .iter()
            .find(|a| a.label == labels::CAWG_METADATA)
            .expect("cawg.metadata assertion");
        assert_eq!(stored.data["@context"]["dc"], metadata::NS_DC);
        assert_eq!(stored.data["dc:title"], "Demo asset");
        assert_eq!(stored.data["dc:description"], "Signed in a unit test");
        let fields = &result.inspection.meta_fields;
        assert_eq!(fields.name_source, "manifest");
        assert_eq!(fields.description.as_deref(), Some("Signed in a unit test"));
    }

    #[test]
    fn meta_assertion_only_with_meta_code_and_without_empty_description() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-no-meta");
        std::fs::create_dir_all(&dir).unwrap();
        let mut req = request("no_manifest.jpg", &dir.join("no-meta.jpg"));
        req.units = vec!["image".into()];
        let manifest = sign(&req).unwrap().inspection.manifest.unwrap();
        assert!(!manifest
            .assertions
            .iter()
            .any(|a| a.label.starts_with(labels::CAWG_METADATA)));

        let stored = meta_assertion(MetaInput {
            name: Some("  Title "),
            description: Some("  "),
            meta: None,
        })
        .unwrap();
        assert!(stored.is_valid());
        let json = serde_json::to_value(&stored).unwrap();
        assert_eq!(json["dc:title"], "Title");
        assert!(json.get("dc:description").is_none());
    }

    #[test]
    fn resigning_a_signed_file_keeps_the_stored_meta_code_inputs() {
        let first = sign_no_manifest("iscc-c2pa-demo-test-resign");
        let signed = &first.inspection;
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-resign");
        let output = dir.join("resigned.jpg");
        let mut req = request("no_manifest.jpg", &output);
        req.source = signed.path.clone();
        req.title = signed.meta_fields.name.clone();
        req.description = signed.meta_fields.description.clone();
        let result = sign(&req).unwrap();

        let manifest = result.inspection.manifest.as_ref().unwrap();
        assert_eq!(manifest.ingredients.len(), 1, "Edit intent");
        assert_eq!(result.inspection.meta_fields, signed.meta_fields);
        let meta = unit_match(&manifest.soft_bindings[0], "Meta-Code");
        assert_eq!(meta.similarity, Some(1.0));
        assert_eq!(meta.embedded.iscc, first.units[0].iscc);
    }

    #[test]
    fn sign_asset_with_existing_manifest_adds_parent_ingredient() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-edit");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("CA-signed.jpg");
        let mut req = request("CA.jpg", &output);
        req.training.clear();
        req.units = vec!["image".into(), "instance".into()];
        let result = sign(&req).unwrap();

        let manifest = result.inspection.manifest.expect("manifest present");
        assert!(manifest.manifest_count >= 2);
        assert_eq!(manifest.ingredients.len(), 1);
        assert_eq!(manifest.ingredients[0].relationship, "ParentOf");
        assert!(manifest.training_mining.is_none());
        assert_eq!(manifest.soft_bindings[0].units.len(), 2);
        // The source already carried a manifest; stripping it on both sides keeps the match exact.
        let instance = unit_match(&manifest.soft_bindings[0], "Instance-Code");
        assert_eq!(instance.similarity, Some(1.0));
    }

    // Windows and macOS file systems are case-insensitive by default.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn sign_rejects_output_that_is_the_source_in_different_case() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-same");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("same.jpg");
        std::fs::copy(fixture("no_manifest.jpg"), &source).unwrap();
        let mut req = request("no_manifest.jpg", &dir.join("SAME.JPG"));
        req.source = source.to_string_lossy().into_owned();
        let err = sign(&req).unwrap_err().to_string();
        assert!(err.contains("different from the source"), "{err}");
        assert!(source.exists(), "source must survive a rejected request");
    }

    #[test]
    fn failed_signing_keeps_previous_output() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-keep");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("kept.jpg");
        std::fs::write(&output, b"previous").unwrap();
        let mut req = request("no_manifest.jpg", &output);
        req.credentials = Credentials::Custom {
            cert_path: fixture("expected_iscc.json"),
            key_path: fixture("expected_iscc.json"),
            alg: "es256".into(),
        };
        assert!(sign(&req).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"previous");
        assert!(
            !dir.join("~kept.jpg").exists(),
            "temp file must be cleaned up"
        );

        req.credentials = Credentials::Demo;
        sign(&req).unwrap();
        assert!(
            std::fs::read(&output).unwrap().len() > 8,
            "output replaced on success"
        );
        assert!(!dir.join("~kept.jpg").exists());
    }

    #[test]
    fn sign_epub_and_read_it_back() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-epub");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("demo-signed.epub");
        let mut req = request("demo.epub", &output);
        req.units = ["meta", "text", "data", "instance"]
            .map(String::from)
            .to_vec();
        let result = sign(&req).unwrap();

        assert_eq!(result.units.len(), 4);
        let inspection = &result.inspection;
        assert_eq!(inspection.kind, crate::formats::Kind::Text);
        assert!(inspection.iscc_excludes_manifest);
        assert!(
            inspection.preview.starts_with("data:image/jpeg;base64,"),
            "cover preview"
        );
        assert_eq!(inspection.meta_fields.name, "title from metadata");
        assert_eq!(inspection.meta_fields.name_source, "metadata");
        let manifest = inspection.manifest.as_ref().expect("manifest present");
        assert_eq!(
            manifest.validation_state, "Trusted",
            "{:?}",
            manifest.validation
        );
        let sb = &manifest.soft_bindings[0];
        assert_eq!(sb.units, result.units);
        assert_eq!(sb.metadata, Some(expected_metadata()));
        assert_eq!(unit_match(sb, "Content-Code Text").similarity, Some(1.0));
        // The book's own title outranks the one typed at signing.
        assert!(unit_match(sb, "Meta-Code").similarity.unwrap() < 1.0);
        // Removing a manifest from a zip rewrites the central directory and leaves the manifest
        // bytes behind, so the bitstream units of the stripped file only approximate those of the
        // source. Data-Code varies with those (partly random) bytes: 94-100% over 15 signatures.
        let data = unit_match(sb, "Data-Code").similarity.unwrap();
        assert!(data > 0.9, "Data-Code similarity {data}");
        assert!(unit_match(sb, "Instance-Code").similarity.unwrap() < 1.0);
    }

    #[test]
    fn signing_with_prefilled_fields_reproduces_the_meta_code() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-meta");
        std::fs::create_dir_all(&dir).unwrap();
        // Titled by its file name (the manifest title takes over) and titled through XMP.
        for (file, source_after) in [
            ("no_manifest.jpg", "manifest"),
            ("meta-xmp.jpg", "metadata"),
        ] {
            let source = inspect::inspect(Path::new(&fixture(file))).unwrap();
            let output = dir.join(file);
            let mut req = request(file, &output);
            req.title = source.meta_fields.name.clone();
            req.description = source.meta_fields.description.clone();
            req.meta = source.meta_fields.meta.clone();
            let result = sign(&req).unwrap();

            let inspection = &result.inspection;
            assert_eq!(
                inspection.meta_fields.name, source.meta_fields.name,
                "{file}"
            );
            assert_eq!(inspection.meta_fields.name_source, source_after, "{file}");
            let manifest = inspection.manifest.as_ref().unwrap();
            let meta = unit_match(&manifest.soft_bindings[0], "Meta-Code");
            assert_eq!(meta.similarity, Some(1.0), "{file}");
            assert_eq!(
                meta.embedded.iscc, source.iscc[0].iscc,
                "{file}: embedded equals the source's Meta-Code"
            );
        }
    }

    #[test]
    fn tampered_file_keeps_content_match_and_loses_instance_match() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-tamper");
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("tampered.jpg");
        sign(&request("no_manifest.jpg", &output)).unwrap();
        // Flip one bit in the entropy-coded data near the end, away from markers and stuffing.
        let mut bytes = std::fs::read(&output).unwrap();
        let pos = (bytes.len() - 64..bytes.len() - 2)
            .find(|&i| bytes[i] < 0xFE && bytes[i - 1] != 0xFF)
            .unwrap();
        bytes[pos] ^= 0x01;
        std::fs::write(&output, &bytes).unwrap();

        let inspection = inspect::inspect(&output).unwrap();
        let manifest = inspection.manifest.expect("manifest still readable");
        assert_eq!(manifest.validation_state, "Invalid");
        // One flipped bit: pixels practically unchanged, bytes nearly equal, exact hash gone.
        assert!(
            unit_match(&manifest.soft_bindings[0], "Content-Code Image")
                .similarity
                .unwrap()
                > 0.95
        );
        assert!(
            unit_match(&manifest.soft_bindings[0], "Data-Code")
                .similarity
                .unwrap()
                > 0.9
        );
        assert!(
            unit_match(&manifest.soft_bindings[0], "Instance-Code")
                .similarity
                .unwrap()
                < 1.0
        );
    }

    #[test]
    fn webp_and_tiff_sign_and_verify_content() {
        // c2pa-rs 0.91 rewrites the RIFF size (WebP) and appends a new IFD (TIFF) when embedding,
        // and cannot strip either back to the original; Content-Code is unaffected.
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-formats");
        std::fs::create_dir_all(&dir).unwrap();
        let img = image::RgbImage::from_fn(96, 64, |x, y| {
            image::Rgb([(x * 2) as u8, (y * 3) as u8, ((x + y) % 256) as u8])
        });
        for ext in ["webp", "tif"] {
            let source = dir.join(format!("synthetic.{ext}"));
            img.save(&source).unwrap();
            let output = dir.join(format!("synthetic-signed.{ext}"));
            let mut req = request("no_manifest.jpg", &output);
            req.source = source.to_string_lossy().into_owned();
            let result = sign(&req).unwrap();

            let manifest = result
                .inspection
                .manifest
                .as_ref()
                .expect("manifest present");
            assert_eq!(
                manifest.validation_state, "Trusted",
                "{ext}: {:?}",
                manifest.validation
            );
            assert_eq!(
                unit_match(&manifest.soft_bindings[0], "Content-Code Image").similarity,
                Some(1.0),
                "{ext}"
            );
            // Stripping a WebP keeps its manifest, and the panel must not claim otherwise. (TIFF
            // stripping unlinks the manifest but leaves its bytes and a new IFD behind.)
            if ext == "webp" {
                assert!(!result.inspection.iscc_excludes_manifest);
            }
        }
    }

    /// What removing the manifest from a signed copy restores.
    #[derive(Clone, Copy, PartialEq)]
    enum Strip {
        /// The source's bytes, exactly.
        Exact,
        /// Bytes close to the source's.
        Traces,
        /// Nothing: c2pa-rs reports success but keeps the manifest.
        Kept,
    }

    #[test]
    fn every_format_signs_and_matches_its_content() {
        // Stripping restores TXT, Markdown, GIF and M4A byte for byte. Zip containers keep the
        // removed manifest's bytes and a rewritten directory (Data-Code 69-92% over five
        // signatures of these small files), SVG keeps the c2pa namespace declaration (67%), MP3
        // its ID3 tag as c2pa-rs rewrote it (v2.4, no padding), FLAC the empty ID3 tag c2pa-rs
        // put in front. c2pa-rs 0.91 cannot remove the manifest chunk from a WAV, as from WebP.
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-formats-all");
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("demo.txt", Strip::Exact),
            ("demo.md", Strip::Exact),
            ("demo.gif", Strip::Exact),
            ("demo.m4a", Strip::Exact),
            ("demo.mp3", Strip::Traces),
            ("demo.flac", Strip::Traces),
            ("demo.wav", Strip::Kept),
            ("demo.svg", Strip::Traces),
            ("demo.docx", Strip::Traces),
            ("demo.pptx", Strip::Traces),
            ("demo.xlsx", Strip::Traces),
            ("demo.odt", Strip::Traces),
            ("demo.ods", Strip::Traces),
            ("demo.odp", Strip::Traces),
        ];
        for (file, strip) in cases {
            let source = inspect::inspect(Path::new(&fixture(file))).unwrap();
            let mut req = request(file, &dir.join(file));
            req.title = source.meta_fields.name.clone();
            req.description = source.meta_fields.description.clone();
            req.meta = source.meta_fields.meta.clone();
            req.units = ["meta", source.kind.slug(), "data", "instance"]
                .map(String::from)
                .to_vec();
            let result = sign(&req).unwrap();

            assert_eq!(
                result.inspection.iscc_excludes_manifest,
                strip != Strip::Kept,
                "{file}"
            );
            let manifest = result.inspection.manifest.as_ref().unwrap();
            assert_eq!(manifest.validation_state, "Trusted", "{file}");
            let similarity = |i: usize| manifest.soft_bindings[0].matches[i].similarity.unwrap();
            assert_eq!(similarity(0), 1.0, "{file} Meta-Code");
            assert_eq!(similarity(1), 1.0, "{file} Content-Code");
            if strip == Strip::Exact {
                assert_eq!((similarity(2), similarity(3)), (1.0, 1.0), "{file}");
            } else {
                assert!(similarity(2) > 0.5, "{file} Data-Code {}", similarity(2));
                assert!(similarity(3) < 1.0, "{file} Instance-Code");
            }
        }
    }

    /// Sign `no_manifest.jpg` through the timestamp service `url` into the directory `name`.
    fn sign_with_tsa(name: &str, url: &str) -> Result<SignResult> {
        let dir = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let mut req = request("no_manifest.jpg", &dir.join("timestamped.jpg"));
        req.tsa_url = Some(url.to_owned());
        sign(&req)
    }

    #[test]
    fn unreachable_timestamp_service_signs_without_timestamp() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("http://127.0.0.1:{port}/");
        let result = sign_with_tsa("iscc-c2pa-demo-test-tsa-down", &url).unwrap();
        let TimestampOutcome::Failed {
            url: failed,
            reason,
        } = &result.timestamp
        else {
            panic!("{:?}", result.timestamp);
        };
        assert_eq!(failed, &url);
        assert_eq!(reason, "no connection");
        let manifest = result.inspection.manifest.unwrap();
        assert_eq!(manifest.validation_state, "Trusted");
        assert_eq!(manifest.signature.unwrap().timestamp.status, "none");
    }

    #[test]
    fn timestamp_service_url_must_be_http() {
        let err = sign_with_tsa("iscc-c2pa-demo-test-tsa-url", "ftp://example.com/tsa")
            .unwrap_err()
            .to_string();
        assert!(err.contains("http:// or https://"), "{err}");
    }

    /// Real round trip to the default service; run before a release with `--ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn default_timestamp_service_is_trusted() {
        let result =
            sign_with_tsa("iscc-c2pa-demo-test-tsa", crate::timestamp::DEFAULT_TSA_URL).unwrap();
        assert_eq!(result.timestamp, TimestampOutcome::Added);
        let manifest = result.inspection.manifest.unwrap();
        assert_eq!(manifest.validation_state, "Trusted");
        let timestamp = manifest.signature.unwrap().timestamp;
        assert_eq!(timestamp.status, "trusted");
        assert_eq!(timestamp.tsa.as_deref(), Some("Encypher Corp."));
    }

    #[test]
    fn inspect_fixture_without_manifest() {
        let inspection = inspect::inspect(Path::new(&fixture("no_manifest.jpg"))).unwrap();
        assert!(inspection.manifest.is_none());
        assert!(inspection.manifest_error.is_none());
        assert!(!inspection.iscc_excludes_manifest);
        assert_eq!(inspection.iscc.len(), 4);
        assert_eq!(inspection.iscc[0].name, "Meta-Code");
        assert_eq!(inspection.meta_fields.name, "no manifest");
        assert_eq!(inspection.meta_fields.name_source, "filename");
        assert!(inspection.preview.starts_with("data:image/jpeg;base64,"));
    }
}
