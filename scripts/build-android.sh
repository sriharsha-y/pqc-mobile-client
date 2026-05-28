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

echo "==> Generating Kotlin bindings (via --library mode)"
# --library (not --udl) so bindgen sees the proc-macro-exported `request`
# method too, not just UDL declarations. It reads a host-built dylib;
# --features cli enables the uniffi-bindgen binary. The cross-compile above
# is feature-free so clap/goblin/uniffi_bindgen don't bloat the mobile .so.
#
# CRITICAL — host dylib must NOT be stripped (mozilla/uniffi-rs#2520).
# `uniffi-bindgen --library` reads UNIFFI_META_* symbols from .symtab, but
# our `strip = true` (release) removes them on Linux, so bindgen silently
# emits zero bindings and the AAR ships no Kotlin API (macOS strip keeps
# them, hiding this). Disable strip for the host build only; the mobile
# cargo-ndk .so above are already built (stripped) and unaffected. Must
# also cover the `cargo run` below, else it rebuilds a stripped dylib.
export CARGO_PROFILE_RELEASE_STRIP=false
cargo build --release --features cli
HOST_DYLIB="target/release/libpqc_client.dylib"
if [ ! -f "$HOST_DYLIB" ]; then
    # Linux CI runners produce a .so instead.
    HOST_DYLIB="target/release/libpqc_client.so"
fi
if [ ! -f "$HOST_DYLIB" ]; then
    echo "::error::Expected host dynamic library at target/release/libpqc_client.{dylib,so}."
    exit 1
fi

rm -rf generated/kotlin
mkdir -p generated/kotlin
cargo run --release --features cli --bin uniffi-bindgen -- generate \
    --language kotlin \
    --out-dir generated/kotlin \
    --library "$HOST_DYLIB"

# Fail-fast if bindgen wrote nothing (it exits 0 on empty output) — a
# zero-binding generated/kotlin yields an AAR with no Kotlin API.
if [ -z "$(find generated/kotlin -name '*.kt' -print -quit)" ]; then
    echo "::error::uniffi-bindgen produced ZERO Kotlin bindings in generated/kotlin/."
    echo "::error::Almost certainly the uniffi-rs#2520 strip bug — the host dylib lost its"
    echo "::error::UNIFFI_META_* symbols. Confirm CARGO_PROFILE_RELEASE_STRIP=false is in"
    echo "::error::effect for the host build above (nm \$HOST_DYLIB | grep UNIFFI_META)."
    exit 1
fi

echo "==> Extracting rustls-platform-verifier Kotlin glue into android/libs/"
# rustls-platform-verifier ships its Android classes only as a Maven AAR
# vendored inside the -android crate, which consumers of our AAR can't
# reach. Extract its classes.jar into android/libs/ so AGP bundles it into
# our AAR (self-contained, no extra repos for consumers). Locate the crate
# via cargo metadata.
PV_INFO=$(cargo metadata --format-version 1 \
  | python3 -c '
import json, sys
m = json.load(sys.stdin)
for p in m["packages"]:
    if p["name"] == "rustls-platform-verifier-android":
        print(p["manifest_path"])
        print(p["version"])
        sys.exit(0)
sys.exit("rustls-platform-verifier-android not found in cargo metadata")
')
PV_MANIFEST=$(echo "$PV_INFO" | sed -n '1p')
PV_VERSION=$(echo "$PV_INFO" | sed -n '2p')
PV_DIR=$(dirname "$PV_MANIFEST")
PV_AAR="$PV_DIR/maven/rustls/rustls-platform-verifier/$PV_VERSION/rustls-platform-verifier-$PV_VERSION.aar"
if [ ! -f "$PV_AAR" ]; then
    echo "::error::Expected upstream AAR at $PV_AAR"
    exit 1
fi

mkdir -p android/libs
rm -f android/libs/rustls-platform-verifier-*.jar
TMP=$(mktemp -d)
unzip -q -o "$PV_AAR" classes.jar -d "$TMP"
mv "$TMP/classes.jar" "android/libs/rustls-platform-verifier-$PV_VERSION.jar"
rm -rf "$TMP"
echo "  Wrote android/libs/rustls-platform-verifier-$PV_VERSION.jar"

echo
echo "Android build complete:"
echo "  Native libs:      $OUT_DIR/{arm64-v8a,armeabi-v7a,x86_64}/libpqc_client.so"
echo "  Kotlin bindings:  generated/kotlin/"
echo "  Vendored glue:    android/libs/rustls-platform-verifier-$PV_VERSION.jar"
echo
echo "Next: package as AAR (see docs/android.md)."
