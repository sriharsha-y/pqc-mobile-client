# pqc-mobile-client

[![check](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/check.yml/badge.svg?branch=main)](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/check.yml)
[![android](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/android.yml/badge.svg?branch=main)](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/android.yml)
[![ios](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/ios.yml/badge.svg?branch=main)](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/ios.yml)
[![release](https://github.com/sriharsha-y/pqc-mobile-client/actions/workflows/release.yml/badge.svg)](https://github.com/sriharsha-y/pqc-mobile-client/releases)

Post-Quantum TLS HTTPS client for mobile apps — **iOS 15.1+** and **Android API 29+**. Single Rust core built on `rustls` + `rustls-post-quantum` + `aws-lc-rs` + `reqwest`, exposed to Kotlin and Swift via UniFFI.

Designed for any mobile app — **native iOS (Swift/Obj-C), native Android (Kotlin/Java), or React Native** — that needs to negotiate `X25519MLKEM768` against PQC-enabled servers (Akamai, Cloudflare, AWS) on OS versions that don't have native PQC TLS yet.

> Companion design docs: [`PQC_MOBILE_ANALYSIS.md`](../commshub/PQC_MOBILE_ANALYSIS.md), [`PQC_RN_IMPLEMENTATION.md`](../commshub/PQC_RN_IMPLEMENTATION.md).

## Why this exists

Akamai's edge has hybrid PQC TLS (`X25519MLKEM768`, IANA codepoint `0x11EC`) enabled. iOS 26+ and Chrome already negotiate it by default, but:

- **iOS 15.1–18:** no native PQC TLS. `URLSession`'s TLS engine is closed; ATS doesn't expose group selection.
- **Android API 29+:** system Conscrypt does not advertise `X25519MLKEM768` by default on any shipped Android release (incl. Android 17). Google has not committed to a default-on date.
- **Cronet** ships PQC via Chrome's BoringSSL, but the only Maven-published artifact that includes PQC requires Google Play Services (no GMS → no PQC). The `cronet-embedded` Maven artifact is frozen at Chromium 113 (pre-PQC).

This crate provides a **unified, single-codebase, FIPS-validated** alternative that works on every supported OS version and on every Android device regardless of GMS availability.

## Who consumes this

| Consumer | Pattern | Doc |
|---|---|---|
| Native Android app (Kotlin/Java) using OkHttp / Retrofit / Ktor | Custom `Interceptor` that delegates to `PqcHttpClient` | [`android/README.md`](android/README.md) §3 |
| Native Android app using `HttpURLConnection` or no framework | Call `PqcHttpClient` directly from Kotlin/Java | [`android/README.md`](android/README.md) §6 |
| React Native Android app | Same `Interceptor` installed via `OkHttpClientProvider.setOkHttpClientFactory` | [`android/README.md`](android/README.md) §5 |
| Native iOS app using `URLSession` | Register `PqcURLProtocol` on the session config | [`ios/README.md`](ios/README.md) §3 |
| Native iOS app using a custom HTTP client (AsyncHTTPClient, etc.) | Call `PqcHttpClient` directly from Swift/Obj-C | [`ios/README.md`](ios/README.md) §5 |
| React Native iOS app | `PqcURLProtocol` registered via `RCTSetCustomNSURLSessionConfigurationProvider` | [`ios/README.md`](ios/README.md) §6 |

The Rust core, Kotlin bindings, and Swift bindings are **identical** across all six consumers. Only the integration glue at the call site differs.

## Architecture

```
   Consumer app (any of the six above)
                 │
        ┌────────┴────────┐
        ▼                 ▼
  Kotlin bindings   Swift bindings
   (UniFFI)          (UniFFI)
        │                 │
   libpqc_client.so    PqcCore.xcframework
        └────────┬────────┘
                 ▼
   ┌─────────────────────────────────────┐
   │   pqc_client (this crate)             │
   │   reqwest ─ hyper ─ rustls          │
   │   rustls-post-quantum (X25519MLKEM768)
   │   aws-lc-rs (FIPS 140-3)            │
   │   rustls-platform-verifier          │
   │   tokio                             │
   └─────────────────────────────────────┘
```

## Layout

```
pqc-mobile-client/
├── Cargo.toml              Rust crate manifest
├── rust-toolchain.toml     Pinned Rust toolchain + cross-compile targets
├── build.rs                UniFFI scaffolding generation
├── src/
│   ├── lib.rs              UniFFI entry point
│   ├── pqc.udl             UniFFI interface (generates Kotlin + Swift bindings)
│   ├── client.rs           PqcHttpClient implementation (wraps reqwest)
│   ├── config.rs           Configuration types
│   ├── tls.rs              rustls + PQC + platform-verifier wiring
│   ├── error.rs            PqcError enum
│   └── types.rs            HttpRequest / HttpResponse / HttpMethod
├── tests/
│   └── smoke.rs            Network smoke test against Cloudflare PQ endpoint
├── scripts/
│   ├── setup.sh            One-time developer setup
│   ├── build-android.sh    Cross-compile to all Android ABIs + Kotlin bindings
│   └── build-ios.sh        Build XCFramework + Swift bindings
├── android/README.md       Consumption guide (native + React Native)
├── ios/README.md           Consumption guide (native + React Native)
└── docs/                   (populate as design evolves)
```

## Quick start

```bash
./scripts/setup.sh                 # one-time: rust targets, cargo-ndk, uniffi-bindgen
cargo test -- --nocapture          # sanity-test against pq.cloudflareresearch.com
./scripts/build-android.sh         # cross-compile all Android ABIs + Kotlin bindings
./scripts/build-ios.sh             # build XCFramework + Swift bindings
```

## Releases

Releases are driven by [release-please](https://github.com/googleapis/release-please) from **conventional commits** — see [CONTRIBUTING.md](CONTRIBUTING.md) for the commit message format.

The flow:

1. Land conventional commits on `main`:
   - `feat: …` bumps minor
   - `fix: …` bumps patch
   - `feat!: …` bumps major
   - `chore:` / `ci:` / `docs:` / `refactor:` do not trigger a release
2. The `release` workflow opens (and continuously updates) a PR titled `chore(main): release X.Y.Z` containing:
   - the version bump in `Cargo.toml`
   - the new `CHANGELOG.md` entries grouped by type (Features / Bug Fixes / etc.)
3. Review and merge that PR when ready to cut a release.
4. release-please then:
   - tags the merge commit as `vX.Y.Z`
   - creates a [GitHub Release](https://github.com/sriharsha-y/pqc-mobile-client/releases) at the tag with the CHANGELOG entries as the body
5. The same workflow's downstream jobs build Android + iOS artifacts and attach them as release assets:
   - `pqc-mobile-client-<version>-android.tar.gz` (`.so` files + Kotlin bindings)
   - `pqc-mobile-client-<version>-ios.zip` (XCFramework + Swift bindings)

No manual tagging required. The `CHANGELOG.md` lives in-repo and is maintained automatically.

## What this covers

| Capability | Status |
|---|---|
| HTTP/1.1, HTTP/2 (via reqwest + hyper) | ✅ |
| HTTP/3 / QUIC | ⏳ Opt-in via cargo feature, not enabled by default |
| Hybrid PQC TLS (`X25519MLKEM768`) | ✅ Default |
| Classical fallback (X25519, P-256) | ✅ Automatic |
| System trust store (iOS Keychain / Android KeyStore) | ✅ Via `rustls-platform-verifier` |
| Cert pinning (SPKI SHA-256) | ✅ Layered on top of platform verifier; empty pin list disables |
| Cookies | ✅ `reqwest` cookie store |
| gzip / brotli decompression | ✅ |
| Redirects | ✅ |
| Connection pooling | ✅ |
| Timeouts (connect / read / total) | ✅ |
| Cancellation | ✅ Via UniFFI async + tokio |
| All HTTP methods | ✅ GET, POST, PUT, DELETE, PATCH, HEAD, OPTIONS |
| Negotiated TLS group reporting on `HttpResponse` | ✅ Via instrumented `CryptoProvider` (see `src/kx_tracker.rs`) |
| Android GMS + non-GMS devices | ✅ |
| iOS 15.1 – 18 | ✅ |
| iOS 26+ | ✅ (skip via `#available` and let native URLSession negotiate PQC) |

## What this does NOT cover

- **WebViews** (`WKWebView` on iOS, system WebView on Android) — use system network stack, not interceptable.
- **iOS background URLSession** (resumable uploads while app is suspended) — OS daemon owns the socket.
- **RN `<Image>` / Fresco / SDWebImage / Glide** — own HTTP loaders. Acceptable for non-sensitive image CDN traffic.
- **RN iOS WebSocket (`SRWebSocket`)** — CFStream-based, not URLSession.
- **3rd-party native SDKs** (Firebase, Sentry, Razorpay, AppsFlyer, etc.) — bundle their own HTTP clients.
- **Streaming bodies > a few MB** — possible but needs ~300 LOC of FFI plumbing; not in MVP.

## Dependencies

| Crate | Version | Purpose |
|---|---|---|
| `reqwest` | 0.12 | HTTP client (redirects, cookies, gzip, brotli, pooling, HTTP/2) |
| `rustls` | 0.23 | Pure-Rust TLS 1.3 stack |
| `rustls-post-quantum` | 0.2 | Adds `X25519MLKEM768` to default group list |
| `rustls-platform-verifier` | 0.5 | Defers cert validation to OS trust store |
| `aws-lc-rs` | 1.13 | FIPS 140-3 validated crypto provider (also used for SPKI SHA-256) |
| `x509-parser` | 0.16 | Extract SPKI bytes from server cert for pinning |
| `base64` | 0.22 | Decode user-supplied pin hashes |
| `tokio` | 1 | Async runtime |
| `uniffi` | 0.29 | Generates Kotlin + Swift bindings from `src/pqc.udl` |

## Status

**Baseline verified.** Crate compiles, all unit tests pass (11), and the smoke test against `pq.cloudflareresearch.com` returns `200` with `X25519MLKEM768` negotiated *and verified* (the smoke test now asserts on the actual negotiated group, not a hardcoded constant). Verified on Rust stable `1.95`, macOS host. Integration recipes documented for native and React Native on both platforms; cross-compile scripts ready but not yet exercised in CI.

Outstanding items:
- Sample RN app exercising the integration end-to-end.

## License

Apache-2.0 (matches dependencies).
