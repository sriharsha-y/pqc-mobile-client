# Native Android example

A minimal **pure-native Android** app (Kotlin, no React Native) that calls
`PqcHttpClient` directly, opens a Post-Quantum TLS connection to
`pq.cloudflareresearch.com`, and reports the negotiated key-exchange group
from the server's `/cdn-cgi/trace` report.

One screen — a post-quantum toggle and a live result card, styled to match the
[React Native sample](../RnSample). The toggle drives `enablePostQuantum`, so
flipping it off makes the edge report `kex=X25519` (classical):

```
status = 200
alpn   = h2
kex    = X25519MLKEM768

✅ Post-quantum hybrid negotiated.
```

## How it consumes the library

Unlike the [Install](../../README.md#install) docs (which pull the published
Maven artifact), this sample wires up the **locally-built outputs** so you can
exercise a dev build of the crate. `app/build.gradle.kts` points its source
sets at the repo:

| What | Where it comes from |
|---|---|
| Native `.so` libs (`libpqc_client.so` + platform-verifier) | `../../target/jniLibs/` |
| UniFFI Kotlin bindings (`pqc.kt`) | `../../generated/kotlin/` |
| `PqcAndroidInit` JNI shim | `../../android/src/main/kotlin/` |
| `rustls-platform-verifier` Kotlin glue jar | `../../android/libs/` |
| JNA + kotlinx-coroutines | Maven Central |

This reconstructs exactly what the published AAR bundles. To consume the
**published** artifact instead, replace those source-set lines and the
`files(...)` dependency with a single
`implementation("io.github.sriharsha-y:pqc-mobile-client:<version>")`.

## Prerequisites

1. **Build the native outputs once** (from the repo root):

   ```bash
   make android
   ```

   This produces `target/jniLibs/`, `generated/kotlin/`, and
   `android/libs/rustls-platform-verifier-*.jar` — all `.gitignore`d, all
   consumed by this sample. Re-run it whenever you change the Rust crate.

2. **Android SDK.** Open this folder in Android Studio (it will offer to
   install the matching SDK/build-tools), or set `ANDROID_HOME` /
   `local.properties` (`sdk.dir=/path/to/Android/sdk`) for CLI builds.

## Build & run

**Android Studio:** open `examples/NativeAndroid`, pick a device/emulator,
press Run.

**Command line:**

```bash
cd examples/NativeAndroid
./gradlew installDebug      # build + install to a connected device/emulator
# or just assemble the APK:
./gradlew assembleDebug     # → app/build/outputs/apk/debug/app-debug.apk
```

> `arm64-v8a` covers modern physical devices; `x86_64` covers the standard
> emulator. Both slices are in `target/jniLibs/`.

## Files

```
NativeAndroid/
├── settings.gradle.kts
├── build.gradle.kts                  # AGP 8.5.0 / Kotlin 1.9.20 (matches the lib)
├── gradle.properties
└── app/
    ├── build.gradle.kts              # source-set wiring to local build outputs
    └── src/main/
        ├── AndroidManifest.xml       # INTERNET permission + SampleApplication
        ├── java/.../SampleApplication.kt   # calls PqcAndroidInit.init(this)
        └── java/.../MainActivity.kt        # the button + PQC verification
```

See [`docs/android.md`](../../docs/android.md) for the production integration
patterns (OkHttp interceptor, Retrofit/Ktor, cert pinning, ProGuard).
