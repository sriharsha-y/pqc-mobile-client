//! Smoke tests against Cloudflare's PQ research endpoint. Requires network.
//! PQ negotiation is confirmed server-side via `/cdn-cgi/trace` (`kex=`),
//! which is per-connection and authoritative, so tests run in parallel.
//! Run: `cargo test --release --test smoke -- --nocapture`

use pqc_client::{
    BodyProvider, HttpMethod, HttpRequest, PqcConfig, PqcError, PqcHttpClient, PqcResponse,
    RedirectPolicy,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
        default_timeout_ms: Some(15_000),
        connect_timeout_ms: None,
        read_idle_timeout_ms: None,
        enable_cookies: false,
        user_agent: Some("pqc-client-smoke-test/0.3.1".to_string()),
        redirect_policy: RedirectPolicy::SameOriginOnly {},
        dns_resolver: None,
        proxy_url: None,
        max_inflight_total: Some(64),
        max_inflight_per_host: Some(5),
        enable_cache: false,
        cache_dir: None,
        max_cache_bytes: None,
        max_memory_cache_bytes: None,
    }
}

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Get,
        url: url.to_string(),
        headers: HashMap::new(),
        body: None,
        body_stream: None,
        body_stream_length: None,
        timeout_ms: None,
    }
}

/// Send a request, retrying a few times to ride out transient flakiness in
/// the external test endpoints. These tests gate CI (check.yml runs on
/// every PR and push:main), and public echo/research endpoints — httpbin
/// especially — intermittently time out or return 5xx under load, which
/// would otherwise red main for reasons unrelated to this crate.
///
/// Retries on transport errors and on 5xx/429 with linear backoff;
/// returns the first response with a < 500, non-429 status. Use ONLY for
/// success-path tests — error-path tests (pin / trust failures) assert on
/// the returned Err and must NOT retry.
async fn request_resilient(client: &PqcHttpClient, req: HttpRequest) -> Arc<PqcResponse> {
    const ATTEMPTS: u32 = 4;
    let mut last = String::new();
    for attempt in 1..=ATTEMPTS {
        match client.request(req.clone()).await {
            Ok(resp) if resp.status() < 500 && resp.status() != 429 => return resp,
            Ok(resp) => last = format!("HTTP {}", resp.status()),
            Err(e) => last = format!("{e:?}"),
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_secs(attempt as u64));
        }
    }
    panic!(
        "request to {} failed after {ATTEMPTS} attempts (last: {last})",
        req.url
    );
}

#[tokio::test]
async fn pq_handshake_cloudflare() {
    let client = PqcHttpClient::new(default_test_config())
        .expect("client construction should succeed with empty pin list");
    // /cdn-cgi/trace returns the key exchange the edge negotiated, in
    // the body — server-authoritative, no client-side global involved.
    let resp = request_resilient(
        &client,
        get("https://pq.cloudflareresearch.com/cdn-cgi/trace"),
    )
    .await;
    let status = resp.status();
    let final_url = resp.final_url();
    let negotiated_protocol = resp.negotiated_protocol();
    let body_bytes = resp.bytes().await.expect("body drain should succeed");
    let body = String::from_utf8_lossy(&body_bytes);
    let kex = parse_kex(&body).expect("trace body should contain a kex= line");
    println!("status={status} kex={kex} alpn={negotiated_protocol}");
    assert_eq!(
        kex, "X25519MLKEM768",
        "Cloudflare should negotiate X25519MLKEM768 when the client offers it"
    );
    // No redirect on /cdn-cgi/trace, so final_url should echo the request URL.
    assert_eq!(
        final_url, "https://pq.cloudflareresearch.com/cdn-cgi/trace",
        "final_url should report the URL the body came from"
    );
    // ALPN must be set so the server negotiates h2; without it the http2
    // feature is silently a no-op.
    assert_eq!(
        negotiated_protocol, "h2",
        "ALPN must select h2 against Cloudflare"
    );
}

/// A bogus pin must surface the typed PinningFailure end-to-end.
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

/// An expired cert (badssl.com) must surface TrustVerification, not a
/// generic Network/Tls error.
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

/// POST round-trip: method, headers, and body survive to the server.
/// Tries multiple public echo endpoints in order — postman-echo first
/// (Postman-backed, generally reliable), then httpbin.org as a fallback
/// because both have hit recurring 5xx incidents under load and the test
/// shouldn't red CI for an unrelated third-party outage. If all echo
/// services are down the test fails loudly so we don't silently skip.
#[tokio::test]
async fn post_body_round_trips() {
    let client = PqcHttpClient::new(default_test_config()).expect("client should construct");

    let body = br#"{"hello":"pqc","n":42}"#.to_vec();
    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        vec!["application/json".to_string()],
    );

    // Endpoints tried in order. Both are public echo services; we want
    // at least one to be up. Each gets the full `request_resilient`
    // retry budget before falling through to the next.
    let endpoints = ["https://postman-echo.com/post", "https://httpbin.org/post"];

    let mut last_err: Option<String> = None;
    let mut got_resp = None;
    for url in endpoints {
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: url.to_string(),
            headers: headers.clone(),
            body: Some(body.clone()),
            body_stream: None,
            body_stream_length: None,
            timeout_ms: None,
        };
        // Each endpoint is its own resilient attempt; we don't panic
        // until ALL endpoints have been tried. So inline the retry
        // loop instead of using request_resilient's panic-on-fail.
        let mut endpoint_ok = None;
        for attempt in 1..=3 {
            match client.request(req.clone()).await {
                Ok(r) if r.status() < 500 && r.status() != 429 => {
                    endpoint_ok = Some(r);
                    break;
                }
                Ok(r) => last_err = Some(format!("{url}: HTTP {}", r.status())),
                Err(e) => last_err = Some(format!("{url}: {e:?}")),
            }
            if attempt < 3 {
                std::thread::sleep(std::time::Duration::from_secs(attempt));
            }
        }
        if let Some(r) = endpoint_ok {
            got_resp = Some(r);
            break;
        }
    }

    let resp =
        got_resp.unwrap_or_else(|| panic!("all POST echo endpoints failed (last: {last_err:?})"));
    assert_eq!(resp.status(), 200, "POST should return 200");

    // Both echo services include the unique payload bytes back somewhere
    // in the response (postman-echo under `json`, httpbin under `data`
    // or `json`). Substring-match the unique payload so the assertion
    // isn't coupled to either's exact schema.
    let body_bytes = resp.bytes().await.expect("body drain should succeed");
    let body_str = String::from_utf8_lossy(&body_bytes);
    assert!(
        body_str.contains("\"hello\""),
        "echoed body should contain our payload key, got: {body_str}"
    );
    assert!(
        body_str.contains("\"pqc\""),
        "echoed body should contain our payload value, got: {body_str}"
    );
}

/// Many concurrent requests on one client — guards Send/Sync safety across
/// tokio tasks, which consumers rely on when fanning out calls.
#[tokio::test]
async fn concurrent_requests_share_one_client() {
    let client =
        Arc::new(PqcHttpClient::new(default_test_config()).expect("client should construct"));

    // Enough to exercise the connection pool without rate-limiting.
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
        let status = resp.status();
        assert!(status < 500, "unexpected status: {status}");
        // Drop the response without draining the body — confirms the
        // permit-release-on-drop path is healthy under concurrent load.
        // (Dropping aborts the body stream; matches OkHttp `close()`.)
        drop(resp);
        ok += 1;
    }
    assert_eq!(ok, N, "all {N} concurrent requests should succeed");
}

/// Regression for the 0.8.x FFI permit-leak: a foreign consumer that
/// holds the `Arc<PqcResponse>` alive after `cancel()` (Kotlin Cleaner
/// pattern) must still release the per-host inflight permit so the
/// NEXT request can proceed. Pre-fix the permits only released on
/// `Drop`. Shape: cap=2 per host, 4 sequential requests, each holds
/// its Arc → pre-fix the 3rd `request()` deadlocks on `acquire_owned`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_releases_inflight_permits_under_ffi_holder_pattern() {
    let mut cfg = default_test_config();
    cfg.max_inflight_per_host = Some(2);
    cfg.max_inflight_total = Some(64);
    let client = PqcHttpClient::new(cfg).expect("client should construct");

    let work = async {
        let mut held: Vec<Arc<PqcResponse>> = Vec::new();
        for _ in 0..4u32 {
            let resp = request_resilient(&client, get("https://pq.cloudflareresearch.com/")).await;
            resp.cancel();
            held.push(resp);
            // Yield so the next acquire can park if it's going to —
            // without this we'd only catch a synchronous block.
            tokio::task::yield_now().await;
        }
    };

    tokio::time::timeout(std::time::Duration::from_secs(30), work)
        .await
        .expect("permit starvation: cap=2 + FFI-holder pattern deadlocked the 3rd request");
}

/// Test-only `BodyProvider` that yields pre-staged chunks. Lets us
/// drive the streaming-upload code path without a real file source.
struct VecBodyProvider {
    chunks: Mutex<std::collections::VecDeque<Vec<u8>>>,
}

impl VecBodyProvider {
    fn new<I: IntoIterator<Item = Vec<u8>>>(chunks: I) -> Self {
        Self {
            chunks: Mutex::new(chunks.into_iter().collect()),
        }
    }
}

impl BodyProvider for VecBodyProvider {
    fn next_chunk(&self) -> Result<Option<Vec<u8>>, PqcError> {
        Ok(self
            .chunks
            .lock()
            .expect("provider lock poisoned")
            .pop_front())
    }
    fn cancel(&self) {
        // Drop any remaining chunks so a re-poll after cancel sees EOF.
        // The in-memory provider has nothing else to release.
        self.chunks.lock().expect("provider lock poisoned").clear();
    }
}

/// Streaming POST: send a body via `BodyProvider` (chunks pulled by
/// the client over FFI / `spawn_blocking`) and verify the assembled
/// payload reaches the server. Three chunks → reqwest concatenates +
/// uses chunked transfer-encoding (no Content-Length set), so this
/// exercises both the streaming wire format and the multi-chunk
/// FFI pull loop.
#[tokio::test]
async fn streaming_post_body_round_trips() {
    let client = PqcHttpClient::new(default_test_config()).expect("client should construct");

    // Three chunks; the server should reassemble to the concatenation.
    let chunks: Vec<Vec<u8>> = vec![
        br#"{"a":"#.to_vec(),
        br#""hello","b":"#.to_vec(),
        br#""world"}"#.to_vec(),
    ];

    let mut headers = HashMap::new();
    headers.insert(
        "content-type".to_string(),
        vec!["application/json".to_string()],
    );

    let endpoints = ["https://postman-echo.com/post", "https://httpbin.org/post"];
    let mut last_err: Option<String> = None;
    let mut got_resp: Option<Arc<PqcResponse>> = None;
    for url in endpoints {
        // Each retry needs a FRESH provider — streams aren't rewindable.
        let provider: Arc<dyn BodyProvider> = Arc::new(VecBodyProvider::new(chunks.clone()));
        let req = HttpRequest {
            method: HttpMethod::Post,
            url: url.to_string(),
            headers: headers.clone(),
            body: None,
            body_stream: Some(provider),
            body_stream_length: None, // chunked transfer-encoding
            timeout_ms: None,
        };
        match client.request(req).await {
            Ok(r) if r.status() < 500 && r.status() != 429 => {
                got_resp = Some(r);
                break;
            }
            Ok(r) => last_err = Some(format!("{url}: HTTP {}", r.status())),
            Err(e) => last_err = Some(format!("{url}: {e:?}")),
        }
    }

    let resp = got_resp
        .unwrap_or_else(|| panic!("all streaming POST echo endpoints failed (last: {last_err:?})"));
    assert_eq!(resp.status(), 200, "streaming POST should return 200");

    let body_bytes = resp.bytes().await.expect("body drain should succeed");
    let body_str = String::from_utf8_lossy(&body_bytes);
    // The concatenated payload contains both keys + values; echo
    // services include the request body somewhere in the response.
    assert!(
        body_str.contains("\"hello\"") && body_str.contains("\"world\""),
        "echoed body should contain the assembled payload, got: {body_str}"
    );
}

/// `proxy_url` actually routes traffic: stand up an in-process HTTP CONNECT
/// proxy, point the client at it, and assert (a) the request still succeeds
/// end-to-end and (b) the proxy observed a CONNECT to the target host. No
/// MITM / trusted CA is needed — CONNECT tunnels the TLS bytes through to the
/// real server, so this proves routing without decrypting.
#[tokio::test]
async fn proxy_routes_traffic_through_local_connect_proxy() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    // Hosts the proxy was asked to CONNECT to — the proof of routing.
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let seen_acceptor = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let seen = seen_acceptor.clone();
            tokio::spawn(async move {
                // Read request head up to CRLFCRLF (the CONNECT line + headers).
                let mut buf = Vec::new();
                let mut tmp = [0u8; 1024];
                loop {
                    match client.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                // `CONNECT host:port HTTP/1.1` → capture `host:port`.
                let head = String::from_utf8_lossy(&buf);
                let target = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or_default()
                    .to_string();
                seen.lock().expect("seen poisoned").push(target.clone());

                // Open the tunnel to the real target and splice both ways.
                let Ok(mut upstream) = TcpStream::connect(&target).await else {
                    return;
                };
                if client
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            });
        }
    });

    let mut cfg = default_test_config();
    cfg.proxy_url = Some(format!("http://{proxy_addr}"));
    let client = PqcHttpClient::new(cfg).expect("client should construct with a proxy");

    let resp = request_resilient(
        &client,
        get("https://pq.cloudflareresearch.com/cdn-cgi/trace"),
    )
    .await;
    assert!(
        resp.status() < 500,
        "request via proxy returned {}",
        resp.status()
    );
    let _ = resp.bytes().await;

    let targets = seen.lock().expect("seen poisoned").clone();
    assert!(
        targets
            .iter()
            .any(|t| t.contains("pq.cloudflareresearch.com")),
        "proxy should have seen a CONNECT to the target host; saw: {targets:?}"
    );
}

/// Body + body_stream both set → InvalidRequest. Guards the
/// mutually-exclusive contract documented on `HttpRequest`.
#[tokio::test]
async fn body_and_body_stream_both_set_rejected() {
    let client = PqcHttpClient::new(default_test_config()).expect("client should construct");
    let provider: Arc<dyn BodyProvider> = Arc::new(VecBodyProvider::new(vec![b"x".to_vec()]));
    let req = HttpRequest {
        method: HttpMethod::Post,
        url: "https://postman-echo.com/post".to_string(),
        headers: HashMap::new(),
        body: Some(b"y".to_vec()),
        body_stream: Some(provider),
        body_stream_length: None,
        timeout_ms: None,
    };
    let err = client.request(req).await.expect_err("must reject");
    assert!(
        matches!(err, PqcError::InvalidRequest),
        "expected InvalidRequest, got {:?}",
        err
    );
}
