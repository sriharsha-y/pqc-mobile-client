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
# We use --library not --udl so the bindgen sees BOTH UDL declarations
# AND proc-macro-exported (#[uniffi::export]) methods. The PqcHttpClient
# `request` method specifically lives in a proc-macro impl block in
# src/client.rs (annotated `async_runtime = "tokio"` so the FFI bridge
# drives reqwest/hyper on a real tokio runtime); --udl-only mode would
# miss it and the resulting Kotlin binding wouldn't expose `request`.
#
# --library mode needs a built dylib of the crate; build a host one.
# --features cli enables the uniffi-bindgen binary (gated by
# required-features in Cargo.toml). The cross-compile build above is
# intentionally feature-free so clap / goblin / uniffi_bindgen don't
# get linked into the .so / .a artifacts shipped to mobile.
#
# CRITICAL — the host dylib MUST NOT be stripped (mozilla/uniffi-rs#2520):
# our [profile.release] sets `strip = true` for mobile artifact size, but
# `uniffi-bindgen --library` discovers the interface by reading the
# UNIFFI_META_* symbols out of the dylib's regular symbol table (.symtab).
# On Linux, `strip` removes those from .symtab (leaving them only in
# .dynsym, which bindgen does NOT read), so bindgen SILENTLY generates
# zero bindings (exit 0, empty out-dir) and the resulting AAR ships with
# no Kotlin API. macOS `strip` keeps the symbols, so this only bites Linux
# CI — which is exactly why it shipped undetected for several releases.
# Override strip=false for THIS host build only; the mobile cargo-ndk .so
# above were already built (stripped) before this point and are unaffected.
# The export must also cover the `cargo run` bindgen invocation below, or
# it would rebuild a stripped host dylib and re-trigger the bug.
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

# Fail-fast guard against a silent regression of the strip bug above (or
# any other cause of empty --library output). uniffi-bindgen exits 0 even
# when it finds no UNIFFI_META symbols and writes nothing — a zero-binding
# generated/kotlin compiles into an AAR with no Kotlin API and was shipped
# to Maven Central undetected. Refuse to continue if no .kt was produced.
if [ -z "$(find generated/kotlin -name '*.kt' -print -quit)" ]; then
    echo "::error::uniffi-bindgen produced ZERO Kotlin bindings in generated/kotlin/."
    echo "::error::Almost certainly the uniffi-rs#2520 strip bug — the host dylib lost its"
    echo "::error::UNIFFI_META_* symbols. Confirm CARGO_PROFILE_RELEASE_STRIP=false is in"
    echo "::error::effect for the host build above (nm \$HOST_DYLIB | grep UNIFFI_META)."
    exit 1
fi

echo "==> Extracting rustls-platform-verifier Kotlin glue into android/libs/"
# rustls-platform-verifier ships its Android-side classes (the
# org.rustls.platformverifier.* helpers that the Rust verifier JNIs
# into) as a Maven AAR vendored inside the rustls-platform-verifier-
# android crate. Consumers of OUR published AAR cannot reach that
# private Maven layout, so we extract the upstream classes.jar and
# stage it under android/libs/ — AGP will then bundle it into our
# AAR's libs/ directory, making our Maven Central publication
# self-contained (no extra repository declarations on the consumer).
#
# Locating the crate: ask cargo metadata for the resolved path of
# `rustls-platform-verifier-android` and join with the documented
# maven/ subpath shipped by the crate.
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
