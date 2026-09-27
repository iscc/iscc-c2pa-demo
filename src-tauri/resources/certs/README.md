# Certificates and trust settings

Compiled into the binary with `include_str!` (see `src/context.rs`). All files are unmodified
copies of their upstream versions.

| File | Source | Licence |
|---|---|---|
| `C2PA-TRUST-LIST.pem`, `C2PA-TSA-TRUST-LIST.pem` | [c2pa-org/conformance-public](https://github.com/c2pa-org/conformance-public) `trust-list/`: the C2PA trust list and the C2PA TSA trust list | CC-BY-4.0, by the C2PA ([licence](https://github.com/c2pa-org/conformance-public/blob/main/LICENSE)) |
| `es256.pem`, `es256.pub` | [c2pa-rs](https://github.com/contentauth/c2pa-rs) `sdk/tests/fixtures/certs/`: the ES256 test certificate chain and its private key | MIT or Apache-2.0 |
| `test_cert_root_bundle.pem` | c2pa-rs `sdk/tests/fixtures/certs/trust/`: the roots of the c2pa-rs test certificates | MIT or Apache-2.0 |
| `valid_eku_oids.cfg` | c2pa-rs `sdk/src/crypto/cose/valid_eku_oids.cfg`: the extended key usages c2pa-rs accepts for signers | MIT or Apache-2.0 |

`es256.pem` is a publicly known test key, published with c2pa-rs for testing. It is not a
leaked secret, and files signed with it prove nothing about who signed them. The app uses it as
its built-in demo signer and trusts its root only inside the app; other validators report the
signer as unknown.
