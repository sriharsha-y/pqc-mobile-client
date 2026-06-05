//! pqc_client — Post-Quantum TLS HTTPS client for React Native (iOS + Android).
//!
//! Wraps `reqwest` + `rustls` + `rustls-post-quantum` + `aws-lc-rs` and exposes
//! a small async API through UniFFI. Consumers call this from Kotlin (Android
//! OkHttp Interceptor) and Swift (iOS URLProtocol) — see ../docs/ for the
//! integration patterns.

mod client;
mod config;
mod error;
mod pinning;
mod tls;
mod types;

// Opt-in RFC 9111 response cache. Compiled only with the `cache` feature;
// the runtime `PqcConfig.enable_cache` flag gates it further. See src/cache.rs.
#[cfg(feature = "cache")]
mod cache;

// Android-only JNI shim that initializes rustls-platform-verifier with
// the Application Context. Must be called from MainApplication.onCreate
// before constructing PqcHttpClient — see src/android_init.rs.
#[cfg(target_os = "android")]
mod android_init;

pub use client::{PqcHttpClient, PqcResponse};
pub use config::{DnsResolver, PqcConfig, RedirectPolicy};
pub use error::PqcError;
pub use types::{HttpMethod, HttpRequest};

uniffi::setup_scaffolding!("pqc");
