//! Search the ISCC test network for Content Credentials that match a file's ISCC. Two services
//! answer the soft binding query of the C2PA Soft Binding Resolution API: the test store, which
//! finds the manifests uploaded to it at once, and the resolver, which finds every manifest
//! declared on the ISCC Discovery Protocol a minute or so after its declaration. The query goes
//! by POST, so the ISCC stays out of URLs and their logs; it holds all of the file's units except
//! its Meta-Code, which the services do not score. The services' answers are only candidates: each
//! manifest store is fetched and checked here against the file ([`recover::screen`],
//! [`recover::examine`]), and their scores only decide which candidates are fetched. Only the
//! ISCC leaves the machine, never the file.

use std::cmp::Reverse;
use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::context::ISCC_SOFT_BINDING_ALG;
use crate::inspect;
use crate::iscc::{self, encode_seq, IsccUnit};
use crate::recover::{self, Examined, InHand, IsccMatch};
use crate::timestamp::connection_reason;

/// The services asked, in the order their matches are listed.
pub const SERVICES: [Service; 2] = [
    Service {
        host: "c2pa-store-test.iscc.io",
        kind: ServiceKind::Store,
    },
    Service {
        host: "c2pa-test.iscc.io",
        kind: ServiceKind::Resolver,
    },
];

/// Matches asked of each service, and taken from its answer at most. More than are usually listed,
/// so that a few uploads with a copied ISCC do not push a genuine manifest out; a crowd of them
/// still can, and the services' ranking decides among equal scores.
const MAX_ASKED: usize = 20;
/// Manifest stores fetched and screened at most, best service score first.
const MAX_FETCHED: usize = 20;
/// Screened candidates examined in full at most, which hashes the file for each: exact ones first,
/// then the most similar.
const MAX_EXAMINED: usize = 8;
/// Exact matches whose credentials are put back into a copy of the file at most.
const MAX_REBUILT: usize = 3;
/// Candidates screened or examined at once.
const WORKERS: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// A cold resolver query asks the aggregator and probes each candidate's gateway.
const QUERY_TIMEOUT: Duration = Duration::from_secs(20);
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest answer read: the store refuses manifest stores above 2 MiB.
const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;

/// A service of the test network that answers soft binding queries.
#[derive(Clone, Copy, Debug)]
pub struct Service {
    pub host: &'static str,
    kind: ServiceKind,
}

/// How a service serves the manifest store of a match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceKind {
    /// Under `/v1/manifests/{id}` of its own.
    Store,
    /// Under the match's `endpoint`, which names the declaration.
    Resolver,
}

/// What the search found, what it left out, and which services could not be asked.
#[derive(Serialize, Debug)]
pub struct Lookup {
    /// Near-identical matches: exact ones first, then by earliest verified time.
    pub matches: Vec<Found>,
    /// Matches with related content that are not near-identical, most similar first.
    pub related: Vec<Found>,
    /// Candidates examined and left out, with the reason.
    pub left_out: Vec<LeftOut>,
    /// Candidates beyond [`MAX_FETCHED`] or [`MAX_EXAMINED`], not examined.
    pub unchecked: usize,
    pub failures: Vec<Failure>,
    /// Lowest Content-Code similarity of a near-identical match ([`recover::NEAR_IDENTICAL`]).
    pub near_identical: f64,
}

/// A service that did not answer, and why.
#[derive(Serialize, Debug)]
pub struct Failure {
    pub host: &'static str,
    pub reason: String,
}

/// A candidate left out, and why.
#[derive(Serialize, Debug, PartialEq)]
pub struct LeftOut {
    pub manifest_id: String,
    pub host: &'static str,
    pub reason: String,
}

/// A manifest store whose soft binding matches the file's ISCC, examined against the file.
#[derive(Serialize, Debug)]
pub struct Found {
    /// Key of the fetched manifest store, for Restore.
    pub store_id: String,
    pub manifest_id: String,
    /// Hosts of the services that found these exact bytes.
    pub hosts: Vec<&'static str>,
    /// Best similarity score a service gave, 100 for an equal Instance-Code; for ranking only.
    pub score: u8,
    #[serde(flatten)]
    pub examined: Examined,
}

/// One match in a service's answer (C2PA `softBindingQueryResult`).
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Match {
    manifest_id: String,
    similarity_score: Option<f64>,
    endpoint: Option<String>,
}

#[derive(Deserialize, Debug)]
struct QueryResult {
    #[serde(default)]
    matches: Vec<Match>,
}

/// A match with the address of its manifest store.
#[derive(Clone, Debug)]
struct Hit {
    manifest_id: String,
    score: u8,
    host: &'static str,
    url: String,
}

/// A fetched manifest store with the hits that led to it: one manifest ID, the same bytes.
struct Fetched {
    store: Vec<u8>,
    hits: Vec<Hit>,
}

/// Search both services for manifests matching `units`, the ISCC-UNITs of the file at `path` as
/// the app shows them, fetch the candidates, screen them all without hashing the file, and examine
/// the best of them against the file. Fails only when the units make no query; a service that does
/// not answer is listed in `failures`.
pub fn lookup(path: &Path, units: &[String]) -> Result<Lookup> {
    let format = inspect::asset_format(path)?;
    let units: Vec<IsccUnit> = units
        .iter()
        .map(|u| iscc::describe_unit(u))
        .collect::<Result<_>>()?;
    recover::forget_found();
    let (hits, failures) = ask_all(&query_value(&units)?);
    let (hits, unchecked) = ranked(hits);
    let (fetched, mut left_out) = fetch_all(hits);
    let file = InHand {
        path,
        format,
        units: &units,
    };
    let candidates = screen_all(&file, fetched, &mut left_out);
    let mut lookup = examine_all(&file, candidates, &mut left_out);
    lookup.unchecked += unchecked;
    lookup.left_out = left_out;
    lookup.failures = failures;
    Ok(lookup)
}

/// Screen every fetched store against the file without hashing it; those that fail go to
/// `left_out`, the rest come back with their ISCC match, the most promising first.
fn screen_all(
    file: &InHand,
    fetched: Vec<Fetched>,
    left_out: &mut Vec<LeftOut>,
) -> Vec<(Fetched, IsccMatch)> {
    let screened = bounded(fetched, |f| {
        let iscc = recover::screen(file, &f.hits[0].manifest_id, &f.store);
        (f, iscc)
    });
    let mut candidates = Vec::new();
    for (fetched, iscc) in screened {
        match iscc {
            Ok(iscc) => candidates.push((fetched, iscc)),
            Err(reason) => left_out.extend(leave_out(&fetched, &reason)),
        }
    }
    candidates.sort_by(|(_, a), (_, b)| promise(b).total_cmp(&promise(a)));
    candidates
}

/// Examine the first [`MAX_EXAMINED`] screened `candidates` against the file, the rebuild check
/// for the first [`MAX_REBUILT`] exact ones; those that fail go to `left_out`. The lookup's
/// matches and related ones, ordered, and how many candidates were not examined.
fn examine_all(
    file: &InHand,
    mut candidates: Vec<(Fetched, IsccMatch)>,
    left_out: &mut Vec<LeftOut>,
) -> Lookup {
    let unchecked = candidates.len().saturating_sub(MAX_EXAMINED);
    candidates.truncate(MAX_EXAMINED);
    let mut exact = 0;
    let jobs: Vec<_> = candidates
        .into_iter()
        .map(|(fetched, iscc)| {
            exact += usize::from(iscc.exact);
            let rebuild = iscc.exact && exact <= MAX_REBUILT;
            (fetched, iscc, rebuild)
        })
        .collect();
    let examined = bounded(jobs, |(f, iscc, rebuild)| {
        let examined = recover::examine(file, &f.store, iscc, rebuild);
        (f, examined)
    });
    let (mut matches, mut related) = (Vec::new(), Vec::new());
    for (fetched, outcome) in examined {
        match outcome {
            Ok(examined) if examined.iscc.near => matches.push(found(fetched, examined)),
            Ok(examined) => related.push(found(fetched, examined)),
            Err(reason) => left_out.extend(leave_out(&fetched, &reason)),
        }
    }
    matches.sort_by_key(match_order);
    related.sort_by(|a, b| similarity(b).total_cmp(&similarity(a)));
    Lookup {
        matches,
        related,
        left_out: Vec::new(),
        unchecked,
        failures: Vec::new(),
        near_identical: recover::NEAR_IDENTICAL,
    }
}

/// How worth examining a screened match is: exact ones first, then by Content-Code similarity.
fn promise(iscc: &IsccMatch) -> f64 {
    if iscc.exact {
        2.0
    } else {
        iscc.content.unwrap_or(0.0)
    }
}

/// A left-out entry for each hit of `fetched`, for `reason`.
fn leave_out(fetched: &Fetched, reason: &str) -> Vec<LeftOut> {
    fetched
        .hits
        .iter()
        .map(|h| LeftOut {
            manifest_id: h.manifest_id.clone(),
            host: h.host,
            reason: reason.to_owned(),
        })
        .collect()
}

/// `work` on every item, at most [`WORKERS`] at once, results in item order.
fn bounded<T: Send, R: Send>(items: Vec<T>, work: impl Fn(T) -> R + Sync) -> Vec<R> {
    let mut results = Vec::with_capacity(items.len());
    let mut items = items.into_iter().peekable();
    while items.peek().is_some() {
        let batch: Vec<T> = items.by_ref().take(WORKERS).collect();
        let work = &work;
        std::thread::scope(|scope| {
            let runs: Vec<_> = batch
                .into_iter()
                .map(|item| scope.spawn(move || work(item)))
                .collect();
            results.extend(runs.into_iter().filter_map(|run| run.join().ok()));
        });
    }
    results
}

/// The base64 ISCC-SEQ the services are asked with: every unit but the Meta-Code, which they do
/// not score and which would only tell them more about the file.
fn query_value(units: &[IsccUnit]) -> Result<String> {
    let queried: Vec<String> = units
        .iter()
        .filter(|u| u.unit != "meta")
        .map(|u| u.iscc.clone())
        .collect();
    if queried.is_empty() {
        bail!("the file has no unit to search with");
    }
    Ok(STANDARD.encode(encode_seq(&queried)?))
}

/// Ask every service at once; the hits of those that answered and why the others did not.
fn ask_all(value: &str) -> (Vec<Hit>, Vec<Failure>) {
    let answers: Vec<Result<Vec<Hit>>> = std::thread::scope(|scope| {
        let asks: Vec<_> = SERVICES
            .iter()
            .map(|service| scope.spawn(|| query(service, value)))
            .collect();
        asks.into_iter()
            .map(|ask| {
                ask.join()
                    .unwrap_or_else(|_| Err(anyhow!("the query failed")))
            })
            .collect()
    });
    let mut failures = Vec::new();
    let mut hits = Vec::new();
    for (service, answer) in SERVICES.iter().zip(answers) {
        match answer {
            Ok(found) => hits.extend(found),
            Err(e) => failures.push(Failure {
                host: service.host,
                reason: format!("{e:#}"),
            }),
        }
    }
    (hits, failures)
}

/// The hits to fetch, one per manifest and service, best score first (the store's first on a
/// tie), at most [`MAX_FETCHED`]; and how many were dropped for that limit.
fn ranked(hits: Vec<Hit>) -> (Vec<Hit>, usize) {
    let mut kept: Vec<Hit> = Vec::new();
    for hit in hits {
        let same = |k: &&mut Hit| k.manifest_id == hit.manifest_id && k.host == hit.host;
        match kept.iter_mut().find(same) {
            Some(k) if hit.score > k.score => *k = hit,
            Some(_) => {}
            None => kept.push(hit),
        }
    }
    kept.sort_by_key(|h| Reverse(h.score));
    let dropped = kept.len().saturating_sub(MAX_FETCHED);
    kept.truncate(MAX_FETCHED);
    (kept, dropped)
}

/// Fetch the manifest store of every hit at once; see [`merged`] for what comes back.
fn fetch_all(hits: Vec<Hit>) -> (Vec<Fetched>, Vec<LeftOut>) {
    let answers: Vec<(Hit, Result<Vec<u8>>)> = std::thread::scope(|scope| {
        let fetches: Vec<_> = hits
            .into_iter()
            .map(|hit| {
                scope.spawn(move || {
                    let store = get(&hit.url, FETCH_TIMEOUT);
                    (hit, store)
                })
            })
            .collect();
        fetches.into_iter().filter_map(|f| f.join().ok()).collect()
    });
    merged(answers)
}

/// The fetched manifest stores of `answers`, hits that gave the same bytes for the same manifest
/// ID merged, in hit order; hits whose store could not be fetched are left out.
fn merged(answers: Vec<(Hit, Result<Vec<u8>>)>) -> (Vec<Fetched>, Vec<LeftOut>) {
    let mut fetched: Vec<Fetched> = Vec::new();
    let mut left_out = Vec::new();
    for (hit, store) in answers {
        match store {
            Ok(store) => match fetched
                .iter_mut()
                .find(|f| f.store == store && f.hits[0].manifest_id == hit.manifest_id)
            {
                Some(f) => f.hits.push(hit),
                None => fetched.push(Fetched {
                    store,
                    hits: vec![hit],
                }),
            },
            Err(e) => left_out.push(LeftOut {
                reason: format!("its manifest store could not be fetched: {e:#}"),
                manifest_id: hit.manifest_id,
                host: hit.host,
            }),
        }
    }
    (fetched, left_out)
}

/// The listed match of the manifest store `fetched`, examined as `examined`, kept for Restore.
fn found(fetched: Fetched, examined: Examined) -> Found {
    let score = fetched.hits.iter().map(|h| h.score).max().unwrap_or(0);
    let mut hosts: Vec<&'static str> = fetched.hits.iter().map(|h| h.host).collect();
    hosts.dedup();
    Found {
        manifest_id: fetched.hits[0].manifest_id.clone(),
        store_id: recover::keep(fetched.store),
        hosts,
        score,
        examined,
    }
}

/// Order of near-identical matches (C2PA UX Recommendations, Fingerprinting: earliest known
/// provenance first): exact ones first, then by verified signing time, earliest first, those
/// without one last, then the most similar.
fn match_order(found: &Found) -> (bool, bool, Option<String>, Reverse<u64>) {
    let time = verified_time(found).map(str::to_owned);
    let similar = (similarity(found) * 1e6) as u64;
    (
        !found.examined.iscc.exact,
        time.is_none(),
        time,
        Reverse(similar),
    )
}

/// Signing time confirmed by a timestamp service on the trust list; c2pa-rs writes it in UTC
/// (RFC 3339), so the strings sort by time.
fn verified_time(found: &Found) -> Option<&str> {
    let signature = found.examined.manifest.signature.as_ref()?;
    let ts = &signature.timestamp;
    (ts.status == "trusted" && !ts.legacy)
        .then_some(ts.time.as_deref())
        .flatten()
}

/// Similarity of the Content-Code, exact matches counting as identical.
fn similarity(found: &Found) -> f64 {
    let iscc = &found.examined.iscc;
    if iscc.exact {
        1.0
    } else {
        iscc.content.unwrap_or(0.0)
    }
}

/// HTTP agent with timeouts, without redirects, reading error statuses as answers.
fn agent(timeout: Duration) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .build();
    ureq::Agent::new_with_config(config)
}

/// GET `url` and return the body of a 200 answer.
fn get(url: &str, timeout: Duration) -> Result<Vec<u8>> {
    body(agent(timeout).get(url).call(), timeout)
}

/// POST the JSON `payload` to `url` and return the body of a 200 answer.
fn post(url: &str, payload: &serde_json::Value, timeout: Duration) -> Result<Vec<u8>> {
    let sent = agent(timeout)
        .post(url)
        .header("Content-Type", "application/json")
        .send(payload.to_string());
    body(sent, timeout)
}

/// The body of a 200 answer, else why the request failed.
fn body(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut response = response.map_err(|e| request_error(&e, timeout))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_BODY_BYTES)
        .read_to_vec()
        .map_err(|e| request_error(&e, timeout))?;
    if status != 200 {
        bail!("{}", http_reason(status, &body));
    }
    Ok(body)
}

/// Why a request failed: the connection, else ureq's own words.
fn request_error(error: &ureq::Error, timeout: Duration) -> anyhow::Error {
    anyhow!(connection_reason(error, timeout).unwrap_or_else(|| error.to_string()))
}

/// "HTTP 400: detail" from an error answer, the detail when it has one.
fn http_reason(status: u16, body: &[u8]) -> String {
    let detail = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_owned));
    match detail {
        Some(detail) => format!("HTTP {status}: {detail}"),
        None => format!("HTTP {status}"),
    }
}

/// Ask `service` for the manifests matching the base64 ISCC-SEQ `value`.
fn query(service: &Service, value: &str) -> Result<Vec<Hit>> {
    let url = format!(
        "https://{}/v1/matches/byBinding?maxResults={MAX_ASKED}",
        service.host
    );
    let payload = json!({ "alg": ISCC_SOFT_BINDING_ALG, "value": value });
    let body = post(&url, &payload, QUERY_TIMEOUT)?;
    let result: QueryResult =
        serde_json::from_slice(&body).map_err(|e| anyhow!("unexpected answer: {e}"))?;
    Ok(result
        .matches
        .into_iter()
        .take(MAX_ASKED)
        .filter_map(|m| hit(service, m))
        .collect())
}

/// The match `m` with the address of its manifest store; none when a resolver's endpoint lies
/// outside the resolver, which is never fetched.
fn hit(service: &Service, m: Match) -> Option<Hit> {
    let id = path_segment(&m.manifest_id);
    let url = match service.kind {
        ServiceKind::Store => format!("https://{}/v1/manifests/{id}", service.host),
        ServiceKind::Resolver => {
            let endpoint = m.endpoint.as_deref()?;
            if !endpoint.starts_with(&format!("https://{}/", service.host)) {
                return None;
            }
            format!("{}/manifests/{id}", endpoint.trim_end_matches('/'))
        }
    };
    Some(Hit {
        score: m.similarity_score.unwrap_or(0.0).clamp(0.0, 100.0).round() as u8,
        manifest_id: m.manifest_id,
        host: service.host,
        url,
    })
}

/// `text` percent-encoded as one URL path segment (RFC 3986 unreserved characters, `:` and `@`
/// kept, as in C2PA manifest URNs).
fn path_segment(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b':' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: &str, score: u8, host: &'static str) -> Hit {
        Hit {
            manifest_id: id.to_owned(),
            score,
            host,
            url: String::new(),
        }
    }

    #[test]
    fn hits_are_fetched_once_per_manifest_and_service_best_first() {
        let hits = vec![
            hit("a", 80, "store"),
            hit("b", 100, "store"),
            hit("a", 90, "store"),
            hit("a", 90, "resolver"),
            hit("b", 100, "resolver"),
        ];
        let (kept, dropped) = ranked(hits);
        let kept: Vec<_> = kept
            .into_iter()
            .map(|h| (h.manifest_id, h.score, h.host))
            .collect();
        assert_eq!(
            kept,
            [
                ("b".into(), 100, "store"),
                ("b".into(), 100, "resolver"),
                ("a".into(), 90, "store"),
                ("a".into(), 90, "resolver"),
            ]
        );
        assert_eq!(dropped, 0);
    }

    #[test]
    fn at_most_twenty_hits_are_fetched_and_the_rest_counted() {
        let hits = (0..25)
            .map(|i| hit(&i.to_string(), 75 + i, "store"))
            .collect();
        let (kept, dropped) = ranked(hits);
        assert_eq!(kept.len(), MAX_FETCHED);
        assert_eq!(kept[0].score, 99);
        assert_eq!(dropped, 5);
    }

    #[test]
    fn the_same_bytes_from_both_services_are_examined_once() {
        let answers = vec![
            (hit("a", 100, "store"), Ok(b"store a".to_vec())),
            (hit("a", 100, "resolver"), Ok(b"store a".to_vec())),
            (hit("a", 90, "elsewhere"), Ok(b"other bytes".to_vec())),
            (hit("b", 100, "resolver"), Err(anyhow!("HTTP 404"))),
        ];
        let (fetched, left_out) = merged(answers);
        let hosts: Vec<Vec<&str>> = fetched
            .iter()
            .map(|f| f.hits.iter().map(|h| h.host).collect())
            .collect();
        assert_eq!(hosts, [vec!["store", "resolver"], vec!["elsewhere"]]);
        assert_eq!(
            left_out,
            [LeftOut {
                manifest_id: "b".into(),
                host: "resolver",
                reason: "its manifest store could not be fetched: HTTP 404".into(),
            }]
        );
    }

    #[test]
    fn the_query_leaves_out_the_meta_code() {
        let units: Vec<IsccUnit> = [
            "ISCC:AADXGJPPWDJ7QI7XL4LH43DWUJ6WLYBHK6Z72JXO7Z2UGD5QDQMBDQI",
            "ISCC:GADWYSEQ44YZMECUTBZO2GWBOISZNBVO24KF4UTHRNPRQI74E6UKRYY",
            "ISCC:IADZUC27FXMNE4P7YLACBSKWIMZSVGZWYMSCN4QMV6MEEKABNLIBAPQ",
        ]
        .iter()
        .map(|u| iscc::describe_unit(u).unwrap())
        .collect();
        let value = STANDARD.decode(query_value(&units).unwrap()).unwrap();
        let sent = iscc::decode_seq(&value).unwrap();
        assert_eq!(sent, [units[1].iscc.clone(), units[2].iscc.clone()]);
        assert!(query_value(&units[..1]).is_err());
    }

    #[test]
    fn store_matches_are_fetched_from_the_store() {
        let m = Match {
            manifest_id: "urn:c2pa:1234/x y".to_owned(),
            similarity_score: Some(87.0),
            endpoint: None,
        };
        let h = super::hit(&SERVICES[0], m).unwrap();
        assert_eq!(
            h.url,
            "https://c2pa-store-test.iscc.io/v1/manifests/urn:c2pa:1234%2Fx%20y"
        );
        assert_eq!(h.score, 87);
    }

    #[test]
    fn resolver_matches_are_fetched_through_their_endpoint_on_the_resolver_only() {
        let at = |endpoint: &str| Match {
            manifest_id: "urn:c2pa:1".to_owned(),
            similarity_score: Some(100.0),
            endpoint: Some(endpoint.to_owned()),
        };
        let resolver = &SERVICES[1];
        let h = super::hit(resolver, at("https://c2pa-test.iscc.io/v1/iscc/abc")).unwrap();
        assert_eq!(
            h.url,
            "https://c2pa-test.iscc.io/v1/iscc/abc/manifests/urn:c2pa:1"
        );
        assert!(super::hit(resolver, at("https://elsewhere.example/v1/iscc/abc")).is_none());
        assert!(super::hit(resolver, at("https://c2pa-test.iscc.io.example/x")).is_none());
        let mut bare = at("");
        bare.endpoint = None;
        assert!(super::hit(resolver, bare).is_none());
    }

    /// A local server's answer: `status` with `body`.
    fn answer(status: &str, headers: &str, body: &str) -> String {
        let reply = format!(
            "HTTP/1.1 {status}
{headers}Content-Length: {}
Connection: close

{body}",
            body.len()
        );
        crate::timestamp::tests::serve(Box::leak(reply.into_boxed_str()))
    }

    #[test]
    fn only_a_whole_200_answer_counts_and_redirects_are_not_followed() {
        let ok = answer("200 OK", "", "store");
        assert_eq!(get(&ok, FETCH_TIMEOUT).unwrap(), b"store");
        let detail = r#"{"detail": "Manifest not found"}"#;
        let missing = answer(
            "404 Not Found",
            "Content-Type: application/json
",
            detail,
        );
        let error = get(&missing, FETCH_TIMEOUT).unwrap_err();
        assert_eq!(format!("{error:#}"), "HTTP 404: Manifest not found");
        let moved = answer(
            "302 Found",
            &format!(
                "Location: {ok}
"
            ),
            "",
        );
        assert_eq!(
            format!("{:#}", get(&moved, FETCH_TIMEOUT).unwrap_err()),
            "HTTP 302"
        );
        let huge = "x".repeat(MAX_BODY_BYTES as usize + 1);
        assert!(get(&answer("200 OK", "", &huge), FETCH_TIMEOUT).is_err());
    }

    #[test]
    fn error_answers_carry_their_detail() {
        assert_eq!(
            http_reason(400, br#"{"detail": "Empty ISCC-SEQ"}"#),
            "HTTP 400: Empty ISCC-SEQ"
        );
        assert_eq!(http_reason(502, b"<html>"), "HTTP 502");
    }

    /// The demo picture, its signed copy's manifest store and that manifest's label.
    struct Signed {
        source: std::path::PathBuf,
        store: Vec<u8>,
        label: String,
    }

    fn signed() -> Signed {
        let signed =
            crate::sign::tests::sign_fixture("no_manifest.jpg", "iscc-c2pa-demo-test-order");
        Signed {
            source: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no_manifest.jpg"),
            store: c2pa::jumbf_io::load_jumbf_from_file(&signed).unwrap(),
            label: inspect::inspect(&signed).unwrap().manifest.unwrap().label,
        }
    }

    /// The manifest of `signed` as found for the demo picture, signed at `time` (verified or
    /// not), matching exactly or with the Content-Code `content`.
    fn found(signed: &Signed, time: Option<(&str, bool)>, exact: bool, content: f64) -> Found {
        let units = inspect::inspect(&signed.source).unwrap().iscc;
        let file = InHand {
            path: &signed.source,
            format: inspect::asset_format(&signed.source).unwrap(),
            units: &units,
        };
        let iscc = recover::screen(&file, &signed.label, &signed.store).unwrap();
        let mut examined = recover::examine(&file, &signed.store, iscc, true).unwrap();
        examined.iscc.exact = exact;
        examined.iscc.content = Some(content);
        let ts = &mut examined.manifest.signature.as_mut().unwrap().timestamp;
        ts.time = time.map(|(t, _)| t.to_owned());
        ts.status = if time.is_some_and(|(_, verified)| verified) {
            "trusted"
        } else {
            "untrusted"
        };
        Found {
            store_id: String::new(),
            manifest_id: signed.label.clone(),
            hosts: vec!["store"],
            score: 100,
            examined,
        }
    }

    /// Near-identical matches come exact first, then earliest verified time first; an unverified
    /// time counts as none.
    #[test]
    fn matches_are_listed_exact_first_then_by_earliest_verified_time() {
        let s = signed();
        let mut matches = [
            found(&s, Some(("2026-10-10T09:00:00+00:00", true)), false, 0.97),
            found(&s, None, true, 1.0),
            found(&s, Some(("2026-10-09T09:00:00+00:00", false)), true, 1.0),
            found(&s, Some(("2026-10-10T08:00:00+00:00", true)), true, 1.0),
            found(&s, Some(("2026-10-01T08:00:00+00:00", true)), false, 0.93),
        ];
        matches.sort_by_key(match_order);
        let order: Vec<(bool, Option<&str>)> = matches
            .iter()
            .map(|f| (f.examined.iscc.exact, verified_time(f)))
            .collect();
        assert_eq!(
            order,
            [
                (true, Some("2026-10-10T08:00:00+00:00")),
                (true, None),
                (true, None),
                (false, Some("2026-10-01T08:00:00+00:00")),
                (false, Some("2026-10-10T09:00:00+00:00")),
            ]
        );
    }

    /// Search the test network for the file named by `LOOKUP_FILE` and print what was found:
    /// `LOOKUP_FILE=photo.jpg cargo test --lib live_lookup -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_lookup() {
        let path = std::env::var("LOOKUP_FILE").expect("LOOKUP_FILE names a file");
        let path = Path::new(&path);
        let units: Vec<String> = inspect::inspect(path)
            .unwrap()
            .iscc
            .into_iter()
            .map(|u| u.iscc)
            .collect();
        let found = lookup(path, &units).unwrap();
        for (list, entries) in [("match", &found.matches), ("related", &found.related)] {
            for f in entries {
                let e = &f.examined;
                println!(
                    "{list}: {} {:?} exact={} content={:?} for_file={:?} rebuild={:?} test_key={} attributed={}",
                    f.manifest_id, f.hosts, e.iscc.exact, e.iscc.content, e.for_file, e.rebuild, e.manifest.test_key, e.attributed
                );
            }
        }
        println!("left out: {:?}", found.left_out);
        println!(
            "unchecked: {}, failures: {:?}",
            found.unchecked, found.failures
        );
    }
}
