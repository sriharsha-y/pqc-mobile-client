use std::sync::{Arc, OnceLock};

use rustls::crypto::CryptoProvider;
use rustls::ClientConfig;
use rustls_platform_verifier::BuilderVerifierExt;

use crate::config::PqcConfig;
use crate::error::PqcError;
use crate::kx_tracker::instrument_provider;
use crate::pinning::{decode_pin_list, PinningVerifier};

// instrument_provider Box::leaks one wrapper per kx group on every
// call. Cache the wrapped provider per (post_quantum on/off) so
// repeated PqcHttpClient construction reuses the same wrappers
// instead of leaking a fresh set on every client.
static INSTRUMENTED_PQ: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
static INSTRUMENTED_CLASSICAL: OnceLock<Arc<CryptoProvider>> = OnceLock::new();

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
    // Wrap so the negotiated kx group is recorded into a global atomic
    // and can be read after each request via kx_tracker::last_negotiated_group_str().
    // The wrapper allocates 'static memory per kx group; the OnceLock
    // pair ensures we wrap each provider variant at most once per process,
    // regardless of how many PqcHttpClient instances are constructed.
    let slot = if cfg.enable_post_quantum {
        &INSTRUMENTED_PQ
    } else {
        &INSTRUMENTED_CLASSICAL
    };
    let provider = slot
        .get_or_init(|| {
            let base = if cfg.enable_post_quantum {
                rustls_post_quantum::provider()
            } else {
                rustls::crypto::aws_lc_rs::default_provider()
            };
            Arc::new(instrument_provider(base))
        })
        .clone();

    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| PqcError::Tls)?;

    let tls = if cfg.pinned_cert_sha256.is_empty() {
        // No pinning: just the platform verifier.
        builder.with_platform_verifier().with_no_client_auth()
    } else {
        // Pinning enabled: wrap the platform verifier so the chain still
        // validates against the system trust store, and additionally
        // require that one cert's SPKI hash matches a configured pin.
        let inner: Arc<dyn rustls::client::danger::ServerCertVerifier> =
            Arc::new(rustls_platform_verifier::Verifier::new());
        let pins = decode_pin_list(&cfg.pinned_cert_sha256)?;
        let pinning = Arc::new(PinningVerifier::new(inner, pins));

        builder
            .dangerous()
            .with_custom_certificate_verifier(pinning)
            .with_no_client_auth()
    };

    Ok(tls)
}
