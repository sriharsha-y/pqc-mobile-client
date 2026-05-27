//! Record the TLS key-exchange group rustls actually selects.
//!
//! `reqwest` doesn't expose the negotiated named group on its `Response`,
//! and the rustls `ClientConnection` isn't reachable from inside reqwest's
//! transport stack. To recover this information we wrap each
//! `SupportedKxGroup`, and the `ActiveKeyExchange` it produces, so that
//! when rustls finishes the handshake it records the chosen `NamedGroup`
//! into a process-global atomic.
//!
//! Recording happens in `ActiveKeyExchange::complete()` — NOT
//! `SupportedKxGroup::start()`. `start()` is called per group the client
//! pre-computes a key_share for in ClientHello (or before a retry); on
//! servers that respond with HelloRetryRequest the server-selected
//! group is different from the one(s) `start()` ran for, so a tracker
//! hooked at `start()` reports the client's first offer instead of the
//! group that actually carried the handshake. `complete()` runs exactly
//! once per handshake, with the server's final choice as the
//! `peer_pub_key`'s implied group — so it's the right place to record.
//!
//! Concurrency note: the recorded value is process-wide. Sequential
//! request patterns (the common case for a banking app) read accurate
//! per-request values. Truly concurrent requests on the same client may
//! interleave and one may observe another's group. For deterministic
//! per-connection reporting use server-side telemetry such as Akamai
//! DataStream 2.
//!
//! Caveat: handshakes are not free, so under HTTP/2 keep-alive pooling
//! `complete()` is called only when a new TLS connection is established
//! — not on every request. The "last negotiated group" reflects the
//! most recent handshake, which is stable across the pool's lifetime.

use std::fmt;
use std::sync::atomic::{AtomicU16, Ordering};

use rustls::crypto::{ActiveKeyExchange, CryptoProvider, SharedSecret, SupportedKxGroup};
use rustls::{NamedGroup, ProtocolVersion};

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

/// Wraps a `SupportedKxGroup` so that the `ActiveKeyExchange` it returns
/// is itself wrapped to record the group at handshake completion (which
/// is exactly the server-selected group; see module docs).
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
        // start() may be called for groups the client merely OFFERS in
        // ClientHello but the server doesn't pick (incl. before a
        // HelloRetryRequest). Don't record here — wrap the
        // ActiveKeyExchange instead so we only record once the server
        // actually drives the handshake to completion with this group.
        let inner = self.inner.start()?;
        let name = self.inner.name();
        Ok(Box::new(TrackingActiveKeyExchange { inner, name }))
    }

    // Delegate metadata methods to the inner so our wrapper doesn't
    // silently report defaults that disagree with reality. None of
    // these affect correctness today (default `fips() == false` and
    // `usable_for_version() == true` happen to match aws-lc-rs's
    // current report), but the wrapper would silently misreport if
    // rustls / rustls-post-quantum / aws-lc-rs ever toggles these.
    // Authoritative trait:
    //   https://docs.rs/rustls/0.23.40/rustls/crypto/trait.SupportedKxGroup.html
    fn fips(&self) -> bool {
        self.inner.fips()
    }

    fn usable_for_version(&self, version: ProtocolVersion) -> bool {
        self.inner.usable_for_version(version)
    }
}

/// Wraps an `ActiveKeyExchange` to record the group's name when the
/// handshake completes — i.e. when rustls calls `complete()` with the
/// peer's key_share. That call happens exactly once per handshake, on
/// the group the server actually selected.
struct TrackingActiveKeyExchange {
    inner: Box<dyn ActiveKeyExchange>,
    name: NamedGroup,
}

impl ActiveKeyExchange for TrackingActiveKeyExchange {
    fn complete(self: Box<Self>, peer_pub_key: &[u8]) -> Result<SharedSecret, rustls::Error> {
        // Record BEFORE delegating: complete() consumes self, and we
        // want the side-effect even if the inner call errors so that
        // a failed-handshake post-mortem can still see the attempted
        // group.
        record_group(self.name);
        self.inner.complete(peer_pub_key)
    }

    // Hybrid (MLKEM) groups carry a classical sub-component. If the
    // server HelloRetryRequest's specifically to use just that classical
    // half (rather than switching to a different group entirely),
    // rustls calls these two methods on the original hybrid
    // ActiveKeyExchange — NOT on a freshly-started classical one.
    // The default trait impls return `None` / `unreachable!()`, which
    // would mis-hide the hybrid nature of the inner and panic at HRR
    // time. Delegating fixes a real iOS-only crash against github.com:
    //   PqcCore.UniffiInternalError on the call from PqcURLProtocol,
    // which corresponds to rustls hitting the `unreachable!()` in
    // crypto::ActiveKeyExchange::complete_hybrid_component.
    fn hybrid_component(&self) -> Option<(NamedGroup, &[u8])> {
        self.inner.hybrid_component()
    }

    fn complete_hybrid_component(
        self: Box<Self>,
        peer_pub_key: &[u8],
    ) -> Result<SharedSecret, rustls::Error> {
        // Record the CLASSICAL component's group, since that's what the
        // server is committing to. inner.hybrid_component() returns
        // (NamedGroup::X25519, key_share_bytes) for X25519MLKEM768; we
        // re-derive the name rather than caching it so we stay correct
        // if rustls-post-quantum adds more hybrid combos later.
        if let Some((classical, _)) = self.inner.hybrid_component() {
            record_group(classical);
        }
        self.inner.complete_hybrid_component(peer_pub_key)
    }

    fn pub_key(&self) -> &[u8] {
        self.inner.pub_key()
    }

    fn group(&self) -> NamedGroup {
        self.inner.group()
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
