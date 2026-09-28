//! Shared c2pa settings: bundled trust lists, soft-binding algorithms and claim generator info.

use serde_json::{json, Value};

/// Official C2PA trust list (conformance-public/trust-list), so conforming products validate as trusted.
const C2PA_TRUST_LIST: &str = include_str!("../resources/certs/C2PA-TRUST-LIST.pem");
/// Official C2PA time stamping authority trust list.
const C2PA_TSA_TRUST_LIST: &str = include_str!("../resources/certs/C2PA-TSA-TRUST-LIST.pem");
/// Root bundle of the c2pa-rs test certificates, so assets signed with the built-in demo key validate.
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

/// Base settings JSON shared by reading and signing contexts.
pub fn base_settings() -> Value {
    json!({
        "trust": {
            "anchors": [
                {
                    "trust_anchors": C2PA_TRUST_LIST,
                    "trust_kind": "manifest",
                    "trust_uri": "https://github.com/c2pa-org/conformance-public/blob/main/trust-list/C2PA-TRUST-LIST.pem"
                },
                {
                    "trust_anchors": C2PA_TSA_TRUST_LIST,
                    "trust_kind": "tsa",
                    "trust_uri": "https://github.com/c2pa-org/conformance-public/blob/main/trust-list/C2PA-TSA-TRUST-LIST.pem"
                },
                {
                    "trust_anchors": TEST_ROOT_BUNDLE,
                    "trust_kind": "manifest",
                    "trust_uri": "urn:c2pa-rs:test-root-bundle"
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
                "version": env!("CARGO_PKG_VERSION")
            },
            // The demo records no action besides `c2pa.opened`, so the spec requires
            // `allActionsIncluded: true`.
            "actions": { "auto_all_actions_included": true }
        }
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use c2pa::assertions::DigitalSourceType;
    use c2pa::{create_signer, Builder, BuilderIntent, Context, Reader, SigningAlg};

    use super::*;

    const EKU_ROOT: &str = include_str!("../tests/fixtures/eku/root_cert.pem");
    const EKU_LEAF: &str = include_str!("../tests/fixtures/eku/leaf_cert.pem");
    const EKU_KEY: &str = include_str!("../tests/fixtures/eku/leaf_key.pem");
    const EKU_BASE: &[u8] = include_bytes!("../tests/fixtures/eku/base.jpg");

    /// Sign the 2x2 fixture JPEG with a leaf whose only EKU is C2PA claim signing.
    fn sign_with_c2pa_eku() -> Vec<u8> {
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

    /// Validation state of `asset` read with `settings` plus the fixture root as manifest anchor.
    fn validation_state(asset: &[u8], mut settings: Value) -> String {
        settings["trust"]["anchors"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "trust_anchors": EKU_ROOT,
                "trust_kind": "manifest",
                "trust_uri": "urn:iscc-c2pa-demo:eku-test-root"
            }));
        let context = Context::new().with_settings(settings).unwrap();
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
}
