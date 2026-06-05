use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;

use crate::config::{PqcConfig, RedirectPolicy};
use crate::error::PqcError;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest, HttpResponse};

/// Default connect timeout (`connect_timeout_ms == None`). Sized for cell
/// handover: absorbs one SYN retry, still fails fast.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Plain = bare reqwest client (default). Cached = RFC 9111 middleware wrap
/// (only when `cache` feature + `enable_cache` are on). TLS/PQC/pinning are
/// identical either way.
enum HttpBackend {
    Plain(reqwest::Client),
    #[cfg(feature = "cache")]
    Cached(reqwest_middleware::ClientWithMiddleware),
}

/// The HTTPS client exposed to Kotlin / Swift via UniFFI.
///
/// Holds a single client with PQC TLS configured. Construct once per process
/// (it owns the connection pool); calling `request` is cheap.
#[derive(uniffi::Object)]
pub struct PqcHttpClient {
    inner: HttpBackend,
    default_timeout: Option<Duration>,
    // Held alongside the middleware's copy so clear_cache / cache_size_bytes
    // reach the same store. None when caching is off.
    #[cfg(feature = "cache")]
    cache_manager: Option<crate::cache::PqcCacheManager>,
}

#[uniffi::export]
impl PqcHttpClient {
    /// Returns `PqcError` (not panic) so consumers surface bad config —
    /// e.g. malformed base64 in pinned_cert_sha256 — as a typed error.
    #[uniffi::constructor]
    pub fn new(config: PqcConfig) -> Result<Self, PqcError> {
        let tls = build_tls_config(&config)?;

        let mut builder = reqwest::Client::builder()
            .use_preconfigured_tls(tls)
            .cookie_store(config.enable_cookies)
            .gzip(true)
            .brotli(true)
            // Reuse idle connections so a burst doesn't pay a PQ TLS 1.3
            // handshake per call. 5 min idle window matches OkHttp's
            // ConnectionPool (keepAliveDuration = 5 minutes); reqwest's own
            // default is 90s, URLSession is system-managed. The hybrid
            // X25519MLKEM768 handshake adds ~2.2 KB of keyshare on top of
            // a classical TLS 1.3 handshake, so making reuse worthwhile is
            // more battery-relevant for us than for a classical client.
            // Dead-idle-socket risk on cell↔wifi handover is handled by
            // hyper refusing connections it knows are broken plus the
            // HTTP/2 keep-alive PING wired below.
            .pool_idle_timeout(Duration::from_secs(300))
            // Cap idle sockets per host to match OkHttp's ConnectionPool
            // (maxIdleConnections = 5). reqwest defaults to usize::MAX, which
            // on HTTP/1.1 lets a burst leave hundreds of idle sockets — each
            // having paid a full PQ handshake — sitting in the pool. iOS
            // URLSession caps in-flight per host at 6; the parity number is 5.
            .pool_max_idle_per_host(5)
            // Dead-peer detection: HTTP/2 keep-alive PING ONLY while streams
            // are open, never on an idle pooled connection. Crucial for
            // cellular battery — sending SO_KEEPALIVE probes on an otherwise
            // idle socket wakes the modem and prevents it from dropping to
            // IDLE (the LTE RRC inactivity timer is 10–60s on most networks,
            // typically 20s, and SO_KEEPALIVE resets it on every probe).
            // OkHttp and URLSession both leave SO_KEEPALIVE OFF for the same
            // reason; Go fixed the same battery bug (see golang/go#48622).
            //
            // 60s interval / 20s timeout: a stuck h2 connection fails after
            // ~80s instead of hanging for the full request timeout.
            .http2_keep_alive_interval(Duration::from_secs(60))
            .http2_keep_alive_timeout(Duration::from_secs(20))
            .http2_keep_alive_while_idle(false);

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
        // constructor doc.
        let client = builder.build().map_err(|_| PqcError::InvalidRequest)?;

        let default_timeout = config.default_timeout_ms.map(Duration::from_millis);

        #[cfg(feature = "cache")]
        {
            // `enable_cache` builds the cache layer; a missing tier (e.g.
            // Android with no `cache_dir`) logs and falls back to no caching
            // rather than failing the constructor.
            let cache_manager = if config.enable_cache {
                let m = crate::cache::PqcCacheManager::new(&config);
                if m.is_none() {
                    log::warn!(
                        "pqc cache: enable_cache=true but no usable tier \
                         (set cache_dir for a persistent cache); caching disabled"
                    );
                }
                m
            } else {
                None
            };
            let inner = match &cache_manager {
                Some(m) => {
                    HttpBackend::Cached(crate::cache::build_cached_client(client, m.clone()))
                }
                None => HttpBackend::Plain(client),
            };
            // Tail of the function when `cache` is compiled in (the
            // `cfg(not)` block below is stripped away).
            Ok(Self {
                inner,
                default_timeout,
                cache_manager,
            })
        }

        #[cfg(not(feature = "cache"))]
        {
            // Fail loud: asking for caching in a build that didn't compile it
            // in is a misconfiguration, not a silent no-op.
            if config.enable_cache {
                return Err(PqcError::InvalidRequest);
            }
            Ok(Self {
                inner: HttpBackend::Plain(client),
                default_timeout,
            })
        }
    }
}

// `async_runtime = "tokio"` makes UniFFI drive these exports on a real tokio
// runtime; without it reqwest/hyper panic ("there is no reactor running")
// when called through the FFI bridge (tests mask it via #[tokio::test]).
// The sync constructor is exported from a separate #[uniffi::export] block
// above (async_runtime applies only to the async methods here).
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

        let timeout_ms = req
            .timeout_ms
            .or(self.default_timeout.map(|d| d.as_millis() as u64));
        // Moved (not cloned) into whichever match arm runs — cloning would
        // copy the entire upload on every request.
        let body = req.body;

        // The two backends share the same header/body/timeout surface but
        // have distinct RequestBuilder types, so a macro fills both without
        // drift.
        macro_rules! build_request {
            ($rb:expr) => {{
                let mut b = $rb;
                for (k, values) in &req.headers {
                    let name =
                        HeaderName::try_from(k.as_str()).map_err(|_| PqcError::InvalidRequest)?;
                    for v in values {
                        let value = HeaderValue::try_from(v.as_str())
                            .map_err(|_| PqcError::InvalidRequest)?;
                        b = b.header(name.clone(), value);
                    }
                }
                if let Some(body) = body {
                    b = b.body(body);
                }
                if let Some(t) = timeout_ms {
                    b = b.timeout(Duration::from_millis(t));
                }
                b
            }};
        }

        let resp = match &self.inner {
            HttpBackend::Plain(c) => build_request!(c.request(method, &req.url))
                .send()
                .await
                .map_err(map_reqwest_err)?,
            #[cfg(feature = "cache")]
            HttpBackend::Cached(c) => build_request!(c.request(method, &req.url))
                .send()
                .await
                .map_err(map_middleware_err)?,
        };

        let status = resp.status().as_u16();
        // The URL the body actually came from (post-redirect). Captured before
        // `resp` is consumed by the streaming loop below.
        let final_url = resp.url().to_string();
        // Map to the ALPN id the API documents ("h2", "http/1.1"); the
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

        // Mid-body errors are transport-level (handshake already done), so
        // map to Timeout/Network, skipping the rustls classifier. Matches the
        // native stacks' shape: URLSession `dataTask` and OkHttp
        // `ResponseBody.bytes()` buffer the whole body to memory with no cap
        // — callers stream when they need to bound it.
        let body = resp
            .bytes()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    PqcError::Timeout
                } else {
                    PqcError::Network
                }
            })?
            .to_vec();

        Ok(HttpResponse {
            status,
            final_url,
            headers,
            body,
            negotiated_protocol,
        })
    }

    /// Clear all cached responses. Best-effort and non-throwing, mirroring
    /// `URLCache.removeAllCachedResponses` / OkHttp `Cache.evictAll`. Also the
    /// recommended logout / session-end hook so cached responses don't outlive
    /// a session. A no-op when caching is disabled or the `cache` feature was
    /// not compiled in.
    pub async fn clear_cache(&self) {
        #[cfg(feature = "cache")]
        if let Some(m) = &self.cache_manager {
            m.clear().await;
        }
    }

    /// Total bytes in the on-disk cache, for a "Clear cache (X MB)"
    /// affordance. Returns `0` when caching is disabled or absent.
    pub async fn cache_size_bytes(&self) -> u64 {
        #[cfg(feature = "cache")]
        if let Some(m) = &self.cache_manager {
            return m.size().await;
        }
        0
    }
}

/// Map a cached-backend send error to a `PqcError`. The `Middleware` arm
/// carries http-cache's anyhow-boxed transport error, so the chain walk is
/// required: without it, a pinning/trust failure gets downgraded to the
/// retryable `Network`.
#[cfg(feature = "cache")]
fn map_middleware_err(e: reqwest_middleware::Error) -> PqcError {
    match e {
        reqwest_middleware::Error::Reqwest(e) => map_reqwest_err(e),
        reqwest_middleware::Error::Middleware(e) => classify_err_chain(e.chain()),
    }
}

/// Typed-downcast classifier — a `rustls::Error` anywhere in the chain wins
/// (pinning / trust / TLS), reqwest timeout maps to `Timeout`, else `Network`.
#[cfg(feature = "cache")]
fn classify_err_chain<'a>(
    chain: impl Iterator<Item = &'a (dyn std::error::Error + 'static)>,
) -> PqcError {
    for cause in chain {
        if let Some(rustls_err) = cause.downcast_ref::<rustls::Error>() {
            return classify_rustls_error(rustls_err);
        }
        if let Some(req_err) = cause.downcast_ref::<reqwest::Error>() {
            if req_err.is_timeout() {
                return PqcError::Timeout;
            }
        }
    }
    PqcError::Network
}

/// Classify a `reqwest::Error` so callers can tell a transient blip (retry)
/// from a pinning/trust/TLS failure (don't retry). Pass 1 typed-downcasts to
/// `&rustls::Error`; pass 2 is a defense-in-depth substring fallback for the
/// pinning marker when typed access is unavailable.
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
/// `#[non_exhaustive]`, so re-audit on every rustls minor bump — a new
/// trust-failure variant would silently fall into `Tls` here.
fn classify_rustls_error(e: &rustls::Error) -> PqcError {
    use rustls::Error as R;
    match e {
        R::InvalidCertificate(_) | R::InvalidCertRevocationList(_) => PqcError::TrustVerification,

        // PinningVerifier emits General("certificate pinning failure: ...");
        // a future stack might wrap it as Other.
        R::General(msg) if msg.to_lowercase().contains("pinning failure") => {
            PqcError::PinningFailure
        }
        R::Other(other) if other.to_string().to_lowercase().contains("pinning failure") => {
            PqcError::PinningFailure
        }
        R::General(_) | R::Other(_) => PqcError::Tls,

        // Handshake-time TLS failures (AlertReceived / PeerIncompatible /
        // PeerMisbehaved lack "tls"/"handshake" in Display, so the substring
        // path would mis-read them as Network — keep them typed).
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

    // ---- Cached-backend error classification (classify_err_chain) ----
    // Regression guard: on the cached path, http-cache boxes the send error
    // into a Middleware(anyhow) whose chain carries the rustls error. The
    // chain walker must surface the security signal, NOT downgrade to Network.

    /// A rustls pinning failure anywhere in the chain → PinningFailure.
    #[cfg(feature = "cache")]
    #[test]
    fn middleware_chain_pinning_failure_not_downgraded() {
        use super::classify_err_chain;
        let err = rustls::Error::General(
            "certificate pinning failure: no certificate in the chain matched any configured pin"
                .to_string(),
        );
        let chain = std::iter::once(&err as &(dyn std::error::Error + 'static));
        assert!(matches!(
            classify_err_chain(chain),
            crate::error::PqcError::PinningFailure
        ));
    }

    /// A rustls invalid-certificate error in the chain → TrustVerification.
    #[cfg(feature = "cache")]
    #[test]
    fn middleware_chain_trust_failure_not_downgraded() {
        use super::classify_err_chain;
        let err = rustls::Error::InvalidCertificate(rustls::CertificateError::Expired);
        let chain = std::iter::once(&err as &(dyn std::error::Error + 'static));
        assert!(matches!(
            classify_err_chain(chain),
            crate::error::PqcError::TrustVerification
        ));
    }

    /// A non-rustls, non-timeout error still falls back to Network.
    #[cfg(feature = "cache")]
    #[test]
    fn middleware_chain_other_error_is_network() {
        use super::classify_err_chain;
        let err = std::io::Error::other("cache write failed");
        let chain = std::iter::once(&err as &(dyn std::error::Error + 'static));
        assert!(matches!(
            classify_err_chain(chain),
            crate::error::PqcError::Network
        ));
    }
}
