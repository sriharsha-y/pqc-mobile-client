/// One pinned domain and the SPKI hashes accepted for it. Pinning is scoped
/// per host: a connection is pin-checked only against the entries whose `host`
/// matches its SNI hostname; a host with no matching entry is left to the
/// platform verifier alone (so pinning one host never breaks requests to an
/// unpinned host). Mirrors Apple `NSPinnedDomains` / Android
/// `NetworkSecurityConfig` `<domain>`.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CertPin {
    /// Hostname to pin, e.g. `"api.example.com"`. Matched ASCII
    /// case-insensitively against the connection's SNI. IP-literal SNI is
    /// never matched — pinning is by hostname only.
    pub host: String,

    /// When true, also pins every subdomain of `host` (`a.host`, `b.a.host`,
    /// …) but NOT a bare different host. Mirrors `NSIncludesSubdomains` /
    /// Android `includeSubdomains`.
    #[uniffi(default = false)]
    pub include_subdomains: bool,

    /// Base64 SHA-256 of a DER SPKI (standard or URL-safe) accepted for this
    /// host. A connection matches if ANY cert in its chain — leaf or
    /// intermediate — carries one of these hashes (the leaf must still parse).
    /// See `src/pinning.rs` for pin-selection guidance (intermediate CA, >= 2
    /// pins, never a public root).
    pub spki_sha256: Vec<String>,

    /// Optional pin-set expiration as `"YYYY-MM-DD"` (00:00 UTC). On or after
    /// that instant the host's pins are treated as absent and it falls back to
    /// the platform verifier alone (**fail-open**) — mirroring Android
    /// `<pin-set expiration>` / TrustKit `kTSKExpirationDate`, so an app that
    /// stops receiving updates isn't bricked when its pinned key rotates.
    /// `None` (default) = never expires. A malformed date fails
    /// `PqcHttpClient::new` with `InvalidRequest`.
    ///
    /// Trade-off: once expired, this host no longer rejects a user-installed-CA
    /// / MITM cert. Set the date accordingly.
    #[uniffi(default = None)]
    pub expiration: Option<String>,
}

/// Configuration handed to `PqcHttpClient::new`. Defaults are tuned for
/// mobile and safe to set from Swift/Kotlin without reading the docs.
/// All `*_ms` fields are milliseconds.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PqcConfig {
    /// Per-host SPKI pin set. Empty disables pinning entirely. Each entry
    /// scopes its pins to a host (see `CertPin`); hosts with no matching
    /// entry are not pinned. See `src/pinning.rs`.
    pub pinned_domains: Vec<CertPin>,

    // ----- Timeouts -----
    /// Total request budget (handshake + headers + body); on expiry the
    /// request aborts with `PqcError::Timeout`. `None` means no total cap —
    /// discouraged for mobile.
    pub default_timeout_ms: Option<u64>,

    /// TCP-connect / TLS-handshake budget, capped separately from
    /// `default_timeout_ms` so a stalled connect (e.g. cellular handover)
    /// fails fast instead of burning the whole request budget. `None` = 10s.
    pub connect_timeout_ms: Option<u64>,

    /// Idle timeout between body-bytes read operations. The timer resets
    /// after every successful chunk read, so a healthy slow download is
    /// not killed by it; only a stalled server (TCP open but no bytes
    /// flowing) is. Mirrors OkHttp's `readTimeout`.
    ///
    /// `None` (default) leaves this off — only `default_timeout_ms` (the
    /// total request budget) applies. Set this when downloading large
    /// bodies where you'd rather kill a stuck stream within seconds than
    /// wait for the total budget to expire (recommended values: 10–30s for
    /// APIs, 60s+ for large file downloads).
    #[uniffi(default = None)]
    pub read_idle_timeout_ms: Option<u64>,

    // ----- Cookies -----
    /// Off by default: no cookie jar, so callers round-trip
    /// `Set-Cookie`/`Cookie` themselves. Auto-attaching cookies across
    /// endpoints is a session-leak vector — enable only when needed.
    pub enable_cookies: bool,

    // ----- User-Agent -----
    /// Sent verbatim as `User-Agent`. `None` uses reqwest's default, which
    /// many WAFs (Akamai Bot Manager, bank UA allowlists) reject — set your
    /// app's identifier.
    pub user_agent: Option<String>,

    // ----- DNS -----
    /// Which DNS resolver to use. `None` (default) selects `System` —
    /// libc `getaddrinfo` driven on tokio's blocking pool, which on
    /// Android honors user-configured Private DNS (DNS-over-TLS) and on
    /// iOS honors the system resolver chain.
    ///
    /// Set to `Some(Hickory)` to use the bundled hickory-dns async
    /// resolver, which enables RFC 8305 Happy Eyeballs (concurrent
    /// v4/v6 connection racing — meaningfully faster on dual-stack
    /// networks where one family is broken). The trade-off: hickory
    /// bypasses Android's Private DNS setting, so consumers whose users
    /// depend on DoT for privacy/policy should leave this at `None`.
    #[uniffi(default = None)]
    pub dns_resolver: Option<DnsResolver>,

    // ----- Proxy -----
    /// Optional proxy all requests route through, e.g. `"http://192.168.1.5:8888"`.
    /// For **debugging**: the Rust client does its own TLS and bypasses the OS
    /// network layer, so proxies (Charles/Burp/Proxyman) can't observe it —
    /// pointing them here lets them capture traffic.
    ///
    /// To MITM HTTPS the proxy CA must be OS-trusted (iOS: install + enable its
    /// root profile; Android: a debug `network_security_config`) AND pinning off
    /// (`pinned_domains` empty). Embedded credentials (`http://user:pass@host`)
    /// are honored; reqwest coerces a bare `host:port` to `http://`, and only
    /// unparseable values fail `PqcHttpClient::new` with `InvalidRequest`.
    ///
    /// `None` (default) adds no proxy, but reqwest still honors `HTTP(S)_PROXY`
    /// env vars if set. Leave `None` in production.
    #[uniffi(default = None)]
    pub proxy_url: Option<String>,

    // ----- Redirects -----
    /// How to handle 3xx. Default `SameOriginOnly` — cross-origin redirects
    /// are refused so a redirect can't silently downgrade to an un-pinned
    /// host.
    ///
    /// "Refused" here follows reqwest semantics: the redirect is **not
    /// followed**, and the 3xx response itself (with its `Location` header) is
    /// returned to the caller — it is *not* turned into an error. Callers that
    /// treat any `status < 400` as success should therefore check for 3xx, or
    /// read `final_url` on the response to confirm where the body came from.
    pub redirect_policy: RedirectPolicy,

    // ----- Concurrency -----
    /// Maximum concurrent in-flight requests across all hosts. Acquired
    /// before cache lookup and network send, so cache hits also count
    /// against the budget — matches OkHttp's `Dispatcher.maxRequests`.
    /// Default 64 mirrors OkHttp; `Some(n)` enforces n; `None` disables the
    /// global gate entirely (use only when a consumer needs unbounded
    /// concurrency, e.g. server-side or tunnelled use).
    #[uniffi(default = Some(64))]
    pub max_inflight_total: Option<u32>,

    /// Maximum concurrent in-flight requests per host, keyed by URL hostname
    /// (no port, no scheme). Default 5 mirrors OkHttp's
    /// `Dispatcher.maxRequestsPerHost`. URLSession's analogous cap is 6
    /// in-flight per host; we pick the lower OkHttp value. `None` disables
    /// the per-host gate.
    ///
    /// Once a host is seen for the first time, its semaphore lives for the
    /// lifetime of the client (one entry per unique host). For a typical
    /// mobile app this is bounded by the number of distinct API hosts the
    /// app talks to (usually under 100); the memory cost is negligible.
    #[uniffi(default = Some(5))]
    pub max_inflight_per_host: Option<u32>,

    // ----- Caching -----
    /// Opt-in RFC 9111 response cache (default false). When enabled it mirrors
    /// the platform HTTP caches (Android OkHttp `Cache`, iOS `URLCache`):
    /// cacheability is decided by request method + response status + cache
    /// headers (`Cache-Control`, `ETag`, `Last-Modified`, `Vary`, …), never by
    /// file type / `Content-Type`. A private cache (`shared = false`), so it
    /// honors `no-store`/`no-cache` but — like the native private caches —
    /// will cache responses to `Authorization`-bearing requests when their
    /// headers permit; suppress those by having the server send `no-store`.
    ///
    /// Only effective in builds compiled with the `cache` cargo feature; in a
    /// feature-less build this is a no-op (and `clear_cache`/`cache_size_bytes`
    /// are inert). See `src/cache.rs`.
    #[uniffi(default = false)]
    pub enable_cache: bool,

    /// Directory for the persistent on-disk cache tier (present on both
    /// platforms, matching OkHttp's `Cache` directory and `URLCache`'s disk
    /// store). Pass an app-writable path — Android `context.cacheDir`, iOS the
    /// `.cachesDirectory`. `None` disables the disk tier; the cache then lives
    /// only in the in-memory tier where one exists (iOS), or is effectively
    /// disabled (Android). Ignored when `enable_cache` is false.
    #[uniffi(default = None)]
    pub cache_dir: Option<String>,

    /// Hard ceiling on the on-disk cache in bytes. When exceeded, the oldest
    /// entries are evicted to stay under it (cf. OkHttp's `maxSize`). `None`
    /// defaults to 20 MiB, matching a typical `URLCache` disk capacity.
    #[uniffi(default = None)]
    pub max_cache_bytes: Option<u64>,

    /// Hard ceiling on the in-process LRU memory cache tier, in bytes.
    /// `None` defaults to 4 MiB on both platforms (matching `URLCache`'s
    /// historical memory capacity). `Some(0)` opts out of the memory tier
    /// entirely — Android consumers who want OkHttp-style disk-only
    /// behavior set this to `Some(0)`.
    ///
    /// Native baseline note: OkHttp's bundled `Cache` is disk-only (the
    /// `Cache` class is `final` and not extensible), so OkHttp users get
    /// no HTTP memory cache out of the box. URLCache on iOS does have a
    /// memory tier. We expose the same tier on both platforms because
    /// modern Android ART has dynamic heaps and the historical Dalvik
    /// caps that drove OkHttp's choice no longer apply.
    #[uniffi(default = None)]
    pub max_memory_cache_bytes: Option<u64>,
}

/// What the client does on a 3xx. The reqwest default (10 unbounded
/// redirects) is too permissive for a security-sensitive client. Variants
/// are struct-style (the `{}`) to preserve the generated binding shape.
/// `NoRedirects` (not `None`) avoids colliding with `Option::None` in matches.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum RedirectPolicy {
    NoRedirects {},
    SameOriginOnly {},
    Limited { max: u8 },
}

/// DNS resolver selection. See `PqcConfig::dns_resolver` for the full
/// trade-off (Happy Eyeballs vs. Android Private DNS interaction).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum DnsResolver {
    /// libc `getaddrinfo` (synchronous, on tokio's blocking pool).
    /// Honors Android Private DNS / DoT and the iOS system resolver chain.
    System,
    /// hickory-resolver (async, RFC 8305 Happy Eyeballs). Bypasses
    /// Android Private DNS.
    Hickory,
}

/// Compile-time drift detector for `PqcConfig` field count. Fires on
/// `cargo check` / `cargo build` — not gated to tests — so the failure
/// shows up in the cheapest local loop, not just CI.
///
/// The function is never called. `#[allow(dead_code)]` silences the
/// unused warning; the function still type-checks, which is all we
/// need.
#[allow(dead_code)]
fn pqc_config_field_destructure_check(cfg: PqcConfig) {
    // If this destructure fails with `pattern does not mention field X`,
    // a field was added to PqcConfig. FIX in this order:
    //   1. android/src/main/kotlin/io/github/sriharsha_y/pqc/PqcConfigDefaults.kt
    //   2. Sources/PqcCore/PqcConfig+Defaults.swift
    //   3. add the new field below.
    let PqcConfig {
        pinned_domains: _,
        default_timeout_ms: _,
        connect_timeout_ms: _,
        read_idle_timeout_ms: _,
        enable_cookies: _,
        user_agent: _,
        dns_resolver: _,
        proxy_url: _,
        redirect_policy: _,
        max_inflight_total: _,
        max_inflight_per_host: _,
        enable_cache: _,
        cache_dir: _,
        max_cache_bytes: _,
        max_memory_cache_bytes: _,
    } = cfg;
}
