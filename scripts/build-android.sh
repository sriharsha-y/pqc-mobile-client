#!/usr/bin/env bash
# Build pqc_client for all Android ABIs and generate Kotlin bindings.
# Output:
#   target/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so
#   generated/kotlin/...        (UniFFI Kotlin bindings)
set -euo pipefail

: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to your Android NDK path}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OUT_DIR="target/jniLibs"
mkdir -p "$OUT_DIR"

echo "==> Cross-compiling for arm64-v8a, armeabi-v7a, x86_64"
cargo ndk \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -o "$OUT_DIR" \
    build --release

echo "==> Generating Kotlin bindings"
mkdir -p generated/kotlin
uniffi-bindgen generate \
    --library "target/aarch64-linux-android/release/libpqc_client.so" \
    --language kotlin \
    --out-dir generated/kotlin

echo
echo "Android build complete:"
echo "  Native libs:      $OUT_DIR/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so"
echo "  Kotlin bindings:  generated/kotlin/"
echo
echo "Next: package as AAR (see android/README.md)."
