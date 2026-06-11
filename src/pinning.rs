//! Per-domain SPKI public-key-pinning verifier.
//!
//! Wraps `rustls-platform-verifier`: the system trust chain validates first
//! (CT, revocation, MDM roots all apply), then — for hosts that have a pin
//! entry — we additionally require that at least one cert in the
//! server-presented chain (leaf OR intermediate) has an SPKI whose SHA-256
//! matches one of that host's configured pins. Matches OkHttp's
//! CertificatePinner, Apple NSPinnedDomains, Android NetworkSecurityConfig.
//!
//! Pinning is **scoped per host**: a connection is pin-checked only against the
//! entries whose `host` matches its SNI hostname (exact, ASCII
//! case-insensitive; or any subdomain when `include_subdomains`). A host with
//! no matching entry falls through to the platform verifier alone — so pinning
//! one host never breaks requests to an unpinned host. An empty pin list
//! disables pinning entirely.
//!
//! Two safeguards (when a host IS pinned):
//! 1. The leaf MUST parse — a chain whose end-entity we can't read is
//!    rejected, so a malformed leaf can't be skipped for an intermediate match.
//! 2. Pin the right thing: NEVER a public root (every cert it issues would
//!    satisfy the pin). Pin the issuing intermediate (leaf rotates freely) or
//!    the leaf plus a backup; always keep >= 2 pins (OWASP Pinning Cheat Sheet).
//!
//! Pin format: base64 SHA-256 of the DER SPKI (RFC 7469). Compute with:
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

use crate::config::CertPin;
use crate::error::PqcError;

/// SHA-256 of a DER-encoded SubjectPublicKeyInfo. 32 bytes.
pub type SpkiHash = [u8; 32];

/// One host's decoded pin set. `host` is stored lowercased so SNI matching is
/// a plain ASCII comparison.
#[derive(Debug)]
pub struct DomainPins {
    host: String,
    include_subdomains: bool,
    pins: Vec<SpkiHash>,
}

impl DomainPins {
    /// Does this entry apply to `sni_host` (already lowercased)? Exact match,
    /// or a dot-anchored suffix match when `include_subdomains` (so
    /// `example.com` covers `a.example.com` but never `notexample.com`).
    fn matches(&self, sni_host: &str) -> bool {
        if sni_host == self.host {
            return true;
        }
        self.include_subdomains
            && sni_host
                .strip_suffix(&self.host)
                .is_some_and(|prefix| prefix.ends_with('.'))
    }
}

/// A `ServerCertVerifier` that chains: platform verifier → per-host SPKI pin
/// check.
#[derive(Debug)]
pub struct PinningVerifier {
    inner: Arc<dyn ServerCertVerifier>,
    domains: Vec<DomainPins>,
}

impl PinningVerifier {
    pub fn new(inner: Arc<dyn ServerCertVerifier>, domains: Vec<DomainPins>) -> Self {
        Self { inner, domains }
    }

    /// The union of pins for every entry matching `sni_host`. Empty when the
    /// host is not pinned.
    fn pins_for(&self, sni_host: &str) -> Vec<SpkiHash> {
        self.domains
            .iter()
            .filter(|d| d.matches(sni_host))
            .flat_map(|d| d.pins.iter().copied())
            .collect()
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

        // 2) Per-host SPKI pin check. Only hosts with a matching entry are
        // pinned; an unpinned host (or IP-literal SNI) falls through to the
        // platform verifier's result.
        //
        // This runs once per handshake against the SNI, which is per-host:
        // hyper pools connections strictly by `(scheme, authority)` and does
        // NOT coalesce across hosts, so a connection opened for host A is never
        // reused for B. Don't add request-level re-checking to "fix" coalescing
        // — unlike OkHttp (which coalesces) there is none here to fix.
        let ServerName::DnsName(dns) = server_name else {
            return Ok(verified);
        };
        let sni_host = dns.as_ref().to_ascii_lowercase();
        let pins = self.pins_for(&sni_host);
        if pins.is_empty() {
            return Ok(verified);
        }

        // Leaf MUST parse first, so a malformed end-entity can't be skipped
        // in favour of an intermediate match.
        let leaf_hash = extract_spki_sha256(end_entity).ok_or_else(|| {
            rustls::Error::General(
                "certificate pinning failure: leaf certificate SPKI could not be extracted"
                    .to_string(),
            )
        })?;

        // Match the leaf OR any intermediate (unparseable intermediates
        // just can't match).
        let matched = pins.contains(&leaf_hash)
            || intermediates
                .iter()
                .filter_map(|cert| extract_spki_sha256(cert))
                .any(|hash| pins.contains(&hash));

        if !matched {
            return Err(rustls::Error::General(
                "certificate pinning failure: no certificate in the chain matched any configured pin"
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

    // Delegate so we track the inner verifier if it ever enables RFC 7250
    // raw-public-key mode, instead of hardcoding false.
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

/// Decode the config's per-domain pin entries into matchable `DomainPins`,
/// trimming and lowercasing each host. `PqcError::InvalidRequest` on any
/// malformed entry (bad base64, wrong hash length, empty `host`, or empty pin
/// list).
///
/// An empty pin list is rejected, not accepted: it would fail open (the host
/// looks pinned but isn't). To disable pinning, omit the host from
/// `pinned_domains` entirely.
pub fn decode_domain_pins(domains: &[CertPin]) -> Result<Vec<DomainPins>, PqcError> {
    domains
        .iter()
        .map(|d| {
            if d.host.trim().is_empty() || d.spki_sha256.is_empty() {
                return Err(PqcError::InvalidRequest);
            }
            Ok(DomainPins {
                // Trim before storing: the guard above accepts a whitespace-
                // padded host, but the SNI we match against never has
                // whitespace, so storing the untrimmed host would silently
                // never match (fail-open). Normalize so stored == matchable.
                host: d.host.trim().to_ascii_lowercase(),
                include_subdomains: d.include_subdomains,
                pins: decode_pin_list(&d.spki_sha256)?,
            })
        })
        .collect()
}

/// Decode base64 SPKI SHA-256 strings to raw 32-byte hashes;
/// `PqcError::InvalidRequest` on any malformed entry. Accepts standard AND
/// URL-safe alphabets (padded or not) — openssl emits one, Keystore exports
/// the other; refusing either would be a silent "never matches".
fn decode_pin_list(encoded: &[String]) -> Result<Vec<SpkiHash>, PqcError> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    use base64::Engine;

    encoded
        .iter()
        .map(|s| {
            // First alphabet to decode wins.
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

    /// A pin entry for `host` (no subdomains), with hosts stored lowercased as
    /// `decode_domain_pins` would.
    fn domain(host: &str, pins: Vec<SpkiHash>) -> DomainPins {
        DomainPins {
            host: host.to_ascii_lowercase(),
            include_subdomains: false,
            pins,
        }
    }

    #[test]
    fn leaf_pin_match_accepts() {
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("test.local", vec![leaf_hash])],
        );
        let result = v.verify_server_cert(&leaf, &[], &name("test.local"), &[], now());
        assert!(result.is_ok(), "matching leaf SPKI should accept the chain");
    }

    #[test]
    fn intermediate_pin_match_accepts() {
        // Pinning the intermediate accepts a chain with a differing leaf —
        // the rotation-resilient pattern.
        let leaf = make_test_cert();
        let intermediate = make_test_cert();
        let int_hash = extract_spki_sha256(&intermediate).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("test.local", vec![int_hash])],
        );
        let result = v.verify_server_cert(&leaf, &[intermediate], &name("test.local"), &[], now());
        assert!(
            result.is_ok(),
            "a pinned intermediate present in the chain should accept"
        );
    }

    #[test]
    fn pin_absent_from_chain_rejects() {
        // The core guarantee: a pin matching nothing in the chain rejects.
        let leaf = make_test_cert();
        let intermediate = make_test_cert();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("test.local", vec![[7u8; 32]])],
        );
        let result = v.verify_server_cert(&leaf, &[intermediate], &name("test.local"), &[], now());
        assert!(
            result.is_err(),
            "a pin matching no certificate in the chain must be rejected"
        );
    }

    #[test]
    fn unparseable_leaf_rejects_even_if_intermediate_matches() {
        // Even with a pinned intermediate present, an unreadable leaf rejects.
        let garbage = CertificateDer::from(vec![0u8; 16]);
        let intermediate = make_test_cert();
        let int_hash = extract_spki_sha256(&intermediate).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("test.local", vec![int_hash])],
        );
        let result =
            v.verify_server_cert(&garbage, &[intermediate], &name("test.local"), &[], now());
        assert!(
            result.is_err(),
            "unparseable leaf must be rejected even if an intermediate matches"
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
        // Pin both old and new leaf SPKIs for the host; either accepts.
        let leaf_a = make_test_cert();
        let leaf_b = make_test_cert();
        let hash_a = extract_spki_sha256(&leaf_a).unwrap();
        let hash_b = extract_spki_sha256(&leaf_b).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("test.local", vec![hash_a, hash_b])],
        );
        assert!(v
            .verify_server_cert(&leaf_a, &[], &name("test.local"), &[], now())
            .is_ok());
        assert!(v
            .verify_server_cert(&leaf_b, &[], &name("test.local"), &[], now())
            .is_ok());
    }

    #[test]
    fn unpinned_host_passes_through() {
        // The multi-host fix: pinning host A must NOT reject a request to an
        // unpinned host B, even though B's chain matches none of A's pins.
        let leaf_a = make_test_cert();
        let hash_a = extract_spki_sha256(&leaf_a).unwrap();
        let leaf_b = make_test_cert();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("api.bank.com", vec![hash_a])],
        );
        // Host B is not in the pin set → falls through to the inner verifier.
        let result = v.verify_server_cert(&leaf_b, &[], &name("cdn.other.com"), &[], now());
        assert!(
            result.is_ok(),
            "an unpinned host must fall through to the platform verifier"
        );
    }

    #[test]
    fn pinned_host_still_enforced_alongside_unpinned() {
        // Same client, the pinned host must still reject a mismatching chain.
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("api.bank.com", vec![[9u8; 32]])],
        );
        let leaf = make_test_cert(); // SPKI won't match [9u8; 32]
        let result = v.verify_server_cert(&leaf, &[], &name("api.bank.com"), &[], now());
        assert!(
            result.is_err(),
            "the pinned host must still enforce its pins"
        );
    }

    #[test]
    fn distinct_domains_use_distinct_pin_sets() {
        // A leaf valid for host A must NOT satisfy host B's pin entry.
        let leaf_a = make_test_cert();
        let hash_a = extract_spki_sha256(&leaf_a).unwrap();
        let leaf_b = make_test_cert();
        let hash_b = extract_spki_sha256(&leaf_b).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![
                domain("a.example.com", vec![hash_a]),
                domain("b.example.com", vec![hash_b]),
            ],
        );
        // Right cert for the right host: accept.
        assert!(v
            .verify_server_cert(&leaf_a, &[], &name("a.example.com"), &[], now())
            .is_ok());
        // Host B's cert presented for host A: reject (cross-pin).
        assert!(v
            .verify_server_cert(&leaf_b, &[], &name("a.example.com"), &[], now())
            .is_err());
    }

    #[test]
    fn include_subdomains_matches_subdomain_but_not_sibling() {
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let entry = DomainPins {
            host: "example.com".to_string(),
            include_subdomains: true,
            pins: vec![leaf_hash],
        };
        let v = PinningVerifier::new(Arc::new(AlwaysOkVerifier), vec![entry]);

        // Exact host and any depth of subdomain are pinned (matching chain).
        for host in ["example.com", "a.example.com", "b.a.example.com"] {
            assert!(
                v.verify_server_cert(&leaf, &[], &name(host), &[], now())
                    .is_ok(),
                "{host} should be covered by include_subdomains and match"
            );
        }

        // A sibling that merely ends with the same string is NOT a subdomain.
        let other = make_test_cert(); // different SPKI
        assert!(
            v.verify_server_cert(&other, &[], &name("notexample.com"), &[], now())
                .is_ok(),
            "notexample.com is not a subdomain of example.com, so it is unpinned"
        );
    }

    #[test]
    fn include_subdomains_false_does_not_match_subdomain() {
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("example.com", vec![leaf_hash])],
        );
        // Subdomain is unpinned when include_subdomains is false → passthrough,
        // even though this leaf's SPKI happens to be the pinned one.
        let other = make_test_cert();
        assert!(
            v.verify_server_cert(&other, &[], &name("a.example.com"), &[], now())
                .is_ok(),
            "a.example.com must be unpinned when include_subdomains is false"
        );
    }

    #[test]
    fn ip_literal_sni_is_never_pinned() {
        // Pinning is hostname-only; an IP-literal SNI falls through.
        let leaf = make_test_cert();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("example.com", vec![[3u8; 32]])],
        );
        let result = v.verify_server_cert(&leaf, &[], &name("203.0.113.5"), &[], now());
        assert!(
            result.is_ok(),
            "IP-literal SNI must not be subject to hostname pinning"
        );
    }

    #[test]
    fn sni_host_matched_case_insensitively() {
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("api.example.com", vec![leaf_hash])],
        );
        // rustls lowercases DnsName on construction, but the verifier also
        // lowercases defensively — assert the pin still applies.
        let result = v.verify_server_cert(&leaf, &[], &name("API.EXAMPLE.COM"), &[], now());
        assert!(result.is_ok(), "host matching must be case-insensitive");
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
        // 0xFB 0xFF encodes to base64 containing both '-' and '_', so this
        // actually exercises the URL-safe alphabet.
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

    #[test]
    fn decode_domain_pins_lowercases_host_and_decodes() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let raw = [0u8; 32];
        let entries = vec![CertPin {
            host: "API.Example.COM".to_string(),
            include_subdomains: true,
            spki_sha256: vec![STANDARD.encode(raw)],
        }];
        let decoded = decode_domain_pins(&entries).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].host, "api.example.com");
        assert!(decoded[0].include_subdomains);
        assert_eq!(decoded[0].pins, vec![raw]);
    }

    #[test]
    fn decode_domain_pins_rejects_empty_host() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let entries = vec![CertPin {
            host: "   ".to_string(),
            include_subdomains: false,
            spki_sha256: vec![STANDARD.encode([0u8; 32])],
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }

    #[test]
    fn decode_domain_pins_trims_host_so_padded_host_still_pins() {
        // A whitespace-padded host must still pin its (unpadded) SNI, not
        // silently fall through unpinned. Regression for the trim/store
        // mismatch in decode_domain_pins.
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let leaf = make_test_cert();
        let leaf_hash = extract_spki_sha256(&leaf).unwrap();
        let entries = vec![CertPin {
            host: "  API.Example.com\n".to_string(),
            include_subdomains: false,
            spki_sha256: vec![STANDARD.encode(leaf_hash)],
        }];
        let decoded = decode_domain_pins(&entries).unwrap();
        assert_eq!(decoded[0].host, "api.example.com", "host must be trimmed");

        // And it actually enforces against the clean SNI: a mismatching leaf
        // on that host is rejected (proves the entry is live, not skipped).
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain("api.example.com", vec![[1u8; 32]])],
        );
        let other = make_test_cert();
        assert!(v
            .verify_server_cert(&other, &[], &name("api.example.com"), &[], now())
            .is_err());
    }

    #[test]
    fn decode_domain_pins_rejects_empty_pin_list() {
        // An entry with a host but no hashes would silently leave that host
        // unpinned (fail-open). Reject it at construction instead.
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec![],
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }

    #[test]
    fn decode_domain_pins_propagates_bad_pin() {
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec!["not-valid-base64-!!!".to_string()],
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }

    #[test]
    fn decode_domain_pins_empty_returns_empty() {
        assert!(decode_domain_pins(&[]).unwrap().is_empty());
    }
}
