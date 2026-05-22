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
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-15.1}"
# Simulator builds use this independent target var.
export IPHONESIMULATOR_DEPLOYMENT_TARGET="${IPHONESIMULATOR_DEPLOYMENT_TARGET:-15.1}"

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

echo "==> Generating Swift bindings"
rm -rf generated/swift
mkdir -p generated/swift
cargo run --release --bin uniffi-bindgen -- generate \
    --language swift \
    --out-dir generated/swift \
    src/pqc.udl

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

echo
echo "iOS build complete:"
echo "  XCFramework:    generated/PqcCore.xcframework"
echo "  Swift binding:  generated/swift/pqc.swift"
echo
echo "Next: consume from a CocoaPod or SPM package (see ios/README.md)."
