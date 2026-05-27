//! SPKI public-key-pinning verifier.
//!
//! Wraps `rustls-platform-verifier` so the system trust chain validates
//! first (CT, revocation, MDM-distributed enterprise roots all still
//! apply), then enforces that the **leaf** certificate's SubjectPublicKeyInfo
//! SHA-256 matches one of the configured pins.
//!
//! Empty pin list disables pinning (verification falls through to the
//! platform verifier alone).
//!
//! ## Why leaf-strict, not any-cert-in-chain
//!
//! Earlier revisions of this file matched the pin against any cert in the
//! server-presented chain (leaf + intermediates). That semantic has two
//! footguns:
//!
//! 1. **Root-pin bypass.** An operator who pins a popular root CA's SPKI
//!    (a common mistake when copy-pasting from "find your cert's hash"
//!    tutorials) accepts any chain that includes that root — including
//!    one from an unrelated compromised intermediate under the same root.
//!    The pin then offers no protection beyond the OS trust store.
//!
//! 2. **Silent leaf-parse skip.** If the leaf fails to parse but an
//!    intermediate matches a configured pin, the verifier would accept
//!    the chain even though the leaf was never compared to its pin.
//!
//! Leaf-strict closes both: the leaf MUST be parseable and MUST match.
//! For rotation, configure BOTH the active leaf SPKI AND the pre-deployed
//! next leaf SPKI as pins. Intermediate or root pins are NOT supported by
//! design.
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

        // Leaf-strict pinning. The end-entity (leaf) certificate's SPKI
        // MUST be parseable and MUST match a configured pin. We never
        // accept a match against any intermediate the server included —
        // that would let a server present a chain where only an
        // intermediate (or a server-included root from the trust store)
        // matches the pin, defeating the pinning guarantee.
        //
        // For rotation, configure BOTH the active leaf SPKI AND the
        // pre-deployed next leaf SPKI as pins.
        let leaf_hash = extract_spki_sha256(end_entity).ok_or_else(|| {
            rustls::Error::General(
                "certificate pinning failure: leaf certificate SPKI could not be extracted"
                    .to_string(),
            )
        })?;

        if !self.pins.contains(&leaf_hash) {
            return Err(rustls::Error::General(
                "certificate pinning failure: leaf SPKI does not match any configured pin"
                    .to_string(),
            ));
        }

        Ok(verified)
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

    // RFC 7250 raw-public-key mode is opt-in client-side; today the
    // platform verifier returns `false`. Delegating defensively so that
    // if rustls-platform-verifier ever flips this for a future Apple /
    // Android trust-store API, we don't silently downgrade the signal
    // to `false` and confuse rustls's chain-parsing branch.
    // https://docs.rs/rustls/0.23.40/rustls/client/danger/trait.ServerCertVerifier.html#method.requires_raw_public_keys
    fn requires_raw_public_keys(&self) -> bool {
        self.inner.requires_raw_public_keys()
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
///
/// Accepts BOTH standard (`+`/`/`) and URL-safe (`-`/`_`) base64
/// alphabets, with or without padding. The UDL's `pinned_cert_sha256`
/// doc-comment promises both, and the two alphabets show up in the
/// wild for the same use case: openssl emits standard; JWT-adjacent
/// tooling and Android Keystore exports emit URL-safe. Refusing one
/// or the other would be a silent footgun — the pin just "never
/// matches" with no useful error.
pub fn decode_pin_list(encoded: &[String]) -> Result<Vec<SpkiHash>, PqcError> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine;

    encoded
        .iter()
        .map(|s| {
            // Try alphabets in order; first to decode wins. The wrong
            // alphabet on a given string fails fast with an
            // InvalidByte error, so the fall-through is cheap.
            let bytes = STANDARD
                .decode(s)
                .or_else(|_| STANDARD_NO_PAD.decode(s))
                .or_else(|_| URL_SAFE.decode(s))
                .or_else(|_| URL_SAFE_NO_PAD.decode(s))
                .map_err(|_| PqcError::InvalidRequest)?;
            bytes.try_into().map_err(|_| PqcError::InvalidRequest)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use rustls::client::danger::HandshakeSignatureValid;
    use rustls::pki_types::CertificateDer;
    use std::time::Duration;

    fn make_test_cert() -> CertificateDer<'static> {
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["test.local".to_string()]).unwrap();
        CertificateDer::from(cert.der().to_vec())
    }

    /// Stub inner verifier that always accepts the chain. Lets us exercise
    /// the pinning-layer logic in isolation from the platform trust store.
    #[derive(Debug)]
    struct AlwaysOkVerifier;

    impl ServerCertVerifier for AlwaysOkVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![SignatureScheme::ECDSA_NISTP256_SHA256]
        }
    }

    fn now() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000))
    }

    fn name(host: &str) -> ServerName<'static> {
        ServerName::try_from(host.to_string()).unwrap()
    }

    #[test]
    fn leaf_pin_match_accepts() {
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![leaf_hash]);
        let result = v.verify_server_cert(&leaf, &[], &name("test.local"), &[], now());
        assert!(result.is_ok(), "matching leaf SPKI should accept the chain");
    }

    #[test]
    fn leaf_mismatch_rejects_even_if_intermediate_matches() {
        // Defense-in-depth: pinning to an intermediate alone must NOT
        // accept the chain. Old behavior would have accepted this.
        let leaf = make_test_cert();
        let intermediate = make_test_cert();
        let int_hash = extract_spki_sha256(&intermediate).unwrap();
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![int_hash]);
        let result = v.verify_server_cert(&leaf, &[intermediate], &name("test.local"), &[], now());
        assert!(
            result.is_err(),
            "intermediate-only pin match must be rejected under leaf-strict semantics"
        );
    }

    #[test]
    fn unparseable_leaf_rejects_even_with_pin_list() {
        // If the leaf DER fails to parse, we MUST NOT silently fall
        // through and accept on the basis of some other cert. Old
        // behavior would have skipped the leaf and checked intermediates.
        let garbage = CertificateDer::from(vec![0u8; 16]);
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![[0u8; 32]]);
        let result = v.verify_server_cert(&garbage, &[], &name("test.local"), &[], now());
        assert!(
            result.is_err(),
            "unparseable leaf must be rejected, not skipped"
        );
    }

    #[test]
    fn empty_pin_list_disables_pinning() {
        let leaf = make_test_cert();
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![]);
        let result = v.verify_server_cert(&leaf, &[], &name("test.local"), &[], now());
        assert!(
            result.is_ok(),
            "empty pin list should fall through to inner verifier"
        );
    }

    #[test]
    fn rotation_pin_set_accepts_either_leaf() {
        // Backwards-compatible rotation: pin both old and new leaf SPKIs;
        // either should accept.
        let leaf_a = make_test_cert();
        let leaf_b = make_test_cert();
        let hash_a = extract_spki_sha256(&leaf_a).unwrap();
        let hash_b = extract_spki_sha256(&leaf_b).unwrap();
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![hash_a, hash_b]);
        assert!(v
            .verify_server_cert(&leaf_a, &[], &name("test.local"), &[], now())
            .is_ok());
        assert!(v
            .verify_server_cert(&leaf_b, &[], &name("test.local"), &[], now())
            .is_ok());
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

    #[test]
    fn decode_pin_list_accepts_url_safe_alphabet() {
        use base64::engine::general_purpose::URL_SAFE;
        use base64::Engine;
        // Construct a 32-byte payload whose base64 encoding actually
        // contains the URL-safe-distinctive characters '-' and '_'
        // (which standard base64 would emit as '+' and '/'). The byte
        // sequence 0xFB 0xFF picks up both in adjacent positions.
        let raw: [u8; 32] = [0xFB, 0xFF]
            .iter()
            .cycle()
            .take(32)
            .copied()
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let encoded = URL_SAFE.encode(raw);
        assert!(
            encoded.contains('-') || encoded.contains('_'),
            "test fixture should exercise URL-safe alphabet, got {encoded}"
        );
        let decoded = decode_pin_list(&[encoded]).expect("URL-safe base64 must decode");
        assert_eq!(decoded[0], raw);
    }

    #[test]
    fn decode_pin_list_accepts_unpadded_url_safe() {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let raw = [0x42u8; 32];
        let encoded = URL_SAFE_NO_PAD.encode(raw);
        assert!(!encoded.ends_with('='), "test fixture should be unpadded");
        let decoded = decode_pin_list(&[encoded]).expect("unpadded URL-safe must decode");
        assert_eq!(decoded[0], raw);
    }
}
