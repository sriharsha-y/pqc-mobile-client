# android/ — Gradle library module

This directory is the Android Gradle library module that builds the AAR published to Maven Central as `io.github.sriharsha-y:pqc-mobile-client`.

**Consumer integration docs live elsewhere:**
- **Consuming the published AAR** (Maven Central / tarball / local Gradle): [`../docs/android.md`](../docs/android.md)
- **Hacking on this module / the Rust core**: [`../CLAUDE.md`](../CLAUDE.md)

What's in this directory:

- `build.gradle.kts` — AGP library module config + Maven Central publishing (vanniktech) + fat-AAR bundling of `rustls-platform-verifier`.
- `AndroidManifest.xml` — empty stub (AGP requires it for AAR assembly).
- `gradlew`, `gradle/wrapper/` — Pinned Gradle 8.7 wrapper (matches CI).
- `src/main/kotlin/uniffi/pqc/android/PqcAndroidInit.kt` — hand-written JNI bridge that hands the `Application` context to `rustls-platform-verifier` at process start.
- `libs/` — auto-extracted from the `rustls-platform-verifier-android` Cargo crate by `../scripts/build-android.sh`. Gitignored; do not edit.
- The native `.so` files and UniFFI Kotlin bindings are NOT in this directory — `build.gradle.kts` reads them from `../target/jniLibs/` and `../generated/kotlin/` via `sourceSets["main"]` overrides.

To build the AAR locally:

```bash
../scripts/build-android.sh   # cross-compile + extract rustls-pv jar
./gradlew assembleRelease     # produces build/outputs/aar/pqc-mobile-client-release.aar
```

The `preBuild` task will fail with a helpful error if you forgot the first step.
