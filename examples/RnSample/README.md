# RnSample — pqc-mobile-client integration sample

A minimal React Native 0.76 app that wires `pqc_client` (Rust core via UniFFI) into RN's native HTTP stack on both platforms. On launch it issues a `fetch()` to `https://pq.cloudflareresearch.com/` — Cloudflare's research endpoint echoes the negotiated TLS key-exchange group in the response body. If everything's wired correctly the screen displays `kex = X25519MLKEM768`.

## What this proves

| | Coverage |
|---|---|
| RN JS `fetch()` flows through the Rust core | ✅ Both platforms |
| TLS handshake negotiates `X25519MLKEM768` | ✅ Verified end-to-end |
| Drop-in via `OkHttpClientProvider` (Android) | ✅ |
| Drop-in via `RCTSetCustomNSURLSessionConfigurationProvider` + `URLProtocol` (iOS) | ✅ |
| iOS 26+ bypass (lets native URLSession handle PQC) | ✅ Gated on `@available(iOS 26.0, *)` |

## Prerequisites

- Rust stable + targets installed (run repo-root `./scripts/setup.sh` once)
- Android SDK + NDK 27+ with `ANDROID_NDK_HOME` exported
- Xcode 16+ with command-line tools
- Node 22+, CocoaPods 1.16+

## One-shot setup

```bash
cd examples/RnSample
./scripts/wire-pqc.sh        # builds Rust core for Android + iOS
npm install
(cd ios && pod install)
```

## Run

```bash
# iOS simulator
npx react-native run-ios

# Android emulator (or attached device)
npx react-native run-android
```

The Metro bundler starts in a separate terminal automatically. The app shows a card with the HTTP status and an excerpt of the Cloudflare response body — look for `kex = X25519MLKEM768`.

## What's wired where

### Android — `OkHttpClientProvider` swap

- `android/app/build.gradle` adds `net.java.dev.jna:jna` and `kotlinx-coroutines-core` deps; points `jniLibs.srcDir` + `java.srcDir` at the repo's build outputs (`../../../../target/jniLibs` and `../../../../generated/kotlin`); and pulls the rustls-platform-verifier Kotlin glue via `fileTree("../../../../android/libs")` (extracted by `scripts/build-android.sh`). An `afterEvaluate` guard fails the build with a friendly error if those jars are missing — e.g. if you run `./gradlew` on a fresh checkout without `wire-pqc.sh`.
- `android/app/src/main/java/com/rnsample/MainApplication.kt` installs the factory in `onCreate()` **before** `super.onCreate()` — late install silently no-ops per [react-native#34789](https://github.com/facebook/react-native/issues/34789).
- `android/app/src/main/java/com/rnsample/PqcInterceptor.kt` adapts OkHttp's `Interceptor` contract to `PqcHttpClient.request()`. Must be the **last** interceptor; later ones never fire because the Rust core terminates the chain.
- `android/app/proguard-rules.pro` keeps `io.github.sriharsha_y.pqc.**`, JNA, and JNI methods so R8 doesn't strip them.

### iOS — `URLProtocol` interception

- `ios/Podfile` adds `pod 'PqcCore', :path => '../../../'` pointing at the repo-root `PqcCore.podspec`, which vendors the XCFramework + Swift binding. The podspec uses bare paths (`pqc.swift`, `PqcCore.xcframework`) so the published release zip works directly; for local `:path` mode, `scripts/build-ios.sh` materializes those bare names as repo-root symlinks into `generated/`. Run `scripts/build-ios.sh` (or `wire-pqc.sh`) before `pod install` or it will fail to find the sources.
- `ios/RnSample/PqcURLProtocol.swift` is the `URLProtocol` subclass. Sample intercepts every `https://`; a real app should narrow this to specific hostnames.
- `ios/RnSample/AppDelegate.mm` calls `RCTSetCustomNSURLSessionConfigurationProvider(...)` from `didFinishLaunchingWithOptions`. The provider returns a `URLSessionConfiguration` with `[PqcURLProtocol class]` prepended to `protocolClasses` — *except* on iOS 26+ where the native URLSession already negotiates PQC.

### JS

- `App.tsx` calls `fetch()` against Cloudflare's `/cdn-cgi/trace` endpoint and parses the `kex=` line the edge returns. A `Switch` toggles between this library's PQC client and the OS network stack by setting an `X-Pqc-Mode` request header: when `off`, the native layer falls through to OkHttp / URLSession instead of routing through the library. No cryptographic JS — the handshake is entirely native.

## Verification

The app reads the **server's** report of the negotiated key exchange, which is authoritative and correct even under concurrent requests (unlike a client-side header — see below):

- Toggle **on** → the trace shows `kex=X25519MLKEM768` (post-quantum hybrid negotiated).
- Toggle **off** → the request uses the OS network stack (OkHttp / URLSession), which doesn't offer the hybrid, so the trace shows `kex=X25519`.
- This works because `https://pq.cloudflareresearch.com/cdn-cgi/trace` (any Cloudflare-served host exposes `/cdn-cgi/trace`) returns the key exchange the edge actually negotiated for *that* connection, in the response body.
- On the wire: USB-tether the device, capture with Wireshark, filter `tls.handshake.type == 1`, inspect `key_share` extension for group `0x11EC` (IANA codepoint for `X25519MLKEM768`).

> **Why not a client-side header?** The negotiated group is a property of the TLS *connection*, not the request; the Rust core can only observe it via a process-global side-channel that races under concurrent requests. The server's `kex=` avoids that entirely. For an arbitrary backend with no trace endpoint, use server-side TLS observability (Cloudflare DataStream, AWS CloudTrail `tlsDetails.keyExchange`, etc.).

## Limitations

This sample intentionally elides things a real banking app needs:

- No cert pinning configured (the integration accepts an empty `pinnedCertSha256` list). See [`../../docs/android.md` Section 10](../../docs/android.md) and [`../../docs/ios.md` Section 10](../../docs/ios.md) for how to compute and configure pins.
- `PqcURLProtocol.swift` intercepts every `https://` URL — a real app should restrict to known API hostnames so 3rd-party SDKs and CDN traffic continue to use URLSession's native stack.
- WebViews (`react-native-webview`) and 3rd-party native SDKs (Firebase, Sentry, payment SDKs, etc.) bring their own HTTP stacks and are NOT covered by the swap. See [`../../README.md`](../../README.md) for the full coverage matrix.
