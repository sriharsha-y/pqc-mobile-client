use std::sync::Arc;

use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;

use crate::config::PqcConfig;
use crate::error::PqcError;

/// Build a rustls `ClientConfig` with the requested crypto provider.
///
/// - When `enable_post_quantum` is true, uses `rustls_post_quantum::provider()`
///   which prepends `X25519MLKEM768` to the default group list (so the
///   `ClientHello` carries both `X25519MLKEM768` and `X25519` key_shares).
/// - Cert validation defers to the platform trust store via
///   `rustls-platform-verifier` (iOS Security framework / Android KeyStore),
///   so MDM-distributed enterprise roots, captive portals, and OS revocation
///   continue to work.
///
/// TODO: layer a custom `ServerCertVerifier` on top when `pinned_cert_sha256`
/// is non-empty so that platform verification AND SPKI pinning both apply.
pub fn build_tls_config(cfg: &PqcConfig) -> Result<ClientConfig, PqcError> {
    let provider = if cfg.enable_post_quantum {
        Arc::new(rustls_post_quantum::provider())
    } else {
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    };

    let tls = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| PqcError::Tls)?
        .with_platform_verifier()
        .with_no_client_auth();

    // TODO(pinning): if !cfg.pinned_cert_sha256.is_empty() {
    //     wrap tls.dangerous().with_custom_certificate_verifier(...)
    // }

    Ok(tls)
}
