//! Smoke test against Cloudflare's PQ research endpoint.
//!
//! Run with: `cargo test --release --test smoke -- --nocapture`
//!
//! Whether the PQ hybrid was actually negotiated is confirmed via the
//! SERVER's report — Cloudflare's `/cdn-cgi/trace` returns a `kex=` line
//! with the key exchange the edge negotiated for that connection. This
//! is authoritative and per-connection, so the tests no longer depend on
//! any client-side global state and can run in parallel.
//!
//! NOTE: requires network access. Skip in offline CI with `--skip network`.

use pqc_client::{HttpMethod, HttpRequest, PqcConfig, PqcError, PqcHttpClient, RedirectPolicy};
use std::collections::HashMap;

/// Extract the `kex=` value from a Cloudflare `/cdn-cgi/trace` body.
fn parse_kex(trace_body: &str) -> Option<String> {
    trace_body
        .lines()
        .find_map(|line| line.strip_prefix("kex="))
        .map(|s| s.trim().to_string())
}

/// Default config for these tests. Matches the documented production
/// defaults so a behavior drift between test config and the example
/// app's call sites is visible.
fn default_test_config() -> PqcConfig {
    PqcConfig {
        pinned_cert_sha256: vec![],
        enable_post_quantum: true,
        default_timeout_ms: Some(15_000),
        connect_timeout_ms: None,
        max_body_bytes: None,
        enable_cookies: false,
        user_agent: Some("pqc-client-smoke-test/0.3.1".to_string()),
        redirect_policy: RedirectPolicy::SameOriginOnly {},
    }
}

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: HashMap::new(),
        body: None,
        timeout_ms: None,
    }
}

#[tokio::test]
async fn pq_handshake_cloudflare() {
    let client = PqcHttpClient::new(default_test_config())
        .expect("client construction should succeed with empty pin list");
    // /cdn-cgi/trace returns the key exchange the edge negotiated, in
    // the body — server-authoritative, no client-side global involved.
    let resp = client
        .request(get("https://pq.cloudflareresearch.com/cdn-cgi/trace"))
        .await
        .expect("request should succeed");
    assert!(resp.status < 500, "unexpected status: {}", resp.status);
    let body = String::from_utf8_lossy(&resp.body);
    let kex = parse_kex(&body).expect("trace body should contain a kex= line");
    println!(
        "status={} kex={} alpn={}",
        resp.status, kex, resp.negotiated_protocol
    );
    assert_eq!(
        kex, "X25519MLKEM768",
        "Cloudflare should negotiate X25519MLKEM768 when the client offers it"
    );
    // Regression for M1: ALPN must be set so reqwest negotiates h2 with
    // any HTTP/2-capable server. Without `tls.alpn_protocols`, the
    // server falls back to HTTP/1.1 silently and our `http2(...)`
    // feature flag becomes a lie.
    assert_eq!(
        resp.negotiated_protocol, "h2",
        "ALPN must select h2 against Cloudflare"
    );
}

/// Classical handshake: with PQC disabled on the CLIENT, it offers only
/// classical groups, so the edge negotiates X25519 regardless of whether
/// it supports PQC. This is deterministic and server-independent — we no
/// longer depend on finding a server that happens to lack PQC support.
/// Confirmed via the same server-authoritative `/cdn-cgi/trace` `kex=`.
#[tokio::test]
async fn classical_handshake_when_pq_disabled() {
    let mut cfg = default_test_config();
    cfg.enable_post_quantum = false;
    let client = PqcHttpClient::new(cfg).expect("client should construct");
    let resp = client
        .request(get("https://pq.cloudflareresearch.com/cdn-cgi/trace"))
        .await
        .expect("request should succeed");
    assert!(resp.status < 500, "unexpected status: {}", resp.status);
    let body = String::from_utf8_lossy(&resp.body);
    let kex = parse_kex(&body).expect("trace body should contain a kex= line");
    println!("status={} kex={}", resp.status, kex);
    assert_eq!(
        kex, "X25519",
        "with PQC disabled the client should negotiate classical X25519"
    );
}

/// Pin failure: configure an obviously-wrong pin and assert the typed
/// error variant. Validates the M5 typed downcast path and the
/// pinning verifier's error propagation end-to-end.
#[tokio::test]
async fn pin_failure_surfaces_typed_error() {
    let mut cfg = default_test_config();
    // 32 zero bytes, base64-encoded — guaranteed not to match any real
    // SPKI hash.
    cfg.pinned_cert_sha256 = vec!["AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string()];
    let client = PqcHttpClient::new(cfg).expect("client should construct with bogus pin");
    let err = client
        .request(get("https://pq.cloudflareresearch.com/"))
        .await
        .expect_err("pin mismatch must fail");
    assert!(
        matches!(err, PqcError::PinningFailure),
        "expected PinningFailure, got {:?}",
        err
    );
}

/// Trust verification: hit a host whose cert chain the platform
/// verifier should reject (badssl.com's expired endpoint). Validates
/// that PqcError::TrustVerification is what surfaces, not a generic
/// Network or Tls error. badssl.com is documented as a stable test
/// fixture — see https://badssl.com.
#[tokio::test]
async fn trust_failure_surfaces_typed_error() {
    let client = PqcHttpClient::new(default_test_config()).expect("client should construct");
    let err = client
        .request(get("https://expired.badssl.com/"))
        .await
        .expect_err("expired cert must fail");
    assert!(
        matches!(err, PqcError::TrustVerification),
        "expected TrustVerification, got {:?}",
        err
    );
}

/// POST with a body: verifies the request-encoding path end-to-end —
/// method, headers, and body bytes all survive the FFI boundary and
/// land at the server as advertised. httpbin.org/post echoes the
/// posted JSON under `json` (or `data` if the content-type is not
/// application/json), so we can inspect the round-trip.
///
/// Why httpbin: documented stable echo service that supports POST,
/// arbitrary headers, and HTTPS with a normally-trusted leaf cert.
/// If httpbin.org is ever decommissioned, swap to postman-echo.com or
/// stand up a local rustls server fixture.
#[tokio::test]
async fn post_body_round_trips() {
    let client = PqcHttpClient::new(default_test_config()).expect("client should construct");

    let body = br#"{"hello":"pqc","n":42}"#.to_vec();
    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        vec!["application/json".to_string()],
    );

    let req = HttpRequest {
        method: HttpMethod::Post,
        url: "https://httpbin.org/post".to_string(),
        headers,
        body: Some(body.clone()),
        timeout_ms: None,
    };

    let resp = client.request(req).await.expect("POST should succeed");
    assert_eq!(resp.status, 200, "POST should return 200");

    // httpbin echoes the body bytes back under either `data` or `json`.
    // Don't full-parse the JSON; substring-match the unique payload we
    // sent so the assertion isn't coupled to httpbin's exact schema.
    let body_str = String::from_utf8_lossy(&resp.body);
    assert!(
        body_str.contains("\"hello\""),
        "echoed body should contain our payload key, got: {body_str}"
    );
    assert!(
        body_str.contains("\"pqc\""),
        "echoed body should contain our payload value, got: {body_str}"
    );
}

/// Concurrent requests on a SINGLE PqcHttpClient: catches regressions
/// where the client (or its underlying reqwest::Client / hyper pool)
/// is not Send/Sync-safe across tokio tasks.
///
/// Why this matters at the FFI: the UniFFI-exposed `request` method
/// is `async`, and consumers on iOS/Android routinely fan out parallel
/// calls (image grids, prefetch waves). A non-Sync client would either
/// panic under load or silently serialize requests — the latter is
/// invisible in single-threaded tests but tanks throughput in prod.
#[tokio::test]
async fn concurrent_requests_share_one_client() {
    use std::sync::Arc;

    let client =
        Arc::new(PqcHttpClient::new(default_test_config()).expect("client should construct"));

    // 8 = enough to exercise hyper's connection pool (default
    // per-host pool size is small) and force at least one new
    // connection beyond the first reused one. Small enough that
    // Cloudflare's PQ research endpoint doesn't rate-limit us.
    const N: usize = 8;

    let handles: Vec<_> = (0..N)
        .map(|_| {
            let c = client.clone();
            tokio::spawn(async move { c.request(get("https://pq.cloudflareresearch.com/")).await })
        })
        .collect();

    let mut ok = 0usize;
    for h in handles {
        let resp = h
            .await
            .expect("task should not panic")
            .expect("request should succeed");
        assert!(resp.status < 500, "unexpected status: {}", resp.status);
        // Liveness is the signal here: 8 tasks sharing one
        // Arc<PqcHttpClient> all reach completion without panicking or
        // deadlocking. (Per-request KEX confirmation belongs to the
        // server-side `/cdn-cgi/trace` check, not a client global.)
        ok += 1;
    }
    assert_eq!(ok, N, "all {N} concurrent requests should succeed");
}
