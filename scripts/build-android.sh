#!/usr/bin/env bash
# Build pqc_client for all Android ABIs and generate Kotlin bindings.
# Output:
#   target/jniLibs/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so
#   generated/kotlin/...        (UniFFI Kotlin bindings)
set -euo pipefail

: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to your Android NDK path}"

# Make sure ~/.cargo/bin is on PATH so uniffi-bindgen is found in fresh shells
# (CI runners and local shells that haven't sourced ~/.cargo/env).
if [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
fi

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
rm -rf generated/kotlin
mkdir -p generated/kotlin
cargo run --release --bin uniffi-bindgen -- generate \
    --language kotlin \
    --out-dir generated/kotlin \
    src/pqc.udl

echo
echo "Android build complete:"
echo "  Native libs:      $OUT_DIR/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so"
echo "  Kotlin bindings:  generated/kotlin/"
echo
echo "Next: package as AAR (see android/README.md)."
