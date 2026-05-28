# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this crate is

`pqc_client` is a Rust library that exposes a Post-Quantum TLS HTTPS client (X25519MLKEM768 hybrid, IANA `0x11EC`) to iOS 13.0+ and Android API 24+ via **UniFFI**. The floor is set by `rustls-platform-verifier` 0.5: on Android the AAR's `minSdk` is 22 (full revocation checking requires API ≥ 24, hence our 24 floor); on iOS the verifier uses `SecTrustEvaluateWithError` (iOS 12+), and we pin 13.0 for Swift/UniFFI tooling headroom. The same Rust core ships to six consumers (native iOS/Android, RN iOS/Android, with OkHttp/URLSession or direct API patterns) — only the call-site glue differs.

The crate compiles to `cdylib` + `staticlib` + `lib`. There is also a host-only `uniffi-bindgen` binary used during the build to generate Kotlin/Swift bindings from `src/pqc.udl`.

## Common commands

```bash
./scripts/setup.sh                              # one-time: rustup targets + cargo-ndk
cargo test --release -- --nocapture            # unit + network smoke test (hits pq.cloudflareresearch.com)
cargo test --release --test smoke -- --nocapture <test_name>   # single test
# Smoke tests confirm the negotiated KEX via the server's /cdn-cgi/trace
# report (the `kex=` line), so they hold no shared client state and run in parallel.
cargo fmt && cargo clippy --all-targets -- -D warnings
./scripts/build-android.sh                      # cross-compile all ABIs → target/jniLibs/, Kotlin bindings → generated/kotlin/
./scripts/build-ios.sh                          # XCFramework → generated/PqcCore.xcframework, Swift bindings → generated/swift/
```

`scripts/build-android.sh` requires `ANDROID_NDK_HOME`. `scripts/build-ios.sh` honors `IPHONEOS_DEPLOYMENT_TARGET` / `IPHONESIMULATOR_DEPLOYMENT_TARGET` (both default to 13.0, matching `PqcCore.podspec`'s `s.platform = :ios, '13.0'` and `.github/workflows/release.yml`'s pinned env — keep these three in sync when bumping the floor).

## The `cli` feature gate — do not break this

The `uniffi-bindgen` binary is declared with `required-features = ["cli"]` in `Cargo.toml`. This is load-bearing:

- Mobile cross-compiles (`cargo build --target aarch64-apple-ios`, `cargo ndk ...`) must run **without** `--features cli`. Otherwise `clap`, `goblin`, and the full `uniffi_bindgen` tree get linked into `libpqc_client.a` / `.so`, ballooning the iOS arm64 archive from ~50 MB to ~90 MB.
- Generating bindings always uses `cargo run --release --features cli --bin uniffi-bindgen -- generate ...` on the **host**.
- When adding new code paths or examples, do not enable `cli` on default or on the library target.

## Release profile / panic strategy

`[profile.release]` is tuned for mobile size (`opt-level = "z"`, LTO, `codegen-units = 1`, `strip = true`). `panic = "unwind"` is intentional — `panic = "abort"` would make panicking test assertions SIGABRT and report 0 failures with non-zero exit. See comment in `Cargo.toml`.

## Source layout

- `src/lib.rs` — module wiring + `uniffi::include_scaffolding!("pqc")`.
- `src/android_init.rs` — JNI bridge (`Java_uniffi_pqc_android_PqcAndroidInit_nativeInit`) that hands the Android `Application` context to `rustls-platform-verifier`. iOS has no equivalent (Apple's `Security` framework is process-wide).
- `src/pqc.udl` — UniFFI interface; **this is the source of truth for the Kotlin/Swift API surface**. Changes here regenerate bindings.
- `src/client.rs` — `PqcHttpClient`, the reqwest wrapper.
- `src/tls.rs` — rustls + `rustls-post-quantum` + `rustls-platform-verifier` wiring; this is where the PQC group list is installed.
- `src/pinning.rs` — SPKI SHA-256 cert pinning layered on top of platform verifier.
- `src/config.rs`, `src/types.rs`, `src/error.rs` — `PqcConfig`, `HttpRequest`/`HttpResponse`/`HttpMethod`, `PqcError`.
- `tests/smoke.rs` — asserts a real handshake against `pq.cloudflareresearch.com` reports `X25519MLKEM768` as the negotiated group (not a hardcoded constant — it reads the tracker output).
- `examples/RnSample/` — runnable React Native sample exercising both the Android OkHttp interceptor path and the iOS `PqcURLProtocol` path.
- `docs/android.md`, `docs/ios.md` — consumer integration guides.
- `PqcCore.podspec` — local CocoaPod that vendors the iOS XCFramework for RN consumption.

## Releases — release-please via conventional commits

Releases are fully automated by [release-please](https://github.com/googleapis/release-please) from conventional commits on `main`. **Do not bump `Cargo.toml` versions or tag manually.** Workflow:

- `feat:` → minor bump, `fix:` → patch, `feat!:`/`fix!:` → major, others (`docs:`, `chore:`, `ci:`, `refactor:`, `test:`, `build:`, `perf:`) → no release.
- The release workflow opens/updates a PR titled `chore(main): release X.Y.Z`. Merging it tags `vX.Y.Z`, cuts the GitHub Release, and attaches `pqc-mobile-client-<version>-android.tar.gz` + `PqcCore-<version>.zip` (the latter is the CocoaPods/SPM-shaped iOS asset).
- **Commits must not carry AI/model attribution** (no `Co-Authored-By: Claude …`, no "Generated with …" trailers).

## CI

`.github/workflows/` has `check.yml` (fmt/clippy/test), `android.yml`, `ios.yml`, and `release.yml`. The Android and iOS workflows exercise the cross-compile scripts; recent commits pin `IPHONEOS_DEPLOYMENT_TARGET` and guard against the `cli`-feature size regression — keep both invariants intact when touching CI or build scripts.
