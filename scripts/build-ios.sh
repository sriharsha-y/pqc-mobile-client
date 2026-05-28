#!/usr/bin/env bash
# Build pqc_client for iOS (device + simulator), generate Swift bindings,
# and assemble PqcCore.xcframework.
# Output:
#   generated/PqcCore.xcframework
#   generated/swift/...         (UniFFI Swift bindings)
set -euo pipefail

# Make sure ~/.cargo/bin is on PATH so uniffi-bindgen is found in fresh shells.
if [ -f "${HOME}/.cargo/env" ]; then
    # shellcheck disable=SC1091
    source "${HOME}/.cargo/env"
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Match the consumer app's minimum deployment target. Both cc-rs (used by
# aws-lc-sys to build C objects) and rustc's linker read these vars; if
# they disagree, the linker errors with "object file built for newer iOS
# version than being linked" and __chkstk_darwin and friends go undefined.
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"
# Simulator builds use this independent target var.
export IPHONESIMULATOR_DEPLOYMENT_TARGET="${IPHONESIMULATOR_DEPLOYMENT_TARGET:-13.0}"

echo "==> IPHONEOS_DEPLOYMENT_TARGET=$IPHONEOS_DEPLOYMENT_TARGET"

echo "==> Building for iOS device arm64"
cargo build --release --target aarch64-apple-ios

echo "==> Building for iOS simulator (arm64 + x86_64)"
cargo build --release --target aarch64-apple-ios-sim
cargo build --release --target x86_64-apple-ios

echo "==> Combining simulator slices via lipo"
mkdir -p target/ios-sim
# Rust's iOS-simulator target naming is asymmetric: arm64 uses
# `-sim` suffix, x86_64 doesn't (it's just `x86_64-apple-ios`).
lipo -create \
    target/aarch64-apple-ios-sim/release/libpqc_client.a \
    target/x86_64-apple-ios/release/libpqc_client.a \
    -output target/ios-sim/libpqc_client.a

echo "==> Generating Swift bindings (via --library mode)"
# We use --library not --udl so the bindgen sees BOTH UDL declarations
# AND proc-macro-exported (#[uniffi::export]) methods. The PqcHttpClient
# `request` method specifically lives in a proc-macro impl block in
# src/client.rs (annotated `async_runtime = "tokio"` so the FFI bridge
# drives reqwest/hyper on a real tokio runtime); --udl-only mode would
# miss it and the resulting Swift binding wouldn't expose `request`.
#
# --library mode needs a built dylib of the crate; build a host one.
cargo build --release --features cli
HOST_DYLIB="target/release/libpqc_client.dylib"
if [ ! -f "$HOST_DYLIB" ]; then
    echo "::error::Expected host dylib at $HOST_DYLIB after cargo build."
    exit 1
fi

rm -rf generated/swift
mkdir -p generated/swift
cargo run --release --features cli --bin uniffi-bindgen -- generate \
    --language swift \
    --out-dir generated/swift \
    --library "$HOST_DYLIB"

# Fail-fast if bindgen silently wrote nothing (it exits 0 on an empty
# interface). No strip override is needed here, unlike build-android.sh:
# this path is macOS-only and macOS strip keeps the UNIFFI_META symbols
# bindgen reads (mozilla/uniffi-rs#2520 only bites Linux).
if [ ! -s generated/swift/pqc.swift ]; then
    echo "::error::uniffi-bindgen produced no Swift binding (generated/swift/pqc.swift missing or empty)."
    exit 1
fi

# UniFFI emits {pqc.swift, pqcFFI.h, pqcFFI.modulemap}.
# The XCFramework needs the .h + a `module.modulemap` inside its Headers/
# directory of each slice. We rename pqcFFI.modulemap to module.modulemap
# so Xcode/SourceKit find a Swift module called "pqcFFI" backed by the C header.
HEADERS_DIR="generated/headers"
rm -rf "$HEADERS_DIR"
mkdir -p "$HEADERS_DIR"
cp generated/swift/*.h "$HEADERS_DIR/"
cp generated/swift/pqcFFI.modulemap "$HEADERS_DIR/module.modulemap"

echo "==> Assembling XCFramework"
rm -rf generated/PqcCore.xcframework
xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libpqc_client.a \
        -headers "$HEADERS_DIR" \
    -library target/ios-sim/libpqc_client.a \
        -headers "$HEADERS_DIR" \
    -output generated/PqcCore.xcframework

# Repo-root symlinks for local Pod consumption. PqcCore.podspec's
# `s.source_files` / `s.vendored_frameworks` reference bare `pqc.swift`
# and `PqcCore.xcframework` because that's the layout inside the release
# zip the published Pod fetches. For the example app's `:path => '../../../'`
# Pod entry, the Pod root IS the repo root — these symlinks make the bare
# names resolve to the just-built `generated/` artefacts. Both symlinks
# are ignored in .gitignore.
ln -sfn generated/swift/pqc.swift pqc.swift
ln -sfn generated/PqcCore.xcframework PqcCore.xcframework

echo
echo "iOS build complete:"
echo "  XCFramework:    generated/PqcCore.xcframework  (symlinked at ./PqcCore.xcframework)"
echo "  Swift binding:  generated/swift/pqc.swift      (symlinked at ./pqc.swift)"
echo
echo "Next: consume from a CocoaPod or SPM package (see docs/ios.md)."
