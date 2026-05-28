use std::sync::Arc;

use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;

use crate::config::PqcConfig;
use crate::error::PqcError;
use crate::pinning::{decode_pin_list, PinningVerifier};

/// Build a rustls `ClientConfig` with the requested crypto provider.
///
/// - When `enable_post_quantum` is true, uses `rustls_post_quantum::provider()`
///   which prepends `X25519MLKEM768` to the default group list (so the
///   `ClientHello` carries both `X25519MLKEM768` and `X25519` key_shares).
/// - Cert chain validation defers to the platform trust store via
///   `rustls-platform-verifier` (iOS Security framework / Android KeyStore),
///   so MDM-distributed enterprise roots, captive portals, and OS revocation
///   continue to work.
/// - When `pinned_cert_sha256` is non-empty, wraps the platform verifier in a
///   `PinningVerifier` that additionally enforces an SPKI pin from the chain.
pub fn build_tls_config(cfg: &PqcConfig) -> Result<ClientConfig, PqcError> {
    let provider = Arc::new(if cfg.enable_post_quantum {
        rustls_post_quantum::provider()
    } else {
        // rustls's DEFAULT_KX_GROUPS includes X25519MLKEM768 regardless of
        // the `prefer-post-quantum` feature (it only changes the hybrid's
        // position), so default_provider() still offers the hybrid and a
        // PQ-capable server negotiates it. To actually disable PQC, drop
        // the MLKEM hybrid so the ClientHello carries classical groups only.
        let mut base = rustls::crypto::aws_lc_rs::default_provider();
        base.kx_groups
            .retain(|g| g.name() != rustls::NamedGroup::X25519MLKEM768);
        base
    });

    // builder_with_provider consumes the provider, but the pinning branch
    // also needs it to share the same crypto stack. Arc::clone is a
    // refcount bump, not a deep copy.
    let builder = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|_| PqcError::Tls)?;

    let tls = if cfg.pinned_cert_sha256.is_empty() {
        // No pinning: just the platform verifier.
        builder.with_platform_verifier().with_no_client_auth()
    } else {
        // Wrap the platform verifier (chain still validates against the
        // system store) and additionally require an SPKI pin match.
        // The inner verifier MUST share rustls's CryptoProvider: a bare
        // `Verifier::new()` reaches for the process-default provider,
        // which we never install, and panics ("rustls default
        // CryptoProvider not set") on the first signature check.
        let inner: Arc<dyn rustls::client::danger::ServerCertVerifier> =
            Arc::new(rustls_platform_verifier::Verifier::new().with_provider(provider.clone()));
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
