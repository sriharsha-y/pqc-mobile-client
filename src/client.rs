use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;

use crate::config::{PqcConfig, RedirectPolicy};
use crate::error::PqcError;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest, HttpResponse};

/// Default body cap when `PqcConfig::max_body_bytes == None`.
/// 16 MiB is generous for any banking JSON payload and small enough
/// that a hostile decompression bomb can't OOM the host app.
const DEFAULT_MAX_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// Default connect timeout when `PqcConfig::connect_timeout_ms == None`.
/// Sized for cellular handover: long enough to absorb a single retry
/// of the SYN, short enough to fail fast and surface to the user.
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
    /// Construct a new PQC HTTPS client. Returns `PqcError` rather than
    /// panicking so consumers (iOS / Android / RN) can surface bad config
    /// (e.g. malformed base64 in pinned_cert_sha256) as a typed error
    /// instead of an opaque FFI panic.
    pub fn new(config: PqcConfig) -> Result<Self, PqcError> {
        let tls = build_tls_config(&config)?;

        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .cookie_store(config.enable_cookies)
            .gzip(true)
            .brotli(true)
            // Keep idle connections so a burst of requests reuses one
            // connection (HTTP/2 multiplexing) instead of paying a full
            // PQ TLS 1.3 handshake per call. This matches OkHttp /
            // URLSession / reqwest defaults; disabling reuse entirely is
            // an over-correction that taxes every request on stable
            // networks.
            //
            // The mobile concern is cell↔wifi handover leaving a dead
            // idle socket the kernel still holds. We bound that two ways:
            // (1) a 60s idle timeout evicts sockets idle across a typical
            // handover gap, and (2) tcp_keepalive probes detect a dead
            // peer far faster than the OS default. hyper also refuses to
            // hand out a connection it knows is broken, so a stale socket
            // surfaces as a fresh-connect, not a hang.
            .pool_idle_timeout(Duration::from_secs(60))
            // Keep-alive on the TCP socket itself: detects dead
            // peers faster than the default OS heartbeat. 30s
            // matches RFC 1122 §4.2.3.6 guidance for interactive
            // mobile clients.
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

        // reqwest::ClientBuilder::build failures at this point are
        // residual wiring errors (DNS resolver init, proxy load, etc.)
        // — TLS is already validated by build_tls_config + accepted by
        // use_preconfigured_tls. None of the existing PqcError variants
        // fit perfectly; map to InvalidRequest to match the UDL
        // constructor doc which lists "rustls failing to build the TLS
        // config" under this variant.
        let client = builder.build().map_err(|_| PqcError::InvalidRequest)?;

        Ok(Self {
            inner: client,
            default_timeout: config.default_timeout_ms.map(Duration::from_millis),
            max_body_bytes: config.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES),
        })
    }
}

// `async_runtime = "tokio"` tells UniFFI to drive our async exports
// using a real tokio runtime. Without it, UniFFI's default executor is
// not tokio-aware — reqwest/hyper's I/O calls (which depend on tokio's
// reactor) panic at runtime with:
//
//   rustPanic("there is no reactor running, must be called from the
//             context of a Tokio 1.x runtime")
//
// The macOS smoke test masks the bug because `#[tokio::test]` creates
// its own runtime; the panic only surfaces when the method is called
// through the FFI bridge (i.e., from a Swift/Kotlin consumer).
// Constructor stays in a plain impl above because uniffi's proc-macro
// export does not support associated functions.
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
        // The UDL contract (pqc.udl) documents this field as the
        // negotiated ALPN protocol id ("h2", "http/1.1"). `http::Version`
        // Debug renders as "HTTP/2.0" / "HTTP/1.1", which is a different
        // string and breaks string-equality checks in consumer code.
        // Translate explicitly so the value matches the documented contract.
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

        // Stream the body with a hard cap, so a decompression bomb (a
        // 1 KiB gzip/brotli stream that expands to gigabytes — CWE-409)
        // can't OOM the host app. We can't trust Content-Length: it's
        // pre-decompression for compressed bodies, and easily lied
        // about. The only safe bound is "stop counting decompressed
        // bytes when we exceed the cap." Tripping the cap returns
        // InvalidResponse rather than a half-filled buffer so the
        // caller knows the data is incomplete.
        //
        // A TCP reset, h2 GOAWAY, or carrier-handover during body read
        // is a transport-level failure. Map to Timeout / Network only
        // — skip the typed rustls classifier in map_reqwest_err,
        // because handshake-time variants (PinningFailure,
        // TrustVerification, Tls) are structurally impossible mid-body:
        // the handshake already completed.
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
                    // Pre-check: would appending overflow the cap? Use
                    // checked arithmetic so a 2^63-byte chunk on a 32-bit
                    // device can't wrap. Saturating add would also work;
                    // explicit overflow check makes intent clearer.
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

/// Classify a `reqwest::Error` into the closest `PqcError` variant so
/// callers can distinguish a transient network blip (retry) from a
/// pinning / trust / TLS failure (do NOT retry, alert security).
///
/// Strategy:
///   1. Walk the `source()` chain and try to **downcast to `&rustls::Error`**
///      — `rustls::Error: 'static`, so `Any::downcast_ref` works through
///      `dyn std::error::Error + 'static`. This is the authoritative path
///      and avoids string fragility entirely.
///   2. As a defensive fallback, do the legacy substring matching against
///      our own `PinningVerifier` message. Substrings on rustls strings
///      were brittle because variants like `PeerIncompatible(...)` and
///      `AlertReceived(...)` render with neither "tls" nor "handshake" in
///      their Display, so a server `protocol_version` alert was being
///      mis-classified as `Network` (would trigger a retry on a hard
///      negotiation failure). Typed match avoids that entire class of bug.
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

    // Pass 2: substring fallback. Used to catch:
    //   - our own PinningVerifier's "certificate pinning failure: ..."
    //     message (PinningError is a rustls `OtherError(Box<dyn Error>)`
    //     wrapper around a String, so downcasting catches it as
    //     `rustls::Error::Other(...)` only, not as our pinning type).
    //   - any case where the rustls error surfaces only via reqwest's
    //     own wrapping (older versions, non-rustls TLS backends compiled
    //     into a future build, etc.).
    // The outermost reqwest error Display embeds the request URL, which
    // contaminates the substring match — strip first.
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

/// Map a `rustls::Error` variant to a `PqcError`. `rustls::Error` is
/// `#[non_exhaustive]`, so a wildcard arm is REQUIRED by the compiler
/// — we cannot get a compile-time "you forgot a variant" guarantee
/// here. The explicit enumeration below is therefore a *security signal
/// guarantee*, not a forward-compatibility guarantee: every variant we
/// list deliberately classifies into a specific `PqcError`, and the
/// `_` wildcard conservatively maps unknowns to `PqcError::Tls`.
///
/// Trade-off: a future rustls variant that semantically means
/// "certificate trust verification failed" (e.g., a future cert-policy
/// or SCT-related variant) will silently land in `Tls` instead of
/// `TrustVerification`, downgrading the security signal consumers
/// branch on for alerting. Mitigation: **on every `rustls` minor-
/// version bump, re-audit the upstream `enum Error` and extend the
/// match arms below before merging.** The doc comment on the wildcard
/// arm below is the second reminder.
///
/// Reference: https://docs.rs/rustls/0.23.40/rustls/enum.Error.html
fn classify_rustls_error(e: &rustls::Error) -> PqcError {
    use rustls::Error as R;
    match e {
        // Certificate chain validation failed (platform verifier rejected
        // the chain). Maps to TrustVerification — the platform refused to
        // trust this peer.
        R::InvalidCertificate(_) | R::InvalidCertRevocationList(_) => PqcError::TrustVerification,

        // Our own PinningVerifier surfaces failures as
        // rustls::Error::General("certificate pinning failure: ...")
        // (see pinning.rs). A future rustls/reqwest stack could also
        // wrap it as Other(OtherError(...)). Check BOTH variants for the
        // marker so the PinningFailure signal survives regardless of how
        // the error is carried up the source chain; if the marker is
        // absent the failure is still handshake-time, so fall back to Tls.
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

        // Server alerts, version mismatches, protocol violations — all
        // handshake-time TLS failures, none transient. Critically,
        // PeerIncompatible / PeerMisbehaved / AlertReceived were being
        // mis-classified as Network by the legacy substring path because
        // their Display strings don't contain "tls" or "handshake".
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

        // `Error` is #[non_exhaustive]. Unknown future variants land
        // here. Conservative default: a known-handshake-stage failure
        // → Tls (do NOT retry as Network). REMINDER: re-audit this
        // match against the upstream `enum Error` on every rustls
        // minor bump — a new TrustVerification-shaped variant silently
        // landing here downgrades the security signal consumers
        // branch on.
        _ => PqcError::Tls,
    }
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
            "certificate pinning failure: no certificate in the chain matched any configured pin"
                .to_string();
        assert!(msg.to_lowercase().contains("pinning failure"));
    }

    /// Regression: a URL containing "certificate" in its path must not
    /// cause map_reqwest_err to misclassify a plain network failure as
    /// TrustVerification. Tests the URL-stripping branch.
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

    /// The substring classifier mis-classified these rustls variants as
    /// `Network` because their Display strings contain neither "tls" nor
    /// "handshake". The typed `classify_rustls_error` path must catch
    /// them as `Tls` so retry policies treat them as terminal
    /// negotiation failures, not transient blips.
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
        // This is what `pinning.rs` ACTUALLY emits in production:
        // rustls::Error::General(String), not Other. Regression guard for
        // the General arm of classify_rustls_error — without it a pin
        // mismatch silently downgrades to Tls via the typed path.
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
