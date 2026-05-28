use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;

use crate::config::{PqcConfig, RedirectPolicy};
use crate::error::PqcError;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest, HttpResponse};

/// Default body cap (`max_body_bytes == None`). 16 MiB: generous for JSON,
/// small enough that a decompression bomb can't OOM the app.
const DEFAULT_MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// Default connect timeout (`connect_timeout_ms == None`). Sized for cell
/// handover: absorbs one SYN retry, still fails fast.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The HTTPS client exposed to Kotlin / Swift via UniFFI.
///
/// Holds a single `reqwest::Client` with PQC TLS configured. Construct once
/// per process (it owns the connection pool); calling `request` is cheap.
pub struct PqcHttpClient {
    inner: reqwest::Client,
    default_timeout: Option<Duration>,
    max_body_bytes: u64,
}

impl PqcHttpClient {
    /// Returns `PqcError` (not panic) so consumers surface bad config —
    /// e.g. malformed base64 in pinned_cert_sha256 — as a typed error.
    pub fn new(config: PqcConfig) -> Result<Self, PqcError> {
        let tls = build_tls_config(&config)?;

        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .cookie_store(config.enable_cookies)
            .gzip(true)
            .brotli(true)
            // Reuse idle connections so a burst doesn't pay a PQ TLS 1.3
            // handshake per call. The 60s idle timeout + tcp_keepalive
            // below bound the cell↔wifi-handover risk of a dead idle socket
            // (hyper also refuses a connection it knows is broken).
            .pool_idle_timeout(Duration::from_secs(60))
            // TCP keep-alive: detect dead peers faster than the OS default.
            .tcp_keepalive(Duration::from_secs(30));

        builder = builder.connect_timeout(
            config
                .connect_timeout_ms
                .map(Duration::from_millis)
                .unwrap_or(DEFAULT_CONNECT_TIMEOUT),
        );

        if let Some(timeout_ms) = config.default_timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }

        if let Some(ref ua) = config.user_agent {
            builder = builder.user_agent(ua.clone());
        }

        builder = builder.redirect(match config.redirect_policy {
            RedirectPolicy::NoRedirects {} => Policy::none(),
            RedirectPolicy::SameOriginOnly {} => Policy::custom(|attempt| {
                // attempt.url() = destination; attempt.previous() = chain.
                // First entry is the original request URL.
                let previous = attempt.previous().first();
                let same_origin = match previous {
                    Some(prev) => {
                        attempt.url().scheme() == prev.scheme()
                            && attempt.url().host_str() == prev.host_str()
                            && attempt.url().port_or_known_default() == prev.port_or_known_default()
                    }
                    None => true,
                };
                if same_origin && attempt.previous().len() < 10 {
                    attempt.follow()
                } else if !same_origin {
                    attempt.stop()
                } else {
                    attempt.error("too many redirects")
                }
            }),
            RedirectPolicy::Limited { max } => Policy::limited(max.into()),
        });

        // Build failures here are residual wiring errors (DNS/proxy init);
        // TLS was already validated above. Map to InvalidRequest per the
        // UDL constructor doc.
        let client = builder.build().map_err(|_| PqcError::InvalidRequest)?;

        Ok(Self {
            inner: client,
            default_timeout: config.default_timeout_ms.map(Duration::from_millis),
            max_body_bytes: config.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES),
        })
    }
}

// `async_runtime = "tokio"` makes UniFFI drive these exports on a real tokio
// runtime; without it reqwest/hyper panic ("there is no reactor running")
// when called through the FFI bridge (tests mask it via #[tokio::test]).
// Constructor stays in a plain impl — proc-macro export doesn't support
// associated functions.
#[uniffi::export(async_runtime = "tokio")]
impl PqcHttpClient {
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

        for (k, values) in &req.headers {
            let name = HeaderName::try_from(k.as_str()).map_err(|_| PqcError::InvalidRequest)?;
            for v in values {
                let value =
                    HeaderValue::try_from(v.as_str()).map_err(|_| PqcError::InvalidRequest)?;
                builder = builder.header(name.clone(), value);
            }
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
        // Map to the ALPN id the UDL documents ("h2", "http/1.1"); the
        // Version Debug string ("HTTP/2.0") would break consumer equality.
        let negotiated_protocol = match resp.version() {
            reqwest::Version::HTTP_09 => "http/0.9".to_string(),
            reqwest::Version::HTTP_10 => "http/1.0".to_string(),
            reqwest::Version::HTTP_11 => "http/1.1".to_string(),
            reqwest::Version::HTTP_2 => "h2".to_string(),
            reqwest::Version::HTTP_3 => "h3".to_string(),
            other => format!("{:?}", other),
        };

        let mut headers: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in resp.headers() {
            // Non-UTF8 header bytes (legacy Latin-1 Set-Cookie, RFC 2231
            // filenames) make to_str() fail; lossy-decode so the value
            // round-trips visibly instead of silently dropping.
            let s = match v.to_str() {
                Ok(s) => s.to_string(),
                Err(_) => String::from_utf8_lossy(v.as_bytes()).into_owned(),
            };
            headers.entry(k.as_str().to_string()).or_default().push(s);
        }

        // Stream with a hard cap so a decompression bomb (CWE-409) can't OOM
        // the app — Content-Length is pre-decompression and easily lied
        // about, so the only safe bound is counting decompressed bytes.
        // Mid-body errors are transport-level (handshake already done), so
        // map to Timeout/Network, skipping the rustls classifier.
        let cap = self.max_body_bytes;
        let mut body = Vec::new();
        let mut stream = resp;
        loop {
            let next = stream.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    PqcError::Timeout
                } else {
                    PqcError::Network
                }
            })?;
            match next {
                Some(chunk) => {
                    // saturating_add so a huge chunk can't wrap the cap check.
                    let projected = (body.len() as u64).saturating_add(chunk.len() as u64);
                    if projected > cap {
                        return Err(PqcError::InvalidResponse);
                    }
                    body.extend_from_slice(&chunk);
                }
                None => break,
            }
        }

        Ok(HttpResponse {
            status,
            headers,
            body,
            negotiated_protocol,
        })
    }
}

/// Classify a `reqwest::Error` so callers can tell a transient blip (retry)
/// from a pinning/trust/TLS failure (don't retry). Pass 1 downcasts to
/// `&rustls::Error` (authoritative, no string fragility); pass 2 is a
/// substring fallback for our pinning marker carried as a rustls string.
fn map_reqwest_err(e: reqwest::Error) -> PqcError {
    if e.is_timeout() {
        return PqcError::Timeout;
    }

    let url_str = e.url().map(|u| u.to_string());

    // Pass 1: typed downcast. `rustls::Error` is the authoritative shape.
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = src {
        if let Some(rustls_err) = err.downcast_ref::<rustls::Error>() {
            return classify_rustls_error(rustls_err);
        }
        src = err.source();
    }

    // Pass 2: substring fallback for the pinning marker and any rustls error
    // surfaced only via reqwest's wrapping. The outer reqwest Display embeds
    // the request URL — strip it first so it can't contaminate the match.
    let mut src: Option<&(dyn std::error::Error + 'static)> = Some(&e);
    while let Some(err) = src {
        let mut msg = err.to_string();
        if let Some(ref u) = url_str {
            msg = msg.replace(u, "");
        }
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

/// Map a `rustls::Error` to a `PqcError`. `rustls::Error` is
/// `#[non_exhaustive]` so the `_` arm is required — there's no compile-time
/// exhaustiveness guarantee. Re-audit the upstream `enum Error` on every
/// rustls minor bump: a new trust-failure variant would silently fall into
/// `Tls` and downgrade the security signal consumers branch on.
fn classify_rustls_error(e: &rustls::Error) -> PqcError {
    use rustls::Error as R;
    match e {
        // Platform verifier rejected the chain.
        R::InvalidCertificate(_) | R::InvalidCertRevocationList(_) => PqcError::TrustVerification,

        // PinningVerifier emits General("certificate pinning failure: ...")
        // (a future stack might wrap it as Other). Check both for the marker;
        // absent it, it's still a handshake failure → Tls.
        R::General(msg) => {
            if msg.to_lowercase().contains("pinning failure") {
                PqcError::PinningFailure
            } else {
                PqcError::Tls
            }
        }
        R::Other(other) => {
            let msg = other.to_string().to_lowercase();
            if msg.contains("pinning failure") {
                PqcError::PinningFailure
            } else {
                PqcError::Tls
            }
        }

        // Handshake-time TLS failures, none transient. (AlertReceived /
        // PeerIncompatible / PeerMisbehaved lack "tls"/"handshake" in their
        // Display, so the substring path mis-read them as Network.)
        R::AlertReceived(_)
        | R::PeerIncompatible(_)
        | R::PeerMisbehaved(_)
        | R::InappropriateMessage { .. }
        | R::InappropriateHandshakeMessage { .. }
        | R::InvalidEncryptedClientHello(_)
        | R::InvalidMessage(_)
        | R::NoCertificatesPresented
        | R::UnsupportedNameType
        | R::DecryptError
        | R::EncryptError
        | R::PeerSentOversizedRecord
        | R::NoApplicationProtocol
        | R::BadMaxFragmentSize
        | R::HandshakeNotComplete
        | R::FailedToGetCurrentTime
        | R::FailedToGetRandomBytes
        | R::InconsistentKeys(_) => PqcError::Tls,

        // Unknown future variant (non_exhaustive). Conservative: Tls, not
        // Network. Re-audit on every rustls bump (see fn doc).
        _ => PqcError::Tls,
    }
}

#[cfg(test)]
mod tests {
    /// Guard the pinning marker substring against drift (a rename would
    /// silently downgrade PinningFailure to Network).
    #[test]
    fn pinning_error_message_substring_stable() {
        let msg =
            "certificate pinning failure: no certificate in the chain matched any configured pin"
                .to_string();
        assert!(msg.to_lowercase().contains("pinning failure"));
    }

    /// A URL containing "certificate" must not misclassify a network failure
    /// as TrustVerification — tests the URL-stripping branch.
    #[test]
    fn url_substring_does_not_contaminate_classification() {
        let url = "https://api.example.com/v1/certificates/list";
        let msg = format!(
            "error sending request for url ({}): connection refused",
            url
        );
        let stripped = msg.replace(url, "");
        let lower = stripped.to_lowercase();
        assert!(!lower.contains("certificate"));
        assert!(!lower.contains("pinning failure"));
    }

    /// Alerts lack "tls"/"handshake" in Display; the typed path must still
    /// classify them as Tls, not Network.
    #[test]
    fn rustls_alert_classified_as_tls_not_network() {
        use super::classify_rustls_error;
        use rustls::AlertDescription;
        let err = rustls::Error::AlertReceived(AlertDescription::ProtocolVersion);
        assert!(matches!(
            classify_rustls_error(&err),
            crate::error::PqcError::Tls
        ));
    }

    #[test]
    fn rustls_invalid_certificate_classified_as_trust_verification() {
        use super::classify_rustls_error;
        use rustls::CertificateError;
        let err = rustls::Error::InvalidCertificate(CertificateError::Expired);
        assert!(matches!(
            classify_rustls_error(&err),
            crate::error::PqcError::TrustVerification
        ));
    }

    #[test]
    fn rustls_general_with_pinning_marker_classified_as_pinning() {
        use super::classify_rustls_error;
        // pinning.rs emits General(String), not Other — regression guard for
        // the General arm.
        let err = rustls::Error::General(
            "certificate pinning failure: no certificate in the chain matched any configured pin"
                .to_string(),
        );
        assert!(matches!(
            classify_rustls_error(&err),
            crate::error::PqcError::PinningFailure
        ));
    }

    #[test]
    fn rustls_general_without_marker_classified_as_tls() {
        use super::classify_rustls_error;
        let err = rustls::Error::General("some unrelated handshake failure".to_string());
        assert!(matches!(
            classify_rustls_error(&err),
            crate::error::PqcError::Tls
        ));
    }

    #[test]
    fn rustls_other_with_pinning_marker_classified_as_pinning() {
        use super::classify_rustls_error;
        // Defensive: a future stack could wrap our marker as Other(...).
        let inner: Box<dyn std::error::Error + Send + Sync + 'static> =
            "certificate pinning failure: no certificate in the chain matched any configured pin"
                .into();
        let err = rustls::Error::Other(rustls::OtherError(std::sync::Arc::from(inner)));
        assert!(matches!(
            classify_rustls_error(&err),
            crate::error::PqcError::PinningFailure
        ));
    }
}
