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

// Android-only JNI shim that initializes rustls-platform-verifier with
// the Application Context. Must be called from MainApplication.onCreate
// before constructing PqcHttpClient — see src/android_init.rs.
#[cfg(target_os = "android")]
mod android_init;

pub use client::PqcHttpClient;
pub use config::{PqcConfig, RedirectPolicy};
pub use error::PqcError;
pub use types::{HttpMethod, HttpRequest, HttpResponse};

uniffi::include_scaffolding!("pqc");
