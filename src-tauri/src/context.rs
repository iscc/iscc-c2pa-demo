//! Shared c2pa settings: bundled trust lists, soft-binding algorithms and claim generator info.

use std::sync::LazyLock;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use c2pa::crypto::cose::{CertificateTrustPolicy, TrustAnchorType};
use serde_json::{json, Value};

/// Official C2PA trust list (conformance-public/trust-list), so conforming products validate as trusted.
const C2PA_TRUST_LIST: &str = include_str!("../resources/certs/C2PA-TRUST-LIST.pem");
/// Official C2PA time stamping authority trust list.
const C2PA_TSA_TRUST_LIST: &str = include_str!("../resources/certs/C2PA-TSA-TRUST-LIST.pem");
/// Root bundle of the c2pa-rs test certificates, the demo key's among them. Their private keys are
/// public, so they are no trust anchor here; [`signed_with_test_key`] only tells their signers apart.
const TEST_ROOT_BUNDLE: &str = include_str!("../resources/certs/test_cert_root_bundle.pem");
/// Allowed signer EKUs (c2pa-rs `valid_eku_oids.cfg`). Custom trust settings replace the c2pa-rs
/// defaults, and without this list only the emailProtection EKU is accepted, so signers with the
/// C2PA or documentSigning EKU fail with "certificate missing required EKU".
const TRUST_CONFIG: &str = include_str!("../resources/certs/valid_eku_oids.cfg");
/// Built-in demo signing certificate chain (c2pa-rs test fixture, ES256, not for production).
pub const DEMO_SIGN_CERT: &str = include_str!("../resources/certs/es256.pub");
/// Built-in demo private key matching [`DEMO_SIGN_CERT`].
pub const DEMO_SIGN_KEY: &str = include_str!("../resources/certs/es256.pem");
/// Signing algorithm of the built-in demo key.
pub const DEMO_SIGN_ALG: &str = "es256";

/// C2PA soft-binding algorithm identifier for ISCC (registry entry 3).
pub const ISCC_SOFT_BINDING_ALG: &str = "io.iscc.v0";
/// `bindingMetadata.description` of the soft-binding assertion (C2PA spec 2.3+, informational).
pub const ISCC_BINDING_DESCRIPTION: &str =
    "International Standard Content Code (ISCC - ISO 24138:2024) - Open Source Content Identification";
/// `bindingMetadata.contact` of the soft-binding assertion.
pub const ISCC_BINDING_CONTACT: &str = "info@iscc.io";
/// `bindingMetadata.informationalUrl` of the soft-binding assertion: the IEP-0020 page.
pub const ISCC_BINDING_INFO_URL: &str = "https://ieps.iscc.codes/iep-0020/";
/// Name reported as claim generator in signed manifests.
pub const CLAIM_GENERATOR_NAME: &str = "ISCC C2PA Demo";
/// Version of the C2PA specification signed manifests declare they follow (`specVersion` in
/// `claim_generator_info`, spec 2.4+; the Conformance Program requires it). c2pa-rs 0.91 does not
/// set it, and its settings have no field for it: the key goes through their map of other
/// values, which is written into the claim as is (c2pa-rs issue 2828). Never also call
/// `set_spec_version`: the claim would get the key twice. Not 2.5 yet: it is not released, and
/// c2pa-rs writes the parent ingredient's validation results with `trustListUri`, which 2.5
/// deprecates, when the source's signer is on a trust list.
pub const C2PA_SPEC_VERSION: &str = "2.4.0";

/// Base labels (`labels::parse_label`) of the assertions signing lists in `created_assertions`,
/// the ones the specification attributes to the signer. c2pa-rs 0.91 lists every assertion but
/// the hard binding and the actions in `gathered_assertions` by default, which the specification
/// does not attribute to the signer, and which for the parent ingredient breaks the rule that it
/// sits in the same list as the actions that reference it.
pub const CREATED_ASSERTION_LABELS: [&str; 5] = [
    "c2pa.ingredient",
    "c2pa.soft-binding",
    "c2pa.thumbnail.claim",
    "cawg.metadata",
    "cawg.training-mining",
];

/// URI under which [`signed_with_test_key`] checks against the c2pa-rs test root bundle.
const TEST_ROOT_URI: &str = "urn:c2pa-rs:test-root-bundle";

/// URI of the C2PA trust list, reported for signers that chain to it.
pub const C2PA_TRUST_URI: &str =
    "https://github.com/c2pa-org/conformance-public/blob/main/trust-list/C2PA-TRUST-LIST.pem";

/// The TSA trust list without the certificates that are also on the C2PA trust list.
///
/// c2pa-rs 0.91 checks a certificate against every anchor set, whatever its kind, and shuffles
/// the order of the sets when it loads settings (a `HashSet` dedupes them). A signer whose root
/// is on both lists would be reported as found in either list at random. With each certificate
/// in one set only, the report is stable; timestamps still validate, because tokens are checked
/// against every set as well.
static TSA_ONLY_ANCHORS: LazyLock<String> =
    LazyLock::new(|| without_certs_of(C2PA_TSA_TRUST_LIST, C2PA_TRUST_LIST));

/// The certificates of the PEM bundle `pem` that are not in the PEM bundle `other`.
fn without_certs_of(pem: &str, other: &str) -> String {
    let others: Vec<String> = pem_blocks(other).map(pem_body).collect();
    pem_blocks(pem)
        .filter(|block| !others.contains(&pem_body(block)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Each `BEGIN CERTIFICATE` ... `END CERTIFICATE` block of a PEM bundle, markers included.
fn pem_blocks(pem: &str) -> impl Iterator<Item = &str> {
    const END: &str = "-----END CERTIFICATE-----";
    pem.match_indices("-----BEGIN CERTIFICATE-----")
        .filter_map(move |(start, _)| {
            pem[start..]
                .find(END)
                .map(|len| &pem[start..start + len + END.len()])
        })
}

/// Base64 body of a PEM block without whitespace, so line endings do not matter.
fn pem_body(block: &str) -> String {
    block
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .flat_map(str::split_whitespace)
        .collect()
}

/// Base settings JSON shared by reading and signing contexts.
pub fn base_settings() -> Value {
    json!({
        "trust": {
            "anchors": [
                {
                    "trust_anchors": C2PA_TRUST_LIST,
                    "trust_kind": "manifest",
                    "trust_uri": C2PA_TRUST_URI
                },
                {
                    "trust_anchors": TSA_ONLY_ANCHORS.as_str(),
                    "trust_kind": "tsa",
                    "trust_uri": "https://github.com/c2pa-org/conformance-public/blob/main/trust-list/C2PA-TSA-TRUST-LIST.pem"
                }
            ],
            "trust_config": TRUST_CONFIG
        },
        "verify": {
            "remote_manifest_fetch": false,
            "verify_after_sign": true
        },
        "soft_binding": {
            "soft_binding_algorithms": [ISCC_SOFT_BINDING_ALG]
        },
        "builder": {
            "claim_generator_info": {
                "name": CLAIM_GENERATOR_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "specVersion": C2PA_SPEC_VERSION
            },
            // The demo records no action besides `c2pa.opened`, so the spec requires
            // `allActionsIncluded: true`.
            "actions": { "auto_all_actions_included": true },
            // The app writes its own claim thumbnail (`thumbnail.rs`) and none for ingredients.
            "thumbnail": { "enabled": false },
            "created_assertion_labels": CREATED_ASSERTION_LABELS
        }
    })
}

/// Whether the certificate chain `chain_pem` (end entity first, as c2pa's
/// `SignatureInfo::cert_chain` gives it) chains to the c2pa-rs test root bundle: a public test
/// key, the demo key among them, as which anyone can sign. Such a signer is never trusted here.
pub fn signed_with_test_key(chain_pem: &str) -> bool {
    let ders: Vec<Vec<u8>> = pem_blocks(chain_pem)
        .filter_map(|block| B64.decode(pem_body(block)).ok())
        .collect();
    let Some((end_entity, chain)) = ders.split_first() else {
        return false;
    };
    let mut policy = CertificateTrustPolicy::new();
    policy.add_valid_ekus(TRUST_CONFIG.as_bytes());
    let anchors = policy.add_trust_anchors(
        TEST_ROOT_BUNDLE.as_bytes(),
        TEST_ROOT_URI,
        TrustAnchorType::Manifest,
        None,
    );
    anchors.is_ok()
        && policy
            .check_certificate_trust(chain, end_entity, None)
            .is_ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::HashSet;
    use std::io::Cursor;

    use c2pa::assertions::DigitalSourceType;
    use c2pa::{create_signer, Builder, BuilderIntent, Context, Reader, SigningAlg};

    use super::*;

    const EKU_ROOT: &str = include_str!("../tests/fixtures/eku/root_cert.pem");
    const EKU_LEAF: &str = include_str!("../tests/fixtures/eku/leaf_cert.pem");
    const EKU_KEY: &str = include_str!("../tests/fixtures/eku/leaf_key.pem");
    const EKU_BASE: &[u8] = include_bytes!("../tests/fixtures/eku/base.jpg");
    /// Trust list URI of the fixture root.
    pub(crate) const EKU_TRUST_URI: &str = "urn:iscc-c2pa-demo:eku-test-root";

    /// Sign the 2x2 fixture JPEG with a leaf whose only EKU is C2PA claim signing.
    pub(crate) fn sign_with_c2pa_eku() -> Vec<u8> {
        let mut settings = base_settings();
        settings["verify"]["verify_after_sign"] = json!(false);
        let signer = create_signer::from_keys(
            EKU_LEAF.as_bytes(),
            EKU_KEY.as_bytes(),
            SigningAlg::Es256,
            None,
        )
        .unwrap();
        let mut builder = Builder::from_context(Context::new().with_settings(settings).unwrap());
        builder.set_intent(BuilderIntent::Create(DigitalSourceType::Empty));
        let mut dest = Cursor::new(Vec::new());
        builder
            .sign(
                signer.as_ref(),
                "image/jpeg",
                &mut Cursor::new(EKU_BASE),
                &mut dest,
            )
            .unwrap();
        dest.into_inner()
    }

    /// `settings` plus the fixture root as manifest anchor with the URI `EKU_TRUST_URI`.
    pub(crate) fn with_eku_root(mut settings: Value) -> Value {
        settings["trust"]["anchors"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "trust_anchors": EKU_ROOT,
                "trust_kind": "manifest",
                "trust_uri": EKU_TRUST_URI
            }));
        settings
    }

    /// `settings` plus the c2pa-rs test root bundle as manifest anchor, as a validator that
    /// trusts the demo key has it.
    pub(crate) fn with_test_root(mut settings: Value) -> Value {
        settings["trust"]["anchors"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "trust_anchors": TEST_ROOT_BUNDLE,
                "trust_kind": "manifest",
                "trust_uri": TEST_ROOT_URI
            }));
        settings
    }

    /// Validation state of `asset` read with `settings` plus the fixture root as manifest anchor.
    fn validation_state(asset: &[u8], settings: Value) -> String {
        let context = Context::new()
            .with_settings(with_eku_root(settings))
            .unwrap();
        let reader = Reader::from_context(context)
            .with_stream("image/jpeg", Cursor::new(asset))
            .unwrap();
        format!("{:?}", reader.validation_state())
    }

    #[test]
    fn signer_with_c2pa_eku_is_trusted() {
        assert_eq!(
            validation_state(&sign_with_c2pa_eku(), base_settings()),
            "Trusted"
        );
    }

    /// c2pa-rs 0.91 drops its default EKU list when trust anchors come from settings. If this
    /// fails after an upgrade, upstream restored the defaults and `TRUST_CONFIG` may be removable.
    #[test]
    fn signer_with_c2pa_eku_is_invalid_without_trust_config() {
        let mut settings = base_settings();
        settings["trust"]
            .as_object_mut()
            .unwrap()
            .remove("trust_config");
        assert_eq!(validation_state(&sign_with_c2pa_eku(), settings), "Invalid");
    }

    #[test]
    fn the_public_test_key_is_told_apart() {
        assert!(signed_with_test_key(DEMO_SIGN_CERT));
        assert!(!signed_with_test_key(EKU_LEAF));
        assert!(!signed_with_test_key(&format!("{EKU_LEAF}\n{EKU_ROOT}")));
        assert!(!signed_with_test_key(""));
    }

    /// Files signed with the demo key validate, but their signer is on no trust list here.
    #[test]
    fn the_demo_key_is_not_trusted() {
        let signed =
            crate::sign::tests::sign_fixture("no_manifest.jpg", "iscc-c2pa-demo-test-untrusted");
        let context = Context::new().with_settings(base_settings()).unwrap();
        let reader = Reader::from_context(context).with_file(&signed).unwrap();
        assert_eq!(format!("{:?}", reader.validation_state()), "Valid");
        assert_eq!(crate::inspect::signer_trust_list(&reader), None);
    }

    #[test]
    fn every_tsa_certificate_is_in_one_anchor_set() {
        let signers: Vec<String> = pem_blocks(C2PA_TRUST_LIST).map(pem_body).collect();
        let tsa_only: Vec<String> = pem_blocks(&TSA_ONLY_ANCHORS).map(pem_body).collect();
        assert!(!tsa_only.is_empty());
        for cert in pem_blocks(C2PA_TSA_TRUST_LIST).map(pem_body) {
            assert!(signers.contains(&cert) != tsa_only.contains(&cert));
        }
    }

    /// Trust lists reported for the signer of `asset` over 16 reads, with the fixture root on the
    /// manifest list and a TSA list holding `tsa_anchors`.
    fn reported_signer_lists(asset: &[u8], tsa_anchors: &str) -> HashSet<Option<String>> {
        let mut settings = with_eku_root(base_settings());
        settings["trust"]["anchors"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "trust_anchors": tsa_anchors,
                "trust_kind": "tsa",
                "trust_uri": "urn:iscc-c2pa-demo:shared-tsa"
            }));
        (0..16)
            .map(|_| {
                let context = Context::new().with_settings(settings.clone()).unwrap();
                let reader = Reader::from_context(context)
                    .with_stream("image/jpeg", Cursor::new(asset))
                    .unwrap();
                crate::inspect::signer_trust_list(&reader)
            })
            .collect()
    }

    /// A root on both a signer list and a TSA list, like the Trufo root of OpenAI's signer, is
    /// reported on the signer list once the TSA list leaves out the signer list's certificates.
    #[test]
    fn shared_root_is_reported_on_the_signer_list() {
        let asset = sign_with_c2pa_eku();
        let shared = format!("{EKU_ROOT}\n{C2PA_TSA_TRUST_LIST}");
        let filtered = without_certs_of(&shared, EKU_ROOT);
        assert_eq!(
            reported_signer_lists(&asset, &filtered),
            HashSet::from([Some(EKU_TRUST_URI.to_owned())])
        );
        // If this fails after an upgrade, c2pa-rs keeps the anchor order or checks signers
        // against signer lists only, and `TSA_ONLY_ANCHORS` may be removable.
        assert!(reported_signer_lists(&asset, &shared)
            .contains(&Some("urn:iscc-c2pa-demo:shared-tsa".to_owned())));
    }
}
