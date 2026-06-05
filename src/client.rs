use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::redirect::Policy;
use tokio::sync::Semaphore;

use crate::config::{DnsResolver, PqcConfig, RedirectPolicy};
use crate::error::PqcError;
use crate::tls::build_tls_config;
use crate::types::{HttpMethod, HttpRequest};
use tokio::sync::OwnedSemaphorePermit;

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

/// Per-host concurrency gate. The map grows on first contact with a new
/// host and lives for the client's lifetime. The `Mutex` is only held to
/// look up or insert a per-host `Semaphore` — `acquire_owned` is awaited
/// outside the lock so no `.await` happens across the critical section.
struct PerHostInflight {
    cap: u32,
    map: Mutex<HashMap<String, Arc<Semaphore>>>,
}

/// The HTTPS client exposed to Kotlin / Swift via UniFFI.
///
/// Holds a single client with PQC TLS configured. Construct once per process
/// (it owns the connection pool); calling `request` is cheap.
#[derive(uniffi::Object)]
pub struct PqcHttpClient {
    inner: HttpBackend,
    default_timeout: Option<Duration>,
    /// Global in-flight cap (OkHttp `Dispatcher.maxRequests` parity).
    /// `None` when the consumer disabled the global gate.
    global_inflight: Option<Arc<Semaphore>>,
    /// Per-host in-flight cap (OkHttp `Dispatcher.maxRequestsPerHost`
    /// parity). `None` when the consumer disabled the per-host gate.
    per_host_inflight: Option<PerHostInflight>,
    // Held alongside the middleware's copy so clear_cache / cache_size_bytes
    // reach the same store. None when caching is off.
    #[cfg(feature = "cache")]
    cache_manager: Option<crate::cache::PqcStreamingCacheManager>,
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

        // Per-read idle timeout — reqwest resets the timer after every
        // successful read, so this kills a stalled stream without burning
        // the total request budget. Mirrors OkHttp's readTimeout.
        if let Some(t) = config.read_idle_timeout_ms {
            builder = builder.read_timeout(Duration::from_millis(t));
        }

        if let Some(ref ua) = config.user_agent {
            builder = builder.user_agent(ua.clone());
        }

        // DNS resolver — opt-in hickory for Happy Eyeballs (v4/v6 race).
        // System is the default and matches today's behavior; we only flip
        // hickory_dns on when explicitly requested, because it bypasses
        // Android Private DNS. The hickory-dns reqwest feature is always
        // compiled in (small cost) so the runtime toggle is real.
        if matches!(config.dns_resolver, Some(DnsResolver::Hickory)) {
            builder = builder.hickory_dns(true);
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

        let global_inflight = config
            .max_inflight_total
            .map(|n| Arc::new(Semaphore::new(n as usize)));
        let per_host_inflight = config.max_inflight_per_host.map(|n| PerHostInflight {
            cap: n,
            map: Mutex::new(HashMap::new()),
        });

        #[cfg(feature = "cache")]
        {
            // `enable_cache` builds the cache layer; a missing tier (e.g.
            // Android with no `cache_dir`) logs and falls back to no caching
            // rather than failing the constructor.
            let cache_manager = if config.enable_cache {
                let m = crate::cache::PqcStreamingCacheManager::new(&config);
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
                global_inflight,
                per_host_inflight,
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
                global_inflight,
                per_host_inflight,
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
    pub async fn request(&self, req: HttpRequest) -> Result<Arc<PqcResponse>, PqcError> {
        // Concurrency gates — acquired before cache lookup OR network send,
        // and held until this function returns (after the body is fully
        // consumed). Cache hits count too, matching OkHttp's Dispatcher,
        // because OkHttp counts Calls, not network sockets.
        //
        // Global first, then per-host: a request "in flight" includes time
        // queued waiting for the per-host gate. Acquisition is `.await` —
        // queued requests park, never spin.
        //
        // OwnedSemaphorePermit's Drop releases the slot. The bindings
        // are unused on purpose; the lifetimes are what matter.
        let _global_permit = match &self.global_inflight {
            Some(s) => Some(
                s.clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| PqcError::Network)?,
            ),
            None => None,
        };
        let _host_permit = match &self.per_host_inflight {
            Some(ph) => {
                // Host key matches OkHttp: URL host only (no port, no scheme).
                // A parse failure here would also fail in reqwest below, but
                // we surface it now so the permit isn't held while we discover
                // the URL is invalid.
                let host = reqwest::Url::parse(&req.url)
                    .map_err(|_| PqcError::InvalidRequest)?
                    .host_str()
                    .unwrap_or_default()
                    .to_owned();
                let sem = {
                    let mut m = ph.map.lock().expect("per-host inflight map poisoned");
                    m.entry(host)
                        .or_insert_with(|| Arc::new(Semaphore::new(ph.cap as usize)))
                        .clone()
                };
                Some(sem.acquire_owned().await.map_err(|_| PqcError::Network)?)
            }
            None => None,
        };

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

        // Return a PqcResponse handle. The body is NOT drained here —
        // consumers pull via async `read_chunk()` / `bytes()`. The
        // semaphore permits move INTO the response so they're held
        // until the response is fully consumed or dropped, matching
        // OkHttp's "in-flight until response.close()" lifecycle.
        Ok(Arc::new(PqcResponse {
            status,
            final_url,
            headers,
            negotiated_protocol,
            body: tokio::sync::Mutex::new(Some(resp)),
            _global_permit,
            _host_permit,
        }))
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

/// Streaming HTTP response handle. Returned by [`PqcHttpClient::request`]
/// as soon as the response head (status, headers) has been received —
/// the body is pulled on demand via [`PqcResponse::read_chunk`] or
/// drained whole via [`PqcResponse::bytes`].
///
/// Matches the streaming default of OkHttp's `ResponseBody` and
/// URLSession's `URLSession.bytes(for:)`. The buffered convenience
/// (`bytes()`) mirrors `body.bytes()` / `data(for:)`.
///
/// # Concurrency
///
/// `&self` methods can be called concurrently; an internal async mutex
/// serializes body reads. Only one reader at a time makes progress
/// (chunks have to come out in order), but headers/status are pre-
/// captured so those getters never block.
///
/// # Lifecycle
///
/// The connection (HTTP/2 stream or HTTP/1.1 socket) and the in-flight
/// semaphore permits acquired at request time both live inside this
/// object. Dropping the response without calling `bytes()` aborts the
/// body stream — same as OkHttp `Response.close()` and URLSession's
/// task-cancel semantics.
///
/// # Cancellation note
///
/// UniFFI 0.29 does not propagate foreign-runtime cancellation
/// (Swift `Task.cancel()`, Kotlin coroutine cancel) into Rust.
/// Consumers who want to abort an in-flight body read must call
/// [`PqcResponse::cancel`] explicitly. See `docs/ios.md` and
/// `docs/android.md`.
#[derive(uniffi::Object)]
pub struct PqcResponse {
    // (Debug impl is manual below — the body field's mutex contents
    // include reqwest::Response which isn't Debug.)
    status: u16,
    headers: HashMap<String, Vec<String>>,
    final_url: String,
    negotiated_protocol: String,
    /// The live response. Wrapped in `tokio::sync::Mutex` so multiple
    /// `&self` callers can coordinate access (UniFFI Object methods
    /// must take `&self`). `Option` so consume-style operations
    /// (`bytes`, `cancel`) can `take()` it.
    body: tokio::sync::Mutex<Option<reqwest::Response>>,
    /// In-flight semaphore permits acquired at request time. Held
    /// here so they're released by `Drop` exactly when the response
    /// is no longer in flight from the consumer's perspective.
    /// Underscore-prefixed because we never read them — the lifetime
    /// is what matters.
    _global_permit: Option<OwnedSemaphorePermit>,
    _host_permit: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for PqcResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PqcResponse")
            .field("status", &self.status)
            .field("final_url", &self.final_url)
            .field("negotiated_protocol", &self.negotiated_protocol)
            .field("headers_len", &self.headers.len())
            .field(
                "body_consumed",
                &self.body.try_lock().map(|g| g.is_none()).ok(),
            )
            .finish()
    }
}

#[uniffi::export]
impl PqcResponse {
    /// HTTP status code (e.g. 200, 404, 503).
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Response headers. Multi-valued headers (`Set-Cookie`, `Vary`,
    /// `Link`) appear with all values intact — never collapsed.
    pub fn headers(&self) -> HashMap<String, Vec<String>> {
        self.headers.clone()
    }

    /// The URL the body actually came from, after any followed
    /// redirects. Equals the request URL when no redirect occurred.
    /// Lets callers detect a redirect they refused (see
    /// `RedirectPolicy`) — mirrors OkHttp `Response.request().url()`
    /// and `URLResponse.url`.
    pub fn final_url(&self) -> String {
        self.final_url.clone()
    }

    /// The negotiated ALPN protocol (`"h2"`, `"http/1.1"`, etc.).
    /// Useful for logging / observability; the consumer never has to
    /// branch on this.
    pub fn negotiated_protocol(&self) -> String {
        self.negotiated_protocol.clone()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl PqcResponse {
    /// Pull the next chunk of the body. Returns `Ok(None)` when the
    /// body has been fully consumed (EOF). Chunks arrive in network
    /// order and are not bounded in size — typically 16 KB to 64 KB
    /// for HTTP/2, larger for HTTP/1.1.
    ///
    /// Calling `read_chunk` after `bytes()` or `cancel()` always
    /// returns `Ok(None)` — both consume the body.
    pub async fn read_chunk(&self) -> Result<Option<Vec<u8>>, PqcError> {
        let mut guard = self.body.lock().await;
        let resp = match guard.as_mut() {
            Some(r) => r,
            None => return Ok(None),
        };
        match resp.chunk().await {
            Ok(Some(b)) => Ok(Some(b.to_vec())),
            Ok(None) => {
                // EOF — drop the live response so the connection
                // returns to the pool promptly instead of waiting for
                // the PqcResponse to be dropped.
                *guard = None;
                Ok(None)
            }
            Err(e) => {
                // Mid-body errors are transport-level (handshake
                // already done), so map to Timeout/Network. The rustls
                // classifier doesn't apply mid-body.
                let mapped = if e.is_timeout() {
                    PqcError::Timeout
                } else {
                    PqcError::Network
                };
                *guard = None;
                Err(mapped)
            }
        }
    }

    /// Drain the entire body to a `Vec<u8>` (the buffered convenience,
    /// mirroring OkHttp `body.bytes()` and URLSession `data(for:)`).
    /// Internally consumes the response — subsequent `read_chunk`
    /// calls return `Ok(None)`.
    pub async fn bytes(&self) -> Result<Vec<u8>, PqcError> {
        let mut guard = self.body.lock().await;
        let resp = match guard.take() {
            Some(r) => r,
            None => return Ok(Vec::new()),
        };
        match resp.bytes().await {
            Ok(b) => Ok(b.to_vec()),
            Err(e) => {
                if e.is_timeout() {
                    Err(PqcError::Timeout)
                } else {
                    Err(PqcError::Network)
                }
            }
        }
    }

    /// Abort the body stream and release the underlying connection.
    /// Idempotent — a second call is a no-op. Required for consumers
    /// who want to abort mid-download because UniFFI 0.29 doesn't
    /// propagate foreign cancellation into Rust.
    pub async fn cancel(&self) {
        let mut guard = self.body.lock().await;
        *guard = None; // Dropping the reqwest::Response aborts the stream.
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
