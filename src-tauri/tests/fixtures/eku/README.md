# EKU fixtures

Copied from `contentauth/c2pa-conformance-tool` (`wasm/tests/fixtures/eku`, commit `ec754fd`,
Apache-2.0). Test-only keys, worthless outside the tests.

- `root_cert.pem`: self-signed test CA.
- `leaf_cert.pem` / `leaf_key.pem`: ES256 leaf chained to the root whose only Extended Key Usage
  is the C2PA claim-signing OID `1.3.6.1.4.1.62558.2.1`. It is deliberately not
  `id-kp-emailProtection`, which c2pa-rs accepts regardless of `trust.trust_config`.
- `base.jpg`: unsigned 2x2 JPEG, signed with the leaf key at test time.

Used by the tests in `src/context.rs` to check that `trust.trust_config` keeps signers with the
C2PA or documentSigning EKU trusted.
