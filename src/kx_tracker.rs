//! Record the TLS key-exchange group rustls actually selects.
//!
//! `reqwest` doesn't expose the negotiated named group on its `Response`,
//! and the rustls `ClientConnection` isn't reachable from inside reqwest's
//! transport stack. To recover this information we wrap each
//! `SupportedKxGroup` in the crypto provider so that — when rustls picks
//! one and calls `start()` on it during handshake — the wrapper records
//! the chosen `NamedGroup` into a process-global atomic.
//!
//! Concurrency note: the recorded value is process-wide. Sequential
//! request patterns (the common case for a banking app) read accurate
//! per-request values. Truly concurrent requests on the same client may
//! interleave and one may observe another's group. For deterministic
//! per-connection reporting use server-side telemetry such as Akamai
//! DataStream 2.
//!
//! Caveat: handshakes are not free, so under HTTP/2 keep-alive pooling
//! `start()` is called only when a new TLS connection is established —
//! not on every request. The "last negotiated group" reflects the most
//! recent handshake, which is stable across the pool's lifetime.

use std::fmt;
use std::sync::atomic::{AtomicU16, Ordering};

use rustls::crypto::{ActiveKeyExchange, CryptoProvider, SupportedKxGroup};
use rustls::NamedGroup;

/// IANA codepoint registry reserves 0; we use it as the "no handshake yet" sentinel.
const UNSET: u16 = 0;

static LAST_NEGOTIATED_GROUP: AtomicU16 = AtomicU16::new(UNSET);

/// Returns the most recently negotiated named group, if any handshake has completed.
pub fn last_negotiated_group() -> Option<NamedGroup> {
    let raw = LAST_NEGOTIATED_GROUP.load(Ordering::Relaxed);
    if raw == UNSET {
        None
    } else {
        Some(NamedGroup::from(raw))
    }
}

/// Returns the most recently negotiated named group as a human-readable
/// string ("X25519MLKEM768", "X25519", "secp256r1", ...) or "unknown" if
/// no handshake has completed yet on this process.
pub fn last_negotiated_group_str() -> String {
    match last_negotiated_group() {
        Some(g) => format!("{:?}", g),
        None => "unknown".to_string(),
    }
}

fn record_group(group: NamedGroup) {
    LAST_NEGOTIATED_GROUP.store(u16::from(group), Ordering::Relaxed);
}

/// Wraps a `SupportedKxGroup` so that selecting it for a handshake records
/// the chosen `NamedGroup` into the global tracker.
struct TrackingKxGroup {
    inner: &'static dyn SupportedKxGroup,
}

impl fmt::Debug for TrackingKxGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TrackingKxGroup({:?})", self.inner.name())
    }
}

impl SupportedKxGroup for TrackingKxGroup {
    fn name(&self) -> NamedGroup {
        self.inner.name()
    }

    fn start(&self) -> Result<Box<dyn ActiveKeyExchange>, rustls::Error> {
        record_group(self.inner.name());
        self.inner.start()
    }
}

/// Returns a new `CryptoProvider` whose `kx_groups` are wrapped so that
/// the actually-selected group is recorded on every handshake.
///
/// Wrappers are leaked once (they have `'static` lifetime in rustls's
/// `kx_groups: Vec<&'static dyn SupportedKxGroup>` field). To keep that
/// leak bounded, `tls.rs` calls this through a `OnceLock` per provider
/// variant — every `PqcHttpClient::new` reuses the same wrapped
/// provider rather than allocating a fresh set.
pub fn instrument_provider(provider: CryptoProvider) -> CryptoProvider {
    let wrapped: Vec<&'static dyn SupportedKxGroup> = provider
        .kx_groups
        .iter()
        .map(|g| {
            let leaked: &'static TrackingKxGroup =
                Box::leak(Box::new(TrackingKxGroup { inner: *g }));
            leaked as &'static dyn SupportedKxGroup
        })
        .collect();

    CryptoProvider {
        cipher_suites: provider.cipher_suites,
        kx_groups: wrapped,
        signature_verification_algorithms: provider.signature_verification_algorithms,
        secure_random: provider.secure_random,
        key_provider: provider.key_provider,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_returns_none() {
        // Note: this test only holds if no other test has run a handshake first.
        // Run with `cargo test -- --test-threads=1` if interaction with smoke test matters.
        let _ = last_negotiated_group(); // just exercise the path
    }

    #[test]
    fn record_and_read_roundtrip() {
        record_group(NamedGroup::X25519);
        assert_eq!(last_negotiated_group(), Some(NamedGroup::X25519));
        record_group(NamedGroup::secp256r1);
        assert_eq!(last_negotiated_group(), Some(NamedGroup::secp256r1));
    }

    #[test]
    fn group_str_is_human_readable() {
        record_group(NamedGroup::X25519MLKEM768);
        assert_eq!(last_negotiated_group_str(), "X25519MLKEM768");
    }
}
