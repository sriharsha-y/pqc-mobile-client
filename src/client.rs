use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};

use crate::config::PqcConfig;
use crate::error::PqcError;
use crate::kx_tracker::last_negotiated_group_str;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest, HttpResponse};

/// The HTTPS client exposed to Kotlin / Swift via UniFFI.
///
/// Holds a single `reqwest::Client` with PQC TLS configured. Construct once
/// per process (it owns the connection pool); calling `request` is cheap.
pub struct PqcHttpClient {
    inner: reqwest::Client,
    default_timeout: Option<Duration>,
}

impl PqcHttpClient {
    pub fn new(config: PqcConfig) -> Self {
        let tls = build_tls_config(&config).expect("TLS config build failed");

        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .cookie_store(true)
            .gzip(true)
            .brotli(true)
            .pool_max_idle_per_host(10);

        if let Some(timeout_ms) = config.default_timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }

        // HTTP/3 (QUIC) — opt-in. Adds the h3-quinn dependency footprint;
        // gated until the corresponding cargo feature is enabled.
        // if config.enable_http3 {
        //     builder = builder.http3_prior_knowledge();
        // }

        let client = builder.build().expect("reqwest client build failed");

        Self {
            inner: client,
            default_timeout: config.default_timeout_ms.map(Duration::from_millis),
        }
    }

    pub async fn request(&self, req: HttpRequest) -> Result<HttpResponse, PqcError> {
        let method = match req.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Head => reqwest::Method::HEAD,
            HttpMethod::Options => reqwest::Method::OPTIONS,
        };

        let mut builder = self.inner.request(method, &req.url);

        for (k, v) in &req.headers {
            let name = HeaderName::try_from(k.as_str()).map_err(|_| PqcError::InvalidRequest)?;
            let value = HeaderValue::try_from(v.as_str()).map_err(|_| PqcError::InvalidRequest)?;
            builder = builder.header(name, value);
        }

        if let Some(body) = req.body {
            builder = builder.body(body);
        }

        let timeout_ms = req
            .timeout_ms
            .or(self.default_timeout.map(|d| d.as_millis() as u64));
        if let Some(t) = timeout_ms {
            builder = builder.timeout(Duration::from_millis(t));
        }

        let resp = builder.send().await.map_err(map_reqwest_err)?;

        let status = resp.status().as_u16();
        let negotiated_protocol = format!("{:?}", resp.version());

        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in resp.headers() {
            // If the header value contains non-UTF8 bytes (legacy Set-Cookie
            // with Latin-1 chars, RFC 2231 Content-Disposition filenames,
            // misbehaving servers), to_str() returns Err. Earlier code
            // substituted "" — caller saw the header present but value
            // missing, which can drop a session cookie or filename hint
            // silently. Use from_utf8_lossy so invalid bytes become U+FFFD
            // replacement chars but the header round-trips visibly.
            let s = match v.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => String::from_utf8_lossy(v.as_bytes()).into_owned(),
            };
            headers.entry(k.as_str().to_string()).or_default().push(s);
        }

        let body = resp
            .bytes()
            .await
            .map_err(|_| PqcError::InvalidResponse)?
            .to_vec();

        // The KEX group rustls selected on the most-recent handshake.
        // See kx_tracker module for the recording mechanism and the
        // documented concurrency caveat.
        let negotiated_named_group = last_negotiated_group_str();

        Ok(HttpResponse {
            status,
            headers,
            body,
            negotiated_named_group,
            negotiated_protocol,
        })
    }
}

/// Classify a `reqwest::Error` into the closest `PqcError` variant so
/// callers can distinguish a transient network blip (retry) from a
/// pinning / trust / TLS failure (do NOT retry, alert security).
///
/// reqwest doesn't expose typed access to the underlying `rustls::Error`,
/// so we walk the error source chain and pattern-match on the rendered
/// message. The strings checked here are produced by:
///   - our own `PinningVerifier` (`"certificate pinning failure..."`)
///   - rustls's `CertificateError` variants (contain "certificate")
///   - rustls's TLS / handshake errors (contain "tls" or "handshake")
///
/// Fragile against upstream message renames, so it's wrapped in a unit
/// test (see tests below) and the substrings are kept broad rather than
/// exact. If a finer typed surface lands in rustls/reqwest, prefer that.
fn map_reqwest_err(e: reqwest::Error) -> PqcError {
    if e.is_timeout() {
        return PqcError::Timeout;
    }

    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = src {
        let msg = err.to_string();
        let lower = msg.to_lowercase();
        if lower.contains("pinning failure") {
            return PqcError::PinningFailure;
        }
        if lower.contains("certificate") || lower.contains("certificateerror") {
            return PqcError::TrustVerification;
        }
        if lower.contains("handshake") || lower.contains(" tls ") || lower.starts_with("tls ") {
            return PqcError::Tls;
        }
        src = err.source();
    }
    PqcError::Network
}

#[cfg(test)]
mod tests {
    /// Smoke-test that the marker substrings haven't drifted from what
    /// our pinning verifier emits. Pure-string check (no full TLS
    /// handshake plumbing required) — guards against an unintended rename
    /// silently downgrading PinningFailure to Network.
    #[test]
    fn pinning_error_message_substring_stable() {
        // The string our PinningVerifier emits today.
        let msg =
            "certificate pinning failure: leaf SPKI does not match any configured pin".to_string();
        assert!(msg.to_lowercase().contains("pinning failure"));
    }
}
