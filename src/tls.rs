use std::sync::Arc;

use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;

use crate::config::PqcConfig;
use crate::error::PqcError;
use crate::pinning::{decode_pin_list, PinningVerifier};

/// Build a rustls `ClientConfig` for the PQC hybrid handshake.
///
/// - Always uses `rustls_post_quantum::provider()`, which prepends
///   `X25519MLKEM768` to the default group list (so the `ClientHello` carries
///   both `X25519MLKEM768` and classical `X25519` key_shares; a PQ-capable
///   peer negotiates the hybrid, anything else falls back to classical). There
///   is deliberately no switch to turn this off — see `PqcConfig`.
/// - Cert chain validation defers to the platform trust store via
///   `rustls-platform-verifier` (iOS Security framework / Android KeyStore),
///   so MDM-distributed enterprise roots, captive portals, and OS revocation
///   continue to work.
/// - When `pinned_cert_sha256` is non-empty, wraps the platform verifier in a
///   `PinningVerifier` that additionally enforces an SPKI pin from the chain.
pub fn build_tls_config(cfg: &PqcConfig) -> Result<ClientConfig, PqcError> {
    // Always offer the X25519MLKEM768 hybrid. This provider prepends it to the
    // default group list, so the ClientHello carries both the hybrid and
    // classical X25519 — servers that don't support the hybrid negotiate
    // classical automatically, transparently to the caller.
    let provider = Arc::new(rustls_post_quantum::provider());

    // builder_with_provider consumes the provider, but the pinning branch
    // also needs it to share the same crypto stack. Arc::clone is a
    // refcount bump, not a deep copy.
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| PqcError::Tls)?;

    let tls = if cfg.pinned_cert_sha256.is_empty() {
        // No pinning: just the platform verifier. In rustls-platform-verifier
        // 0.7, `with_platform_verifier()` returns Result (it loads the system
        // trust store eagerly and can fail) — propagate as Tls.
        builder
            .with_platform_verifier()
            .map_err(|_| PqcError::Tls)?
            .with_no_client_auth()
    } else {
        // Wrap the platform verifier (chain still validates against the
        // system store) and additionally require an SPKI pin match.
        // The inner verifier MUST share rustls's CryptoProvider — in 0.7 the
        // provider is a required `Verifier::new()` argument (was a separate
        // `.with_provider()` call in 0.5). Returns Result.
        let inner: Arc<dyn rustls::client::danger::ServerCertVerifier> = Arc::new(
            rustls_platform_verifier::Verifier::new(provider.clone()).map_err(|_| PqcError::Tls)?,
        );
        let pins = decode_pin_list(&cfg.pinned_cert_sha256)?;
        let pinning = Arc::new(PinningVerifier::new(inner, pins));

        builder
            .dangerous()
            .with_custom_certificate_verifier(pinning)
            .with_no_client_auth()
    };

    // ALPN must be set explicitly: reqwest's use_preconfigured_tls does
    // NOT inject it into a caller-supplied ClientConfig, so without this
    // the server falls back to HTTP/1.1 and the http2 flag is a lie.
    // h2 first per RFC 7301 ordering.
    let mut tls = tls;
    tls.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(tls)
}
