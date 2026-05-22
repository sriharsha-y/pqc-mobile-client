//! SPKI public-key-pinning verifier.
//!
//! Wraps `rustls-platform-verifier` so the system trust chain validates
//! first (CT, revocation, MDM-distributed enterprise roots all still
//! apply), then enforces that at least one cert in the chain has its
//! SubjectPublicKeyInfo SHA-256 matching one of the configured pins.
//!
//! Empty pin list disables pinning (verification falls through to the
//! platform verifier alone).
//!
//! Pin format: base64-encoded SHA-256 of the DER-encoded SPKI (the
//! same format used by HTTP Public Key Pinning RFC 7469 and by Cronet's
//! `addPublicKeyPins`).
//!
//! How to compute a pin from a server cert:
//! ```sh
//! openssl s_client -servername api.example.com -connect api.example.com:443 < /dev/null 2>/dev/null \
//!   | openssl x509 -pubkey -noout \
//!   | openssl pkey -pubin -outform der \
//!   | openssl dgst -sha256 -binary \
//!   | base64
//! ```

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};

use crate::error::PqcError;

/// SHA-256 of a DER-encoded SubjectPublicKeyInfo. 32 bytes.
pub type SpkiHash = [u8; 32];

/// A `ServerCertVerifier` that chains: platform verifier → SPKI pin check.
#[derive(Debug)]
pub struct PinningVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    pins: Vec<SpkiHash>,
}

impl PinningVerifier {
    pub fn new(inner: Arc<dyn ServerCertVerifier>, pins: Vec<SpkiHash>) -> Self {
        Self { inner, pins }
    }
}

impl ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // 1) Chain validation via the platform verifier.
        let verified = self.inner.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        )?;

        // 2) SPKI pin check. Empty list means pinning disabled.
        if self.pins.is_empty() {
            return Ok(verified);
        }

        let chain = std::iter::once(end_entity).chain(intermediates.iter());
        for cert in chain {
            if let Some(hash) = extract_spki_sha256(cert) {
                if self.pins.contains(&hash) {
                    return Ok(verified);
                }
            }
        }

        Err(rustls::Error::General(
            "certificate pinning failure: no SPKI in chain matched a configured pin".to_string(),
        ))
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// SHA-256 of the DER-encoded SPKI extracted from an X.509 certificate.
/// Returns `None` if the cert fails to parse.
pub fn extract_spki_sha256(cert: &CertificateDer<'_>) -> Option<SpkiHash> {
    use x509_parser::prelude::*;

    let (_, x509) = X509Certificate::from_der(cert.as_ref()).ok()?;
    let spki_der = x509.tbs_certificate.subject_pki.raw;
    let digest = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, spki_der);
    digest.as_ref().try_into().ok()
}

/// Decode a list of base64-encoded SPKI SHA-256 strings into raw 32-byte hashes.
/// Returns `PqcError::InvalidRequest` if any entry is malformed.
pub fn decode_pin_list(encoded: &[String]) -> Result<Vec<SpkiHash>, PqcError> {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    encoded
        .iter()
        .map(|s| {
            let bytes = STANDARD.decode(s).map_err(|_| PqcError::InvalidRequest)?;
            bytes.try_into().map_err(|_| PqcError::InvalidRequest)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use rustls::pki_types::CertificateDer;

    fn make_test_cert() -> CertificateDer<'static> {
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["test.local".to_string()]).unwrap();
        CertificateDer::from(cert.der().to_vec())
    }

    #[test]
    fn extract_spki_sha256_returns_32_bytes() {
        let cert = make_test_cert();
        let hash = extract_spki_sha256(&cert).expect("should extract SPKI hash");
        assert_eq!(hash.len(), 32);
    }

    #[test]
    fn extract_spki_sha256_is_deterministic() {
        let cert = make_test_cert();
        let h1 = extract_spki_sha256(&cert).unwrap();
        let h2 = extract_spki_sha256(&cert).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn extract_spki_sha256_different_certs_different_hashes() {
        let a = extract_spki_sha256(&make_test_cert()).unwrap();
        let b = extract_spki_sha256(&make_test_cert()).unwrap();
        assert_ne!(
            a, b,
            "freshly generated certs should produce different SPKI hashes"
        );
    }

    #[test]
    fn extract_spki_sha256_returns_none_on_garbage() {
        let garbage = CertificateDer::from(vec![0u8; 16]);
        assert!(extract_spki_sha256(&garbage).is_none());
    }

    #[test]
    fn decode_pin_list_accepts_valid_base64() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let raw = [0u8; 32];
        let encoded = vec![STANDARD.encode(raw)];
        let decoded = decode_pin_list(&encoded).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0], raw);
    }

    #[test]
    fn decode_pin_list_rejects_wrong_length() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let encoded = vec![STANDARD.encode([0u8; 16])]; // not 32 bytes
        assert!(decode_pin_list(&encoded).is_err());
    }

    #[test]
    fn decode_pin_list_rejects_invalid_base64() {
        let encoded = vec!["not-valid-base64-!!!".to_string()];
        assert!(decode_pin_list(&encoded).is_err());
    }

    #[test]
    fn decode_pin_list_empty_returns_empty() {
        let decoded = decode_pin_list(&[]).unwrap();
        assert!(decoded.is_empty());
    }
}
