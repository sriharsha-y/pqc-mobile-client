//! Smoke test against Cloudflare's PQ research endpoint.
//!
//! Run with: `cargo test -- --nocapture`
//!
//! NOTE: requires network access. Skip in offline CI with `--skip network`.

use pqc_client::{HttpMethod, HttpRequest, PqcConfig, PqcHttpClient};
use std::collections::HashMap;

#[tokio::test]
async fn pq_handshake_cloudflare() {
    let client = PqcHttpClient::new(PqcConfig {
        pinned_cert_sha256: vec![],
        enable_post_quantum: true,
        enable_http3: false,
        default_timeout_ms: Some(15_000),
    });

    let req = HttpRequest {
        method: HttpMethod::Get,
        url: "https://pq.cloudflareresearch.com/".to_string(),
        headers: HashMap::new(),
        body: None,
        timeout_ms: None,
    };

    let resp = client.request(req).await.expect("request should succeed");
    assert!(resp.status < 500, "unexpected status: {}", resp.status);
    println!("status={} group={}", resp.status, resp.negotiated_named_group);
}
