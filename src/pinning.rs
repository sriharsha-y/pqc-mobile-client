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
//! Expiration: a `CertPin` may carry an optional `"YYYY-MM-DD"` expiration. On
//! or after 00:00 UTC of that date the entry's pins are treated as absent, so
//! the host falls back to the platform verifier alone (**fail-open**). This is
//! the exact semantics of Android `<pin-set expiration>` and TrustKit
//! `kTSKExpirationDate` — a safety valve so an app that stops receiving pin
//! updates isn't permanently bricked when its pinned key rotates. (OkHttp's
//! `CertificatePinner` and Apple's `NSPinnedDomains` have no expiration at
//! all.) Unset = never expires.
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
    /// Seconds since the Unix epoch (00:00 UTC of the configured date) at which
    /// this entry's pins stop being enforced. `None` = never expires. See the
    /// module doc for the fail-open rationale.
    expires_at: Option<u64>,
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

    /// Whether this entry's pins have lapsed as of `now_secs`. Inclusive (`>=`):
    /// pins are off from 00:00 UTC of the expiry date. `None` = never expires.
    fn is_expired(&self, now_secs: u64) -> bool {
        self.expires_at.is_some_and(|t| now_secs >= t)
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

    /// Union of pins for every non-expired entry matching `sni_host` (`now_secs`
    /// is the handshake time). Empty means the host is unpinned or every
    /// matching entry has expired, so the caller falls back to the platform
    /// verifier alone (fail-open). Entries expire independently.
    fn pins_for(&self, sni_host: &str, now_secs: u64) -> Vec<SpkiHash> {
        self.domains
            .iter()
            .filter(|d| d.matches(sni_host))
            .filter(|d| {
                let expired = d.is_expired(now_secs);
                if expired {
                    log::debug!("matching pin set expired; falling back to platform trust");
                }
                !expired
            })
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
        // Expired entries are excluded here (fail-open), so a host whose only
        // matching pin set has lapsed behaves exactly like an unpinned host.
        let pins = self.pins_for(&sni_host, now.as_secs());
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
                // Malformed date → fail closed here; a typo must not silently
                // make a pin permanent. Absent = never expires.
                expires_at: match &d.expiration {
                    Some(s) => Some(parse_yyyy_mm_dd(s).ok_or(PqcError::InvalidRequest)?),
                    None => None,
                },
            })
        })
        .collect()
}

/// Parse strict `"YYYY-MM-DD"` to seconds since the Unix epoch at 00:00 UTC.
/// `None` on any deviation: field widths, non-digits, month/day out of range
/// (leap years handled). Dependency-free — the date→days step is Howard
/// Hinnant's `days_from_civil`.
fn parse_yyyy_mm_dd(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    // Exact layout: 4 digits, '-', 2 digits, '-', 2 digits.
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = &s[range];
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let year = digits(0..4)?;
    let month = digits(5..7)?;
    let day = digits(8..10)?;

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }

    // Pre-1970 dates give negative days; clamp to 0 (already-expired, the right
    // fail-open outcome) so the result stays in u64.
    let days = days_from_civil(year as i64, month as i64, day as i64);
    let secs = days.checked_mul(86_400)?;
    Some(secs.max(0) as u64)
}

/// Number of days in `month` (1..=12) of `year`, accounting for leap years.
fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Proleptic Gregorian leap-year rule.
fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

/// Days from 1970-01-01 (UTC) to `y-m-d`. Howard Hinnant's `days_from_civil`
/// (http://howardhinnant.github.io/date_algorithms.html#days_from_civil),
/// exact for all valid Gregorian dates.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
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
    /// `decode_domain_pins` would. Never expires.
    fn domain(host: &str, pins: Vec<SpkiHash>) -> DomainPins {
        DomainPins {
            host: host.to_ascii_lowercase(),
            include_subdomains: false,
            pins,
            expires_at: None,
        }
    }

    /// Like `domain` but with an explicit expiration instant (epoch seconds).
    fn domain_exp(host: &str, pins: Vec<SpkiHash>, expires_at: u64) -> DomainPins {
        DomainPins {
            host: host.to_ascii_lowercase(),
            include_subdomains: false,
            pins,
            expires_at: Some(expires_at),
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
            expires_at: None,
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
            expiration: None,
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
            expiration: None,
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
            expiration: None,
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
            expiration: None,
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }

    #[test]
    fn decode_domain_pins_propagates_bad_pin() {
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec!["not-valid-base64-!!!".to_string()],
            expiration: None,
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }

    #[test]
    fn decode_domain_pins_empty_returns_empty() {
        assert!(decode_domain_pins(&[]).unwrap().is_empty());
    }

    // ----- Expiration -----

    #[test]
    fn expired_pin_set_falls_through_to_platform_verifier() {
        // A pin matching nothing in the chain would normally reject; once the
        // entry's expiration has passed it is treated as absent, so the host
        // falls back to the (accepting) platform verifier — fail-open, the
        // Android <pin-set expiration> / TrustKit kTSKExpirationDate behavior.
        let leaf = make_test_cert(); // SPKI won't match the bogus pin
        let past = now().as_secs() - 1; // already expired at `now()`
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain_exp("api.bank.com", vec![[9u8; 32]], past)],
        );
        assert!(
            v.verify_server_cert(&leaf, &[], &name("api.bank.com"), &[], now())
                .is_ok(),
            "an expired pin set must fail open (fall through to platform trust)"
        );
    }

    #[test]
    fn unexpired_pin_set_is_still_enforced() {
        // Same setup, expiration in the future → the mismatching pin still
        // rejects. Proves the fall-through above is expiry, not a globally
        // disabled check.
        let leaf = make_test_cert();
        let future = now().as_secs() + 86_400;
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain_exp("api.bank.com", vec![[9u8; 32]], future)],
        );
        assert!(
            v.verify_server_cert(&leaf, &[], &name("api.bank.com"), &[], now())
                .is_err(),
            "a not-yet-expired pin set must still be enforced"
        );
    }

    #[test]
    fn expiration_boundary_is_inclusive() {
        // At exactly the expiration instant the pins are already disabled
        // ("on that date" per the native configs → now == expires_at expired).
        let leaf = make_test_cert();
        let exactly_now = now().as_secs();
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![domain_exp("api.bank.com", vec![[9u8; 32]], exactly_now)],
        );
        assert!(
            v.verify_server_cert(&leaf, &[], &name("api.bank.com"), &[], now())
                .is_ok(),
            "expiration must be inclusive: now == expires_at means expired"
        );
    }

    #[test]
    fn live_entry_still_enforced_when_a_matching_entry_expired() {
        // Two entries match the same host; the union drops the expired one but
        // keeps enforcing the live one (independent per-entry expiry).
        let leaf = make_test_cert(); // matches neither bogus pin
        let past = now().as_secs() - 1;
        let future = now().as_secs() + 86_400;
        let v = PinningVerifier::new(
            Arc::new(AlwaysOkVerifier),
            vec![
                domain_exp("api.bank.com", vec![[8u8; 32]], past), // expired
                domain_exp("api.bank.com", vec![[9u8; 32]], future), // live
            ],
        );
        assert!(
            v.verify_server_cert(&leaf, &[], &name("api.bank.com"), &[], now())
                .is_err(),
            "a live matching entry must still enforce despite an expired sibling"
        );
    }

    #[test]
    fn parse_yyyy_mm_dd_known_values() {
        assert_eq!(parse_yyyy_mm_dd("1970-01-01"), Some(0));
        assert_eq!(parse_yyyy_mm_dd("1970-01-02"), Some(86_400));
        // 2023-11-14 00:00:00 UTC.
        assert_eq!(parse_yyyy_mm_dd("2023-11-14"), Some(1_699_920_000));
    }

    #[test]
    fn parse_yyyy_mm_dd_handles_leap_years() {
        assert!(
            parse_yyyy_mm_dd("2024-02-29").is_some(),
            "2024 is a leap year"
        );
        assert!(
            parse_yyyy_mm_dd("2023-02-29").is_none(),
            "2023 is not a leap year"
        );
        assert!(
            parse_yyyy_mm_dd("2000-02-29").is_some(),
            "2000 is a leap year (divisible by 400)"
        );
        assert!(
            parse_yyyy_mm_dd("1900-02-29").is_none(),
            "1900 is not a leap year (divisible by 100, not 400)"
        );
    }

    #[test]
    fn parse_yyyy_mm_dd_rejects_malformed() {
        for bad in [
            "",
            "2024-1-1",      // unpadded fields
            "2024/01/01",    // wrong separator
            "2024-13-01",    // month out of range
            "2024-00-01",    // month zero
            "2024-02-30",    // day out of range for February
            "2024-01-32",    // day out of range
            "2024-01-00",    // day zero
            "20240-1-1",     // wrong field widths
            "abcd-ef-gh",    // non-digits
            "2024-01-01 ",   // trailing space (length 11)
            "2024-01-01T00", // extra characters
        ] {
            assert!(
                parse_yyyy_mm_dd(bad).is_none(),
                "{bad:?} should be rejected as malformed"
            );
        }
    }

    #[test]
    fn decode_domain_pins_parses_expiration() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec![STANDARD.encode([0u8; 32])],
            expiration: Some("2030-01-01".to_string()),
        }];
        let decoded = decode_domain_pins(&entries).unwrap();
        assert_eq!(decoded[0].expires_at, parse_yyyy_mm_dd("2030-01-01"));
        assert!(decoded[0].expires_at.is_some());
    }

    #[test]
    fn decode_domain_pins_none_expiration_never_expires() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec![STANDARD.encode([0u8; 32])],
            expiration: None,
        }];
        let decoded = decode_domain_pins(&entries).unwrap();
        assert_eq!(decoded[0].expires_at, None);
    }

    #[test]
    fn decode_domain_pins_rejects_malformed_expiration() {
        // A typo'd date fails closed at construction, not silently "never
        // expires".
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let entries = vec![CertPin {
            host: "example.com".to_string(),
            include_subdomains: false,
            spki_sha256: vec![STANDARD.encode([0u8; 32])],
            expiration: Some("01-01-2030".to_string()), // wrong order/format
        }];
        assert!(decode_domain_pins(&entries).is_err());
    }
}
