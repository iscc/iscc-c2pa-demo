//! Best-effort RFC 3161 timestamping for c2pa signing: a signer wrapper that asks a time
//! stamping authority (TSA) with timeouts and signs without a timestamp when the service fails.
//!
//! c2pa itself stays offline: its HTTP features are off, and only this module's own resolver,
//! used for nothing but the timestamp request, goes online.

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use c2pa::crypto::cose::{check_end_entity_certificate_profile, CertificateTrustPolicy};
use c2pa::crypto::time_stamp::{
    default_rfc3161_message, default_rfc3161_request, tsa_signer_cert_der_from_token,
    TimeStampError,
};
use c2pa::http::http::{header, Request, Response};
use c2pa::http::{HttpResolverError, SyncHttpResolver};
use c2pa::status_tracker::StatusTracker;
use c2pa::{BoxedSigner, Context, Signer, SigningAlg};
use serde::Serialize;

/// A timestamp service offered in the Sign form.
#[derive(Serialize, Debug, Clone, Copy)]
pub struct TsaPreset {
    pub name: &'static str,
    pub url: &'static str,
    /// Whether its timestamps validate against the bundled C2PA TSA trust list.
    pub trusted: bool,
}

/// Services offered in the Sign form; the first is the default. Only Encypher was found to be
/// free, anonymous and on the C2PA TSA trust list; the others show "Unverified time".
pub const TSA_PRESETS: [TsaPreset; 3] = [
    TsaPreset {
        name: "Encypher",
        url: "https://tsa.encypher.com/tsa/timestamp",
        trusted: true,
    },
    TsaPreset {
        name: "DigiCert",
        url: "http://timestamp.digicert.com",
        trusted: false,
    },
    TsaPreset {
        name: "Sectigo",
        url: "http://timestamp.sectigo.com",
        trusted: false,
    },
];

/// Timestamp service used when none is chosen.
pub const DEFAULT_TSA_URL: &str = TSA_PRESETS[0].url;

/// Longest wait for a timestamp, connecting and reading included.
pub const TIMEOUT: Duration = Duration::from_secs(10);
/// Longest wait for the connection alone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound for a timestamp reply; c2pa reads at most 1 MB and preallocates `Content-Length`.
const MAX_REPLY_BYTES: u64 = 1_000_000;
/// Room c2pa reserves in the signature for the timestamp; a bigger one makes signing fail.
const TOKEN_RESERVE: usize = 10_000;
/// Extended key usage a timestamp certificate must carry (id-kp-timeStamping).
const TIME_STAMPING_EKU: &str = "1.3.6.1.5.5.7.3.8";

/// Why the last timestamp request failed, shared with the caller after the signer moved into a
/// c2pa `Context`.
pub type FailureSlot = Arc<Mutex<Option<String>>>;

/// HTTP resolver with timeouts and without redirects, for timestamp requests only.
struct UreqResolver(ureq::Agent);

impl UreqResolver {
    fn new(timeout: Duration) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Self(ureq::Agent::new_with_config(config))
    }
}

impl SyncHttpResolver for UreqResolver {
    fn http_resolve(
        &self,
        request: Request<Vec<u8>>,
    ) -> Result<Response<Box<dyn Read>>, HttpResolverError> {
        let response = self
            .0
            .run(request)
            .map_err(|e| HttpResolverError::Other(Box::new(e)))?;
        let mut builder = Response::builder()
            .status(response.status())
            .version(response.version());
        for (name, value) in response.headers() {
            // c2pa preallocates the announced length; drop an implausible one.
            let too_long = name == header::CONTENT_LENGTH
                && value
                    .to_str()
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .is_none_or(|len| len > MAX_REPLY_BYTES);
            if !too_long {
                builder = builder.header(name, value);
            }
        }
        let body = response.into_body().into_reader();
        Ok(builder.body(Box::new(body) as Box<dyn Read>)?)
    }
}

/// Signer that timestamps through its own resolver and, when the TSA fails, records why and
/// signs without a timestamp instead of failing.
pub struct BestEffortTsa {
    inner: BoxedSigner,
    url: String,
    timeout: Duration,
    failure: FailureSlot,
}

impl BestEffortTsa {
    /// Wrap `inner`, which should have been created with the same TSA URL so c2pa reserves
    /// room for the token.
    pub fn new(inner: BoxedSigner, url: &str) -> Self {
        Self {
            inner,
            url: url.to_owned(),
            timeout: TIMEOUT,
            failure: FailureSlot::default(),
        }
    }

    /// The same signer with a different overall timeout.
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// Handle to the failure reason, readable after signing.
    pub fn failure_handle(&self) -> FailureSlot {
        Arc::clone(&self.failure)
    }

    /// Ask the TSA for a token over `message`; c2pa checks the token before returning it.
    fn request(&self, message: &[u8]) -> Result<Vec<u8>, TimeStampError> {
        let body = default_rfc3161_message(message)?;
        let context = Context::new().with_resolver(UreqResolver::new(self.timeout));
        default_rfc3161_request(&self.url, None, &body, message, &context)
    }
}

impl Signer for BestEffortTsa {
    fn sign(&self, data: &[u8]) -> c2pa::Result<Vec<u8>> {
        self.inner.sign(data)
    }

    fn alg(&self) -> SigningAlg {
        self.inner.alg()
    }

    fn certs(&self) -> c2pa::Result<Vec<Vec<u8>>> {
        self.inner.certs()
    }

    fn reserve_size(&self) -> usize {
        self.inner.reserve_size()
    }

    fn time_authority_url(&self) -> Option<String> {
        Some(self.url.clone())
    }

    fn ocsp_val(&self) -> Option<Vec<u8>> {
        self.inner.ocsp_val()
    }

    fn send_timestamp_request(&self, message: &[u8]) -> Option<c2pa::Result<Vec<u8>>> {
        let token = self
            .request(message)
            .map_err(|e| failure_reason(&e, self.timeout))
            .and_then(embeddable);
        match token {
            Ok(token) => Some(Ok(token)),
            Err(reason) => {
                *self.failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
                None
            }
        }
    }
}

/// `token` if c2pa can embed it, else why not. c2pa accepts any token that verifies, but then
/// fails the whole signature when the token outgrows its reserve or when its certificate fails
/// the C2PA certificate profile, which `verify_after_sign` checks.
fn embeddable(token: Vec<u8>) -> Result<Vec<u8>, String> {
    if token.len() > TOKEN_RESERVE {
        return Err(format!(
            "timestamp too large: {} bytes, room for {TOKEN_RESERVE}",
            token.len()
        ));
    }
    let cert = tsa_signer_cert_der_from_token(&token)
        .ok()
        .flatten()
        .ok_or_else(|| "timestamp without certificate".to_owned())?;
    match certificate_problem(&cert) {
        Some(problem) => Err(problem),
        None => Ok(token),
    }
}

/// Why the timestamp certificate `cert` (DER) fails the C2PA certificate profile, if it does;
/// the same check c2pa runs on it after signing. Some failures, such as a missing EKU, are only
/// logged, so the log decides.
fn certificate_problem(cert: &[u8]) -> Option<String> {
    let mut policy = CertificateTrustPolicy::default();
    policy.clear_ekus();
    policy.add_mandatory_ekus(TIME_STAMPING_EKU.as_bytes());
    let mut log = StatusTracker::default();
    let result = check_end_entity_certificate_profile(cert, &policy, &mut log, None);
    let logged = log
        .filter_errors()
        .next()
        .map(|i| i.description.to_string());
    logged.or_else(|| result.err().map(|e| e.to_string()))
}

/// Short, readable reason for a failed timestamp request. Transport failures are named only
/// (the host is shown next to them); a bad reply keeps its detail, the only clue to what is wrong.
fn failure_reason(error: &TimeStampError, timeout: Duration) -> String {
    let transport = match error {
        TimeStampError::HttpResolverError(HttpResolverError::Other(e)) => {
            e.downcast_ref::<ureq::Error>()
        }
        _ => None,
    };
    match (transport, error) {
        (Some(ureq::Error::Timeout(_)), _) => {
            format!("timed out after {} s", timeout.as_secs_f32())
        }
        (Some(ureq::Error::HostNotFound), _) => "host not found".to_owned(),
        (Some(ureq::Error::ConnectionFailed | ureq::Error::Io(_)), _) => "no connection".to_owned(),
        (Some(ureq::Error::Tls(_) | ureq::Error::Rustls(_)), _) => {
            "secure connection failed".to_owned()
        }
        (_, TimeStampError::HttpErrorResponse(status, _)) if *status != 200 => {
            format!("HTTP {status}")
        }
        _ => format!("invalid response: {error}"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::time::Instant;

    use c2pa::create_signer;

    use super::*;
    use crate::context;

    /// Demo signer wrapped for `url`.
    fn tsa_signer(url: &str) -> BestEffortTsa {
        let inner = create_signer::from_keys(
            context::DEMO_SIGN_CERT.as_bytes(),
            context::DEMO_SIGN_KEY.as_bytes(),
            SigningAlg::Es256,
            Some(url.to_owned()),
        )
        .unwrap();
        BestEffortTsa::new(inner, url)
    }

    /// Request a timestamp and return the recorded failure, asserting that none was produced.
    fn failure_of(signer: &BestEffortTsa) -> String {
        assert!(signer.send_timestamp_request(b"message").is_none());
        let reason = signer.failure_handle().lock().unwrap().clone();
        reason.expect("failure recorded")
    }

    /// Read a whole HTTP request: headers, then as many body bytes as `Content-Length` says.
    /// Closing a socket with unread data resets the connection on Windows.
    fn read_request(stream: &mut impl Read) {
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = stream.read(&mut buf) {
            request.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&request).to_lowercase();
            let Some(end) = text.find("\r\n\r\n") else {
                continue;
            };
            let length = text[..end]
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if request.len() >= end + 4 + length {
                return;
            }
        }
    }

    /// Local server that answers every connection with `reply`.
    fn serve(reply: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/tsa", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                read_request(&mut stream);
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        url
    }

    #[test]
    fn closed_port_means_no_connection() {
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let reason = failure_of(&tsa_signer(&format!("http://127.0.0.1:{port}/")));
        assert_eq!(reason, "no connection");
    }

    #[test]
    fn silent_server_times_out() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let signer = tsa_signer(&url).with_timeout(Duration::from_millis(500));
        let start = Instant::now();
        let reason = failure_of(&signer);
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(reason, "timed out after 0.5 s");
        drop(listener);
    }

    #[test]
    fn http_error_status_is_reported() {
        let url = serve("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(failure_of(&tsa_signer(&url)), "HTTP 500");
    }

    #[test]
    fn wrong_content_type_is_an_invalid_response() {
        let url =
            serve("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\nhi");
        let reason = failure_of(&tsa_signer(&url));
        assert!(reason.starts_with("invalid response"), "{reason}");
    }

    #[test]
    fn garbage_token_is_an_invalid_response() {
        let url = serve(
            "HTTP/1.1 200 OK\r\nContent-Type: application/timestamp-reply\r\n\
             Content-Length: 999999999999\r\n\r\nnot a token",
        );
        let reason = failure_of(&tsa_signer(&url));
        assert!(reason.starts_with("invalid response"), "{reason}");
    }

    #[test]
    fn signer_keeps_the_reserve_for_the_token() {
        let without = create_signer::from_keys(
            context::DEMO_SIGN_CERT.as_bytes(),
            context::DEMO_SIGN_KEY.as_bytes(),
            SigningAlg::Es256,
            None,
        )
        .unwrap();
        let with = tsa_signer(DEFAULT_TSA_URL);
        assert_eq!(with.reserve_size(), without.reserve_size() + TOKEN_RESERVE);
        assert_eq!(with.time_authority_url().as_deref(), Some(DEFAULT_TSA_URL));
    }

    /// Where the RFC 3161 token lies in a signed file: the DER structure around its signedData
    /// OID.
    pub(crate) fn token_span(file: &[u8]) -> std::ops::Range<usize> {
        const SIGNED_DATA: [u8; 11] = [
            0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
        ];
        let oid = file.windows(11).position(|w| w == SIGNED_DATA).unwrap();
        let start = oid - 4;
        assert_eq!(file[start..start + 2], [0x30, 0x82], "two-byte DER length");
        let length = usize::from(u16::from_be_bytes([file[start + 2], file[start + 3]]));
        start..start + 4 + length
    }

    /// The RFC 3161 token in a signed fixture.
    fn fixture_token(name: &str) -> Vec<u8> {
        let file = std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        file[token_span(&file)].to_vec()
    }

    #[test]
    fn tokens_of_real_services_are_embeddable() {
        for name in ["tsa/encypher.jpg", "tsa/digicert.jpg"] {
            let token = fixture_token(name);
            assert_eq!(embeddable(token.clone()), Ok(token), "{name}");
        }
    }

    #[test]
    fn token_larger_than_the_reserve_is_refused() {
        let reason = embeddable(vec![0; TOKEN_RESERVE + 1]).unwrap_err();
        assert_eq!(reason, "timestamp too large: 10001 bytes, room for 10000");
    }

    #[test]
    fn certificate_without_time_stamping_eku_is_refused() {
        let demo = tsa_signer(DEFAULT_TSA_URL).certs().unwrap();
        let problem = certificate_problem(&demo[0]).expect("demo certificate is no TSA's");
        assert!(problem.contains("EKU"), "{problem}");
    }

    /// Real round trip to the default service; run before a release with `--ignored`.
    #[test]
    #[ignore = "needs the network"]
    fn default_service_returns_a_token() {
        let signer = tsa_signer(DEFAULT_TSA_URL);
        let token = signer.send_timestamp_request(b"message");
        assert!(
            matches!(token, Some(Ok(ref t)) if !t.is_empty()),
            "{:?}",
            signer.failure_handle()
        );
    }
}
