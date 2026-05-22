#!/usr/bin/env bash
# Build pqc_client for iOS (device + simulator), generate Swift bindings,
# and assemble PqcCore.xcframework.
# Output:
#   generated/PqcCore.xcframework
#   generated/swift/...         (UniFFI Swift bindings)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> Building for iOS device arm64"
cargo build --release --target aarch64-apple-ios

echo "==> Building for iOS simulator (arm64 + x86_64)"
cargo build --release --target aarch64-apple-ios-sim
cargo build --release --target x86_64-apple-ios

echo "==> Combining simulator slices via lipo"
mkdir -p target/ios-sim
lipo -create \
    target/aarch64-apple-ios-sim/release/libpqc_client.a \
    target/x86_64-apple-ios-sim/release/libpqc_client.a \
    -output target/ios-sim/libpqc_client.a

echo "==> Generating Swift bindings"
mkdir -p generated/swift
uniffi-bindgen generate \
    --library target/aarch64-apple-ios/release/libpqc_client.a \
    --language swift \
    --out-dir generated/swift

# UniFFI emits .swift + module.modulemap + .h; the .h goes into the XCFramework headers
HEADERS_DIR="generated/headers"
mkdir -p "$HEADERS_DIR"
cp generated/swift/*.h "$HEADERS_DIR/" || true
cp generated/swift/module.modulemap "$HEADERS_DIR/" || true

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
