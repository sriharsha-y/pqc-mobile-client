/// Configuration handed to `PqcHttpClient::new`. Every field is meant
/// to be reasonable to set from Swift/Kotlin without consulting the
/// docs — defaults are tuned for the banking-on-mobile target.
///
/// All `*_ms` fields are milliseconds. `None` means "use a sensible
/// builder default", NOT "infinite" — there are no infinite timeouts
/// in this client by design.
#[derive(Debug, Clone)]
pub struct PqcConfig {
    /// SHA-256 of the DER-encoded SPKI for each leaf certificate that
    /// the client is willing to accept, base64-encoded (standard or
    /// URL-safe alphabet). Empty list disables pinning. See
    /// `src/pinning.rs` for the matching semantics.
    pub pinned_cert_sha256: Vec<String>,

    /// Advertise X25519MLKEM768 (IANA 0x11EC) as the most-preferred
    /// key-exchange group. Servers that don't accept it fall back to
    /// classical (X25519 / secp256r1 / secp384r1) — the client still
    /// works, the handshake just isn't post-quantum. Toggle off only
    /// for A/B comparison or to debug a PQ-specific server bug.
    pub enable_post_quantum: bool,

    // ----- Timeouts -----
    /// Total request budget (handshake + headers + body). On expiry
    /// the request is aborted with `PqcError::Timeout`. `None` means
    /// no total cap — discouraged for mobile.
    pub default_timeout_ms: Option<u64>,

    /// TCP-connect / TLS-handshake budget. Separated from
    /// `default_timeout_ms` because on cellular handover the
    /// connect phase can hang for the entire total budget; capping
    /// it independently lets the client fail fast and retry without
    /// burning the full timeout window. `None` defaults to 10s.
    pub connect_timeout_ms: Option<u64>,

    // ----- Body protection -----
    /// Hard ceiling on a response body's size, in bytes (post-
    /// decompression). Bodies exceeding this trip
    /// `PqcError::InvalidResponse` rather than allocating GBs.
    ///
    /// gzip + brotli are enabled by default on the underlying reqwest
    /// builder; without this cap a 1 KiB encoded stream can expand to
    /// GBs and OOM-kill the host app (decompression-bomb class,
    /// CWE-409). `None` defaults to 16 MiB which is generous for any
    /// banking JSON payload.
    pub max_body_bytes: Option<u64>,

    // ----- Cookies -----
    /// Off by default. When false, the client carries no cookie jar
    /// at all — callers must round-trip `Set-Cookie` / `Cookie`
    /// header values explicitly via `HttpRequest.headers` and
    /// `HttpResponse.headers`. Banking clients typically want this:
    /// auto-attaching cookies across endpoints is a session-leak
    /// vector. Enable only when you have a reason.
    pub enable_cookies: bool,

    // ----- User-Agent -----
    /// Sent verbatim as the `User-Agent` request header. `None` lets
    /// reqwest send its default (`reqwest/0.12.x`), which Akamai Bot
    /// Manager and many bank WAFs reject — banking partners commonly
    /// enforce a UA allowlist. Set this to your app's identifier.
    pub user_agent: Option<String>,

    // ----- Redirects -----
    /// How to handle HTTP 3xx responses. See `RedirectPolicy`
    /// variants for semantics. Default is
    /// `RedirectPolicy::SameOriginOnly` — cross-origin redirects are
    /// refused so a redirect to an un-pinned host can never silently
    /// downgrade the TLS guarantees we just negotiated.
    pub redirect_policy: RedirectPolicy,
}

/// What the client does on a 3xx response. Banking flows often want
/// "no redirects at all" or "only within the same origin"; the
/// reqwest default of up-to-10 unbounded redirects is too permissive
/// for a security-sensitive HTTPS client.
///
/// Variants are struct-style (even unit ones) because UDL `[Enum]
/// interface` enforces that shape, and PqcConfig — which references
/// this — is declared in UDL. The shape is purely a UDL syntactic
/// constraint, not a design choice.
///
/// `NoRedirects` rather than `None` to avoid Rust naming collision
/// with `Option::None` in match arms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectPolicy {
    NoRedirects {},
    SameOriginOnly {},
    Limited { max: u8 },
}

impl Default for RedirectPolicy {
    fn default() -> Self {
        Self::SameOriginOnly {}
    }
}
