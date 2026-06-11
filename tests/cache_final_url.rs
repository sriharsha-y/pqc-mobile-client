//! Regression tests for `final_url` under the `cache` feature: the http-cache
//! streaming layer drops the response URL (see `cache::capture_response_url`),
//! which used to surface as `http://no.url.provided.local` instead of the
//! post-redirect URL. Local 302-redirect server; no network.
//!
//! Run: `cargo test --features cache --test cache_final_url`
#![cfg(feature = "cache")]

use pqc_client::{HttpMethod, HttpRequest, PqcConfig, PqcHttpClient, RedirectPolicy};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Minimal HTTP/1.1 server, keep-alive so reqwest can follow on one
/// connection: `/auth` → 302 to a non-cacheable 200; `/auth-cacheable` → 302
/// to a `max-age` 200 (so the composite response is cached and a repeat
/// request is a HIT).
async fn serve(listener: TcpListener) {
    loop {
        let (mut sock, _) = match listener.accept().await {
            Ok(x) => x,
            Err(_) => return,
        };
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                loop {
                    let n = match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let he = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let head = String::from_utf8_lossy(&buf[..he]).to_string();
                let path = head
                    .lines()
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/");
                let resp = if path.contains("/api/cacheable") {
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nCache-Control: max-age=3600\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nnull"
                } else if path.contains("/auth-cacheable") {
                    "HTTP/1.1 302 Found\r\nLocation: /api/cacheable?login_challenge=XYZ789\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
                } else if path.contains("/api/login") {
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4\r\nConnection: keep-alive\r\n\r\nnull"
                } else {
                    "HTTP/1.1 302 Found\r\nLocation: /api/login?login_challenge=ABC123\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n"
                };
                if sock.write_all(resp.as_bytes()).await.is_err() {
                    return;
                }
                buf.drain(..he);
            }
        });
    }
}

/// Caching is enabled iff `cache_dir` is given.
fn test_config(cache_dir: Option<&str>) -> PqcConfig {
    PqcConfig {
        pinned_domains: vec![],
        default_timeout_ms: Some(5_000),
        connect_timeout_ms: None,
        read_idle_timeout_ms: None,
        enable_cookies: true,
        user_agent: None,
        dns_resolver: None,
        proxy_url: None,
        redirect_policy: RedirectPolicy::Limited { max: 20 },
        max_inflight_total: None,
        max_inflight_per_host: None,
        enable_cache: cache_dir.is_some(),
        cache_dir: cache_dir.map(str::to_string),
        max_cache_bytes: None,
        max_memory_cache_bytes: None,
    }
}

fn get(url: String) -> HttpRequest {
    HttpRequest {
        method: HttpMethod::Get,
        url,
        headers: HashMap::new(),
        body: None,
        body_stream: None,
        body_stream_length: None,
        timeout_ms: None,
    }
}

async fn spawn_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener));
    addr
}

/// With the `cache` feature COMPILED but `enable_cache = false` (the runtime
/// default — and how release artifacts ship), the backend is `Plain` and a
/// redirected GET must report the post-redirect URL from `resp.url()`. Guards
/// against `final_url` being selected on `cfg(feature = "cache")` instead of
/// the actual backend.
#[tokio::test]
async fn plain_backend_under_cache_feature_preserves_post_redirect_final_url() {
    let addr = spawn_server().await;
    let client = PqcHttpClient::new(test_config(None)).expect("client should construct");

    let resp = client
        .request(get(format!("http://{addr}/auth")))
        .await
        .expect("request should succeed");
    let final_url = resp.final_url();
    let _ = resp.bytes().await;

    assert!(
        final_url.contains("/api/login") && final_url.contains("login_challenge=ABC123"),
        "Plain backend (enable_cache=false) under a cache-feature build must \
         still report the post-redirect URL, got: {final_url}"
    );
}

/// With caching on, a redirected GET must still report the POST-REDIRECT URL
/// (incl. query) — not the `no.url.provided.local` placeholder.
#[tokio::test]
async fn cached_redirect_preserves_post_redirect_final_url() {
    let addr = spawn_server().await;
    let dir = std::env::temp_dir().join("pqc_cache_final_url_test");
    let _ = std::fs::create_dir_all(&dir);
    let client =
        PqcHttpClient::new(test_config(dir.to_str())).expect("client should construct with cache");

    let resp = client
        .request(get(format!("http://{addr}/auth")))
        .await
        .expect("request should succeed");
    let final_url = resp.final_url();
    let _ = resp.bytes().await;
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !final_url.contains("no.url.provided.local"),
        "cache layer leaked the placeholder URL: {final_url}"
    );
    assert!(
        final_url.contains("/api/login") && final_url.contains("login_challenge=ABC123"),
        "final_url must be the post-redirect URL with its query, got: {final_url}"
    );
}

/// A cacheable redirected GET: the MISS reports the post-redirect URL; the
/// HIT falls back to the request URL (the middleware never runs on a hit —
/// documented divergence from native caches, see `cache::UrlSlot::take`).
/// Neither may be the placeholder.
#[tokio::test]
async fn cacheable_redirect_hit_falls_back_to_request_url() {
    let addr = spawn_server().await;
    let dir = std::env::temp_dir().join("pqc_cache_final_url_hit_test");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
    let client =
        PqcHttpClient::new(test_config(dir.to_str())).expect("client should construct with cache");
    let url = format!("http://{addr}/auth-cacheable");

    let miss = client
        .request(get(url.clone()))
        .await
        .expect("miss should succeed");
    let miss_url = miss.final_url();
    let _ = miss.bytes().await;

    let hit = client
        .request(get(url.clone()))
        .await
        .expect("hit should succeed");
    let hit_url = hit.final_url();
    let _ = hit.bytes().await;
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        miss_url.contains("/api/cacheable") && miss_url.contains("login_challenge=XYZ789"),
        "MISS must report the post-redirect URL, got: {miss_url}"
    );
    assert_eq!(
        hit_url, url,
        "HIT must fall back to the request URL, got: {hit_url}"
    );
}
