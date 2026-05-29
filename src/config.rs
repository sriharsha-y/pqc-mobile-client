/// Configuration handed to `PqcHttpClient::new`. Defaults are tuned for
/// mobile and safe to set from Swift/Kotlin without reading the docs.
/// All `*_ms` fields are milliseconds.
#[derive(Debug, Clone, uniffi::Record)]
pub struct PqcConfig {
    /// Base64 SHA-256 of a DER SPKI (standard or URL-safe) the client will
    /// accept. Matches if ANY cert in the chain — leaf or intermediate —
    /// has a matching SPKI hash (the leaf must still parse); empty disables
    /// pinning. Pin the issuing intermediate CA, keep >= 2 pins, never pin a
    /// public root. See `src/pinning.rs`.
    pub pinned_cert_sha256: Vec<String>,

    /// Advertise X25519MLKEM768 (IANA 0x11EC) as the preferred KEX group
    /// (default true). The ClientHello also carries classical groups, so a
    /// peer that rejects the hybrid falls back to classical — this is a
    /// preference, not enforcement. Set false only for A/B comparison.
    #[uniffi(default = true)]
    pub enable_post_quantum: bool,

    // ----- Timeouts -----
    /// Total request budget (handshake + headers + body); on expiry the
    /// request aborts with `PqcError::Timeout`. `None` means no total cap —
    /// discouraged for mobile.
    pub default_timeout_ms: Option<u64>,

    /// TCP-connect / TLS-handshake budget, capped separately from
    /// `default_timeout_ms` so a stalled connect (e.g. cellular handover)
    /// fails fast instead of burning the whole request budget. `None` = 10s.
    pub connect_timeout_ms: Option<u64>,

    // ----- Body protection -----
    /// Hard ceiling on a response body (post-decompression); exceeding it
    /// trips `PqcError::InvalidResponse`. Guards against decompression bombs
    /// (CWE-409) — gzip/brotli are on, so without a cap a tiny stream can
    /// expand to GBs and OOM the app. `None` defaults to 16 MiB.
    pub max_body_bytes: Option<u64>,

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

    // ----- Redirects -----
    /// How to handle 3xx. Default `SameOriginOnly` — cross-origin redirects
    /// are refused so a redirect can't silently downgrade to an un-pinned
    /// host.
    pub redirect_policy: RedirectPolicy,

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
