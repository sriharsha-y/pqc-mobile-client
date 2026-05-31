# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this crate is

`pqc_client` is a Rust library that exposes a Post-Quantum TLS HTTPS client (X25519MLKEM768 hybrid, IANA `0x11EC`) to iOS 13.0+ and Android API 24+ via **UniFFI**. The floor is set by `rustls-platform-verifier` 0.5: on Android the AAR's `minSdk` is 22 (full revocation checking requires API ≥ 24, hence our 24 floor); on iOS the verifier uses `SecTrustEvaluateWithError` (iOS 12+), and we pin 13.0 for Swift/UniFFI tooling headroom. The same Rust core ships to six consumers (native iOS/Android, RN iOS/Android, with OkHttp/URLSession or direct API patterns) — only the call-site glue differs.

The crate compiles to `cdylib` + `staticlib` + `lib`. There is also a host-only `uniffi-bindgen` binary used during the build to generate Kotlin/Swift bindings from the crate's proc-macro definitions (read via `--library` mode from the compiled dylib — there is no `.udl` file).

## Common commands

Common tasks are unified under `make` — a thin dispatcher over `scripts/` +
cargo (run `make help` to list targets). Targets call the exact commands CI
runs, so `make check` reproduces the CI `check` job locally. The scripts under
`scripts/` remain the source of truth and stay runnable directly.

```bash
make setup        # one-time: rustup targets + cargo-ndk
make check        # fmt --check + clippy + test (mirrors the CI 'check' job)
make test         # unit + network smoke test (hits pq.cloudflareresearch.com)
make android      # cross-compile all ABIs → target/jniLibs/, Kotlin bindings → generated/kotlin/
make ios          # XCFramework → generated/PqcCore.xcframework, Swift bindings → generated/swift/
make build        # android + ios
make audit        # cargo-audit + cargo-deny (supply-chain)
make help         # list all targets
```

```bash
# Single smoke test (no make target). Smoke tests confirm the negotiated KEX
# via the server's /cdn-cgi/trace report (the `kex=` line), so they hold no
# shared client state and run in parallel.
cargo test --release --test smoke -- --nocapture <test_name>
```

`scripts/build-android.sh` requires `ANDROID_NDK_HOME`. `scripts/build-ios.sh` honors `IPHONEOS_DEPLOYMENT_TARGET` / `IPHONESIMULATOR_DEPLOYMENT_TARGET` (both default to 13.0, matching `PqcCore.podspec`'s `s.platform = :ios, '13.0'` and `.github/workflows/release.yml`'s pinned env — keep these three in sync when bumping the floor).

## The `cli` feature gate — do not break this

The `uniffi-bindgen` binary is declared with `required-features = ["cli"]` in `Cargo.toml`. This is load-bearing:

- Mobile cross-compiles (`cargo build --target aarch64-apple-ios`, `cargo ndk ...`) must run **without** `--features cli`. Otherwise `clap`, `goblin`, and the full `uniffi_bindgen` tree get linked into `libpqc_client.a` / `.so`, ballooning the iOS arm64 archive (today ~71 MiB with `cache`) and bloating the Android arm64-v8a `.so` (today ~5.5 MiB with `cache`).
- Generating bindings always uses `cargo run --release --features cli --bin uniffi-bindgen -- generate ...` on the **host**.
- When adding new code paths or examples, do not enable `cli` on default or on the library target.

## The `cache` feature — opt-in response cache

`cache` (off by default, like `cli`) compiles in the RFC 9111 response cache (`src/cache.rs`): the `http-cache`/`http-cache-semantics` stack + `cacache` (disk) + `moka` (iOS memory tier) + `postcard` (record serde). It's gated at compile time **and** at runtime (`PqcConfig.enable_cache`, also off by default). The mobile build scripts pass `--features cache` (override with `PQC_CARGO_FEATURES=`), so release artifacts ship it. Measured cost (release profile, `strip=true`, LTO): adds ~9 MiB to the iOS arm64 `.a` (61 → 71 MiB) but only ~1 MiB to the **linked** binary delta after `clang -dead_strip` (5.0 → 6.0 MiB) — the .a is bloated by bitcode metadata the linker discards. Android arm64-v8a `.so` grows ~0.8 MiB (4.7 → 5.5 MiB). Do **not** make it a `default` feature — the default/CI build must stay cache-free, and `--features cache` must never be combined with `cli`. The FFI surface is identical with or without it (`clear_cache`/`cache_size_bytes` are always exported, and inert when caching is off), so bindings are stable across both builds.

## Release profile / panic strategy

`[profile.release]` is tuned for mobile size (`opt-level = "z"`, LTO, `codegen-units = 1`, `strip = true`). `panic = "unwind"` is intentional — `panic = "abort"` would make panicking test assertions SIGABRT and report 0 failures with non-zero exit. See comment in `Cargo.toml`.

## Source layout

- `src/lib.rs` — module wiring + `uniffi::setup_scaffolding!("pqc")`.
- `src/android_init.rs` — JNI bridge (`Java_io_github_sriharsha_1y_pqc_android_PqcAndroidInit_nativeInit`; the `_1` is JNI escaping for the `_` in the `sriharsha_y` package segment) that hands the Android `Application` context to `rustls-platform-verifier`. iOS has no equivalent (Apple's `Security` framework is process-wide).
- The UniFFI API surface is defined **in Rust via proc-macros** (no `.udl` file): `#[derive(uniffi::Record)]` on the config/request/response types, `#[derive(uniffi::Enum)]` on `HttpMethod`/`RedirectPolicy`, `#[derive(uniffi::Error)]` + `#[uniffi(flat_error)]` on `PqcError`, and `#[derive(uniffi::Object)]` + `#[uniffi::export]` (with `#[uniffi::constructor]` on `new`, `async_runtime = "tokio"` on `request`) on `PqcHttpClient`. The Rust types are the source of truth for the Kotlin/Swift bindings. **NOTE: `PqcError` MUST keep `#[uniffi(flat_error)]`** — without it the generated error loses its `message` field (a silent breaking change for consumers).
- `src/client.rs` — `PqcHttpClient`, the reqwest wrapper. Holds an `HttpBackend` enum: `Plain(reqwest::Client)` by default, or `Cached(ClientWithMiddleware)` when the `cache` feature + `enable_cache` are on.
- `src/cache.rs` — (`cache` feature) `PqcCacheManager`, a private (`shared = false`), byte-bounded `cacache` disk tier + iOS `moka` memory tier, plugged into the `http-cache-reqwest` middleware. Cacheability is header/status/method-driven, never file-type. See the module doc for the deliberate divergences from native LRU.
- `src/tls.rs` — rustls + `rustls-post-quantum` + `rustls-platform-verifier` wiring; this is where the PQC group list is installed.
- `src/pinning.rs` — SPKI SHA-256 cert pinning layered on top of platform verifier.
- `src/config.rs`, `src/types.rs`, `src/error.rs` — `PqcConfig`, `HttpRequest`/`HttpResponse`/`HttpMethod`, `PqcError`.
- `tests/smoke.rs` — asserts a real handshake against `pq.cloudflareresearch.com` negotiates `X25519MLKEM768`, read from the server's `/cdn-cgi/trace` `kex=` report (server-authoritative; no client-side tracker).
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
