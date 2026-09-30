//! Create, sign and embed a C2PA manifest carrying an ISCC soft binding, a claim thumbnail when
//! the source has a picture and, optionally, a CAWG training and data mining assertion and an
//! RFC 3161 timestamp.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{anyhow, bail, Context as _, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use c2pa::assertions::{labels, DigitalSourceType, Metadata, SoftBinding};
use c2pa::{create_signer, BoxedSigner, Builder, BuilderIntent, Context, SigningAlg};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::asset::{self, Asset};
use crate::context::{self, base_settings, ISCC_SOFT_BINDING_ALG};
use crate::inspect::{self, Inspection, TRAINING_MINING_LABEL};
use crate::iscc::{self, IsccUnit, MetaInput, UnitSelection};
use crate::metadata;
use crate::thumbnail::{self, THUMBNAIL_EDGE, THUMBNAIL_MIME, THUMBNAIL_QUALITY};
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
    /// Digital source type URI recorded on the parent ingredient when the source carries no
    /// Content Credentials; a source with Content Credentials keeps its own history instead.
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

    // IEP-0020: every unit is generated from the source as a whole, including any Content
    // Credentials it already carries.
    let selection = UnitSelection::from_slugs(&request.units);
    let asset = asset::read(source, &bytes, format)?;
    let meta = MetaInput {
        name: Some(&request.title),
        description: request.description.as_deref(),
        meta: request.meta.as_deref(),
    };
    let units = iscc::units_for(&bytes, asset.content(), meta, &selection)?;
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
    builder.set_intent(BuilderIntent::Edit);
    add_parent(&mut builder, source, &bytes, mime, &request.source_type)?;
    add_thumbnail(&mut builder, &asset)?;

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

/// Record the source as the parent ingredient; with the edit intent c2pa then adds the
/// `c2pa.opened` action that references it. A source without Content Credentials gets the
/// digital source type chosen by the user.
fn add_parent(
    builder: &mut Builder,
    source: &Path,
    bytes: &[u8],
    mime: &str,
    source_type: &str,
) -> Result<()> {
    let title = source.file_name().map(|n| n.to_string_lossy().into_owned());
    let parent = builder.add_ingredient_from_stream(
        json!({ "title": title, "relationship": "parentOf" }).to_string(),
        mime,
        &mut Cursor::new(bytes),
    )?;
    if parent.active_manifest().is_none() {
        let dst: DigitalSourceType = serde_json::from_value(json!(source_type))
            .with_context(|| format!("unknown digital source type {source_type}"))?;
        parent.set_digital_source_type(dst);
    }
    Ok(())
}

/// Set the claim thumbnail from the asset's picture: the image itself, a cover, a saved document
/// thumbnail or cover art. An asset without a picture gets none.
fn add_thumbnail(builder: &mut Builder, asset: &Asset) -> Result<()> {
    if let Some(picture) = asset.picture() {
        let jpeg = thumbnail::scaled_jpeg(picture, THUMBNAIL_EDGE, THUMBNAIL_QUALITY)?;
        builder.set_thumbnail(THUMBNAIL_MIME, &mut Cursor::new(jpeg))?;
    }
    Ok(())
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
    use crate::inspect::Preservation;
    use c2pa::ValidationState;

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
        let manifest = result
            .inspection
            .manifest
            .as_ref()
            .expect("manifest present");
        // Embedding a JPEG only inserts the manifest store, so the source view is the source.
        let sb = &manifest.soft_bindings[0];
        assert_eq!(sb.preservation, Some(Preservation::Preserved));
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
        // The units describe the source as a whole, its old manifest store included, which
        // embedding replaced: the Instance-Code identifies the source, not this file.
        let source = std::fs::read(fixture("CA.jpg")).unwrap();
        let instance = unit_match(&manifest.soft_bindings[0], "Instance-Code");
        assert_eq!(instance.embedded, iscc::instance_unit(&source).unwrap());
        assert!(instance.similarity < Some(1.0));
        assert_eq!(
            manifest.soft_bindings[0].preservation,
            Some(Preservation::Resigned)
        );
        assert_eq!(manifest.ingredients[0].digital_source_type, None);
    }

    #[test]
    fn unsigned_source_records_opened_action() {
        let result = sign_no_manifest("iscc-c2pa-demo-test-opened");
        let manifest = result.inspection.manifest.expect("manifest present");
        assert_eq!(manifest.ingredients.len(), 1);
        let parent = &manifest.ingredients[0];
        assert_eq!(parent.relationship, "ParentOf");
        assert_eq!(parent.title.as_deref(), Some("no_manifest.jpg"));
        assert_eq!(
            parent.digital_source_type.as_deref(),
            Some("http://cv.iptc.org/newscodes/digitalsourcetype/digitalCapture")
        );
        let actions = &manifest
            .assertions
            .iter()
            .find(|a| a.label.starts_with(labels::ACTIONS))
            .expect("actions assertion")
            .data;
        let list = actions["actions"].as_array().unwrap();
        assert_eq!(list.len(), 1, "{actions}");
        assert_eq!(list[0]["action"], "c2pa.opened");
        assert!(list[0].get("digitalSourceType").is_none());
        assert_eq!(actions["allActionsIncluded"], true, "{actions}");
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
        // A zip has a collection hash, not a data hash, so there is no source view: the units
        // are compared with the whole file, whose manifest entry and rewritten directory differ
        // from the source.
        assert_eq!(sb.preservation, Some(Preservation::NoSourceView));
        let data = unit_match(sb, "Data-Code").similarity.unwrap();
        assert!(data > 0.5, "Data-Code similarity {data}");
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
        assert_eq!(
            manifest.soft_bindings[0].preservation,
            Some(Preservation::FileChanged)
        );
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

    /// Replace the embedded Instance-Code of the signed file `path` with the one the file's
    /// source view has now, as an attacker would to fake source preservation.
    fn forge_instance_code(path: &Path) {
        let inspection = inspect::inspect(path).unwrap();
        let sb = &inspection.manifest.expect("manifest present").soft_bindings[0];
        let row = unit_match(sb, "Instance-Code");
        let raw = |iscc: &str| crate::iscc::encode_seq(&[iscc.to_owned()]).unwrap();
        let (embedded, forged) = (raw(&row.embedded.iscc), raw(row.computed.as_ref().unwrap()));
        let mut bytes = std::fs::read(path).unwrap();
        let pos = bytes
            .windows(embedded.len())
            .position(|w| w == embedded)
            .expect("Instance-Code stored uncompressed");
        bytes[pos..pos + forged.len()].copy_from_slice(&forged);
        std::fs::write(path, &bytes).unwrap();
    }

    #[test]
    fn forged_instance_code_is_not_source_preservation() {
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-forged");
        std::fs::create_dir_all(&dir).unwrap();

        // Bytes appended after signing, Instance-Code replaced with the one of the altered file.
        let jpeg = dir.join("appended.jpg");
        sign(&request("no_manifest.jpg", &jpeg)).unwrap();
        let mut bytes = std::fs::read(&jpeg).unwrap();
        bytes.extend_from_slice(b"appended after signing");
        std::fs::write(&jpeg, &bytes).unwrap();
        forge_instance_code(&jpeg);

        // Hard binding intact, but a WebP is not source-preserving: forge the proof it lacks.
        let webp = dir.join("synthetic.webp");
        image::RgbImage::from_fn(64, 48, |x, y| image::Rgb([x as u8, y as u8, 128]))
            .save(&webp)
            .unwrap();
        let signed_webp = dir.join("synthetic-signed.webp");
        let mut req = request("no_manifest.jpg", &signed_webp);
        req.source = webp.to_string_lossy().into_owned();
        sign(&req).unwrap();
        forge_instance_code(&signed_webp);

        for path in [jpeg, signed_webp] {
            let manifest = inspect::inspect(&path).unwrap().manifest.unwrap();
            assert_eq!(
                manifest.soft_bindings[0].preservation,
                Some(Preservation::SignatureInvalid),
                "{}: {:?}",
                path.display(),
                manifest.validation
            );
        }
    }

    #[test]
    fn webp_and_tiff_sign_and_verify_content() {
        // c2pa-rs 0.91 rewrites the RIFF size (WebP) and appends a new IFD (TIFF) when embedding,
        // so neither is source-preserving; Content-Code is unaffected.
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
            assert_eq!(
                manifest.soft_bindings[0].preservation,
                Some(Preservation::Changed),
                "{ext}"
            );
        }
    }

    #[test]
    fn source_preservation_per_format() {
        // Measured with c2pa-rs 0.91. Preserved: embedding only inserts the manifest store.
        // Changed: SVG gains a namespace declaration (and a metadata wrapper), MP3 gets its ID3
        // tag rewritten as v2.4, FLAC an ID3 tag in front, WAV a new RIFF size, TIFF a new IFD.
        // No source view: zip containers (collection hash) and M4A (BMFF hash).
        use Preservation::*;
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-formats-all");
        std::fs::create_dir_all(&dir).unwrap();
        let cases = [
            ("demo.txt", Preserved),
            ("demo.md", Preserved),
            ("demo.gif", Preserved),
            ("libpng-test.png", Preserved),
            ("demo.m4a", NoSourceView),
            ("demo.mp3", Changed),
            ("demo.flac", Changed),
            ("demo.wav", Changed),
            ("demo.svg", Changed),
            ("demo.tif", Changed),
            ("demo.docx", NoSourceView),
            ("demo.pptx", NoSourceView),
            ("demo.xlsx", NoSourceView),
            ("demo.odt", NoSourceView),
            ("demo.ods", NoSourceView),
            ("demo.odp", NoSourceView),
        ];
        let mut failures = Vec::new();
        for (file, expected) in cases {
            let source = inspect::inspect(Path::new(&fixture(file))).unwrap();
            let mut req = request(file, &dir.join(file));
            req.title = source.meta_fields.name.clone();
            req.description = source.meta_fields.description.clone();
            req.meta = source.meta_fields.meta.clone();
            req.units = ["meta", source.kind.slug(), "data", "instance"]
                .map(String::from)
                .to_vec();
            let result = sign(&req).unwrap();

            // The embedded units are those shown for the source.
            assert_eq!(result.units, source.iscc, "{file}");
            let manifest = result.inspection.manifest.as_ref().unwrap();
            assert_eq!(manifest.validation_state, "Trusted", "{file}");
            let sb = &manifest.soft_bindings[0];
            let similarity = |i: usize| sb.matches[i].similarity.unwrap();
            if sb.preservation != Some(expected) {
                failures.push(format!("{file}: {:?}", sb.preservation));
                continue;
            }
            assert_eq!(similarity(0), 1.0, "{file} Meta-Code");
            assert_eq!(similarity(1), 1.0, "{file} Content-Code");
            if expected == Preserved {
                assert_eq!((similarity(2), similarity(3)), (1.0, 1.0), "{file}");
            } else {
                assert!(similarity(2) > 0.5, "{file} Data-Code {}", similarity(2));
                assert!(similarity(3) < 1.0, "{file} Instance-Code");
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Open the manifest store of `path` with the app's settings.
    fn reader(path: &Path) -> c2pa::Reader {
        let context = Context::new().with_settings(base_settings()).unwrap();
        c2pa::Reader::from_context(context).with_file(path).unwrap()
    }

    /// Sign the fixture `file` with only Data- and Instance-Code into the directory `dir`.
    fn sign_fixture(file: &str, dir: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join(file);
        let mut req = request(file, &output);
        req.units = vec!["data".into(), "instance".into()];
        sign(&req).unwrap();
        output
    }

    #[test]
    fn signed_files_carry_one_jpeg_thumbnail() {
        let with_picture = [
            "no_manifest.jpg",
            "libpng-test.png",
            "meta-xmp.webp",
            "demo.gif",
            "demo.tif",
            "demo.svg",
            "demo.epub",
            "demo.odt",
            "demo.pptx",
            "withcover.mp3",
        ];
        let without_picture = [
            "demo.docx",
            "demo.xlsx",
            "demo.txt",
            "demo.md",
            "demo.mp3",
            "demo.m4a",
        ];
        let cases = with_picture
            .iter()
            .map(|f| (*f, true))
            .chain(without_picture.iter().map(|f| (*f, false)));
        for (file, has_picture) in cases {
            let output = sign_fixture(file, "iscc-c2pa-demo-test-thumbnails");
            let reader = reader(&output);
            assert_eq!(
                reader.validation_state(),
                ValidationState::Trusted,
                "{file}"
            );
            let manifest = reader.active_manifest().unwrap();
            let parent = &manifest.ingredients()[0];
            assert!(
                parent.thumbnail_ref().is_none(),
                "{file}: ingredient thumbnail"
            );
            let shown = inspect::inspect(&output)
                .unwrap()
                .manifest
                .unwrap()
                .thumbnail;
            assert_eq!(
                shown.starts_with("data:image/jpeg;base64,"),
                has_picture,
                "{file}: thumbnail for the UI"
            );
            let thumbnail = manifest.thumbnail();
            assert_eq!(thumbnail.is_some(), has_picture, "{file}: claim thumbnail");
            let Some((format, jpeg)) = thumbnail else {
                continue;
            };
            assert_eq!(format, THUMBNAIL_MIME, "{file}");
            assert!(jpeg.len() < 12_000, "{file}: {} bytes", jpeg.len());
            let decoded =
                image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
            assert!(
                decoded.width().max(decoded.height()) <= THUMBNAIL_EDGE,
                "{file}"
            );
        }
    }

    #[test]
    fn signed_source_keeps_its_parents_thumbnail() {
        let output = sign_fixture("CA.jpg", "iscc-c2pa-demo-test-parent-thumbnail");
        let reader = reader(&output);
        let manifest = reader.active_manifest().unwrap();
        let (format, jpeg) = manifest.thumbnail().expect("claim thumbnail");
        assert_eq!(format, THUMBNAIL_MIME);
        let decoded = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg).unwrap();
        assert_eq!(decoded.width().max(decoded.height()), THUMBNAIL_EDGE);

        let parent = &manifest.ingredients()[0];
        let parent_label = parent.active_manifest().expect("parent manifest");
        let identifier = &parent.thumbnail_ref().expect("parent thumbnail").identifier;
        assert!(
            identifier.contains(parent_label),
            "{identifier} is not in {parent_label}"
        );
    }

    #[test]
    fn signing_adds_little_to_a_jpeg() {
        let result = sign_no_manifest("iscc-c2pa-demo-test-growth");
        let source = std::fs::metadata(fixture("no_manifest.jpg")).unwrap().len();
        let signed = std::fs::metadata(&result.output).unwrap().len();
        assert!(
            signed - source < 16_000,
            "grew by {} bytes",
            signed - source
        );
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
        assert_eq!(inspection.iscc.len(), 4);
        assert_eq!(inspection.iscc[0].name, "Meta-Code");
        assert_eq!(inspection.meta_fields.name, "no manifest");
        assert_eq!(inspection.meta_fields.name_source, "filename");
        assert!(inspection.preview.starts_with("data:image/jpeg;base64,"));
    }

    /// The trust list shown is the active signer's, even when a parent's signer chains to
    /// another list. c2pa's `trustListUri` can name the parent's list, in no stable order, so
    /// read several times.
    #[test]
    fn trust_list_is_the_active_signers() {
        use crate::context::tests::{sign_with_c2pa_eku, with_eku_root, EKU_TRUST_URI};
        let dir = std::env::temp_dir().join("iscc-c2pa-demo-test-trust-list");
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("eku.jpg");
        std::fs::write(&source, sign_with_c2pa_eku()).unwrap();
        let output = dir.join("eku-signed.jpg");
        let _ = std::fs::remove_file(&output);
        let mut req = request("eku/base.jpg", &output);
        req.source = source.to_string_lossy().into_owned();
        sign(&req).unwrap();

        let settings = with_eku_root(context::base_settings());
        let mut c2pa_uris = std::collections::HashSet::new();
        for _ in 0..16 {
            let context = Context::new().with_settings(settings.clone()).unwrap();
            let reader = c2pa::Reader::from_context(context)
                .with_file(&output)
                .unwrap();
            assert_eq!(
                inspect::signer_trust_list(&reader).as_deref(),
                Some("urn:c2pa-rs:test-root-bundle")
            );
            let results = serde_json::to_value(reader.validation_results()).unwrap();
            c2pa_uris.insert(results["trustListUri"].as_str().map(str::to_owned));
        }
        // If this fails after an upgrade, c2pa-rs names the active signer's list itself and
        // `signer_trust_list` may be replaceable by `trustListUri`.
        assert!(
            c2pa_uris.contains(&Some(EKU_TRUST_URI.to_owned())),
            "{c2pa_uris:?}"
        );
    }
}
