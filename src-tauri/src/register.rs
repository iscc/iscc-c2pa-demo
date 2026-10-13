//! Register a file's manifest store on the ISCC test network: upload it to the test store, which
//! keeps it, serves it byte for byte and declares its ISCC soft binding on the ISCC Discovery
//! Protocol, so that the resolver finds it a minute or so later. The store answers at once with
//! the manifest's address and where its declaration stands. The whole manifest store leaves the
//! machine, with its thumbnail, title and signer, and stays public; the file itself never does.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::lookup::{self, STORE_HOST};
use crate::sign::SIDECAR_EXTENSION;

/// The store declares during the upload, which waits for the hub.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Largest manifest store the test store takes.
const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
/// Largest answer read.
const MAX_ANSWER_BYTES: u64 = 64 * 1024;

/// The test store's answer to an upload: the manifest it keeps, where it serves it, and its
/// declaration on the test network.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Registration {
    #[serde(rename(deserialize = "manifestId"))]
    pub manifest_id: String,
    #[serde(rename(deserialize = "manifestUrl"))]
    pub manifest_url: String,
    pub declaration: Declaration,
}

/// Where the declaration of an uploaded manifest stands.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq)]
pub struct Declaration {
    /// `declared` (on the hub, with an ISCC-ID), `pending` (the store retries), `failed`,
    /// `skipped` (no 256-bit Data-Code and Instance-Code), `disabled` or `withdrawn`.
    pub status: String,
    #[serde(default, rename(deserialize = "isccId"))]
    pub iscc_id: Option<String>,
    /// The hub that holds the declaration.
    #[serde(default)]
    pub hub: Option<String>,
}

/// Register the manifest store of the file at `path` on the test network.
pub fn register(path: &Path) -> Result<Registration> {
    register_at(&format!("https://{STORE_HOST}"), path)
}

/// [`register`] with the store at `base`.
fn register_at(base: &str, path: &Path) -> Result<Registration> {
    let store = manifest_store(path)?;
    if store.len() > MAX_UPLOAD_BYTES {
        bail!(
            "the manifest store is {:.1} MB; the test store takes at most 2 MB",
            store.len() as f64 / (1024.0 * 1024.0)
        );
    }
    let response = lookup::agent(TIMEOUT)
        .post(format!("{base}/v1/manifests?declare=true"))
        .header("Content-Type", "application/c2pa")
        .send(&store[..]);
    answer(response)
}

/// The manifest store of the file at `path`: a sidecar itself, the store the file embeds, else
/// the sidecar next to it, as c2pa-rs reads them.
pub fn manifest_store(path: &Path) -> Result<Vec<u8>> {
    let is_sidecar = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(SIDECAR_EXTENSION));
    if !is_sidecar {
        match c2pa::jumbf_io::load_jumbf_from_file(path) {
            Ok(store) => return Ok(store),
            Err(c2pa::Error::JumbfNotFound) => {}
            Err(e) => return Err(e).context("cannot read the manifest store"),
        }
    }
    let sidecar = if is_sidecar {
        path.to_path_buf()
    } else {
        path.with_extension(SIDECAR_EXTENSION)
    };
    if !sidecar.exists() {
        bail!("this file has no Content Credentials to register");
    }
    std::fs::read(&sidecar).with_context(|| format!("cannot read {}", sidecar.display()))
}

/// The registration in a 200 answer, else why the store did not take the upload.
fn answer(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Registration> {
    let mut response = response.map_err(|e| lookup::request_error(&e, TIMEOUT))?;
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("Retry-After")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_vec()
        .map_err(|e| lookup::request_error(&e, TIMEOUT))?;
    match status {
        200 | 201 => serde_json::from_slice(&body)
            .map_err(|e| anyhow!("unexpected answer from the test store: {e}")),
        429 => bail!(
            "too many uploads from this address; try again in {} s",
            retry.as_deref().unwrap_or("60")
        ),
        _ => bail!(
            "the test store answered {}",
            lookup::http_reason(status, &body)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sign::tests::request;
    use crate::timestamp::tests::serve;

    /// A local store answering every upload with `status` and the JSON `body`, plus `headers`.
    fn store(status: &str, headers: &str, body: &str) -> String {
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n{headers}Content-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        );
        serve(Box::leak(reply.into_boxed_str()))
            .trim_end_matches("/tsa")
            .to_owned()
    }

    /// `no_manifest.jpg` signed into the temporary folder `name`, as a copy or with a sidecar.
    fn signed(name: &str, sidecar: bool) -> std::path::PathBuf {
        let dir = tempfile::Builder::new()
            .prefix(name)
            .tempdir()
            .unwrap()
            .keep();
        let source = dir.join("photo.jpg");
        std::fs::copy(
            format!(
                "{}/tests/fixtures/no_manifest.jpg",
                env!("CARGO_MANIFEST_DIR")
            ),
            &source,
        )
        .unwrap();
        let mut req = request("no_manifest.jpg", &dir.join("photo-signed.jpg"));
        req.source = source.to_string_lossy().into_owned();
        req.sidecar = sidecar;
        std::path::PathBuf::from(crate::sign::sign(&req).unwrap().output)
    }

    /// Whether `store` starts with a JUMBF superbox.
    fn is_jumbf(store: &[u8]) -> bool {
        store.len() > 8 && &store[4..8] == b"jumb"
    }

    #[test]
    fn manifest_store_of_an_embedded_file_and_of_a_sidecar() {
        let copy = signed("register-embedded", false);
        assert!(is_jumbf(&manifest_store(&copy).unwrap()));
        let sidecar = signed("register-sidecar", true);
        let bytes = std::fs::read(&sidecar).unwrap();
        assert!(is_jumbf(&bytes));
        assert_eq!(manifest_store(&sidecar).unwrap(), bytes);
        assert_eq!(
            manifest_store(&sidecar.with_extension("jpg")).unwrap(),
            bytes
        );
        // A sidecar named directly is read as named, on case-sensitive file systems too.
        let upper = sidecar.with_extension("C2PA");
        std::fs::rename(&sidecar, &upper).unwrap();
        assert_eq!(manifest_store(&upper).unwrap(), bytes);
    }

    #[test]
    fn a_file_without_credentials_has_nothing_to_register() {
        let path = format!(
            "{}/tests/fixtures/no_manifest.jpg",
            env!("CARGO_MANIFEST_DIR")
        );
        let error = manifest_store(Path::new(&path)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "this file has no Content Credentials to register"
        );
    }

    #[test]
    fn a_stored_manifest_comes_back_with_its_declaration() {
        let body = r#"{"manifestId":"urn:c2pa:1","manifestUrl":"https://s/v1/manifests/urn:c2pa:1",
            "declaration":{"status":"declared","isccId":"ISCC:MAIGKV5FAAXOXYAB","hub":"https://staging.iscc.id"}}"#;
        let base = store("200 OK", "", body);
        let registration = register_at(&base, &signed("register-ok", false)).unwrap();
        assert_eq!(
            registration,
            Registration {
                manifest_id: "urn:c2pa:1".into(),
                manifest_url: "https://s/v1/manifests/urn:c2pa:1".into(),
                declaration: Declaration {
                    status: "declared".into(),
                    iscc_id: Some("ISCC:MAIGKV5FAAXOXYAB".into()),
                    hub: Some("https://staging.iscc.id".into()),
                },
            }
        );
    }

    #[test]
    fn a_skipped_declaration_has_no_iscc_id() {
        let body =
            r#"{"manifestId":"urn:c2pa:1","manifestUrl":"u","declaration":{"status":"skipped"}}"#;
        let base = store("200 OK", "", body);
        let registration = register_at(&base, &signed("register-skipped", false)).unwrap();
        assert_eq!(registration.declaration.status, "skipped");
        assert_eq!(registration.declaration.iscc_id, None);
    }

    #[test]
    fn a_refusal_carries_the_stores_detail() {
        let base = store(
            "400 Bad Request",
            "",
            r#"{"detail":"claim signature does not verify","code":"claimSignature.mismatch"}"#,
        );
        let error = register_at(&base, &signed("register-refused", false)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the test store answered HTTP 400: claim signature does not verify"
        );
    }

    #[test]
    fn too_many_uploads_say_when_to_try_again() {
        let base = store(
            "429 Too Many Requests",
            "Retry-After: 42\r\n",
            r#"{"detail":"slow down"}"#,
        );
        let error = register_at(&base, &signed("register-limited", false)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "too many uploads from this address; try again in 42 s"
        );
    }

    /// Uploads a demo manifest to the real test store (allowed without asking), so it needs the
    /// network: `cargo test --lib -- --ignored registers_on_the_test_store`.
    #[test]
    #[ignore]
    fn registers_on_the_test_store() {
        let registration = register(&signed("register-live", false)).unwrap();
        assert!(registration
            .manifest_url
            .ends_with(&registration.manifest_id));
        println!("{registration:?}");
    }
}
